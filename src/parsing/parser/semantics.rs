//! Sublime Text scope semantics: meta-scope emission (`push_meta_ops`),
//! stack operations (`perform_op`), and capture-op construction.

use super::embed::escape_str;
use super::*;

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
pub(super) fn build_capture_ops(
    capture_map: &CaptureMapping,
    regions: &Region,
) -> Vec<(usize, ScopeStackOp)> {
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

impl ParseState {
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

    pub(super) fn push_meta_ops(
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
            MatchOperation::Push {
                ctx_refs: ref context_refs,
                ..
            }
            | MatchOperation::Set {
                ctx_refs: ref context_refs,
                ..
            } => {
                let is_set = matches!(*match_op, MatchOperation::Set { .. });
                let set_pop_count = match *match_op {
                    MatchOperation::Set { pop_count, .. } => pop_count.max(1),
                    _ => 1,
                };
                // `pop: N + push:` is **lookahead**: per ST docs, when pop is
                // combined with push the pop happens first and the matched
                // text is treated as a lookahead, so the popped frames'
                // meta_scope atoms must be dropped from the visible stack
                // before the trigger token records its scope. (`pop: N + set:`
                // is the documented exception — stacking — handled below.)
                let push_pop_count = match *match_op {
                    MatchOperation::Push { pop_count, .. } => pop_count,
                    _ => 0,
                };
                let is_push_with_pop = push_pop_count > 0;
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
                    // `pop: N + push:` with N > 1: per ST's lookahead rule,
                    // the (N-1) deeper popped contexts' meta_scope /
                    // meta_content_scope atoms must be dropped from the
                    // visible stack BEFORE the trigger token records its
                    // scope. Without this, the trigger token observes the
                    // leaked deeper meta_scope — observed on Java's
                    // `@RunWith(JUnit4.class)` where
                    // `pop: 2 + push: annotation-parameters-body` left
                    // `meta.annotation.identifier.java`
                    // (annotation-unqualified-identifier's meta_scope) on
                    // the stack at the `(` token instead of just
                    // `meta.annotation.parameters.java meta.group.java`.
                    //
                    // `pop: N + set:` is the ST-documented exception
                    // (stacking, not lookahead): the trigger token
                    // RECEIVES the popped frames' meta_scope. That path is
                    // handled in the non-initial phase below — both the
                    // genuine stacking case (target has its own ms) and the
                    // case where the target has no ms (e.g. TS's
                    // `(?:get|set|async){{identifier_break}} pop: 2 + set:`
                    // in `object-property-name`, where ST keeps
                    // `meta.mapping.key.js` visible to the `get` token).
                    //
                    // Skip when cur or any popped deeper context has
                    // `clear_scopes`: those interact with the clear_stack
                    // through the existing non-initial pipeline (Restore
                    // ordering vs head_pop, multi-target Clear preview),
                    // and the rotation here would race with that.
                    let mut apply_initial_deeper_pop = is_push_with_pop && push_pop_count > 1;
                    apply_initial_deeper_pop &= cur_context.clear_scopes.is_none();
                    if apply_initial_deeper_pop {
                        let stack_len = self.stack.len();
                        for depth in 1..push_pop_count.min(stack_len) {
                            let level_idx = stack_len - 1 - depth;
                            let ctx = syntax_set.get_context(&self.stack[level_idx].context)?;
                            if ctx.clear_scopes.is_some() {
                                apply_initial_deeper_pop = false;
                                break;
                            }
                        }
                    }
                    if apply_initial_deeper_pop {
                        let cur_ms_rotate = cur_context.meta_scope.len();
                        if cur_ms_rotate > 0 {
                            ops.push((index, ScopeStackOp::Pop(cur_ms_rotate)));
                        }
                        let stack_len = self.stack.len();
                        for depth in 1..push_pop_count.min(stack_len) {
                            let level_idx = stack_len - 1 - depth;
                            let ctx = syntax_set.get_context(&self.stack[level_idx].context)?;
                            if !ctx.meta_content_scope.is_empty() {
                                ops.push((index, ScopeStackOp::Pop(ctx.meta_content_scope.len())));
                            }
                            if !ctx.meta_scope.is_empty() {
                                ops.push((index, ScopeStackOp::Pop(ctx.meta_scope.len())));
                            }
                        }
                        for scope in cur_context.meta_scope.iter() {
                            ops.push((index, ScopeStackOp::Push(*scope)));
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

                        // `pop: N + set:` (set_pop_count > 1) is **stacking**
                        // per ST docs: the trigger token receives the popped
                        // frames' meta_scope, and the deeper-pop happens
                        // AFTER the trigger records its scope. Mirror the
                        // `MatchOperation::Pop` arm: pop each deeper frame's
                        // mcs+ms in top-to-bottom order, then Restore that
                        // frame's `clear_scopes` if any. Without the
                        // per-depth Restore, atoms cleared by a deeper
                        // frame stay in clear_stack out of reach, and the
                        // per-target Clear below then bites one atom too
                        // deep. Observed on Python regex inside a
                        // `r'''(?ix:...)` triple-quoted string: the
                        // activate-x-mode `pop: 3 + set:[...]` left the
                        // outer's cleared `meta.mode.extended.regexp` in
                        // clear_stack; the inner's `clear_scopes: 1` then
                        // cleared `source.regexp.python` instead.
                        //
                        // Always emits for Set-with-pop. The Push-with-pop
                        // (lookahead) path popped the deeper frames in the
                        // initial phase already; it doesn't reach this arm
                        // because `is_set` is false there.
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
                // `pop: N + embed:` is **lookahead** per ST: the trigger
                // token must NOT inherit the popped frames' meta_scope.
                // Route through `Push { pop_count }` so the existing
                // Push-with-pop lookahead path in `push_meta_ops` /
                // `perform_op` handles deeper-pop emission. The
                // pre-recursive Pops below cover the embed-specific
                // cur_context meta_scope suppression (depth 0); the
                // Push-with-pop deeper-pop loop covers depths 1..N.
                let synthetic = MatchOperation::Push {
                    ctx_refs: contexts.clone(),
                    pop_count,
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
                // Branch acts like Push for meta ops purposes — `pop:N + branch:`
                // is lookahead per ST, same as `pop:N + push:`. At exec time,
                // Branch is transformed into a synthetic Push before calling
                // push_meta_ops, so this arm is a safety fallback.
                let synthetic = MatchOperation::Push {
                    ctx_refs: alternatives.clone(),
                    pop_count,
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
    pub(super) fn perform_op(
        &mut self,
        line: &str,
        regions: &Region,
        pat: &MatchPattern,
        syntax_set: &SyntaxSet,
    ) -> Result<bool, ParsingError> {
        let (ctx_refs, old_proto_ids, is_embed) = match pat.operation {
            MatchOperation::Push {
                ref ctx_refs,
                pop_count,
            } => {
                // `pop: N + push:` (pop_count > 0): pop N frames from the
                // runtime stack before the push loop, mirroring Set's arm.
                // The topmost popped frame's prototypes carry over so a
                // `with_prototype` from the leaving context stays active on
                // the new push (same convention as Set).
                let old_proto_ids = if pop_count > 0 {
                    let topmost = self.stack.pop().map(|s| s.prototypes);
                    for _ in 1..pop_count {
                        self.stack.pop();
                    }
                    topmost
                } else {
                    None
                };
                if pop_count > 0 {
                    let final_len = self.stack.len() + ctx_refs.len();
                    self.branch_points
                        .retain(|bp| final_len > bp.stack_depth.saturating_sub(bp.pop_count));
                    self.escape_stack.retain(|e| e.stack_depth < final_len);
                }
                (ctx_refs, old_proto_ids, false)
            }
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
}
