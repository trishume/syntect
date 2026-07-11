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

        Ok(())
    }
}

/// Escape a string for use in regex substitution (re-export for use in escape resolution).
pub(super) fn escape_str(s: &str) -> String {
    escape(s)
}
