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

/// Output of [`ParseState::parse_line`].
///
/// `ops` contains the scope-stack operations for the current line. They are
/// final unless [`ParseState::speculative_lines`] returns non-zero, in which
/// case a future call may deliver corrections through `revised`.
///
/// Callers that do not need cross-line accuracy can use `.ops` directly,
/// which behaves identically to the old `Vec<(usize, ScopeStackOp)>` return.
#[derive(Debug, Clone, Default)]
#[must_use]
pub struct ParseLineOutput {
    /// Ops for the current line.
    pub ops: Vec<(usize, ScopeStackOp)>,
    /// Present iff a `fail` during this call revised lines before the
    /// current one. When present, it is a wholesale replacement for the
    /// entire still-uncommitted window: corrected ops for the last
    /// `revised.len()` lines before the current one, in input order.
    ///
    /// The parser state at the window base — the point right before the
    /// first `revised` line — is immutable: it can never be retroactively
    /// corrected by a later call. Consumers therefore reset their scope
    /// stack once, to the snapshot they took at that boundary, and
    /// re-apply `revised` then `ops`.
    pub revised: Option<Vec<Vec<(usize, ScopeStackOp)>>>,
    /// Warnings collected during parsing (e.g. branch point expiry).
    pub warnings: Vec<ParseWarning>,
}

/// A non-fatal problem encountered while parsing a line, reported through
/// [`ParseLineOutput::warnings`]. The parse result is still usable; the
/// warning flags that a `branch_point` was resolved by force rather than
/// by its syntax's own `fail` logic, so some scopes may not match what
/// Sublime Text would produce.
#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum ParseWarning {
    /// A `branch_point` stayed unresolved for more than 128 lines and was
    /// finalized on its current alternative; a later `fail` naming it is
    /// ignored.
    BranchPointExpired {
        /// The `branch_point` name from the syntax definition.
        name: String,
    },
    /// A single `parse_line` call exceeded its backtracking budget and
    /// committed the current parse instead of exploring further
    /// alternatives.
    SpeculationBudgetExhausted {
        /// The `branch_point` whose `fail` would have exceeded the budget.
        name: String,
    },
}

impl std::fmt::Display for ParseWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseWarning::BranchPointExpired { name } => write!(
                f,
                "branch point '{name}' expired (exceeded 128-line rewind limit)"
            ),
            ParseWarning::SpeculationBudgetExhausted { name } => write!(
                f,
                "speculation budget exhausted at branch point '{name}'; committing current parse"
            ),
        }
    }
}

/// Maximum number of times a zero-width escape match can fire at the
/// same byte position within a single line execution before subsequent
/// fires are suppressed (cursor advances one char without applying the
/// escape). Bounds the unbounded branch-fail rewind cycle that hangs
/// the parser on Perl POD-embedded language sections (#650). Threshold
/// is generous to permit legitimate alt-cycle re-executions that
/// re-encounter the same offset.
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
/// Snapshot at [`speculative_lines`]` == 0` boundaries, so the cached
/// state can never be invalidated by a later revision.
///
/// This state doesn't keep track of the current scope stack and parsing only returns changes to this stack
/// so if you want to construct scope stacks you'll need to keep track of that as well.
/// Note that [`HighlightState`] contains exactly this as a public field that you can use.
///
/// **Note:** Caching is for advanced users who have tons of time to maximize performance or want to do so eventually.
/// It is not recommended that you try caching the first time you implement highlighting.
///
/// [`HighlightState`]: ../highlighting/struct.HighlightState.html
/// [`speculative_lines`]: ParseState::speculative_lines
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ParseState {
    core: Core,
    /// Warnings accumulated during parsing, drained into `ParseLineOutput`.
    warnings: Vec<ParseWarning>,
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
    /// Per-line count of zero-width escape fires keyed by byte
    /// position. Once the count at a position exceeds
    /// `ZERO_WIDTH_ESCAPE_FIRE_LIMIT`, subsequent zero-width escapes
    /// at that position are suppressed (cursor advances one char
    /// without applying the escape, mirroring the would_loop
    /// enforcement). The threshold is high enough to permit
    /// legitimate multi-attempt re-executions through the trail's
    /// alt-cycle and still bound the unbounded branch-fail rewind
    /// cycle that hangs the parser on Perl POD-embedded sections
    /// (#650). Cleared per line execution, so a restart reprocessing
    /// the same line offset starts fresh. Intentionally NOT part of
    /// the decision checkpoint.
    zero_width_escape_fires: HashMap<usize, u32>,
    /// Trail-engine state: buffered window, per-line provisional ops,
    /// and the decision trail. See the `trail` module.
    trail: trail::TrailState,
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

mod committed;
mod core;
mod embed;
mod semantics;
#[cfg(feature = "yaml-load")]
#[cfg(test)]
mod tests;
mod trail;

pub use committed::CommittedParser;

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
            warnings: Vec::new(),
            skipped_branches: Vec::new(),
            zero_width_escape_fires: HashMap::default(),
            trail: trail::TrailState::default(),
        }
    }
}
