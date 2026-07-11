//! A wrapper around [`ParseState`] for batch consumers that only want
//! lines whose ops can no longer be revised.

use super::*;

/// Batch-friendly wrapper around [`ParseState`] that only hands back
/// lines once they are final.
///
/// `parse_line` returns ops eagerly, which means a line parsed inside an
/// unresolved `branch_point` may later be revised through
/// [`ParseLineOutput::revised`]. This wrapper absorbs that protocol:
/// [`feed`] returns the ops of every line that just became final —
/// usually the line itself, nothing while a speculation window is open,
/// and several lines at once when one resolves. Call [`finish`] after
/// the last line: at end of input no further `fail` can fire, so
/// whatever is still buffered is final.
///
/// [`feed`]: CommittedParser::feed
/// [`finish`]: CommittedParser::finish
#[derive(Debug, Clone)]
pub struct CommittedParser {
    state: ParseState,
    /// Ops of fed lines not yet handed back, oldest first.
    buffered: Vec<Vec<(usize, ScopeStackOp)>>,
    warnings: Vec<ParseWarning>,
}

impl CommittedParser {
    /// Creates a parser for the given syntax. See [`ParseState::new`].
    pub fn new(syntax: &SyntaxReference) -> CommittedParser {
        CommittedParser {
            state: ParseState::new(syntax),
            buffered: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Parses one line and returns the ops of every line that became
    /// final through this call, oldest first (empty while a
    /// `branch_point` speculation is open).
    ///
    /// The `syntax_set` argument follows the same rules as
    /// [`ParseState::parse_line`].
    pub fn feed(
        &mut self,
        line: &str,
        syntax_set: &SyntaxSet,
    ) -> Result<Vec<Vec<(usize, ScopeStackOp)>>, ParsingError> {
        let ParseLineOutput {
            ops,
            revised,
            warnings,
        } = self.state.parse_line(line, syntax_set)?;
        self.warnings.extend(warnings);
        // `revised` replaces the whole uncommitted window, which is
        // exactly what `buffered` holds: flushes only happen at
        // `speculative_lines() == 0` boundaries, and those are where the
        // parser starts a new window.
        if let Some(revised) = revised {
            let start = self.buffered.len() - revised.len();
            self.buffered.truncate(start);
            self.buffered.extend(revised);
        }
        self.buffered.push(ops);
        if self.state.speculative_lines() == 0 {
            Ok(std::mem::take(&mut self.buffered))
        } else {
            Ok(Vec::new())
        }
    }

    /// Drains the lines still buffered at end of input, oldest first.
    /// With no further input, no `fail` can revise them — they are
    /// final as parsed.
    pub fn finish(&mut self) -> Vec<Vec<(usize, ScopeStackOp)>> {
        std::mem::take(&mut self.buffered)
    }

    /// Warnings accumulated by [`feed`](CommittedParser::feed) calls
    /// since the last drain.
    pub fn take_warnings(&mut self) -> Vec<ParseWarning> {
        std::mem::take(&mut self.warnings)
    }

    /// The wrapped [`ParseState`]. When [`speculative_lines`] is `0`,
    /// cloning it (along with your accumulated scope stack) is the
    /// supported way to snapshot for incremental re-parsing.
    ///
    /// [`speculative_lines`]: ParseState::speculative_lines
    pub fn state(&self) -> &ParseState {
        &self.state
    }
}
