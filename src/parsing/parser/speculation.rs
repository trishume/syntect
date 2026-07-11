//! Branch-point speculation: corrected-op arbitration after a `fail`
//! (`merge_flushed`, `prefer_inner_replay_corrections`) and the
//! backtracking entry point itself (`handle_fail`).

use super::*;

impl ParseState {
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
    /// `new_per_slot_bp` records the BP attribution for each new slot;
    /// stored per slot in `flushed_ops_bp_per_slot` so subsequent merges
    /// can compare against the BP that owns each individual slot rather
    /// than a single BP for the whole buffer. When two cross-line fails
    /// on the same `parse_line` both contribute slots, each slot's BP
    /// reflects which fail wrote it. When `prefer_inner_replay_corrections`
    /// substituted a slot from inner replay, that slot's `BpInfo`
    /// surfaces the inner producer via `inner_producer` — finer-grained
    /// attribution than rolling everything up to the outer BP.
    fn merge_flushed(
        &mut self,
        snap: usize,
        new_ops: Vec<Vec<(usize, ScopeStackOp)>>,
        new_per_slot_bp: Vec<BpInfo>,
    ) {
        let new_len = new_ops.len();
        debug_assert_eq!(new_len, new_per_slot_bp.len());
        // For SnapLeStart's per-slot line-number discriminator and
        // for trace events, we need the new BP's line_number — outer
        // attribution is uniform across the call (slots may carry
        // distinct inner_producers but share the outer's line/depth),
        // so reading slot 0 is representative. Fall back to `0` for
        // an empty merge (no slots to compare against anyway).
        let new_bp_line_number = new_per_slot_bp.first().map(|b| b.line_number).unwrap_or(0);
        match self.flushed_ops_start {
            None => {
                self.flushed_ops = new_ops;
                self.flushed_ops_start = Some(snap);
                self.flushed_ops_bp_per_slot = new_per_slot_bp;
            }
            Some(start) if snap <= start => {
                // Per-slot line-number discriminator: a prior slot
                // established by a BP from a later line is finer-grained
                // than the wrapping earlier-line retry that's now
                // overwriting. Preserve such slots; let equal/earlier-line
                // priors fall through to the overwrite (peer-BP arbitration
                // at the same line, or a fresher finding).
                let prior_len = self.flushed_ops.len();
                let overlap_end = (start + prior_len).min(snap + new_len);
                let mut composed_ops = new_ops;
                let mut composed_bp = new_per_slot_bp;
                for abs in start..overlap_end {
                    let prior_idx = abs - start;
                    let new_idx = abs - snap;
                    if self.flushed_ops_bp_per_slot[prior_idx].line_number > new_bp_line_number {
                        composed_ops[new_idx] = std::mem::take(&mut self.flushed_ops[prior_idx]);
                        composed_bp[new_idx] = self.flushed_ops_bp_per_slot[prior_idx].clone();
                    }
                }
                self.flushed_ops = composed_ops;
                self.flushed_ops_start = Some(snap);
                self.flushed_ops_bp_per_slot = composed_bp;
            }
            Some(start) => {
                // Per-slot effective-depth discriminator in the
                // [snap..overlap_end) range. A prior slot's effective
                // producer depth is the max of its own `stack_depth`
                // and any recursive `inner_producer` chain; preserve
                // the prior when its effective depth strictly exceeds
                // the new BP's. Cluster A's killing inner-most
                // `groups(d=4,l=3)` overwrite of `AQI(d=5,l=1)` slots
                // (eff prior=5 > new=4) gets preserved here; rolled-up
                // later merges keep firing because the surviving
                // slots' `inner_producer` chains carry the AQI depth
                // forward
                // (`cross_line_chained_fail_pushes_target_meta_scope_on_continuation_line`,
                // whose doc comment carries the probe rationale).
                let prior_ops = std::mem::take(&mut self.flushed_ops);
                let prior_bp = std::mem::take(&mut self.flushed_ops_bp_per_slot);
                let prior_len = prior_ops.len();
                let keep = snap - start;
                let overlap_end = (start + prior_len).min(snap + new_len);
                let overlap_count = overlap_end.saturating_sub(snap);
                let mut composed_ops: Vec<Vec<(usize, ScopeStackOp)>> =
                    Vec::with_capacity(keep + new_len);
                let mut composed_bp: Vec<BpInfo> = Vec::with_capacity(keep + new_len);
                let mut prior_ops_iter = prior_ops.into_iter();
                let mut prior_bp_iter = prior_bp.into_iter();
                let mut new_ops_iter = new_ops.into_iter();
                let mut new_bp_iter = new_per_slot_bp.into_iter();
                for _ in 0..keep {
                    composed_ops.push(prior_ops_iter.next().unwrap());
                    composed_bp.push(prior_bp_iter.next().unwrap());
                }
                for _ in 0..overlap_count {
                    let new_op = new_ops_iter.next().unwrap();
                    let new_bp = new_bp_iter.next().unwrap();
                    let prior_op = prior_ops_iter.next().unwrap();
                    let prior_bp_slot = prior_bp_iter.next().unwrap();
                    if Self::effective_producer_depth(&prior_bp_slot)
                        > Self::effective_producer_depth(&new_bp)
                    {
                        composed_ops.push(prior_op);
                        composed_bp.push(prior_bp_slot);
                    } else {
                        composed_ops.push(new_op);
                        composed_bp.push(new_bp);
                    }
                }
                composed_ops.extend(new_ops_iter);
                composed_bp.extend(new_bp_iter);
                self.flushed_ops = composed_ops;
                // SnapGtStart preserves the prior buffer's start
                // index — `keep` prior slots remain before the
                // overlap/new tail. Original truncate-and-extend
                // didn't touch `flushed_ops_start` either.
                self.flushed_ops_start = Some(start);
                self.flushed_ops_bp_per_slot = composed_bp;
            }
        }
    }

    fn effective_producer_depth(bp: &BpInfo) -> usize {
        let mut d = bp.stack_depth;
        let mut cur = bp.inner_producer.as_deref();
        while let Some(c) = cur {
            if c.stack_depth > d {
                d = c.stack_depth;
            }
            cur = c.inner_producer.as_deref();
        }
        d
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
        &mut self,
        outer_snap: usize,
        replayed_ops: &mut [Vec<(usize, ScopeStackOp)>],
        replayed_per_slot_bp: &mut [BpInfo],
        inner_ops: &[Vec<(usize, ScopeStackOp)>],
        inner_start: usize,
        outer_bp: &BpInfo,
        inner_bp: Option<&BpInfo>,
        inner_corrections_bp: &[BpInfo],
    ) {
        // Two regimes:
        //
        // 1. Inner at outer's exact depth (sibling resolution of the
        //    same branch family) or exactly one deeper (immediate
        //    refinement, e.g. outer=declarations(3),
        //    inner=annotation-identifier(4) on the same line — inner
        //    brings `meta.path.java` from the qualified-identifier alt
        //    that outer's locally-computed parse drops). Always
        //    substitute.
        //
        // 2. Inner more than one deeper (multigen16-style: outer
        //    class-members(4), inner object-type(11)). Inner is
        //    nested INSIDE outer's resolved alt, and its reparse may
        //    push atoms outer's alt already provides — substituting
        //    blindly would double them
        //    (`deeper_inner_bp_correction_does_not_double_outer_meta_scope`).
        //    Substitute only when the inner's ops are an `immediately-
        //    pop`-style tail-extension of outer's: identical prefix +
        //    one or more `Pop` ops appended at positions outer
        //    already covers. That's what an `immediately-pop`-alt
        //    failover commits at the inner BP's trigger position
        //    when outer's per-line replay terminated before the
        //    nested fail fired (cluster-C
        //    `multi_line_annotation_pops_meta_scope_at_eol`).
        //
        // Inner shallower than outer is also skipped: such inners
        // are fresh top-level-style BPs whose commitment is
        // structurally less specific than outer's and would overwrite
        // outer's correct refined parse with a coarser one.
        let depth_diff =
            inner_bp.map(|inner| inner.stack_depth as isize - outer_bp.stack_depth as isize);
        let in_depth_window = matches!(depth_diff, Some(0) | Some(1));
        let allow_deep_extension = matches!(depth_diff, Some(d) if d > 1);
        debug_assert_eq!(replayed_ops.len(), replayed_per_slot_bp.len());
        // Helper closure: build the per-slot BpInfo recorded for a
        // slot we just substituted from inner replay. Outer attribution
        // is preserved (so subsequent line/depth comparisons against
        // the outer wrapping BP keep working) and the inner producer
        // is recorded under `inner_producer` for finer-grained
        // discrimination at the next merge. `slot_inner_bp` already
        // carries any deeper nested attribution recursively.
        let attribute_substituted = |slot_inner_bp: Option<&BpInfo>| -> BpInfo {
            let producer = slot_inner_bp
                .cloned()
                .or_else(|| inner_bp.cloned())
                .expect("substitution path requires an inner producer");
            BpInfo {
                name: outer_bp.name.clone(),
                stack_depth: outer_bp.stack_depth,
                line_number: outer_bp.line_number,
                inner_producer: Some(Box::new(producer)),
            }
        };
        for (i, (outer_local, slot_bp_attr)) in replayed_ops
            .iter_mut()
            .zip(replayed_per_slot_bp.iter_mut())
            .enumerate()
        {
            let global_i = outer_snap + i;
            if global_i < inner_start {
                continue;
            }
            let inner_idx = global_i - inner_start;
            let slot_inner_bp = inner_corrections_bp.get(inner_idx);
            // Per-slot line-number escape valve: an inner correction
            // whose BP was created on a later line than outer's wrapping
            // BP carries finer-grained context that outer's recompute
            // can't reproduce. Substitute regardless of depth.
            let slot_line_overrides = matches!(
                slot_inner_bp,
                Some(sib) if sib.line_number > outer_bp.line_number
            );
            if let Some(corrected) = inner_ops.get(inner_idx) {
                if in_depth_window {
                    *outer_local = corrected.clone();
                    *slot_bp_attr = attribute_substituted(slot_inner_bp);
                } else if slot_line_overrides {
                    *outer_local = corrected.clone();
                    *slot_bp_attr = attribute_substituted(slot_inner_bp);
                } else if allow_deep_extension && Self::inner_extends_outer(outer_local, corrected)
                {
                    *outer_local = corrected.clone();
                    *slot_bp_attr = attribute_substituted(slot_inner_bp);
                } else {
                    // Iter 7 substitution path: when SkippedDeepNonExtension
                    // would fire and `is_replace_shape` matches, substitute
                    // outer_local with inner's corrected ops. If the
                    // substitution would over-push a `meta.*` atom not
                    // already on the running shadow stack (G2 discriminator
                    // — deferral rationale in the iter-7 notes of
                    // `cross_line_alternative_replacement_substitution_does_not_double_meta_scope`),
                    // append a
                    // compensating Pop(1) immediately after each over-pushed
                    // Push so the consumer's stack mirror sees the
                    // intended single push without doubling.
                    let (_, outer_first, inner_first) =
                        Self::ops_divergence(outer_local, corrected);
                    let outer_meta = Self::collect_meta_scopes(outer_local);
                    let inner_meta = Self::collect_meta_scopes(corrected);
                    let would_be_skipped_deep =
                        inner_bp.is_some() && !matches!(depth_diff, Some(d) if d < 0);
                    let do_substitute = would_be_skipped_deep
                        && Self::is_replace_shape(
                            &outer_first,
                            &inner_first,
                            &outer_meta,
                            &inner_meta,
                        );
                    if do_substitute {
                        let outer_pre_substitution = outer_local.clone();
                        *outer_local = corrected.clone();
                        *slot_bp_attr = attribute_substituted(slot_inner_bp);
                        // Per-meta atom over-push: positive entries of
                        // (inner_net - outer_net). When non-empty, inner
                        // pushes a meta atom that outer doesn't.
                        let outer_net = Self::net_meta_delta(&outer_pre_substitution);
                        let inner_net = Self::net_meta_delta(corrected);
                        let mut over_push: HashMap<String, usize> = HashMap::new();
                        for (atom, inner_c) in &inner_net {
                            let outer_c = outer_net.get(atom).copied().unwrap_or(0);
                            if *inner_c > outer_c {
                                over_push.insert(atom.clone(), inner_c - outer_c);
                            }
                        }
                        // G2 gate: skip comp-pop when any over-push atom
                        // is already on the running shadow stack — that
                        // signals inner is legitimately preserving an
                        // existing meta scope (not introducing a fresh
                        // one), so popping it would break the consumer.
                        if !over_push.is_empty() {
                            let would_double = self.shadow.as_slice().iter().any(|s| {
                                let n = s.build_string();
                                n.starts_with("meta.") && over_push.contains_key(&n)
                            });
                            if !would_double {
                                let total_extra: usize = over_push.values().sum();
                                let mut remaining = over_push;
                                let mut filtered: Vec<(usize, ScopeStackOp)> =
                                    Vec::with_capacity(outer_local.len() + total_extra);
                                for entry in outer_local.iter() {
                                    filtered.push(entry.clone());
                                    if let (pos, ScopeStackOp::Push(s)) = (entry.0, &entry.1) {
                                        let name = s.build_string();
                                        if let Some(rem) = remaining.get_mut(&name) {
                                            if *rem > 0 {
                                                filtered.push((pos, ScopeStackOp::Pop(1)));
                                                *rem -= 1;
                                            }
                                        }
                                    }
                                }
                                *outer_local = filtered;
                            }
                        }
                        // Iter 9 deferred sub-B's outer-side comp-pop:
                        // iter-8 analysis flagged outer over-pushes
                        // (`meta.function.*` family inner correctly
                        // omits) but the symmetric predicate widening
                        // surfaced inner-BP stack-base misalignment
                        // first, so the widening stays deferred;
                        // `is_replace_shape` documents the predicate
                        // that did land.
                    } else {
                    }
                }
            } else {
            }
        }
    }

    /// Length of the longest equal prefix between `outer` and `inner`,
    /// plus a summary of each side's first op past that prefix (or
    /// `None` if that side is exhausted at the prefix boundary). Used
    /// by Iter 7's `is_replace_shape` substitution predicate (production)
    /// and the `SubstitutionShapeAnalysis` trace event (debug).
    fn ops_divergence(
        outer: &[(usize, ScopeStackOp)],
        inner: &[(usize, ScopeStackOp)],
    ) -> (usize, Option<OpSummary>, Option<OpSummary>) {
        let mut prefix = 0usize;
        let max = outer.len().min(inner.len());
        while prefix < max && outer[prefix] == inner[prefix] {
            prefix += 1;
        }
        let outer_div = outer.get(prefix).map(Self::summarize_op);
        let inner_div = inner.get(prefix).map(Self::summarize_op);
        (prefix, outer_div, inner_div)
    }

    /// Collect the names of every `Push(scope)` whose scope name
    /// begins with `meta.`, in occurrence order. These are the
    /// meta-scope frames the ops would push onto the actual scope
    /// stack when applied. Used by Iter 7's substitution predicate
    /// (`is_replace_shape`) and the `SubstitutionShapeAnalysis` trace.
    fn collect_meta_scopes(ops: &[(usize, ScopeStackOp)]) -> Vec<String> {
        let mut out = Vec::new();
        for (_, op) in ops {
            if let ScopeStackOp::Push(scope) = op {
                let s = scope.build_string();
                if s.starts_with("meta.") {
                    out.push(s);
                }
            }
        }
        out
    }

    fn summarize_op(entry: &(usize, ScopeStackOp)) -> OpSummary {
        let (pos, op) = entry;
        let (kind, scope) = match op {
            ScopeStackOp::Push(s) => ("push", s.build_string()),
            ScopeStackOp::Pop(_) => ("pop", String::new()),
            ScopeStackOp::Clear(_) => ("clear", String::new()),
            ScopeStackOp::Restore => ("restore", String::new()),
            ScopeStackOp::Noop => ("noop", String::new()),
        };
        OpSummary {
            pos: *pos,
            kind,
            scope,
        }
    }

    /// True when both outer and inner have at least one diverging op past their common prefix
    /// and `inner_meta_scopes` contains a `meta.*` atom not in `outer_meta_scopes`. Used to
    /// gate the substitution path in `prefer_inner_replay_corrections`.
    fn is_replace_shape(
        outer_div: &Option<OpSummary>,
        inner_div: &Option<OpSummary>,
        outer_meta_scopes: &[String],
        inner_meta_scopes: &[String],
    ) -> bool {
        if outer_div.is_none() || inner_div.is_none() {
            return false;
        }
        inner_meta_scopes
            .iter()
            .any(|s| s.starts_with("meta.") && !outer_meta_scopes.contains(s))
    }

    /// Simulate `ops` on a fresh scope-name stack and return the multiset of `meta.*` atoms
    /// remaining at the end. `Push`/`Pop(N)` are simulated literally; other op variants are
    /// treated as no-ops. Used to compute the per-atom net push delta between outer and inner
    /// ops at the comp-pop site in `prefer_inner_replay_corrections`.
    fn net_meta_delta(ops: &[(usize, ScopeStackOp)]) -> HashMap<String, usize> {
        let mut sim_stack: Vec<String> = Vec::new();
        for (_, op) in ops {
            match op {
                ScopeStackOp::Push(s) => sim_stack.push(s.build_string()),
                ScopeStackOp::Pop(n) => {
                    for _ in 0..*n {
                        sim_stack.pop();
                    }
                }
                _ => {}
            }
        }
        let mut counts: HashMap<String, usize> = HashMap::new();
        for s in &sim_stack {
            if s.starts_with("meta.") {
                *counts.entry(s.clone()).or_insert(0) += 1;
            }
        }
        counts
    }

    /// True when `inner` starts with the entire `outer` op sequence and
    /// the trailing extension contains only `Pop` ops at positions
    /// `outer` already covers (i.e. the inner reparse adds end-of-line
    /// pops that the outer's locally-computed parse did not emit, and
    /// nothing else).
    fn inner_extends_outer(
        outer: &[(usize, ScopeStackOp)],
        inner: &[(usize, ScopeStackOp)],
    ) -> bool {
        if inner.len() <= outer.len() {
            return false;
        }
        if inner[..outer.len()] != *outer {
            return false;
        }
        let max_pos = outer.last().map(|(p, _)| *p).unwrap_or(0);
        inner[outer.len()..]
            .iter()
            .all(|(pos, op)| *pos <= max_pos && matches!(op, ScopeStackOp::Pop(_)))
    }

    /// Iter-19 discriminator: should cross-line
    /// `handle_fail` rewind to BP-creation snapshot and force alt 5 on
    /// re-entry? Composite signal — `class-members` name gate, not-
    /// already-alt-5 loop guard, empty inner corrections, max-depth BP
    /// strictly deeper than outer, and the first replayed slot's ops
    /// contain a Push of `meta.function.return-type.java`. Iter-18's
    /// per-(`bp_line`, alt) matrix witness: TRUE at row 5 (`@ 3394`
    /// BENEFICIAL) + 2 safe-neutrals (2976, 2982); FALSE at all 12
    /// harmful method-decl sites at all alts in `syntax_test_java.java`.
    fn class_members_alt5_should_rewind(
        outer_bp_info: &BpInfo,
        next_alt_index: usize,
        inner_corrections_empty: bool,
        inner_replay_max: &MaxDepthSeen,
        replayed_ops: &[Vec<(usize, ScopeStackOp)>],
        return_type_scope: Scope,
    ) -> bool {
        if outer_bp_info.name != "class-members" {
            return false;
        }
        if next_alt_index >= 5 {
            return false;
        }
        if !inner_corrections_empty {
            return false;
        }
        let max_bp = match inner_replay_max.bp.as_ref() {
            Some(bp) => bp,
            None => return false,
        };
        if max_bp.stack_depth <= outer_bp_info.stack_depth {
            return false;
        }
        let first_slot = match replayed_ops.first() {
            Some(slot) => slot,
            None => return false,
        };
        first_slot.iter().any(|(_, op)| match op {
            ScopeStackOp::Push(s) => return_type_scope.is_prefix_of(*s),
            _ => false,
        })
    }

    /// Handle a `fail` operation by rewinding to the named branch point.
    /// Returns Ok(true) if backtracking happened (caller should continue from rewound position).
    /// Returns Ok(false) if the fail had no effect.
    pub(super) fn handle_fail(
        &mut self,
        name: &str,
        line: &str,
        start: &mut usize,
        non_consuming_push_at: &mut (usize, usize, usize),
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
                inner_producer: None,
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
                let saved_flushed_bp = std::mem::take(&mut self.flushed_ops_bp_per_slot);
                // Install a fresh max-depth tracker for the duration of
                // this inner replay; restore the outer tracker (if any)
                // when we're done. Mirrors `saved_flushed`'s nesting.
                let saved_max_depth = self.inner_replay_max_depth.replace(MaxDepthSeen::default());
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
                let inner_corrections_bp = std::mem::take(&mut self.flushed_ops_bp_per_slot);
                self.flushed_ops = saved_flushed;
                self.flushed_ops_start = saved_flushed_start;
                self.flushed_ops_bp_per_slot = saved_flushed_bp;
                let _inner_replay_max =
                    std::mem::take(&mut self.inner_replay_max_depth).unwrap_or_default();
                self.inner_replay_max_depth = saved_max_depth;
                let mut replayed_per_slot_bp: Vec<BpInfo> =
                    vec![outer_bp_info.clone(); replayed_ops.len()];
                if let Some(start) = inner_corrections_start {
                    if !inner_corrections.is_empty() {
                        let inner_bp_first = inner_corrections_bp.first().cloned();
                        self.prefer_inner_replay_corrections(
                            pending_lines_snapshot_len,
                            &mut replayed_ops,
                            &mut replayed_per_slot_bp,
                            &inner_corrections,
                            start,
                            &outer_bp_info,
                            inner_bp_first.as_ref(),
                            &inner_corrections_bp,
                        );
                    }
                }
                self.merge_flushed(
                    pending_lines_snapshot_len,
                    replayed_ops,
                    replayed_per_slot_bp,
                );

                // Restart the current line from the beginning under the
                // restored state.
                ops.clear();
                *start = 0;
                *non_consuming_push_at = (0, 0, 0);
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
            inner_producer: None,
        };
        // bp borrow ends here.

        let pop_count = self.branch_points[bp_index].pop_count;

        // Restore parser state to the snapshot.
        // Keep `stack_snapshot` available — the same-line fix below needs
        // it to compute popped-context meta_scope clearance via
        // `push_meta_ops`.
        self.stack = stack_snapshot.clone();
        self.proto_starts = proto_starts_snapshot.clone();
        self.escape_stack = escape_stack_snapshot.clone();
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
            let synthetic_op_alt_n = MatchOperation::Push {
                ctx_refs: vec![next_alt.clone()],
                pop_count,
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

            // Snapshot the branch_points so the post-replay
            // `class_members_alt5_should_rewind` discriminator can restore them
            // for the inline alt-5 re-run below. The captured `next_alternative`
            // is overwritten to 5 inside the rewind block, so its value here
            // doesn't matter.
            let branch_points_snapshot_for_rewind: Vec<BranchPoint> = self.branch_points.clone();

            // Save prior flushed_ops state and clear it so any nested
            // cross-line fails firing during the replay loop below write
            // into a clean slot we can detect afterward.
            let saved_flushed = std::mem::take(&mut self.flushed_ops);
            let saved_flushed_start = self.flushed_ops_start.take();
            let saved_flushed_bp = std::mem::take(&mut self.flushed_ops_bp_per_slot);
            // Install a fresh max-depth tracker for the duration of
            // this inner replay; restore the outer tracker (if any)
            // when we're done. Mirrors `saved_flushed`'s nesting.
            let saved_max_depth = self.inner_replay_max_depth.replace(MaxDepthSeen::default());

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
            let mut inner_corrections = std::mem::take(&mut self.flushed_ops);
            let mut inner_corrections_start = self.flushed_ops_start.take();
            let mut inner_corrections_bp = std::mem::take(&mut self.flushed_ops_bp_per_slot);
            self.flushed_ops = saved_flushed;
            self.flushed_ops_start = saved_flushed_start;
            self.flushed_ops_bp_per_slot = saved_flushed_bp;
            let mut _inner_replay_max =
                std::mem::take(&mut self.inner_replay_max_depth).unwrap_or_default();
            self.inner_replay_max_depth = saved_max_depth;

            // Composite-discriminator rewind: when the cross-line `class-members` BP
            // exhausts an earlier alt without inner corrections and the inner replay reached
            // a strictly-deeper BP under a `meta.function.return-type.java` push, force alt 5
            // (`member-maybe-field`) by inlining its setup + replay loop here. The
            // `next_alt_index < 5` short-circuit inside the discriminator prevents a re-fire
            // after this block runs. The naive "set `next_alternative = 5` and let the caller
            // re-trigger" variant doesn't work — the caller's re-parse doesn't re-encounter
            // the same BP at the same byte position, so the target alt's replay never runs.
            let return_type_scope =
                Scope::new("meta.function.return-type.java").expect("well-formed scope");
            if Self::class_members_alt5_should_rewind(
                &outer_bp_info,
                next_alt_index,
                inner_corrections.is_empty(),
                &_inner_replay_max,
                &replayed_ops,
                return_type_scope,
            ) {
                // Restore parser state to BP-creation snapshot. The
                // `branch_points` snapshot has the BP at next_alternative
                // = next_alt_index + 1 (post-line-2511 advance); we
                // overwrite to 6 below to record alt 5 as just tried.
                self.branch_points = branch_points_snapshot_for_rewind;
                self.stack = stack_snapshot.clone();
                self.proto_starts = proto_starts_snapshot.clone();
                self.escape_stack = escape_stack_snapshot.clone();
                self.first_line = first_line_snapshot;
                *non_consuming_push_at = non_consuming_push_at_snapshot;

                // Mirror line ~2511's pattern: advance past alt 5 so a
                // future fail on this BP goes to the exhaustion path
                // (alt 5 has been tried by the inline re-run below).
                self.branch_points[bp_index].next_alternative = 5 + 1;

                // Re-execute the alt-cycle setup for alt 5 (mirrors
                // lines ~2495-2508 but with alt index 5 instead of N).
                let next_alt_5 = self.branch_points[bp_index].alternatives[5].clone();
                if pop_count > 0 {
                    for _ in 0..pop_count {
                        self.stack.pop();
                    }
                }
                let with_prototype_5 = self.branch_points[bp_index].with_prototype.clone();
                let context_id_5 = next_alt_5.id()?;
                let proto_ids_5 = match with_prototype_5 {
                    Some(ref p) => vec![p.id()?],
                    None => Vec::new(),
                };
                self.stack.push(StateLevel {
                    context: context_id_5,
                    prototypes: proto_ids_5,
                    captures: None,
                });

                // Re-build first_line_prefix for alt 5 (mirrors lines
                // ~2611-2632).
                let mut first_line_prefix_5 = prefix_ops.clone();
                let synthetic_op_alt_5 = MatchOperation::Push {
                    ctx_refs: vec![next_alt_5.clone()],
                    pop_count,
                };
                let level_ctx_id_5 = stack_snapshot.last().map(|l| l.context);
                let post_set_stack_5 = std::mem::replace(&mut self.stack, stack_snapshot.clone());
                if let Some(level_ctx_id) = level_ctx_id_5 {
                    let level_context = syntax_set.get_context(&level_ctx_id)?;
                    self.push_meta_ops(
                        true,
                        trigger_match_start,
                        level_context,
                        &synthetic_op_alt_5,
                        syntax_set,
                        &mut first_line_prefix_5,
                    )?;
                    for scope in &trigger_pat_scope {
                        first_line_prefix_5.push((trigger_match_start, ScopeStackOp::Push(*scope)));
                    }
                    first_line_prefix_5.extend(trigger_capture_ops.iter().cloned());
                    if !trigger_pat_scope.is_empty() {
                        first_line_prefix_5
                            .push((match_start_pos, ScopeStackOp::Pop(trigger_pat_scope.len())));
                    }
                    self.push_meta_ops(
                        false,
                        match_start_pos,
                        level_context,
                        &synthetic_op_alt_5,
                        syntax_set,
                        &mut first_line_prefix_5,
                    )?;
                }
                self.stack = post_set_stack_5;

                // Re-run inner replay loop with alt 5 (mirrors lines
                // ~2649-2693).
                let saved_flushed_5 = std::mem::take(&mut self.flushed_ops);
                let saved_flushed_start_5 = self.flushed_ops_start.take();
                let saved_flushed_bp_5 = std::mem::take(&mut self.flushed_ops_bp_per_slot);
                let saved_max_depth_5 =
                    self.inner_replay_max_depth.replace(MaxDepthSeen::default());

                let mut replayed_ops_5: Vec<Vec<(usize, ScopeStackOp)>> =
                    Vec::with_capacity(truncated_lines.len());
                for (i, replay_line) in truncated_lines.iter().enumerate() {
                    let prev_replay_ctx = self.replay_ctx.replace(ReplayCtx {
                        line_number: bp_line_number + i,
                        pending_lines_snapshot_offset: pending_lines_snapshot_len + i,
                    });
                    let prev_replay_prefix = if i == 0 {
                        self.replay_prefix_ops.replace(first_line_prefix_5.clone())
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
                        let mut first_line_ops = first_line_prefix_5.clone();
                        first_line_ops.extend(tail_ops);
                        first_line_ops
                    } else {
                        tail_ops
                    };
                    replayed_ops_5.push(line_ops);
                }

                // Capture and restore (mirrors lines ~2705-2713).
                let inner_corrections_5 = std::mem::take(&mut self.flushed_ops);
                let inner_corrections_start_5 = self.flushed_ops_start.take();
                let inner_corrections_bp_5 = std::mem::take(&mut self.flushed_ops_bp_per_slot);
                self.flushed_ops = saved_flushed_5;
                self.flushed_ops_start = saved_flushed_start_5;
                self.flushed_ops_bp_per_slot = saved_flushed_bp_5;
                let inner_replay_max_5 =
                    std::mem::take(&mut self.inner_replay_max_depth).unwrap_or_default();
                self.inner_replay_max_depth = saved_max_depth_5;

                // Reassign outer locals so the merge code below sees
                // alt 5's results.
                replayed_ops = replayed_ops_5;
                inner_corrections = inner_corrections_5;
                inner_corrections_start = inner_corrections_start_5;
                inner_corrections_bp = inner_corrections_bp_5;
                _inner_replay_max = inner_replay_max_5;
            }

            let mut replayed_per_slot_bp: Vec<BpInfo> =
                vec![outer_bp_info.clone(); replayed_ops.len()];
            if let Some(start) = inner_corrections_start {
                if !inner_corrections.is_empty() {
                    let inner_bp_first = inner_corrections_bp.first().cloned();
                    self.prefer_inner_replay_corrections(
                        pending_lines_snapshot_len,
                        &mut replayed_ops,
                        &mut replayed_per_slot_bp,
                        &inner_corrections,
                        start,
                        &outer_bp_info,
                        inner_bp_first.as_ref(),
                        &inner_corrections_bp,
                    );
                }
            }
            self.merge_flushed(
                pending_lines_snapshot_len,
                replayed_ops,
                replayed_per_slot_bp,
            );

            // Restart the current line from the beginning.
            ops.clear();
            *start = 0;
            *non_consuming_push_at = (0, 0, 0);

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
            let synthetic_op_alt_n = MatchOperation::Push {
                ctx_refs: vec![next_alt.clone()],
                pop_count,
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
}
