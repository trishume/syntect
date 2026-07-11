//! Native `embed`/`escape` support: escape execution and the regex
//! escaping used for escape backref resolution.

use super::*;

impl ParseState {
    /// Execute an escape match: apply escape_captures, pop stack down to
    /// the embed's stack_depth, and remove the escape entry.
    pub(super) fn exec_escape(
        &mut self,
        escape_idx: usize,
        match_start: usize,
        _match_end: usize,
        regions: &Region,
        syntax_set: &SyntaxSet,
        ops: &mut Vec<(usize, ScopeStackOp)>,
    ) -> Result<(), ParsingError> {
        let entry = &self.core.escape_stack[escape_idx];
        let target_depth = entry.stack_depth;
        let escape_captures = entry.captures.clone();

        // Drain orphan scope atoms left on the consumer's scope stack by
        // a prior cross-line replay whose later same-line fails
        // truncated the owning context out of `self.core.stack` — the Push
        // was committed to `flushed_ops` and can't be unwound by
        // `ops.truncate`, so we emit a balancing Pop here. Without this,
        // e.g. LaTeX `\end{lstlisting}` leaves
        // `meta.environment.verbatim.lstlisting.latex` on the stack
        // because a speculative `meta.path.java` atom pushed inside the
        // embedded Java shifts every subsequent Pop by one.
        //
        // The drain also rebalances ordinary v2 `embed_scope` escapes:
        // the embedded syntax's top-level scope is pushed at embed time
        // yet excluded from the pop-side model below, so the consumer
        // systematically holds one more atom than `expected_depth`.
        //
        // The consumer's actual depth is engine-specific. The legacy
        // engine mirrors it in `shadow` (end-of-prior-line consumer
        // stack) plus the ops emitted so far on the current line. The
        // trail engine reconstructs it exactly from the committed stack
        // at the window base plus the window's provisional ops.
        #[cfg(not(feature = "trail-engine"))]
        let consumer_depth = {
            let mut current_shadow = self.shadow.clone();
            for (_, op) in ops.iter() {
                let _ = current_shadow.apply(op);
            }
            current_shadow.as_slice().len()
        };
        #[cfg(feature = "trail-engine")]
        let consumer_depth = self.window_consumer_depth(ops);
        let expected_depth: usize = {
            let mut total = 0usize;
            let mut prev_embed_scope_replaces = false;
            for lvl in &self.core.stack {
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
        while self.core.stack.len() > target_depth {
            let level = &self.core.stack[self.core.stack.len() - 1];
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
                let skip = self.core.stack.len() >= 2
                    && syntax_set
                        .get_context(&self.core.stack[self.core.stack.len() - 2].context)
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

            self.core.stack.pop();
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
        self.core.escape_stack.truncate(escape_idx);

        // Invalidate branch points whose alt frame is no longer on the
        // stack. This mirrors the `alt frame still present` predicate used
        // at the other five BP-prune sites (handle_fail's late guard plus
        // the Push/Set/Embed/Pop retain calls): subtracting the bp's own
        // pop_count is necessary so a `pop: N + branch_point` whose
        // snapshot captures the pre-pop depth doesn't false-prune itself.
        // (Trail engine: the post-token `prune_dead_branches` hook covers
        // this site.)
        #[cfg(not(feature = "trail-engine"))]
        {
            let stack_len = self.core.stack.len();
            self.branch_points
                .retain(|bp| stack_len > bp.stack_depth.saturating_sub(bp.pop_count));
        }

        Ok(())
    }
}

/// Escape a string for use in regex substitution (re-export for use in escape resolution).
pub(super) fn escape_str(s: &str) -> String {
    escape(s)
}
