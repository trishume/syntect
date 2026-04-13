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
    /// Corrected ops produced by a cross-line `fail` replay, to be returned
    /// as `ParseLineOutput::replayed` at the end of `parse_line`.
    flushed_ops: Vec<Vec<(usize, ScopeStackOp)>>,
    /// Warnings accumulated during parsing, drained into `ParseLineOutput`.
    warnings: Vec<String>,
    /// Active escape patterns from embed operations. The escape regex takes
    /// strict precedence over normal patterns — it is checked first and can
    /// truncate the search region.
    escape_stack: Vec<EscapeEntry>,
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
    /// truncated off `ops` along with alt[0]'s subsequent work.
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
            flushed_ops: Vec::new(),
            warnings: Vec::new(),
            escape_stack: Vec::new(),
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

        let ops = self.parse_line_inner(line, syntax_set)?;

        // Collect any corrected ops produced by a cross-line `fail` during the
        // parse above.  These are stored by `handle_fail` in `self.flushed_ops`.
        let replayed = std::mem::take(&mut self.flushed_ops);

        // Keep the line string for potential future cross-line replay.
        if !self.branch_points.is_empty() {
            self.pending_lines.push(line.to_string());
        } else {
            // No active branch points: any buffered strings are stale.
            self.pending_lines.clear();
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
        let mut match_start = 0;
        let mut res = Vec::new();

        if self.first_line {
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
            }

            *start = match_end;

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
                .search(line, start, line.len(), Some(&mut esc_regions))
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
                        pop_would_loop = check_pop_loop
                            && !consuming
                            && matches!(match_pat.operation, MatchOperation::Pop(_));

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

        if let Some(maybe_region) = search_cache.get(&match_ptr) {
            if let Some(ref region) = *maybe_region {
                let (cached_start, cached_end) = region.pos(0).unwrap();
                if cached_start >= start && cached_end <= search_end {
                    // Cached match is valid within the truncated region.
                    return Some(region.clone());
                } else if cached_start >= start && cached_start < search_end {
                    // Match starts within range but extends past search_end.
                    // Can't use cache — need to re-search. Fall through below.
                } else if cached_start >= search_end {
                    // Cached match is beyond our search end — treat as no match
                    return None;
                }
                // cached_start < start: cache miss, re-search below
            } else {
                // Didn't find a match earlier, so no point trying to match it again
                return None;
            }
        }

        let (regex, can_cache) = match (match_pat.has_captures, captures) {
            (true, Some(captures)) => {
                let (region, s) = captures;
                (&match_pat.regex_with_refs(region, s), false)
            }
            _ => (match_pat.regex(), true),
        };
        // print!("  executing regex: {:?} at pos {} on line {}", regex.regex_str(), start, line);
        let matched = regex.search(line, start, search_end, Some(regions));

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
                let bp = BranchPoint {
                    name: name.clone(),
                    next_alternative: 1, // 0 is about to be pushed
                    alternatives: alternatives.clone(),
                    stack_snapshot: self.stack.clone(),
                    proto_starts_snapshot: self.proto_starts.clone(),
                    match_start: *start, // position before this match's advance
                    trigger_match_start: match_start,
                    pat_scope: pat.scope.clone(),
                    line_number: self.line_number.saturating_sub(1), // current line (already incremented)
                    ops_snapshot_len: ops.len(),
                    stack_depth: self.stack.len(),
                    non_consuming_push_at_snapshot: *non_consuming_push_at,
                    first_line_snapshot: self.first_line,
                    with_prototype: pat.with_prototype.clone(),
                    pending_lines_snapshot_len: self.pending_lines.len(),
                    escape_stack_snapshot: self.escape_stack.clone(),
                    pop_count,
                };
                self.branch_points.push(bp);
                // When pop_count > 0 (pop + branch), use Set semantics to
                // pop the current context before pushing the first alternative.
                synthetic_op = if pop_count > 0 {
                    MatchOperation::Set(vec![alternatives[0].clone()])
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
        if let Some(ref capture_map) = pat.captures {
            // captures could appear in an arbitrary order, have to produce ops in right order
            // ex: ((bob)|(hi))* could match hibob in wrong order, and outer has to push first
            // we don't have to handle a capture matching multiple times, Sublime doesn't
            let mut map: Vec<((usize, i32), ScopeStackOp)> = Vec::new();
            for &(cap_index, ref scopes) in capture_map.iter() {
                if let Some((cap_start, cap_end)) = reg_match.regions.pos(cap_index) {
                    // marking up empty captures causes pops to be sorted wrong
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
        // Find the branch point by name (most recent first)
        let bp_index = self.branch_points.iter().rposition(|bp| bp.name == name);
        let bp_index = match bp_index {
            Some(i) => i,
            None => return Ok(false), // No such branch point, fail is no-op
        };

        let cur_line = self.line_number.saturating_sub(1);
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

        // Check validity: stack depth still >= branch's stack_depth
        if self.stack.len() < bp.stack_depth {
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
            // `brace-interpolation-fallback`). Same-line only — a
            // cross-line branch_point that never succeeded is left
            // alone here because replaying through it is handled
            // via the cross-line path in the successful case.
            let is_cross_line = bp.line_number < cur_line;
            if !is_cross_line {
                let stack_snapshot = bp.stack_snapshot.clone();
                let proto_starts_snapshot = bp.proto_starts_snapshot.clone();
                let escape_stack_snapshot = bp.escape_stack_snapshot.clone();
                let first_line_snapshot = bp.first_line_snapshot;
                let non_consuming_push_at_snapshot = bp.non_consuming_push_at_snapshot;
                let ops_snapshot_len = bp.ops_snapshot_len;
                let match_start_pos = bp.match_start;
                self.branch_points.remove(bp_index);

                self.stack = stack_snapshot;
                self.proto_starts = proto_starts_snapshot;
                self.escape_stack = escape_stack_snapshot;
                self.first_line = first_line_snapshot;
                *non_consuming_push_at = non_consuming_push_at_snapshot;
                ops.truncate(ops_snapshot_len.min(ops.len()));

                // Advance one char past the branch_point match to avoid
                // immediately re-matching the same `(?=...)` lookahead.
                if let Some((i, _)) = line[match_start_pos..].char_indices().nth(1) {
                    *start = match_start_pos + i;
                } else {
                    // End of line — no character to advance past.
                    *start = line.len();
                }
                search_cache.clear();
                return Ok(true);
            }
            self.branch_points.remove(bp_index);
            return Ok(false); // All alternatives exhausted (cross-line)
        }

        // Determine if this is a cross-line fail (branch was created on a previous line).
        let is_cross_line = bp.line_number < cur_line;

        // Extract everything we need from bp before mutating self.
        let next_alt_index = bp.next_alternative;
        let next_alt = bp.alternatives[next_alt_index].clone();
        let match_start_pos = bp.match_start;
        let trigger_match_start = bp.trigger_match_start;
        let trigger_pat_scope = bp.pat_scope.clone();
        let stack_snapshot = bp.stack_snapshot.clone();
        let proto_starts_snapshot = bp.proto_starts_snapshot.clone();
        let first_line_snapshot = bp.first_line_snapshot;
        let non_consuming_push_at_snapshot = bp.non_consuming_push_at_snapshot;
        let ops_snapshot_len = bp.ops_snapshot_len;
        let pending_lines_snapshot_len = bp.pending_lines_snapshot_len;
        let escape_stack_snapshot = bp.escape_stack_snapshot.clone();
        // bp borrow ends here.

        let pop_count = self.branch_points[bp_index].pop_count;

        // Restore parser state to the snapshot.
        self.stack = stack_snapshot;
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
        let context = syntax_set.get_context(&context_id)?;
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
            // The new alternative (context) is already on the stack, so
            // parse_line_inner will process each buffered line starting in that
            // context, producing correct ops.
            let truncated_lines: Vec<String> = self
                .pending_lines
                .drain(pending_lines_snapshot_len..)
                .collect();

            let mut replayed_ops: Vec<Vec<(usize, ScopeStackOp)>> =
                Vec::with_capacity(truncated_lines.len());
            for replay_line in &truncated_lines {
                let line_ops = self.parse_line_inner(replay_line, syntax_set)?;
                replayed_ops.push(line_ops);
            }
            // Append (rather than overwrite) in case multiple cross-line fails
            // fire on the same parse_line call.
            self.flushed_ops.extend(replayed_ops);

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
            *start = match_start_pos;

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
            for scope in &trigger_pat_scope {
                ops.push((trigger_match_start, ScopeStackOp::Push(*scope)));
            }
            if !trigger_pat_scope.is_empty() {
                ops.push((match_start_pos, ScopeStackOp::Pop(trigger_pat_scope.len())));
            }

            // Emit meta scope ops for the new context at the rewind position.
            if let Some(clear_amount) = context.clear_scopes {
                ops.push((match_start_pos, ScopeStackOp::Clear(clear_amount)));
            }
            for scope in context.meta_scope.iter() {
                ops.push((match_start_pos, ScopeStackOp::Push(*scope)));
            }
            for scope in context.meta_content_scope.iter() {
                ops.push((match_start_pos, ScopeStackOp::Push(*scope)));
            }
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
                    // Each deeper context's scopes are popped in
                    // top-to-bottom order: meta_content_scope first
                    // (pushed after its own meta_scope, hence above on
                    // the stack), then meta_scope.
                    for depth in 1..pop_count {
                        let level_idx = stack_len - 1 - depth;
                        let ctx =
                            syntax_set.get_context(&self.stack[level_idx].context)?;
                        let skip_content = version >= 2
                            && level_idx >= 1
                            && syntax_set
                                .get_context(&self.stack[level_idx - 1].context)
                                .map(|c| c.embed_scope_replaces)
                                .unwrap_or(false);
                        if !skip_content && !ctx.meta_content_scope.is_empty() {
                            ops.push((
                                index,
                                ScopeStackOp::Pop(ctx.meta_content_scope.len()),
                            ));
                        }
                        if !ctx.meta_scope.is_empty() {
                            ops.push((index, ScopeStackOp::Pop(ctx.meta_scope.len())));
                        }
                    }
                }

                // cleared scopes are restored after the scopes from match pattern that invoked the pop are applied
                if !initial && cur_context.clear_scopes.is_some() {
                    ops.push((index, ScopeStackOp::Restore))
                }
            }
            // for some reason the ST3 behaviour of set is convoluted and is inconsistent with the docs and other ops
            // - the meta_content_scope of the current context is applied to the matched thing, unlike pop
            // - the clear_scopes are applied after the matched token, unlike push
            // - the interaction with meta scopes means that the token has the meta scopes of both the current scope and the new scope.
            MatchOperation::Push(ref context_refs) | MatchOperation::Set(ref context_refs) => {
                let is_set = matches!(*match_op, MatchOperation::Set(_));
                // a match pattern that "set"s keeps the meta_content_scope and meta_scope from the previous context
                if initial {
                    // v2: pop parent's meta_content_scope so matched text does not see it
                    if is_set && version >= 2 && !cur_context.meta_content_scope.is_empty() {
                        ops.push((
                            index,
                            ScopeStackOp::Pop(cur_context.meta_content_scope.len()),
                        ));
                    }
                    if is_set && cur_context.clear_scopes.is_some() {
                        // cleared scopes from the old context are restored immediately
                        ops.push((index, ScopeStackOp::Restore));
                    }
                    // add each context's meta scope
                    if version >= 2 {
                        // v2: For push with multiple contexts, only apply clear_scopes
                        // from the last (topmost) context, not sum all of them.
                        //
                        // For `set:`, skip the Clear here: the non-initial
                        // "repush" phase generates its own Clear, and emitting
                        // one in both phases produces two Clears for a single
                        // Restore, stranding a scope on clear_stack and
                        // causing Pop underflow when the push group unwinds.
                        // (The v1 path already had this guard via `!is_set`.)
                        let last_idx = context_refs.len().saturating_sub(1);
                        for (i, r) in context_refs.iter().enumerate() {
                            let ctx = r.resolve(syntax_set)?;

                            if !is_set && i == last_idx {
                                if let Some(clear_amount) = ctx.clear_scopes {
                                    ops.push((index, ScopeStackOp::Clear(clear_amount)));
                                }
                            }

                            for scope in ctx.meta_scope.iter() {
                                ops.push((index, ScopeStackOp::Push(*scope)));
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
                    let repush = (is_set
                        && (!cur_context.meta_scope.is_empty()
                            || !cur_context.meta_content_scope.is_empty()))
                        || context_refs.iter().any(|r| {
                            let ctx = r.resolve(syntax_set).unwrap();

                            !ctx.meta_content_scope.is_empty()
                                || (ctx.clear_scopes.is_some() && is_set)
                        });
                    if repush {
                        // remove previously pushed meta scopes, so that meta content scopes will be applied in the correct order
                        let mut num_to_pop: usize = context_refs
                            .iter()
                            .map(|r| {
                                let ctx = r.resolve(syntax_set).unwrap();
                                ctx.meta_scope.len()
                            })
                            .sum();

                        // also pop off the original context's meta scopes
                        if is_set {
                            if version >= 2 {
                                // v2: set excludes parent meta_content_scope from matched text
                                num_to_pop += cur_context.meta_scope.len();
                            } else {
                                num_to_pop += cur_context.meta_content_scope.len()
                                    + cur_context.meta_scope.len();
                            }
                        }

                        // do all the popping as one operation
                        if num_to_pop > 0 {
                            ops.push((index, ScopeStackOp::Pop(num_to_pop)));
                        }

                        // now we push meta scope and meta context scope for each context pushed
                        if version >= 2 {
                            // v2: For multiple push, only apply clear_scopes from last context
                            let last_idx = context_refs.len().saturating_sub(1);
                            let mut prev_embed_scope_replaces = false;
                            for (i, r) in context_refs.iter().enumerate() {
                                let ctx = r.resolve(syntax_set)?;

                                if is_set && i == last_idx {
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
                    MatchOperation::Set(contexts.clone())
                } else {
                    MatchOperation::Push(contexts.clone())
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
                    MatchOperation::Set(alternatives.clone())
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
                    self.branch_points
                        .retain(|bp| bp.stack_depth <= self.stack.len());
                    self.escape_stack
                        .retain(|e| e.stack_depth < self.stack.len());
                }
                (contexts, None, true)
            }
            MatchOperation::Set(ref ctx_refs) => {
                // a `with_prototype` stays active when the context is `set`
                // until the context layer in the stack (where the `with_prototype`
                // was initially applied) is popped off.
                (ctx_refs, self.stack.pop().map(|s| s.prototypes), false)
            }
            MatchOperation::Pop(n) => {
                for _ in 0..n {
                    self.stack.pop();
                }
                // Invalidate branch points whose stack depth is now above current stack
                self.branch_points
                    .retain(|bp| bp.stack_depth <= self.stack.len());
                // Remove escape entries whose stack_depth >= current stack
                self.escape_stack
                    .retain(|e| e.stack_depth < self.stack.len());
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

        // Pop all stack levels down to target_depth, emitting proper meta scope pops
        while self.stack.len() > target_depth {
            let level = &self.stack[self.stack.len() - 1];
            let ctx = syntax_set.get_context(&level.context)?;

            // Pop meta_content_scope
            if !ctx.meta_content_scope.is_empty() {
                // v2: check if context below has embed_scope_replaces
                let version = self.current_syntax_version(syntax_set);
                let skip = version >= 2
                    && self.stack.len() >= 2
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
    /// `main` must be idempotent.
    #[test]
    fn extending_syntax_does_not_double_push_top_level_scope() {
        use crate::parsing::SyntaxSet;
        let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        // Git Diff extends Diff (Basic) — a concrete case of the bug.
        let syntax = ss.find_syntax_by_name("Git Diff").unwrap();
        let mut state = ParseState::new(syntax);
        let o = ops(&mut state, "From 1234567890 Mon Sep 17 00:00:00 2001\n", &ss);
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
        let final_scopes: Vec<String> =
            stack.as_slice().iter().map(|s| format!("{:?}", s)).collect();
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
        let final_scopes: Vec<String> =
            stack.as_slice().iter().map(|s| format!("{:?}", s)).collect();
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
        let after_assignment: Vec<String> =
            stack.as_slice().iter().map(|s| format!("{:?}", s)).collect();
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
        let final_scopes: Vec<String> =
            stack.as_slice().iter().map(|s| format!("{:?}", s)).collect();
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
    fn v2_set_clear_scopes_only_from_last_context() {
        // Kills: L1029 replace && with || / == with != in push_meta_ops
        // In v2, clear_scopes during a set should only apply from the last
        // context, and only when `is_set` is true AND `i == last_idx`.
        let syntax_str = r#"
name: V2ClearLast
scope: source.v2clear
version: 2
contexts:
  main:
    - meta_scope: meta.main.v2clear
    - match: 'GO'
      set: [ctx-b, ctx-a]
  ctx-a:
    - meta_scope: meta.a.v2clear
    - match: '\w+'
      scope: word.a.v2clear
  ctx-b:
    - clear_scopes: true
    - meta_scope: meta.b.v2clear
    - match: '\w+'
      scope: word.b.v2clear
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let raw_ops = ops(&mut state, "GO hello\n", &ss);

        // ctx-a is last in the set stack (top of stack), and it has no
        // clear_scopes.  ctx-b has clear_scopes but is NOT the last context
        // in v2.  If the condition is inverted (|| instead of &&), clear_scopes
        // would incorrectly apply from ctx-b.
        let states = stack_states(raw_ops);
        // "hello" matches in ctx-a (top of stack), which should have meta.a
        let hello_states: Vec<_> = states
            .iter()
            .filter(|s| s.contains("word.a.v2clear"))
            .collect();
        assert!(
            !hello_states.is_empty(),
            "expected word.a.v2clear, got states: {:?}",
            states
        );
        // meta.a should be present (not cleared), since ctx-a is top and has
        // no clear_scopes of its own
        assert!(
            hello_states.iter().any(|s| s.contains("meta.a.v2clear")),
            "meta.a should be present since ctx-a (last) has no clear_scopes: {:?}",
            hello_states
        );
        // source.v2clear must also be present — if clear_scopes from ctx-b
        // fires incorrectly (mutations on L1029: && → || or == → !=), the
        // root scope would be cleared.
        assert!(
            hello_states
                .iter()
                .any(|s| s.contains("source.v2clear")),
            "source.v2clear should not be cleared (clear_scopes should only apply from last context): {:?}",
            hello_states
        );
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
}
