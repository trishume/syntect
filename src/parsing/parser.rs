// Suppression of a false positive clippy lint. Upstream issue:
//
//   mutable_key_type false positive for raw pointers
//   https://github.com/rust-lang/rust-clippy/issues/6745
//
// We use `*const MatchPattern` as key in our `SearchCache` hash map.
// Clippy thinks this is a problem since `MatchPattern` has interior mutability
// via `MatchPattern::regex::regex` which is an `AtomicLazyCell`.
// But raw pointers are hashed via the pointer itself, not what is pointed to.
// See https://github.com/rust-lang/rust/blob/1.54.0/library/core/src/hash/mod.rs#L717-L725
#![allow(clippy::mutable_key_type)]

use super::regex::{Regex, Region};
use super::scope::*;
use super::syntax_definition::*;
use crate::parsing::syntax_definition::ContextId;
use crate::parsing::syntax_set::{SyntaxReference, SyntaxSet};
use fnv::FnvHasher;
use regex_syntax::escape;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;

/// Errors that can occur while parsing.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ParsingError {
    #[error("Somehow main context was popped from the stack")]
    MissingMainContext,
    /// A context is missing. Usually caused by a syntax referencing a another
    /// syntax that is not known to syntect. See e.g. <https://github.com/trishume/syntect/issues/421>
    #[error("Missing context with ID '{0:?}'")]
    MissingContext(ContextId),
    #[error("Bad index to match_at: {0}")]
    BadMatchIndex(usize),
    #[error("Tried to use a ContextReference that has not bee resolved yet: {0:?}")]
    UnresolvedContextReference(ContextReference),
}

/// Keeps the current parser state (the internal syntax interpreter stack) between lines of parsing.
///
/// If you are parsing an entire file you create one of these at the start and use it
/// all the way to the end.
///
/// # Caching
///
/// One reason this is exposed is that since it implements `Clone` you can actually cache
/// these (probably along with a [`HighlightState`]) and only re-start parsing from the point of a change.
/// See the docs for [`HighlightState`] for more in-depth discussion of caching.
///
/// This state doesn't keep track of the current scope stack and parsing only returns changes to this stack
/// so if you want to construct scope stacks you'll need to keep track of that as well.
/// Note that [`HighlightState`] contains exactly this as a public field that you can use.
///
/// **Note:** Caching is for advanced users who have tons of time to maximize performance or want to do so eventually.
/// It is not recommended that you try caching the first time you implement highlighting.
///
/// [`HighlightState`]: ../highlighting/struct.HighlightState.html
/// Output of [`ParseState::parse_line`].
///
/// `ops` contains the scope-stack operations for the current line, as before.
/// `replayed` is non-empty only after a cross-line `fail` fires: it contains
/// the corrected ops for each buffered line (in chronological order) so that
/// callers who want full cross-line accuracy can re-apply them.
///
/// Callers that do not need cross-line accuracy can use `.ops` directly,
/// which behaves identically to the old `Vec<(usize, ScopeStackOp)>` return.
#[derive(Debug, Clone, Default)]
pub struct ParseLineOutput {
    /// Ops for the current line.
    pub ops: Vec<(usize, ScopeStackOp)>,
    /// Ops for previously buffered lines that have now been corrected, in order.
    /// Non-empty only when a cross-line `fail` just resolved.
    pub replayed: Vec<Vec<(usize, ScopeStackOp)>>,
    /// Warnings collected during parsing (e.g. branch point expiry).
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ParseState {
    stack: Vec<StateLevel>,
    first_line: bool,
    // See issue #101. Contains indices of frames pushed by `with_prototype`s.
    // Doesn't look at `with_prototype`s below top of stack.
    proto_starts: Vec<usize>,
    /// Active branch points for backtracking support.
    branch_points: Vec<BranchPoint>,
    /// Line counter for 128-line branch point expiry.
    line_number: usize,
    /// Line strings buffered while branch points are active, for potential
    /// cross-line `fail` replay. Only the strings are stored; the ops are
    /// returned to callers immediately (same as before).
    pending_lines: Vec<String>,
    /// Snapshot of `shadow` at the start of each buffered line in
    /// `pending_lines`. Used by the cross-line-fail replay to restore
    /// `shadow` to its state at the first replayed line's beginning, so
    /// the shadow mirrors what the consumer does (reset + apply
    /// replayed).
    pending_line_start_shadows: Vec<ScopeStack>,
    /// Corrected ops produced by a cross-line `fail` replay, to be returned
    /// as `ParseLineOutput::replayed` at the end of `parse_line`. When
    /// populated, entry `i` corresponds to `pending_lines[flushed_ops_start + i]`.
    flushed_ops: Vec<Vec<(usize, ScopeStackOp)>>,
    /// Pending-lines index that `flushed_ops[0]` maps to when `flushed_ops`
    /// is non-empty. Reset to `None` between `parse_line` calls.
    flushed_ops_start: Option<usize>,
    /// Identity of the branch point whose cross-line replay produced
    /// `flushed_ops`. `None` when `flushed_ops_start` is `None`.
    /// Used by `prefer_inner_replay_corrections` to discriminate
    /// between an inner BP whose correction is structurally a refinement
    /// of the outer BP's resolved alternative (prefer) vs. one that
    /// would over-apply (keep outer).
    flushed_ops_bp: Option<BpInfo>,
    /// Warnings accumulated during parsing, drained into `ParseLineOutput`.
    warnings: Vec<String>,
    /// Active escape patterns from embed operations. The escape regex takes
    /// strict precedence over normal patterns — it is checked first and can
    /// truncate the search region.
    escape_stack: Vec<EscapeEntry>,
    /// Mirror of the consumer's scope stack. Updated at `parse_line`
    /// boundaries (not mid-line) from the returned `ops` and
    /// `replayed`, mirroring the consumer's behaviour (reset to the
    /// first-replayed line's start, then apply replayed, then apply
    /// current ops). `exec_escape` uses it to detect orphan atoms left
    /// on the consumer's stack by a prior cross-line replay whose
    /// later same-line fails truncated the owning context out of
    /// `self.stack` (the Push for the atom is committed in
    /// `flushed_ops`, so it can't be taken back by `ops.truncate`) —
    /// and emits a balancing Pop before the normal escape pops.
    shadow: ScopeStack,
    /// Active while `handle_fail` recurses into `parse_line_inner*` to
    /// replay a buffered past line under a new alternative. Overrides
    /// the "current line" / "pending_lines slot" bookkeeping that
    /// branches created during the re-parse record, so they anchor to
    /// the replay line `L+i` rather than the outer `parse_line`'s
    /// current line. Without it, a later fail on the outer line
    /// misclassifies the replay-born branch as same-line and applies
    /// its replay-line-relative `match_start` to a shorter outer line
    /// (the byte-20-out-of-13 panic on `syntax_test_java.java:10263`
    /// inside `@MultiLineAnnotation(...)`).
    replay_ctx: Option<ReplayCtx>,
    /// Ops the outer cross-line replay has already composed for the
    /// first replayed line — outer prefix_ops + new-alt meta/pat/capture
    /// emission. A branch_point created during the inner re-parse
    /// records `replay_prefix_ops + ops` as its own `prefix_ops`, so
    /// when it later fails and reconstructs its line, the outer
    /// captures (e.g. `[foo]:` LRD opener) survive instead of being
    /// rebuilt from an empty Vec — the cause of `meta.link.reference`
    /// scope loss in `syntax_test_markdown.md`'s `[foo]: /url` cases
    /// where `link-def-title-continuation`'s fail spawns a nested
    /// `link-def-attr-continuation` whose own fail then replayed line 3
    /// without the original captures.
    replay_prefix_ops: Option<Vec<(usize, ScopeStackOp)>>,
    /// Branch_points whose alternatives have all been exhausted at a
    /// specific cursor position on the current line. Subsequent
    /// `find_best_match` calls at that position skip the matching pattern
    /// so the parent context's NEXT rule gets a chance — Sublime Text's
    /// behaviour. Without this, syntect's prior approach (advance one
    /// character past the lookahead) lets stale keyword rules match in
    /// the middle of identifiers (e.g. `package` inside `$package` after
    /// `declarations` exhausted on the leading `$`). Cleared whenever
    /// the cursor moves.
    skipped_branches: Vec<(usize, String)>,
}

/// Identity of a branch point whose cross-line replay wrote ops to
/// `flushed_ops`. Captured so `prefer_inner_replay_corrections` can
/// compare the inner BP (whose corrections are candidate replacements)
/// against the outer BP (whose locally-computed replay is the default).
#[derive(Debug, Clone, Eq, PartialEq)]
struct BpInfo {
    name: String,
    /// Stack depth at branch creation (mirrors `BranchPoint::stack_depth`).
    stack_depth: usize,
    /// Line number at branch creation (mirrors `BranchPoint::line_number`).
    line_number: usize,
}

/// Bookkeeping override used while `handle_fail` is re-parsing a
/// buffered past line. See the `replay_ctx` field on `ParseState`.
#[derive(Debug, Clone, Eq, PartialEq)]
struct ReplayCtx {
    /// Virtual "current line" of the inner re-parse (`bp.line_number + i`).
    line_number: usize,
    /// Slot in `self.pending_lines` that a branch created during this
    /// replay iteration should record as its snapshot length, so a
    /// future cross-line fail replays from `L+i` onward.
    pending_lines_snapshot_offset: usize,
}

/// A resolved escape pattern from an `embed` operation, stored on the escape stack.
#[derive(Debug, Clone, Eq, PartialEq)]
struct EscapeEntry {
    /// The resolved escape regex (backrefs substituted at push time).
    regex: Regex,
    /// Capture mapping for escape_captures scopes.
    captures: Option<CaptureMapping>,
    /// Stack depth at the time of the embed push — when escape fires,
    /// pop down to this depth.
    stack_depth: usize,
}

/// Snapshot of parser state at a branch point, used for backtracking.
#[derive(Debug, Clone, Eq, PartialEq)]
struct BranchPoint {
    name: String,
    /// Index of the next alternative to try (0 = first alt already tried).
    next_alternative: usize,
    alternatives: Vec<ContextReference>,
    stack_snapshot: Vec<StateLevel>,
    proto_starts_snapshot: Vec<usize>,
    /// Character position to rewind to. Despite the name, this is the
    /// branch match's *end* position — where the parser resumes from.
    match_start: usize,
    /// Real start of the branch_point match text. Together with
    /// `match_start` (above, which is the match end) this bounds the
    /// span on which `pat_scope` applies. Used to re-emit the keyword
    /// scope — e.g. `keyword.operator.comparison.sql` on `LIKE` —
    /// after a `fail` rewind, since the original Push/Pop pair was
    /// truncated off `ops` along with `alt[0]`'s subsequent work.
    trigger_match_start: usize,
    /// Scopes declared on the branch_point match itself (re-emitted on
    /// fail-retry over the [`trigger_match_start`, `match_start`) span).
    pat_scope: Vec<Scope>,
    /// Line number when the branch was created (for 128-line limit).
    line_number: usize,
    /// Length of ops vec at snapshot time — truncation point on fail.
    ops_snapshot_len: usize,
    /// Stack depth at creation — if stack shrinks below this, branch is invalid.
    stack_depth: usize,
    non_consuming_push_at_snapshot: (usize, usize),
    first_line_snapshot: bool,
    with_prototype: Option<ContextReference>,
    /// `pending_lines.len()` at snapshot time, for cross-line replay truncation.
    pending_lines_snapshot_len: usize,
    escape_stack_snapshot: Vec<EscapeEntry>,
    /// Number of contexts to pop before pushing the alternative (for pop + branch).
    pop_count: usize,
    /// Ops emitted on the branch-creation line before the branch match.
    /// Used by cross-line fail replay to reconstruct the first buffered
    /// line without re-parsing its pre-branch prefix under the new
    /// alternative (which would misattribute pre-branch content to
    /// rules of the new alternative — e.g. in multi-line SQL `LIKE …
    /// ESCAPE …`, every non-whitespace before the `LIKE` fires
    /// `else-pop` in the escape-alternative, derailing the stack).
    prefix_ops: Vec<(usize, ScopeStackOp)>,
    /// Capture Push/Pop ops emitted alongside the branch_point match's
    /// `pat_scope`. Re-emitted on fail-retry between the pat_scope
    /// Push and Pop so captures like `keyword.declaration.data.haskell`
    /// on the first capture group of `(data)(?:\s+(family|instance))?`
    /// survive a branch swap — without this, a `data CtxCls ctx => …`
    /// (where `alt[0]` `data-signature` fails into `alt[1]` `data-context`)
    /// drops the keyword scope from the `data` token.
    capture_ops: Vec<(usize, ScopeStackOp)>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct StateLevel {
    context: ContextId,
    prototypes: Vec<ContextId>,
    captures: Option<(Region, String)>,
}

#[derive(Debug)]
struct RegexMatch<'a> {
    regions: Region,
    context: &'a Context,
    pat_index: usize,
    from_with_prototype: bool,
    would_loop: bool,
    /// For escape matches (pat_index == usize::MAX): index into escape_stack.
    escape_index: usize,
}

/// Maps the pattern to the start index, which is -1 if not found.
type SearchCache = HashMap<*const MatchPattern, Option<Region>, BuildHasherDefault<FnvHasher>>;

/// Build the ordered Push/Pop ops for a match's `captures:` mapping over
/// its regex `regions`. Captures can appear in arbitrary source order
/// (e.g. `((bob)|(hi))*` matching `hibob` — the outer group must Push
/// before any inner group). Empty captures are skipped because they'd
/// otherwise sort a Pop before its Push. The returned ops are already
/// position-ordered and safe to append to a parser ops vec.
///
/// Each capture's `(cap_start, cap_end)` is clipped to the outer match
/// range `regions.pos(0)` so a group matching inside a `(?=...)` /
/// `(?<=...)` whose own span extends past the consumed range still
/// colours the overlap (the boundary char) and nothing beyond it. This
/// mirrors Sublime Text; without the clip, a lookahead-internal
/// `captures:` entry leaked scope over unmatched trailing chars or was
/// silently dropped upstream in `parse_captures`.
fn build_capture_ops(capture_map: &CaptureMapping, regions: &Region) -> Vec<(usize, ScopeStackOp)> {
    let mut map: Vec<((usize, i32), ScopeStackOp)> = Vec::new();
    let (match_start, match_end) = match regions.pos(0) {
        Some(bounds) => bounds,
        None => return Vec::new(),
    };
    for &(cap_index, ref scopes) in capture_map.iter() {
        if let Some((cap_start, cap_end)) = regions.pos(cap_index) {
            let clipped_start = cap_start.max(match_start);
            let clipped_end = cap_end.min(match_end);
            if clipped_start >= clipped_end {
                continue;
            }
            for scope in scopes.iter() {
                map.push((
                    (clipped_start, -((clipped_end - clipped_start) as i32)),
                    ScopeStackOp::Push(*scope),
                ));
            }
            map.push(((clipped_end, i32::MIN), ScopeStackOp::Pop(scopes.len())));
        }
    }
    map.sort_by(|a, b| a.0.cmp(&b.0));
    map.into_iter().map(|((i, _), op)| (i, op)).collect()
}

// To understand the implementation of this, here's an introduction to how
// Sublime Text syntax definitions work.
//
// Let's say we have the following made-up syntax definition:
//
//     contexts:
//       main:
//         - match: A
//           scope: scope.a.first
//           push: context-a
//         - match: b
//           scope: scope.b
//         - match: \w+
//           scope: scope.other
//       context-a:
//         - match: a+
//           scope: scope.a.rest
//         - match: (?=.)
//           pop: true
//
// There are two contexts, `main` and `context-a`. Each context contains a list
// of match rules with instructions for how to proceed.
//
// Let's say we have the input string " Aaaabxxx". We start at position 0 in
// the string. We keep a stack of contexts, which at the beginning is just main.
//
// So we start by looking at the top of the context stack (main), and look at
// the rules in order. The rule that wins is the first one that matches
// "earliest" in the input string. In our example:
//
// 1. The first one matches "A". Note that matches are not anchored, so this
//    matches at position 1.
// 2. The second one matches "b", so position 5. The first rule is winning.
// 3. The third one matches "\w+", so also position 1. But because the first
//    rule comes first, it wins.
//
// So now we execute the winning rule. Whenever we matched some text, we assign
// the scope (if there is one) to the matched text and advance our position to
// after the matched text. The scope is "scope.a.first" and our new position is
// after the "A", so 2. The "push" means that we should change our stack by
// pushing `context-a` on top of it.
//
// In the next step, we repeat the above, but now with the rules in `context-a`.
// The result is that we match "a+" and assign "scope.a.rest" to "aaa", and our
// new position is now after the "aaa". Note that there was no instruction for
// changing the stack, so we stay in that context.
//
// In the next step, the first rule doesn't match anymore, so we go to the next
// rule where "(?=.)" matches. The instruction is to "pop", which means we
// pop the top of our context stack, which means we're now back in main.
//
// This time in main, we match "b", and in the next step we match the rest with
// "\w+", and we're done.
//
//
// ## Preventing loops
//
// These are the basics of how matching works. Now, you saw that you can write
// patterns that result in an empty match and don't change the position. These
// are called non-consuming matches. The problem with them is that they could
// result in infinite loops. Let's look at a syntax where that is the case:
//
//     contexts:
//       main:
//         - match: (?=.)
//           push: test
//       test:
//         - match: \w+
//           scope: word
//         - match: (?=.)
//           pop: true
//
// This is a bit silly, but it's a minimal example for explaining how matching
// works in that case.
//
// Let's say we have the input string " hello". In `main`, our rule matches and
// we go into `test` and stay at position 0. Now, the best match is the rule
// with "pop". But if we used that rule, we'd pop back to `main` and would still
// be at the same position we started at! So this would be an infinite loop,
// which we don't want.
//
// So what Sublime Text does in case a looping rule "won":
//
// * If there's another rule that matches at the same position and does not
//   result in a loop, use that instead.
// * Otherwise, go to the next position and go through all the rules in the
//   current context again. Note that it means that the "pop" could again be the
//   winning rule, but that's ok as it wouldn't result in a loop anymore.
//
// So in our input string, we'd skip one character and try to match the rules
// again. This time, the "\w+" wins because it comes first.

impl ParseState {
    /// Creates a state from a syntax definition, keeping its own reference-counted point to the
    /// main context of the syntax
    pub fn new(syntax: &SyntaxReference) -> ParseState {
        let start_state = StateLevel {
            context: syntax.context_ids()["__start"],
            prototypes: Vec::new(),
            captures: None,
        };
        ParseState {
            stack: vec![start_state],
            first_line: true,
            proto_starts: Vec::new(),
            branch_points: Vec::new(),
            line_number: 0,
            pending_lines: Vec::new(),
            pending_line_start_shadows: Vec::new(),
            flushed_ops: Vec::new(),
            flushed_ops_start: None,
            flushed_ops_bp: None,
            warnings: Vec::new(),
            escape_stack: Vec::new(),
            shadow: ScopeStack::new(),
            replay_ctx: None,
            replay_prefix_ops: None,
            skipped_branches: Vec::new(),
        }
    }

    /// Parses a single line of the file. Because of the way regex engines work you unfortunately
    /// have to pass in a single line contiguous in memory. This can be bad for really long lines.
    /// Sublime Text avoids this by just not highlighting lines that are too long (thousands of characters).
    ///
    /// For efficiency reasons this returns only the changes to the current scope at each point in the line.
    /// You can use [`ScopeStack::apply`] on each operation in succession to get the stack for a given point.
    /// Look at the code in `highlighter.rs` for an example of doing this for highlighting purposes.
    ///
    /// The returned vector is in order both by index to apply at (the `usize`) and also by order to apply them at a
    /// given index (e.g popping old scopes before pushing new scopes).
    ///
    /// The [`SyntaxSet`] has to be the one that contained the syntax that was used to construct
    /// this [`ParseState`], or an extended version of it. Otherwise the parsing would return the
    /// wrong result or even panic. The reason for this is that contexts within the [`SyntaxSet`]
    /// are referenced via indexes.
    ///
    /// [`ScopeStack::apply`]: struct.ScopeStack.html#method.apply
    /// [`SyntaxSet`]: struct.SyntaxSet.html
    /// [`ParseState`]: struct.ParseState.html
    pub fn parse_line(
        &mut self,
        line: &str,
        syntax_set: &SyntaxSet,
    ) -> Result<ParseLineOutput, ParsingError> {
        if self.stack.is_empty() {
            return Err(ParsingError::MissingMainContext);
        }

        // Skipped-branch entries are tied to byte offsets within a single
        // line — they don't survive across line boundaries.
        self.skipped_branches.clear();

        // Prune branch points older than 128 lines
        let cur_line = self.line_number;
        let warnings = &mut self.warnings;
        self.branch_points.retain(|bp| {
            let alive = cur_line.saturating_sub(bp.line_number) <= 128;
            if !alive {
                warnings.push(format!(
                    "branch point '{}' expired (exceeded 128-line rewind limit)",
                    bp.name
                ));
            }
            alive
        });
        self.line_number += 1;

        let pending_lines_before = self.pending_lines.len();

        let ops = self.parse_line_inner(line, syntax_set)?;

        // Collect any corrected ops produced by a cross-line `fail` during the
        // parse above.  These are stored by `handle_fail` in `self.flushed_ops`.
        let replayed = std::mem::take(&mut self.flushed_ops);
        self.flushed_ops_start = None;
        self.flushed_ops_bp = None;

        // Update shadow to reflect consumer's view at end of this line.
        // The consumer (see `syntest`) resets its scope stack to
        // `parsed_line_buffer[start_idx].stack_before` when `replayed` is
        // non-empty, then applies replayed then applies current-line ops.
        // Mirror that here so `shadow` matches the consumer downstream.
        //
        // While re-applying replays, also overwrite each buffered line's
        // pending_line_start_shadows entry with the corrected baseline.
        // Without this, a later replay covering this same line would reset
        // shadow to a stale snapshot captured before the prior replay's
        // correction landed, causing scope leaks (e.g.
        // meta.link.reference.def.markdown persisting past back-to-back
        // Markdown link reference definitions, since each LRD's correction
        // arrives in the *next* line's parse_line and the snapshot for
        // that next line was captured from the buggy uncorrected stack).
        if !replayed.is_empty() {
            let start_idx = pending_lines_before
                .checked_sub(replayed.len())
                .unwrap_or(0);
            if let Some(snap) = self.pending_line_start_shadows.get(start_idx) {
                self.shadow = snap.clone();
            }
            for (i, line_ops) in replayed.iter().enumerate() {
                for (_, op) in line_ops {
                    let _ = self.shadow.apply(op);
                }
                // After applying replayed[i], shadow == start of buffered
                // line (start_idx + i + 1). Overwrite that snapshot so the
                // next replay covering it starts from the corrected
                // baseline rather than the stale one captured pre-replay.
                let next_idx = start_idx + i + 1;
                if next_idx < self.pending_line_start_shadows.len() {
                    self.pending_line_start_shadows[next_idx] = self.shadow.clone();
                }
            }
        }

        // Snapshot the shadow now (post-replays, pre-current-ops) — this
        // becomes the baseline for the next line if the current line ends
        // with live branch_points and gets buffered for future replay.
        let shadow_at_start_corrected = self.shadow.clone();

        for (_, op) in &ops {
            let _ = self.shadow.apply(op);
        }

        // Keep the line string for potential future cross-line replay.
        if !self.branch_points.is_empty() {
            self.pending_lines.push(line.to_string());
            self.pending_line_start_shadows
                .push(shadow_at_start_corrected);
        } else {
            // No active branch points: any buffered strings are stale.
            self.pending_lines.clear();
            self.pending_line_start_shadows.clear();
        }

        let warnings = std::mem::take(&mut self.warnings);

        Ok(ParseLineOutput {
            ops,
            replayed,
            warnings,
        })
    }

    /// Returns `true` when the parser is inside a `branch_point` and the
    /// result of `parse_line` may be revised by a future `fail` action.
    /// Once the branch resolves (or if no branch was entered), this returns
    /// `false` and all ops emitted so far are final.
    pub fn is_speculative(&self) -> bool {
        !self.branch_points.is_empty()
    }

    /// Inner parsing loop: processes `line` with the current parser state and
    /// returns the scope-stack operations.  Does **not** touch `pending_lines`
    /// or `flushed_ops`, so it is safe to call recursively from `handle_fail`
    /// for cross-line replay without re-entrancy issues.
    fn parse_line_inner(
        &mut self,
        line: &str,
        syntax_set: &SyntaxSet,
    ) -> Result<Vec<(usize, ScopeStackOp)>, ParsingError> {
        self.parse_line_inner_from(line, syntax_set, 0)
    }

    /// Parse `line` starting at `start_at` rather than column 0. Used by
    /// cross-line `fail` replay: the first buffered line's pre-branch
    /// prefix was correctly parsed under the pre-branch state, so the
    /// replay resumes *after* the branch match under the new alternative.
    /// When `start_at > 0` the `first_line` bookkeeping is skipped — the
    /// caller has already emitted (or preserved) the initial
    /// meta_content_scope push.
    fn parse_line_inner_from(
        &mut self,
        line: &str,
        syntax_set: &SyntaxSet,
        start_at: usize,
    ) -> Result<Vec<(usize, ScopeStackOp)>, ParsingError> {
        let mut match_start = start_at;
        let mut res = Vec::new();

        if start_at == 0 && self.first_line {
            let cur_level = &self.stack[self.stack.len() - 1];
            let context = syntax_set.get_context(&cur_level.context)?;
            if !context.meta_content_scope.is_empty() {
                res.push((0, ScopeStackOp::Push(context.meta_content_scope[0])));
            }
            self.first_line = false;
        }

        let mut regions = Region::new();
        let fnv = BuildHasherDefault::<FnvHasher>::default();
        let mut search_cache: SearchCache = HashMap::with_capacity_and_hasher(128, fnv);
        // Used for detecting loops with push/pop, see long comment above.
        let mut non_consuming_push_at = (0, 0);

        while self.parse_next_token(
            line,
            syntax_set,
            &mut match_start,
            &mut search_cache,
            &mut regions,
            &mut non_consuming_push_at,
            &mut res,
        )? {}

        Ok(res)
    }

    #[allow(clippy::too_many_arguments)]
    fn parse_next_token(
        &mut self,
        line: &str,
        syntax_set: &SyntaxSet,
        start: &mut usize,
        search_cache: &mut SearchCache,
        regions: &mut Region,
        non_consuming_push_at: &mut (usize, usize),
        ops: &mut Vec<(usize, ScopeStackOp)>,
    ) -> Result<bool, ParsingError> {
        let check_pop_loop = {
            let (pos, stack_depth) = *non_consuming_push_at;
            pos == *start && stack_depth == self.stack.len()
        };

        // Trim proto_starts that are no longer valid
        while self
            .proto_starts
            .last()
            .map(|start| *start >= self.stack.len())
            .unwrap_or(false)
        {
            self.proto_starts.pop();
        }

        let best_match = self.find_best_match(
            line,
            *start,
            syntax_set,
            search_cache,
            regions,
            check_pop_loop,
        )?;

        if let Some(reg_match) = best_match {
            // Check if this is an escape match (sentinel pat_index)
            if reg_match.pat_index == usize::MAX {
                let (match_start, match_end) = reg_match.regions.pos(0).unwrap();
                *start = match_end;
                self.exec_escape(
                    reg_match.escape_index,
                    match_start,
                    match_end,
                    &reg_match.regions,
                    syntax_set,
                    ops,
                )?;
                search_cache.clear();
                return Ok(true);
            }

            if reg_match.would_loop {
                // A push that doesn't consume anything (a regex that resulted
                // in an empty match at the current position) can not be
                // followed by a non-consuming pop. Otherwise we're back where
                // we started and would try the same sequence of matches again,
                // resulting in an infinite loop. In this case, Sublime Text
                // advances one character and tries again, thus preventing the
                // loop.

                // println!("pop_would_loop for match {:?}, start {}", reg_match, *start);

                // nth(1) gets the next character if there is one. Need to do
                // this instead of just += 1 because we have byte indices and
                // unicode characters can be more than 1 byte.
                if let Some((i, _)) = line[*start..].char_indices().nth(1) {
                    *start += i;
                    return Ok(true);
                } else {
                    // End of line, no character to advance and no point trying
                    // any more patterns.
                    return Ok(false);
                }
            }

            let match_end = reg_match.regions.pos(0).unwrap().1;

            // Check if this is a Fail operation — handle before advancing start
            let context = reg_match.context;
            let match_pattern = context.match_at(reg_match.pat_index)?;
            if let MatchOperation::Fail(_) = match_pattern.operation {
                let level_context = {
                    let id = &self.stack[self.stack.len() - 1].context;
                    syntax_set.get_context(id)?
                };
                return self.exec_pattern(
                    line,
                    &reg_match,
                    level_context,
                    syntax_set,
                    start,
                    non_consuming_push_at,
                    ops,
                    search_cache,
                );
            }

            let consuming = match_end > *start;
            if !consuming {
                // The match doesn't consume any characters. If this is a
                // "push", remember the position and stack size so that we can
                // check the next "pop" for loops. Otherwise leave the state,
                // e.g. non-consuming "set" could also result in a loop.
                if matches!(
                    match_pattern.operation,
                    MatchOperation::Push(_)
                        | MatchOperation::Branch { .. }
                        | MatchOperation::Embed { .. }
                ) {
                    *non_consuming_push_at = (match_end, self.stack.len() + 1);
                }
                // Inside a cross-line replay, a non-consuming `Branch` whose
                // match lands past every character of the replay line creates
                // a chained `branch_point` that the outer parse will then
                // exhaust on the next (often empty) line. The exhaustion's
                // pop ops attach to *this* replay line — i.e. the next line's
                // baseline — collapsing the parent context one boundary too
                // early. Skip the branch creation; the parent rule will fire
                // again at the start of the outer line and the BP will be
                // anchored there instead. Observed on Markdown's LRD blank
                // line: link-def-attr's `match: $` was creating a BP-2
                // inside link-title-continuation's exhaustion replay, then
                // collapsing `meta.link.reference.def.markdown` on the empty
                // line.
                if matches!(match_pattern.operation, MatchOperation::Branch { .. })
                    && self.replay_ctx.is_some()
                    && match_end >= line.len()
                {
                    return Ok(false);
                }
            }

            *start = match_end;

            // Prune stale skipped_branches entries: a BP marked as skipped
            // at an older cursor position no longer matters once the
            // cursor has advanced past that position.
            self.skipped_branches.retain(|(c, _)| *c >= *start);

            // ignore `with_prototype`s below this if a context is pushed
            if reg_match.from_with_prototype {
                // use current height, since we're before the actual push
                self.proto_starts.push(self.stack.len());
            }

            let level_context = {
                let id = &self.stack[self.stack.len() - 1].context;
                syntax_set.get_context(id)?
            };
            self.exec_pattern(
                line,
                &reg_match,
                level_context,
                syntax_set,
                start,
                non_consuming_push_at,
                ops,
                search_cache,
            )?;

            Ok(true)
        } else if self.skipped_branches.iter().any(|(c, _)| *c == *start) {
            // No pattern matched, but we suppressed at least one Branch
            // at this cursor (its alts all exhausted). Advance one char as
            // a last resort to break the loop.
            self.skipped_branches.retain(|(c, _)| *c != *start);
            if let Some((i, _)) = line[*start..].char_indices().nth(1) {
                *start += i;
                search_cache.clear();
                Ok(true)
            } else {
                Ok(false)
            }
        } else {
            Ok(false)
        }
    }

    fn find_best_match<'a>(
        &self,
        line: &str,
        start: usize,
        syntax_set: &'a SyntaxSet,
        search_cache: &mut SearchCache,
        regions: &mut Region,
        check_pop_loop: bool,
    ) -> Result<Option<RegexMatch<'a>>, ParsingError> {
        let cur_level = &self.stack[self.stack.len() - 1];
        let context = syntax_set.get_context(&cur_level.context)?;
        let prototype = if let Some(ref p) = context.prototype {
            Some(p)
        } else {
            None
        };

        // Build an iterator for the contexts we want to visit in order
        let context_chain = {
            let proto_start = self.proto_starts.last().cloned().unwrap_or(0);
            // Sublime applies with_prototypes from bottom to top
            let with_prototypes = self.stack[proto_start..].iter().flat_map(|lvl| {
                lvl.prototypes
                    .iter()
                    .map(move |ctx| (true, ctx, lvl.captures.as_ref()))
            });
            let cur_prototype = prototype.into_iter().map(|ctx| (false, ctx, None));
            let cur_context =
                Some((false, &cur_level.context, cur_level.captures.as_ref())).into_iter();
            with_prototypes.chain(cur_prototype).chain(cur_context)
        };

        // println!("{:#?}", cur_level);
        // println!("token at {} on {}", start, line.trim_right());

        // Check escape patterns first — they take strict precedence.
        // If an escape matches at `start`, return it immediately as a synthetic match.
        // If it matches later, truncate the search region for normal patterns.
        let mut search_end = line.len();
        let mut escape_match: Option<(usize, Region)> = None; // (escape_stack_index, region)

        for (ei, entry) in self.escape_stack.iter().enumerate() {
            let mut esc_regions = Region::new();
            if entry
                .regex
                .search(line, start, line.len(), Some(&mut esc_regions), true)
            {
                let (esc_start, _esc_end) = esc_regions.pos(0).unwrap();
                if esc_start < search_end {
                    search_end = esc_start;
                    escape_match = Some((ei, esc_regions));
                }
            }
        }

        // If escape matches right at `start`, it wins immediately — no need to
        // search normal patterns.
        if let Some((ei, ref esc_region)) = escape_match {
            let esc_start = esc_region.pos(0).unwrap().0;
            if esc_start == start {
                return Ok(Some(RegexMatch {
                    regions: esc_region.clone(),
                    context: syntax_set.get_context(&cur_level.context)?,
                    pat_index: usize::MAX, // sentinel for escape match
                    from_with_prototype: false,
                    would_loop: false,
                    escape_index: ei,
                }));
            }
        }

        let mut min_start = usize::MAX;
        let mut best_match: Option<RegexMatch<'_>> = None;
        let mut pop_would_loop = false;

        for (from_with_proto, ctx, captures) in context_chain {
            for (pat_context, pat_index) in context_iter(syntax_set, syntax_set.get_context(ctx)?) {
                let match_pat = pat_context.match_at(pat_index)?;

                // Skip Branch patterns whose name was just exhausted at this
                // cursor. See ParseState::skipped_branches and the same-line
                // exhaustion handler in handle_fail.
                if let MatchOperation::Branch { name, .. } = &match_pat.operation {
                    if self
                        .skipped_branches
                        .iter()
                        .any(|(c, n)| *c == start && n == name)
                    {
                        continue;
                    }
                }

                if let Some(match_region) = self.search_with_end(
                    line,
                    start,
                    search_end,
                    match_pat,
                    captures,
                    search_cache,
                    regions,
                ) {
                    let (match_start, match_end) = match_region.pos(0).unwrap();

                    // println!("matched pattern {:?} at start {} end {} (pop would loop: {}, min start: {}, initial start: {}, check_pop_loop: {}, stack_len: {})", match_pat, match_start, match_end, pop_would_loop, min_start, start, check_pop_loop, self.stack.len());

                    if match_start < min_start || (match_start == min_start && pop_would_loop) {
                        // New match is earlier in text than old match,
                        // or old match was a looping pop at the same
                        // position.

                        // println!("setting as current match");

                        min_start = match_start;

                        let consuming = match_end > start;
                        // A non-consuming `pop: N` after a non-consuming push
                        // only loops when N == 1 — that restores the exact
                        // pre-push stack, so the push rule fires again. With
                        // N >= 2 the stack drops strictly below the pre-push
                        // depth, so the outer context no longer has the same
                        // trigger in scope (e.g. Haskell's `immediately-pop2`
                        // as the fallback branch alternative for
                        // `declaration-type-end`).
                        pop_would_loop = check_pop_loop
                            && !consuming
                            && matches!(match_pat.operation, MatchOperation::Pop(1));

                        let push_too_deep = matches!(
                            match_pat.operation,
                            MatchOperation::Push(_)
                                | MatchOperation::Branch { .. }
                                | MatchOperation::Embed { .. }
                        ) && self.stack.len() >= 100;

                        if push_too_deep {
                            return Ok(None);
                        }

                        best_match = Some(RegexMatch {
                            regions: match_region,
                            context: pat_context,
                            pat_index,
                            from_with_prototype: from_with_proto,
                            would_loop: pop_would_loop,
                            escape_index: 0, // not an escape match
                        });

                        if match_start == start && !pop_would_loop {
                            // We're not gonna find a better match after this,
                            // so as an optimization we can stop matching now.
                            return Ok(best_match);
                        }
                    }
                }
            }
        }

        // If no normal match was found before the escape position, or escape
        // position is earlier, use the escape match.
        if let Some((ei, esc_region)) = escape_match {
            let esc_start = esc_region.pos(0).unwrap().0;
            if esc_start < min_start || (esc_start == min_start && pop_would_loop) {
                return Ok(Some(RegexMatch {
                    regions: esc_region,
                    context: syntax_set.get_context(&cur_level.context)?,
                    pat_index: usize::MAX, // sentinel for escape match
                    from_with_prototype: false,
                    would_loop: false,
                    escape_index: ei,
                }));
            }
        }

        Ok(best_match)
    }

    fn search_with_end(
        &self,
        line: &str,
        start: usize,
        search_end: usize,
        match_pat: &MatchPattern,
        captures: Option<&(Region, String)>,
        search_cache: &mut SearchCache,
        regions: &mut Region,
    ) -> Option<Region> {
        // println!("{} - {:?} - {:?}", match_pat.regex_str, match_pat.has_captures, cur_level.captures.is_some());
        let match_ptr = match_pat as *const MatchPattern;

        // Only consult the cache when searching the full line. Cached entries
        // are produced under full-line lookahead semantics: a truncated search
        // at an embed-escape boundary may flip lookahead/lookbehind results
        // that depended on chars past `search_end`. Concretely, ``done`` at
        // the close of a backticked `for…done` would be cached as
        // no-match (the keyword's `(?!cmd_char)` saw the closing backtick
        // through full-line search) and then short-circuited inside the
        // backtick embed even though the lookahead actually succeeds against
        // the embed's escape boundary.
        if search_end == line.len() {
            if let Some(maybe_region) = search_cache.get(&match_ptr) {
                if let Some(ref region) = *maybe_region {
                    let (cached_start, _cached_end) = region.pos(0).unwrap();
                    if cached_start >= start {
                        return Some(region.clone());
                    }
                    // cached_start < start: cache miss, re-search below
                } else {
                    // Didn't find a match earlier, so no point trying again.
                    return None;
                }
            }
        }

        let (regex, can_cache) = match (match_pat.has_captures, captures) {
            (true, Some(captures)) => {
                let (region, s) = captures;
                (&match_pat.regex_with_refs(region, s), false)
            }
            _ => (match_pat.regex(), true),
        };
        // Only `MatchOperation::None` patterns must avoid zero-length matches; every other
        // operation legitimately needs them (lookaheads with branch/fail, empty patterns with
        // pop/set, etc.). The regex engine handles this via its `FIND_NOT_EMPTY` option.
        let allow_empty = !matches!(match_pat.operation, MatchOperation::None);
        // print!("  executing regex: {:?} at pos {} on line {}", regex.regex_str(), start, line);
        let matched = regex.search(line, start, search_end, Some(regions), allow_empty);

        if matched {
            let (match_start, match_end) = regions.pos(0).unwrap();
            // this is necessary to avoid infinite looping on dumb patterns
            let does_something = match match_pat.operation {
                MatchOperation::None => match_start != match_end,
                MatchOperation::Push(_)
                | MatchOperation::Branch { .. }
                | MatchOperation::Embed { .. } => self.stack.len() < 100,
                _ => true,
            };
            if can_cache && does_something && search_end == line.len() {
                // Only cache when searching the full line — truncated searches
                // could give different results for later positions.
                search_cache.insert(match_pat, Some(regions.clone()));
            }
            if does_something {
                // print!("catch {} at {} on {}", match_pat.regex_str, match_start, line);
                return Some(regions.clone());
            }
        } else if can_cache && search_end == line.len() {
            search_cache.insert(match_pat, None);
        }
        None
    }

    /// Returns true if the stack was changed.
    /// For `Fail` operations, returns `Ok(true)` if backtracking was performed
    /// (caller should continue parsing from the rewound position).
    fn exec_pattern<'a>(
        &mut self,
        line: &str,
        reg_match: &RegexMatch<'a>,
        level_context: &'a Context,
        syntax_set: &'a SyntaxSet,
        start: &mut usize,
        non_consuming_push_at: &mut (usize, usize),
        ops: &mut Vec<(usize, ScopeStackOp)>,
        search_cache: &mut SearchCache,
    ) -> Result<bool, ParsingError> {
        let (match_start, match_end) = reg_match.regions.pos(0).unwrap();
        let context = reg_match.context;
        let pat = context.match_at(reg_match.pat_index)?;

        // Handle Fail: attempt backtracking
        if let MatchOperation::Fail(ref name) = pat.operation {
            return self.handle_fail(
                name,
                line,
                start,
                non_consuming_push_at,
                ops,
                search_cache,
                syntax_set,
            );
        }

        // For Branch, we need to snapshot state before executing, then synthesize a Push.
        let is_branch = matches!(pat.operation, MatchOperation::Branch { .. });
        let synthetic_op;

        if is_branch {
            if let MatchOperation::Branch {
                ref name,
                ref alternatives,
                pop_count,
            } = pat.operation
            {
                // Snapshot current state.
                //
                // NOTE on field naming: `match_start` here stores the
                // position the parser should *resume* from on fail —
                // which is the branch match's end position (since the
                // parser has already consumed the match). `match_end`
                // and `pat_scope` carry the *real* match span plus the
                // keyword's own scopes so a same-line fail rewind can
                // re-emit them (they were truncated off `ops` along
                // with the alt[0]'s subsequent work).
                // When `handle_fail` is mid-replay, `self.line_number` /
                // `self.pending_lines` still reflect the *outer* current
                // line — read through `replay_ctx` so a branch born
                // during replay anchors to the virtual replay line `L+i`.
                let (bp_line_number, bp_pending_lines_snapshot_len) = match &self.replay_ctx {
                    Some(ctx) => (ctx.line_number, ctx.pending_lines_snapshot_offset),
                    None => (self.line_number.saturating_sub(1), self.pending_lines.len()),
                };
                // When this branch is born inside an outer cross-line
                // replay's `parse_line_inner_from`, the local `ops` Vec
                // is the inner re-parse's `res` — it does *not* include
                // the outer prefix the outer replay is about to splice
                // in front. Without prepending that outer prefix, a
                // later fail of *this* branch reconstructs its line
                // from an empty prefix, dropping the outer captures
                // entirely (the `[foo]:` LRD opener vanished from
                // `syntax_test_markdown.md`'s `[foo]: /url` cases when
                // a `link-def-attr-continuation` born inside the
                // `link-def-title-continuation` replay later failed).
                let prefix_ops = match &self.replay_prefix_ops {
                    Some(outer) => {
                        let mut combined = outer.clone();
                        combined.extend(ops.iter().cloned());
                        combined
                    }
                    None => ops.clone(),
                };
                let bp = BranchPoint {
                    name: name.clone(),
                    next_alternative: 1, // 0 is about to be pushed
                    alternatives: alternatives.clone(),
                    stack_snapshot: self.stack.clone(),
                    proto_starts_snapshot: self.proto_starts.clone(),
                    match_start: *start, // position before this match's advance
                    trigger_match_start: match_start,
                    pat_scope: pat.scope.clone(),
                    line_number: bp_line_number,
                    ops_snapshot_len: ops.len(),
                    stack_depth: self.stack.len(),
                    non_consuming_push_at_snapshot: *non_consuming_push_at,
                    first_line_snapshot: self.first_line,
                    with_prototype: pat.with_prototype.clone(),
                    pending_lines_snapshot_len: bp_pending_lines_snapshot_len,
                    escape_stack_snapshot: self.escape_stack.clone(),
                    pop_count,
                    prefix_ops,
                    capture_ops: pat
                        .captures
                        .as_ref()
                        .map(|m| build_capture_ops(m, &reg_match.regions))
                        .unwrap_or_default(),
                };
                self.branch_points.push(bp);
                // When pop_count > 0 (pop + branch), use Set semantics to
                // pop the current context before pushing the first alternative.
                synthetic_op = if pop_count > 0 {
                    MatchOperation::Set {
                        ctx_refs: vec![alternatives[0].clone()],
                        pop_count,
                    }
                } else {
                    MatchOperation::Push(vec![alternatives[0].clone()])
                };
            } else {
                unreachable!()
            }
        } else {
            synthetic_op = pat.operation.clone();
        }

        let op_to_use = if is_branch {
            &synthetic_op
        } else {
            &pat.operation
        };

        self.push_meta_ops(true, match_start, level_context, op_to_use, syntax_set, ops)?;
        for s in &pat.scope {
            ops.push((match_start, ScopeStackOp::Push(*s)));
        }
        let capture_ops = pat
            .captures
            .as_ref()
            .map(|m| build_capture_ops(m, &reg_match.regions))
            .unwrap_or_default();
        ops.extend(capture_ops.iter().cloned());
        if !pat.scope.is_empty() {
            ops.push((match_end, ScopeStackOp::Pop(pat.scope.len())));
        }
        self.push_meta_ops(false, match_end, level_context, op_to_use, syntax_set, ops)?;

        if is_branch {
            // Execute the synthetic Push through perform_op
            let synthetic_pat = MatchPattern::new(
                pat.has_captures,
                pat.regex.regex_str().to_string(),
                pat.scope.clone(),
                pat.captures.clone(),
                synthetic_op,
                pat.with_prototype.clone(),
            );
            self.perform_op(line, &reg_match.regions, &synthetic_pat, syntax_set)
        } else {
            self.perform_op(line, &reg_match.regions, pat, syntax_set)
        }
    }

    /// Merge a cross-line replay's per-line corrected ops into `flushed_ops`.
    ///
    /// Multiple cross-line fails can fire on a single `parse_line` call (e.g.
    /// Java line 624 with two live branches from line 615, both snapshotted at
    /// `pending_lines_snapshot_len = 0`). Each fail's replay covers
    /// `pending_lines[snap..pending_lines.len())`. A naive `extend` leaves
    /// `flushed_ops` with duplicates — consumers of `ParseLineOutput::replayed`
    /// index `replayed[i] ↔ pending_lines[i]` by `buf_len - replayed.len()`,
    /// so duplicates misalign every byte offset.
    ///
    /// Composition rule, given current start `a` and new fail's `snap`:
    /// - `snap <= a`: new fail supersedes everything; replace.
    /// - `snap > a`: keep `[a..snap)` from prior fails, replace `[snap..N)`.
    ///
    /// `bp_info` records the BP whose replay produced `new_ops`; stored
    /// alongside `flushed_ops_start` for later use by
    /// `prefer_inner_replay_corrections`. When two cross-line fails on
    /// the same `parse_line` both contribute, we keep the BP info of the
    /// one that ultimately owns the prefix (matches the start-replacement
    /// rules above).
    fn merge_flushed(
        &mut self,
        snap: usize,
        new_ops: Vec<Vec<(usize, ScopeStackOp)>>,
        bp_info: BpInfo,
    ) {
        match self.flushed_ops_start {
            None => {
                self.flushed_ops = new_ops;
                self.flushed_ops_start = Some(snap);
                self.flushed_ops_bp = Some(bp_info);
            }
            Some(start) if snap <= start => {
                self.flushed_ops = new_ops;
                self.flushed_ops_start = Some(snap);
                self.flushed_ops_bp = Some(bp_info);
            }
            Some(start) => {
                let keep = snap - start;
                self.flushed_ops.truncate(keep);
                self.flushed_ops.extend(new_ops);
            }
        }
    }

    /// Replace `replayed_ops[i]` with `inner_ops` for overlapping
    /// indices. Only fires when the inner BP was created at a stack
    /// depth less-than-or-equal to the outer BP's depth — i.e. the
    /// inner BP is a sibling-or-shallower correction at the same
    /// structural level, not a nested BP firing inside outer's resolved
    /// alternative. The "deeper inner" case is the multigen16
    /// regression seat: outer = `class-members` at depth 4, inner =
    /// `object-type` at depth 9 (nested inside outer's resolved field
    /// alt). Preferring its corrections doubled `meta.field.type.java`
    /// on `Java/syntax_test_java.java:3462+` because outer's full-line
    /// ops correctly emit one `meta.field.type` and inner's reparse
    /// adds another.
    ///
    /// The depth-equal case is the original `@A.B\n(par=1)\nenum E {}`
    /// regression seat from PR #663: outer and inner are both
    /// `declarations` at depth 3 — sibling resolutions of the same
    /// branch family, and inner's `name`-alt CORRECTLY supersedes
    /// outer's locally-computed `path`-alt freeze.
    fn prefer_inner_replay_corrections(
        outer_snap: usize,
        replayed_ops: &mut [Vec<(usize, ScopeStackOp)>],
        inner_ops: &[Vec<(usize, ScopeStackOp)>],
        inner_start: usize,
        outer_bp: &BpInfo,
        inner_bp: Option<&BpInfo>,
    ) {
        if let Some(inner) = inner_bp {
            // Substitute only when inner is at outer's exact depth
            // (sibling resolution of the same branch family) or
            // exactly one deeper (immediate refinement, e.g.
            // outer=declarations(3), inner=annotation-identifier(4)
            // on the same line — inner brings `meta.path.java` from
            // the qualified-identifier alt that outer's
            // locally-computed parse drops).
            //
            // Skip when inner is shallower OR more than one deeper:
            // - Inner shallower: inner is a fresh top-level-style
            //   BP created during outer's replay; its commitment is
            //   structurally less specific than outer's and would
            //   overwrite outer's correct refined parse with a
            //   coarser one.
            // - Inner more than one deeper (multigen16-style: outer
            //   class-members(4), inner object-type(11)): inner is
            //   nested INSIDE outer's resolved alt and its reparse
            //   adds atoms outer's alt already provides
            //   (`deeper_inner_bp_correction_does_not_double_outer_meta_scope`).
            let depth_diff = inner.stack_depth as isize - outer_bp.stack_depth as isize;
            if !(depth_diff == 0 || depth_diff == 1) {
                return;
            }
        }
        for (i, outer_local) in replayed_ops.iter_mut().enumerate() {
            let global_i = outer_snap + i;
            if global_i < inner_start {
                continue;
            }
            let inner_idx = global_i - inner_start;
            if let Some(corrected) = inner_ops.get(inner_idx) {
                *outer_local = corrected.clone();
            }
        }
    }

    /// Handle a `fail` operation by rewinding to the named branch point.
    /// Returns Ok(true) if backtracking happened (caller should continue from rewound position).
    /// Returns Ok(false) if the fail had no effect.
    fn handle_fail(
        &mut self,
        name: &str,
        line: &str,
        start: &mut usize,
        non_consuming_push_at: &mut (usize, usize),
        ops: &mut Vec<(usize, ScopeStackOp)>,
        search_cache: &mut SearchCache,
        syntax_set: &SyntaxSet,
    ) -> Result<bool, ParsingError> {
        // Find the branch point by name (most recent first), skipping
        // records whose alternative's pushed frame is no longer on
        // the stack. The alternative lives at
        // `bp.stack_depth - bp.pop_count + 1`, so `stack.len() >
        // bp.stack_depth - bp.pop_count` means the frame is still
        // present. Without this skip, a nested `branch_point` with
        // the same name whose inner alternative popped cleanly would
        // shadow an enclosing record via `rposition`, rewinding to
        // the inner branch position instead of the outer one
        // (Haskell's raw-string QQ `[r|[a-zA-Z]|]`).
        let stack_len = self.stack.len();
        let bp_index = self.branch_points.iter().rposition(|bp| {
            bp.name == name && stack_len > bp.stack_depth.saturating_sub(bp.pop_count)
        });
        let bp_index = match bp_index {
            Some(i) => i,
            None => return Ok(false), // No such branch point, fail is no-op
        };

        // During a replay recursion, `cur_line` is the virtual replay
        // line — without this override a same-line fail fired inside
        // the re-parse would be misclassified as cross-line, and a
        // fail on the outer line for a branch created during replay
        // would be misclassified as same-line.
        let cur_line = match &self.replay_ctx {
            Some(ctx) => ctx.line_number,
            None => self.line_number.saturating_sub(1),
        };
        let bp = &self.branch_points[bp_index];

        // Check validity: not >128 lines old
        if cur_line.saturating_sub(bp.line_number) > 128 {
            let bp = self.branch_points.remove(bp_index);
            self.warnings.push(format!(
                "branch point '{}' expired (exceeded 128-line rewind limit)",
                bp.name
            ));
            return Ok(false);
        }

        // Check validity: the alt frame is still on the stack. Mirrors the
        // bp lookup predicate above (`stack_len > bp.stack_depth.saturating_sub(bp.pop_count)`)
        // so a `pop: N + branch_point` whose snapshot captures the
        // pre-pop depth doesn't false-positive here. Without subtracting
        // `pop_count`, Java's `pop: 2 + branch_point: annotation-qualified-parameters`
        // failed lookup with `self.stack.len() < bp.stack_depth` and the
        // fail became a no-op, leaking `meta.annotation.identifier.java`
        // into every nested-annotation extends path.
        if self.stack.len() <= bp.stack_depth.saturating_sub(bp.pop_count) {
            self.branch_points.remove(bp_index);
            return Ok(false);
        }

        // Check if there are more alternatives
        if bp.next_alternative >= bp.alternatives.len() {
            // All alternatives exhausted: restore parser state to the
            // pre-branch snapshot so the stuck alternative's pushed
            // contexts and emitted ops are discarded, then advance
            // past the branch_point match position by one character so
            // we don't immediately re-enter the same branch_point and
            // loop. Before this, the branch_point was silently removed
            // while its last alternative's contexts remained on the
            // stack — the cause of the "scope stack stays in
            // `meta.interpolation.brace.shell`" cascade in Zsh when
            // both `brace-interpolation-sequence` and
            // `brace-interpolation-series` failed and there was no
            // fallback alternative (Zsh explicitly excludes
            // `brace-interpolation-fallback`).
            //
            // Cross-line exhaustion takes the same shape, plus a replay
            // of the buffered lines under the pre-branch state so
            // callers see corrected ops for lines they've already been
            // handed. Without this, the unterminated TypeScript type
            // expression at `sublimehq/Packages#3598`
            // (`type x = { bar: (cb: (\n};`) left the inner
            // `ts-type-function-parameter-list-body` on the stack
            // forever, contaminating every subsequent line's scope
            // stack with `meta.type.js, meta.group.js` — 274 cascading
            // assertion failures in `syntax_test_typescript.ts`.
            let is_cross_line = bp.line_number < cur_line;
            let bp_line_number = bp.line_number;
            let stack_snapshot = bp.stack_snapshot.clone();
            let proto_starts_snapshot = bp.proto_starts_snapshot.clone();
            let escape_stack_snapshot = bp.escape_stack_snapshot.clone();
            let first_line_snapshot = bp.first_line_snapshot;
            let non_consuming_push_at_snapshot = bp.non_consuming_push_at_snapshot;
            let ops_snapshot_len = bp.ops_snapshot_len;
            let match_start_pos = bp.match_start;
            let pending_lines_snapshot_len = bp.pending_lines_snapshot_len;
            let prefix_ops = bp.prefix_ops.clone();
            let outer_bp_info = BpInfo {
                name: bp.name.clone(),
                stack_depth: bp.stack_depth,
                line_number: bp.line_number,
            };
            self.branch_points.remove(bp_index);

            self.stack = stack_snapshot;
            self.proto_starts = proto_starts_snapshot;
            self.escape_stack = escape_stack_snapshot;
            self.first_line = first_line_snapshot;
            *non_consuming_push_at = non_consuming_push_at_snapshot;
            ops.truncate(ops_snapshot_len.min(ops.len()));

            if is_cross_line {
                // Re-parse each buffered line under the restored (pre-branch)
                // state so `parse_line` can surface the corrected ops via
                // `ParseLineOutput::replayed`. The first buffered line is the
                // branch-creation line: emit its saved `prefix_ops` (the ops
                // emitted before the branch match) verbatim, then advance past
                // the branch match by one character before resuming — otherwise
                // the same branch_point would fire again at the original match
                // position and we'd loop.
                //
                // Keep `pending_lines` intact (don't drain): if an outer
                // branch_point on this same line also fails after this
                // exhaustion replay, its own replay needs access to the same
                // buffered lines.
                let truncated_lines: Vec<String> =
                    self.pending_lines[pending_lines_snapshot_len..].to_vec();
                // Save prior flushed_ops state and clear it so any nested
                // cross-line fails firing during the replay loop below
                // write into a clean slot we can detect afterward.
                let saved_flushed = std::mem::take(&mut self.flushed_ops);
                let saved_flushed_start = self.flushed_ops_start.take();
                let saved_flushed_bp = self.flushed_ops_bp.take();
                let mut replayed_ops: Vec<Vec<(usize, ScopeStackOp)>> =
                    Vec::with_capacity(truncated_lines.len());
                for (i, replay_line) in truncated_lines.iter().enumerate() {
                    // Tag branches created during this iteration with
                    // the replay line's identity, not the outer line's.
                    let prev_replay_ctx = self.replay_ctx.replace(ReplayCtx {
                        line_number: bp_line_number + i,
                        pending_lines_snapshot_offset: pending_lines_snapshot_len + i,
                    });
                    // No new-alt construction here (all alternatives
                    // exhausted), so the first-line prefix is just
                    // `prefix_ops`. Surface it to inner branch creations
                    // so their `prefix_ops` keeps the outer captures.
                    let prev_replay_prefix = if i == 0 {
                        self.replay_prefix_ops.replace(prefix_ops.clone())
                    } else {
                        self.replay_prefix_ops.take()
                    };
                    let inner_result = if i == 0 {
                        self.skipped_branches
                            .push((match_start_pos, name.to_string()));
                        self.parse_line_inner_from(replay_line, syntax_set, match_start_pos)
                    } else {
                        self.parse_line_inner(replay_line, syntax_set)
                    };
                    self.replay_ctx = prev_replay_ctx;
                    self.replay_prefix_ops = prev_replay_prefix;
                    let tail_ops = inner_result?;
                    let line_ops = if i == 0 {
                        let mut first_line_ops = prefix_ops.clone();
                        first_line_ops.extend(tail_ops);
                        first_line_ops
                    } else {
                        tail_ops
                    };
                    replayed_ops.push(line_ops);
                }
                // Capture inner corrections (if any), restore prior state,
                // then prefer the inner corrections for overlapping indices.
                let inner_corrections = std::mem::take(&mut self.flushed_ops);
                let inner_corrections_start = self.flushed_ops_start.take();
                let inner_corrections_bp = self.flushed_ops_bp.take();
                self.flushed_ops = saved_flushed;
                self.flushed_ops_start = saved_flushed_start;
                self.flushed_ops_bp = saved_flushed_bp;
                if let Some(start) = inner_corrections_start {
                    if !inner_corrections.is_empty() {
                        Self::prefer_inner_replay_corrections(
                            pending_lines_snapshot_len,
                            &mut replayed_ops,
                            &inner_corrections,
                            start,
                            &outer_bp_info,
                            inner_corrections_bp.as_ref(),
                        );
                    }
                }
                self.merge_flushed(
                    pending_lines_snapshot_len,
                    replayed_ops,
                    outer_bp_info.clone(),
                );

                // Restart the current line from the beginning under the
                // restored state.
                ops.clear();
                *start = 0;
                *non_consuming_push_at = (0, 0);
                search_cache.clear();
                return Ok(true);
            }

            // Same-line exhaustion: rewind the cursor to the BP's
            // original position and record the branch_point's name so
            // subsequent `find_best_match` calls at that position skip
            // the same-name Branch pattern. This lets the parent
            // context's NEXT rule fire instead of advancing past the
            // lookahead match — mirrors ST's branch-point exhaustion
            // semantics. The previous behaviour (advance one char) let
            // stale keyword rules match in the middle of identifiers,
            // e.g. `package` inside `$package` after `declarations`
            // exhausted on the leading `$`. If `find_best_match`
            // returns nothing at this cursor, `parse_next_token`'s
            // no-match fallback advances one char as a last resort.
            self.skipped_branches
                .push((match_start_pos, name.to_string()));
            *start = match_start_pos;
            search_cache.clear();
            return Ok(true);
        }

        // Determine if this is a cross-line fail (branch was created on a previous line).
        let is_cross_line = bp.line_number < cur_line;

        // Extract everything we need from bp before mutating self.
        let bp_line_number = bp.line_number;
        let next_alt_index = bp.next_alternative;
        let next_alt = bp.alternatives[next_alt_index].clone();
        let match_start_pos = bp.match_start;
        let trigger_match_start = bp.trigger_match_start;
        let trigger_pat_scope = bp.pat_scope.clone();
        let trigger_capture_ops = bp.capture_ops.clone();
        let stack_snapshot = bp.stack_snapshot.clone();
        let proto_starts_snapshot = bp.proto_starts_snapshot.clone();
        let first_line_snapshot = bp.first_line_snapshot;
        let non_consuming_push_at_snapshot = bp.non_consuming_push_at_snapshot;
        let ops_snapshot_len = bp.ops_snapshot_len;
        let pending_lines_snapshot_len = bp.pending_lines_snapshot_len;
        let escape_stack_snapshot = bp.escape_stack_snapshot.clone();
        let prefix_ops = bp.prefix_ops.clone();
        let outer_bp_info = BpInfo {
            name: bp.name.clone(),
            stack_depth: bp.stack_depth,
            line_number: bp.line_number,
        };
        // bp borrow ends here.

        let pop_count = self.branch_points[bp_index].pop_count;

        // Restore parser state to the snapshot.
        // Keep `stack_snapshot` available — the same-line fix below needs
        // it to compute popped-context meta_scope clearance via
        // `push_meta_ops`.
        self.stack = stack_snapshot.clone();
        self.proto_starts = proto_starts_snapshot;
        self.escape_stack = escape_stack_snapshot;
        self.first_line = first_line_snapshot;
        *non_consuming_push_at = non_consuming_push_at_snapshot;

        // Update the branch point record before popping/pushing
        // (must happen before the pop which may invalidate indices).
        self.branch_points[bp_index].next_alternative = next_alt_index + 1;

        // For pop + branch: re-pop the contexts (snapshot was taken pre-pop).
        if pop_count > 0 {
            for _ in 0..pop_count {
                self.stack.pop();
            }
        }

        // Push the next alternative onto the stack.
        let with_prototype = self.branch_points[bp_index].with_prototype.clone();
        let context_id = next_alt.id()?;
        let captures = None; // no captures available at rewind time

        let proto_ids = match with_prototype {
            Some(ref p) => vec![p.id()?],
            None => Vec::new(),
        };

        self.stack.push(StateLevel {
            context: context_id,
            prototypes: proto_ids,
            captures,
        });

        if is_cross_line {
            // Cross-line fail: the ops for lines since the branch was created
            // have already been returned to callers.  Re-parse those lines under
            // the new alternative and store the corrected ops in `flushed_ops`
            // so that `parse_line` can surface them via `ParseLineOutput::replayed`.
            //
            // The first buffered line is the branch-creation line. Its
            // pre-branch prefix (cols 0..trigger_match_start) was correctly
            // parsed under the *pre-branch* state — not the new alternative.
            // Re-parsing it from column 0 with the new alternative on the
            // stack would misattribute that prefix to the new alternative's
            // rules (observed on multi-line SQL `LIKE … ESCAPE …`: every
            // non-whitespace before `LIKE` fires `else-pop` in the
            // escape-alternative, derailing the stack). Instead, reuse the
            // prefix_ops saved at branch-creation time, manually emit the
            // branch trigger's pat.scope and the new alternative's meta
            // scope ops, then resume parsing from match_end with the new
            // alternative on the stack via `parse_line_inner_from`.
            // Keep `pending_lines` intact (don't drain): if a second branch_point
            // on the current line also fails after this retry, its own replay
            // needs access to the same buffered lines. Nested branches from the
            // same earlier line share the buffer.
            let truncated_lines: Vec<String> =
                self.pending_lines[pending_lines_snapshot_len..].to_vec();

            // Compose the first replayed line's prefix (outer prefix_ops +
            // new-alt meta/pat/capture/meta_content emission) up front so a
            // branch_point born inside the inner re-parse can inherit it
            // via `self.replay_prefix_ops`. Built once per fail; cloned
            // and extended with `tail_ops` to form the final line_ops.
            //
            // Use `push_meta_ops` with a synthetic Set/Push for the
            // new alternative — same path the same-line fail above and
            // the original branch creation take — so both the new
            // alternative's own meta scopes AND the popped contexts'
            // meta_scope/mcs clearance Pop (for `pop: N + branch_point`,
            // N > 0) get re-emitted. A bespoke re-emit of just
            // `context.meta_scope` / `context.meta_content_scope` is
            // missing the popped-contexts Pop, leaving Java's
            // `pop: 2 + branch_point: annotation-qualified-parameters`
            // crossing a line boundary with both the popped context's
            // meta_scope (`meta.annotation.identifier.java`) AND the
            // outer declaration's meta_scope (`meta.enum.java` /
            // `meta.class.java` / `meta.interface.java`) leaked on the
            // stack — cascading across 8000+ lines past the multi-line
            // annotation-modified declaration at lines 2260-2297 of
            // `syntax_test_java.java`.
            //
            // `push_meta_ops` reads `self.stack` to compute the popped
            // contexts' scope atoms, so swap in `stack_snapshot` (pre-pop
            // state captured at branch creation) for the duration of the
            // calls — `self.stack` currently holds the post-set state
            // (alt N already pushed).
            let mut first_line_prefix = prefix_ops.clone();
            let synthetic_op_alt_n = if pop_count > 0 {
                MatchOperation::Set {
                    ctx_refs: vec![next_alt.clone()],
                    pop_count,
                }
            } else {
                MatchOperation::Push(vec![next_alt.clone()])
            };
            let level_ctx_id = stack_snapshot.last().map(|l| l.context);
            let post_set_stack = std::mem::replace(&mut self.stack, stack_snapshot.clone());
            if let Some(level_ctx_id) = level_ctx_id {
                let level_context = syntax_set.get_context(&level_ctx_id)?;
                self.push_meta_ops(
                    true,
                    trigger_match_start,
                    level_context,
                    &synthetic_op_alt_n,
                    syntax_set,
                    &mut first_line_prefix,
                )?;
                for scope in &trigger_pat_scope {
                    first_line_prefix.push((trigger_match_start, ScopeStackOp::Push(*scope)));
                }
                first_line_prefix.extend(trigger_capture_ops.iter().cloned());
                if !trigger_pat_scope.is_empty() {
                    first_line_prefix
                        .push((match_start_pos, ScopeStackOp::Pop(trigger_pat_scope.len())));
                }
                self.push_meta_ops(
                    false,
                    match_start_pos,
                    level_context,
                    &synthetic_op_alt_n,
                    syntax_set,
                    &mut first_line_prefix,
                )?;
            }
            self.stack = post_set_stack;

            // Save prior flushed_ops state and clear it so any nested
            // cross-line fails firing during the replay loop below write
            // into a clean slot we can detect afterward.
            let saved_flushed = std::mem::take(&mut self.flushed_ops);
            let saved_flushed_start = self.flushed_ops_start.take();
            let saved_flushed_bp = self.flushed_ops_bp.take();

            let mut replayed_ops: Vec<Vec<(usize, ScopeStackOp)>> =
                Vec::with_capacity(truncated_lines.len());
            for (i, replay_line) in truncated_lines.iter().enumerate() {
                // Tag branches created during this iteration with the
                // replay line's identity, not the outer line's.
                let prev_replay_ctx = self.replay_ctx.replace(ReplayCtx {
                    line_number: bp_line_number + i,
                    pending_lines_snapshot_offset: pending_lines_snapshot_len + i,
                });
                // Expose the first-line prefix so a branch_point born
                // during this line's re-parse anchors its `prefix_ops`
                // to the full line state — outer captures included.
                // Subsequent replayed lines start fresh.
                let prev_replay_prefix = if i == 0 {
                    self.replay_prefix_ops.replace(first_line_prefix.clone())
                } else {
                    self.replay_prefix_ops.take()
                };
                let inner_result = if i == 0 {
                    self.parse_line_inner_from(replay_line, syntax_set, match_start_pos)
                } else {
                    self.parse_line_inner(replay_line, syntax_set)
                };
                self.replay_ctx = prev_replay_ctx;
                self.replay_prefix_ops = prev_replay_prefix;
                let tail_ops = inner_result?;
                let line_ops = if i == 0 {
                    let mut first_line_ops = first_line_prefix.clone();
                    first_line_ops.extend(tail_ops);
                    first_line_ops
                } else {
                    tail_ops
                };
                replayed_ops.push(line_ops);
            }
            // Capture inner corrections (if any), restore prior state, then
            // prefer the inner corrections for overlapping indices. Without
            // this, the outer's locally-computed `replayed_ops[i]` for
            // indices an inner cross-line fail later corrected (during a
            // later iteration of this same loop) would silently overwrite
            // the inner's more accurate correction in `flushed_ops` —
            // observed on Java's `@A.B\n(par=1)\nenum E {}` where the
            // outer `declarations` cross-line replay's line-1 ops froze the
            // dotted annotation as `path` alt before the inner
            // `annotation-qualified-identifier` cross-line fail's `name`-alt
            // resolution arrived (during line-2 reparse).
            let inner_corrections = std::mem::take(&mut self.flushed_ops);
            let inner_corrections_start = self.flushed_ops_start.take();
            let inner_corrections_bp = self.flushed_ops_bp.take();
            self.flushed_ops = saved_flushed;
            self.flushed_ops_start = saved_flushed_start;
            self.flushed_ops_bp = saved_flushed_bp;
            if let Some(start) = inner_corrections_start {
                if !inner_corrections.is_empty() {
                    Self::prefer_inner_replay_corrections(
                        pending_lines_snapshot_len,
                        &mut replayed_ops,
                        &inner_corrections,
                        start,
                        &outer_bp_info,
                        inner_corrections_bp.as_ref(),
                    );
                }
            }
            self.merge_flushed(
                pending_lines_snapshot_len,
                replayed_ops,
                outer_bp_info.clone(),
            );

            // Restart the current line from the beginning.
            ops.clear();
            *start = 0;
            *non_consuming_push_at = (0, 0);

            // Guard: the replayed `parse_line_inner` calls above can
            // mutate `self.branch_points` (adding new branches,
            // removing expired or exhausted ones), which can shift or
            // invalidate `bp_index`. Indexing with the stale position
            // previously panicked outright on files that exercise
            // nested cross-line branching (observed on
            // `JavaScript/syntax_test_js.js` and
            // `syntax_test_typescript.ts`). Skip the bookkeeping if
            // the branch point has been removed — the replay already
            // completed, which is the essential work of the fail.
            if bp_index < self.branch_points.len() {
                self.branch_points[bp_index].ops_snapshot_len = 0;
            }
        } else {
            // Same-line fail: truncate ops back to the snapshot point and rewind.
            ops.truncate(ops_snapshot_len.min(ops.len()));
            // Empty-line continuation: when the same-line fail fires at
            // pos 0 of an empty line (just `\n`), the alt-N replacement
            // is typically a `match: '' pop: N` (e.g. Markdown's
            // `link-def-attr-continuation` failing into
            // `immediately-pop2`). Executing that pop at pos 0 emits
            // visible scope Pops on the empty line's only character —
            // collapsing the parent meta_scope (e.g.
            // `meta.link.reference.def.markdown`) before the empty
            // line's own scope is recorded. Advance to past-EOL so the
            // pop emits there instead, which ScopeRegionIterator wraps
            // to the next line's baseline. ST's behavior matches: the
            // LRD pop straddles line 3→line 4, not line 2→line 3.
            let resume = if match_start_pos == 0 && line.len() <= 1 && line.trim().is_empty() {
                line.len()
            } else {
                match_start_pos
            };
            *start = resume;

            // Keep `ops_snapshot_len` pointing at the pre-branch state.
            // Subsequent fails on the same branch_point must truncate
            // back to *here* — not to the position after the pat.scope
            // re-emit below — otherwise a second fail would preserve
            // the first re-emit's (Push at trigger_match_start) while
            // appending another re-emit, producing the disordered
            // sequence (trigger, Push), (match_end, Pop), (trigger,
            // Push), (match_end, Pop). `ScopeRegionIterator` then
            // panics in `easy.rs` because position goes backwards.
            self.branch_points[bp_index].ops_snapshot_len = ops.len();

            // Re-emit the branch_point match's own scopes over their
            // original span. Without this, keywords that trigger a
            // branch (e.g. `LIKE` with
            // `scope: keyword.operator.comparison.sql`) lose their
            // scope whenever alt[0] fails and alt[1..] succeeds,
            // because the original Push/Pop pair was truncated off
            // `ops` together with alt[0]'s subsequent work.
            //
            // Use `push_meta_ops` with a synthetic Set/Push for the
            // new alternative — same path the original branch creation
            // takes — so both the new alternative's own meta scopes
            // AND the popped contexts' meta_scope/mcs clearance Pop
            // (for `pop: N + branch_point`, N > 0) get re-emitted.
            // A bespoke re-emit of just `context.meta_scope` /
            // `context.meta_content_scope` was missing the
            // popped-contexts Pop, leaving Java's
            // `pop: 2 + branch_point: annotation-qualified-parameters`
            // with `meta.annotation.identifier.java meta.path.java`
            // (annotation-qualified-identifier's `meta_scope`) leaked
            // on the stack after the branch_point's first alt failed
            // and the second alt (`immediately-pop`) ran.
            //
            // `push_meta_ops` reads `self.stack` to compute the
            // popped contexts' scope atoms, so swap in `stack_snapshot`
            // (pre-pop state captured at branch creation) for the
            // duration of the calls — `self.stack` currently holds the
            // post-set state (alt N already pushed).
            let synthetic_op_alt_n = if pop_count > 0 {
                MatchOperation::Set {
                    ctx_refs: vec![next_alt.clone()],
                    pop_count,
                }
            } else {
                MatchOperation::Push(vec![next_alt.clone()])
            };
            let level_ctx_id = stack_snapshot.last().map(|l| l.context);
            let post_set_stack = std::mem::replace(&mut self.stack, stack_snapshot.clone());
            if let Some(level_ctx_id) = level_ctx_id {
                let level_context = syntax_set.get_context(&level_ctx_id)?;
                self.push_meta_ops(
                    true,
                    trigger_match_start,
                    level_context,
                    &synthetic_op_alt_n,
                    syntax_set,
                    ops,
                )?;
                for scope in &trigger_pat_scope {
                    ops.push((trigger_match_start, ScopeStackOp::Push(*scope)));
                }
                // Captures emitted alongside the original pat.scope (e.g.
                // `keyword.declaration.data.haskell` on the first capture of
                // `(data)(?:\s+(family|instance))?`) were truncated off with
                // alt[0]'s ops. Re-emit them inside the pat_scope brackets so
                // the keyword scope survives the branch swap.
                ops.extend(trigger_capture_ops.iter().cloned());
                if !trigger_pat_scope.is_empty() {
                    ops.push((match_start_pos, ScopeStackOp::Pop(trigger_pat_scope.len())));
                }
                self.push_meta_ops(
                    false,
                    match_start_pos,
                    level_context,
                    &synthetic_op_alt_n,
                    syntax_set,
                    ops,
                )?;
            }
            self.stack = post_set_stack;
        }

        // Clear search cache since we're rewinding.
        search_cache.clear();

        Ok(true)
    }

    /// Get the syntax version for the current parse state
    fn current_syntax_version(&self, syntax_set: &SyntaxSet) -> u32 {
        if let Some(level) = self.stack.last() {
            let syntax_index = level.context.syntax_index;
            syntax_set
                .syntaxes()
                .get(syntax_index)
                .map_or(1, |s| s.version)
        } else {
            1
        }
    }

    fn push_meta_ops(
        &self,
        initial: bool,
        index: usize,
        cur_context: &Context,
        match_op: &MatchOperation,
        syntax_set: &SyntaxSet,
        ops: &mut Vec<(usize, ScopeStackOp)>,
    ) -> Result<(), ParsingError> {
        let version = self.current_syntax_version(syntax_set);
        // println!("metas ops for {:?}, initial: {}",
        //          match_op,
        //          initial);
        // println!("{:?}", cur_context.meta_scope);
        match *match_op {
            MatchOperation::Pop(n) => {
                // For `pop: N` with N > 1, every context being popped
                // contributes scope atoms on the scope stack that must
                // be unwound in LIFO order. The TOP context's trigger
                // text must not see its own `meta_content_scope`, so
                // that one is popped in the initial phase; all other
                // scope unwinding (top context's `meta_scope`, then
                // each deeper context's `meta_content_scope` followed
                // by its `meta_scope`) happens in the non-initial
                // phase, immediately after the match text's own
                // scope has been popped.
                //
                // Before this fix only the top context's scopes were
                // ever popped, leaving the N-1 deeper contexts'
                // `meta_scope` / `meta_content_scope` atoms orphaned —
                // the cause of the "scope stack grows unboundedly"
                // cascade in Makefile and Zsh (Category A).
                let stack_len = self.stack.len();
                let pop_count = n.min(stack_len);
                if initial {
                    // v2: if the context immediately below the top has
                    // embed_scope_replaces, cur_context's meta_content_scope
                    // was never pushed, so don't generate a Pop for it.
                    let skip = version >= 2
                        && stack_len >= 2
                        && syntax_set
                            .get_context(&self.stack[stack_len - 2].context)
                            .map(|c| c.embed_scope_replaces)
                            .unwrap_or(false);
                    if !skip && !cur_context.meta_content_scope.is_empty() {
                        ops.push((
                            index,
                            ScopeStackOp::Pop(cur_context.meta_content_scope.len()),
                        ));
                    }
                } else {
                    // Top context's meta_scope comes off first (it sat
                    // immediately below the trigger text's scope on the
                    // stack).
                    if !cur_context.meta_scope.is_empty() {
                        ops.push((index, ScopeStackOp::Pop(cur_context.meta_scope.len())));
                    }
                    // Restore cur_context's `clear_scopes` BEFORE
                    // popping deeper contexts. The Clear hid the
                    // deeper context's meta_scope/mcs atoms; with the
                    // Restore deferred to the very end, the
                    // depth-loop's `Pop(deep.meta_scope.len())` would
                    // pop visible-stack scopes that don't belong to
                    // the deeper context — observed on Java's
                    // `case DayType when -> "incomplete"`, where
                    // `case-label-expression`'s `clear_scopes: 1`
                    // cleared `case-label`'s `meta.case.java` and
                    // `case-label-end`'s `pop: 2` then popped the
                    // surrounding `meta.block.java` (switch's block)
                    // off the consumer's stack instead.
                    if cur_context.clear_scopes.is_some() {
                        ops.push((index, ScopeStackOp::Restore))
                    }
                    // Each deeper context's scopes are popped in
                    // top-to-bottom order: meta_content_scope first
                    // (pushed after its own meta_scope, hence above on
                    // the stack), then meta_scope, then any Restore
                    // paired with that context's own `clear_scopes`
                    // (mirrors the push order Clear → meta_scope → mcs
                    // in reverse).
                    for depth in 1..pop_count {
                        let level_idx = stack_len - 1 - depth;
                        let ctx = syntax_set.get_context(&self.stack[level_idx].context)?;
                        let skip_content = version >= 2
                            && level_idx >= 1
                            && syntax_set
                                .get_context(&self.stack[level_idx - 1].context)
                                .map(|c| c.embed_scope_replaces)
                                .unwrap_or(false);
                        if !skip_content && !ctx.meta_content_scope.is_empty() {
                            ops.push((index, ScopeStackOp::Pop(ctx.meta_content_scope.len())));
                        }
                        if !ctx.meta_scope.is_empty() {
                            ops.push((index, ScopeStackOp::Pop(ctx.meta_scope.len())));
                        }
                        if ctx.clear_scopes.is_some() {
                            ops.push((index, ScopeStackOp::Restore));
                        }
                    }
                }
            }
            // for some reason the ST3 behaviour of set is convoluted and is inconsistent with the docs and other ops
            // - the meta_content_scope of the current context is applied to the matched thing, unlike pop
            // - the clear_scopes are applied after the matched token, unlike push
            // - the interaction with meta scopes means that the token has the meta scopes of both the current scope and the new scope.
            MatchOperation::Push(ref context_refs)
            | MatchOperation::Set {
                ctx_refs: ref context_refs,
                ..
            } => {
                let is_set = matches!(*match_op, MatchOperation::Set { .. });
                let set_pop_count = match *match_op {
                    MatchOperation::Set { pop_count, .. } => pop_count.max(1),
                    _ => 1,
                };
                // a match pattern that "set"s keeps the meta_content_scope and meta_scope from the previous context
                if initial {
                    // v2: pop the USER-DECLARED part of cur.mcs so the
                    // matched text doesn't see it. The AUTO-INJECTED
                    // top-level scope (added to `main.meta_content_scope[0]`
                    // by `add_initial_contexts`) stays on the stack across
                    // the trigger — verified against ST 4200 stable on
                    // TOML's `[section]` rule, where the `[` trigger sees
                    // `source.toml` (TOML main's auto-injected mcs)
                    // alongside the trigger's own `meta.section.toml`. The
                    // distinction matches ST's documented v2 set behavior:
                    // the matched text doesn't inherit cur.mcs, but the
                    // file's top-level scope is conceptually always on.
                    //
                    // Skip when cur_context's mcs was never pushed because
                    // the context immediately below has
                    // `embed_scope_replaces` (the embedded syntax's main
                    // mcs is suppressed in favor of `embed_scope` on the
                    // wrapper). Without this, the Pop takes off the
                    // topmost wrapper-pushed scope — observed on Markdown
                    // bash fenced blocks where `source.shell.bash` (the
                    // last embed_scope token) was disappearing on the
                    // embedded main's first `set:` rule.
                    let stack_len = self.stack.len();
                    let skip_cur_mcs_pop = version >= 2
                        && stack_len >= 2
                        && syntax_set
                            .get_context(&self.stack[stack_len - 2].context)
                            .map(|c| c.embed_scope_replaces)
                            .unwrap_or(false);
                    if is_set
                        && version >= 2
                        && !cur_context.meta_content_scope.is_empty()
                        && !skip_cur_mcs_pop
                    {
                        // Identify the auto-injected top-level scope: the
                        // syntax's `scope:` directive lands at
                        // `main.meta_content_scope[0]` via
                        // `add_initial_contexts`. Compare cur.mcs[0] to
                        // the syntax's top-level scope; if they match,
                        // exclude position 0 from the Pop so the matched
                        // text retains the file scope.
                        let cur_syntax_idx = self.stack[stack_len - 1].context.syntax_index;
                        let top_level_scope =
                            syntax_set.syntaxes().get(cur_syntax_idx).map(|s| s.scope);
                        let mut pop_count = cur_context.meta_content_scope.len();
                        if top_level_scope == cur_context.meta_content_scope.first().copied() {
                            pop_count -= 1;
                        }
                        if pop_count > 0 {
                            ops.push((index, ScopeStackOp::Pop(pop_count)));
                        }
                    }
                    // NOTE: cur_context.clear_scopes Restore is emitted in the
                    // non-initial phase below, AFTER Pop(cur.meta_scope + target.meta_scope)
                    // has run. Restoring here (pre-match) would place the cleared
                    // scopes on top of the stack above the target.meta_scope push,
                    // and the non-initial Pop would then remove the restored scopes
                    // instead of the intended meta_scopes — dropping cur's cleared
                    // state on the floor. Observed as duplicate
                    // `meta.mapping.value.json` atoms in nested JSON objects.
                    // add each context's meta scope
                    if version >= 2 {
                        // Push: emit Clear for every pushed context that has
                        // `clear_scopes`, at its own index position in the
                        // push order — same as v1. Sublime permits at most
                        // one `clear_scopes` per push list, but when it sits
                        // on a non-topmost entry (e.g. Python's
                        // `f-string-replacement-meta` at index 0 of a
                        // 3-context push) restricting to `i == last_idx`
                        // silently drops it, leaking the parent's
                        // `meta_content_scope` atoms into interpolation
                        // content.
                        //
                        // Single-context `set:` with clear_scopes on the
                        // target: emit Clear here — before target.meta_scope
                        // is pushed and before the trigger match scope — so
                        // the matched text sees the cleared stack.
                        // Observed on Lisp's `(defun fn (...)`: the
                        // parameter-list `(` otherwise kept the enclosing
                        // `meta.function.lisp` alongside
                        // `meta.function.parameters.lisp` because Clear
                        // fired only after the match in the non-initial
                        // phase. See
                        // `v2_set_to_target_with_clear_scopes_clears_parent_meta_content_scope`.
                        //
                        // Exception: when cur_context itself carries
                        // `meta_scope` / `meta_content_scope`, those atoms sit
                        // on top of the visible stack at this point and the
                        // initial-phase Clear would hide them. The non-initial
                        // Pop (sized by cur.ms.len() + target.ms.len()) would
                        // then pop the wrong atoms (from below cur's ms),
                        // and the Restore that follows would resurrect cur's
                        // ms back onto the stack instead of the parent atoms
                        // the Clear was meant to hide. Defer the Clear into
                        // the non-initial phase (after Pop+Restore) so it
                        // hides parent atoms, not cur's. Bash's
                        // `tilde-modifier` (clear+ms) → set:
                        // `tilde-modifier-username` (clear+mcs) is the
                        // canonical instance. See
                        // `cur_meta_scope_set_to_target_with_clear_scopes`.
                        //
                        // Multi-context `set:` keeps Clear in the non-initial
                        // phase (emitted inline after preceding contexts'
                        // mcs pushes). Moving it to the initial phase here
                        // would strip atoms from below the outer mcs rather
                        // than from the top of the just-pushed inner mcs
                        // stack — Makefile's `set: [value-to-be-defined,
                        // eat-whitespace-then-pop]` relies on Clear eating
                        // the last-pushed mcs atom, which Restore then
                        // replaces when eat-whitespace-then-pop pops.
                        let cur_has_meta = !cur_context.meta_scope.is_empty()
                            || !cur_context.meta_content_scope.is_empty();
                        let single_context_set_clear =
                            is_set && context_refs.len() == 1 && !cur_has_meta;
                        // Multi-context `set:` whose target body declares
                        // `clear_scopes: N` AND a non-empty `meta_scope`,
                        // with cur empty: ST drops one EXTRA atom beyond
                        // Clear(N) on the trigger token. The body content
                        // sees only Clear(N) atoms gone. Observed on PHP
                        // `function bye(): never {` — at the `:`, ST drops
                        // both `meta.function.php` (function-block's mcs,
                        // what Clear(1) would clear) AND the next-deeper
                        // `source.php.embedded.html` (the embed wrapper's
                        // mcs); syntect previously kept both, leaking
                        // nested `meta.function.php` /
                        // `meta.function.return-type.php` into the colon.
                        // See `php_multi_set_target_clear_drops_extra_parent_mcs_on_trigger`.
                        //
                        // The target's `meta_scope` non-emptiness is what
                        // anchors the extra drop on the trigger: that ms
                        // is pushed on top of the trigger and asks ST to
                        // strip one more parent atom below Clear's reach.
                        // Targets with `meta_content_scope` only (e.g.
                        // Zsh's `zsh-redirection-glob-range-end`, which has
                        // `clear_scopes: 1` + `meta_content_scope` but no
                        // `meta_scope`) must NOT trigger this — there's no
                        // ms to anchor the extra drop on the trigger token,
                        // so doing it strips fundamental scopes
                        // (`source.shell.zsh`,
                        // `meta.function-call.arguments.shell`) that ST
                        // keeps.
                        let target_clear_amt = if is_set {
                            context_refs.iter().find_map(|r| {
                                r.resolve(syntax_set)
                                    .ok()
                                    .and_then(|c| match c.clear_scopes {
                                        Some(ClearAmount::TopN(n))
                                            if n > 0 && !c.meta_scope.is_empty() =>
                                        {
                                            Some(n)
                                        }
                                        _ => None,
                                    })
                            })
                        } else {
                            None
                        };
                        let cur_inert = !cur_has_meta && cur_context.clear_scopes.is_none();
                        let multi_set_extra_drop = is_set
                            && set_pop_count == 1
                            && context_refs.len() > 1
                            && cur_inert
                            && target_clear_amt.is_some();
                        if let (true, Some(amt)) = (multi_set_extra_drop, target_clear_amt) {
                            ops.push((index, ScopeStackOp::Clear(ClearAmount::TopN(amt + 1))));
                        }
                        // Multi-context `set:` whose non-topmost target has
                        // `clear_scopes: N` + an empty `meta_scope`
                        // (`meta_content_scope`-only): ST applies the Clear
                        // to atoms that EARLIER targets pushed via their
                        // `meta_scope`, and the strip is visible to the
                        // trigger token's own scopes — observed on Zsh's
                        // `zsh-redirection-glob-range-begin`'s
                        //   set: [string-path-pattern-body,
                        //         zsh-redirection-glob-range-end, …]
                        // where `string-path-pattern-body` pushes
                        // `meta.string.glob.shell string.unquoted.shell` and
                        // `…-range-end` declares `clear_scopes: 1`. The
                        // capture-2 scope `meta.range.shell.zsh
                        // punctuation.definition.range.begin.shell.zsh` on
                        // the `<` is asserted with `- string`, so
                        // `string.unquoted.shell` must be hidden at the
                        // trigger. Without this preview Clear it leaks into
                        // every glob-range opening. The matching Restore is
                        // emitted at the start of the non-initial phase
                        // below so `head_pop` finds the full visible stack.
                        // See
                        // `v2_multi_set_non_topmost_clear_scopes_strips_preceding_meta_scope_at_trigger`.
                        let mut initial_atoms_pushed: usize = 0;
                        for r in context_refs.iter() {
                            let ctx = r.resolve(syntax_set)?;

                            if is_set && context_refs.len() > 1 && ctx.meta_scope.is_empty() {
                                if let Some(ClearAmount::TopN(n)) = ctx.clear_scopes {
                                    let initial_clear = n.min(initial_atoms_pushed);
                                    if initial_clear > 0 {
                                        ops.push((
                                            index,
                                            ScopeStackOp::Clear(ClearAmount::TopN(initial_clear)),
                                        ));
                                        initial_atoms_pushed -= initial_clear;
                                    }
                                }
                            }

                            let emit_clear_here = !is_set || single_context_set_clear;
                            if emit_clear_here {
                                if let Some(clear_amount) = ctx.clear_scopes {
                                    ops.push((index, ScopeStackOp::Clear(clear_amount)));
                                }
                            }

                            for scope in ctx.meta_scope.iter() {
                                ops.push((index, ScopeStackOp::Push(*scope)));
                                initial_atoms_pushed += 1;
                            }
                        }
                    } else {
                        for r in context_refs.iter() {
                            let ctx = r.resolve(syntax_set)?;

                            if !is_set {
                                if let Some(clear_amount) = ctx.clear_scopes {
                                    ops.push((index, ScopeStackOp::Clear(clear_amount)));
                                }
                            }

                            for scope in ctx.meta_scope.iter() {
                                ops.push((index, ScopeStackOp::Push(*scope)));
                            }
                        }
                    }
                } else {
                    // `pop: N + set:` (set_pop_count > 1) unwinds N-1 deeper
                    // contexts in addition to the usual set-replace semantics;
                    // their meta_scope / meta_content_scope atoms sitting on
                    // the scope stack must be popped off, so force repush to
                    // fire even if the immediate contexts had no mcs/ms.
                    let repush = (is_set
                        && (set_pop_count > 1
                            || !cur_context.meta_scope.is_empty()
                            || !cur_context.meta_content_scope.is_empty()
                            // cur has clear_scopes but no meta_scope/mcs: we still
                            // need to Pop the target.meta_scope pushed in initial,
                            // Restore cur.clear_scopes, and re-push target.meta_scope
                            // + target.meta_content_scope in the correct order.
                            || cur_context.clear_scopes.is_some()))
                        || context_refs.iter().any(|r| {
                            let ctx = r.resolve(syntax_set).unwrap();

                            !ctx.meta_content_scope.is_empty()
                                || (ctx.clear_scopes.is_some() && is_set)
                        });
                    if repush {
                        // Head pop: target.meta_scope (just pushed in the
                        // initial phase) + cur's own meta scopes. These come
                        // off as one Pop because they sit at the top of the
                        // visible stack and don't need per-frame Restore.
                        let target_ms_sum: usize = context_refs
                            .iter()
                            .map(|r| {
                                let ctx = r.resolve(syntax_set).unwrap();
                                ctx.meta_scope.len()
                            })
                            .sum();
                        let mut head_pop = target_ms_sum;
                        if is_set {
                            if version >= 2 {
                                // v2: the user-declared part of
                                // cur.meta_content_scope was popped in the
                                // initial phase (the auto-injected
                                // top-level scope, if any, stays). Only
                                // cur.meta_scope sits on top of the
                                // visible stack at this point.
                                head_pop += cur_context.meta_scope.len();
                            } else {
                                head_pop += cur_context.meta_content_scope.len()
                                    + cur_context.meta_scope.len();
                            }
                        }

                        // `pop: N + set:` with clear_scopes on the leaving
                        // context: restore the cleared atoms BEFORE the
                        // head Pop so its count finds the visible stack
                        // intact. Without this, Pop eats atoms from below
                        // the popped range — observed on Batch File
                        // `cmd-set-quoted-value-inner-end` (`clear_scopes: 1`)
                        // firing `pop: 2, set: ignored-tail-outer`, which
                        // otherwise drops `meta.command.set.dosbatch` from
                        // the trailing content of every `set "var"=...`
                        // line.
                        let restore_before_pop =
                            is_set && set_pop_count > 1 && cur_context.clear_scopes.is_some();
                        if restore_before_pop {
                            ops.push((index, ScopeStackOp::Restore));
                        }

                        // Pair to the initial-phase preview Clears emitted
                        // above for non-topmost `clear_scopes` targets with
                        // empty `meta_scope`. Each Restore here undoes one
                        // such preview Clear so the upcoming `head_pop`
                        // sees the full visible stack the captures pushed
                        // onto. The body's matching Clears are re-emitted
                        // below in the per-target loop, so the steady-state
                        // post-trigger view is unchanged.
                        if is_set && version >= 2 && context_refs.len() > 1 {
                            let mut atoms = 0usize;
                            let mut preview_clears = 0usize;
                            for r in context_refs.iter() {
                                let ctx = r.resolve(syntax_set)?;
                                if ctx.meta_scope.is_empty() {
                                    if let Some(ClearAmount::TopN(n)) = ctx.clear_scopes {
                                        let initial_clear = n.min(atoms);
                                        if initial_clear > 0 {
                                            preview_clears += 1;
                                            atoms -= initial_clear;
                                        }
                                    }
                                }
                                atoms += ctx.meta_scope.len();
                            }
                            for _ in 0..preview_clears {
                                ops.push((index, ScopeStackOp::Restore));
                            }
                        }

                        if head_pop > 0 {
                            ops.push((index, ScopeStackOp::Pop(head_pop)));
                        }

                        // Restore scopes cleared by the leaving context, now that
                        // cur.meta_scope and the initial phase's target.meta_scope
                        // push have been popped off. The restored atoms land below
                        // the target's upcoming meta_scope / meta_content_scope push.
                        if is_set && cur_context.clear_scopes.is_some() && !restore_before_pop {
                            ops.push((index, ScopeStackOp::Restore));
                        }

                        // `pop: N + set:` (set_pop_count > 1) unwinds N-1
                        // deeper contexts in addition to the usual
                        // set-replace semantics. Mirror the
                        // `MatchOperation::Pop` arm at lines 1954-1971: pop
                        // each deeper frame's mcs+ms in top-to-bottom order,
                        // then Restore that frame's `clear_scopes` if any.
                        // Without the per-depth Restore, atoms cleared by a
                        // deeper frame stay in clear_stack out of reach,
                        // and the per-target Clear below then bites one
                        // atom too deep. Observed on Python regex inside
                        // a `r'''(?ix:...)` triple-quoted string: the
                        // activate-x-mode `pop: 3 + set:[group-body-extended,
                        // maybe-unexpected-quantifiers]` left
                        // `group-body-extended_outer`'s cleared
                        // `meta.mode.extended.regexp` in clear_stack;
                        // group-body-extended_target's `clear_scopes: 1`
                        // then cleared `source.regexp.python` instead.
                        if is_set && set_pop_count > 1 {
                            let stack_len = self.stack.len();
                            for depth in 1..set_pop_count.min(stack_len) {
                                let level_idx = stack_len - 1 - depth;
                                let ctx = syntax_set.get_context(&self.stack[level_idx].context)?;
                                if !ctx.meta_content_scope.is_empty() {
                                    ops.push((
                                        index,
                                        ScopeStackOp::Pop(ctx.meta_content_scope.len()),
                                    ));
                                }
                                if !ctx.meta_scope.is_empty() {
                                    ops.push((index, ScopeStackOp::Pop(ctx.meta_scope.len())));
                                }
                                if ctx.clear_scopes.is_some() {
                                    ops.push((index, ScopeStackOp::Restore));
                                }
                            }
                        }

                        // Pair to the initial-phase Clear(N+1) emitted for the
                        // multi-context-set + cur-empty + target-clear case
                        // above. The body content needs only the target's own
                        // Clear(N) applied (emitted by the per-context loop
                        // below); restoring here brings the (N+1)-atom batch
                        // back onto the live stack, then the per-context
                        // Clear(N) eats N of them and leaves the extra atom
                        // visible to the body. The target context's matching
                        // Restore in `MatchOperation::Pop` then unwinds the
                        // smaller batch and the larger trigger-only batch is
                        // left consumed.
                        let cur_inert_restore = !cur_context.meta_scope.is_empty()
                            || !cur_context.meta_content_scope.is_empty()
                            || cur_context.clear_scopes.is_some();
                        let target_clear_amt_restore = if is_set {
                            context_refs.iter().find_map(|r| {
                                r.resolve(syntax_set)
                                    .ok()
                                    .and_then(|c| match c.clear_scopes {
                                        Some(ClearAmount::TopN(n))
                                            if n > 0 && !c.meta_scope.is_empty() =>
                                        {
                                            Some(n)
                                        }
                                        _ => None,
                                    })
                            })
                        } else {
                            None
                        };
                        let multi_set_extra_drop_restore = is_set
                            && version >= 2
                            && set_pop_count == 1
                            && context_refs.len() > 1
                            && !cur_inert_restore
                            && target_clear_amt_restore.is_some();
                        if multi_set_extra_drop_restore {
                            ops.push((index, ScopeStackOp::Restore));
                        }

                        // now we push meta scope and meta context scope for each context pushed
                        if version >= 2 {
                            // v2: For multi-context `set:`, Clear is emitted
                            // here so it strips the topmost just-pushed mcs
                            // atom (as Sublime does for multi-context set).
                            // Single-context `set:` ordinarily emits Clear in
                            // the initial phase (so the trigger token sees
                            // the cleared stack); re-emitting here would
                            // double-push onto clear_stack and cause Pop
                            // underflow when the context unwinds.
                            //
                            // Exception: when cur_context has its own
                            // `meta_scope` / `meta_content_scope` the initial
                            // phase deferred the Clear to here so the Pop
                            // above could find cur's ms on the visible stack.
                            // Re-introduce the Clear now, after Pop+Restore,
                            // so it strips parent atoms (the intended
                            // target) rather than cur's ms.
                            //
                            // Clear is emitted per-context (not only for the
                            // topmost) because `clear_scopes` on a non-topmost
                            // context is a real pattern: Bash's
                            // `set: [def-function-body, def-function-params,
                            // def-function-name]` has `clear_scopes: 1` on
                            // def-function-params (middle). Each Clear is
                            // placed just before that context's own mcs/ms
                            // pushes so it strips the previous iteration's
                            // last-pushed atom, matching Sublime's semantics.
                            let cur_has_meta = !cur_context.meta_scope.is_empty()
                                || !cur_context.meta_content_scope.is_empty();
                            let single_context_set_clear =
                                is_set && context_refs.len() == 1 && !cur_has_meta;
                            let mut prev_embed_scope_replaces = false;
                            for r in context_refs.iter() {
                                let ctx = r.resolve(syntax_set)?;

                                if is_set && !single_context_set_clear {
                                    if let Some(clear_amount) = ctx.clear_scopes {
                                        ops.push((index, ScopeStackOp::Clear(clear_amount)));
                                    }
                                }

                                for scope in ctx.meta_scope.iter() {
                                    ops.push((index, ScopeStackOp::Push(*scope)));
                                }
                                // v2: if the previous context has embed_scope_replaces,
                                // skip this context's meta_content_scope (the embedded
                                // syntax's top-level scope is replaced by embed_scope)
                                if !prev_embed_scope_replaces {
                                    for scope in ctx.meta_content_scope.iter() {
                                        ops.push((index, ScopeStackOp::Push(*scope)));
                                    }
                                }
                                prev_embed_scope_replaces = ctx.embed_scope_replaces;
                            }
                        } else {
                            for r in context_refs {
                                let ctx = r.resolve(syntax_set)?;

                                // for some reason, contrary to my reading of the docs, set does this after the token
                                if is_set {
                                    if let Some(clear_amount) = ctx.clear_scopes {
                                        ops.push((index, ScopeStackOp::Clear(clear_amount)));
                                    }
                                }

                                for scope in ctx.meta_scope.iter() {
                                    ops.push((index, ScopeStackOp::Push(*scope)));
                                }
                                for scope in ctx.meta_content_scope.iter() {
                                    ops.push((index, ScopeStackOp::Push(*scope)));
                                }
                            }
                        }
                    }
                }
            }
            MatchOperation::Embed {
                ref contexts,
                pop_count,
                ..
            } => {
                // When pop_count > 0 (pop + embed), use Set semantics to handle
                // popping the current context's meta scopes before pushing the
                // embedded contexts' meta scopes.
                let synthetic = if pop_count > 0 {
                    MatchOperation::Set {
                        ctx_refs: contexts.clone(),
                        pop_count,
                    }
                } else {
                    MatchOperation::Push(contexts.clone())
                };
                if pop_count > 0 {
                    // ST-observed divergence from plain `pop + set:`: on
                    // `pop + embed:` the trigger match's text sees **neither**
                    // `cur_context.meta_scope` nor `cur_context.meta_content_scope`.
                    // Both are suppressed on match text, then never restored
                    // — the embed replaces cur entirely. The probe lives at
                    // the top of `v2_pop_embed_suppresses_cur_meta_scope_on_match`.
                    //
                    // Emit those Pops ourselves in the initial phase, then pass
                    // a scope-stripped cur_context through to the recursive
                    // Set-semantic logic so its `num_to_pop` in the non-initial
                    // phase does not double-count these atoms (they are already
                    // off the stack). clear_scopes, with_prototype, and other
                    // fields are preserved on the stripped context — only the
                    // meta-scope vectors differ.
                    //
                    // Observed divergence on `<jsp:declaration>`'s `>`:
                    // syntect was producing
                    //   [..., meta.tag.jsp.declaration.begin.html,
                    //        meta.tag.jsp.declaration.begin.html,
                    //        punctuation.definition.tag.end.html]
                    // because the rule's explicit
                    //   scope: meta.tag.jsp.declaration.begin.html
                    //          punctuation.definition.tag.end.html
                    // was re-adding the atom that ST drops through the embed.
                    if initial {
                        if !cur_context.meta_content_scope.is_empty() {
                            ops.push((
                                index,
                                ScopeStackOp::Pop(cur_context.meta_content_scope.len()),
                            ));
                        }
                        if !cur_context.meta_scope.is_empty() {
                            ops.push((index, ScopeStackOp::Pop(cur_context.meta_scope.len())));
                        }
                    }
                    let stripped = Context {
                        meta_scope: Vec::new(),
                        meta_content_scope: Vec::new(),
                        ..cur_context.clone()
                    };
                    return self
                        .push_meta_ops(initial, index, &stripped, &synthetic, syntax_set, ops);
                }
                return self.push_meta_ops(
                    initial,
                    index,
                    cur_context,
                    &synthetic,
                    syntax_set,
                    ops,
                );
            }
            MatchOperation::None | MatchOperation::Fail(_) => (),
            MatchOperation::Branch {
                ref alternatives,
                pop_count,
                ..
            } => {
                // Branch acts like Push for meta ops purposes (or Set when pop_count > 0).
                // At exec time, Branch is transformed into a synthetic Push/Set before
                // calling push_meta_ops, so this arm is a safety fallback.
                let synthetic = if pop_count > 0 {
                    MatchOperation::Set {
                        ctx_refs: alternatives.clone(),
                        pop_count,
                    }
                } else {
                    MatchOperation::Push(alternatives.clone())
                };
                return self.push_meta_ops(
                    initial,
                    index,
                    cur_context,
                    &synthetic,
                    syntax_set,
                    ops,
                );
            }
        }

        Ok(())
    }

    /// Returns true if the stack was changed
    fn perform_op(
        &mut self,
        line: &str,
        regions: &Region,
        pat: &MatchPattern,
        syntax_set: &SyntaxSet,
    ) -> Result<bool, ParsingError> {
        let (ctx_refs, old_proto_ids, is_embed) = match pat.operation {
            MatchOperation::Push(ref ctx_refs) => (ctx_refs, None, false),
            MatchOperation::Embed {
                ref contexts,
                pop_count,
                ..
            } => {
                if pop_count > 0 {
                    for _ in 0..pop_count {
                        self.stack.pop();
                    }
                    let stack_len = self.stack.len();
                    self.branch_points
                        .retain(|bp| stack_len > bp.stack_depth.saturating_sub(bp.pop_count));
                    self.escape_stack.retain(|e| e.stack_depth < stack_len);
                }
                (contexts, None, true)
            }
            MatchOperation::Set {
                ref ctx_refs,
                pop_count,
            } => {
                // a `with_prototype` stays active when the context is `set`
                // until the context layer in the stack (where the `with_prototype`
                // was initially applied) is popped off. With `pop: N + set:`
                // (pop_count > 1), the topmost popped frame's prototypes are
                // what carry forward onto the new push.
                let pops = pop_count.max(1);
                let old_proto_ids = self.stack.pop().map(|s| s.prototypes);
                for _ in 1..pops {
                    self.stack.pop();
                }
                // Prune branch_points / escape_stack against the *final* stack
                // length (after the common push loop below).
                //
                // The retain predicate must mirror `handle_fail`'s validity
                // check (`stack.len() > bp.stack_depth - bp.pop_count`),
                // which subtracts the bp's own `pop_count`. Without that
                // subtraction, a `pop: N + branch_point` whose synthetic
                // Set has `pop_count: N` removes its own freshly-created
                // bp here — `bp.stack_depth` snapshots the *pre-pop*
                // depth, so `bp.stack_depth > final_len` even though the
                // alt-0 frame lives on at `final_len`. Symptom in Java:
                // the `branch_point: annotation-qualified-parameters`
                // declared on `annotation-qualified-identifier-name`'s
                // `pop: 2 + branch_point` was dropped at creation,
                // making its later `(?=\S)` `fail` a no-op and leaking
                // `meta.annotation.identifier.java meta.path.java` past
                // every nested-annotation extends path.
                let final_len = self.stack.len() + ctx_refs.len();
                self.branch_points
                    .retain(|bp| final_len > bp.stack_depth.saturating_sub(bp.pop_count));
                self.escape_stack.retain(|e| e.stack_depth < final_len);
                (ctx_refs, old_proto_ids, false)
            }
            MatchOperation::Pop(n) => {
                for _ in 0..n {
                    self.stack.pop();
                }
                // Invalidate branch points whose alt frame is no longer on
                // the stack. Use the same threshold as `handle_fail`'s
                // validity check — see the comment in the Set arm above.
                let stack_len = self.stack.len();
                self.branch_points
                    .retain(|bp| stack_len > bp.stack_depth.saturating_sub(bp.pop_count));
                // Remove escape entries whose stack_depth >= current stack
                self.escape_stack.retain(|e| e.stack_depth < stack_len);
                return Ok(true);
            }
            MatchOperation::None => return Ok(false),
            MatchOperation::Branch { .. } | MatchOperation::Fail(_) => {
                // Branch and Fail are handled in exec_pattern, not here
                return Ok(false);
            }
        };

        // Record stack depth before pushing (for Embed escape entry)
        let stack_depth_before = self.stack.len();

        for (i, r) in ctx_refs.iter().enumerate() {
            let mut proto_ids = if i == 0 {
                // it is only necessary to preserve the old prototypes
                // at the first stack frame pushed
                old_proto_ids.clone().unwrap_or_else(Vec::new)
            } else {
                Vec::new()
            };
            if i == ctx_refs.len() - 1 {
                // if a with_prototype was specified, and multiple contexts were pushed,
                // then the with_prototype applies only to the last context pushed, i.e.
                // top most on the stack after all the contexts are pushed - this is also
                // referred to as the "target" of the push by sublimehq - see
                // https://forum.sublimetext.com/t/dev-build-3111/19240/17 for more info
                if let Some(ref p) = pat.with_prototype {
                    proto_ids.push(p.id()?);
                }
            }
            let context_id = r.id()?;
            let context = syntax_set.get_context(&context_id)?;
            let captures = {
                let mut uses_backrefs = context.uses_backrefs;
                if !proto_ids.is_empty() {
                    uses_backrefs = uses_backrefs
                        || proto_ids
                            .iter()
                            .any(|id| syntax_set.get_context(id).unwrap().uses_backrefs);
                }
                if uses_backrefs {
                    Some((regions.clone(), line.to_owned()))
                } else {
                    None
                }
            };
            self.stack.push(StateLevel {
                context: context_id,
                prototypes: proto_ids,
                captures,
            });
        }

        // For Embed: push an EscapeEntry with the resolved escape regex
        if is_embed {
            if let MatchOperation::Embed { ref escape, .. } = pat.operation {
                let resolved_regex = if escape.has_captures {
                    // Resolve backrefs in escape regex using the triggering match's captures
                    let new_regex_str =
                        substitute_backrefs_in_regex(escape.escape_regex.regex_str(), |i| {
                            regions.pos(i).map(|(s, e)| escape_str(&line[s..e]))
                        });
                    Regex::new(new_regex_str)
                } else {
                    escape.escape_regex.clone()
                };
                self.escape_stack.push(EscapeEntry {
                    regex: resolved_regex,
                    captures: escape.escape_captures.clone(),
                    stack_depth: stack_depth_before,
                });
            }
        }

        Ok(true)
    }

    /// Execute an escape match: apply escape_captures, pop stack down to
    /// the embed's stack_depth, and remove the escape entry.
    fn exec_escape(
        &mut self,
        escape_idx: usize,
        match_start: usize,
        _match_end: usize,
        regions: &Region,
        syntax_set: &SyntaxSet,
        ops: &mut Vec<(usize, ScopeStackOp)>,
    ) -> Result<(), ParsingError> {
        let entry = &self.escape_stack[escape_idx];
        let target_depth = entry.stack_depth;
        let escape_captures = entry.captures.clone();

        // Drain orphan scope atoms left on the consumer's scope stack by
        // a prior cross-line replay whose later same-line fails
        // truncated the owning context out of `self.stack` — the Push
        // was committed to `flushed_ops` and can't be unwound by
        // `ops.truncate`, so we emit a balancing Pop here. Without this,
        // e.g. LaTeX `\end{lstlisting}` leaves
        // `meta.environment.verbatim.lstlisting.latex` on the stack
        // because a speculative `meta.path.java` atom pushed inside the
        // embedded Java shifts every subsequent Pop by one.
        //
        // `shadow` mirrors what the consumer will actually hold at this
        // point: end-of-prior-line shadow + ops-so-far on the current
        // line. `expected_depth` is what the consumer *should* have
        // based on `self.stack`'s meta_scope / meta_content_scope
        // contributions (with the v2 `embed_scope_replaces` mcs gating
        // applied below, matching the pop loop).
        let mut current_shadow = self.shadow.clone();
        for (_, op) in ops.iter() {
            let _ = current_shadow.apply(op);
        }
        let consumer_depth = current_shadow.as_slice().len();
        let expected_depth: usize = {
            let mut total = 0usize;
            let mut prev_embed_scope_replaces = false;
            for lvl in &self.stack {
                let ctx = syntax_set.get_context(&lvl.context)?;
                total += ctx.meta_scope.len();
                if !prev_embed_scope_replaces {
                    total += ctx.meta_content_scope.len();
                }
                prev_embed_scope_replaces = ctx.embed_scope_replaces;
            }
            total
        };
        if consumer_depth > expected_depth {
            ops.push((
                match_start,
                ScopeStackOp::Pop(consumer_depth - expected_depth),
            ));
        }

        // Pop all stack levels down to target_depth, emitting proper meta scope pops
        while self.stack.len() > target_depth {
            let level = &self.stack[self.stack.len() - 1];
            let ctx = syntax_set.get_context(&level.context)?;

            // Pop meta_content_scope.  If the context below has
            // embed_scope_replaces (it's a v2 embed_scope wrapper), the top
            // context is the embedded syntax's main — whose mcs was never
            // pushed on the way in — so skip the pop here too.  Gating this
            // on `current_syntax_version >= 2` would be wrong: the version
            // is read from the top context (the embedded syntax), but
            // embed_scope_replaces is set only by the v2 host syntax.  A v2
            // host embedding a v1 grammar (e.g. Rails HTML embedding Ruby)
            // would otherwise Pop a scope that was never pushed, misaligning
            // every scope below until the escape closes.
            if !ctx.meta_content_scope.is_empty() {
                let skip = self.stack.len() >= 2
                    && syntax_set
                        .get_context(&self.stack[self.stack.len() - 2].context)
                        .map(|c| c.embed_scope_replaces)
                        .unwrap_or(false);
                if !skip {
                    ops.push((match_start, ScopeStackOp::Pop(ctx.meta_content_scope.len())));
                }
            }

            // Pop meta_scope
            if !ctx.meta_scope.is_empty() {
                ops.push((match_start, ScopeStackOp::Pop(ctx.meta_scope.len())));
            }

            // Restore cleared scopes
            if ctx.clear_scopes.is_some() {
                ops.push((match_start, ScopeStackOp::Restore));
            }

            self.stack.pop();
        }

        // Apply escape_captures scopes
        if let Some(ref capture_map) = escape_captures {
            let mut map: Vec<((usize, i32), ScopeStackOp)> = Vec::new();
            for &(cap_index, ref scopes) in capture_map.iter() {
                if let Some((cap_start, cap_end)) = regions.pos(cap_index) {
                    if cap_start == cap_end {
                        continue;
                    }
                    for scope in scopes.iter() {
                        map.push((
                            (cap_start, -((cap_end - cap_start) as i32)),
                            ScopeStackOp::Push(*scope),
                        ));
                    }
                    map.push(((cap_end, i32::MIN), ScopeStackOp::Pop(scopes.len())));
                }
            }
            map.sort_by(|a, b| a.0.cmp(&b.0));
            for ((index, _), op) in map.into_iter() {
                ops.push((index, op));
            }
        }

        // Remove this escape entry and any inner (later) escape entries
        self.escape_stack.truncate(escape_idx);

        // Invalidate branch points whose stack depth is now above current stack
        self.branch_points
            .retain(|bp| bp.stack_depth <= self.stack.len());

        Ok(())
    }
}

/// Escape a string for use in regex substitution (re-export for use in escape resolution).
fn escape_str(s: &str) -> String {
    escape(s)
}

#[cfg(feature = "yaml-load")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsing::ScopeStackOp::{Pop, Push};
    use crate::parsing::{Scope, ScopeStack, SyntaxSet, SyntaxSetBuilder};
    use crate::util::debug_print_ops;
    use crate::utils::testdata;

    const TEST_SYNTAX: &str = include_str!("../../testdata/parser_tests.sublime-syntax");
    #[test]
    fn can_parse_simple() {
        let ss = &*testdata::PACKAGES_SYN_SET;
        let mut state = {
            let syntax = ss.find_syntax_by_name("Ruby (Rails)").unwrap();
            ParseState::new(syntax)
        };

        let ops1 = ops(&mut state, "module Bob::Wow::Troll::Five; 5; end", ss);
        // `source.ruby.rails` is pushed once — the file's top-level
        // scope. Earlier versions of `add_initial_contexts` inserted
        // the scope into `main.meta_content_scope` twice (once on
        // initial load, once again after `resolve_extends` re-ran),
        // which showed up here as a duplicate Push. That duplication
        // also broke assertions of the form `- source source` in the
        // Diff test fixtures; the initial-contexts fix removes it.
        let test_ops1 = vec![
            (0, Push(Scope::new("source.ruby.rails").unwrap())),
            (0, Push(Scope::new("meta.namespace.ruby").unwrap())),
            (
                0,
                Push(Scope::new("keyword.declaration.namespace.ruby").unwrap()),
            ),
            (6, Pop(1)),
            (7, Pop(1)),
            (7, Push(Scope::new("meta.namespace.ruby").unwrap())),
            (7, Push(Scope::new("entity.name.namespace.ruby").unwrap())),
            (7, Push(Scope::new("support.other.namespace.ruby").unwrap())),
        ];
        assert_eq!(&ops1[0..test_ops1.len()], &test_ops1[..]);

        let ops2 = ops(&mut state, "def lol(wow = 5)", ss);
        let test_ops2 = [
            (0, Push(Scope::new("meta.function.ruby").unwrap())),
            (
                0,
                Push(Scope::new("keyword.declaration.function.ruby").unwrap()),
            ),
            (3, Pop(2)),
            (3, Push(Scope::new("meta.function.ruby").unwrap())),
            (4, Push(Scope::new("entity.name.function.ruby").unwrap())),
            (7, Pop(1)),
        ];
        assert_eq!(&ops2[0..test_ops2.len()], &test_ops2[..]);
    }

    #[test]
    fn can_parse_yaml() {
        let ps = &*testdata::PACKAGES_SYN_SET;
        let mut state = {
            let syntax = ps.find_syntax_by_name("YAML").unwrap();
            ParseState::new(syntax)
        };

        assert_eq!(
            ops(&mut state, "key: value\n", ps),
            vec![
                (0, Push(Scope::new("source.yaml").unwrap())),
                (0, Push(Scope::new("meta.mapping.key.yaml").unwrap())),
                (0, Push(Scope::new("meta.string.yaml").unwrap())),
                (
                    0,
                    Push(Scope::new("string.unquoted.plain.out.yaml").unwrap())
                ),
                (3, Pop(2)),
                (3, Pop(1)),
                (3, Push(Scope::new("meta.mapping.yaml").unwrap())),
                (
                    3,
                    Push(Scope::new("punctuation.separator.key-value.mapping.yaml").unwrap())
                ),
                (4, Pop(2)),
                (5, Push(Scope::new("meta.string.yaml").unwrap())),
                (
                    5,
                    Push(Scope::new("string.unquoted.plain.out.yaml").unwrap())
                ),
                (10, Pop(2)),
            ]
        );
    }

    #[test]
    fn can_parse_includes() {
        let ss = &*testdata::PACKAGES_SYN_SET;
        let mut state = {
            let syntax = ss.find_syntax_by_name("HTML (Rails)").unwrap();
            ParseState::new(syntax)
        };

        let ops = ops(&mut state, "<script>var lol = '<% def wow(", ss);

        assert!(
            !ops.is_empty(),
            "expected non-empty ops for line with includes"
        );
        let mut stack = ScopeStack::new();
        for (_, op) in ops.iter() {
            stack.apply(op).expect("#[cfg(test)]");
        }
        let stack_str = format!("{:?}", stack.as_slice());
        assert!(
            stack_str.contains("text.html.rails"),
            "expected text.html.rails in scope stack, got: {:?}",
            stack.as_slice()
        );
    }

    #[test]
    fn can_parse_backrefs() {
        let ss = &*testdata::PACKAGES_SYN_SET;
        let mut state = {
            let syntax = ss.find_syntax_by_name("Ruby (Rails)").unwrap();
            ParseState::new(syntax)
        };

        // For parsing HEREDOC, the "SQL" is captured at the beginning and then used in another
        // regex with a backref, to match the end of the HEREDOC. Note that there can be code
        // after the marker (`.strip`) here.
        assert_eq!(
            ops(&mut state, "lol = <<-SQL.strip", ss),
            vec![
                (0, Push(Scope::new("source.ruby.rails").unwrap())),
                (
                    4,
                    Push(Scope::new("keyword.operator.assignment.ruby").unwrap())
                ),
                (5, Pop(1)),
                (6, Push(Scope::new("meta.string.heredoc.ruby").unwrap())),
                (
                    6,
                    Push(Scope::new("punctuation.definition.heredoc.ruby").unwrap())
                ),
                (9, Pop(1)),
                (9, Push(Scope::new("meta.tag.heredoc.ruby").unwrap())),
                (9, Push(Scope::new("entity.name.tag.ruby").unwrap())),
                (12, Pop(1)),
                (12, Pop(2)),
                (
                    12,
                    Push(Scope::new("punctuation.accessor.dot.ruby").unwrap())
                ),
                (13, Pop(1)),
            ]
        );

        assert_eq!(
            ops(&mut state, "wow", ss),
            vec![
                (0, Push(Scope::new("meta.string.heredoc.ruby").unwrap())),
                (0, Push(Scope::new("source.sql.embedded.ruby").unwrap()),),
                (0, Push(Scope::new("source.sql").unwrap())),
                (0, Push(Scope::new("source.sql.mysql").unwrap())),
                (0, Push(Scope::new("source.sql.basic").unwrap())),
                (0, Push(Scope::new("meta.column-name.sql").unwrap())),
                (3, Pop(1)),
            ]
        );

        assert_eq!(
            ops(&mut state, "SQL", ss),
            vec![
                (0, Pop(4)),
                (0, Pop(1)),
                (0, Push(Scope::new("meta.string.heredoc.ruby").unwrap())),
                (0, Push(Scope::new("meta.tag.heredoc.ruby").unwrap())),
                (0, Push(Scope::new("entity.name.tag.ruby").unwrap())),
                (3, Pop(2)),
                (3, Pop(1)),
            ]
        );
    }

    #[test]
    fn can_parse_preprocessor_rules() {
        let ss = &*testdata::PACKAGES_SYN_SET;
        let mut state = {
            let syntax = ss.find_syntax_by_name("C").unwrap();
            ParseState::new(syntax)
        };

        assert_eq!(
            ops(&mut state, "#ifdef FOO", ss),
            vec![
                (0, Push(Scope::new("source.c").unwrap())),
                (0, Push(Scope::new("meta.preprocessor.c").unwrap())),
                (0, Push(Scope::new("keyword.control.import.c").unwrap())),
                (6, Pop(1)),
                (10, Pop(1)),
            ]
        );
        assert_eq!(
            ops(&mut state, "{", ss),
            vec![
                (0, Push(Scope::new("meta.block.c").unwrap())),
                (
                    0,
                    Push(Scope::new("punctuation.section.block.begin.c").unwrap())
                ),
                (1, Pop(1)),
            ]
        );
        assert_eq!(
            ops(&mut state, "#else", ss),
            vec![
                (0, Push(Scope::new("meta.preprocessor.c").unwrap())),
                (0, Push(Scope::new("keyword.control.import.c").unwrap())),
                (5, Pop(1)),
                (5, Pop(1)),
            ]
        );
        assert_eq!(
            ops(&mut state, "{", ss),
            vec![
                (0, Push(Scope::new("meta.block.c").unwrap())),
                (
                    0,
                    Push(Scope::new("punctuation.section.block.begin.c").unwrap())
                ),
                (1, Pop(1)),
            ]
        );
        assert_eq!(
            ops(&mut state, "#endif", ss),
            vec![
                (0, Pop(1)),
                (0, Push(Scope::new("meta.block.c").unwrap())),
                (0, Push(Scope::new("meta.preprocessor.c").unwrap())),
                (0, Push(Scope::new("keyword.control.import.c").unwrap())),
                (6, Pop(2)),
                (6, Pop(2)),
                (6, Push(Scope::new("meta.block.c").unwrap())),
            ]
        );
        assert_eq!(
            ops(&mut state, "    foo;", ss),
            vec![
                (7, Push(Scope::new("punctuation.terminator.c").unwrap())),
                (8, Pop(1)),
            ]
        );
        assert_eq!(
            ops(&mut state, "}", ss),
            vec![
                (
                    0,
                    Push(Scope::new("punctuation.section.block.end.c").unwrap())
                ),
                (1, Pop(1)),
                (1, Pop(1)),
            ]
        );
    }

    #[test]
    fn can_parse_issue25() {
        let ss = &*testdata::PACKAGES_SYN_SET;
        let mut state = {
            let syntax = ss.find_syntax_by_name("C").unwrap();
            ParseState::new(syntax)
        };

        // test fix for issue #25
        assert_eq!(ops(&mut state, "struct{estruct", ss).len(), 10);
    }

    #[test]
    fn can_compare_parse_states() {
        // `ParseState` equality checks the stack, active branch points,
        // and the buffered `pending_lines` used for cross-line branch
        // replay. Because `class Foo {` opens a still-unresolved branch
        // (`declarations`), the literal source text is retained in
        // `pending_lines`, so two states that parsed the same syntactic
        // shape with different identifiers (e.g. `Foo` vs `Bar`) compare
        // unequal today — unlike earlier versions of this test. Keep the
        // two inputs identical here and assert the remaining invariants:
        // identical inputs -> equal states, advancing one -> divergence.
        let ss = &*testdata::PACKAGES_SYN_SET;
        let syntax = ss.find_syntax_by_name("Java").unwrap();
        let mut state1 = ParseState::new(syntax);
        let mut state2 = ParseState::new(syntax);

        assert_eq!(ops(&mut state1, "class Foo {", ss).len(), 13);
        assert_eq!(ops(&mut state2, "class Foo {", ss).len(), 13);

        assert_eq!(state1, state2);
        ops(&mut state1, "}", ss);
        assert_ne!(state1, state2);
    }

    #[test]
    fn can_parse_non_nested_clear_scopes() {
        let line = "'hello #simple_cleared_scopes_test world test \\n '";
        let expect = [
            "<source.test>, <example.meta-scope.after-clear-scopes.example>, <example.pushes-clear-scopes.example>",
            "<source.test>, <example.meta-scope.after-clear-scopes.example>, <example.pops-clear-scopes.example>",
            "<source.test>, <string.quoted.single.example>, <constant.character.escape.example>",
        ];
        expect_scope_stacks(line, &expect, TEST_SYNTAX);
    }

    #[test]
    fn can_parse_non_nested_too_many_clear_scopes() {
        let line = "'hello #too_many_cleared_scopes_test world test \\n '";
        let expect = [
            "<example.meta-scope.after-clear-scopes.example>, <example.pushes-clear-scopes.example>",
            "<example.meta-scope.after-clear-scopes.example>, <example.pops-clear-scopes.example>",
            "<source.test>, <string.quoted.single.example>, <constant.character.escape.example>",
        ];
        expect_scope_stacks(line, &expect, TEST_SYNTAX);
    }

    #[test]
    fn can_parse_nested_clear_scopes() {
        let line = "'hello #nested_clear_scopes_test world foo bar test \\n '";
        let expect = [
            "<source.test>, <example.meta-scope.after-clear-scopes.example>, <example.pushes-clear-scopes.example>",
            "<source.test>, <example.meta-scope.cleared-previous-meta-scope.example>, <foo>",
            "<source.test>, <example.meta-scope.after-clear-scopes.example>, <example.pops-clear-scopes.example>",
            "<source.test>, <string.quoted.single.example>, <constant.character.escape.example>",
        ];
        expect_scope_stacks(line, &expect, TEST_SYNTAX);
    }

    #[test]
    fn can_parse_infinite_loop() {
        let line = "#infinite_loop_test 123";
        let expect = ["<source.test>, <constant.numeric.test>"];
        expect_scope_stacks(line, &expect, TEST_SYNTAX);
    }

    #[test]
    fn can_parse_infinite_seeming_loop() {
        // See https://github.com/SublimeTextIssues/Core/issues/1190 for an
        // explanation.
        let line = "#infinite_seeming_loop_test hello";
        let expect = [
            "<source.test>, <keyword.test>",
            "<source.test>, <test>, <string.unquoted.test>",
            "<source.test>, <test>, <keyword.control.test>",
        ];
        expect_scope_stacks(line, &expect, TEST_SYNTAX);
    }

    #[test]
    fn can_parse_prototype_that_pops_main() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  prototype:
    # This causes us to pop out of the main context. Sublime Text handles that
    # by pushing main back automatically.
    - match: (?=!)
      pop: true
  main:
    - match: foo
      scope: test.good
"#;

        let line = "foo!";
        let expect = ["<source.test>, <test.good>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    #[test]
    fn can_parse_prototype_that_pops_multiple_context() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  prototype:
    - match: "!"
      pop: 2
  bar:
    - match: \bbaz\b
      push: baz
      scope: main.baz
  foo:
    - match: \bbar\b
      push: bar
      scope: test.bar
    - match: \bgood\b
      push: baz
      scope: test.good
  baz: []
    
  main:
    - match: \bfoo\b
      push: foo
      scope: test.foo
"#;

        let line = "foo bar baz ! good";
        let expect = ["<source.test>, <test.good>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    #[test]
    fn can_parse_syntax_with_newline_in_character_class() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: foo[\n]
      scope: foo.end
    - match: foo
      scope: foo.any
"#;

        let line = "foo";
        let expect = ["<source.test>, <foo.end>"];
        expect_scope_stacks(line, &expect, syntax);

        let line = "foofoofoo";
        let expect = [
            "<source.test>, <foo.any>",
            "<source.test>, <foo.any>",
            "<source.test>, <foo.end>",
        ];
        expect_scope_stacks(line, &expect, syntax);
    }

    #[test]
    fn can_parse_issue120() {
        let syntax = SyntaxDefinition::load_from_str(
            include_str!("../../testdata/embed_escape_test.sublime-syntax"),
            false,
            None,
        )
        .unwrap();

        let line1 = "\"abctest\" foobar";
        let expect1 = [
            "<meta.attribute-with-value.style.html>, <string.quoted.double>, <punctuation.definition.string.begin.html>",
            "<meta.attribute-with-value.style.html>, <source.css>",
            "<meta.attribute-with-value.style.html>, <string.quoted.double>, <punctuation.definition.string.end.html>",
            "<meta.attribute-with-value.style.html>, <source.css>, <test.embedded>",
            "<top-level.test>",
        ];

        expect_scope_stacks_with_syntax(line1, &expect1, syntax.clone());

        let line2 = ">abctest</style>foobar";
        let expect2 = [
            "<meta.tag.style.begin.html>, <punctuation.definition.tag.end.html>",
            "<source.css.embedded.html>, <test.embedded>",
            "<top-level.test>",
        ];
        expect_scope_stacks_with_syntax(line2, &expect2, syntax);
    }

    #[test]
    fn can_parse_non_consuming_pop_that_would_loop() {
        // See https://github.com/trishume/syntect/issues/127
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    # This makes us go into "test" without consuming any characters
    - match: (?=hello)
      push: test
  test:
    # If we used this match, we'd go back to "main" without consuming anything,
    # and then back into "test", infinitely looping. ST detects this at this
    # point and ignores this match until at least one character matched.
    - match: (?!world)
      pop: true
    - match: \w+
      scope: test.matched
"#;

        let line = "hello";
        let expect = ["<source.test>, <test.matched>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    #[test]
    fn can_parse_non_consuming_set_and_pop_that_would_loop() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    # This makes us go into "a" without advancing
    - match: (?=test)
      push: a
  a:
    # This makes us go into "b" without advancing
    - match: (?=t)
      set: b
  b:
    # If we used this match, we'd go back to "main" without having advanced,
    # which means we'd have an infinite loop like with the previous test.
    # So even for a "set", we have to check if we're advancing or not.
    - match: (?=t)
      pop: true
    - match: \w+
      scope: test.matched
"#;

        let line = "test";
        let expect = ["<source.test>, <test.matched>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    #[test]
    fn can_parse_non_consuming_set_after_consuming_push_that_does_not_loop() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    # This makes us go into "a", but we consumed a character
    - match: t
      push: a
    - match: \w+
      scope: test.matched
  a:
    # This makes us go into "b" without consuming
    - match: (?=e)
      set: b
  b:
    # This match does not result in an infinite loop because we already consumed
    # a character to get into "a", so it's ok to pop back into "main".
    - match: (?=e)
      pop: true
"#;

        let line = "test";
        let expect = ["<source.test>, <test.matched>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    #[test]
    fn can_parse_non_consuming_set_after_consuming_set_that_does_not_loop() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: (?=hello)
      push: a
    - match: \w+
      scope: test.matched
  a:
    - match: h
      set: b
  b:
    - match: (?=e)
      set: c
  c:
    # This is not an infinite loop because "a" consumed a character, so we can
    # actually pop back into main and then match the rest of the input.
    - match: (?=e)
      pop: true
"#;

        let line = "hello";
        let expect = ["<source.test>, <test.matched>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    #[test]
    fn can_parse_non_consuming_pop_that_would_loop_at_end_of_line() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    # This makes us go into "test" without consuming, even at the end of line
    - match: ""
      push: test
  test:
    - match: ""
      pop: true
    - match: \w+
      scope: test.matched
"#;

        let line = "hello";
        let expect = ["<source.test>, <test.matched>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    #[test]
    fn non_consuming_pop_n_below_pre_push_depth_is_not_a_loop() {
        // Mirror of the Haskell `declaration-type-end` branch where the
        // fallback alternative is `immediately-pop2` (empty match with
        // `pop: 2`). The outer wrapper's meta_scope must come off when
        // the pop-2 fallback fires — pre-fix, the loop guard flagged
        // any non-consuming `pop` after a non-consuming push as
        // looping, so the parser advanced one char past the branch and
        // the pop-2 fired at the wrong column, leaving the wrapper's
        // scope covering the trailing `y` token.
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: open
      scope: test.open
      push: wrapper
    - match: y
      scope: test.main.y
    - match: z
      scope: test.main.z
  wrapper:
    - meta_scope: test.wrapper
    - match: ""
      branch_point: fallback
      branch:
        - try
        - give-up
  try:
    - match: x
      scope: test.try.match
    - match: (?=y)
      fail: fallback
  give-up:
    - match: ""
      pop: 2
"#;
        // With the fix, `give-up` fires pop-2 at column 4 (the fail
        // position), unwinding both `give-up` and `wrapper`; `y` is
        // then scoped by `main`'s rule. Without the fix, would_loop
        // advanced start past column 4, the pop-2 fired at column 5,
        // and `y` stayed inside the wrapper and never matched
        // `test.main.y`.
        expect_scope_stacks("openyz", &["<source.test>, <test.main.y>"], syntax);
    }

    #[test]
    fn can_parse_empty_but_consuming_set_that_does_not_loop() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: (?=hello)
      push: a
    - match: ello
      scope: test.good
  a:
    # This is an empty match, but it consumed a character (the "h")
    - match: (?=e)
      set: b
  b:
    # .. so it's ok to pop back to main from here
    - match: ""
      pop: true
    - match: ello
      scope: test.bad
"#;

        let line = "hello";
        let expect = ["<source.test>, <test.good>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    #[test]
    fn can_parse_non_consuming_pop_that_does_not_loop() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    # This is a non-consuming push, so "b" will need to check for a
    # non-consuming pop
    - match: (?=hello)
      push: [b, a]
    - match: ello
      scope: test.good
  a:
    # This pop is ok, it consumed "h"
    - match: (?=e)
      pop: true
  b:
    # This is non-consuming, and we set to "c"
    - match: (?=e)
      set: c
  c:
    # It's ok to pop back to "main" here because we consumed a character in the
    # meantime.
    - match: ""
      pop: true
    - match: ello
      scope: test.bad
"#;

        let line = "hello";
        let expect = ["<source.test>, <test.good>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    #[test]
    fn can_parse_non_consuming_pop_with_multi_push_that_does_not_loop() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: (?=hello)
      push: [b, a]
    - match: ello
      scope: test.good
  a:
    # This pop is ok, as we're not popping back to "main" yet (which would loop),
    # we're popping to "b"
    - match: ""
      pop: true
    - match: \w+
      scope: test.bad
  b:
    - match: \w+
      scope: test.good
"#;

        let line = "hello";
        let expect = ["<source.test>, <test.good>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    #[test]
    fn can_parse_non_consuming_pop_of_recursive_context_that_does_not_loop() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: xxx
      scope: test.good
    - include: basic-identifiers

  basic-identifiers:
    - match: '\w+::'
      scope: test.matched
      push: no-type-names

  no-type-names:
      - include: basic-identifiers
      - match: \w+
        scope: test.matched.inside
      # This is a tricky one because when this is the best match,
      # we have two instances of "no-type-names" on the stack, so we're popping
      # back from "no-type-names" to another "no-type-names".
      - match: ''
        pop: true
"#;

        let line = "foo::bar::* xxx";
        let expect = ["<source.test>, <test.good>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    /// Ruby's `?\u{012ACF 0gxs}`: `\h{0,6}` can match zero-width at the
    /// space. Without the `FIND_NOT_EMPTY` engine option, the zero-width
    /// match wins and hides the later non-empty match of `0`, which then
    /// falls through to the `\S` fallback. With the option on
    /// `MatchOperation::None` patterns, the engine retries past the
    /// zero-width position and matches `0` as `number.hex`.
    #[test]
    fn scope_only_pattern_that_matches_zero_width_finds_later_non_empty() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: \h{0,6}
      scope: number.hex
    - match: \S
      scope: invalid.illegal
"#;

        let line = "012ACF 0gxs";
        let expect = [
            "<source.test>, <number.hex>",      // "012ACF" and "0"
            "<source.test>, <invalid.illegal>", // "g", "x", "s"
        ];
        expect_scope_stacks(line, &expect, syntax);
    }

    /// Cabal's `\|\||&&||!` operator regex has a stray empty alternative
    /// between `&&` and `!`. Under leftmost-first matching the empty alt
    /// wins zero-width at the `!` position. With `FIND_NOT_EMPTY`, the
    /// engine rejects the empty alt and matches `!` via the real
    /// alternative.
    #[test]
    fn scope_only_pattern_with_middle_empty_alt_matches_bang() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: \|\||&&||!
      scope: keyword.operator
"#;
        let line = "!";
        let expect = ["<source.test>, <keyword.operator>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    /// Rust's `prelude_types: (?x:|Box|Option|…)` puts a `|` before every
    /// alternative, including the first. Under leftmost-first the leading
    /// empty alt wins zero-width at every position, so `\b(?x:|Box|Vec)\b`
    /// never matches `Box` or `Vec`. With `FIND_NOT_EMPTY`, the engine
    /// rejects the zero-width alt and matches `Vec` via the real
    /// alternative.
    #[test]
    fn scope_only_pattern_with_leading_empty_alt_in_group_matches_name() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: \b(?x:|Box|Vec)\b
      scope: support.type
"#;
        let line = "Vec";
        let expect = ["<source.test>, <support.type>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    #[test]
    fn can_parse_non_consuming_pop_order() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: (?=hello)
      push: test
  test:
    # This matches first
    - match: (?=e)
      push: good
    # But this (looping) match replaces it, because it's an earlier match
    - match: (?=h)
      pop: true
    # And this should not replace it, as it's a later match (only matches at
    # the same position can replace looping pops).
    - match: (?=o)
      push: bad
  good:
    - match: \w+
      scope: test.good
  bad:
    - match: \w+
      scope: test.bad
"#;

        let line = "hello";
        let expect = ["<source.test>, <test.good>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    #[test]
    fn can_parse_prototype_with_embed() {
        let syntax = r#"
name: Javadoc
scope: text.html.javadoc
contexts:
  prototype:
    - match: \*
      scope: punctuation.definition.comment.javadoc

  main:
    - meta_include_prototype: false
    - match: /\*\*
      scope: comment.block.documentation.javadoc punctuation.definition.comment.begin.javadoc
      embed: contents
      embed_scope: comment.block.documentation.javadoc text.html.javadoc
      escape: \*/
      escape_captures:
        0: comment.block.documentation.javadoc punctuation.definition.comment.end.javadoc

  contents:
    - match: ''
"#;

        let syntax = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
        expect_scope_stacks_with_syntax("/** * */", &["<comment.block.documentation.javadoc>, <punctuation.definition.comment.begin.javadoc>", "<comment.block.documentation.javadoc>, <text.html.javadoc>, <punctuation.definition.comment.javadoc>", "<comment.block.documentation.javadoc>, <punctuation.definition.comment.end.javadoc>"], syntax);
    }

    #[test]
    fn can_parse_context_included_in_prototype_via_named_reference() {
        let syntax = r#"
scope: source.test
contexts:
  prototype:
    - match: a
      push: a
    - match: b
      scope: test.bad
  main:
    - match: unused
  # This context is included in the prototype (see `push: a`).
  # Because of that, ST doesn't apply the prototype to this context, so if
  # we're in here the "b" shouldn't match.
  a:
    - match: a
      scope: test.good
"#;

        let stack_states = stack_states(parse("aa b", syntax));
        assert_eq!(
            stack_states,
            vec![
                "<source.test>",
                "<source.test>, <test.good>",
                "<source.test>",
            ],
            "Expected test.bad to not match"
        );
    }

    #[test]
    fn can_parse_with_prototype_set() {
        let syntax = r#"%YAML 1.2
---
scope: source.test-set-with-proto
contexts:
  main:
    - match: a
      scope: a
      set: next1
      with_prototype:
        - match: '1'
          scope: '1'
        - match: '2'
          scope: '2'
        - match: '3'
          scope: '3'
        - match: '4'
          scope: '4'
    - match: '5'
      scope: '5'
      set: [next3, next2]
      with_prototype:
        - match: c
          scope: cwith
  next1:
    - match: b
      scope: b
      set: next2
  next2:
    - match: c
      scope: c
      push: next3
    - match: e
      scope: e
      pop: true
    - match: f
      scope: f
      set: [next1, next2]
  next3:
    - match: d
      scope: d
    - match: (?=e)
      pop: true
    - match: c
      scope: cwithout
"#;

        expect_scope_stacks_with_syntax(
            "a1b2c3d4e5",
            &[
                "<a>", "<1>", "<b>", "<2>", "<c>", "<3>", "<d>", "<4>", "<e>", "<5>",
            ],
            SyntaxDefinition::load_from_str(syntax, true, None).unwrap(),
        );
        expect_scope_stacks_with_syntax(
            "5cfcecbedcdea",
            &[
                "<5>",
                "<cwith>",
                "<f>",
                "<e>",
                "<b>",
                "<d>",
                "<cwithout>",
                "<a>",
            ],
            SyntaxDefinition::load_from_str(syntax, true, None).unwrap(),
        );
    }

    #[test]
    fn can_parse_issue176() {
        let syntax = r#"
scope: source.dummy
contexts:
  main:
    - match: (test)(?=(foo))(f)
      captures:
        1: test
        2: ignored
        3: f
      push:
        - match: (oo)
          captures:
            1: keyword
"#;

        let syntax = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
        expect_scope_stacks_with_syntax(
            "testfoo",
            &["<test>", /*"<ignored>",*/ "<f>", "<keyword>"],
            syntax,
        );
    }

    #[test]
    fn can_parse_two_with_prototypes_at_same_stack_level() {
        let syntax_yamlstr = r#"
%YAML 1.2
---
# See http://www.sublimetext.com/docs/3/syntax.html
scope: source.example-wp
contexts:
  main:
    - match: a
      scope: a
      push:
        - match: b
          scope: b
          set:
            - match: c
              scope: c
          with_prototype:
            - match: '2'
              scope: '2'
      with_prototype:
        - match: '1'
          scope: '1'
"#;

        let syntax = SyntaxDefinition::load_from_str(syntax_yamlstr, true, None).unwrap();
        expect_scope_stacks_with_syntax("abc12", &["<1>", "<2>"], syntax);
    }

    #[test]
    fn can_parse_two_with_prototypes_at_same_stack_level_set_multiple() {
        let syntax_yamlstr = r#"
%YAML 1.2
---
# See http://www.sublimetext.com/docs/3/syntax.html
scope: source.example-wp
contexts:
  main:
    - match: a
      scope: a
      push:
        - match: b
          scope: b
          set: [context1, context2, context3]
          with_prototype:
            - match: '2'
              scope: '2'
      with_prototype:
        - match: '1'
          scope: '1'
    - match: '1'
      scope: digit1
    - match: '2'
      scope: digit2
  context1:
    - match: e
      scope: e
      pop: true
    - match: '2'
      scope: digit2
  context2:
    - match: d
      scope: d
      pop: true
    - match: '2'
      scope: digit2
  context3:
    - match: c
      scope: c
      pop: true
"#;

        let syntax = SyntaxDefinition::load_from_str(syntax_yamlstr, true, None).unwrap();
        expect_scope_stacks_with_syntax("ab12", &["<1>", "<2>"], syntax.clone());
        expect_scope_stacks_with_syntax("abc12", &["<1>", "<digit2>"], syntax.clone());
        expect_scope_stacks_with_syntax("abcd12", &["<1>", "<digit2>"], syntax.clone());
        expect_scope_stacks_with_syntax("abcde12", &["<digit1>", "<digit2>"], syntax);
    }

    #[test]
    fn can_parse_two_with_prototypes_at_same_stack_level_updated_captures() {
        let syntax_yamlstr = r#"
%YAML 1.2
---
# See http://www.sublimetext.com/docs/3/syntax.html
scope: source.example-wp
contexts:
  main:
    - match: (a)
      scope: a
      push:
        - match: (b)
          scope: b
          set:
            - match: c
              scope: c
          with_prototype:
            - match: d
              scope: d
      with_prototype:
        - match: \1
          scope: '1'
          pop: true
"#;

        let syntax = SyntaxDefinition::load_from_str(syntax_yamlstr, true, None).unwrap();
        expect_scope_stacks_with_syntax("aa", &["<a>", "<1>"], syntax.clone());
        expect_scope_stacks_with_syntax("abcdb", &["<a>", "<b>", "<c>", "<d>", "<1>"], syntax);
    }

    #[test]
    fn can_parse_two_with_prototypes_at_same_stack_level_updated_captures_ignore_unexisting() {
        let syntax_yamlstr = r#"
%YAML 1.2
---
# See http://www.sublimetext.com/docs/3/syntax.html
scope: source.example-wp
contexts:
  main:
    - match: (a)(-)
      scope: a
      push:
        - match: (b)
          scope: b
          set:
            - match: c
              scope: c
          with_prototype:
            - match: d
              scope: d
      with_prototype:
        - match: \2
          scope: '2'
          pop: true
        - match: \1
          scope: '1'
          pop: true
"#;

        let syntax = SyntaxDefinition::load_from_str(syntax_yamlstr, true, None).unwrap();
        expect_scope_stacks_with_syntax("a--", &["<a>", "<2>"], syntax.clone());
        // it seems that when ST encounters a non existing pop backreference, it just pops back to the with_prototype's original parent context - i.e. cdb is unscoped
        // TODO: it would be useful to have syntest functionality available here for easier testing and clarity
        expect_scope_stacks_with_syntax("a-bcdba-", &["<a>", "<b>"], syntax);
    }

    #[test]
    fn can_parse_syntax_with_eol_and_newline() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: foo$\n
      scope: foo.newline
"#;

        let line = "foo";
        let expect = ["<source.test>, <foo.newline>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    #[test]
    fn can_parse_syntax_with_eol_only() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: foo$
      scope: foo.newline
"#;

        let line = "foo";
        let expect = ["<source.test>, <foo.newline>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    #[test]
    fn can_parse_syntax_with_beginning_of_line() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: \w+
      scope: word
      push:
        # this should not match at the end of the line
        - match: ^\s*$
          pop: true
        - match: =+
          scope: heading
          pop: true
    - match: .*
      scope: other
"#;

        let syntax_newlines = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
        let syntax_set = link(syntax_newlines);

        let mut state = ParseState::new(&syntax_set.syntaxes()[0]);
        assert_eq!(
            ops(&mut state, "foo\n", &syntax_set),
            vec![
                (0, Push(Scope::new("source.test").unwrap())),
                (0, Push(Scope::new("word").unwrap())),
                (3, Pop(1))
            ]
        );
        assert_eq!(
            ops(&mut state, "===\n", &syntax_set),
            vec![(0, Push(Scope::new("heading").unwrap())), (3, Pop(1))]
        );

        assert_eq!(
            ops(&mut state, "bar\n", &syntax_set),
            vec![(0, Push(Scope::new("word").unwrap())), (3, Pop(1))]
        );
        // This should result in popping out of the context
        assert_eq!(ops(&mut state, "\n", &syntax_set), vec![]);
        // So now this matches other
        assert_eq!(
            ops(&mut state, "====\n", &syntax_set),
            vec![(0, Push(Scope::new("other").unwrap())), (4, Pop(1))]
        );
    }

    #[test]
    fn can_parse_syntax_with_comment_and_eol() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: (//).*$
      scope: comment.line.double-slash
"#;

        let syntax_newlines = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
        let syntax_set = link(syntax_newlines);

        let mut state = ParseState::new(&syntax_set.syntaxes()[0]);
        assert_eq!(
            ops(&mut state, "// foo\n", &syntax_set),
            vec![
                (0, Push(Scope::new("source.test").unwrap())),
                (0, Push(Scope::new("comment.line.double-slash").unwrap())),
                // 6 is important here, should not be 7. The pattern should *not* consume the newline,
                // but instead match before it. This is important for whitespace-sensitive syntaxes
                // where newlines terminate statements such as Scala.
                (6, Pop(1))
            ]
        );
    }

    #[test]
    fn can_parse_text_with_unicode_to_skip() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: (?=.)
      push: test
  test:
    - match: (?=.)
      pop: true
    - match: x
      scope: test.good
"#;

        // U+03C0 GREEK SMALL LETTER PI, 2 bytes in UTF-8
        expect_scope_stacks("\u{03C0}x", &["<source.test>, <test.good>"], syntax);
        // U+0800 SAMARITAN LETTER ALAF, 3 bytes in UTF-8
        expect_scope_stacks("\u{0800}x", &["<source.test>, <test.good>"], syntax);
        // U+1F600 GRINNING FACE, 4 bytes in UTF-8
        expect_scope_stacks("\u{1F600}x", &["<source.test>, <test.good>"], syntax);
    }

    #[test]
    fn can_include_backrefs() {
        let syntax = SyntaxDefinition::load_from_str(
            r#"
                name: Backref Include Test
                scope: source.backrefinc
                contexts:
                  main:
                    - match: (a)
                      scope: a
                      push: context1
                  context1:
                    - include: context2
                  context2:
                    - match: \1
                      scope: b
                      pop: true
                "#,
            true,
            None,
        )
        .unwrap();

        expect_scope_stacks_with_syntax("aa", &["<a>", "<b>"], syntax);
    }

    #[test]
    fn can_include_nested_backrefs() {
        let syntax = SyntaxDefinition::load_from_str(
            r#"
                name: Backref Include Test
                scope: source.backrefinc
                contexts:
                  main:
                    - match: (a)
                      scope: a
                      push: context1
                  context1:
                    - include: context3
                  context3:
                    - include: context2
                  context2:
                    - match: \1
                      scope: b
                      pop: true
                "#,
            true,
            None,
        )
        .unwrap();

        expect_scope_stacks_with_syntax("aa", &["<a>", "<b>"], syntax);
    }

    #[test]
    fn can_avoid_infinite_stack_depth() {
        let syntax = SyntaxDefinition::load_from_str(
            r#"
                name: Stack Depth Test
                scope: source.stack_depth
                contexts:
                  main:
                    - match: (a)
                      scope: a
                      push: context1

                    
                  context1:
                    - match: b
                      scope: b
                    - match: ''
                      push: context1
                    - match: ''
                      pop: 1
                    - match: c
                      scope: c
                "#,
            true,
            None,
        )
        .unwrap();

        let syntax_set = link(syntax);
        let mut state = ParseState::new(&syntax_set.syntaxes()[0]);
        expect_scope_stacks_for_ops(ops(&mut state, "a bc\n", &syntax_set), &["<a>"]);
        expect_scope_stacks_for_ops(ops(&mut state, "bc\n", &syntax_set), &["<b>"]);
    }

    /// Regression guard for the "extends double-inserts top_level_scope"
    /// bug: `add_initial_contexts` runs once during initial YAML load and
    /// again from `resolve_extends` after a child inherits its parent's
    /// contexts. On the second run `main.meta_content_scope` already
    /// begins with the child's top-level scope from the first run; if the
    /// code naively re-inserts at position 0 and re-copies to `__main`,
    /// the file scope ends up pushed twice at the start of every parse
    /// (observed as `[source.diff.git, source.diff.git]` on Git Diff and
    /// all the Rails (Rails) syntaxes, which broke assertions of the
    /// form `- source source`). The copy to `__main` must strip an
    /// already-present top_level_scope prefix, and the insert into
    /// Regression guard for the "`meta_append` / `meta_prepend` resets
    /// `meta_include_prototype` to its default" bug: the SQL base
    /// declares `inside-like-single-quoted-string` with
    /// `meta_include_prototype: false` so its `--` comment rule (from
    /// the SQL prototype) does NOT fire inside LIKE strings. TSQL extends
    /// that context with `meta_append: true` to add a `[…]` character-set
    /// rule, but doesn't restate `meta_include_prototype: false`. Before
    /// the fix, the merge in `syntax_set.rs` left the child's default
    /// `meta_include_prototype: true`, so the SQL prototype attached to
    /// the merged context, and `--` inside LIKE strings was scoped as
    /// a comment — 4,918 cascading assertion failures in
    /// `syntax_test_tsql.sql`.
    ///
    /// Synthetic shape: a parent with a `prototype` matching `--` as a
    /// comment, a base context with `meta_include_prototype: false`,
    /// and a child that extends the parent and `meta_append`s a single
    /// rule to that base context without restating
    /// `meta_include_prototype`. After merge, `--` inside the base
    /// context's matched span must NOT take a `comment.*` scope.
    #[test]
    fn meta_append_inherits_meta_include_prototype_from_parent() {
        use crate::parsing::syntax_set::SyntaxSetBuilder;

        let dir =
            std::env::temp_dir().join(format!("syntect-meta-append-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("parent.sublime-syntax"),
            r#"
name: Parent
scope: source.parent
file_extensions: [parent]
contexts:
  prototype:
    - match: '--'
      scope: punctuation.definition.comment
      push: comment-body
  comment-body:
    - meta_scope: comment.line
    - match: $
      pop: 1
  main:
    - match: \bopen\b
      push: inside
  inside:
    - meta_include_prototype: false
    - meta_scope: meta.inside
    - match: \bclose\b
      pop: 1
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("child.sublime-syntax"),
            r#"
name: Child
scope: source.child
file_extensions: [child]
extends: parent.sublime-syntax
contexts:
  inside:
    - meta_append: true
    - match: '!'
      scope: punctuation.bang.child
"#,
        )
        .unwrap();
        let mut builder = SyntaxSetBuilder::new();
        builder.add_from_folder(&dir, true).unwrap();
        let ss = builder.build();
        let syntax = ss
            .find_syntax_by_scope(Scope::new("source.child").unwrap())
            .unwrap();
        let mut state = ParseState::new(syntax);
        let o = ops(&mut state, "open -- close\n", &ss);
        let _ = std::fs::remove_dir_all(&dir);
        let comment_pushes = o
            .iter()
            .filter(|(_, op)| {
                matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("comment"))
            })
            .count();
        assert_eq!(
            comment_pushes, 0,
            "`--` inside the inside context (meta_include_prototype: false in parent) \
             must not match the parent's prototype comment rule after meta_append merge; \
             ops were: {:?}",
            o
        );
    }

    /// `main` must be idempotent.
    #[test]
    fn extending_syntax_does_not_double_push_top_level_scope() {
        use crate::parsing::SyntaxSet;
        let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        // Git Diff extends Diff (Basic) — a concrete case of the bug.
        let syntax = ss.find_syntax_by_name("Git Diff").unwrap();
        let mut state = ParseState::new(syntax);
        let o = ops(
            &mut state,
            "From 1234567890 Mon Sep 17 00:00:00 2001\n",
            &ss,
        );
        let source_pushes = o
            .iter()
            .filter(|(_, op)| matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s) == "<source.diff.git>"))
            .count();
        assert_eq!(
            source_pushes, 1,
            "source.diff.git should be pushed exactly once for the file's top-level scope; ops were: {:?}",
            o
        );
    }

    /// Regression guard for the "branch_point match loses its own scope
    /// on fail-retry" bug: when the keyword that triggers a
    /// branch_point (e.g. `LIKE` in SQL with
    /// `scope: keyword.operator.comparison.sql`) has its first
    /// alternative fail, the Push/Pop for that scope was truncated off
    /// `ops` along with alt[0]'s subsequent work and never re-emitted.
    /// The eventual successful alternative then produced a parse where
    /// the keyword carried no scope — 4,942 cascading assertion
    /// failures in TSQL.
    ///
    /// The test triggers the same shape synthetically: a `trigger` match
    /// with its own scope branches into two alternatives; the first
    /// fails, the second succeeds; the `trigger` token must still carry
    /// the declared scope after the retry.
    #[test]
    fn branch_point_match_scope_survives_fail_retry() {
        // Expected: `trigger` gets `keyword.operator.test`; the
        // following word gets `ok.test` (via alt-succeeds).
        expect_scope_stacks(
            "trigger yes",
            &["<keyword.operator.test>", "<ok.test>"],
            r#"
                name: Branch Pat Scope Test
                scope: source.test
                contexts:
                  main:
                    - match: \btrigger\b
                      scope: keyword.operator.test
                      branch_point: t
                      branch:
                        - alt-fails
                        - alt-succeeds
                    - match: \S+
                      scope: text.test
                  alt-fails:
                    - match: (?=\S)
                      fail: t
                  alt-succeeds:
                    - match: \S+
                      scope: ok.test
                      pop: 1
                "#,
        );
    }

    /// Regression guard for "branch_point fail-retry drops the
    /// trigger match's `captures:` scopes". The non-fail path emits
    /// capture Push/Pop ops inside the pat_scope brackets; the
    /// same-line fail re-emit must do the same — otherwise the
    /// inner capture scopes are truncated off `ops` together with
    /// alt[0]'s subsequent work and never replayed. Observed on
    /// Haskell's `data CtxCls ctx => ModId.QTyCls`, where the
    /// `(data)(?:\s+(family|instance))?` branch_point match's first
    /// capture `keyword.declaration.data.haskell` was dropped from
    /// the `data` token whenever `data-signature` failed into
    /// `data-context` — 22 assertion failures in
    /// `syntax_test_haskell.hs`.
    #[test]
    fn branch_point_capture_scopes_survive_fail_retry() {
        // The `(word)\s` branch_point match carries both `scope:`
        // and `captures:`. Alt[0] fails on the `!` lookahead,
        // forcing replay into alt[1]. `inner.capture` on the first
        // capture group must remain on the stack over `word`.
        expect_scope_stacks(
            "word !",
            &["<outer.match>, <inner.capture>"],
            r#"
                name: Branch Capture Re-emit Test
                scope: source.test
                contexts:
                  main:
                    - match: (word)\s
                      scope: outer.match
                      captures:
                        1: inner.capture
                      branch_point: bp
                      branch:
                        - alt-fails
                        - alt-succeeds
                  alt-fails:
                    - match: (?=!)
                      fail: bp
                  alt-succeeds:
                    - match: \S+
                      scope: ok.test
                      pop: 1
                "#,
        );
    }

    /// Regression guard for "branch_point fail-retry drops the new
    /// alternative's `meta_scope` from the trigger character". The
    /// non-fail push path emits the new context's `meta_scope` at
    /// `match_start` so the matched text sees it. The same-line
    /// fail re-emit must do the same — emit `meta_scope` (and any
    /// `clear_scopes`) at `trigger_match_start`, before the
    /// trigger's `pat.scope`. Placing them after the match meant
    /// `for (var i = 0; …)` parsed the `(` with
    /// `[meta.for.js, punctuation.section.group.begin.js]` instead
    /// of `[meta.for.js, meta.group.js, punctuation.section.group.begin.js]`,
    /// failing eight assertions in `syntax_test_js_control.js`.
    #[test]
    fn branch_point_fail_retry_applies_meta_scope_to_trigger() {
        // Mirrors the JS for-loop shape: `\(` triggers a branch with
        // `pop: 1`; alt 0 fails, alt 1 succeeds; alt 1 has a
        // `meta_scope` that must wrap the `(` itself.
        expect_scope_stacks(
            "(x",
            &["<meta.group.test>, <punctuation.test>"],
            r#"
                name: Branch Meta Scope Test
                scope: source.test
                contexts:
                  main:
                    - match: ''
                      push: trigger
                  trigger:
                    - match: \(
                      scope: punctuation.test
                      branch_point: g
                      branch:
                        - alt-fails
                        - alt-succeeds
                      pop: 1
                  alt-fails:
                    - meta_scope: meta.group.test
                    - match: (?=\S)
                      fail: g
                  alt-succeeds:
                    - meta_scope: meta.group.test
                    - match: \S+
                      scope: ok.test
                      pop: 1
                "#,
        );
    }

    /// Category A proper regression guard: a same-line `branch_point`
    /// whose alternatives all `fail` must unwind to the pre-branch
    /// snapshot and advance the cursor, rather than leaving the stack
    /// stuck in the last attempted alternative. This was the cause of
    /// the Zsh `meta.interpolation.brace.shell never pops` cascade
    /// (Zsh excludes the usual `brace-interpolation-fallback` branch,
    /// so `{no}` exhausted both `sequence` and `series` alternatives
    /// and the parser silently left the scope stack inside
    /// `brace-interpolation-series-begin`).
    #[test]
    fn branch_point_with_all_alternatives_failing_unwinds_state() {
        let syntax = SyntaxDefinition::load_from_str(
            r#"
                name: All Alternatives Fail Test
                scope: source.test
                contexts:
                  main:
                    - match: (?=\{)
                      branch_point: brace
                      branch:
                        - brace-strict
                        - brace-numeric
                    - match: \w+
                      scope: plain.test
                  brace-strict:
                    - meta_scope: meta.interpolation.brace.test
                    - match: \{
                      scope: punctuation.begin.test
                      push: brace-strict-body
                  brace-strict-body:
                    - meta_content_scope: inside-strict.test
                    - match: foo
                      scope: keyword.test
                    - match: \}
                      scope: punctuation.end.test
                      pop: 2
                    - match: (?=\S)
                      fail: brace
                  brace-numeric:
                    - meta_scope: meta.interpolation.brace.test
                    - match: \{
                      scope: punctuation.begin.test
                      push: brace-numeric-body
                  brace-numeric-body:
                    - meta_content_scope: inside-numeric.test
                    - match: \d+
                      scope: constant.numeric.test
                    - match: \}
                      scope: punctuation.end.test
                      pop: 2
                    - match: (?=\S)
                      fail: brace
                "#,
            true,
            None,
        )
        .unwrap();

        let syntax_set = link(syntax);
        let mut state = ParseState::new(&syntax_set.syntaxes()[0]);
        // `{no}` — neither strict (expects `foo`) nor numeric (expects
        // digits) matches, so both branches fail. Before the fix, the
        // stack stayed in `brace-numeric-body` across the `\n`.
        let o = ops(&mut state, "{no}\n", &syntax_set);
        let mut stack = ScopeStack::new();
        for (_, op) in &o {
            stack.apply(op).unwrap();
        }
        let final_scopes: Vec<String> = stack
            .as_slice()
            .iter()
            .map(|s| format!("{:?}", s))
            .collect();
        assert!(
            !final_scopes
                .iter()
                .any(|s| s.contains("meta.interpolation.brace")),
            "meta.interpolation.brace leaked past end of line; stack: {:?}",
            final_scopes
        );
        assert!(
            !final_scopes
                .iter()
                .any(|s| s.contains("inside-strict") || s.contains("inside-numeric")),
            "inside-* meta_content_scope leaked past end of line; stack: {:?}",
            final_scopes
        );
    }

    /// Regression guard for the "cross-line branch_point exhaustion
    /// leaves contexts on the stack forever" bug: when ALL
    /// alternatives of a `branch_point` fail on a line *after* the
    /// branch was created, the parser must restore the pre-branch
    /// snapshot, truncate ops, and replay the buffered lines under
    /// the restored state. Pre-fix, the cross-line exhaustion path
    /// silently removed the branch record while leaving the last
    /// alternative's pushed contexts on the state stack — 274
    /// assertion failures in `syntax_test_typescript.ts` and
    /// another 10 in `syntax_test_C#9.cs` cascaded from that ghost
    /// state (`sublimehq/Packages#3598`'s incomplete
    /// `type x = { bar: (cb: ( };` was the minimal reproducer).
    ///
    /// Shape: a `branch_point` with two alternatives, each with a
    /// distinctive `meta_scope` and a `\w+` rule scoped by the
    /// alternative. Line 1 fires the branch; alt[0] consumes the
    /// newline and stays active. Line 2 fires `fail: bp` from
    /// alt[0] (cross-line retry into alt[1]), then the replay puts
    /// alt[1] on the stack, re-parses line 2, and alt[1] also fires
    /// `fail: bp` — cross-line exhaustion. After line 2:
    ///   - `is_speculative` must be false (branch record gone);
    ///   - a subsequent benign line must parse under the pre-branch
    ///     context (`main`), not under a leaked alternative. Pre-fix,
    ///     `beta` remained on the stack and the next line's `\w+`
    ///     scoped as `beta.word.cle` instead of `main.word.cle`.
    #[test]
    fn cross_line_branch_exhaustion_unwinds_state() {
        let syntax_str = r#"
name: CrossLineExhaustion
scope: source.cle
contexts:
  main:
    - match: 'TRY'
      scope: trigger.cle
      branch_point: bp
      branch: [alpha, beta]
    - match: '\w+'
      scope: main.word.cle
  alpha:
    - meta_scope: meta.alpha.cle
    - match: '\n'
    - match: 'FAIL'
      fail: bp
    - match: '\w+'
      scope: alpha.word.cle
  beta:
    - meta_scope: meta.beta.cle
    - match: '\n'
    - match: 'FAIL'
      fail: bp
    - match: '\w+'
      scope: beta.word.cle
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // Line 1: `TRY` fires branch `bp`; alt[0] `alpha` is pushed
        // and consumes the trailing newline, staying on the stack.
        let _out1 = state.parse_line("TRY\n", &ss).expect("parse line 1");

        // Line 2: alpha's `FAIL` rule fires `fail: bp` — cross-line
        // retry into `beta`. The beta replay leaves beta on the
        // stack; the re-parse of line 2 under beta hits beta's
        // `FAIL` rule, firing `fail: bp` again with no alternatives
        // left — cross-line exhaustion.
        let out2 = state.parse_line("FAIL\n", &ss).expect("parse line 2");

        // Exhaustion must clear every branch_point record.
        assert!(
            !state.is_speculative(),
            "cross-line exhaustion must drop all branch_point records"
        );

        // The exhaustion path replays buffered lines under the
        // restored pre-branch state, so `replayed` is non-empty.
        assert!(
            !out2.replayed.is_empty(),
            "cross-line exhaustion must emit replayed ops for the pre-branch state"
        );

        // Strong invariant: the subsequent line must be parsed under
        // `main` (the pre-branch context) — not under whichever
        // alternative was last active. Pre-fix, `beta` stayed on the
        // stack and `benign` would have scoped as `beta.word.cle`.
        let out3 = state.parse_line("benign\n", &ss).expect("parse line 3");
        let pushed: Vec<String> = out3
            .ops
            .iter()
            .filter_map(|(_, op)| match op {
                ScopeStackOp::Push(s) => Some(format!("{:?}", s)),
                _ => None,
            })
            .collect();

        assert!(
            pushed.iter().any(|s| s.contains("main.word.cle")),
            "post-exhaustion line must be scoped under main; got pushes: {:?}",
            pushed
        );
        for leaked in [
            "meta.alpha.cle",
            "meta.beta.cle",
            "alpha.word.cle",
            "beta.word.cle",
        ] {
            assert!(
                !pushed.iter().any(|s| s.contains(leaked)),
                "{} leaked into post-exhaustion line; got pushes: {:?}",
                leaked,
                pushed
            );
        }
    }

    /// Category E regression guard: a cross-line `fail` that triggers
    /// a replay which itself adds and removes branch points must not
    /// out-of-bounds-index the original `bp_index` afterwards. This
    /// test is a targeted end-to-end probe; the real reproduction lives
    /// in `testdata/Packages/JavaScript/tests/syntax_test_js.js` and
    /// `syntax_test_typescript.ts`, where nested cross-line branching
    /// previously panicked at `parser.rs:1014`. The guard leaves the
    /// scope-op stream consistent enough for the syntest harness's
    /// `catch_unwind` to report a file-level `PANIC` rather than
    /// crashing the whole run — it does not attempt to produce
    /// correct ops for the failing file (the replay-consistency issue
    /// is tracked as a follow-up).
    #[test]
    #[ignore = "requires testdata/Packages submodule"]
    fn cross_line_fail_with_nested_branch_does_not_panic() {
        use crate::parsing::SyntaxSet;
        use std::panic::AssertUnwindSafe;
        let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        let syntax = ss
            .find_syntax_by_path("Packages/JavaScript/JavaScript.sublime-syntax")
            .unwrap();
        let path = "testdata/Packages/JavaScript/tests/syntax_test_js.js";
        let content = std::fs::read_to_string(path).unwrap();
        let mut state = ParseState::new(syntax);
        // Wrap in catch_unwind so a later unrelated panic from the
        // replay-consistency issue doesn't mask the parser.rs:1014
        // regression we care about.
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            for line in content.lines() {
                let mut s = line.to_string();
                s.push('\n');
                let _ = state.parse_line(&s, &ss);
            }
        }));
        if let Err(payload) = result {
            // Extract the panic message and assert it is NOT the
            // bp_index out-of-bounds at parser.rs:1014.
            let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                String::from("<non-string panic payload>")
            };
            assert!(
                !msg.contains("index out of bounds"),
                "parser panicked with bounds violation (Category E \
                 regression): {msg}"
            );
            // A different panic (e.g. from the replay-consistency
            // issue) is acceptable here — that's tracked separately.
        }
    }

    /// Minimal repro of the Category A "pop: N loses deeper contexts'
    /// scopes" bug. Two pushed contexts A and B (with B on top): A has
    /// `meta_scope: outer`, B has `meta_content_scope: inner`. When B
    /// fires `pop: 2`, the scope stack must come fully back to the base
    /// — before the fix, A's `outer` was orphaned on the scope stack
    /// because `push_meta_ops` only emitted pops for the top context.
    /// Checked against the scope stack produced by the ops (the
    /// context-stack pop already worked; the scope-stack pop did not).
    #[test]
    fn pop_n_unwinds_all_n_contexts_meta_scopes() {
        let syntax = SyntaxDefinition::load_from_str(
            r#"
                name: Pop N Test
                scope: source.test
                contexts:
                  main:
                    - match: \(
                      scope: open
                      push: [outer, inner]
                  outer:
                    - meta_scope: outer.test
                  inner:
                    - meta_content_scope: inner.test
                    - match: \)
                      scope: close
                      pop: 2
                "#,
            true,
            None,
        )
        .unwrap();

        let syntax_set = link(syntax);
        let mut state = ParseState::new(&syntax_set.syntaxes()[0]);
        let o = ops(&mut state, "(x)\n", &syntax_set);
        let mut stack = ScopeStack::new();
        for (_, op) in &o {
            stack.apply(op).unwrap();
        }
        let final_scopes: Vec<String> = stack
            .as_slice()
            .iter()
            .map(|s| format!("{:?}", s))
            .collect();
        assert!(
            !final_scopes.iter().any(|s| s.contains("outer.test")),
            "outer.test meta_scope leaked past pop: 2; final stack: {:?}",
            final_scopes
        );
        assert!(
            !final_scopes.iter().any(|s| s.contains("inner.test")),
            "inner.test meta_content_scope leaked past pop: 2; final stack: {:?}",
            final_scopes
        );
    }

    /// End-to-end check that `make syntest`'s Makefile failure has no
    /// harness-level cause: loads the real Packages Makefile syntax
    /// and parses two lines, asserting that after `bar := $(foo)\n`
    /// the scope stack no longer carries `meta.string.makefile` when
    /// the next source line is parsed. Gated on the test-assets
    /// being available; marked `#[ignore]` so it runs with
    /// `cargo test -- --ignored` in the repo root (the Packages
    /// submodule is required).
    #[test]
    #[ignore = "requires testdata/Packages submodule"]
    fn makefile_meta_string_does_not_leak_past_eol() {
        use crate::parsing::SyntaxSet;
        let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        let syntax = ss
            .find_syntax_by_path("Packages/Makefile/Makefile.sublime-syntax")
            .unwrap();
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        for (_, op) in ops(&mut state, "bar := $(foo)\n", &ss) {
            stack.apply(&op).unwrap();
        }
        let after_assignment: Vec<String> = stack
            .as_slice()
            .iter()
            .map(|s| format!("{:?}", s))
            .collect();
        assert!(
            !after_assignment
                .iter()
                .any(|s| s.contains("meta.string.makefile")),
            "meta.string.makefile leaks past EOL of `bar := $(foo)\\n`; stack: {:?}",
            after_assignment
        );
    }

    /// Triage repro for Category A (Zsh/TSQL/Makefile "context never
    /// pops" cascade) — models the shape used by Makefile's variable
    /// definitions: a lookahead push, then `set: [value, eat]` with a
    /// zero-width match inside `value` that `set`s to a third context
    /// carrying `meta_content_scope` and `include`ing an EOL popper.
    ///
    /// On `bar\n`, after the line terminates the stack should hold no
    /// atoms of `meta.string.test`; without the fix the scope leaks to
    /// the next line because the chained `set`s leave the popper
    /// without a valid non-consuming push recorded for loop protection,
    /// so the zero-width `$` match ends up guarded as a potential loop.
    #[test]
    fn chained_set_with_included_eol_popper_pops_at_line_boundary() {
        let syntax = SyntaxDefinition::load_from_str(
            r#"
                name: EOL Pop Chained Test
                scope: source.test
                contexts:
                  main:
                    - match: (?=\S)
                      push: outer
                  outer:
                    - match: ''
                      set: [value-body, eat-whitespace-then-pop]
                  eat-whitespace-then-pop:
                    - match: \s*
                      pop: 1
                  value-body:
                    - match: ''
                      set: value-content
                  value-content:
                    - meta_content_scope: meta.string.test
                    - include: pop-on-eol
                  pop-on-eol:
                    - match: $
                      pop: 1
                "#,
            true,
            None,
        )
        .unwrap();

        let syntax_set = link(syntax);
        let mut state = ParseState::new(&syntax_set.syntaxes()[0]);
        let o = ops(&mut state, "bar\n", &syntax_set);

        // Apply ops against a fresh ScopeStack and check the final set
        // of live scope atoms — the meta_content_scope must not survive
        // across the `\n` boundary.
        let mut stack = ScopeStack::new();
        for (_, op) in &o {
            stack.apply(op).unwrap();
        }
        let final_scopes: Vec<String> = stack
            .as_slice()
            .iter()
            .map(|s| format!("{:?}", s))
            .collect();
        assert!(
            !final_scopes.iter().any(|s| s.contains("meta.string.test")),
            "meta.string.test leaked past EOL; final scope stack: {:?}",
            final_scopes
        );
    }

    fn expect_scope_stacks(line_without_newline: &str, expect: &[&str], syntax: &str) {
        println!("Parsing with newlines");
        let line_with_newline = format!("{}\n", line_without_newline);
        let syntax_newlines = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
        expect_scope_stacks_with_syntax(&line_with_newline, expect, syntax_newlines);

        println!("Parsing without newlines");
        let syntax_nonewlines = SyntaxDefinition::load_from_str(syntax, false, None).unwrap();
        expect_scope_stacks_with_syntax(line_without_newline, expect, syntax_nonewlines);
    }

    fn expect_scope_stacks_with_syntax(line: &str, expect: &[&str], syntax: SyntaxDefinition) {
        // check that each expected scope stack appears at least once while parsing the given test line

        let syntax_set = link(syntax);
        let mut state = ParseState::new(&syntax_set.syntaxes()[0]);
        let ops = ops(&mut state, line, &syntax_set);
        expect_scope_stacks_for_ops(ops, expect);
    }

    fn expect_scope_stacks_for_ops(ops: Vec<(usize, ScopeStackOp)>, expect: &[&str]) {
        let mut criteria_met = Vec::new();
        for stack_str in stack_states(ops) {
            println!("{}", stack_str);
            for expectation in expect.iter() {
                if stack_str.contains(expectation) {
                    criteria_met.push(expectation);
                }
            }
        }
        if let Some(missing) = expect.iter().find(|e| !criteria_met.contains(e)) {
            panic!("expected scope stack '{}' missing", missing);
        }
    }

    fn parse(line: &str, syntax: &str) -> Vec<(usize, ScopeStackOp)> {
        let syntax = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
        let syntax_set = link(syntax);

        let mut state = ParseState::new(&syntax_set.syntaxes()[0]);
        ops(&mut state, line, &syntax_set)
    }

    fn link(syntax: SyntaxDefinition) -> SyntaxSet {
        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax);
        builder.build()
    }

    fn ops(
        state: &mut ParseState,
        line: &str,
        syntax_set: &SyntaxSet,
    ) -> Vec<(usize, ScopeStackOp)> {
        let output = state.parse_line(line, syntax_set).expect("#[cfg(test)]");
        debug_print_ops(line, &output.ops);
        output.ops
    }

    fn stack_states(ops: Vec<(usize, ScopeStackOp)>) -> Vec<String> {
        let mut states = Vec::new();
        let mut stack = ScopeStack::new();
        for (_, op) in ops.iter() {
            stack.apply(op).expect("#[cfg(test)]");
            let scopes: Vec<String> = stack
                .as_slice()
                .iter()
                .map(|s| format!("{:?}", s))
                .collect();
            let stack_str = scopes.join(", ");
            states.push(stack_str);
        }
        states
    }

    const BRANCH_SYNTAX: &str = r#"
scope: source.branch-test
contexts:
  main:
    - match: '(?=\S)'
      branch_point: stmt
      branch: [let-stmt, generic-stmt]

  let-stmt:
    - match: 'let'
      scope: keyword.declaration.branch-test
      set: let-assign
    - match: '(?=\S)'
      fail: stmt

  let-assign:
    - match: '='
      scope: keyword.operator.assignment.branch-test
      set: let-value
    - match: '(?=\S)'
      fail: stmt

  let-value:
    - match: '\w+'
      scope: constant.other.branch-test
    - match: ';'
      scope: punctuation.terminator.branch-test
      pop: true

  generic-stmt:
    - match: '[^;]+'
      scope: string.unquoted.branch-test
    - match: ';'
      scope: punctuation.terminator.branch-test
      pop: true
"#;

    #[test]
    fn branch_first_alternative_succeeds() {
        // "let = foo;" should parse as a let-statement (first alternative)
        let syntax = SyntaxDefinition::load_from_str(BRANCH_SYNTAX, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let ops = ops(&mut state, "let = foo;", &ss);
        let states = stack_states(ops);
        // Should contain keyword.declaration and keyword.operator.assignment
        assert!(
            states.iter().any(|s| s.contains("keyword.declaration")),
            "Expected keyword.declaration scope, got: {:?}",
            states
        );
        assert!(
            states
                .iter()
                .any(|s| s.contains("keyword.operator.assignment")),
            "Expected keyword.operator.assignment scope, got: {:?}",
            states
        );
        assert!(
            states.iter().any(|s| s.contains("constant.other")),
            "Expected constant.other scope, got: {:?}",
            states
        );
    }

    #[test]
    fn branch_fail_backtracks_to_second_alternative() {
        // "hello;" is not a let-statement, should fail and use generic-stmt
        let syntax = SyntaxDefinition::load_from_str(BRANCH_SYNTAX, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let ops = ops(&mut state, "hello;", &ss);
        let states = stack_states(ops);
        // Should contain string.unquoted (generic-stmt), not keyword.declaration
        assert!(
            states.iter().any(|s| s.contains("string.unquoted")),
            "Expected string.unquoted scope, got: {:?}",
            states
        );
        assert!(
            !states.iter().any(|s| s.contains("keyword.declaration")),
            "Should NOT contain keyword.declaration scope, got: {:?}",
            states
        );
    }

    #[test]
    fn branch_fail_after_partial_match() {
        // "let hello;" — starts like a let-stmt ('let' matches) but no '=' follows, so fail
        let syntax = SyntaxDefinition::load_from_str(BRANCH_SYNTAX, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let raw_ops = ops(&mut state, "let hello;", &ss);

        // After backtracking, keyword.declaration must be absent (ops.truncate removes it)
        let states = stack_states(raw_ops.clone());
        assert!(
            !states.iter().any(|s| s.contains("keyword.declaration")),
            "keyword.declaration should be absent after backtrack, got: {:?}",
            states
        );

        // After backtracking, should use generic-stmt
        assert!(
            states.iter().any(|s| s.contains("string.unquoted")),
            "Expected string.unquoted scope after backtrack, got: {:?}",
            states
        );

        // The string.unquoted push must start at position 0 (covers "let hello", not just "hello")
        let unquoted_pos = raw_ops.iter().find_map(|(pos, op)| match op {
            ScopeStackOp::Push(s) if format!("{:?}", s).contains("string.unquoted") => Some(*pos),
            _ => None,
        });
        assert_eq!(
            unquoted_pos,
            Some(0),
            "string.unquoted should start at position 0 after rewind, got: {:?}",
            unquoted_pos
        );
    }

    #[test]
    fn branch_all_alternatives_exhausted() {
        // Test with a syntax where all alternatives fail — should not panic
        let syntax_str = r#"
scope: source.exhaust-test
contexts:
  main:
    - match: '(?=\S)'
      branch_point: bp
      branch: [alt-a, alt-b]
    - match: '\S+'
      scope: fallback.exhaust-test

  alt-a:
    - match: 'AAA'
      scope: alt-a.exhaust-test
      pop: true
    - match: '(?=\S)'
      fail: bp

  alt-b:
    - match: 'BBB'
      scope: alt-b.exhaust-test
      pop: true
    - match: '(?=\S)'
      fail: bp
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        // "xyz" matches neither AAA nor BBB
        let ops = ops(&mut state, "xyz", &ss);
        // Should not panic, and should eventually move past the input
        assert!(!ops.is_empty(), "Expected some ops, got empty");
    }

    #[test]
    fn branch_fail_emits_meta_content_scope() {
        // The second alternative has meta_content_scope; after backtracking,
        // content inside it should have that scope applied.
        let syntax_str = r#"
scope: source.meta-test
contexts:
  main:
    - match: '(?=\S)'
      branch_point: bp
      branch: [try-special, fallback-ctx]

  try-special:
    - match: 'SPECIAL'
      scope: keyword.meta-test
      pop: true
    - match: '(?=\S)'
      fail: bp

  fallback-ctx:
    - meta_content_scope: meta.fallback.meta-test
    - match: '\w+'
      scope: variable.meta-test
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let ops = ops(&mut state, "hello", &ss);
        let states = stack_states(ops);
        // After backtracking to fallback-ctx, "hello" should have meta.fallback scope
        assert!(
            states.iter().any(|s| s.contains("meta.fallback")),
            "Expected meta.fallback.meta-test scope after backtrack, got: {:?}",
            states
        );
        assert!(
            states.iter().any(|s| s.contains("variable.meta-test")),
            "Expected variable.meta-test scope, got: {:?}",
            states
        );
    }

    #[test]
    fn branch_fail_applies_with_prototype() {
        // The branch pattern has with_prototype; after backtracking to the second
        // alternative, the prototype should still be active.
        let syntax_str = r#"
scope: source.proto-test
contexts:
  main:
    - match: '(?=\S)'
      branch_point: bp
      branch: [try-num, fallback-word]
      with_prototype:
        - match: '#'
          scope: comment.proto-test
          pop: true

  try-num:
    - match: '\d+'
      scope: constant.numeric.proto-test
      pop: true
    - match: '(?=\S)'
      fail: bp

  fallback-word:
    - match: '\w+'
      scope: variable.proto-test
    - match: ';'
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        // "abc#" — 'abc' matches fallback-word, '#' should trigger the prototype
        let ops = ops(&mut state, "abc#", &ss);
        let states = stack_states(ops);
        assert!(
            states.iter().any(|s| s.contains("variable.proto-test")),
            "Expected variable.proto-test scope, got: {:?}",
            states
        );
        assert!(
            states.iter().any(|s| s.contains("comment.proto-test")),
            "Expected comment.proto-test from with_prototype after backtrack, got: {:?}",
            states
        );
    }

    #[test]
    fn branch_cross_line_backtrack() {
        // Syntax: "TRY" on line 1 triggers a branch_point.  try-ctx stays
        // active (consuming the trailing newline) so that it is still live on
        // line 2.  "FAIL" on line 2 fires `fail: bp`, which must rewind to
        // fallback-ctx and re-parse line 1 under that alternative.
        // After parsing line 2, `replayed` must contain corrected ops for
        // line 1 (with the `fallback.content` scope, not a `try.*` scope).
        let syntax_str = r#"
name: CrossLineTest
scope: source.clt
contexts:
  main:
    - match: 'TRY'
      branch_point: bp
      branch: [try-ctx, fallback-ctx]
    - match: '.*'
      scope: main.other
  try-ctx:
    - match: '\n'
      # consume newline, stay in context for the next line
    - match: 'FAIL'
      fail: bp
    - match: '\w+'
      scope: try.word
      pop: true
  fallback-ctx:
    - match: '.*'
      scope: fallback.content
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // Line 1: triggers the branch, tries try-ctx first.
        // try-ctx consumes the newline and stays active.
        let out1 = state.parse_line("TRY\n", &ss).expect("parse line 1 failed");
        // replayed is empty on line 1 (no cross-line fail yet)
        assert!(
            out1.replayed.is_empty(),
            "line 1: expected no replayed ops, got {:?}",
            out1.replayed
        );

        // Line 2: "FAIL" triggers fail: bp — cross-line backtrack.
        // `replayed` must contain re-parsed ops for line 1 under fallback-ctx.
        let out2 = state
            .parse_line("FAIL\n", &ss)
            .expect("parse line 2 failed");
        assert_eq!(
            out2.replayed.len(),
            1,
            "expected exactly one replayed line, got {:?}",
            out2.replayed
        );
        let has_fallback = out2.replayed[0].iter().any(|(_, op)| {
            matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("fallback.content"))
        });
        assert!(
            has_fallback,
            "expected fallback.content scope in replayed line 1 ops, got: {:?}",
            out2.replayed[0]
        );
        // The try.word scope must NOT appear in the replayed ops.
        let has_try_word = out2.replayed[0].iter().any(|(_, op)| {
            matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("try.word"))
        });
        assert!(
            !has_try_word,
            "try.word must not appear in replayed ops after backtrack, got: {:?}",
            out2.replayed[0]
        );
        // Verify current-line ops are clean (ops.clear() fired before re-parse)
        let current_has_try = out2.ops.iter().any(
            |(_, op)| matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("try")),
        );
        assert!(
            !current_has_try,
            "current-line ops should not contain try.* scopes after cross-line fail, got: {:?}",
            out2.ops
        );
    }

    #[test]
    fn cross_line_fail_preserves_pre_branch_prefix_ops() {
        // Replay of the first buffered line on a cross-line fail must
        // preserve the pre-branch prefix ops (which were correctly emitted
        // under the pre-branch state) rather than re-parsing the whole
        // line under the new alternative.
        //
        // Reduced from multi-line SQL `LIKE '…' ESCAPE '…'`: the first
        // buffered line contains a prefix (`prefix `) before the branch
        // trigger (`TRY`). Under the fallback alternative's rules, `prefix`
        // would be scoped as fallback.content from column 0 — but the
        // test expects the original `prefix.word` scope to survive the
        // replay because those characters were parsed under the pre-branch
        // (main) context.
        let syntax_str = r#"
name: CrossLinePrefix
scope: source.clp
contexts:
  main:
    - match: 'prefix'
      scope: prefix.word.clp
    - match: 'TRY'
      branch_point: bp
      branch: [try-ctx, fallback-ctx]
    - match: '\s+'
  try-ctx:
    - match: 'END'
      pop: true
    - match: 'FAIL'
      fail: bp
    - match: '\w+'
      scope: try.word.clp
    - match: '\s+'
  fallback-ctx:
    - match: 'END'
      pop: true
    - match: '\w+'
      scope: fallback.content.clp
    - match: '\s+'
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // Line 1: "prefix TRY post\n" — prefix scoped by main, TRY triggers
        // branch, post is parsed under the chosen alternative.
        let _out1 = state
            .parse_line("prefix TRY post\n", &ss)
            .expect("parse line 1 failed");

        // Line 2: "FAIL\n" — cross-line fail triggers replay of line 1.
        let out2 = state
            .parse_line("FAIL\n", &ss)
            .expect("parse line 2 failed");
        assert_eq!(
            out2.replayed.len(),
            1,
            "expected one replayed line, got {:?}",
            out2.replayed
        );
        // The replayed ops for line 1 must still push prefix.word at col 0
        // (from prefix_ops, emitted pre-branch), not overwrite with
        // fallback.content.
        let replayed_has_prefix = out2.replayed[0].iter().any(|(_, op)| {
            matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("prefix.word"))
        });
        assert!(
            replayed_has_prefix,
            "replayed line must preserve prefix.word from pre-branch parse, got: {:?}",
            out2.replayed[0]
        );
        // fallback.content should appear for the post-TRY remainder.
        let replayed_has_fallback = out2.replayed[0].iter().any(|(_, op)| {
            matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("fallback.content"))
        });
        assert!(
            replayed_has_fallback,
            "replayed line must apply fallback.content for post-branch remainder, got: {:?}",
            out2.replayed[0]
        );
    }

    #[test]
    fn branch_point_expiry_after_128_lines() {
        // A branch point created on line 0 should be discarded when `fail`
        // fires after 129+ lines have elapsed, and a warning should be emitted.
        let syntax_str = r#"
name: ExpiryTest
scope: source.expiry-test
contexts:
  main:
    - match: 'START'
      branch_point: bp
      branch: [try-ctx, fallback-ctx]
    - match: '.*'
      scope: filler.expiry-test
  try-ctx:
    - match: '\n'
      # consume newlines, staying in context
    - match: 'FAIL'
      fail: bp
    - match: '\w+'
      scope: try.matched
      pop: true
  fallback-ctx:
    - match: '.*'
      scope: fallback.content
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        let out0 = state.parse_line("START\n", &ss).expect("parse START");
        assert!(out0.replayed.is_empty());

        // Feed 129 empty lines to exceed the 128-line limit.
        // The pruning warning fires during the filler line that crosses the threshold.
        let mut all_warnings: Vec<String> = Vec::new();
        for _ in 0..129 {
            let out = state.parse_line("\n", &ss).expect("parse filler");
            all_warnings.extend(out.warnings);
        }

        // Now fire fail — should be a no-op (branch point expired)
        let out_fail = state.parse_line("FAIL\n", &ss).expect("parse FAIL");
        all_warnings.extend(out_fail.warnings);
        assert!(
            out_fail.replayed.is_empty(),
            "branch point should have expired, but got replayed ops: {:?}",
            out_fail.replayed
        );
        assert!(
            all_warnings
                .iter()
                .any(|w| w.contains("expired") && w.contains("bp")),
            "expected a warning about branch point expiry, got: {:?}",
            all_warnings
        );
    }

    #[test]
    fn branch_point_still_valid_at_128_lines() {
        // A branch point created on line 0 should still be alive when
        // exactly 128 lines have elapsed (boundary: 128 - 0 = 128 <= 128).
        let syntax_str = r#"
name: ExpiryTest
scope: source.expiry-test
contexts:
  main:
    - match: 'START'
      branch_point: bp
      branch: [try-ctx, fallback-ctx]
    - match: '.*'
      scope: filler.expiry-test
  try-ctx:
    - match: '\n'
      # consume newlines, staying in context
    - match: 'FAIL'
      fail: bp
    - match: '\w+'
      scope: try.matched
      pop: true
  fallback-ctx:
    - match: '.*'
      scope: fallback.content
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        let out0 = state.parse_line("START\n", &ss).expect("parse START");
        assert!(out0.replayed.is_empty());

        // Feed exactly 127 filler lines so that FAIL lands on cur_line=128
        // (128 - 0 = 128 <= 128, so the branch point is still valid)
        let mut all_warnings: Vec<String> = Vec::new();
        for _ in 0..127 {
            let out = state.parse_line("\n", &ss).expect("parse filler");
            all_warnings.extend(out.warnings);
        }

        // Fire fail — branch point should still be alive at the boundary
        let out_fail = state.parse_line("FAIL\n", &ss).expect("parse FAIL");
        all_warnings.extend(out_fail.warnings);
        assert!(
            !out_fail.replayed.is_empty(),
            "branch point should still be valid at exactly 128 lines, but got no replayed ops"
        );
        assert!(
            all_warnings.is_empty(),
            "expected no warnings at the 128-line boundary, got: {:?}",
            all_warnings
        );
        let has_fallback = out_fail.replayed[0].iter().any(|(_, op)| {
            matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("fallback.content"))
        });
        assert!(
            has_fallback,
            "expected fallback.content in replayed ops, got: {:?}",
            out_fail.replayed[0]
        );
    }

    #[test]
    fn branch_stack_depth_invalidation() {
        // Test two scenarios:
        // 1. fail fires when stack depth == bp depth (should succeed)
        // 2. fail fires when stack depth < bp depth (should be a no-op)
        let syntax_str = r#"
name: DepthTest
scope: source.depth-test
contexts:
  main:
    - match: '(?=\S)'
      branch_point: bp
      branch: [try-ctx, fallback-ctx]
  try-ctx:
    - match: 'OK'
      scope: try.ok
      set: post-try
    - match: '(?=\S)'
      fail: bp
  post-try:
    - match: 'FAIL'
      fail: bp
    - match: '\w+'
      scope: post.word
      pop: true
  fallback-ctx:
    - match: '.*'
      scope: fallback.content
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);

        // Scenario 1: fail at equal depth should succeed.
        // OK matches in try-ctx, `set` to post-try (depth unchanged since set = pop+push).
        // FAIL fires in post-try at the same depth as bp → backtrack succeeds.
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let line_ops = ops(&mut state, "OK FAIL\n", &ss);
        let states = stack_states(line_ops);
        assert!(
            states.iter().any(|s| s.contains("fallback.content")),
            "fail at equal depth should trigger backtrack to fallback, got: {:?}",
            states
        );
        assert!(
            !states.iter().any(|s| s.contains("try.ok")),
            "try.ok should be absent after backtrack, got: {:?}",
            states
        );

        // Scenario 2: fail at shallower depth should be a no-op.
        // Use a syntax where the branch context pops before fail fires.
        let syntax_str2 = r#"
name: DepthTest2
scope: source.depth-test2
contexts:
  main:
    - match: 'GO'
      branch_point: bp
      branch: [try-ctx2, fallback-ctx2]
    - match: 'FAIL'
      fail: bp
    - match: '.*'
      scope: main.other
  try-ctx2:
    - match: 'OK'
      scope: try.ok2
      pop: true
    - match: '(?=\S)'
      fail: bp
  fallback-ctx2:
    - match: '.*'
      scope: fallback.content2
      pop: true
"#;
        let syntax2 = SyntaxDefinition::load_from_str(syntax_str2, true, None).unwrap();
        let ss2 = link(syntax2);
        let mut state2 = ParseState::new(&ss2.syntaxes()[0]);
        // GO pushes try-ctx2 (depth increases), OK pops back to main (depth decreases).
        // FAIL fires in main at depth < bp depth → no-op.
        let line_ops2 = ops(&mut state2, "GO OK FAIL\n", &ss2);
        let states2 = stack_states(line_ops2);
        assert!(
            !states2.iter().any(|s| s.contains("fallback.content2")),
            "fail should be a no-op when stack is shallower than branch point, got: {:?}",
            states2
        );
        assert!(
            states2.iter().any(|s| s.contains("try.ok2")),
            "expected try.ok2 from first alternative, got: {:?}",
            states2
        );
    }

    #[test]
    fn branch_nested_overlapping_branch_points() {
        // Two branch points active simultaneously. The inner one fails,
        // the outer should remain valid.
        let syntax_str = r#"
name: NestedTest
scope: source.nested-test
contexts:
  main:
    - match: '(?=\S)'
      branch_point: outer
      branch: [outer-try, outer-fallback]
  outer-try:
    - match: 'A'
      scope: outer.a
      set: inner-branch
    - match: '(?=\S)'
      fail: outer
  inner-branch:
    - match: '(?=\S)'
      branch_point: inner
      branch: [inner-try, inner-fallback]
  inner-try:
    - match: 'X'
      scope: inner.x
      pop: true
    - match: '(?=\S)'
      fail: inner
  inner-fallback:
    - match: '\w+'
      scope: inner.fallback
      pop: true
  outer-fallback:
    - match: '.*'
      scope: outer.fallback
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // "A B" — A matches outer-try, B fails inner → inner-fallback matches B
        let line_ops = ops(&mut state, "A B\n", &ss);
        let states = stack_states(line_ops);
        assert!(
            states.iter().any(|s| s.contains("inner.fallback")),
            "expected inner.fallback after inner branch fail, got: {:?}",
            states
        );
        assert!(
            !states.iter().any(|s| s.contains("outer.fallback")),
            "outer branch should not have failed, got: {:?}",
            states
        );
    }

    /// Regression guard for the Haskell raw-string quasi-quote bug.
    ///
    /// Haskell's `brackets` context routes `[` through
    /// `branch_point: list-or-quasiquote` with alternatives
    /// `[list, quasi-quote]`. The `list` alternative matches `[` and
    /// `set: list-body`; `list-body` includes `list-fail` whose
    /// `\|\]` rule fires `fail: list-or-quasiquote` to fall back to
    /// the quasi-quote alternative. When a quasi-quote body contains
    /// a bracket expression (e.g. the regex raw string
    /// `[r|[a-zA-Z]+|]`), the nested `[` reopens `brackets`, creating
    /// a second `branch_point` with the same name. Its `list`
    /// alternative resolves cleanly when the nested `]` fires. Before
    /// the fix, the inner branch_point record stayed in the vec
    /// (the Pop retain predicate was `bp.stack_depth <= stack.len()`,
    /// non-strict), and a later `fail: list-or-quasiquote` from the
    /// outer list-body `rposition`'d onto the stale inner record,
    /// rewinding to the inner `[` instead of the outer one. The outer
    /// list's meta_scope stayed on the stack and the quasi-quote
    /// alternative never fired — cascading into ~90 col-weighted
    /// syntest failures across the raw-string QQ examples in
    /// `syntax_test_haskell.hs`.
    ///
    /// Shape mirrors Haskell: `brackets` is the branch point,
    /// alternatives are thin wrappers that `set:` onto their real
    /// bodies, so the bp's `stack_depth` lines up with the eventual
    /// content-body depth.
    #[test]
    fn nested_same_name_branch_point_outer_fail_replays_outer() {
        let syntax_str = r#"
name: NestedSameNameBranch
scope: source.nested-same-name-branch
contexts:
  main:
    - include: brackets

  brackets:
    - match: '(?=\[)'
      branch_point: bp
      branch: [list, quasi]

  list:
    - match: '\['
      scope: list.open
      set: list-body

  list-body:
    - meta_scope: list.body
    - match: '\|\]'
      fail: bp
    - match: '\]'
      scope: list.close
      pop: true
    - include: brackets
    - match: '\w+'
      scope: list.word

  quasi:
    - match: '\['
      scope: quasi.open
      set: quasi-body

  quasi-body:
    - meta_scope: quasi.body
    - match: '\|\]'
      scope: quasi.close
      pop: true
    - match: '.'
      scope: quasi.char
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // `[x[y]|]` — outer `[` opens bp (list first). list sets
        // list-body. Inside, `x` is a word, then nested `[y]` opens a
        // second bp whose list alternative resolves via `]`. Outer
        // list-body then hits `|]` and fires `fail: bp`. Expected:
        // outer's quasi alternative takes over — everything inside
        // `[...|]` ends up as `quasi.body` / `quasi.char`, with no
        // `list.body` meta_scope leaking past the replay.
        let line_ops = ops(&mut state, "[x[y]|]\n", &ss);
        let states = stack_states(line_ops);
        assert!(
            states.iter().any(|s| s.contains("quasi.body")),
            "expected quasi.body after outer fail replay, got: {:?}",
            states
        );
        assert!(
            !states.iter().any(|s| s.contains("list.body")),
            "list.body meta_scope leaked past outer fail replay, got: {:?}",
            states
        );
    }

    #[test]
    fn branch_fail_nonexistent_name() {
        // `fail: nonexistent` should be a silent no-op — no panic, parsing continues.
        let syntax_str = r#"
name: NoNameTest
scope: source.noname-test
contexts:
  main:
    - match: '\w+'
      scope: word.noname-test
    - match: '(?=;)'
      fail: nonexistent
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // The key assertion is that this doesn't panic
        let line_ops = ops(&mut state, "hello;\n", &ss);
        let states = stack_states(line_ops);
        assert!(
            states.iter().any(|s| s.contains("word.noname-test")),
            "expected word.noname-test, got: {:?}",
            states
        );
    }

    #[test]
    fn branch_cross_line_multi_replay() {
        // When `fail` fires after 3+ buffered lines, all of them should be
        // replayed correctly under the fallback alternative.
        let syntax_str = r#"
name: MultiReplayTest
scope: source.multi-replay
contexts:
  main:
    - match: 'TRY'
      branch_point: bp
      branch: [try-ctx, fallback-ctx]
    - match: '.*'
      scope: main.other
  try-ctx:
    - match: '\n'
      # stay in context
    - match: 'FAIL'
      fail: bp
    - match: '\w+'
      scope: try.word
  fallback-ctx:
    - match: '.*'
      scope: fallback.content
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        let out1 = state.parse_line("TRY\n", &ss).expect("line 1");
        assert!(out1.replayed.is_empty());

        let out2 = state.parse_line("aaa\n", &ss).expect("line 2");
        assert!(out2.replayed.is_empty());

        let out3 = state.parse_line("bbb\n", &ss).expect("line 3");
        assert!(out3.replayed.is_empty());

        // Line 4: "FAIL" triggers cross-line backtrack; lines 1-3 should be replayed
        let out4 = state.parse_line("FAIL\n", &ss).expect("line 4");
        assert_eq!(
            out4.replayed.len(),
            3,
            "expected 3 replayed lines (lines 1-3), got {:?}",
            out4.replayed
        );

        // The first replayed line (replay of "TRY\n") should have fallback.content
        // because fallback-ctx matches `.*`. After that pop, lines 2-3 are parsed
        // by main, which matches `.*` → main.other.
        let has_fallback = out4.replayed[0].iter().any(|(_, op)| {
            matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("fallback.content"))
        });
        assert!(
            has_fallback,
            "replayed line 0 missing fallback.content, got: {:?}",
            out4.replayed[0]
        );

        // No replayed line should have try.word (all are under fallback path)
        for (i, line_ops) in out4.replayed.iter().enumerate() {
            let has_try_word = line_ops.iter().any(|(_, op)| {
                matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("try.word"))
            });
            assert!(
                !has_try_word,
                "replayed line {} should not have try.word, got: {:?}",
                i, line_ops
            );
        }
        // Verify current-line ops are clean (ops.clear() fired before re-parse)
        let current_has_try = out4.ops.iter().any(
            |(_, op)| matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("try")),
        );
        assert!(
            !current_has_try,
            "current-line ops should not contain try.* scopes after cross-line fail, got: {:?}",
            out4.ops
        );
    }

    #[test]
    fn branch_cross_line_fail_with_preceding_ops() {
        // When the fail-triggering line has matchable content BEFORE the fail keyword,
        // ops are non-empty and start > 0 when fail fires. After cross-line backtrack,
        // those stale ops must be cleared and the line re-parsed from position 0.
        let syntax_str = r#"
name: PrecedingOpsTest
scope: source.preceding-ops
contexts:
  main:
    - match: 'TRY'
      branch_point: bp
      branch: [try-ctx, fallback-ctx]
    - match: '.*'
      scope: main.other
  try-ctx:
    - match: '\n'
      # stay in context across lines
    - match: 'FAIL'
      fail: bp
    - match: '\w+'
      scope: try.word
  fallback-ctx:
    - match: '.*'
      scope: fallback.content
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // Line 1: start the branch
        let out1 = state.parse_line("TRY\n", &ss).expect("line 1");
        assert!(out1.replayed.is_empty());

        // Line 2: "stuff FAIL" — "stuff" matches try.word (ops non-empty, start advances)
        // then FAIL triggers cross-line backtrack.
        let out2 = state.parse_line("stuff FAIL\n", &ss).expect("line 2");

        // Should have replayed line 1 (TRY\n)
        assert_eq!(
            out2.replayed.len(),
            1,
            "expected 1 replayed line, got {}",
            out2.replayed.len()
        );

        // Replayed line should have fallback.content, not try.word
        let replay_has_fallback = out2.replayed[0].iter().any(|(_, op)| {
            matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("fallback.content"))
        });
        assert!(
            replay_has_fallback,
            "replayed line should have fallback.content, got: {:?}",
            out2.replayed[0]
        );

        // Current-line ops must NOT contain try.word (stale ops were cleared)
        let current_has_try = out2.ops.iter().any(|(_, op)| {
            matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("try.word"))
        });
        assert!(
            !current_has_try,
            "current-line ops should not contain try.word after cross-line fail, got: {:?}",
            out2.ops
        );

        // Current-line ops should have main.other (re-parsed from position 0)
        let current_has_main = out2.ops.iter().any(|(_, op)| {
            matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("main.other"))
        });
        assert!(
            current_has_main,
            "current-line should be re-parsed as main.other from position 0, got: {:?}",
            out2.ops
        );
    }

    // ── Mutation-killing pass 3 ──────────────────────────────────────────

    #[test]
    fn is_speculative_reflects_branch_state() {
        // Kills: L306 replace is_speculative -> true / false / delete !
        // is_speculative must be true while inside a branch_point and false otherwise.
        // We use a syntax where both alternatives fail, so the branch point is
        // fully exhausted and removed.
        let syntax_str = r#"
name: SpeculativeTest
scope: source.spec-test
contexts:
  main:
    - match: '(?=\S)'
      branch_point: bp
      branch: [alt-a, alt-b]
    - match: '\S+'
      scope: fallback.spec-test

  alt-a:
    - match: 'AAA'
      scope: alt-a.spec-test
      pop: true
    - match: '(?=\S)'
      fail: bp

  alt-b:
    - match: 'BBB'
      scope: alt-b.spec-test
      pop: true
    - match: '(?=\S)'
      fail: bp
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // Before any branch: not speculative
        assert!(
            !state.is_speculative(),
            "should not be speculative before branch_point is created"
        );

        // "AAA" matches first alternative, so branch stays open (could still fail)
        let _ = state.parse_line("AAA\n", &ss).unwrap();
        // Branch point is created then first alt succeeds — but bp stays until
        // explicitly removed.  Since AAA matched and popped, bp is still there.
        // Actually let's test with a failing input instead:
        let mut state2 = ParseState::new(&ss.syntaxes()[0]);
        assert!(!state2.is_speculative());

        // "xyz" matches neither AAA nor BBB: both alternatives fail, bp exhausted & removed
        let _ = state2.parse_line("xyz\n", &ss).unwrap();
        assert!(
            !state2.is_speculative(),
            "should not be speculative after all alternatives exhausted"
        );

        // Now test it IS speculative mid-branch: use a cross-line syntax
        let syntax_str2 = r#"
name: SpecCross
scope: source.spec-cross
contexts:
  main:
    - match: 'TRY'
      branch_point: bp
      branch: [try-ctx, fallback-ctx]
    - match: '.*'
      scope: main.other
  try-ctx:
    - match: '\n'
    - match: 'FAIL'
      fail: bp
    - match: '\w+'
      scope: try.word
      pop: true
  fallback-ctx:
    - match: '.*'
      scope: fallback.content
      pop: true
"#;
        let syntax2 = SyntaxDefinition::load_from_str(syntax_str2, true, None).unwrap();
        let ss2 = link(syntax2);
        let mut state3 = ParseState::new(&ss2.syntaxes()[0]);
        assert!(!state3.is_speculative());

        let _ = state3.parse_line("TRY\n", &ss2).unwrap();
        assert!(
            state3.is_speculative(),
            "should be speculative after branch_point creation"
        );
    }

    #[test]
    fn consuming_match_not_treated_as_loop() {
        // Kills: L533 replace > with < in find_best_match (consuming check)
        // A pop that consumes characters must NOT be treated as a loop.
        // If consuming is negated (> → <), a consuming pop would be flagged
        // as a loop and skipped, breaking the parse.
        let syntax_str = r#"
name: ConsumingTest
scope: source.consuming
contexts:
  main:
    - match: '(?=\S)'
      push: inner
    - match: '\n'
  inner:
    - match: '\w+'
      scope: word.consuming
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // "(?=\S)" is a zero-length push (non-consuming), then "\w+" is a
        // consuming pop inside inner.  If the consuming check is inverted,
        // the pop would be treated as looping and skipped, causing the parser
        // to advance one character before matching, so the push position
        // would be 1 instead of 0.
        let raw_ops = ops(&mut state, "hello world\n", &ss);
        let first_word_pos = raw_ops
            .iter()
            .find_map(|(pos, op)| match op {
                ScopeStackOp::Push(s) if format!("{:?}", s).contains("word.consuming") => {
                    Some(*pos)
                }
                _ => None,
            })
            .expect("expected at least one word.consuming push");
        assert_eq!(
            first_word_pos, 0,
            "word.consuming must start at position 0 (consuming pop should not be treated as loop)"
        );
    }

    #[test]
    fn captures_clipped_to_match_bounds_when_group_extends_past_match_end() {
        // Repro of a C# generic-function-call divergence against ST.
        // Rule shape: a consumed identifier, then a lookahead containing
        // a capturing group whose match extends *past* the outer rule's
        // consumed end, then a second consumed group starting at the
        // same column where the lookahead began. `captures: 2:` targets
        // the lookahead-internal group. ST clips each captures:N span
        // to the rule's match bounds and only colours the overlap —
        // which here is the single consumed char at the match-end
        // boundary. Syntect used to colour the full group-2 range,
        // emitting a Pop past match_end and leaving the scope active
        // over chars the match never consumed.
        let syntax_str = r#"
name: CapturesClip
scope: source.capclip
contexts:
  main:
    - match: '(foo)(?=(barrr)baz)(bar)'
      captures:
        1: captured-foo.capclip
        2: lookahead-group.capclip
        3: consumed-bar.capclip
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let raw_ops = ops(&mut state, "foobarrrbaz\n", &ss);

        // The rule consumes "foobar" — match_start=0, match_end=6.
        // Group 2's own range (the lookahead match "barrr") extends to
        // column 8. Every op emitted by the captures application must
        // sit within [match_start, match_end]; anything at col 7+ means
        // the lookahead-internal group's span leaked past the match.
        // match_start=0, match_end=6 (rule consumes "foobar"). Group 2's
        // own range (the lookahead match "barrr") extends to col 8.
        //
        // After the fix we expect:
        //   * `lookahead-group.capclip` Pushed at col 3 (cap_start of
        //     group 2, which overlaps the consumed region).
        //   * The matching Pop no later than col 6 (clipped to match_end).
        //   * No op at col 7 or 8 — anything there means the lookahead
        //     range leaked past the match.
        let lookahead_pushes: Vec<usize> = raw_ops
            .iter()
            .filter_map(|(pos, op)| match op {
                ScopeStackOp::Push(s) if format!("{:?}", s).contains("lookahead-group") => {
                    Some(*pos)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            lookahead_pushes,
            vec![3],
            "`captures: 2:` (lookahead-internal group) must Push the \
             clipped scope at match_start=3; raw_ops={:?}",
            raw_ops
        );
        let match_end = 6;
        let past_match: Vec<_> = raw_ops.iter().filter(|(pos, _)| *pos > match_end).collect();
        assert!(
            past_match.is_empty(),
            "No capture op should sit past match_end={}; found {:?}",
            match_end,
            past_match
        );
    }

    #[test]
    fn capture_sort_by_span_length() {
        // Kills: L709 replace - with + in exec_pattern (capture sort key)
        // Captures are sorted so that longer spans come first (pushed before
        // shorter nested ones).  If the sort key sign is flipped, shorter
        // spans push first, producing the wrong nesting order.
        let syntax_str = r#"
name: CaptureSort
scope: source.capsort
contexts:
  main:
    - match: '((a)(b))'
      captures:
        1: outer.capsort
        2: inner-a.capsort
        3: inner-b.capsort
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let raw_ops = ops(&mut state, "ab\n", &ss);

        // With correct sorting: outer pushes first (at pos 0), then inner-a
        // at the same position.  With the sign flipped, inner-a would push
        // before outer, which is wrong.
        let push_order: Vec<&str> = raw_ops
            .iter()
            .filter_map(|(_, op)| match op {
                ScopeStackOp::Push(s) => {
                    let name = format!("{:?}", s);
                    if name.contains("outer.capsort") {
                        Some("outer")
                    } else if name.contains("inner-a.capsort") {
                        Some("inner-a")
                    } else if name.contains("inner-b.capsort") {
                        Some("inner-b")
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            push_order,
            vec!["outer", "inner-a", "inner-b"],
            "captures must push in longest-span-first order, got: {:?}",
            push_order
        );
    }

    #[test]
    fn v2_set_pops_meta_content_scope_from_matched_text() {
        // Kills: L1009 replace += with -= or *= in push_meta_ops
        // When a v2 syntax uses `set`, num_to_pop must include
        // cur_context.meta_scope.len() so that the old meta scope is removed.
        let syntax_str = r#"
name: V2SetMeta
scope: source.v2setmeta
version: 2
contexts:
  main:
    - match: '(?=\S)'
      push: ctx-a
  ctx-a:
    - meta_scope: meta.a.v2setmeta
    - match: 'GO'
      scope: keyword.go.v2setmeta
      set: ctx-b
    - match: '\w+'
      scope: word.a.v2setmeta
  ctx-b:
    - meta_scope: meta.b.v2setmeta
    - match: '\w+'
      scope: word.b.v2setmeta
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let raw_ops = ops(&mut state, "GO hello\n", &ss);
        let states = stack_states(raw_ops);

        // After "GO" triggers `set: ctx-b`, meta.a should be popped and
        // meta.b should be active on "hello".  If num_to_pop is wrong
        // (e.g. subtracted instead of added), meta.a would persist.
        let last_state = states.last().expect("expected some states");
        assert!(
            !last_state.contains("meta.a.v2setmeta"),
            "meta.a should have been popped after set, got: {:?}",
            last_state
        );
    }

    #[test]
    fn v2_set_from_context_with_clear_scopes_restores_cleared_atoms() {
        // When a `set:` fires from a context that had `clear_scopes` of its
        // own (e.g. JSON's `object-value-body`), the cleared scopes must be
        // restored at the correct position on the scope stack: below the
        // target's pushed meta_scope, not on top of it.
        //
        // Previously the Restore fired in the initial phase, before the
        // non-initial Pop of (cur.meta_scope + target.meta_scope). The Pop
        // then removed the restored atoms instead of the intended meta_scopes,
        // dropping cur's cleared state on the floor. This surfaced in the
        // JSON test as duplicate `meta.mapping.value.json` atoms in nested
        // objects — e.g. `[source.json, meta.mapping.value.json,
        // meta.mapping.value.json]` instead of `[source.json,
        // meta.mapping.value.json, meta.mapping.json]`.
        //
        // Reduced JSON-like repro: outer mapping pushes an inner value-body
        // that clears the outer mapping scope, then the inner value-body
        // `set`s to a follow-up context. The follow-up context's matched
        // text must see the outer mapping scope restored below it.
        let syntax_str = r#"
name: V2SetRestore
scope: source.v2setrestore
version: 2
contexts:
  main:
    - meta_scope: meta.outer.v2setrestore
    - match: '\{'
      push: value-body

  value-body:
    - clear_scopes: 1
    - meta_scope: meta.value.v2setrestore
    - match: 'x'
      scope: keyword.x.v2setrestore
      set: follow-up

  follow-up:
    - match: '\w+'
      scope: word.follow.v2setrestore
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let raw_ops = ops(&mut state, "{xhello\n", &ss);

        let states = stack_states(raw_ops);
        // Find the state while parsing "hello" in follow-up.
        let follow_states: Vec<_> = states
            .iter()
            .filter(|s| s.contains("word.follow.v2setrestore"))
            .collect();
        assert!(
            !follow_states.is_empty(),
            "expected to enter follow-up context, got states: {:?}",
            states
        );
        // The outer meta.outer scope must be restored below follow-up's word
        // scope. If the Restore landed above the target's meta_scope push (or
        // was dropped by the non-initial Pop), meta.outer would be missing.
        assert!(
            follow_states
                .iter()
                .any(|s| s.contains("meta.outer.v2setrestore")),
            "meta.outer must be restored after leaving value-body (which cleared it): {:?}",
            follow_states
        );
        // meta.value (from the cleared context) must NOT persist.
        assert!(
            !follow_states
                .iter()
                .any(|s| s.contains("meta.value.v2setrestore")),
            "meta.value (from the exited context) must not leak into follow-up: {:?}",
            follow_states
        );
    }

    #[test]
    fn v2_set_to_target_with_clear_scopes_clears_parent_meta_content_scope() {
        // Reduced from Lisp `function-parameter-list` → `function-parameter-list-body`:
        // the enclosing `function-body` supplies `meta_content_scope:
        // meta.function.lisp`; the inner parameter-list-body declares
        // `clear_scopes: 1` so the `(` and the parameter identifiers inside
        // are not double-scoped with the outer `meta.function`.
        //
        // The `(` token itself (the `set:` trigger) should see the cleared
        // stack — i.e. `meta.function.lisp` is already gone at that column.
        // Previously the v2 initial phase for Set pushed target.meta_scope
        // above the outer mcs without clearing first, so the trigger token
        // reported `[..., meta.function.lisp, meta.function.parameters.lisp,
        // punctuation...]` instead of `[..., meta.function.parameters.lisp,
        // punctuation...]`.
        let syntax_str = r#"
name: V2SetTargetClear
scope: source.v2settargetclear
version: 2
contexts:
  main:
    - match: '\('
      scope: punctuation.section.parens.begin.v2settargetclear
      push: [body, params-open]

  body:
    - meta_content_scope: meta.function.v2settargetclear
    - match: '\)'
      scope: punctuation.section.parens.end.v2settargetclear
      pop: 1

  params-open:
    - match: '\('
      scope: punctuation.section.parameters.begin.v2settargetclear
      set: params-body
    - include: else-pop

  params-body:
    - clear_scopes: 1
    - meta_scope: meta.function.parameters.v2settargetclear
    - match: '\)'
      scope: punctuation.section.parameters.end.v2settargetclear
      pop: 1
    - match: '\w+'
      scope: variable.parameter.v2settargetclear
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        // Mirrors `(defun averagenum (n1 n2))`: outer `(...)` carries
        // meta.function; inner `(...)` is parameter list.
        let raw_ops = ops(&mut state, "( (n1 n2))\n", &ss);

        let states = stack_states(raw_ops);

        // Find the state covering the inner `(` at column 2 (the set trigger).
        // Every state recorded once the parameters context has been entered
        // must NOT still carry the outer meta.function atom.
        let param_states: Vec<_> = states
            .iter()
            .filter(|s| s.contains("meta.function.parameters.v2settargetclear"))
            .collect();
        assert!(
            !param_states.is_empty(),
            "expected to enter params-body context, got states: {:?}",
            states
        );
        // The outer `meta.function` atom (from `body`'s meta_content_scope)
        // must be absent on every state where params-body is active. Match
        // the exact atom name — not a prefix — so that
        // `meta.function.parameters.v2settargetclear` doesn't trigger.
        let outer = "<meta.function.v2settargetclear>";
        for s in &param_states {
            assert!(
                !s.contains(outer),
                "outer meta.function must be cleared under params-body, \
                 but found it alongside meta.function.parameters: {:?}",
                s
            );
        }

        // After the inner `)` pops params-body the clear must Restore, so
        // the outer `meta.function` atom reappears before the outer `)`.
        let after_inner_close: Vec<_> = states
            .iter()
            .rev()
            .take_while(|s| !s.contains("meta.function.parameters.v2settargetclear"))
            .collect();
        assert!(
            after_inner_close
                .iter()
                .any(|s| s.contains("meta.function.v2settargetclear")),
            "meta.function must be restored after params-body pops, got trailing states: {:?}",
            after_inner_close
        );
    }

    #[test]
    fn pop_n_set_with_cur_clear_scopes_restores_before_popping_deeper_frames() {
        // `pop: N + set: X` fired from a context that itself declares
        // `clear_scopes` at the context level: the deeper popped frame's
        // meta_content_scope is partly on the live scope stack and partly
        // in `clear_stack` (stripped by cur's Clear on entry). Emitting the
        // compound Pop before Restore makes Pop eat atoms from below the
        // intended popped range, dropping the outer frame's meta_scope.
        // Shape mirrors Batch File's `cmd-set-quoted-value-inner-end`
        // (`clear_scopes: 1`) firing `pop: 2, set: ignored-tail-outer` below
        // a `cmd-set-quoted-value-inner` that carries a 2-atom
        // meta_content_scope.
        let syntax_str = r#"
name: PopNSetClear
scope: source.popnsetclear
version: 2
contexts:
  main:
    - match: 'a'
      scope: p.a
      push: [outer, middle]

  outer:
    - meta_scope: outer.test
    - match: 'z'
      pop: 1

  middle:
    - meta_content_scope: mid1.test mid2.test
    - match: 'b'
      scope: p.b
      push: top

  top:
    - clear_scopes: 1
    - match: 'c'
      scope: p.c
      pop: 2
      set: target

  target:
    - meta_scope: target.test
    - match: 'd'
      scope: p.d
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let raw_ops = ops(&mut state, "abcd", &ss);
        let states = stack_states(raw_ops);

        // The scope stack state recorded on the `d` token (in `target`):
        // must contain `outer.test` (outer's meta_scope still on the stack)
        // and `target.test` (target's meta_scope, pushed by the pop:2+set:)
        // and must NOT contain `mid1.test` or `mid2.test` (middle was popped
        // by pop:2 and its meta_content_scope atoms — one live, one in
        // clear_stack — should both be gone).
        let d_state = states
            .iter()
            .find(|s| s.contains("p.d"))
            .unwrap_or_else(|| panic!("expected a state containing `p.d`, got: {:?}", states));
        assert!(
            d_state.contains("outer.test"),
            "outer.test must survive pop:2+set: (it sits below the popped range): {}",
            d_state
        );
        assert!(
            d_state.contains("target.test"),
            "target.test must be on the stack (target was pushed by set:): {}",
            d_state
        );
        assert!(
            !d_state.contains("mid1.test") && !d_state.contains("mid2.test"),
            "middle's meta_content_scope atoms must not linger after pop:2+set:, \
             including the atom that was in clear_stack: {}",
            d_state
        );
    }

    #[test]
    fn pop_n_set_restores_deeper_frame_clear_scopes() {
        // `pop: N + set: [...]` (set_pop_count > 1) where one of the
        // popped DEEPER frames has `clear_scopes`: the deeper frame's
        // cleared atoms must be restored as part of the unwind, otherwise
        // they linger in `clear_stack` and the per-target Clear that
        // follows bites one atom too deep on the visible stack.
        //
        // Shape mirrors the Python `r'''(?ix:...)` triple-quoted-string
        // regex case: the regex embed pushes `base-literal-extended`
        // (a 1-atom meta_scope), then `(` matches `groups-extended` and
        // pushes `[group-body-extended_outer, maybe-unexpected-quantifiers,
        // group-start]`. `group-body-extended` has `clear_scopes: 1` (it
        // clears `base-literal-extended`'s ms atom) and a 2-atom meta_scope.
        // `(?ix:` then matches `group-start`'s `pop: 3 + set:
        // [group-body-extended_target, maybe-unexpected-quantifiers]` —
        // a multi-context set that unwinds the three pushed frames and
        // re-pushes [group-body-extended_target, maybe-unexpected-quantifiers].
        // Without the per-depth Restore in the unwind,
        // `group-body-extended_outer`'s cleared atom stayed in clear_stack
        // and the new target's `clear_scopes: 1` then cleared the
        // `source.regexp.python` mcs atom from below — leaking it from the
        // body content scope.
        let syntax_str = r#"
name: PopNSetDeeperClear
scope: source.popnsetdeepclear
version: 2
contexts:
  main:
    - match: 'a'
      scope: p.a
      push: outer

  outer:
    - meta_content_scope: keep-me.test
    - match: 'b'
      scope: p.b
      push: lit

  lit:
    - meta_scope: clear-me.test
    - match: 'c'
      scope: p.c
      push: [frame, inner]

  frame:
    - clear_scopes: 1
    - meta_scope: fra.test frb.test
    - match: 'z'
      pop: 1

  inner:
    - match: 'd'
      scope: p.d
      pop: 2
      set: [target, popper]

  target:
    - clear_scopes: 1
    - meta_scope: tgt.test
    - match: 'e'
      scope: p.e
      pop: 1

  popper:
    - match: ''
      pop: 1
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let raw_ops = ops(&mut state, "abcde", &ss);
        let states = stack_states(raw_ops);

        // The state recorded on the body token `e` (in `target`) must
        // contain `keep-me.test` (outer's mcs, which sits below the
        // popped frames) and `tgt.test` (target's ms). It must NOT
        // contain `fra.test`/`frb.test` (frame's ms — the popped frame)
        // or `clear-me.test` (cleared by frame on entry, restored by
        // the deeper-clear unwind, then cleared again by target's
        // own `clear_scopes: 1`).
        let e_state = states
            .iter()
            .find(|s| s.contains("p.e"))
            .unwrap_or_else(|| panic!("expected a state containing `p.e`, got: {:?}", states));
        assert!(
            e_state.contains("keep-me.test"),
            "keep-me.test must survive pop:3+set: with deeper clear_scopes \
             (without per-depth Restore, target's clear ate this atom): {}",
            e_state
        );
        assert!(
            e_state.contains("tgt.test"),
            "tgt.test must be on the stack (target was pushed by set:): {}",
            e_state
        );
        assert!(
            !e_state.contains("fra.test") && !e_state.contains("frb.test"),
            "frame's meta_scope must not linger after pop:3+set:: {}",
            e_state
        );
        assert!(
            !e_state.contains("clear-me.test"),
            "clear-me.test must be cleared by target's clear_scopes:1 \
             (it was first cleared by frame on entry, restored by the \
             deeper-clear unwind, then cleared again by target): {}",
            e_state
        );
    }

    #[test]
    fn pop_n_set_without_deeper_clear_scopes_unaffected() {
        // Same shape as the test above but with no `clear_scopes` on the
        // deeper popped frame. Verifies the head-pop split doesn't change
        // behavior when the per-depth Restore would be a no-op — defends
        // against regression of Java's `pop:2 + push: annotation-parameters-body`
        // and similar shapes where deeper frames have no clears.
        let syntax_str = r#"
name: PopNSetNoDeeperClear
scope: source.popnsetnodeepclear
version: 2
contexts:
  main:
    - match: 'a'
      scope: p.a
      push: outer

  outer:
    - meta_scope: outer.test
    - match: 'b'
      scope: p.b
      push: [frame, inner]

  frame:
    - meta_scope: fra.test frb.test
    - match: 'z'
      pop: 1

  inner:
    - match: 'c'
      scope: p.c
      pop: 2
      set: target

  target:
    - meta_scope: tgt.test
    - match: 'd'
      scope: p.d
      pop: 1
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let raw_ops = ops(&mut state, "abcd", &ss);
        let states = stack_states(raw_ops);

        let d_state = states
            .iter()
            .find(|s| s.contains("p.d"))
            .unwrap_or_else(|| panic!("expected a state containing `p.d`, got: {:?}", states));
        assert!(
            d_state.contains("outer.test"),
            "outer.test must survive pop:2+set: (sits below the popped range): {}",
            d_state
        );
        assert!(
            d_state.contains("tgt.test"),
            "tgt.test must be on the stack (target was pushed by set:): {}",
            d_state
        );
        assert!(
            !d_state.contains("fra.test") && !d_state.contains("frb.test"),
            "frame's meta_scope must not linger after pop:2+set:: {}",
            d_state
        );
    }

    #[test]
    fn cur_meta_scope_set_to_target_with_clear_scopes() {
        // Plain `set:` (no pop_count) from a context that itself carries
        // `clear_scopes` AND `meta_scope`, into a target that carries
        // `clear_scopes` AND `meta_content_scope`: the initial-phase Clear
        // for target ordinarily emitted by single-context-set previously
        // hid cur's meta_scope (which sits on top of the visible stack at
        // that point). The non-initial Pop then ate the wrong atom (parent's
        // last meta atom) and the trailing Restore resurrected cur.ms back
        // onto the stack instead of the parent atom the Clear was meant to
        // hide. The shape mirrors Bash's tilde-interpolation:
        //   maybe-tilde-interp  -> tilde-modifier (clear+ms)
        //   tilde-modifier (''-empty) -> tilde-modifier-username (clear+mcs)
        //   username pops on lookahead, leaving the parent intact.
        // After this fix, single-context-set defers the target Clear to the
        // non-initial phase whenever cur has ms/mcs, so Pop and Restore
        // operate on the correct atoms.
        //
        // Counterpart to `v2_set_to_target_with_clear_scopes_clears_parent_meta_content_scope`,
        // which exercises the cur-empty case (initial Clear stays as-is so
        // the trigger token sees the cleared stack).
        let syntax_str = r#"
name: CurMsSetTargetClear
scope: source.curmssettargetclear
version: 2
contexts:
  main:
    - match: 'p'
      scope: p.p
      push: parent

  parent:
    - meta_scope: parent1.test parent2.test
    - match: '~'
      scope: keyword.tilde
      set: cur

  cur:
    - clear_scopes: 1
    - meta_scope: cur.test
    - match: ''
      set: target

  target:
    - clear_scopes: 1
    - meta_content_scope: target.mcs1.test target.mcs2.test
    - match: '(?=/)'
      pop: 1
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        // "p~/" — `p` enters parent, `~` sets cur, `''` (zero-width) sets
        // target, `(?=/)` pops target leaving the parser back in `parent`.
        // The trailing literal char is `x` so we get a recorded token after
        // target has popped.
        let raw_ops = ops(&mut state, "p~/x\n", &ss);
        let states = stack_states(raw_ops);

        // Find the state covering `x` after the username has popped: parent
        // was never replaced (set was on parent's `~` rule, but that set: only
        // replaces parent's wrapper... actually parent IS replaced. After
        // pop from target and target's cur (set chain), nothing in `parent`
        // is on the stack — only `main`. So `x` is matched at `main`.)
        // What we really want to assert: between target's pop and any later
        // content, the visible stack must NOT contain `cur.test` — that's
        // the leak this fix targets.
        let leaked: Vec<_> = states.iter().filter(|s| s.contains("cur.test")).collect();
        // cur.test may legitimately appear during the `~` token (cur's
        // meta_scope applies to the trigger of the set). It must NOT appear
        // in any state recorded AFTER target was entered, because at that
        // point cur is gone from the stack.
        // Identify the cutoff: the first state that contains
        // `target.mcs1.test` marks the target-active region; from there
        // onward, `cur.test` must not appear.
        let target_first = states.iter().position(|s| s.contains("target.mcs1.test"));
        if let Some(idx) = target_first {
            for (i, s) in states.iter().enumerate().skip(idx) {
                assert!(
                    !s.contains("cur.test"),
                    "cur.test must not linger from index {} onward (target entered at {}): {}",
                    i,
                    idx,
                    s
                );
            }
        } else {
            // If target's mcs never landed on any recorded state, the test
            // can't pin the lifetime — fall back to the simpler invariant.
            assert!(
                leaked.is_empty(),
                "cur.test leaked into recorded states after the SET chain: {:?}",
                leaked
            );
        }
    }

    #[test]
    fn php_multi_set_target_clear_drops_extra_parent_mcs_on_trigger() {
        // Multi-context `set:` whose target body has `clear_scopes: 1` AND
        // a non-empty `meta_scope`, fired from a cur with no ms/mcs/clear.
        // ST drops the immediate parent's mcs atom (Clear(1)) AND one
        // EXTRA atom (the next-deeper mcs) on the trigger token; the body
        // content sees only Clear(1) atoms gone, so the extra atom is
        // restored. Reduced from PHP `function bye(): never {`: cur is
        // `function-return-type`; target is
        // `[function-return-type-body, type-hint-simple-type]` with
        // `function-return-type-body` declaring `clear_scopes: 1` and
        // `meta_scope: meta.function.return-type.php`; the `:` sits below
        // `function-block`'s `meta_content_scope: meta.function.php` and
        // the embed wrapper's `source.php.embedded.html`, both of which
        // ST drops on the colon and only `source.php.embedded.html` is
        // restored for the body.
        let syntax_str = r#"
name: PhpMultiSetClear
scope: source.phpmultisetclear
version: 2
contexts:
  main:
    - match: '(?=\S)'
      push: outer

  outer:
    - meta_content_scope: outer.mcs.test
    - match: 'F'
      scope: keyword.f.test
      push: parent

  parent:
    - meta_content_scope: parent.mcs.test
    - match: '\('
      scope: parent.lparen.test
      push: cur

  cur:
    - match: ':'
      scope: trigger.colon.test
      set: [body, helper]
    - match: '(?=\S)'
      pop: 1

  body:
    - clear_scopes: 1
    - meta_scope: body.ms.test
    - match: '\w+'
      scope: body.word.test
      pop: 1

  helper:
    - match: '(?=\S)'
      pop: 1
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        // `F(:foo`: F enters parent via outer, `(` enters cur via parent,
        // `:` sets [body, helper], helper else-pops, body matches "foo".
        let raw_ops = ops(&mut state, "F(:foo\n", &ss);
        let states = stack_states(raw_ops);

        let colon_state = states
            .iter()
            .find(|s| s.contains("trigger.colon.test"))
            .unwrap_or_else(|| {
                panic!(
                    "expected a state containing trigger.colon.test, got: {:?}",
                    states
                )
            });
        assert!(
            !colon_state.contains("parent.mcs.test"),
            "trigger must drop parent.mcs (Clear(1) target): {}",
            colon_state
        );
        assert!(
            !colon_state.contains("outer.mcs.test"),
            "trigger must drop outer.mcs (the EXTRA atom anchored by body.ms): {}",
            colon_state
        );
        assert!(
            colon_state.contains("body.ms.test"),
            "trigger must carry body.ms (target's meta_scope): {}",
            colon_state
        );

        let body_state = states
            .iter()
            .find(|s| s.contains("body.word.test"))
            .unwrap_or_else(|| {
                panic!(
                    "expected a state containing body.word.test, got: {:?}",
                    states
                )
            });
        assert!(
            !body_state.contains("parent.mcs.test"),
            "body must drop parent.mcs (Clear(1) target): {}",
            body_state
        );
        assert!(
            body_state.contains("outer.mcs.test"),
            "body must keep outer.mcs (the extra-drop is trigger-only): {}",
            body_state
        );
        assert!(
            body_state.contains("body.ms.test"),
            "body must carry body.ms: {}",
            body_state
        );
    }

    #[test]
    fn multi_set_target_clear_with_target_mcs_only_does_not_extra_drop() {
        // Companion to `php_multi_set_target_clear_drops_extra_parent_mcs_on_trigger`:
        // the same shape but the clear-bearing target has only
        // `meta_content_scope` (no `meta_scope`). ST does NOT drop any
        // extra atom on the trigger here; the trigger keeps both parent
        // mcs atoms. Real-world: Zsh's
        // `zsh-redirection-glob-range-end` (`clear_scopes: 1` +
        // `meta_content_scope: meta.range.shell.zsh`, no meta_scope) at
        // the head of a 5-context set from `zsh-redirection-glob-range-begin`.
        // Without this gate, the fix above strips
        // `meta.function-call.arguments.shell` and even
        // `source.shell.zsh` on the `<` trigger.
        let syntax_str = r#"
name: ZshLikeMultiSetTargetMcsOnly
scope: source.zshlikemulti
version: 2
contexts:
  main:
    - match: '(?=\S)'
      push: outer

  outer:
    - meta_content_scope: outer.mcs.test
    - match: 'F'
      scope: keyword.f.test
      push: parent

  parent:
    - meta_content_scope: parent.mcs.test
    - match: '\('
      scope: parent.lparen.test
      push: cur

  cur:
    - match: ':'
      scope: trigger.colon.test
      set: [body, helper]
    - match: '(?=\S)'
      pop: 1

  body:
    - clear_scopes: 1
    - meta_content_scope: body.mcs.test
    - match: '\w+'
      scope: body.word.test
      pop: 1

  helper:
    - match: '(?=\S)'
      pop: 1
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let raw_ops = ops(&mut state, "F(:foo\n", &ss);
        let states = stack_states(raw_ops);

        let colon_state = states
            .iter()
            .find(|s| s.contains("trigger.colon.test"))
            .unwrap_or_else(|| {
                panic!(
                    "expected a state containing trigger.colon.test, got: {:?}",
                    states
                )
            });
        // No extra-drop must apply: both parent atoms must remain on the
        // trigger token. The Clear(1) for body's clear_scopes is
        // post-match, so even parent.mcs is still on the trigger.
        assert!(
            colon_state.contains("parent.mcs.test"),
            "trigger must keep parent.mcs (target has no meta_scope, no extra-drop): {}",
            colon_state
        );
        assert!(
            colon_state.contains("outer.mcs.test"),
            "trigger must keep outer.mcs (target has no meta_scope, no extra-drop): {}",
            colon_state
        );
    }

    #[test]
    fn v2_set_clear_scopes_applies_from_every_context() {
        // v2: when `set:` lists multiple contexts, `clear_scopes` on any of
        // them — not just the topmost — applies at that context's own
        // position in the stack. The canonical real-world case is Bash's
        //   set: [def-function-body, def-function-params, def-function-name]
        // where `def-function-params` (the middle context) carries
        // `clear_scopes: 1`. The Clear strips the atom that the preceding
        // context's meta_content_scope just pushed, matching Sublime
        // Text's observed behaviour. Previously this test guessed
        // Sublime pinned Clear to the topmost context only; running real
        // v2 syntaxes (Bash function definitions, among others) refuted
        // that guess.
        let syntax_str = r#"
name: V2ClearMid
scope: source.v2clear
version: 2
contexts:
  main:
    - meta_scope: meta.main.v2clear
    - match: 'GO'
      set: [ctx-bottom, ctx-middle, ctx-top]
  ctx-bottom:
    - meta_content_scope: mcs.bottom.v2clear
  ctx-middle:
    - clear_scopes: 1
    - meta_content_scope: mcs.middle.v2clear
  ctx-top:
    - meta_content_scope: mcs.top.v2clear
    - match: '\w+'
      scope: word.top.v2clear
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let raw_ops = ops(&mut state, "GO hello\n", &ss);
        let states = stack_states(raw_ops);

        // "hello" matches in ctx-top. At that point ctx-middle's
        // clear_scopes: 1 must have stripped ctx-bottom's
        // meta_content_scope atom.
        let hello_states: Vec<_> = states
            .iter()
            .filter(|s| s.contains("word.top.v2clear"))
            .collect();
        assert!(
            !hello_states.is_empty(),
            "expected word.top.v2clear, got states: {:?}",
            states
        );
        for s in &hello_states {
            assert!(
                !s.contains("mcs.bottom.v2clear"),
                "ctx-bottom's mcs should have been cleared by ctx-middle's \
                 clear_scopes: 1 before ctx-top's match, got: {:?}",
                s
            );
            assert!(
                s.contains("mcs.middle.v2clear"),
                "ctx-middle's mcs should be on the stack during ctx-top's match, \
                 got: {:?}",
                s
            );
        }

        // Reaching this point without a panic proves the Restore emitted on
        // ctx-middle's pop didn't underflow the clear_stack — the
        // regression the Bash `func () {}` minimal reproducer uncovered.
    }

    #[test]
    fn v2_multi_set_non_topmost_clear_scopes_strips_preceding_meta_scope_at_trigger() {
        // v2: in a multi-context `set:` whose non-topmost target declares
        // `clear_scopes: N` + a non-empty `meta_content_scope` (and an
        // empty `meta_scope`), the Clear must strip atoms that EARLIER
        // contexts in the set list pushed via their `meta_scope` — and
        // the strip must be visible to the TRIGGER match's own scopes
        // (top-level `scope:` and capture scopes).
        //
        // Real-world repro: Zsh's `zsh-redirection-glob-range-begin` is
        // entered via a pop+branch from `redirection-input`. Its match
        // `(\d*)(<)` runs `set: [string-path-pattern-body,
        // zsh-redirection-glob-range-end, zsh-glob-range-number,
        // zsh-redirection-glob-range-operator, zsh-glob-range-number]`.
        // `string-path-pattern-body` has
        // `meta_scope: meta.string.glob.shell string.unquoted.shell`, and
        // `zsh-redirection-glob-range-end` has
        // `clear_scopes: 1` + `meta_content_scope: meta.range.shell.zsh`.
        // The capture-2 scope `meta.range.shell.zsh
        // punctuation.definition.range.begin.shell.zsh` is asserted with
        // `- string` exclusion, so `string.unquoted.shell` must be hidden
        // at the `<` token.
        let syntax_str = r#"
name: V2MultiSetNonTopClear
scope: source.v2mscstrigger
version: 2
contexts:
  main:
    - match: '\['
      scope: punctuation.section.brackets.begin
      set: middle
  middle:
    - meta_include_prototype: false
    - match: '(<)'
      captures:
        1: meta.range.begin punctuation.definition.range.begin
      set:
        - body
        - end
        - top
  body:
    - meta_include_prototype: false
    - meta_scope: meta.glob string.unquoted
    - match: '\]'
      scope: punctuation.section.brackets.end
      pop: true
  end:
    - clear_scopes: 1
    - meta_include_prototype: false
    - meta_content_scope: meta.range
    - match: '>'
      scope: punctuation.definition.range.end
      pop: 1
  top:
    - meta_include_prototype: false
    - match: '\d+'
      scope: constant.numeric
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let raw_ops = ops(&mut state, "[<1>x]\n", &ss);
        let states = stack_states(raw_ops);

        // The `<` token (capture 1) must carry meta.glob + meta.range.begin
        // + punctuation.definition.range.begin, with `string.unquoted`
        // CLEARED by end's `clear_scopes: 1`.
        let begin_states: Vec<_> = states
            .iter()
            .filter(|s| s.contains("punctuation.definition.range.begin"))
            .collect();
        assert!(
            !begin_states.is_empty(),
            "expected punctuation.definition.range.begin in some state, got: {:?}",
            states
        );
        for s in &begin_states {
            assert!(
                s.contains("meta.glob"),
                "meta.glob should remain on the stack at the `<` token, got: {:?}",
                s
            );
            assert!(
                !s.contains("string.unquoted"),
                "string.unquoted should have been cleared by end's \
                 `clear_scopes: 1` on entry, got: {:?}",
                s
            );
            assert!(
                s.contains("meta.range.begin"),
                "meta.range.begin (capture scope) must be on the stack at the \
                 `<` token, got: {:?}",
                s
            );
        }

        // After the trigger, body content (`1`) sees end's
        // meta_content_scope (meta.range) and NOT string.unquoted.
        let digit_states: Vec<_> = states
            .iter()
            .filter(|s| s.contains("constant.numeric"))
            .collect();
        assert!(
            !digit_states.is_empty(),
            "expected constant.numeric in some state, got: {:?}",
            states
        );
        for s in &digit_states {
            assert!(
                !s.contains("string.unquoted"),
                "string.unquoted must stay cleared while inside the range body, \
                 got: {:?}",
                s
            );
            assert!(
                s.contains("meta.range"),
                "end's meta_content_scope (meta.range) must be on the body \
                 content's stack, got: {:?}",
                s
            );
        }
    }

    #[test]
    fn v2_embed_scope_replaces_skips_meta_content_pop_on_exit() {
        // Kills: L912 replace >= with < (version >= 2)
        //        L913 replace >= with < (stack.len() >= 2)
        //        L915 replace - with / (stack.len() - 2)
        // When a v2 syntax uses embed with embed_scope, the escape context
        // has embed_scope_replaces=true.  On pop, the meta_content_scope of
        // the escape context should NOT be popped because it was never pushed.
        // If the version check is wrong, we'd get an extra Pop.
        use crate::parsing::ScopeStack;

        // The host pushes two intermediate contexts before embedding so that
        // the stack depth is 5 when the escape fires:
        //   [main, wrapper-a, wrapper-b, escape, embedded]
        // This distinguishes stack.len()-2 (=3, escape) from stack.len()/2
        // (=2, wrapper-b), catching the L915 `-` → `/` mutation.
        let host = SyntaxDefinition::load_from_str(
            r#"
name: V2SkipHost
scope: source.v2skip
file_extensions: [v2skip]
version: 2
contexts:
  main:
    - match: '(?=<)'
      push: wrapper-a
    - match: '\w+'
      scope: word.v2skip
  wrapper-a:
    - match: '(?=<)'
      push: wrapper-b
  wrapper-b:
    - match: '<<'
      embed: scope:source.v2skipemb
      embed_scope: meta.embedded.v2skip
      escape: '>>'
      escape_captures:
        0: punctuation.end.v2skip
"#,
            true,
            None,
        )
        .unwrap();

        let embedded = SyntaxDefinition::load_from_str(
            r#"
name: V2SkipEmb
scope: source.v2skipemb
file_extensions: [v2skipemb]
version: 2
contexts:
  main:
    - meta_content_scope: content.v2skipemb
    - match: '\w+'
      scope: keyword.v2skipemb
"#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(host);
        builder.add(embedded);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("V2SkipHost").unwrap();
        let mut state = ParseState::new(syntax);
        let raw_ops = state.parse_line("<<x>> hello\n", &ss).unwrap().ops;

        // Build scope stack through all ops and verify it ends clean.
        // If the skip logic is broken (mutations on L912-L915), an extra Pop
        // for meta_content_scope is generated, which pops a scope that was
        // never pushed, corrupting the stack.
        let mut scope_stack = ScopeStack::new();
        for (_, op) in &raw_ops {
            scope_stack
                .apply(op)
                .expect("applying op should not fail — extra Pop means the skip logic is broken");
        }
        // After ">> hello\n", we should be back in main with source.v2skip
        // as the only remaining scope (everything else was popped).
        // If the skip logic is wrong, source.v2skip would be popped too.
        let final_scopes: Vec<String> = scope_stack
            .as_slice()
            .iter()
            .map(|s| format!("{:?}", s))
            .collect();
        assert!(
            final_scopes.iter().any(|s| s.contains("source.v2skip")),
            "source.v2skip should remain on stack after all ops, got: {:?}",
            final_scopes
        );
    }

    #[test]
    fn v2_host_embedding_v1_guest_skips_meta_content_pop_on_escape() {
        // Regression for the Rails html.erb syntest cluster: when a v2 host
        // uses `embed:` + `embed_scope:` to pull in a v1 guest grammar (e.g.
        // Rails/HTML embedding Ruby), `embed_scope_replaces` is set on the
        // wrapper context. On escape, the embedded guest's meta_content_scope
        // must be skipped — it was never pushed on the way in.
        //
        // The exec_escape skip logic was gated on
        // `current_syntax_version() >= 2`, which reads the version from the
        // top-of-stack context. That is the *guest* (Ruby, v1), not the host,
        // so the gate evaluated false and a spurious Pop fired for a scope
        // that was never pushed, misaligning every scope on the stack for
        // the remainder of the host context.
        use crate::parsing::ScopeStack;

        let host = SyntaxDefinition::load_from_str(
            r#"
name: V2HostV1Guest
scope: source.v2host
file_extensions: [v2host]
version: 2
contexts:
  main:
    - match: '<<'
      embed: scope:source.v1guest
      embed_scope: meta.embedded.v2host
      escape: '>>'
      escape_captures:
        0: punctuation.end.v2host
    - match: '\w+'
      scope: word.v2host
"#,
            true,
            None,
        )
        .unwrap();

        // Guest omits `version:` — defaults to 1. Its `scope:` lands in the
        // main context's meta_content_scope (source.v1guest), which the
        // v2 embed_scope_replaces suppresses on push. The escape must
        // symmetrically suppress it on pop.
        let guest = SyntaxDefinition::load_from_str(
            r#"
name: V1Guest
scope: source.v1guest
file_extensions: [v1guest]
contexts:
  main:
    - match: '\w+'
      scope: keyword.v1guest
"#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(host);
        builder.add(guest);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("V2HostV1Guest").unwrap();
        let mut state = ParseState::new(syntax);
        let raw_ops = state.parse_line("<<x>> hello\n", &ss).unwrap().ops;

        // Before the fix, the escape emits a Pop for guest main's mcs even
        // though it was never pushed. Subsequent Pops then strip scopes
        // that should have survived. Applying the op stream must not fail,
        // and source.v2host must remain on the stack at the end.
        let mut scope_stack = ScopeStack::new();
        for (_, op) in &raw_ops {
            scope_stack.apply(op).expect(
                "applying op stream must succeed — a spurious Pop indicates the skip was gated \
                 on the guest's syntax version instead of the embed_scope_replaces flag",
            );
        }
        let final_scopes: Vec<String> = scope_stack
            .as_slice()
            .iter()
            .map(|s| format!("{:?}", s))
            .collect();
        assert!(
            final_scopes.iter().any(|s| s.contains("source.v2host")),
            "source.v2host should remain after escape; got: {:?}",
            final_scopes
        );
    }

    #[test]
    fn nested_embed_outer_escape_wins() {
        // Inner embed's escape must not fire before outer embed's escape.
        // The outer escape at position 3 ("END") should take precedence over
        // the inner escape at position 5 ("zzz"), truncating the search region.
        let syntax = r#"
name: NestedEmbed
scope: source.nested-embed
contexts:
  main:
    - match: 'OUTER'
      embed: mid
      escape: 'END'
      escape_captures:
        0: keyword.escape.outer
    - match: '.'
      scope: main.char

  mid:
    - match: 'INNER'
      embed: deep
      escape: 'zzz'
      escape_captures:
        0: keyword.escape.inner
    - match: '.'
      scope: mid.char

  deep:
    - match: '.'
      scope: deep.char
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // Line 1: enter outer embed, then inner embed
        let out1 = state.parse_line("OUTERINNER\n", &ss).expect("line 1");
        debug_print_ops("OUTERINNER\n", &out1.ops);

        // Line 2: "xxENDzzzAFTER" — outer escape "END" at pos 2 must fire before
        // inner escape "zzz" at pos 5. After outer escape fires, we're back in main.
        let out2 = state.parse_line("xxENDzzzAFTER\n", &ss).expect("line 2");
        let states = stack_states(out2.ops);
        println!("states: {:?}", states);

        // The outer escape scope must appear
        assert!(
            states.iter().any(|s| s.contains("keyword.escape.outer")),
            "outer escape must fire, got: {:?}",
            states
        );
        // The inner escape scope must NOT appear (outer wins)
        assert!(
            !states.iter().any(|s| s.contains("keyword.escape.inner")),
            "inner escape must not fire when outer escape is earlier, got: {:?}",
            states
        );
        // After the outer escape, "zzzAFTER" should be parsed in main context
        assert!(
            states.iter().any(|s| s.contains("main.char")),
            "after outer escape we should be in main, got: {:?}",
            states
        );
    }

    #[test]
    fn embed_escape_with_backref_at_parse_time() {
        // The escape pattern uses \1 to backreference the opening delimiter.
        // Verify that the resolved regex correctly matches at parse time.
        let syntax = r#"
name: BackrefEscape
scope: source.backref-escape
contexts:
  main:
    - match: '(<<|>>)'
      scope: punctuation.open
      embed: inner
      escape: '\1'
      escape_captures:
        0: punctuation.close
    - match: '.'
      scope: main.char

  inner:
    - match: '.'
      scope: inner.char
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
        let ss = link(syntax);

        // Test 1: "<<" opens, ">>" should NOT close it, "<<" should close it
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let out1 = state.parse_line("<<>>stuff<<after\n", &ss).expect("line 1");
        let states = stack_states(out1.ops);
        println!("backref states: {:?}", states);

        // The opening << should push punctuation.open
        assert!(
            states.iter().any(|s| s.contains("punctuation.open")),
            "expected punctuation.open, got: {:?}",
            states
        );
        // ">>" should be parsed as inner.char (not as escape)
        assert!(
            states.iter().any(|s| s.contains("inner.char")),
            ">> should be inner.char since escape is <<, got: {:?}",
            states
        );
        // "<<" at pos 9 should fire as escape (punctuation.close)
        assert!(
            states.iter().any(|s| s.contains("punctuation.close")),
            "matching << should trigger escape, got: {:?}",
            states
        );
        // After escape, "after" should be in main
        assert!(
            states.iter().any(|s| s.contains("main.char")),
            "after escape we should be in main, got: {:?}",
            states
        );
    }

    #[test]
    fn embed_escape_cross_line() {
        // Embed on line 1, content on line 2, escape on line 3.
        // Verifies that escape_stack persists across parse_line calls.
        let syntax = r#"
name: CrossLineEscape
scope: source.cross-line-escape
contexts:
  main:
    - match: 'BEGIN'
      scope: keyword.begin
      embed: body
      escape: 'STOP'
      escape_captures:
        0: keyword.stop
    - match: '.'
      scope: main.char

  body:
    - match: '.'
      scope: body.char
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // Line 1: embed begins
        let out1 = state.parse_line("BEGIN\n", &ss).expect("line 1");
        let states1 = stack_states(out1.ops);
        assert!(
            states1.iter().any(|s| s.contains("keyword.begin")),
            "line 1 should have keyword.begin, got: {:?}",
            states1
        );

        // Line 2: content inside the embed
        let out2 = state.parse_line("hello\n", &ss).expect("line 2");
        let states2 = stack_states(out2.ops);
        assert!(
            states2.iter().any(|s| s.contains("body.char")),
            "line 2 should be body content, got: {:?}",
            states2
        );

        // Line 3: escape fires
        let out3 = state.parse_line("STOPafter\n", &ss).expect("line 3");
        let states3 = stack_states(out3.ops);
        assert!(
            states3.iter().any(|s| s.contains("keyword.stop")),
            "line 3 should have escape keyword.stop, got: {:?}",
            states3
        );
        assert!(
            states3.iter().any(|s| s.contains("main.char")),
            "after escape on line 3, should be in main, got: {:?}",
            states3
        );
    }

    #[test]
    fn embed_inside_branch_then_fail_restores_escape_stack() {
        // An embed inside a branch alternative pushes to escape_stack.
        // When fail fires, the escape_stack must be restored (the embed's
        // escape entry must be removed). The fallback alternative stays on
        // the stack (no pop) so any stale escape entry would survive to
        // the next line, where it would incorrectly fire.
        let syntax = r#"
name: EmbedBranchFail
scope: source.embed-branch
contexts:
  main:
    - match: 'START'
      branch_point: bp
      branch: [try-embed, fallback]
    - match: '.'
      scope: main.char

  try-embed:
    - match: 'EMB'
      embed: embedded
      escape: 'ESC'
      escape_captures:
        0: keyword.escape

  fallback:
    # No pop — stays on the stack so a stale escape entry would persist
    - match: '\w+'
      scope: fallback.matched
    - match: '\n'

  embedded:
    - match: 'FAIL'
      fail: bp
    - match: '.'
      scope: embedded.char
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // "START" triggers branch, tries try-embed first.
        // "EMB" enters the embed (pushing escape entry for "ESC").
        // "FAIL" fires `fail: bp`, which must restore escape_stack and
        // replay under fallback alternative.
        let out = state
            .parse_line("STARTEMBFAIL\n", &ss)
            .expect("parse failed");
        let states = stack_states(out.ops);
        println!("embed+branch+fail states: {:?}", states);

        // After backtracking, fallback should match
        assert!(
            states.iter().any(|s| s.contains("fallback.matched")),
            "fallback should match after fail, got: {:?}",
            states
        );

        // Parse another line — "ESC" must NOT trigger the escape (it was
        // from a reverted branch). Without proper escape_stack restoration,
        // the stale escape entry would fire here.
        let out2 = state.parse_line("xESCy\n", &ss).expect("line 2");
        let states2 = stack_states(out2.ops);
        assert!(
            !states2.iter().any(|s| s.contains("keyword.escape")),
            "stale escape must not fire after branch revert, got: {:?}",
            states2
        );
    }

    #[test]
    fn erb_escape_captures() {
        let ss = SyntaxSet::load_defaults_newlines();
        let syntax = ss.find_syntax_by_extension("erb").unwrap();
        let mut state = ParseState::new(syntax);
        let mut scope_stack = ScopeStack::new();
        let ops = state.parse_line("<%= puts \"hi\" %>\n", &ss).unwrap();
        eprintln!("ERB line ops:");
        for (pos, op) in &ops.ops {
            scope_stack.apply(op).ok();
            eprintln!(
                "  pos={} op={:?}  stack={:?}",
                pos,
                op,
                scope_stack.as_slice()
            );
        }
        let stack_str = format!("{:?}", scope_stack.as_slice());
        // After the line, the %> should have fired the escape
        // and we should be back in HTML context
        assert!(
            !stack_str.contains("source.ruby"),
            "Expected Ruby embed to have ended, got: {}",
            stack_str
        );
    }

    #[test]
    fn embed_js_in_html() {
        let ss = SyntaxSet::load_defaults_newlines();

        for ext in &["html", "erb"] {
            let syntax = ss.find_syntax_by_extension(ext).unwrap();
            let mut state = ParseState::new(syntax);
            let mut scope_stack = ScopeStack::new();
            state
                .parse_line("<script type=\"text/javascript\">\n", &ss)
                .unwrap()
                .ops
                .iter()
                .for_each(|(_, op)| {
                    scope_stack.apply(op).ok();
                });
            let ops = state.parse_line("var x = 5;\n", &ss).unwrap();
            for (_, op) in &ops.ops {
                scope_stack.apply(op).ok();
            }
            let stack_str = format!("{:?}", scope_stack.as_slice());
            assert!(
                stack_str.contains("source.js"),
                "Extension {}: expected source.js in scope stack, got: {}",
                ext,
                stack_str
            );
        }
    }

    #[test]
    fn v2_pop_embed_suppresses_cur_meta_scope_on_match() {
        // `pop: N + embed:` trigger text must NOT carry the popped context's
        // `meta_scope` through, unlike `pop: N + set:` which preserves both
        // cur's and target's meta_scope on the match. Probe against ST confirms:
        //
        //   <tag>hi</tag>            (pop+embed)
        //   col 4 '>'                -> ['source.host', 'end.scope']            (cur ms gone)
        //   col 5 'h' (body)         -> ['source.host', 'embed.scope', 'guest.meta']
        //
        //   <tag>after               (pop+set — contrast)
        //   col 4 '>'                -> ['source.host', 'meta.a', 'after.meta', 'end.scope']
        //
        // Without this guard, syntect emitted
        //   [source.host, meta.a, meta.a, end.scope]
        // because the rule's explicit scope atom shadowed cur.meta_scope onto
        // itself on the trigger text — observed as 5 duplicated
        // `meta.tag.jsp.*.begin.html` atoms on `<jsp:declaration>`/ expression/
        // scriptlet's `>` in `syntax_test_jsp.jsp`.
        let host = SyntaxDefinition::load_from_str(
            r#"
name: PopEmbedHost
scope: source.popembed
file_extensions: [popembed]
version: 2
contexts:
  main:
    - match: '<tag'
      scope: begin.scope
      push: tag-attrs
  tag-attrs:
    - meta_include_prototype: false
    - meta_scope: meta.a
    - match: '>'
      scope: meta.a end.scope
      pop: 1
      embed: scope:source.popembedguest
      embed_scope: embed.scope
      escape: '(?=</tag)'
"#,
            true,
            None,
        )
        .unwrap();
        let guest = SyntaxDefinition::load_from_str(
            r#"
name: PopEmbedGuest
scope: source.popembedguest
version: 2
hidden: true
contexts:
  main:
    - meta_scope: guest.meta
    - match: '\w+'
      scope: word.guest
"#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(host);
        builder.add(guest);
        let ss = builder.build();
        let syntax = ss.find_syntax_by_name("PopEmbedHost").unwrap();
        let mut state = ParseState::new(syntax);
        let ops = state.parse_line("<tag>hi</tag>\n", &ss).unwrap().ops;

        // Walk (range, op) pairs; after applying each op, snapshot the stack
        // keyed by the character position we're at. The `>` match occupies
        // col 4, so we expect the post-op snapshot at that position to have
        // exactly ONE `meta.a` atom, not two.
        use crate::easy::ScopeRangeIterator;
        let line = "<tag>hi</tag>\n";
        let mut stack = ScopeStack::new();
        let mut at_gt: Option<Vec<String>> = None;
        for (range, op) in ScopeRangeIterator::new(&ops, line) {
            stack.apply(op).expect("op stream must apply cleanly");
            // Capture the stack state for the character range covering the `>`
            // trigger (col 4..5, the match text of the pop+embed rule).
            if range.start <= 4 && 4 < range.end {
                at_gt = Some(
                    stack
                        .as_slice()
                        .iter()
                        .map(|s| format!("{:?}", s))
                        .collect(),
                );
            }
        }
        let at_gt = at_gt.expect("range covering `>` must exist in op stream");
        let meta_a_count = at_gt.iter().filter(|s| s.contains("meta.a")).count();
        assert_eq!(
            meta_a_count, 1,
            "match text of pop+embed must carry exactly one `meta.a` atom \
             (from the rule's explicit scope); cur_context.meta_scope must \
             not stack a second copy on top. Got stack: {:?}",
            at_gt
        );
        // And `end.scope` must be the top of the stack (the match's second
        // explicit atom) — if the ordering shifted we'd see a different trailer.
        assert!(
            at_gt
                .last()
                .map(|s| s.contains("end.scope"))
                .unwrap_or(false),
            "stack top on `>` must be `end.scope`, got: {:?}",
            at_gt
        );
    }

    #[test]
    fn cross_line_multi_fail_deduplicates_flushed_ops() {
        // Two nested branch_points created on line 1 that both fail on a
        // later line exercise `handle_fail`'s cross-line path twice on a
        // single `parse_line` call. Before dedup, each fail `extend`ed
        // `flushed_ops` with its own replay, so `ParseLineOutput::replayed`
        // ended up ~2× the pending-lines count — and the consumer (see
        // `examples/syntest.rs`) paired `replayed[i]` with
        // `parsed_line_buffer[buf_len - replayed.len() + i]`, sliding ops
        // from one buffered line onto another's text. That panicked in
        // `ScopeRegionIterator::next` as "byte index N out of bounds" —
        // observed originally at `syntax_test_java.java` line 624.
        let syntax_str = r#"
name: DedupCrossLine
scope: source.dup
contexts:
  main:
    - match: 'A'
      branch_point: bp1
      branch: [a1, a2]
  a1:
    - match: 'B'
      branch_point: bp2
      branch: [b1, b2]
    - match: '(?=FAIL)'
      fail: bp1
  a2:
    - match: '.*'
      scope: a2.fallback
      pop: true
  b1:
    - match: '\n'
    - match: '(?=FAIL)'
      fail: bp2
    - match: 'XYZ'
      pop: true
  b2:
    - match: '\n'
    - match: '(?=FAIL)'
      fail: bp2
    - match: 'XYZ'
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        let out1 = state.parse_line("AB\n", &ss).expect("line 1");
        assert!(out1.replayed.is_empty());

        let out2 = state.parse_line("FOO\n", &ss).expect("line 2");
        assert!(out2.replayed.is_empty());

        // Line 3 fires `fail: bp2` twice (once for alt[1], once to exhaust)
        // and then `fail: bp1` — three cross-line fails back-to-back.
        let out3 = state.parse_line("FAIL\n", &ss).expect("line 3");

        // Invariant: one replayed entry per buffered pending line (2), not
        // `number_of_fails × pending_lines`.
        assert_eq!(
            out3.replayed.len(),
            2,
            "expected exactly 2 replayed lines (one per buffered pending line), got {}: {:?}",
            out3.replayed.len(),
            out3.replayed,
        );

        // Panic guard: each `replayed[i]`'s byte offsets must fit within the
        // corresponding buffered line's length. The original misalignment
        // paired line 617's ops (77 bytes) with line 609's text (59 bytes).
        let line_lens = ["AB\n".len(), "FOO\n".len()];
        for (i, line_ops) in out3.replayed.iter().enumerate() {
            for (pos, op) in line_ops {
                assert!(
                    *pos <= line_lens[i],
                    "replayed[{}] op past EOL: pos={} line_len={} op={:?}",
                    i,
                    pos,
                    line_lens[i],
                    op,
                );
            }
        }
    }

    #[test]
    fn replay_born_branch_routes_as_cross_line_on_later_fail() {
        // A branch created while `handle_fail` is re-parsing a past buffered
        // line must record the *replay line's* number, not the outer
        // `parse_line`'s current line. Otherwise `handle_fail`'s later
        // `is_cross_line = bp.line_number < cur_line` sees equal values on
        // the second fail, routes into the same-line path, and applies
        // `bp.match_start` (a byte offset into the long replay line) to a
        // shorter outer line. That shipped as the `byte index N out of
        // bounds` panic on `syntax_test_java.java:10263` (`  foo = BAR,\n`)
        // and on `syntax_test_markdown.md` under multi-line math blocks.
        let syntax_str = r#"
name: ReplayBornBranch
scope: source.rbb
contexts:
  main:
    - match: 'A'
      branch_point: bp1
      branch: [a1, a2]
  a1:
    - match: '(?=FAIL1)'
      fail: bp1
    - match: '.'
  a2:
    - match: 'B'
      branch_point: bp2
      branch: [b1, b2]
    - match: '.'
  b1:
    - match: '(?=FAIL2)'
      fail: bp2
    - match: '.'
  b2:
    - match: '.*'
      scope: b2.fallback
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // Long line 1 so `B` sits past the short outer line's length; bp2's
        // replay-relative `match_start` would OOB a same-line rewind there.
        let line1 = "A pad pad pad pad pad pad pad pad pad pad B tail\n";
        assert!(line1.find('B').unwrap() > "FAIL2\n".len());

        let out1 = state.parse_line(line1, &ss).expect("line 1 parses");
        assert!(out1.replayed.is_empty());

        // First cross-line fail: swap bp1 → a2. During a2's replay of line 1,
        // `B` fires bp2 and records (with the fix) `line_number = 0` and
        // `pending_lines_snapshot_len = 0` — anchored to line 1, not line 2.
        let out2 = state.parse_line("FAIL1\n", &ss).expect("line 2 parses");
        assert_eq!(out2.replayed.len(), 1, "bp1 replay covers line 1");

        // Second cross-line fail: bp2 must be classified cross-line on this
        // outer line. With the fix it is (line 0 < line 2), so the handler
        // takes the replay path and re-parses past buffered lines under b2.
        // Without the fix bp2 appears same-line (line 2 == line 2) and the
        // handler applies `match_start` = offset-of-B-in-line1 to the
        // 6-byte outer line, corrupting ops / panicking downstream.
        let outer = "FAIL2\n";
        let out3 = state.parse_line(outer, &ss).expect("line 3 parses");

        // Cross-line classification fired a second replay covering the
        // two buffered lines (line 1 + line 2).
        assert_eq!(
            out3.replayed.len(),
            2,
            "expected replay from bp2's cross-line fail to cover both buffered lines, got {}: {:?}",
            out3.replayed.len(),
            out3.replayed,
        );

        // Panic guard: every op offset in both `ops` and `replayed` must
        // fit within its paired line's byte length.
        for (pos, op) in &out3.ops {
            assert!(
                *pos <= outer.len(),
                "outer op past EOL: pos={} len={} op={:?}",
                pos,
                outer.len(),
                op,
            );
        }
        let replay_lines = [line1, "FAIL1\n"];
        for (i, line_ops) in out3.replayed.iter().enumerate() {
            for (pos, op) in line_ops {
                assert!(
                    *pos <= replay_lines[i].len(),
                    "replayed[{}] op past EOL: pos={} len={} op={:?}",
                    i,
                    pos,
                    replay_lines[i].len(),
                    op,
                );
            }
        }
    }

    #[test]
    fn replay_born_branch_inherits_outer_prefix_ops() {
        // Branch born inside another branch's cross-line replay must
        // record the outer replay's first-line prefix as part of its
        // own `prefix_ops`. Otherwise its later cross-line fail
        // reconstructs the replayed line from an empty prefix and the
        // captures emitted before the *outer* branch trigger vanish.
        // Shipped as `[foo]: /url` losing its
        // `meta.link.reference.def.markdown` / `entity.name.reference`
        // scopes in `syntax_test_markdown.md`: the line creates an
        // outer `link-def-title-continuation` branch whose alt-1
        // (`immediately-pop2`) replay spawns a nested
        // `link-def-attr-continuation` branch — when *that* branch
        // fails on the next line its replay drops the original LRD
        // opener captures.
        let syntax_str = r#"
name: ReplayPrefix
scope: source.rp
contexts:
  main:
    - match: '(K)(EY)'
      captures:
        1: keyword.k.rp
        2: variable.k.rp
      push: outer
  outer:
    - match: '$'
      branch_point: bp1
      branch: [a1, a2]
  a1:
    - meta_include_prototype: false
    - match: '(?=FAIL1)'
      fail: bp1
    - match: '.'
  a2:
    - meta_include_prototype: false
    - match: '$'
      branch_point: bp2
      branch: [b1, b2]
    - match: '.'
  b1:
    - meta_include_prototype: false
    - match: '(?=FAIL2)'
      fail: bp2
    - match: '.'
  b2:
    - meta_include_prototype: false
    - match: '\n'
      scope: support.fallback.rp
      pop: 2
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // line 1 — captures `K` and `EY`, pushes `outer`, then `$` fires bp1.
        let line1 = "KEY\n";
        let _ = state.parse_line(line1, &ss).expect("line 1 parses");

        // line 2 — `(?=FAIL1)` in a1 trips bp1's cross-line fail. The
        // alt-1 replay of line 1 spawns bp2 in a2 at end-of-line.
        let line2 = "FAIL1\n";
        let _ = state.parse_line(line2, &ss).expect("line 2 parses");

        // line 3 — `(?=FAIL2)` in b1 trips bp2's cross-line fail. With
        // the fix bp2's `prefix_ops` carries the K / EY captures from
        // bp1's replay, so the second cross-line replay re-emits them.
        // Without the fix bp2's `prefix_ops` is empty and the replayed
        // line 1 ops collapse to just `support.fallback.rp` push/pop.
        let line3 = "FAIL2\n";
        let out3 = state.parse_line(line3, &ss).expect("line 3 parses");

        // bp2's cross-line replay covered both buffered lines (line 1
        // + line 2). Line 1 is the one that must keep its captures.
        assert_eq!(
            out3.replayed.len(),
            2,
            "bp2 cross-line replay should cover line1 + line2, got {:?}",
            out3.replayed,
        );
        let line1_ops = &out3.replayed[0];

        let pushes_keyword = line1_ops.iter().any(|(_, op)| {
            matches!(op, ScopeStackOp::Push(s) if *s == Scope::new("keyword.k.rp").unwrap())
        });
        let pushes_variable = line1_ops.iter().any(|(_, op)| {
            matches!(op, ScopeStackOp::Push(s) if *s == Scope::new("variable.k.rp").unwrap())
        });
        assert!(
            pushes_keyword,
            "line 1 replayed ops should still push keyword.k.rp; got {:?}",
            line1_ops,
        );
        assert!(
            pushes_variable,
            "line 1 replayed ops should still push variable.k.rp; got {:?}",
            line1_ops,
        );
    }

    /// Two back-to-back link reference definitions followed by a
    /// paragraph: each LRD's chain is closed by the *next* line's parse
    /// emitting a `Pop` against the LRD's `meta_scope` via
    /// `flushed_ops`. Without snapshot-drift correction, the
    /// pre-correction `pending_line_start_shadows` (and the consumer's
    /// `parsed_line_buffer[i].stack_before`) used by the second
    /// replay's stack-reset still reflected the first LRD's leftover
    /// `meta.link.reference.def.markdown` push, so the corrected line
    /// 4 ops re-applied that scope onto a stale baseline — leaking it
    /// into the paragraph and on through the rest of the file.
    /// Sized 408 chars / 88 assertions in
    /// `syntax_test_markdown.md`.
    #[test]
    fn back_to_back_lrds_clear_meta_scope_via_corrected_baseline() {
        use crate::parsing::SyntaxSet;
        let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        let syntax = ss
            .find_syntax_by_path("Packages/Markdown/Markdown.sublime-syntax")
            .unwrap();
        let mut state = ParseState::new(syntax);

        // Mirror the syntest consumer's stack-tracking with the
        // snapshot-drift correction the bug requires.
        struct Record {
            stack_before: ScopeStack,
        }
        let mut buffer: Vec<Record> = Vec::new();
        let mut stack = ScopeStack::new();

        for line in ["[foo]: first\n", "[foo]: second\n", "bar\n"] {
            let out = state.parse_line(line, &ss).expect("parse");
            if !out.replayed.is_empty() {
                let buf_len = buffer.len();
                let start_idx = buf_len - out.replayed.len();
                stack = buffer[start_idx].stack_before.clone();
                let mut corrected: Vec<(usize, ScopeStack)> = Vec::new();
                for (i, replayed_ops) in out.replayed.iter().enumerate() {
                    for (_, op) in replayed_ops {
                        let _ = stack.apply(op);
                    }
                    let next_idx = start_idx + i + 1;
                    if next_idx < buf_len {
                        corrected.push((next_idx, stack.clone()));
                    }
                }
                for (idx, c) in corrected {
                    buffer[idx].stack_before = c;
                }
            }
            let stack_before = stack.clone();
            for (_, op) in &out.ops {
                let _ = stack.apply(op);
            }
            buffer.push(Record { stack_before });
        }

        let lrd = Scope::new("meta.link.reference.def.markdown").unwrap();
        let leaked = stack.as_slice().contains(&lrd);
        assert!(
            !leaked,
            "meta.link.reference.def.markdown leaked past back-to-back \
             LRDs into 'bar' paragraph; consumer stack at end: {:?}",
            stack,
        );
        let shadow_leaked = state.shadow.as_slice().contains(&lrd);
        assert!(
            !shadow_leaked,
            "syntect shadow disagrees with corrected consumer stack; \
             shadow at end: {:?}",
            state.shadow,
        );
    }

    #[cfg(feature = "default-onig")]
    #[test]
    fn pop_n_branch_point_keeps_bp_so_alt_fail_unwinds_meta_scope() {
        // Real-syntax repro for the Java class-extends annotation leak:
        // `class T extends a.@b.c Foo {}`. The branch_point on
        // `annotation-qualified-identifier-name`'s `pop: 2 + branch_point:
        // annotation-qualified-parameters` was being pruned by perform_op's
        // post-Set retain (`bp.stack_depth <= final_len` ignored
        // `bp.pop_count`), so the branch's first alt — which has
        // `meta_content_scope: meta.annotation.identifier.java` — was
        // never failed-out, leaking `meta.annotation.identifier.java`
        // past every nested-annotation extends path in the Java suite.
        use crate::parsing::SyntaxSet;
        let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        let syntax = ss
            .find_syntax_by_path("Packages/Java/Java.sublime-syntax")
            .unwrap();
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        for line in ["class T extends a.@b.c Foo {}\n"] {
            let out = state.parse_line(line, &ss).expect("parse");
            for (_, op) in &out.ops {
                let _ = stack.apply(op);
            }
        }
        let ann = Scope::new("meta.annotation.identifier.java").unwrap();
        assert!(
            !stack.as_slice().contains(&ann),
            "meta.annotation.identifier.java leaked past `@b.c` annotation \
             into the outer extends path; final stack: {:?}",
            stack,
        );
        assert!(
            !state.shadow.as_slice().contains(&ann),
            "syntect shadow still carries meta.annotation.identifier.java; \
             shadow: {:?}",
            state.shadow,
        );
    }

    #[cfg(feature = "default-onig")]
    #[test]
    fn cross_line_pop_n_branch_point_alt_fail_unwinds_meta_scope() {
        // Cross-line variant of the Java annotation leak:
        // `@A.B\nclass E {}\n`. At end of line 1, the
        // `annotation-qualified-parameters` branch_point is live waiting
        // for `(`. Line 2 starts with `class`, so alt 1 fails and alt 2
        // (`immediately-pop`) runs via handle_fail's cross-line path. That
        // path used a bespoke re-emit of just `context.meta_scope` /
        // `meta_content_scope`, missing the popped contexts' Pop — leaving
        // `meta.annotation.identifier.java` and the surrounding
        // declaration's meta_scope (`meta.class.java` /
        // `meta.enum.java` / `meta.interface.java`) on the stack. Routing
        // through `push_meta_ops` with a synthetic Set/Push (mirroring the
        // same-line fix) emits the popped contexts' Pop alongside the new
        // alternative's meta_scope push.
        //
        // The consumer must apply `out.replayed` corrected ops the same
        // way `examples/syntest.rs` does: rewind to the buffered line's
        // pre-parse stack, replay the corrected ops in order, then apply
        // the current line's ops. This mirrors the LRD test above.
        use crate::parsing::SyntaxSet;
        struct Record {
            stack_before: ScopeStack,
        }
        let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        let syntax = ss
            .find_syntax_by_path("Packages/Java/Java.sublime-syntax")
            .unwrap();
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let mut buffer: Vec<Record> = Vec::new();
        for line in ["@A.B\n", "class E {}\n"] {
            let out = state.parse_line(line, &ss).expect("parse");
            if !out.replayed.is_empty() {
                let buf_len = buffer.len();
                let start_idx = buf_len - out.replayed.len();
                stack = buffer[start_idx].stack_before.clone();
                let mut corrected: Vec<(usize, ScopeStack)> = Vec::new();
                for (i, replayed_ops) in out.replayed.iter().enumerate() {
                    for (_, op) in replayed_ops {
                        let _ = stack.apply(op);
                    }
                    let next_idx = start_idx + i + 1;
                    if next_idx < buf_len {
                        corrected.push((next_idx, stack.clone()));
                    }
                }
                for (idx, c) in corrected {
                    buffer[idx].stack_before = c;
                }
            }
            let stack_before = stack.clone();
            for (_, op) in &out.ops {
                let _ = stack.apply(op);
            }
            buffer.push(Record { stack_before });
        }
        let ann = Scope::new("meta.annotation.identifier.java").unwrap();
        let cls = Scope::new("meta.class.java").unwrap();
        assert!(
            !stack.as_slice().contains(&ann),
            "meta.annotation.identifier.java leaked past `@A.B` annotation \
             into top-level scope after cross-line `class E {{}}`; final \
             stack: {:?}",
            stack,
        );
        assert!(
            !stack.as_slice().contains(&cls),
            "meta.class.java leaked past `class E {{}}` body close into \
             top-level scope; final stack: {:?}",
            stack,
        );
        assert!(
            !state.shadow.as_slice().contains(&ann),
            "syntect shadow still carries meta.annotation.identifier.java; \
             shadow: {:?}",
            state.shadow,
        );
    }

    #[cfg(feature = "default-onig")]
    #[test]
    fn exhausted_branch_point_falls_through_to_parent_next_rule() {
        // Java's `$x ;` at top level: the `declarations` branch_point's
        // zero-width `(?=[\p{L}_$@<])` lookahead matches `$`. All five
        // alternatives (class/enum/interface/variable/method) fail
        // because `$x` isn't a valid declaration. ST then falls through
        // to the `java` context's NEXT rule (`else-expressions`), which
        // pushes an `expression` chain that scopes `$x` as
        // `meta.variable.identifier.java variable.other.java`. Syntect
        // previously advanced one character past the lookahead, letting
        // the next iteration's regex set match `package` / `class` etc.
        // in the middle of identifiers like `package$` or `$package`.
        //
        // After this fix, exhausting a branch_point at a position
        // rewinds the cursor to that position and marks the
        // branch_point's name as skipped — so the parent context's
        // remaining rules get a chance to fire at the same cursor.
        use crate::parsing::SyntaxSet;
        let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        let syntax = ss
            .find_syntax_by_path("Packages/Java/Java.sublime-syntax")
            .unwrap();
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let dollar_id = "$x ;\n";
        let out = state.parse_line(dollar_id, &ss).expect("parse");
        for (_, op) in &out.ops {
            let _ = stack.apply(op);
        }
        let var_id = Scope::new("meta.variable.identifier.java").unwrap();
        let var_other = Scope::new("variable.other.java").unwrap();
        // `$x` itself is fully popped at `;`; reconstruct the per-byte
        // scope by walking ops up to byte 1 (`x`) and confirm the
        // identifier scope was active there.
        let mut mid = ScopeStack::new();
        for (pos, op) in &out.ops {
            if *pos > 1 {
                break;
            }
            let _ = mid.apply(op);
        }
        assert!(
            mid.as_slice().contains(&var_id),
            "expected meta.variable.identifier.java active over `$x`; got: {:?}",
            mid,
        );
        assert!(
            mid.as_slice().contains(&var_other),
            "expected variable.other.java active over `$x`; got: {:?}",
            mid,
        );
    }

    #[cfg(feature = "default-onig")]
    #[test]
    fn pop_n_restores_clear_before_unwinding_deeper_meta_scopes() {
        // Java's `case DayType when -> "incomplete";` lands in
        // `case-label-expression` (clear_scopes:1, mcs: case.label).
        // The `clear_scopes:1` hides the parent `case-label`'s
        // `meta_scope` (meta.case). When `case-label-end` matches
        // `->` with `pop: 2`, the rule must unwind both
        // `case-label-expression` AND `case-label`. With the deeper
        // meta_scope Pop emitted before the cur_context's clear was
        // restored, the consumer popped the wrong (still-visible)
        // scope — the surrounding `meta.block.java` (switch's block)
        // — leaving `meta.case.java` orphaned past the `->`.
        //
        // Restoring cur_context's clear BEFORE the depth-loop's
        // deeper-meta_scope pops makes the previously-cleared atom
        // visible again so it can be popped correctly.
        use crate::parsing::SyntaxSet;
        let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        let syntax = ss
            .find_syntax_by_path("Packages/Java/Java.sublime-syntax")
            .unwrap();
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        for line in [
            "class C {\n",
            "  void f(Object o) {\n",
            "    return switch (o) {\n",
            "       case DayType when -> \"incomplete\";\n",
        ] {
            let out = state.parse_line(line, &ss).expect("parse");
            for (_, op) in &out.ops {
                let _ = stack.apply(op);
            }
        }
        let case = Scope::new("meta.statement.conditional.case.java").unwrap();
        let label = Scope::new("meta.statement.conditional.case.label.java").unwrap();
        assert!(
            !stack.as_slice().contains(&case),
            "meta.statement.conditional.case.java leaked past `->`; stack: {:?}",
            stack,
        );
        assert!(
            !stack.as_slice().contains(&label),
            "meta.statement.conditional.case.label.java leaked past `->`; stack: {:?}",
            stack,
        );
    }

    #[cfg(feature = "default-onig")]
    #[test]
    fn deeper_inner_bp_correction_does_not_double_outer_meta_scope() {
        // `class C { @anno /**/ fully\n. @anno qualified\n/**/ . /**/\n@anno /**/ object @anno()`
        // triggers a NESTED cross-line replay where the inner BP is
        // structurally a child of the outer BP's resolved alternative
        // (outer `class-members` at depth 4, inner `object-type` at
        // depth 9). PR #663's `prefer_inner_replay_corrections`
        // unconditionally replaced outer's locally-computed ops with
        // inner's corrections, doubling `meta.field.type.java` on the
        // `object @anno()` line — outer's full-line ops correctly
        // emit one `meta.field.type` and inner's reparse adds another
        // because outer's chosen alt already provides that meta_scope.
        //
        // Discriminator: only prefer inner when its stack_depth is at
        // most outer's. Equal-depth siblings (PR #663's original
        // `@A.B\n(par=1)\nenum E {}` case) keep preferring inner;
        // strictly-deeper nested BPs stay with outer's ops.
        use crate::parsing::SyntaxSet;
        let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        let syntax = ss
            .find_syntax_by_path("Packages/Java/Java.sublime-syntax")
            .unwrap();
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        for line in [
            "class C {\n",
            "  @anno /**/ fully\n",
            "  . @anno qualified\n",
            "  /**/ . /**/\n",
            "  @anno /**/ object @anno()\n",
        ] {
            let out = state.parse_line(line, &ss).expect("parse");
            for (_, op) in &out.ops {
                let _ = stack.apply(op);
            }
        }
        // Reconstruct the running stack at byte position 13 of line 5
        // (`object`), where the regression's doubled push was visible.
        let mut at_object = ScopeStack::new();
        let last_line = "  @anno /**/ object @anno()\n";
        let mut state2 = ParseState::new(syntax);
        let prelude = [
            "class C {\n",
            "  @anno /**/ fully\n",
            "  . @anno qualified\n",
            "  /**/ . /**/\n",
        ];
        for line in prelude {
            let out = state2.parse_line(line, &ss).expect("parse");
            for (_, op) in &out.ops {
                let _ = at_object.apply(op);
            }
        }
        let out = state2.parse_line(last_line, &ss).expect("parse");
        for (pos, op) in &out.ops {
            if *pos > 13 {
                break;
            }
            let _ = at_object.apply(op);
        }
        let field_type = Scope::new("meta.field.type.java").unwrap();
        let doubled = at_object
            .as_slice()
            .iter()
            .filter(|s| **s == field_type)
            .count();
        assert!(
            doubled <= 1,
            "meta.field.type.java pushed {} times entering `object` on \
             line 5 (expected at most 1); stack: {:?}",
            doubled,
            at_object,
        );
    }

    /// Regression guard for the `embed_scope`-replaces / inner Set
    /// interaction. With a wrapper context that pushes 3 mcs scopes
    /// and `embed_scope_replaces=true`, the embedded syntax's first
    /// `set:` rule must not drop the topmost wrapper scope — its mcs
    /// pop must be skipped because the embedded main's mcs was never
    /// pushed.
    #[test]
    #[ignore = "requires testdata/Packages submodule"]
    fn embed_scope_replaces_preserves_wrapper_mcs_across_inner_set() {
        let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        let md = ss
            .find_syntax_by_scope(Scope::new("text.html.markdown").unwrap())
            .expect("Markdown loaded");
        let mut state = ParseState::new(md);
        let mut stack = ScopeStack::new();
        for line in ["```bash\n", "#!/usr/bin/env bash\n"] {
            let out = state.parse_line(line, &ss).expect("parse");
            for (_, op) in &out.ops {
                let _ = stack.apply(op);
            }
        }
        let bash = Scope::new("source.shell.bash").unwrap();
        assert!(
            stack.as_slice().contains(&bash),
            "source.shell.bash (wrapper's last embed_scope token) must be \
             on the stack after the embedded syntax's first `set:` fires; \
             stack: {:?}",
            stack
        );
    }

    /// Regression: in a Markdown zsh fenced block, the indented shebang
    /// `   #!/usr/bin/env zsh` must enter `comment.line.shebang.shell`
    /// (lenient `Bash (for Markdown).main` rule), not the regular
    /// `comment.line.number-sign.shell` (strict inherited Bash main).
    /// `Zsh (for Markdown)` extends both `Bash (for Markdown)` (which
    /// owns a custom `main`) and `Zsh` (which inherits Bash's standard
    /// strict `main`). The parent merge must prefer the own definition
    /// over the inherited one.
    #[test]
    #[ignore = "requires testdata/Packages submodule"]
    fn zsh_for_markdown_uses_lenient_shebang_main_from_bash_for_markdown() {
        let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        let md = ss
            .find_syntax_by_scope(Scope::new("text.html.markdown").unwrap())
            .expect("Markdown loaded");
        let mut state = ParseState::new(md);
        let shebang = Scope::new("comment.line.shebang.shell").unwrap();
        let number_sign = Scope::new("comment.line.number-sign.shell").unwrap();
        let mut saw_shebang = false;
        let mut saw_number_sign = false;
        for line in ["```zsh\n", "   #!/usr/bin/env zsh\n"] {
            let out = state.parse_line(line, &ss).expect("parse");
            for (_, op) in &out.ops {
                if let ScopeStackOp::Push(s) = op {
                    if *s == shebang {
                        saw_shebang = true;
                    }
                    if *s == number_sign {
                        saw_number_sign = true;
                    }
                }
            }
        }
        assert!(
            saw_shebang,
            "expected a Push(comment.line.shebang.shell) op (lenient \
             Bash (for Markdown).main wins over Zsh's inherited Bash main)"
        );
        assert!(
            !saw_number_sign,
            "must not fall through to comment.line.number-sign.shell \
             (regular comments rule)"
        );
    }

    /// Regression: in a non-terminated Markdown link reference definition
    /// title, the empty line between the title's last content line and the
    /// next paragraph must keep the LRD's `meta_scope`
    /// (`meta.link.reference.def.markdown`) active at column 0. Without
    /// the fix, the chained branch_point exhaustion
    /// (`link-title-continuation` + `link-def-attr-continuation`) collapses
    /// the entire LRD frame on line 2's `\n`, dropping the LRD scope on
    /// the empty line.
    ///
    /// Mirrors syntest's per-character scope semantics: ops at position
    /// `>= line.len()` apply to the next line's stack baseline (per
    /// `ScopeRegionIterator`), so the per-char stack at line 3 col 0
    /// reflects the post-replay baseline plus only the in-line ops at
    /// position 0.
    #[test]
    #[ignore = "requires testdata/Packages submodule"]
    fn lrd_blank_line_keeps_meta_scope_active() {
        let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        let md = ss
            .find_syntax_by_scope(Scope::new("text.html.markdown").unwrap())
            .expect("Markdown loaded");
        let mut state = ParseState::new(md);
        let mut baseline = ScopeStack::new();
        let mut buffered_lines: Vec<(String, Vec<(usize, ScopeStackOp)>, ScopeStack)> = Vec::new();
        // (line_text, ops, stack_before)

        for &line in &["[//]: # (testing\n", "blah\n", "\n", "text\n"] {
            let out = state.parse_line(line, &ss).expect("parse");
            // Replay handling: reset baseline to pre-first-replayed-line state,
            // then apply replay ops in order to rebuild the live baseline.
            if !out.replayed.is_empty() {
                let start_idx = buffered_lines.len() - out.replayed.len();
                baseline = buffered_lines[start_idx].2.clone();
                for (i, replay_ops) in out.replayed.iter().enumerate() {
                    for (_, op) in replay_ops {
                        let _ = baseline.apply(op);
                    }
                    let entry = &mut buffered_lines[start_idx + i];
                    entry.1 = replay_ops.clone();
                }
            }
            // Snapshot stack_before for this line (for future replay base).
            let stack_before = baseline.clone();
            // Apply this line's live ops at positions < line.len() only —
            // ops at >= line.len() belong to the next line's baseline (per
            // syntest's wrap convention).
            let mut col_0_stack = stack_before.clone();
            let mut after_in_line_stack = stack_before.clone();
            for (pos, op) in &out.ops {
                let _ = baseline.apply(op);
                if *pos < line.len() {
                    let _ = after_in_line_stack.apply(op);
                }
                if *pos == 0 {
                    let _ = col_0_stack.apply(op);
                }
            }
            buffered_lines.push((line.to_string(), out.ops.clone(), stack_before));

            if line == "\n" {
                let lrd = Scope::new("meta.link.reference.def.markdown").unwrap();
                assert!(
                    col_0_stack.as_slice().contains(&lrd),
                    "expected `meta.link.reference.def.markdown` at empty \
                     line col 0; got: {:?}",
                    col_0_stack
                        .as_slice()
                        .iter()
                        .map(|s| s.build_string())
                        .collect::<Vec<_>>()
                );
            }
            if line == "text\n" {
                let paragraph = Scope::new("meta.paragraph.markdown").unwrap();
                let lrd = Scope::new("meta.link.reference.def.markdown").unwrap();
                assert!(
                    after_in_line_stack.as_slice().contains(&paragraph),
                    "expected `meta.paragraph.markdown` on `text` line"
                );
                assert!(
                    !after_in_line_stack.as_slice().contains(&lrd),
                    "LRD must be popped on `text` line; got: {:?}",
                    after_in_line_stack
                        .as_slice()
                        .iter()
                        .map(|s| s.build_string())
                        .collect::<Vec<_>>()
                );
            }
        }
    }

    /// Regression: an `embed: scope:source.guest#leaf` (with `#fragment`)
    /// must NOT mark the wrapper as `embed_scope_replaces`. The fragment
    /// context's `meta_content_scope` is independent of the syntax's
    /// top-level scope; suppressing it strips a real grammar atom and the
    /// next `clear_scopes:` then bites the wrapper instead. Observed on
    /// Python's PEP 723 inline TOML (`#toml`).
    #[test]
    fn fragment_embed_preserves_target_meta_content_scope() {
        use crate::parsing::ScopeStack;

        let host = SyntaxDefinition::load_from_str(
            r#"
name: FragHost
scope: source.fraghost
file_extensions: [fraghost]
version: 2
contexts:
  main:
    - match: '<<'
      embed: scope:source.fragguest#leaf
      embed_scope: wrapper.atom
      escape: '>>'
"#,
            true,
            None,
        )
        .unwrap();

        let guest = SyntaxDefinition::load_from_str(
            r#"
name: FragGuest
scope: source.fragguest
file_extensions: [fragguest]
version: 2
hidden: true
contexts:
  main:
    - match: ''
      pop: true
  leaf:
    - meta_content_scope: leaf.mcs.atom
    - match: '\w+'
      scope: keyword.fragguest
"#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(host);
        builder.add(guest);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("FragHost").unwrap();
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        // Parse a line that opens the embed but never closes it, so the
        // wrapper + leaf stay on the final stack for inspection.
        let out = state.parse_line("<<word\n", &ss).expect("parse");
        for (_, op) in &out.ops {
            stack.apply(op).expect("apply");
        }

        let scopes: Vec<String> = stack
            .as_slice()
            .iter()
            .map(|s| format!("{:?}", s))
            .collect();
        let wrapper = Scope::new("wrapper.atom").unwrap();
        let leaf = Scope::new("leaf.mcs.atom").unwrap();
        assert!(
            stack.as_slice().contains(&wrapper),
            "wrapper.atom must remain visible inside fragment embed; got: {:?}",
            scopes
        );
        assert!(
            stack.as_slice().contains(&leaf),
            "leaf.mcs.atom (fragment target's meta_content_scope) must be \
             pushed and visible; got: {:?}",
            scopes
        );
    }

    /// Regression gate: `embed: scope:source.guest` (NO fragment) still
    /// keeps the `embed_scope_replaces` suppression. The wrapper's last
    /// embed_scope atom equals the guest syntax's top-level scope (the
    /// auto-insert at `yaml_load.rs:706-713`); without suppression the
    /// scope would appear twice on the stack.
    #[test]
    fn non_fragment_embed_still_suppresses_main_mcs() {
        use crate::parsing::ScopeStack;

        let host = SyntaxDefinition::load_from_str(
            r#"
name: NonFragHost
scope: source.nonfraghost
file_extensions: [nonfraghost]
version: 2
contexts:
  main:
    - match: '<<'
      embed: scope:source.nonfragguest
      embed_scope: wrapper2.atom source.nonfragguest
      escape: '>>'
"#,
            true,
            None,
        )
        .unwrap();

        let guest = SyntaxDefinition::load_from_str(
            r#"
name: NonFragGuest
scope: source.nonfragguest
file_extensions: [nonfragguest]
version: 2
hidden: true
contexts:
  main:
    - match: '\w+'
      scope: keyword.nonfragguest
"#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(host);
        builder.add(guest);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("NonFragHost").unwrap();
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        // Parse a line that opens the embed but never closes it, so the
        // wrapper + guest scopes stay on the final stack for inspection.
        let out = state.parse_line("<<word\n", &ss).expect("parse");
        for (_, op) in &out.ops {
            stack.apply(op).expect("apply");
        }

        let guest_scope = Scope::new("source.nonfragguest").unwrap();
        let count = stack
            .as_slice()
            .iter()
            .filter(|s| **s == guest_scope)
            .count();
        assert_eq!(
            count, 1,
            "source.nonfragguest must appear exactly once (wrapper's last \
             embed_scope atom; guest main's auto-inserted top-level scope \
             must be suppressed); stack: {:?}",
            stack
        );
    }

    #[cfg(feature = "default-onig")]
    #[test]
    fn cross_line_all_exhaust_with_pop_count_emits_popped_meta_scope_pops() {
        // Java's `@Anno\n.\nAnno\n(par=1)\nenum E {}` at top level. Line 1
        // creates the `annotations` (alt unqualified) and the inner
        // `annotation-unqualified-parameters` BPs; line 2's `.` matches
        // `(?={{single_dot}}) fail: annotation-identifier`, retrying alt 1
        // (`annotation-qualified-identifier`) cross-line. The qualified
        // alt has `meta_scope: meta.annotation.identifier.java
        // meta.path.java`, which the cross-line replay's outer-locally-
        // computed line-1 ops do NOT carry — outer (`declarations`)'s
        // `parse_line_inner_from(line0, …)` re-parses line 1 under its
        // resolved alt-1 stack and picks alt-0 (unqualified) of the inner
        // `annotation-identifier` BP, only retrying to alt-1 (qualified)
        // when line 2's `.` arrives during outer's replay of line 1.
        // Inner's flushed corrections carry `meta.path.java`, and the
        // refined depth-bounded gate in `prefer_inner_replay_corrections`
        // (`depth_diff in {0, 1}`) substitutes them onto outer's locally
        // computed ops while still skipping the deeper-inner case the
        // doubling guard
        // (`deeper_inner_bp_correction_does_not_double_outer_meta_scope`)
        // protects against.
        //
        // Test setup applies `out.replayed` corrected ops via the same
        // consumer pattern as
        // `cross_line_pop_n_branch_point_alt_fail_unwinds_meta_scope`,
        // then samples the corrected stack at byte 0 of line 1 (`@`).
        use crate::parsing::SyntaxSet;
        struct Record {
            stack_before: ScopeStack,
            ops: Vec<(usize, ScopeStackOp)>,
        }
        let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        let syntax = ss
            .find_syntax_by_path("Packages/Java/Java.sublime-syntax")
            .unwrap();
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let mut buffer: Vec<Record> = Vec::new();
        for line in ["@Anno\n", ".\n", "Anno\n", "(par=1)\n", "enum E {}\n"] {
            let out = state.parse_line(line, &ss).expect("parse");
            if !out.replayed.is_empty() {
                let buf_len = buffer.len();
                let start_idx = buf_len - out.replayed.len();
                stack = buffer[start_idx].stack_before.clone();
                let mut corrected: Vec<(usize, ScopeStack, Vec<(usize, ScopeStackOp)>)> =
                    Vec::new();
                for (i, replayed_ops) in out.replayed.iter().enumerate() {
                    for (_, op) in replayed_ops {
                        let _ = stack.apply(op);
                    }
                    let next_idx = start_idx + i + 1;
                    if next_idx < buf_len {
                        corrected.push((next_idx, stack.clone(), replayed_ops.clone()));
                    }
                    // Capture the replayed ops for this index so a later
                    // sample can reconstruct the line's running stack.
                    if let Some(rec) = buffer.get_mut(start_idx + i) {
                        rec.ops = replayed_ops.clone();
                    }
                }
                for (idx, c_stack, _) in corrected {
                    buffer[idx].stack_before = c_stack;
                }
            }
            let stack_before = stack.clone();
            for (_, op) in &out.ops {
                let _ = stack.apply(op);
            }
            buffer.push(Record {
                stack_before,
                ops: out.ops.clone(),
            });
        }
        // Reconstruct the running scope at byte 0 of line 1 (the `@`)
        // using the buffered (possibly replayed) ops.
        let line0 = &buffer[0];
        let mut at_at = line0.stack_before.clone();
        for (pos, op) in &line0.ops {
            if *pos > 0 {
                break;
            }
            let _ = at_at.apply(op);
        }
        let ann = Scope::new("meta.annotation.identifier.java").unwrap();
        let path = Scope::new("meta.path.java").unwrap();
        assert!(
            at_at.as_slice().contains(&ann),
            "meta.annotation.identifier.java should be active at `@` of \
             line 1 after cross-line retry to qualified alt; stack: {:?}",
            at_at,
        );
        assert!(
            at_at.as_slice().contains(&path),
            "meta.path.java should be active at `@` of line 1 after \
             cross-line retry to qualified alt (its meta_scope is \
             `meta.annotation.identifier.java meta.path.java`); stack: {:?}",
            at_at,
        );
    }
}
