//! The executor token loop: pattern search, best-match selection, and
//! match execution for [`ParseState`].

use super::semantics::build_capture_ops;
use super::*;

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
    #[allow(clippy::too_many_arguments)]
    pub(super) fn parse_next_token(
        &mut self,
        line: &str,
        syntax_set: &SyntaxSet,
        start: &mut usize,
        search_cache: &mut SearchCache,
        regions: &mut Region,
        non_consuming_push_at: &mut (usize, usize, usize),
        ops: &mut Vec<(usize, ScopeStackOp)>,
    ) -> Result<bool, ParsingError> {
        let (check_pop_loop, pre_push_depth) = {
            let (pos, pre, post) = *non_consuming_push_at;
            let armed =
                pos == *start && pre < self.core.stack.len() && self.core.stack.len() <= post;
            (armed, pre)
        };

        // Trim proto_starts that are no longer valid
        while self
            .core
            .proto_starts
            .last()
            .map(|start| *start >= self.core.stack.len())
            .unwrap_or(false)
        {
            self.core.proto_starts.pop();
        }

        let best_match = self.find_best_match(
            line,
            *start,
            syntax_set,
            search_cache,
            regions,
            check_pop_loop,
            pre_push_depth,
        )?;

        if let Some(reg_match) = best_match {
            // Check if this is an escape match (sentinel pat_index)
            if reg_match.pat_index == usize::MAX {
                let (match_start, match_end) = reg_match.regions.pos(0).unwrap();
                let zero_width = match_start == match_end;

                if zero_width
                    && self
                        .zero_width_escape_fires
                        .get(&match_start)
                        .map(|n| *n >= ZERO_WIDTH_ESCAPE_FIRE_LIMIT)
                        .unwrap_or(false)
                {
                    // Zero-width escape repeatedly firing at the same byte
                    // within a single `parse_line_inner_from` — diagnostic of
                    // a branch-fail rewind cycle (#650). Suppress and advance
                    // one character to break the loop, mirroring the
                    // `would_loop` enforcement below. Threshold lets
                    // legitimate alt-cycle replays through `handle_fail`
                    // re-encounter the same offset without false-tripping.
                    if let Some((i, _)) = line[*start..].char_indices().nth(1) {
                        *start += i;
                        return Ok(true);
                    } else {
                        return Ok(false);
                    }
                }

                *start = match_end;
                self.exec_escape(
                    reg_match.escape_index,
                    match_start,
                    match_end,
                    &reg_match.regions,
                    syntax_set,
                    ops,
                )?;
                if zero_width {
                    *self.zero_width_escape_fires.entry(match_start).or_insert(0) += 1;
                }
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
                    let id = &self.core.stack[self.core.stack.len() - 1].context;
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

            // Trail engine: a branch trigger consults or extends the
            // decision trail before anything is emitted or consumed, so
            // a `Refuse` decision can suppress the pattern entirely and
            // a restart can resume from this exact point.
            #[cfg(not(feature = "legacy-engine"))]
            let mut empty_line_resume = false;
            #[cfg(not(feature = "legacy-engine"))]
            if let MatchOperation::Branch {
                ref name,
                ref alternatives,
                pop_count,
            } = match_pattern.operation
            {
                // See `defer_eol_branch`: a non-consuming branch at
                // end-of-string on a re-executed earlier window line
                // anchors on the next line instead (mirrors the legacy
                // replay-time rule further down).
                if match_end <= *start
                    && match_end >= line.len()
                    && self.defer_eol_branch(name, *start)
                {
                    return Ok(false);
                }
                match self.decide_branch(
                    name,
                    alternatives.len(),
                    pop_count,
                    *start,
                    ops.len(),
                    *non_consuming_push_at,
                ) {
                    Some((alt, bumped_same_line)) => {
                        self.set_pending_alt(alt);
                        // Empty-line continuation placement (ST parity,
                        // mirrors the legacy same-line fail rule): when a
                        // same-line fail bumps the alternative on an
                        // empty line's col-0 branch, its non-consuming
                        // replacement (typically `match: '' pop: N`) must
                        // emit its pops past EOL so the empty line keeps
                        // the parent meta scope (e.g. Markdown's
                        // `meta.link.reference.def.markdown` on the blank
                        // line inside a link-reference-definition title).
                        if bumped_same_line
                            && *start == 0
                            && line.len() <= 1
                            && line.trim().is_empty()
                        {
                            empty_line_resume = true;
                        }
                    }
                    None => {
                        // Refused: re-search at the same cursor with the
                        // branch pattern suppressed, so the parent
                        // context's next rule gets a chance.
                        return Ok(true);
                    }
                }
            }

            let consuming = match_end > *start;
            if !consuming {
                // The match doesn't consume any characters. If this is a
                // "push", remember the position and the post-push stack depth
                // interval so that we can check the next "pop" for loops.
                // Otherwise leave the state, e.g. non-consuming "set" could
                // also result in a loop.
                //
                // The interval [pre+1, pre+K] covers every depth the unwind
                // can pass through; for K=1 it collapses to the original
                // armed depth pre+1, preserving the prior single-push guard.
                let k = match &match_pattern.operation {
                    MatchOperation::Push { ctx_refs, .. } => ctx_refs.len(),
                    MatchOperation::Branch { .. } => 1,
                    MatchOperation::Embed { contexts, .. } => contexts.len(),
                    _ => 0,
                };
                if k > 0 {
                    let pre = self.core.stack.len();
                    let post = pre + k;
                    *non_consuming_push_at = (match_end, pre, post);
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
                #[cfg(feature = "legacy-engine")]
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
                self.core.proto_starts.push(self.core.stack.len());
            }

            let level_context = {
                let id = &self.core.stack[self.core.stack.len() - 1].context;
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

            #[cfg(not(feature = "legacy-engine"))]
            if empty_line_resume {
                *start = line.len();
            }

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
        pre_push_depth: usize,
    ) -> Result<Option<RegexMatch<'a>>, ParsingError> {
        let cur_level = &self.core.stack[self.core.stack.len() - 1];
        let context = syntax_set.get_context(&cur_level.context)?;
        let prototype = if let Some(ref p) = context.prototype {
            Some(p)
        } else {
            None
        };

        // Build an iterator for the contexts we want to visit in order
        let context_chain = {
            let proto_start = self.core.proto_starts.last().cloned().unwrap_or(0);
            // Sublime applies with_prototypes from bottom to top
            let with_prototypes = self.core.stack[proto_start..].iter().flat_map(|lvl| {
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

        for (ei, entry) in self.core.escape_stack.iter().enumerate() {
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

                    // println!("matched pattern {:?} at start {} end {} (pop would loop: {}, min start: {}, initial start: {}, check_pop_loop: {}, stack_len: {})", match_pat, match_start, match_end, pop_would_loop, min_start, start, check_pop_loop, self.core.stack.len());

                    if match_start < min_start || (match_start == min_start && pop_would_loop) {
                        // New match is earlier in text than old match,
                        // or old match was a looping pop at the same
                        // position.

                        // println!("setting as current match");

                        min_start = match_start;

                        let consuming = match_end > start;
                        // A non-consuming `pop: N` after a non-consuming push
                        // loops iff its post-pop depth equals the trigger
                        // depth — strict equality. Dropping *below*
                        // pre_push_depth leaves the trigger context entirely
                        // (e.g. Haskell's `immediately-pop2` as the fallback
                        // branch alternative for `declaration-type-end`).
                        // Generalises the prior `Pop(1)` narrowing (which
                        // matched K=1) to multi-context pushes where the loop
                        // closes via a multi-level pop chain (e.g. `pop:1`
                        // then `pop:2` against `push:[a,b,c]`).
                        pop_would_loop = check_pop_loop
                            && !consuming
                            && match &match_pat.operation {
                                MatchOperation::Pop(n) => {
                                    self.core.stack.len().saturating_sub(*n) == pre_push_depth
                                }
                                _ => false,
                            };

                        let push_too_deep = matches!(
                            match_pat.operation,
                            MatchOperation::Push { .. }
                                | MatchOperation::Branch { .. }
                                | MatchOperation::Embed { .. }
                        ) && self.core.stack.len() >= 100;

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
                MatchOperation::Push { .. }
                | MatchOperation::Branch { .. }
                | MatchOperation::Embed { .. } => self.core.stack.len() < 100,
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
        non_consuming_push_at: &mut (usize, usize, usize),
        ops: &mut Vec<(usize, ScopeStackOp)>,
        search_cache: &mut SearchCache,
    ) -> Result<bool, ParsingError> {
        let (match_start, match_end) = reg_match.regions.pos(0).unwrap();
        let context = reg_match.context;
        let pat = context.match_at(reg_match.pat_index)?;

        // The trail engine's fail path doesn't rewind in place, so the
        // executor-local cursor state stays untouched here.
        #[cfg(not(feature = "legacy-engine"))]
        let _ = (&start, &non_consuming_push_at, &search_cache);

        // Handle Fail: attempt backtracking
        if let MatchOperation::Fail(ref name) = pat.operation {
            #[cfg(feature = "legacy-engine")]
            return self.handle_fail(
                name,
                line,
                start,
                non_consuming_push_at,
                ops,
                search_cache,
                syntax_set,
            );
            #[cfg(not(feature = "legacy-engine"))]
            {
                // Whether the fail scheduled a restart or was a no-op,
                // stop the token loop; the window driver takes over.
                self.fail_branch(name);
                return Ok(false);
            }
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
                // Trail engine: the alternative was chosen by
                // `decide_branch` in `parse_next_token`; no snapshot is
                // taken here — the decision's checkpoint carries it.
                #[cfg(not(feature = "legacy-engine"))]
                let chosen_alt = {
                    let _ = name;
                    self.take_pending_alt()
                        .expect("branch reached exec_pattern without a trail decision")
                };
                #[cfg(feature = "legacy-engine")]
                let chosen_alt = 0;
                #[cfg(feature = "legacy-engine")]
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
                    // When `handle_fail` is mid-replay, `self.core.line_number` /
                    // `self.pending_lines` still reflect the *outer* current
                    // line — read through `replay_ctx` so a branch born
                    // during replay anchors to the virtual replay line `L+i`.
                    let (bp_line_number, bp_pending_lines_snapshot_len) = match &self.replay_ctx {
                        Some(ctx) => (ctx.line_number, ctx.pending_lines_snapshot_offset),
                        None => (
                            self.core.line_number.saturating_sub(1),
                            self.pending_lines.len(),
                        ),
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
                        stack_snapshot: self.core.stack.clone(),
                        proto_starts_snapshot: self.core.proto_starts.clone(),
                        match_start: *start, // position before this match's advance
                        trigger_match_start: match_start,
                        pat_scope: pat.scope.clone(),
                        line_number: bp_line_number,
                        ops_snapshot_len: ops.len(),
                        stack_depth: self.core.stack.len(),
                        non_consuming_push_at_snapshot: *non_consuming_push_at,
                        first_line_snapshot: self.core.first_line,
                        with_prototype: pat.with_prototype.clone(),
                        pending_lines_snapshot_len: bp_pending_lines_snapshot_len,
                        escape_stack_snapshot: self.core.escape_stack.clone(),
                        pop_count,
                        prefix_ops,
                        capture_ops: pat
                            .captures
                            .as_ref()
                            .map(|m| build_capture_ops(m, &reg_match.regions))
                            .unwrap_or_default(),
                    };
                    self.branch_points.push(bp);
                    if let Some(tracker) = self.inner_replay_max_depth.as_mut() {
                        let last = self.branch_points.last().unwrap();
                        if last.stack_depth > tracker.depth {
                            tracker.depth = last.stack_depth;
                            tracker.bp = Some(BpInfo {
                                name: last.name.clone(),
                                stack_depth: last.stack_depth,
                                line_number: last.line_number,
                                inner_producer: None,
                            });
                        }
                    }
                }
                // `pop: N + branch:` is **lookahead** per ST: the trigger
                // token must NOT inherit the popped frames' meta_scope.
                // Route through `Push { pop_count }` so the existing
                // Push-with-pop lookahead path in `push_meta_ops` /
                // `perform_op` handles both the deeper-pop emission and
                // the runtime stack mutation.
                synthetic_op = MatchOperation::Push {
                    ctx_refs: vec![alternatives[chosen_alt].clone()],
                    pop_count,
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
}
