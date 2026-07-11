//! Trail-based speculation engine (the default): checkpoint the
//! interpreter state before every branch decision, log the decision on a
//! trail, and handle `fail` by bumping the most recent live same-name
//! decision and deterministically re-executing the buffered window from
//! its checkpoint — instead of surgically correcting already-emitted ops
//! the way the legacy engine in `speculation.rs` does.
//!
//! The executor (`parse_next_token` and everything below it) is shared
//! with the legacy engine and operates on [`Core`]; the only trail-aware
//! points are branch-decision selection and `fail` handling.

use super::*;

/// Cap on checkpoint restarts within a single `parse_line` call, scaled
/// by the window size. Termination is guaranteed without it (every
/// restart lexicographically advances the finite decision trail), but
/// the worst case is exponential in the number of live decisions; the
/// budget converts a pathological syntax into a warning plus a committed
/// parse.
const RESTART_BUDGET_PER_WINDOW_LINE: usize = 64;

/// One branch decision: which alternative of the named `branch_point`
/// is currently on the stack, or `Refuse` when every alternative has
/// been exhausted and the branch pattern is suppressed at its trigger
/// position so the parent context's next rule fires (Sublime Text's
/// exhaustion semantics).
#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) enum Choice {
    Take(usize),
    Refuse,
}

/// Everything needed to resume execution from just before a branch
/// trigger match: the interpreter state plus the executor-local
/// bookkeeping of the line being executed. Restoring a checkpoint and
/// re-searching re-selects the branch trigger deterministically, so the
/// trigger's scopes, captures, and the chosen alternative's meta ops are
/// re-emitted through the ordinary `exec_pattern` path for *any*
/// alternative — nothing needs to be saved for manual re-emission.
#[derive(Debug, Clone, Eq, PartialEq)]
struct Checkpoint {
    core: Core,
    /// Window-line index the decision sits on.
    line_idx: usize,
    /// Cursor position the branch trigger was matched from.
    pos: usize,
    /// Length of the line's ops vec before the trigger emitted anything.
    ops_len: usize,
    non_consuming_push_at: (usize, usize, usize),
    /// Branch suppressions active at this point of the line (from
    /// earlier `Refuse` decisions at not-yet-passed positions).
    skipped_branches: Vec<(usize, String)>,
    /// Live-branch set at decision time, exclusive of this decision.
    live: Vec<LiveBranch>,
}

/// A recorded branch decision plus the checkpoint to restart from when
/// a `fail` bumps it to its next alternative.
#[derive(Debug, Clone, Eq, PartialEq)]
struct Decision {
    name: String,
    choice: Choice,
    num_alts: usize,
    /// Stack depth at creation (before the branch's own pops and the
    /// alternative's push).
    stack_depth: usize,
    /// `pop_count` of the branch operation; the alternative's frame
    /// lives at `stack_depth - pop_count + 1`.
    pop_count: usize,
    /// Absolute line number of creation, for 128-line expiry.
    created_line: usize,
    /// Whether the most recent bump came from a `fail` on the
    /// decision's own line. Drives the empty-line pop placement rule
    /// (the legacy engine's same-line fail path has the same
    /// distinction — its cross-line replay path never relocates the
    /// resume cursor).
    bumped_same_line: bool,
    checkpoint: Checkpoint,
}

/// A decision whose alternative frame is still on the stack — the only
/// decisions a `fail` can target. Dropping an entry finalizes the
/// decision (it stays on the trail for deterministic re-execution).
#[derive(Debug, Clone, Eq, PartialEq)]
struct LiveBranch {
    decision_idx: usize,
    name: String,
    stack_depth: usize,
    pop_count: usize,
    created_line: usize,
}

impl LiveBranch {
    /// The alternative's frame is still on the stack iff the stack is
    /// deeper than the pre-branch depth minus the branch's own pops —
    /// the same predicate the legacy engine uses at its prune sites.
    fn alive(&self, stack_len: usize) -> bool {
        stack_len > self.stack_depth.saturating_sub(self.pop_count)
    }
}

/// The engine state: the buffered window, its single truth stream of
/// per-line ops, and the decision trail.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub(super) struct TrailState {
    /// Absolute line number of `lines[0]`.
    base_line: usize,
    /// Buffered window: every line since the last commit point.
    lines: Vec<String>,
    /// Ops per window line — revised in place on re-execution.
    provisional: Vec<Vec<(usize, ScopeStackOp)>>,
    /// The decision log.
    trail: Vec<Decision>,
    /// Trail cursor during execution: a branch trigger encountered at
    /// the cursor consumes the recorded choice; past the end of the
    /// trail, new decisions are appended.
    cursor: usize,
    /// Decisions a `fail` can still target.
    live: Vec<LiveBranch>,
    /// Window-line index currently being executed.
    exec_line_idx: usize,
    /// Alternative selected by `decide_branch`, consumed by
    /// `exec_pattern`'s branch arm.
    pending_alt: Option<usize>,
    /// Set by `fail_branch`: decision index to restart from.
    pending_backtrack: Option<usize>,
    /// Restarts consumed by the current `parse_line` call.
    restarts: usize,
    /// The consumer's scope stack at the start of `lines[0]` — the fold
    /// of every op committed before the window. Together with the
    /// window's provisional ops this reconstructs the consumer's exact
    /// stack at any execution point (see `window_consumer_depth`),
    /// which `exec_escape` needs to rebalance v2 `embed_scope` atoms.
    base_shadow: ScopeStack,
}

impl ParseState {
    /// Parses a single line of the file. See [`ParseLineOutput`] for the
    /// output contract: `revised` replaces the whole uncommitted window
    /// whenever a cross-line restart re-executed earlier lines, and
    /// [`speculative_lines`] reports how many returned lines are still
    /// subject to revision.
    ///
    /// [`speculative_lines`]: ParseState::speculative_lines
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

        // Decisions older than 128 lines can no longer be rewound to:
        // finalize them.
        let cur_line = self.core.line_number;
        let warnings = &mut self.warnings;
        self.trail.live.retain(|b| {
            let alive = cur_line.saturating_sub(b.created_line) <= 128;
            if !alive {
                warnings.push(ParseWarning::BranchPointExpired {
                    name: b.name.clone(),
                });
            }
            alive
        });
        self.core.line_number += 1;
        let line_number_after = self.core.line_number;

        // No live decisions means no future `fail` can revise the
        // window: commit it and start a fresh one at the current line.
        if self.trail.live.is_empty() {
            for line_ops in &self.trail.provisional {
                for (_, op) in line_ops {
                    let _ = self.trail.base_shadow.apply(op);
                }
            }
            self.trail.lines.clear();
            self.trail.provisional.clear();
            self.trail.trail.clear();
            self.trail.base_line = cur_line;
        }
        self.trail.cursor = self.trail.trail.len();
        self.trail.restarts = 0;

        self.trail.lines.push(line.to_owned());
        self.trail.provisional.push(Vec::new());
        let cur_idx = self.trail.lines.len() - 1;
        let mut min_rewind = cur_idx;

        let mut from_line = cur_idx;
        let mut from_pos = 0usize;
        let mut from_ncpa = (0usize, 0usize, 0usize);

        loop {
            self.execute_window(from_line, from_pos, from_ncpa, syntax_set)?;
            let Some(idx) = self.trail.pending_backtrack.take() else {
                break;
            };
            let ck = self.trail.trail[idx].checkpoint.clone();
            self.core = ck.core;
            self.trail.live = ck.live;
            self.skipped_branches = ck.skipped_branches;
            self.trail.provisional[ck.line_idx].truncate(ck.ops_len);
            for slot in &mut self.trail.provisional[ck.line_idx + 1..] {
                slot.clear();
            }
            min_rewind = min_rewind.min(ck.line_idx);
            from_line = ck.line_idx;
            from_pos = ck.pos;
            from_ncpa = ck.non_consuming_push_at;
        }

        // Mid-window checkpoints carry the line counter of their
        // creation call; the lines-fed count must survive restores.
        self.core.line_number = line_number_after;

        let ops = self.trail.provisional[cur_idx].clone();
        // A restart that rewound past a line boundary re-executed earlier
        // window lines: hand the whole window back. Same-line restarts
        // only reshape `ops`, which is returned fresh anyway.
        let revised = if min_rewind < cur_idx {
            Some(self.trail.provisional[..cur_idx].to_vec())
        } else {
            None
        };
        let warnings = std::mem::take(&mut self.warnings);

        Ok(ParseLineOutput {
            ops,
            revised,
            warnings,
        })
    }

    /// Returns `true` when the parser is inside a `branch_point` and the
    /// result of `parse_line` may be revised by a future `fail` action.
    /// Once the branch resolves (or if no branch was entered), this returns
    /// `false` and all ops emitted so far are final.
    pub fn is_speculative(&self) -> bool {
        !self.trail.live.is_empty()
    }

    /// Number of most-recently-parsed lines (including the line of the
    /// latest `parse_line` call) whose ops may still be revised by a
    /// future call. `0` means every op returned so far is final — the
    /// natural boundary for caching a clone of this state or flushing
    /// buffered output.
    pub fn speculative_lines(&self) -> usize {
        if self.trail.live.is_empty() {
            0
        } else {
            self.trail.lines.len()
        }
    }

    /// Execute the window from `from_line` (starting mid-line at
    /// `from_pos` there, from line start everywhere after) through the
    /// last buffered line, or until a `fail` schedules a restart.
    fn execute_window(
        &mut self,
        from_line: usize,
        from_pos: usize,
        from_ncpa: (usize, usize, usize),
        syntax_set: &SyntaxSet,
    ) -> Result<(), ParsingError> {
        for i in from_line..self.trail.lines.len() {
            let line = self.trail.lines[i].clone();
            self.trail.exec_line_idx = i;
            let (start_at, ncpa) = if i == from_line {
                (from_pos, from_ncpa)
            } else {
                // Suppression entries are keyed to byte offsets of the
                // line they were created on.
                self.skipped_branches.clear();
                (0, (0, 0, 0))
            };
            let mut ops = std::mem::take(&mut self.trail.provisional[i]);
            self.execute_line_from(&line, syntax_set, start_at, ncpa, &mut ops)?;
            self.trail.provisional[i] = ops;
            if self.trail.pending_backtrack.is_some() {
                return Ok(());
            }
        }
        Ok(())
    }

    /// The per-line token loop: mirrors the legacy engine's
    /// `parse_line_inner_from`, plus checkpoint-aware resume (caller
    /// provides the ops prefix and the loop-guard state) and live-branch
    /// pruning after every token.
    fn execute_line_from(
        &mut self,
        line: &str,
        syntax_set: &SyntaxSet,
        start_at: usize,
        ncpa: (usize, usize, usize),
        res: &mut Vec<(usize, ScopeStackOp)>,
    ) -> Result<(), ParsingError> {
        let mut match_start = start_at;

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
        let mut non_consuming_push_at = ncpa;
        self.zero_width_escape_fires.clear();

        while self.parse_next_token(
            line,
            syntax_set,
            &mut match_start,
            &mut search_cache,
            &mut regions,
            &mut non_consuming_push_at,
            res,
        )? {
            if self.trail.pending_backtrack.is_some() {
                break;
            }
            self.prune_dead_branches();
        }
        self.prune_dead_branches();
        Ok(())
    }

    /// Depth of the consumer's scope stack at the current execution
    /// point: the committed stack at the window base plus every op the
    /// current execution has produced so far, up to and including `ops`
    /// (the in-progress line's vec). Deterministic given the trail
    /// prefix, so restarts recompute it identically.
    pub(super) fn window_consumer_depth(&self, ops: &[(usize, ScopeStackOp)]) -> usize {
        let mut stack = self.trail.base_shadow.clone();
        for line_ops in &self.trail.provisional[..self.trail.exec_line_idx] {
            for (_, op) in line_ops {
                let _ = stack.apply(op);
            }
        }
        for (_, op) in ops {
            let _ = stack.apply(op);
        }
        stack.as_slice().len()
    }

    /// Finalize decisions whose alternative frame is no longer on the
    /// stack. One post-token hook replaces the legacy engine's prune
    /// sites in `perform_op` (Pop/Set/pop+push) and `exec_escape`.
    fn prune_dead_branches(&mut self) {
        if !self.trail.live.is_empty() {
            let stack_len = self.core.stack.len();
            self.trail.live.retain(|b| b.alive(stack_len));
        }
    }

    /// Consult or extend the trail for a branch trigger matched from
    /// `pos`. Returns the alternative to push plus whether it is a
    /// bumped replacement (a replayed decision past its first
    /// alternative), or `None` when the decision is `Refuse` — the
    /// caller must suppress the branch pattern at this cursor and
    /// re-search so the parent context's next rule gets a chance.
    pub(super) fn decide_branch(
        &mut self,
        name: &str,
        num_alts: usize,
        pop_count: usize,
        pos: usize,
        ops_len: usize,
        non_consuming_push_at: (usize, usize, usize),
    ) -> Option<(usize, bool)> {
        let line_idx = self.trail.exec_line_idx;
        if self.trail.cursor < self.trail.trail.len() {
            // Deterministic replay: the branch encountered at the cursor
            // must be the one recorded there.
            let d = &self.trail.trail[self.trail.cursor];
            let matches_recorded =
                d.name == name && d.checkpoint.line_idx == line_idx && d.checkpoint.pos == pos;
            debug_assert!(
                matches_recorded,
                "trail replay diverged: recorded {:?}@({},{}) but encountered {:?}@({},{})",
                d.name, d.checkpoint.line_idx, d.checkpoint.pos, name, line_idx, pos
            );
            if matches_recorded {
                let idx = self.trail.cursor;
                self.trail.cursor += 1;
                match self.trail.trail[idx].choice {
                    Choice::Take(alt) => {
                        let d = &self.trail.trail[idx];
                        self.trail.live.push(LiveBranch {
                            decision_idx: idx,
                            name: d.name.clone(),
                            stack_depth: d.stack_depth,
                            pop_count: d.pop_count,
                            created_line: d.created_line,
                        });
                        return Some((alt, self.trail.trail[idx].bumped_same_line));
                    }
                    Choice::Refuse => {
                        self.skipped_branches.push((pos, name.to_string()));
                        return None;
                    }
                }
            }
            // Defensive (unreachable if replay is deterministic): drop
            // the stale suffix and fall through to a fresh decision.
            let cut = self.trail.cursor;
            self.trail.trail.truncate(cut);
            self.trail.live.retain(|b| b.decision_idx < cut);
        }

        let idx = self.trail.trail.len();
        let decision = Decision {
            name: name.to_string(),
            choice: Choice::Take(0),
            num_alts,
            stack_depth: self.core.stack.len(),
            pop_count,
            created_line: self.trail.base_line + line_idx,
            bumped_same_line: false,
            checkpoint: Checkpoint {
                core: self.core.clone(),
                line_idx,
                pos,
                ops_len,
                non_consuming_push_at,
                skipped_branches: self.skipped_branches.clone(),
                live: self.trail.live.clone(),
            },
        };
        self.trail.live.push(LiveBranch {
            decision_idx: idx,
            name: decision.name.clone(),
            stack_depth: decision.stack_depth,
            pop_count: decision.pop_count,
            created_line: decision.created_line,
        });
        self.trail.trail.push(decision);
        self.trail.cursor = idx + 1;
        Some((0, false))
    }

    /// Port of the legacy engine's replay-time rule: during
    /// re-execution of an earlier window line, a non-consuming branch
    /// whose match lands past every character of the line must not
    /// anchor a decision there — the line's re-execution ends and the
    /// branch re-fires at the start of the next line as a fresh
    /// decision, so its eventual failure attaches its pops to that
    /// line's baseline instead of collapsing the parent context one
    /// boundary too early (Markdown's LRD blank line). Returns `true`
    /// when the caller must suppress the branch and end the line.
    pub(super) fn defer_eol_branch(&mut self, name: &str, pos: usize) -> bool {
        if self.trail.exec_line_idx + 1 >= self.trail.lines.len() {
            return false;
        }
        // If the branch at this point is the recorded decision we are
        // replaying (the usual case: it anchored here when this line
        // was the window's last), drop it — the next line's re-fire
        // replaces it. A non-matching cursor means the previous pass
        // deferred here too and the recorded decision belongs to a
        // later point; keep it.
        if self.trail.cursor < self.trail.trail.len() {
            let d = &self.trail.trail[self.trail.cursor];
            if d.name == name
                && d.checkpoint.line_idx == self.trail.exec_line_idx
                && d.checkpoint.pos == pos
            {
                let cut = self.trail.cursor;
                self.trail.trail.truncate(cut);
                self.trail.live.retain(|b| b.decision_idx < cut);
            }
        }
        true
    }

    /// The alternative chosen by [`decide_branch`], handed to
    /// `exec_pattern`'s branch arm.
    ///
    /// [`decide_branch`]: ParseState::decide_branch
    pub(super) fn take_pending_alt(&mut self) -> Option<usize> {
        self.trail.pending_alt.take()
    }

    /// Stash the alternative chosen by [`decide_branch`] for
    /// `exec_pattern`.
    ///
    /// [`decide_branch`]: ParseState::decide_branch
    pub(super) fn set_pending_alt(&mut self, alt: usize) {
        self.trail.pending_alt = Some(alt);
    }

    /// Handle a `fail` action: bump the most recent live same-name
    /// decision to its next alternative (or `Refuse` when exhausted) and
    /// schedule a restart from its checkpoint. Returns `false` when the
    /// fail is a no-op — no live same-name decision, or the restart
    /// budget is exhausted.
    pub(super) fn fail_branch(&mut self, name: &str) -> bool {
        let stack_len = self.core.stack.len();
        let Some(live_idx) = self
            .trail
            .live
            .iter()
            .rposition(|b| b.name == name && b.alive(stack_len))
        else {
            return false;
        };
        let decision_idx = self.trail.live[live_idx].decision_idx;

        let budget = RESTART_BUDGET_PER_WINDOW_LINE * self.trail.lines.len();
        if self.trail.restarts >= budget {
            self.warnings
                .push(ParseWarning::SpeculationBudgetExhausted {
                    name: name.to_string(),
                });
            self.trail.live.clear();
            return false;
        }
        self.trail.restarts += 1;

        let fail_line_idx = self.trail.exec_line_idx;
        let d = &mut self.trail.trail[decision_idx];
        d.choice = match d.choice {
            Choice::Take(i) if i + 1 < d.num_alts => Choice::Take(i + 1),
            _ => Choice::Refuse,
        };
        d.bumped_same_line = fail_line_idx == d.checkpoint.line_idx;
        self.trail.trail.truncate(decision_idx + 1);
        self.trail.cursor = decision_idx;
        self.trail.pending_backtrack = Some(decision_idx);
        true
    }
}
