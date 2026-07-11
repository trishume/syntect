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

/// Maximum number of times a zero-width escape match can fire at the
/// same byte position within a single `parse_line_inner_from` call
/// before subsequent fires are suppressed (cursor advances one char
/// without applying the escape). Bounds the unbounded branch-fail
/// rewind cycle that hangs the parser on Perl POD-embedded language
/// sections (#650). Threshold is generous to permit legitimate
/// alt-cycle replays that re-encounter the same offset.
const ZERO_WIDTH_ESCAPE_FIRE_LIMIT: u32 = 100;

/// The pure interpreter state of the parser: what the executor reads and
/// writes while matching a line, independent of the speculation machinery
/// (branch points, buffered lines, corrected ops) that lives directly on
/// [`ParseState`]. Cloning a `Core` captures everything needed to resume
/// parsing from this point.
#[derive(Debug, Clone, Eq, PartialEq)]
struct Core {
    stack: Vec<StateLevel>,
    first_line: bool,
    // See issue #101. Contains indices of frames pushed by `with_prototype`s.
    // Doesn't look at `with_prototype`s below top of stack.
    proto_starts: Vec<usize>,
    /// Line counter for 128-line branch point expiry.
    line_number: usize,
    /// Active escape patterns from embed operations. The escape regex takes
    /// strict precedence over normal patterns — it is checked first and can
    /// truncate the search region.
    escape_stack: Vec<EscapeEntry>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ParseState {
    core: Core,
    /// Active branch points for backtracking support.
    #[cfg(not(feature = "trail-engine"))]
    branch_points: Vec<BranchPoint>,
    /// Line strings buffered while branch points are active, for potential
    /// cross-line `fail` replay. Only the strings are stored; the ops are
    /// returned to callers immediately (same as before).
    #[cfg(not(feature = "trail-engine"))]
    pending_lines: Vec<String>,
    /// Snapshot of `shadow` at the start of each buffered line in
    /// `pending_lines`. Used by the cross-line-fail replay to restore
    /// `shadow` to its state at the first replayed line's beginning, so
    /// the shadow mirrors what the consumer does (reset + apply
    /// replayed).
    #[cfg(not(feature = "trail-engine"))]
    pending_line_start_shadows: Vec<ScopeStack>,
    /// Corrected ops produced by a cross-line `fail` replay, to be returned
    /// as `ParseLineOutput::replayed` at the end of `parse_line`. When
    /// populated, entry `i` corresponds to `pending_lines[flushed_ops_start + i]`.
    #[cfg(not(feature = "trail-engine"))]
    flushed_ops: Vec<Vec<(usize, ScopeStackOp)>>,
    /// Pending-lines index that `flushed_ops[0]` maps to when `flushed_ops`
    /// is non-empty. Reset to `None` between `parse_line` calls.
    #[cfg(not(feature = "trail-engine"))]
    flushed_ops_start: Option<usize>,
    /// Identity of the branch point whose cross-line replay wrote each
    /// slot of `flushed_ops`. Indexed identically to `flushed_ops`
    /// (length always matches). Each slot remembers the BP whose replay
    /// produced its current ops, so subsequent merges can compare
    /// per-slot rather than per-buffer.
    #[cfg(not(feature = "trail-engine"))]
    flushed_ops_bp_per_slot: Vec<BpInfo>,
    /// Warnings accumulated during parsing, drained into `ParseLineOutput`.
    warnings: Vec<String>,
    /// Mirror of the consumer's scope stack. Updated at `parse_line`
    /// boundaries (not mid-line) from the returned `ops` and
    /// `replayed`, mirroring the consumer's behaviour (reset to the
    /// first-replayed line's start, then apply replayed, then apply
    /// current ops). `exec_escape` uses it to detect orphan atoms left
    /// on the consumer's stack by a prior cross-line replay whose
    /// later same-line fails truncated the owning context out of
    /// `self.core.stack` (the Push for the atom is committed in
    /// `flushed_ops`, so it can't be taken back by `ops.truncate`) —
    /// and emits a balancing Pop before the normal escape pops.
    #[cfg(not(feature = "trail-engine"))]
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
    #[cfg(not(feature = "trail-engine"))]
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
    #[cfg(not(feature = "trail-engine"))]
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
    /// Active while an outer `handle_fail` is replaying buffered lines
    /// inside `parse_line_inner_from` / `parse_line_inner`. Each
    /// `branch_point` creation during the replay updates this to the
    /// strictly-deeper of the new BP and the current tip — so
    /// `prefer_inner_replay_corrections` / `merge_flushed` can see the
    /// inner replay's max-attempted-depth even for failed alternatives
    /// that never surface in `flushed_ops_bp_per_slot`. Saved/restored
    /// across nested inner replays via `mem::replace` (mirrors
    /// `saved_flushed`). `None` outside an inner replay; `Some(default)`
    /// at the start of one. Cluster-B candidate-#2 diagnostic
    /// (probe assertions in
    /// `cross_line_path_field_type_keeps_meta_path_on_continuation_line`).
    #[cfg(not(feature = "trail-engine"))]
    inner_replay_max_depth: Option<MaxDepthSeen>,
    /// Per-line count of zero-width escape fires keyed by byte
    /// position. Once the count at a position exceeds
    /// `ZERO_WIDTH_ESCAPE_FIRE_LIMIT`, subsequent zero-width escapes
    /// at that position are suppressed (cursor advances one char
    /// without applying the escape, mirroring the would_loop
    /// enforcement). The threshold is high enough to permit
    /// legitimate multi-attempt replays through `handle_fail`'s
    /// alt-cycle and still bound the unbounded branch-fail rewind
    /// cycle that hangs the parser on Perl POD-embedded sections
    /// (#650). Cleared per `parse_line_inner_from` invocation, so a
    /// cross-line replay reprocessing the same line offset starts
    /// fresh. Intentionally NOT snapshotted into `BranchPoint`.
    zero_width_escape_fires: HashMap<usize, u32>,
    /// Trail-engine state: buffered window, per-line provisional ops,
    /// and the decision trail. See the `trail` module.
    #[cfg(feature = "trail-engine")]
    trail: trail::TrailState,
}

/// Tracker installed on `ParseState` for the duration of an outer
/// `handle_fail`'s inner replay. Records the strictly-deepest
/// `branch_point` created while the replay loop runs.
#[cfg(not(feature = "trail-engine"))]
#[derive(Debug, Clone, Default, Eq, PartialEq)]
struct MaxDepthSeen {
    depth: usize,
    bp: Option<BpInfo>,
}

/// Compact summary of a `(byte_offset, ScopeStackOp)` pair returned by
/// `ops_divergence` for use by `is_replace_shape`.
#[cfg(not(feature = "trail-engine"))]
#[derive(Debug, Clone, Eq, PartialEq)]
#[allow(dead_code)]
struct OpSummary {
    pos: usize,
    kind: &'static str,
    scope: String,
}

/// Identity of a branch point whose cross-line replay wrote ops to
/// `flushed_ops`. Captured so `prefer_inner_replay_corrections` can
/// compare the inner BP (whose corrections are candidate replacements)
/// against the outer BP (whose locally-computed replay is the default).
#[cfg(not(feature = "trail-engine"))]
#[derive(Debug, Clone, Eq, PartialEq)]
struct BpInfo {
    name: String,
    /// Stack depth at branch creation (mirrors `BranchPoint::stack_depth`).
    stack_depth: usize,
    /// Line number at branch creation (mirrors `BranchPoint::line_number`).
    line_number: usize,
    /// The inner BP whose cross-line replay actually wrote this slot's
    /// content (when an outer's `merge_flushed` is rolling up an
    /// inner-substituted slot via `prefer_inner_replay_corrections`).
    /// `None` when the outer BP itself wrote the slot. Boxed to keep
    /// `BpInfo` small in the common case. Cluster-A diagnostic
    /// (`cross_line_chained_fail_pushes_target_meta_scope_on_continuation_line`)
    /// uses this to discriminate `SnapGtStart` merge slots that share an
    /// identical `(name, depth, line)` outer attribution but differ in
    /// the inner BP whose replay produced them.
    inner_producer: Option<Box<BpInfo>>,
}

/// Bookkeeping override used while `handle_fail` is re-parsing a
/// buffered past line. See the `replay_ctx` field on `ParseState`.
#[cfg(not(feature = "trail-engine"))]
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
#[cfg(not(feature = "trail-engine"))]
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
    non_consuming_push_at_snapshot: (usize, usize, usize),
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

mod core;
mod embed;
mod semantics;
#[cfg(not(feature = "trail-engine"))]
mod speculation;
#[cfg(feature = "yaml-load")]
#[cfg(test)]
mod tests;
#[cfg(feature = "trail-engine")]
mod trail;

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
            core: Core {
                stack: vec![start_state],
                first_line: true,
                proto_starts: Vec::new(),
                line_number: 0,
                escape_stack: Vec::new(),
            },
            #[cfg(not(feature = "trail-engine"))]
            branch_points: Vec::new(),
            #[cfg(not(feature = "trail-engine"))]
            pending_lines: Vec::new(),
            #[cfg(not(feature = "trail-engine"))]
            pending_line_start_shadows: Vec::new(),
            #[cfg(not(feature = "trail-engine"))]
            flushed_ops: Vec::new(),
            #[cfg(not(feature = "trail-engine"))]
            flushed_ops_start: None,
            #[cfg(not(feature = "trail-engine"))]
            flushed_ops_bp_per_slot: Vec::new(),
            warnings: Vec::new(),
            #[cfg(not(feature = "trail-engine"))]
            shadow: ScopeStack::new(),
            #[cfg(not(feature = "trail-engine"))]
            replay_ctx: None,
            #[cfg(not(feature = "trail-engine"))]
            replay_prefix_ops: None,
            skipped_branches: Vec::new(),
            #[cfg(not(feature = "trail-engine"))]
            inner_replay_max_depth: None,
            zero_width_escape_fires: HashMap::default(),
            #[cfg(feature = "trail-engine")]
            trail: trail::TrailState::default(),
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
    #[cfg(not(feature = "trail-engine"))]
    pub fn parse_line(
        &mut self,
        line: &str,
        syntax_set: &SyntaxSet,
    ) -> Result<ParseLineOutput, ParsingError> {
        if self.core.stack.is_empty() {
            return Err(ParsingError::MissingMainContext);
        }

        // Skipped-branch entries are tied to byte offsets within a single
        // line — they don't survive across line boundaries.
        self.skipped_branches.clear();

        // Prune branch points older than 128 lines
        let cur_line = self.core.line_number;
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
        self.core.line_number += 1;

        let pending_lines_before = self.pending_lines.len();

        let ops = self.parse_line_inner(line, syntax_set)?;

        // Collect any corrected ops produced by a cross-line `fail` during the
        // parse above.  These are stored by `handle_fail` in `self.flushed_ops`.
        let replayed = std::mem::take(&mut self.flushed_ops);
        self.flushed_ops_start = None;
        self.flushed_ops_bp_per_slot.clear();

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
    #[cfg(not(feature = "trail-engine"))]
    pub fn is_speculative(&self) -> bool {
        !self.branch_points.is_empty()
    }

    /// Inner parsing loop: processes `line` with the current parser state and
    /// returns the scope-stack operations.  Does **not** touch `pending_lines`
    /// or `flushed_ops`, so it is safe to call recursively from `handle_fail`
    /// for cross-line replay without re-entrancy issues.
    #[cfg(not(feature = "trail-engine"))]
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
    #[cfg(not(feature = "trail-engine"))]
    fn parse_line_inner_from(
        &mut self,
        line: &str,
        syntax_set: &SyntaxSet,
        start_at: usize,
    ) -> Result<Vec<(usize, ScopeStackOp)>, ParsingError> {
        let mut match_start = start_at;
        let mut res = Vec::new();

        if start_at == 0 && self.core.first_line {
            let cur_level = &self.core.stack[self.core.stack.len() - 1];
            let context = syntax_set.get_context(&cur_level.context)?;
            if !context.meta_content_scope.is_empty() {
                res.push((0, ScopeStackOp::Push(context.meta_content_scope[0])));
            }
            self.core.first_line = false;
        }

        let mut regions = Region::new();
        let fnv = BuildHasherDefault::<FnvHasher>::default();
        let mut search_cache: SearchCache = HashMap::with_capacity_and_hasher(128, fnv);
        // Used for detecting loops with push/pop, see long comment above.
        let mut non_consuming_push_at = (0, 0, 0);
        // Per-iteration zero-width escape fire record (#650). Cleared
        // here so each `parse_line_inner_from` call (including replays
        // invoked by `handle_fail`) starts fresh — a legitimate
        // cross-line replay re-entering the same line offset is not
        // a loop.
        self.zero_width_escape_fires.clear();

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
}
