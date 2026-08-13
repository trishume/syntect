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
    /// Corrections to lines already handed back, each tagged with the 1-based
    /// number of the line it replaces. Non-empty only when a cross-line `fail`
    /// just resolved.
    ///
    /// TEXT-EDITOR FORK: the line number is ours. Upstream's is positional, and
    /// a second cross-line fail on one `parse_line` appends another run to it, so
    /// pairing the i-th entry with the i-th buffered line puts one line's ops on
    /// another's text.
    pub replayed: Vec<(usize, Vec<(usize, ScopeStackOp)>)>,
    /// Warnings collected during parsing (e.g. branch point expiry).
    pub warnings: Vec<String>,
}

/// TEXT-EDITOR FORK: how often one zero-width escape may fire at the same byte
/// of a line before the parser stops believing it.
///
/// See the field it bounds, [`ParseState::zero_width_escape_fires`]. High
/// enough that a legitimate alt-cycle replay through the same offset — what
/// Markdown's `$$ … $$` maths blocks do under an embedded-LaTeX branch fail —
/// never reaches it.
const ZERO_WIDTH_ESCAPE_FIRE_LIMIT: u32 = 100;

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
    /// TEXT-EDITOR FORK: how many times a zero-width escape has fired at each
    /// byte of the line being parsed.
    ///
    /// Perl's POD declares six embedded language sections gated by
    /// `escape: (?=^{{pod}})` — a zero-width lookahead anchored to the start of
    /// a line. Against the embedded languages' own branch points that is an
    /// unbounded cycle: an alternative fails, the rewind restores the embed's
    /// escape, the alternative runs again, the same zero-width escape fires at
    /// the same byte, the escape pops back without advancing, and round again.
    /// Upstream's own test corpus hangs on `syntax_test_perl.pl` — their
    /// harness skips the file — and an editor cannot skip a file: the worker
    /// would spin on a core forever and the buffer would never colour.
    ///
    /// So a fire at a position that has already fired
    /// [`ZERO_WIDTH_ESCAPE_FIRE_LIMIT`] times is suppressed and the cursor
    /// advances one character, which is exactly what the `would_loop` guard
    /// below does for the push/pop version of the same shape. Cleared per
    /// `parse_line_inner_from`, so a cross-line replay re-entering the same
    /// offset starts fresh, and deliberately *not* snapshotted into a
    /// `BranchPoint`: the cycle to detect is the one a fail restores, so the
    /// count has to survive the fail to converge.
    ///
    /// This is upstream's diagnosis and upstream's fix, from the open PR #692
    /// against issue #650, taken alone — that PR carries several other changes
    /// on top of unmerged branches. It comes back out when upstream merges it.
    zero_width_escape_fires: HashMap<usize, u32>,
    /// TEXT-EDITOR FORK: the branch points whose alternatives have all run
    /// out at a byte of the line being parsed, by name.
    ///
    /// When a same-line `branch_point` exhausts at a zero-width lookahead,
    /// upstream advances one character past the lookahead's match so the same
    /// `(?=…)` cannot fire again. That is not what Sublime does, and the
    /// character it steps over is a character the parent context never gets to
    /// match: a stale keyword rule then matches *inside* an identifier —
    /// `package` in `$package`, `class` in `Foo.class;` — because the cursor
    /// landed in the middle of one.
    ///
    /// What Sublime does is fall through: the cursor stays where the branch
    /// matched and the parent context's *next* rule is given the position.
    /// So the cursor is rewound to the branch's own start and the branch's
    /// name is recorded here, and [`ParseState::find_best_match`] skips a
    /// `Branch` of that name at that byte — the one thing that would
    /// immediately re-enter the branch that just ran out. Everything else in
    /// the context is searched as usual.
    ///
    /// Entries are byte offsets into one line and never outlive it. They are
    /// dropped as the cursor advances past them, and if nothing at all matches
    /// at the rewound position, `parse_next_token` drops the entry and
    /// advances one character — upstream's behaviour, kept as the last resort
    /// it always was rather than the first answer.
    ///
    /// This is upstream's open PR #664 against issue #631, taken alone; it
    /// comes back out when upstream merges it.
    skipped_branches: Vec<(usize, String)>,
    /// TEXT-EDITOR FORK: while a cross-line `fail` is replaying a buffered
    /// line, the line that line actually *is*.
    ///
    /// `line_number` counts calls to `parse_line`, and a replay does not make
    /// one — so during a replay it still names the line the parser is really
    /// on, several lines later. A `branch_point` created by the replayed text
    /// was then stamped with that number, and the next `fail` for it compared
    /// the two, found them equal, and took the *same-line* path: it rewound
    /// the cursor to a position belonging to the replayed line and looked for
    /// the next reading there, instead of replaying that line again under it.
    ///
    /// Markdown is the file. An unterminated code span before a setext
    /// underline — CommonMark's example 91, `` `Foo `` then `----` — resolves
    /// through two nested branch points; the second one is created during the
    /// first one's replay, and the `fail` that follows on the *next* line was
    /// read as same-line. The underline came out with no scopes at all, and
    /// `markup.heading.2.markdown` was left on the stack for the rest of the
    /// document.
    replay_line: Option<Replay>,
    /// TEXT-EDITOR FORK: a replay asked for by a `fail` that fired *inside* a
    /// replay, for the driver to run once the current one has stopped.
    ///
    /// A cross-line `fail` used to run its own replay wherever it fired,
    /// including in the middle of one. Two loops were then walking the same
    /// buffered lines: the inner finished, the outer carried on from where it
    /// was and re-emitted its own reading over the lines the inner had just
    /// corrected. Corrections are keyed by line and the last one wins, so the
    /// *abandoned* alternative won — `syntax_test_scope_C#7.cs:620`, a tuple
    /// type spread over four lines, came out `meta.cast.cs meta.group.cs`
    /// where Sublime Text has `meta.sequence.tuple.cs` (probe
    /// `csharp_tuple_type`). Alternatives 3 and 4 of the `parens` branch both
    /// replayed inside alternative 2's replay; 4 was the right reading and 2's
    /// remaining iterations wrote over it.
    ///
    /// So a nested `fail` does the state work — restore the snapshot, advance
    /// the branch point — and then stops its line and leaves this behind. One
    /// driver runs the rounds in order, and because each round advances a
    /// branch point's `next_alternative` or removes it, the rounds are bounded
    /// by the alternatives that exist. Breaking out of the nested loops and
    /// keeping the recursion was tried instead and took C# from seconds to past
    /// the corpus budget.
    replay_pending: Option<ReplayRequest>,
    /// Corrected ops produced by a cross-line `fail` replay, to be returned
    /// as `ParseLineOutput::replayed` at the end of `parse_line`.
    /// TEXT-EDITOR FORK: each correction says which line it is for — the
    /// parser's own 1-based `line_number` — instead of being positional.
    ///
    /// Upstream appends here, with the comment "in case multiple cross-line
    /// fails fire on the same parse_line call", and hands the result out as a
    /// bare `Vec<Vec<_>>` whose i-th entry the consumer is expected to pair with
    /// the i-th buffered line. Those two cannot both be right: a second fail on
    /// the same line appends a second run, so the positional mapping slides ops
    /// from one line onto another's text. Upstream's #655 is that bug, reported
    /// as a panic; measured here it turned ShellScript from 893 failing
    /// assertions into 2,691, off 41 corrections.
    ///
    /// Naming the line removes the whole class: a later correction for a line
    /// simply replaces an earlier one, and no consumer has to reconstruct which
    /// lines were buffered.
    flushed_ops: Vec<(usize, Vec<(usize, ScopeStackOp)>)>,
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
    /// TEXT-EDITOR FORK: change 34. The frame the `embed:` was declared in,
    /// when a `pop: N` took it off before the embed went on — the escape's own
    /// text still wears its meta scopes. `None` when it is still on the stack,
    /// which is where the escape finds it anyway.
    holder: Option<(ContextId, usize)>,
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
    /// TEXT-EDITOR FORK: how many of this frame's `meta_content_scope` atoms
    /// were never pushed, because the context under it is a v2 `embed_scope`
    /// wrapper and the host's `embed_scope` *replaces* the embedded syntax's
    /// own scope.
    ///
    /// It has to be remembered rather than worked out later. "The frame below
    /// me has `embed_scope_replaces`" is only the same question at the moment
    /// of the push: a context that arrives on top of a wrapper later — pushed
    /// by a rule of the guest's, not by the embed — did push its own scope,
    /// and asking the stack cannot tell the two apart. Guessing it cost Python
    /// 46 → 98 failing assertions.
    ///
    /// A *count*, and almost always 0 or 1, because what `embed_scope`
    /// replaces is the embedded syntax's own `scope:` — the file scope
    /// `add_initial_contexts` puts at the front of `main`'s
    /// `meta_content_scope` — and nothing else that context happens to carry.
    /// See `mcs_replaced_by_embed_scope`.
    mcs_skipped: usize,
}

/// TEXT-EDITOR FORK: how many of a context's `meta_content_scope` atoms a v2
/// `embed_scope` stands in for.
///
/// Version 2 says `embed_scope` *replaces* the embedded syntax's scope. That
/// scope is one atom, and it is on the stack because `add_initial_contexts`
/// inserts it at the front of `main`'s `meta_content_scope`. So an embed that
/// enters `main` replaces exactly that atom — and an embed that names a
/// *context* of the guest instead (`embed: scope:source.toml…#toml`) replaces
/// nothing, because that context's `meta_content_scope` is the guest's own
/// (`meta.mapping.toml`) and has nothing to do with the file scope.
///
/// Skipping the whole `meta_content_scope` on `embed_scope_replaces` alone was
/// Python's PEP 723 block: TOML's `toml` context lost `meta.mapping.toml`, and
/// then `table-key-accessor`'s `clear_scopes: 1` took `source.toml.embedded.python`
/// off instead — the atom every assertion in the block is about.
fn mcs_replaced_by_embed_scope(
    replaces: bool,
    context_id: &ContextId,
    context: &Context,
    syntax_set: &SyntaxSet,
) -> usize {
    if !replaces {
        return 0;
    }
    let scope = syntax_set.syntaxes()[context_id.syntax_index].scope;
    usize::from(context.meta_content_scope.first() == Some(&scope))
}

/// TEXT-EDITOR FORK: whether a branch point's alternative is still on the stack.
///
/// `stack_depth` is snapshotted *before* a `pop: N + branch:` does its pop, so
/// the alternative's own frame sits at `stack_depth - pop_count + 1` and is
/// present exactly while `stack_len > stack_depth - pop_count`. `handle_fail`
/// has always asked it that way; `exec_escape` asked
/// `stack_depth <= stack_len`, which for `N >= 2` prunes a branch point whose
/// alternative an embed escape left standing — and a later `fail` naming it
/// then silently does nothing.
///
/// That is the same failure as change 4 and the same predicate class as
/// upstream's #660: three ways of asking one question, one of which quietly
/// says no. So there is one place to ask it, and both callers ask here.
fn branch_point_is_live(bp: &BranchPoint, stack_len: usize) -> bool {
    stack_len > bp.stack_depth.saturating_sub(bp.pop_count)
}


/// TEXT-EDITOR FORK: which of a `set:`'s targets may emit its `clear_scopes`
/// *before* the trigger token rather than after it, if any.
///
/// A `set:` pushes its targets' `meta_scope`s in the initial phase, so the
/// trigger text sees them, and their `meta_content_scope`s only in the
/// non-initial phase, after it. A `clear_scopes` on one of those targets is
/// therefore emittable early exactly when everything it can eat is already on
/// the stack by then: nothing of the *leaving* context sitting on top
/// (`cur_context`'s own meta scopes come off in the non-initial phase), and no
/// target *before* it in the list contributing a `meta_content_scope` that has
/// not been pushed yet.
///
/// Zsh is what this is for. `redirection-input` fires `(\d*)(<)` with
/// `set: [string-path-pattern-body, zsh-redirection-glob-range-end, …]`, and
/// the second of those carries `clear_scopes: 1  # clear string.unquoted` —
/// the atom the *first* one's `meta_scope` just pushed. Deferring that Clear
/// left `string.unquoted.shell` on the `<` itself and cleared it one column
/// on; ST scopes that column
/// `meta.string.glob.shell meta.range.shell.zsh punctuation.definition.range.begin.shell.zsh`,
/// with no `string`, asked directly (`tools/sublime-oracle`, the `shellscript`
/// probe).
///
/// Makefile is why the `meta_content_scope` half is a condition and not an
/// assumption: `set: [value-to-be-defined, eat-whitespace-then-pop]` puts the
/// `clear_scopes: 1` on the *last* target, and what it is meant to eat is
/// `value-to-be-defined`'s `meta_content_scope`, which does not exist yet in
/// the initial phase. That one still belongs after the token.
fn early_set_clear(
    is_set: bool,
    cur_context: &Context,
    context_refs: &[ContextReference],
    syntax_set: &SyntaxSet,
) -> Result<Option<usize>, ParsingError> {
    if !is_set
        || !cur_context.meta_scope.is_empty()
        || !cur_context.meta_content_scope.is_empty()
    {
        return Ok(None);
    }
    for (i, r) in context_refs.iter().enumerate() {
        let ctx = r.resolve(syntax_set)?;
        if ctx.clear_scopes.is_some() {
            return Ok(Some(i));
        }
        if !ctx.meta_content_scope.is_empty() {
            return Ok(None);
        }
    }
    Ok(None)
}

/// TEXT-EDITOR FORK: which line a cross-line replay is currently re-reading.
///
/// See [`ParseState::replay_line`]. Both halves are needed and neither can be
/// derived from the parser's counters, because a replay does not advance them.
#[derive(Debug, Clone, Eq, PartialEq)]
struct Replay {
    /// The line's own number, counting from 0, as `line_number - 1` would give
    /// at the top level.
    line_number: usize,
    /// Its index in `pending_lines`, so a branch point created on it can
    /// replay it rather than the lines after it.
    pending_index: usize,
    /// What the enclosing replay has already composed for this line, before
    /// the point the re-parse resumes from.
    ///
    /// A branch point created inside a replay sees an `ops` that starts empty
    /// at the resume position, so its own `prefix_ops` would describe only the
    /// tail of the line. Its correction then *replaces* the enclosing one —
    /// corrections are keyed by line — and everything before the resume point
    /// is lost. In Markdown that came out as an empty correction for the line
    /// the heading was on, and the next line's ops popped a stack nothing had
    /// pushed.
    prefix: Vec<(usize, ScopeStackOp)>,
}

/// TEXT-EDITOR FORK: one replay to run — everything a cross-line `fail` needs
/// in order to re-read the buffered lines under the reading it has just chosen.
///
/// Both cross-line paths reduce to this. They differ only in how `prefix` and
/// `resume_at` are worked out: an exhausted branch point re-emits the ops the
/// creation line already had and resumes one character past the branch match,
/// so the same branch cannot fire again; a next alternative re-emits those ops
/// plus the trigger's own scopes and the alternative's meta scopes, and resumes
/// at the end of the trigger match. Past that point the work is the same, which
/// is why it is one function — see `ParseState::run_replay`.
#[derive(Debug, Clone, Eq, PartialEq)]
struct ReplayRequest {
    /// Where in `pending_lines` the replay starts. Its first entry is the line
    /// the branch point was created on.
    from: usize,
    /// What that first line already has, verbatim, before `resume_at`.
    prefix: Vec<(usize, ScopeStackOp)>,
    /// Where the re-parse of that first line picks up.
    resume_at: usize,
    /// TEXT-EDITOR FORK: a branch point that ran out at `resume_at`, whose
    /// `Branch` must not be tried again there. See `skipped_branches`: this is
    /// that fall-through, for the branch that ran out on an *earlier* line.
    skip: Option<(usize, String)>,
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

/// TEXT-EDITOR FORK: change 31. What one `push:` of several contexts clears in
/// a version 1 syntax: all of their `clear_scopes` added up, applied once,
/// before any of their meta scopes.
///
/// Sublime lists it under Compatibility as "Multiple Target Push Actions with
/// `clear_scopes`", and its example is a `push: [context2, context3]` where
/// both carry `clear_scopes: 1`: version 1 takes two atoms off before pushing
/// either `meta_scope`, so `abc` comes out `meta.ctx2 meta.ctx3 identifier`
/// with the file's own `source.lang` cleared away, where version 2 interleaves
/// them — clear, push, clear, push — and leaves `source.lang meta.ctx3
/// identifier`. ST 4206 answers exactly that at both versions
/// (`tools/sublime-oracle/probes/VerMultiClearV1.sublime-syntax`, the
/// `multiclear_v1` and `multiclear_v2` jobs); this emitted version 2's
/// interleaving whatever the file said.
///
/// `All` swallows the rest, being already everything there is.
fn summed_push_clear(
    context_refs: &[ContextReference],
    syntax_set: &SyntaxSet,
) -> Result<Option<ClearAmount>, ParsingError> {
    let mut total = 0;
    for r in context_refs.iter() {
        match r.resolve(syntax_set)?.clear_scopes {
            Some(ClearAmount::All) => return Ok(Some(ClearAmount::All)),
            Some(ClearAmount::TopN(n)) => total += n,
            None => {}
        }
    }
    Ok((total > 0).then_some(ClearAmount::TopN(total)))
}

/// Build the ordered Push/Pop ops for a match's `captures:` mapping over
/// its regex `regions`. Captures can appear in arbitrary source order
/// (e.g. `((bob)|(hi))*` matching `hibob` — the outer group must Push
/// before any inner group). Empty captures are skipped because they'd
/// otherwise sort a Pop before its Push. The returned ops are already
/// position-ordered and safe to append to a parser ops vec.
fn build_capture_ops(
    capture_map: &CaptureMapping,
    regions: &Region,
    version: u32,
) -> Vec<(usize, ScopeStackOp)> {
    // TEXT-EDITOR FORK: change 30. In a version 1 syntax a capture that goes
    // *backwards* gets no scope at all, and this applied every one of them.
    //
    // Sublime lists it under Compatibility as "Regex Capture Group Order", and
    // its own example is `(?:(x)|(y))+` over `yx`: group 2 matches the `y` at
    // column 0 and group 1 the `x` at column 1. **The documented result is the
    // wrong way round** — it says `x` loses its scope — and ST 4206 answers the
    // opposite (`tools/sublime-oracle/probes/VerCaptureOrderV1.sublime-syntax`,
    // the `captures_v1` job): `y` is bare and `x` keeps `identifier.x`. So the
    // rule is not about which group is lower-numbered, it is a *cursor*: the
    // groups are read in capture-index order and one that begins before the
    // last one it applied is dropped.
    //
    // Which cursor took a second probe, because the two readings that survive
    // that example disagree about the commonest case of all. Against the last
    // applied group's *end*, `((a)b)` would lose its inner group — group 1 is
    // [0,2) and group 2 begins at 1 — and half the version 1 definitions in
    // the set would quietly stop colouring their inner captures. Against its
    // *start*, nesting is untouched and only a genuine step backwards is
    // dropped. ST keeps the inner group (`VerCaptureNestV1`, the `nest_v1` and
    // `nest_siblings_v1` jobs), so it is the start.
    //
    // Version 2 applies all of them, which is what this did unconditionally.
    // The mapping is in the order the YAML hash was read, so the walk sorts by
    // capture index rather than trusting it, and only the groups that carry
    // scopes take part — an unscoped group is not something ST could report a
    // position for either.
    let mut ordered: Vec<&(usize, Vec<Scope>)> = capture_map.iter().collect();
    if version < 2 {
        ordered.sort_by_key(|(cap_index, _)| *cap_index);
    }
    let mut last_start = 0;
    let mut map: Vec<((usize, i32), ScopeStackOp)> = Vec::new();
    for &(cap_index, ref scopes) in ordered.into_iter() {
        if let Some((cap_start, cap_end)) = regions.pos(cap_index) {
            if cap_start == cap_end {
                continue;
            }
            if version < 2 {
                if cap_start < last_start {
                    continue;
                }
                last_start = cap_start;
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
    map.into_iter().map(|((i, _), op)| (i, op)).collect()
}

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
            mcs_skipped: 0,
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
            zero_width_escape_fires: HashMap::new(),
            skipped_branches: Vec::new(),
            replay_line: None,
            replay_pending: None,
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
        self.parse_line_inner_from(line, syntax_set, 0, None)
    }

    /// Parse `line` starting at `start_at` rather than column 0. Used by
    /// cross-line `fail` replay: the first buffered line's pre-branch
    /// prefix was correctly parsed under the pre-branch state, so the
    /// replay resumes *after* the branch match under the new alternative.
    /// When `start_at > 0` the `first_line` bookkeeping is skipped — the
    /// caller has already emitted (or preserved) the initial
    /// meta_content_scope push.
    fn parse_line_inner_from(
        &mut self,
        line: &str,
        syntax_set: &SyntaxSet,
        start_at: usize,
        // TEXT-EDITOR FORK: a branch that ran out at a byte of *this* string,
        // to suppress there. See `ReplayRequest::skip`.
        skip: Option<(usize, String)>,
    ) -> Result<Vec<(usize, ScopeStackOp)>, ParsingError> {
        let mut match_start = start_at;
        let mut res = Vec::new();

        if start_at == 0 && self.first_line {
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
        let mut non_consuming_push_at = (0, 0, 0);
        // TEXT-EDITOR FORK: fresh for every call, replays included — a
        // cross-line replay re-entering the same offset is not a loop. See
        // `zero_width_escape_fires`.
        self.zero_width_escape_fires.clear();
        // TEXT-EDITOR FORK: and the exhausted branches belong to *this* call,
        // because they are byte offsets into the string it was handed. A
        // cross-line replay re-enters here with another line, so its entries
        // are put aside for the duration and the caller's are given back at
        // the end — carried either way, they would suppress a branch at an
        // offset that means nothing in the other string, or index past the end
        // of a shorter one. See `skipped_branches`.
        let outer_skipped = std::mem::take(&mut self.skipped_branches);
        self.skipped_branches.extend(skip);

        while self.parse_next_token(
            line,
            syntax_set,
            &mut match_start,
            &mut search_cache,
            &mut regions,
            &mut non_consuming_push_at,
            &mut res,
        )? {}

        // TEXT-EDITOR FORK: see `outer_skipped` above.
        self.skipped_branches = outer_skipped;

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
        non_consuming_push_at: &mut (usize, usize, usize),
        ops: &mut Vec<(usize, ScopeStackOp)>,
    ) -> Result<bool, ParsingError> {
        // TEXT-EDITOR FORK: the guard is armed over the *range* a non-consuming
        // push reached, not over one depth of it, and what it watches for is a
        // pop landing back where that push started.
        //
        // Upstream records `stack.len() + 1` — the depth a push of *one*
        // context reaches — so a `push:` of K contexts armed a depth the stack
        // never sat at, and a `pop: K` from the top unwound `pre+K → pre`
        // without ever visiting `pre+1`. Nothing flagged it, the position never
        // moved, and the same rule fired again: an unbounded loop with no outer
        // iteration cap. Reachable by any definition in
        // `<install>/user/syntaxes/`, which nothing bounds — see
        // `a_multi_context_push_that_pops_all_the_way_back_does_not_hang`.
        //
        // `armed_pre` is the pre-push depth while the stack is anywhere inside
        // `(pre, post]`; the pop that loops is the one whose landing depth is
        // `pre` exactly. A pop that lands *below* `pre` is not a loop — the
        // context that owned the trigger is gone with it, which is what
        // upstream's `Pop(1)`-only test was standing in for (Haskell's
        // `immediately-pop2`), and this says directly.
        let armed_pre = {
            let (pos, pre, post) = *non_consuming_push_at;
            let depth = self.stack.len();
            if pos == *start && depth > pre && depth <= post {
                Some(pre)
            } else {
                None
            }
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
            armed_pre,
        )?;

        if let Some(reg_match) = best_match {
            // Check if this is an escape match (sentinel pat_index)
            if reg_match.pat_index == usize::MAX {
                let (match_start, match_end) = reg_match.regions.pos(0).unwrap();
                // TEXT-EDITOR FORK: the escape that will not stop firing. See
                // `zero_width_escape_fires`.
                let zero_width = match_start == match_end;
                if zero_width
                    && self.zero_width_escape_fires.get(&match_start).copied().unwrap_or(0)
                        >= ZERO_WIDTH_ESCAPE_FIRE_LIMIT
                {
                    return match line[*start..].char_indices().nth(1) {
                        Some((i, _)) => {
                            *start += i;
                            Ok(true)
                        }
                        // End of line: no character to advance onto, and no
                        // point trying any more patterns.
                        None => Ok(false),
                    };
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
            // TEXT-EDITOR FORK: only when there is somewhere to rewind *to*.
            //
            // Sublime: "If a `fail` action specifies a `branch_point` that was
            // never pushed on the stack, or has already been popped off of it,
            // it will have no effect." It is the *action* that has no effect —
            // the rule is a rule like any other, so its text is consumed and
            // its `scope` applies, and the line carries on.
            //
            // `handle_fail` answers an unresolvable name with `Ok(false)`,
            // which is not "no effect": it is what `parse_next_token` returns
            // to say *stop parsing this line*. So a stray `fail` — CSS has one,
            // `- match: ';' fail: property-or-selector`, reachable outside any
            // branch — silently threw away the colouring of everything after it
            // on that line. Falling through instead sends it down the ordinary
            // path, where `MatchOperation::Fail` is already a no-op.
            let fail_that_resolves = match match_pattern.operation {
                MatchOperation::Fail(ref name) => self.can_rewind_to(name),
                _ => false,
            };
            if fail_that_resolves {
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

            // TEXT-EDITOR FORK: inside a cross-line replay, a non-consuming
            // `Branch` whose match lands past every character of the replayed
            // line does not get a branch point.
            //
            // The one it would create is chained — opened by the replay that an
            // earlier branch's exhaustion started — and the *outer* parse
            // exhausts it on the next line, often an empty one. Its pop ops
            // then attach to this replayed line, which is the next line's
            // baseline, and the parent context collapses one boundary too
            // early. Skipping it is not losing the reading: the parent rule
            // fires again at the start of the outer line and the branch point
            // is anchored there instead.
            //
            // Markdown's link reference definition is the file:
            // `link-def-attr`'s `- match: $` created a branch point inside
            // `link-title-continuation`'s exhaustion replay, and
            // `meta.link.reference.def.markdown` came off on the blank line
            // where ST keeps it. Upstream's #677, whose diagnosis this is.
            if matches!(match_pattern.operation, MatchOperation::Branch { .. })
                && self.replay_line.is_some()
                && match_end >= line.len()
            {
                return Ok(false);
            }

            let consuming = match_end > *start;
            if !consuming {
                // The match doesn't consume any characters. If this is a
                // "push", remember the position and stack size so that we can
                // check the next "pop" for loops. Otherwise leave the state,
                // e.g. non-consuming "set" could also result in a loop.
                // TEXT-EDITOR FORK: how deep this push actually goes, so the
                // guard is armed over all of it. A `branch:` pushes exactly one
                // alternative whatever the list holds; a `push:`/`embed:`
                // pushes its whole list. A `pop: M` in front of any of them
                // takes the stack below where the trigger's own context sits,
                // so nothing can pop *back* to it and the range comes out empty
                // — disarmed, which is the right answer. See `armed_pre`.
                let pre = self.stack.len();
                let reach = match match_pattern.operation {
                    MatchOperation::Push { ref ctx_refs, pop_count } => {
                        Some(pre.saturating_sub(pop_count) + ctx_refs.len())
                    }
                    MatchOperation::Branch { pop_count, .. } => {
                        Some(pre.saturating_sub(pop_count) + 1)
                    }
                    MatchOperation::Embed { ref contexts, pop_count, .. } => {
                        Some(pre.saturating_sub(pop_count) + contexts.len())
                    }
                    // Anything else leaves the arming where it is, which is
                    // upstream's "otherwise leave the state, e.g. non-consuming
                    // `set` could also result in a loop". Overwriting it here
                    // *narrows* the guard — a non-consuming `set:` between the
                    // push and the pop hid the push it was armed on — and hung
                    // `CFML Script (Tags)` and `JavaScript (for PHP
                    // double-quoted)`.
                    _ => None,
                };
                if let Some(reach) = reach {
                    // TEXT-EDITOR FORK: and never narrower than the one depth
                    // upstream arms. With a `pop: M` in front, the arithmetic
                    // above can put the reach at or below `pre` and leave the
                    // range empty — but the stack can still come back up
                    // through `pre + 1` under later pushes at this same byte,
                    // and a `Pop(1)` there lands on `pre` and loops. This guard
                    // may be widened and may not be narrowed.
                    *non_consuming_push_at = (match_end, pre, reach.max(pre + 1));
                }
            }

            *start = match_end;

            // TEXT-EDITOR FORK: a branch that ran out at a byte the cursor has
            // now passed is no longer anything to suppress. See
            // `skipped_branches`.
            self.skipped_branches.retain(|(at, _)| *at >= *start);

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
        } else if self.skipped_branches.iter().any(|(at, _)| *at == *start) {
            // TEXT-EDITOR FORK: nothing matched at a position where a branch
            // point ran out, so the parent context has nothing to offer here
            // either. Drop the entry and advance one character — which is what
            // upstream does unconditionally, kept as the last resort. See
            // `skipped_branches`.
            self.skipped_branches.retain(|(at, _)| *at != *start);
            match line[*start..].char_indices().nth(1) {
                Some((i, _)) => {
                    *start += i;
                    search_cache.clear();
                    Ok(true)
                }
                // End of line: no character to advance onto.
                None => Ok(false),
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
        armed_pre: Option<usize>,
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

                // TEXT-EDITOR FORK: the branch that just ran out at this byte
                // is the one rule that must not be tried again here — it would
                // re-enter the alternatives that have already all failed. Every
                // other rule of the context is searched as usual, which is the
                // fall-through. See `skipped_branches`.
                if let MatchOperation::Branch { name, .. } = &match_pat.operation {
                    if self
                        .skipped_branches
                        .iter()
                        .any(|(at, exhausted)| *at == start && exhausted == name)
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

                    // println!("matched pattern {:?} at start {} end {} (pop would loop: {}, min start: {}, initial start: {}, check_pop_loop: {}, stack_len: {})", match_pat, match_start, match_end, pop_would_loop, min_start, start, check_pop_loop, self.stack.len());

                    if match_start < min_start || (match_start == min_start && pop_would_loop) {
                        // New match is earlier in text than old match,
                        // or old match was a looping pop at the same
                        // position.

                        // println!("setting as current match");

                        min_start = match_start;

                        let consuming = match_end > start;
                        // TEXT-EDITOR FORK: a non-consuming `pop: N` loops when
                        // it lands back on the depth the push started from — the
                        // pre-push stack restored, so the push rule fires again
                        // at a position that never moved. Landing below it is
                        // not a loop: the context that owned the trigger has
                        // gone with the frames (Haskell's `immediately-pop2`, the
                        // fallback alternative for `declaration-type-end`).
                        // See `armed_pre`.
                        pop_would_loop = !consuming
                            && match (armed_pre, &match_pat.operation) {
                                (Some(pre), MatchOperation::Pop(n)) => {
                                    self.stack.len().saturating_sub(*n) == pre
                                }
                                _ => false,
                            };

                        let push_too_deep = matches!(
                            match_pat.operation,
                            MatchOperation::Push { .. }
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

        // TEXT-EDITOR FORK: the cache is about *this* line, and a truncated
        // search is about a shorter one — see the haystack below. A `None`
        // cached from a full-line search is "this pattern does not match here",
        // which is exactly what a pattern whose lookahead trips over the escape
        // says, and reusing it is how `done` before a closing backtick stayed a
        // command name with the cut in place. Neither read nor written while
        // the line is cut.
        let cut = search_end < line.len();

        if let Some(maybe_region) = search_cache.get(&match_ptr).filter(|_| !cut) {
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
        // Only `MatchOperation::None` patterns must avoid zero-length matches; every other
        // operation legitimately needs them (lookaheads with branch/fail, empty patterns with
        // pop/set, etc.). The regex engine handles this via its `FIND_NOT_EMPTY` option.
        let allow_empty = !matches!(match_pat.operation, MatchOperation::None);
        // print!("  executing regex: {:?} at pos {} on line {}", regex.regex_str(), start, line);
        // TEXT-EDITOR FORK: an `escape` does not only bound where an embedded
        // syntax's match may *land* — it ends the text that syntax can see. So
        // the search is over the line cut at the escape, not over the whole
        // line with the range narrowed: Oniguruma's `range` limits where a
        // match may start and end, and a lookahead still reads past it.
        //
        // Bash is the file. Inside `` ` … ` `` the whole shell is embedded with
        // `escape: '\`'`, and `done` is `done{{cmd_break}}` where `cmd_break`
        // is `(?!{{cmd_char}})` — a backtick is not one of Bash's
        // metacharacters, so with the backtick visible that lookahead fails and
        // `` `…; done` `` read `done` as a command name
        // (`variable.function.shell`) rather than the end of the loop. ST scopes
        // it `keyword.control.loop.end.shell`, and does so for `` `done` `` with
        // no loop around it at all — so it is the cut and not the loop
        // (`tools/sublime-oracle`, the `shellscript` probe, `bash_bare`).
        //
        // Only the tail is cut, so lookbehind and `^` are untouched; `$` now
        // holds at the escape, which is where the embedded syntax's line ends
        // as far as it is concerned.
        let haystack = if search_end < line.len() {
            &line[..search_end]
        } else {
            line
        };
        let matched = regex.search(haystack, start, search_end, Some(regions), allow_empty);

        if matched {
            let (match_start, match_end) = regions.pos(0).unwrap();
            // this is necessary to avoid infinite looping on dumb patterns
            let does_something = match match_pat.operation {
                MatchOperation::None => match_start != match_end,
                MatchOperation::Push { .. }
                | MatchOperation::Branch { .. }
                | MatchOperation::Embed { .. } => self.stack.len() < 100,
                // TEXT-EDITOR FORK: change 4's other half. An unresolvable
                // `fail` is let through to `perform_op` as an ordinary rule
                // rather than stopping the line — and a rule that changes
                // nothing must not match empty, or the parser sits at the same
                // position forever. `MatchOperation::None` says exactly that
                // one line above; this is the same rule for the same reason.
                //
                // `- match: '(?=;)' fail: nobody` is the shape, and it hangs
                // without this: upstream's own `branch_fail_nonexistent_name`
                // is that definition, and it is one of the tests this fork
                // cannot run (`cargo test -p syntect` does not compile here),
                // so it went unnoticed until the changes were ported back onto
                // an upstream checkout. A user's own `syntaxes/` folder reaches
                // it with two lines.
                MatchOperation::Fail(ref name) => {
                    match_start != match_end || self.can_rewind_to(name)
                }
                _ => true,
            };
            if can_cache && does_something && !cut {
                // Only cache when searching the full line — truncated searches
                // could give different results for later positions.
                search_cache.insert(match_pat, Some(regions.clone()));
            }
            if does_something {
                // print!("catch {} at {} on {}", match_pat.regex_str, match_start, line);
                return Some(regions.clone());
            }
        } else if can_cache && !cut {
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
        // TEXT-EDITOR FORK: which version's rules the captures of this match
        // follow — the same question `push_meta_ops` asks, and asked the same
        // way, off the context on top of the stack. See `build_capture_ops`.
        let version = self.current_syntax_version(syntax_set);

        // Handle Fail: attempt backtracking.
        //
        // TEXT-EDITOR FORK: and only when there is somewhere to rewind to. The
        // other half of the change at the `Fail` check in `parse_next_token`,
        // which is where the reason is written down — a `fail` naming a branch
        // point nobody opened is a rule like any other, scope and all, not an
        // instruction to abandon the rest of the line.
        let unresolvable = match pat.operation {
            MatchOperation::Fail(ref name) => !self.can_rewind_to(name),
            _ => false,
        };
        if let MatchOperation::Fail(ref name) = pat.operation {
            if !unresolvable {
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
                    // TEXT-EDITOR FORK: the line this branch point is really
                    // on, which is not `line_number` inside a replay. See
                    // `replay_line`.
                    line_number: self.cur_line_number(),
                    ops_snapshot_len: ops.len(),
                    stack_depth: self.stack.len(),
                    non_consuming_push_at_snapshot: *non_consuming_push_at,
                    first_line_snapshot: self.first_line,
                    with_prototype: pat.with_prototype.clone(),
                    // TEXT-EDITOR FORK: see `pending_lines_mark`.
                    pending_lines_snapshot_len: self.pending_lines_mark(),
                    escape_stack_snapshot: self.escape_stack.clone(),
                    pop_count,
                    // TEXT-EDITOR FORK: inside a replay, what this line has
                    // already been given comes first. See `Replay::prefix`.
                    prefix_ops: match &self.replay_line {
                        Some(replay) => {
                            let mut prefix = replay.prefix.clone();
                            prefix.extend(ops.iter().cloned());
                            prefix
                        }
                        None => ops.clone(),
                    },
                    capture_ops: pat
                        .captures
                        .as_ref()
                        .map(|m| build_capture_ops(m, &reg_match.regions, version))
                        .unwrap_or_default(),
                };
                self.branch_points.push(bp);
                // TEXT-EDITOR FORK: `pop: N + branch:` is a lookahead, like
                // `pop: N + push:` and not like `set`. This built a synthetic
                // `Set`, which gave the trigger token the meta scopes of every
                // frame it was unwinding. Asked of ST with a three-frame probe,
                // `bgWQQQ` in `PopPush.sublime-syntax`, the trigger comes out
                // `source meta.tail.branch keyword.trigger.branch` — the target's
                // `meta_scope` alone. So the synthetic is a `Push` carrying the
                // count, which is where the lookahead unwinding lives.
                synthetic_op = if pop_count > 0 {
                    MatchOperation::Push {
                        ctx_refs: vec![alternatives[0].clone()],
                        pop_count,
                    }
                } else {
                    MatchOperation::Push { ctx_refs: vec![alternatives[0].clone()], pop_count: 0 }
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
            .map(|m| build_capture_ops(m, &reg_match.regions, version))
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

    /// Handle a `fail` operation by rewinding to the named branch point.
    /// Returns Ok(true) if backtracking happened (caller should continue from rewound position).
    /// Returns Ok(false) if the fail had no effect.
    /// TEXT-EDITOR FORK: whether a `fail` naming this branch point has
    /// anywhere to rewind to.
    ///
    /// The same test `handle_fail` makes before it does anything, asked before
    /// the rule is routed there at all — see the site in `parse_next_token`
    /// that calls it, which is where the reason lives.
    fn can_rewind_to(&self, name: &str) -> bool {
        let stack_len = self.stack.len();
        self.branch_points
            .iter()
            .any(|bp| bp.name == name && branch_point_is_live(bp, stack_len))
    }

    fn handle_fail(
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
        let bp_index = self
            .branch_points
            .iter()
            .rposition(|bp| bp.name == name && branch_point_is_live(bp, stack_len));
        let bp_index = match bp_index {
            Some(i) => i,
            None => return Ok(false), // No such branch point, fail is no-op
        };

        // TEXT-EDITOR FORK: see `replay_line`.
        let cur_line = self.cur_line_number();
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

        // Check validity: the alternative's frame is still on the stack.
        // TEXT-EDITOR FORK: the fourth place this was asked and the second that
        // got it wrong — `stack.len() < stack_depth` ignores the `pop_count`
        // that the `rposition` above had just accounted for, so a `pop: N +
        // branch:` with N >= 2 found its branch point and then threw it away
        // one line later. Asked the one way now; see `branch_point_is_live`.
        if !branch_point_is_live(bp, self.stack.len()) {
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
            let stack_snapshot = bp.stack_snapshot.clone();
            let proto_starts_snapshot = bp.proto_starts_snapshot.clone();
            let escape_stack_snapshot = bp.escape_stack_snapshot.clone();
            let first_line_snapshot = bp.first_line_snapshot;
            let non_consuming_push_at_snapshot = bp.non_consuming_push_at_snapshot;
            let ops_snapshot_len = bp.ops_snapshot_len;
            let match_start_pos = bp.match_start;
            let pending_lines_snapshot_len = bp.pending_lines_snapshot_len;
            let prefix_ops = bp.prefix_ops.clone();
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
                // emitted before the branch match) verbatim, then resume where
                // the branch matched.
                //
                // TEXT-EDITOR FORK: *where it matched*, with the branch's name
                // suppressed there — not one character past it, which is what
                // this did and what change 19's same-line arm stopped doing for
                // the reason `skipped_branches` gives: the character stepped
                // over is one the parent context never gets to match, so the
                // token the branch was opened on loses its scopes entirely.
                //
                // `package apple dot` in Java's corpus is the line: `dot` opens
                // a declaration whose branch runs out several lines later, and
                // the replay resumed at `ot`, which nothing matches. ST gives
                // the whole word `variable.other.java` — the parent's next
                // rule, which is exactly the fall-through the same-line arm
                // already does.
                let first = &self.pending_lines[pending_lines_snapshot_len];
                let resume_at = match_start_pos.min(first.len());
                let request = ReplayRequest {
                    from: pending_lines_snapshot_len,
                    prefix: prefix_ops,
                    resume_at,
                    skip: Some((resume_at, name.to_string())),
                };
                // TEXT-EDITOR FORK: inside a replay this does not replay. It
                // leaves the request for the driver and ends the line, so that
                // two loops cannot walk the same buffered lines and the reading
                // chosen last is the one that survives. See `replay_pending`.
                if self.replay_line.is_some() {
                    self.replay_pending = Some(request);
                    return Ok(false);
                }
                self.run_replays(request, syntax_set)?;

                // Restart the current line from the beginning under the
                // restored state. TEXT-EDITOR FORK: including what ran out on
                // it, which was exhausted under a reading this restart has
                // just replaced. See `skipped_branches`.
                self.skipped_branches.clear();
                ops.clear();
                *start = 0;
                *non_consuming_push_at = (0, 0, 0);
                search_cache.clear();
                return Ok(true);
            }

            // Same-line exhaustion. TEXT-EDITOR FORK: rewind to where the
            // branch point matched and record its name, so the parent
            // context's next rule is offered the position that the branch
            // took. Upstream advances one character past the lookahead
            // instead, which steps the cursor into the middle of the token the
            // parent was about to match. See `skipped_branches`.
            //
            // Unless the position is not a position in *this* string: a
            // cross-line replay parses an earlier line while `line_number`
            // still names the current one, so a branch point that really is
            // from another line reads as same-line here and its `match_start`
            // can be past the end of the line being replayed. There is nothing
            // on this line to rewind to then, so the line ends — which is what
            // the end-of-line arm did before.
            if match_start_pos <= line.len() {
                self.skipped_branches
                    .push((match_start_pos, name.to_string()));
                *start = match_start_pos;
            } else {
                *start = line.len();
            }
            search_cache.clear();
            return Ok(true);
        }

        // Determine if this is a cross-line fail (branch was created on a previous line).
        let is_cross_line = bp.line_number < cur_line;

        // Extract everything we need from bp before mutating self.
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

        // TEXT-EDITOR FORK: the ops for that unwinding, counted here because
        // this is the last moment the frames it is about are on the stack — the
        // re-emit below happens with the alternative already pushed over them.
        // See `lookahead_unwind_ops`.
        let mut unwind_ops = Vec::new();
        if pop_count > 0 {
            self.lookahead_unwind_ops(pop_count, trigger_match_start, syntax_set, &mut unwind_ops)?;
        }

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
            // A branch alternative is pushed on its own, never as the guest of
            // an embed, and `handle_fail` pushes its `meta_content_scope`.
            mcs_skipped: 0,
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
            // Keep `pending_lines` intact (don't drain): if a second
            // branch_point on the current line also fails after this retry, its
            // own replay needs access to the same buffered lines. Nested
            // branches from the same earlier line share the buffer.
            //
            // Compose the first buffered line's prefix here, then hand the
            // whole thing to the driver. Appending each round's corrections is
            // safe because every one of them names its line: a later round for
            // a line already in the list simply wins, which is how the reading
            // chosen last is the one that survives.
            let mut prefix = prefix_ops;
            // TEXT-EDITOR FORK: the trigger's own unwinding comes first, exactly
            // as `push_meta_ops` emits it. See `lookahead_unwind_ops`.
            prefix.extend(unwind_ops.iter().cloned());
            if let Some(clear_amount) = context.clear_scopes {
                prefix.push((trigger_match_start, ScopeStackOp::Clear(clear_amount)));
            }
            for scope in context.meta_scope.iter() {
                prefix.push((trigger_match_start, ScopeStackOp::Push(*scope)));
            }
            for scope in &trigger_pat_scope {
                prefix.push((trigger_match_start, ScopeStackOp::Push(*scope)));
            }
            // See the matching comment in the same-line branch below — re-emit
            // the trigger match's captures inside the pat_scope brackets so
            // they survive the branch swap.
            prefix.extend(trigger_capture_ops.iter().cloned());
            if !trigger_pat_scope.is_empty() {
                prefix.push((match_start_pos, ScopeStackOp::Pop(trigger_pat_scope.len())));
            }
            for scope in context.meta_content_scope.iter() {
                prefix.push((match_start_pos, ScopeStackOp::Push(*scope)));
            }
            let request = ReplayRequest {
                from: pending_lines_snapshot_len,
                prefix,
                resume_at: match_start_pos,
                // The alternative that was just pushed is what resumes here;
                // nothing is being suppressed. See `ReplayRequest::skip`.
                skip: None,
            };
            // TEXT-EDITOR FORK: as in the exhaustion arm — a `fail` inside a
            // replay hands the request to the driver rather than running a
            // replay of its own. See `replay_pending`.
            if self.replay_line.is_some() {
                self.replay_pending = Some(request);
                return Ok(false);
            }
            self.run_replays(request, syntax_set)?;

            // Restart the current line from the beginning. TEXT-EDITOR FORK:
            // and what ran out on it goes with the reading that ran out. See
            // `skipped_branches`.
            self.skipped_branches.clear();
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
            // TEXT-EDITOR FORK: except at column 0 of an empty line, where the
            // rewind goes to the end of it instead.
            //
            // The alternative being swapped in there is typically a
            // `- match: '' pop: N` — Markdown's `link-def-attr-continuation`
            // failing into `immediately-pop2` — and running that pop at column
            // 0 emits the Pops on the empty line's only character, taking the
            // parent's `meta_scope` off before that line has been scoped at
            // all. Past the end they land on the next line's baseline instead,
            // which is where ST has them: the definition's pop straddles the
            // blank line and the paragraph after it, not the title's line and
            // the blank one. Upstream's #677.
            *start = match match_start_pos == 0 && line.len() <= 1 && line.trim().is_empty() {
                true => line.len(),
                false => match_start_pos,
            };

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

            // TEXT-EDITOR FORK: the trigger's own unwinding comes first, exactly
            // as `push_meta_ops` emits it. See `lookahead_unwind_ops`.
            ops.extend(unwind_ops.iter().cloned());

            // Re-emit the branch_point match's own scopes over their
            // original span. Without this, keywords that trigger a
            // branch (e.g. `LIKE` with
            // `scope: keyword.operator.comparison.sql`) lose their
            // scope whenever alt[0] fails and alt[1..] succeeds,
            // because the original Push/Pop pair was truncated off
            // `ops` together with alt[0]'s subsequent work.
            //
            // The new alternative's `clear_scopes` and `meta_scope`
            // are emitted at `trigger_match_start` *before* the
            // trigger's `pat.scope`, mirroring the non-fail push path
            // in `push_meta_ops` (initial phase): meta_scope sits
            // below the match scope on the stack so the matched text
            // sees both. Placing them at `match_start_pos` would mean
            // the trigger character (e.g. `(` of `for (var i = 0; …)`)
            // never sees the alternative's `meta_scope`. The
            // `meta_content_scope` legitimately stays at
            // `match_start_pos` — mcs only applies after the matched
            // text.
            if let Some(clear_amount) = context.clear_scopes {
                ops.push((trigger_match_start, ScopeStackOp::Clear(clear_amount)));
            }
            for scope in context.meta_scope.iter() {
                ops.push((trigger_match_start, ScopeStackOp::Push(*scope)));
            }
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
            for scope in context.meta_content_scope.iter() {
                ops.push((match_start_pos, ScopeStackOp::Push(*scope)));
            }
        }

        // Clear search cache since we're rewinding.
        search_cache.clear();

        Ok(true)
    }

    /// Which line the parser is on, counting from 0.
    ///
    /// TEXT-EDITOR FORK: `line_number` is a count of `parse_line` calls, which
    /// a cross-line replay does not make. Everything that compares a branch
    /// point's line against "now" has to ask this instead. See `replay_line`.
    /// TEXT-EDITOR FORK: run cross-line replays until no more are asked for.
    ///
    /// One round re-reads `pending_lines[req.from..]` under whatever reading
    /// the caller has just put on the stack, and stops early if a `fail` on one
    /// of those lines asks for another round (`replay_pending`). The rounds are
    /// appended in order, so a later one wins the lines it covers — which is
    /// the point: the reading that survives is the last one chosen, not the
    /// last loop left running.
    ///
    /// An interrupted round keeps what it produced. The lines before the new
    /// round's `from` were read under a branch point that had not been created
    /// yet when they were parsed, so they are still right; the ones from there
    /// on are re-done by the round that follows.
    ///
    /// It terminates because every round is a branch point advancing to its
    /// next alternative or being removed, and neither can go backwards.
    fn run_replays(
        &mut self,
        mut req: ReplayRequest,
        syntax_set: &SyntaxSet,
    ) -> Result<(), ParsingError> {
        loop {
            let corrections = self.run_replay(&req, syntax_set)?;
            self.flushed_ops.extend(corrections);
            match self.replay_pending.take() {
                Some(next) => req = next,
                None => return Ok(()),
            }
        }
    }

    /// One round of the above: each buffered line from `req.from` on, under the
    /// state the caller has restored.
    ///
    /// `pending_lines` is not drained. A second branch point on the current
    /// line that fails after this one needs the same buffered lines, and so
    /// does the next round.
    fn run_replay(
        &mut self,
        req: &ReplayRequest,
        syntax_set: &SyntaxSet,
    ) -> Result<Vec<(usize, Vec<(usize, ScopeStackOp)>)>, ParsingError> {
        let lines: Vec<String> = self.pending_lines[req.from..].to_vec();
        let mut out = Vec::with_capacity(lines.len());
        for (i, line) in lines.iter().enumerate() {
            // TEXT-EDITOR FORK: this buffered line's own number, for as long as
            // it is the one being parsed. Saved and put back, because a `fail`
            // on it reads `cur_line_number`. See `replay_line`.
            let outer = self.replay_line.replace(Replay {
                line_number: (self.line_number - self.pending_lines.len() + req.from + i)
                    .saturating_sub(1),
                pending_index: req.from + i,
                prefix: Vec::new(),
            });
            let line_ops = if i == 0 {
                let mut composed = req.prefix.clone();
                // TEXT-EDITOR FORK: what this line already has, for a branch
                // point created by the re-parse. See `Replay::prefix`.
                if let Some(replay) = self.replay_line.as_mut() {
                    replay.prefix = composed.clone();
                }
                let resume_at = req.resume_at.min(line.len());
                let tail =
                    self.parse_line_inner_from(line, syntax_set, resume_at, req.skip.clone())?;
                composed.extend(tail);
                composed
            } else {
                self.parse_line_inner(line, syntax_set)?
            };
            self.replay_line = outer;
            // The line `pending_lines[from + i]` is: the current line, less the
            // buffered ones before it, plus the offset into that buffer.
            out.push((
                self.line_number - self.pending_lines.len() + req.from + i,
                line_ops,
            ));
            if self.replay_pending.is_some() {
                break;
            }
        }
        Ok(out)
    }

    fn cur_line_number(&self) -> usize {
        match &self.replay_line {
            Some(replay) => replay.line_number,
            None => self.line_number.saturating_sub(1),
        }
    }

    /// Where a branch point created right now starts its replay from.
    ///
    /// TEXT-EDITOR FORK: at the top level that is "everything buffered after
    /// this point", which is `pending_lines.len()` — the line being parsed is
    /// not in the buffer yet. Inside a replay it is the buffer index of the
    /// line being replayed, which *is* in the buffer already: a branch point
    /// on it has to be able to replay it. Recording `len()` there said "there
    /// is nothing to replay", so the next `fail` restored the stack into the
    /// middle of a line it never re-read. See `replay_line`.
    fn pending_lines_mark(&self) -> usize {
        match &self.replay_line {
            Some(replay) => replay.pending_index,
            None => self.pending_lines.len(),
        }
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

    /// TEXT-EDITOR FORK: the ops a `pop: N + push:`/`branch:`/`embed:` emits
    /// for the frames it unwinds, before its trigger takes its own scope.
    ///
    /// The count is a *lookahead* — "the match will not receive the
    /// `meta_scope` of the contexts that are popped" — so the frames come off
    /// in the initial phase, where a `set` leaves the same work until after.
    /// Each cleared frame's Restore goes out before the totals are counted, for
    /// change 6's reason: `clear_scopes` "is applied before `meta_scope` and
    /// `meta_content_scope`", and what is counted here is what each frame
    /// pushed on the way in.
    ///
    /// Two callers, and the second is why this is a function: `push_meta_ops`
    /// emits it when the branch first fires, and `handle_fail` re-emits it when
    /// an alternative is swapped in, because the rewind truncates `ops` back to
    /// *before* the trigger and re-emits the trigger by hand. The unwind is the
    /// trigger's, not the alternative's — `handle_fail` re-pops the same
    /// `pop_count` off the restored stack — so leaving it out of the re-emit
    /// left the ops one reading and the parser's own stack another.
    fn lookahead_unwind_ops(
        &self,
        pop_count: usize,
        index: usize,
        syntax_set: &SyntaxSet,
        ops: &mut Vec<(usize, ScopeStackOp)>,
    ) -> Result<(), ParsingError> {
        let stack_len = self.stack.len();
        let mut atoms = 0usize;
        for depth in 0..pop_count.min(stack_len) {
            let ctx = syntax_set.get_context(&self.stack[stack_len - 1 - depth].context)?;
            if ctx.clear_scopes.is_some() {
                ops.push((index, ScopeStackOp::Restore));
            }
            atoms += ctx.meta_content_scope.len() + ctx.meta_scope.len();
        }
        if atoms > 0 {
            ops.push((index, ScopeStackOp::Pop(atoms)));
        }
        Ok(())
    }

    /// TEXT-EDITOR FORK: unwind the frames *under* the top one, a frame at a
    /// time, for a `pop: N` (or a `pop: N + set:`) that reaches past its own.
    ///
    /// Each frame's `meta_content_scope` comes off, then its `meta_scope`, then
    /// a Restore for its own `clear_scopes` — before the frames beneath it are
    /// touched. The atoms a `clear_scopes` took are the ones the frames below it
    /// pushed, so while it is in force they sit in the clear stack and not on
    /// the live one, and unwinding those frames first Pops that many *live*
    /// atoms from under them. Summing every frame's meta scopes into one `Pop`
    /// cannot express that.
    ///
    /// Three callers, one order: change 21 for a plain `pop: N`, change 23 for a
    /// `pop: N + set:` after its trigger, and change 25 for the same set
    /// *before* its trigger, when the target's `clear_scopes` is emitted early.
    fn unwind_frames_below_top(
        &self,
        count: usize,
        index: usize,
        syntax_set: &SyntaxSet,
        ops: &mut Vec<(usize, ScopeStackOp)>,
    ) -> Result<(), ParsingError> {
        let stack_len = self.stack.len();
        for depth in 1..count.min(stack_len) {
            let level_idx = stack_len - 1 - depth;
            let ctx = syntax_set.get_context(&self.stack[level_idx].context)?;
            let skipped = self.stack[level_idx].mcs_skipped;
            let atoms = ctx.meta_content_scope.len().saturating_sub(skipped);
            if atoms > 0 {
                ops.push((index, ScopeStackOp::Pop(atoms)));
            }
            if !ctx.meta_scope.is_empty() {
                ops.push((index, ScopeStackOp::Pop(ctx.meta_scope.len())));
            }
            if ctx.clear_scopes.is_some() {
                ops.push((index, ScopeStackOp::Restore));
            }
        }
        Ok(())
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
                    // TEXT-EDITOR FORK: from the frame, not from the stack
                    // below it, and only what was actually pushed. See
                    // `StateLevel::mcs_skipped`.
                    let skipped = self
                        .stack
                        .last()
                        .map(|level| level.mcs_skipped)
                        .unwrap_or(0);
                    let atoms = cur_context.meta_content_scope.len().saturating_sub(skipped);
                    if atoms > 0 {
                        ops.push((index, ScopeStackOp::Pop(atoms)));
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
                    // the stack), then meta_scope, then any Restore
                    // paired with that context's own `clear_scopes`
                    // (mirrors the push order Clear → meta_scope → mcs
                    // in reverse). Without the Restore, a `pop: N`
                    // from the top that also unwinds a deeper context
                    // with `clear_scopes` leaves the originally cleared
                    // atoms orphaned — observed on Python f/t-string
                    // interpolation close, where `f-string-replacement-end`
                    // fires `pop: 2` that also pops
                    // `f-string-replacement-meta`.
                    // TEXT-EDITOR FORK: and cur's own `clear_scopes` is
                    // restored here, before the frames underneath are unwound —
                    // not after them, which is where upstream puts it.
                    //
                    // The atoms a `clear_scopes` took are the ones the frames
                    // below pushed, so while it is in force they are in the
                    // clear stack and not on the live one. Unwinding those
                    // frames first therefore Pops that many *live* atoms from
                    // under them, and the Restore then puts the cleared ones
                    // back on top of the damage.
                    //
                    // Haskell's string continuation is the file:
                    // `literal-string-continuation` has `clear_scopes: 2` over
                    // `literal-string-body`'s two-atom `meta_scope`, and its
                    // `- match: \" … pop: 2` emitted `Pop(1) Pop(2) Restore`
                    // where the live stack had one atom to give — the corpus's
                    // last scope-stack divergence, `pop of 2 with 1 on the
                    // stack`. The line after an unterminated continuation then
                    // stayed inside `meta.string.haskell`.
                    //
                    // This is change 6's ordering — "clear_scopes is applied
                    // before meta_scope and meta_content_scope", so it is undone
                    // after them — for a plain `pop: N`, where change 6 did it
                    // for `pop: N + set:`. `pop: 1` is unaffected either way:
                    // the loop below is empty, so the two positions coincide.
                    if pop_count > 1 && cur_context.clear_scopes.is_some() {
                        ops.push((index, ScopeStackOp::Restore));
                    }
                    // TEXT-EDITOR FORK: likewise — see above, and
                    // `unwind_frames_below_top`, which is this loop's one copy.
                    self.unwind_frames_below_top(pop_count, index, syntax_set, ops)?;
                }

                // cleared scopes are restored after the scopes from match pattern that invoked the pop are applied
                // TEXT-EDITOR FORK: unless the unwinding above has already put
                // them back, which it does whenever there are frames under this
                // one whose atoms the clear is holding. See there.
                if !initial && pop_count <= 1 && cur_context.clear_scopes.is_some() {
                    ops.push((index, ScopeStackOp::Restore))
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
                // TEXT-EDITOR FORK: `pop: N + push:` is a *lookahead*, so the
                // frames it unwinds are unwound **before** the matched text takes
                // its scope — all of it in the initial phase, where a `set` leaves
                // the same work until after.
                //
                // Sublime: "for `push`, `embed` and `branch` actions, the pop
                // treats the match as if it were a lookahead, which means the
                // match will not receive the `meta_scope` of the contexts that are
                // popped." Asked of ST directly, with three frames and a
                // `pop: 2 + push:` on the innermost, the trigger token comes out
                //
                //     source meta.tail keyword.trigger
                //
                // — the target's `meta_scope` and not one atom of the two frames
                // being popped. The same shape with `set:` comes out
                //
                //     source meta.outer meta.mid meta.tail keyword.trigger
                //
                // which is every frame plus the target, and is what this parser
                // already did for `set` — so the second sentence of that
                // paragraph ("any match will receive the `meta_scope` of the
                // context being popped *and* the context being pushed") is about
                // `set`, and upstream's #683, which would take the deeper frames
                // off a `pop: N + set:` trigger, is contradicted by the
                // application.
                let push_pop_count = match *match_op {
                    MatchOperation::Push { pop_count, .. } => pop_count,
                    _ => 0,
                };
                if initial && push_pop_count > 0 {
                    self.lookahead_unwind_ops(push_pop_count, index, syntax_set, ops)?;
                }
                // a match pattern that "set"s keeps the meta_content_scope and meta_scope from the previous context
                if initial {
                    // v2: pop parent's meta_content_scope so matched text does not see it
                    //
                    // TEXT-EDITOR FORK: unless the context below is an
                    // `embed_scope` wrapper, in which case this context is an
                    // embedded syntax's `main` and its `meta_content_scope`
                    // was *replaced* by the host's `embed_scope` — the push
                    // path skips it on `embed_scope_replaces` alone, so
                    // popping it here takes an atom off that nothing put on.
                    //
                    // Markdown embedding Bash is the file: the first `set:`
                    // inside a ```bash fence took `source.shell.bash` off, and
                    // from there every frame was one out — the fence's escape
                    // then popped `text.html.markdown`, the document's own
                    // root scope, and a second fence over-popped an empty
                    // stack. ST keeps `source.shell.bash` on the stack for the
                    // whole fence and `text.html.markdown` after it
                    // (`tools/sublime-oracle/probes/mdfence.probe`).
                    let skipped = self
                        .stack
                        .last()
                        .map(|level| level.mcs_skipped)
                        .unwrap_or(0);
                    let atoms = cur_context.meta_content_scope.len().saturating_sub(skipped);
                    if is_set && version >= 2 && atoms > 0 {
                        ops.push((index, ScopeStackOp::Pop(atoms)));
                    }

                    // TEXT-EDITOR FORK: change 25. A `pop: N + set:` whose
                    // target clears carries its trigger on the *unwound* stack.
                    //
                    // The frames a `pop: N + set:` unwinds do reach its trigger
                    // token — ST answers `source meta.outer meta.mid meta.tail
                    // keyword.trigger` for three frames and a `pop: 2 + set:`
                    // (`tools/sublime-oracle/probes/PopPush.sublime-syntax`),
                    // which is why they are unwound after the token and not
                    // before it. But when the *target* carries a `clear_scopes`
                    // that is emitted early (see `early_set_clear`), the clear
                    // and the frames it is counted against are on the same
                    // stack, and ST has unwound them first.
                    //
                    // `(?ix:` in `RegExp (Basic)` is the file, asked directly:
                    // `group-start` fires `pop: 3, set: [group-body-extended,
                    // maybe-unexpected-quantifiers]` and ST scopes the trigger
                    //
                    //     source.regexp meta.group.regexp meta.mode.extended.regexp
                    //         storage.modifier.mode.regexp
                    //
                    // — one `meta.group.regexp`, the target's. Ours had two:
                    // the target's `meta_scope` pushed over the leaving
                    // `group-body`'s, whose second atom the target's
                    // `clear_scopes: 1` ate instead of the one ST's clear
                    // reaches. Unwinding first puts the clear over the same
                    // atom ST's is over, and the column after the trigger — and
                    // every other column of
                    // `tools/sublime-oracle/probes/regexp_modes.probe` — is
                    // where it already was.
                    //
                    // `early_set_clear` guarantees `cur_context` has no meta
                    // scopes of its own here, so nothing of the leaving context
                    // is being taken off the trigger; its `clear_scopes` is put
                    // back first, for the reason the unwinding is a frame at a
                    // time at all.
                    // Version 1 never emits a Clear before the trigger, so
                    // there is nothing early about it there.
                    let early_clear = if version >= 2 {
                        early_set_clear(is_set, cur_context, context_refs, syntax_set)?
                    } else {
                        None
                    };
                    let early_unwind = set_pop_count > 1 && early_clear.is_some();
                    if early_unwind {
                        if cur_context.clear_scopes.is_some() {
                            ops.push((index, ScopeStackOp::Restore));
                        }
                        self.unwind_frames_below_top(set_pop_count, index, syntax_set, ops)?;
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
                        // Multi-context `set:` keeps Clear in the non-initial
                        // phase (emitted inline after preceding contexts'
                        // mcs pushes). Moving it to the initial phase here
                        // would strip atoms from below the outer mcs rather
                        // than from the top of the just-pushed inner mcs
                        // stack — Makefile's `set: [value-to-be-defined,
                        // eat-whitespace-then-pop]` relies on Clear eating
                        // the last-pushed mcs atom, which Restore then
                        // replaces when eat-whitespace-then-pop pops.
                        // TEXT-EDITOR FORK: not when the context being *left*
                        // carries meta scopes of its own.
                        //
                        // Emitting the target's Clear here is right when the
                        // visible top of the stack is the parent atom the Clear is
                        // meant to strip. When `cur_context` has a `meta_scope` or
                        // `meta_content_scope`, that is what sits on top instead,
                        // so the Clear hides *those*; the non-initial `Pop` below
                        // then takes its count from the atoms underneath them, and
                        // the trailing `Restore` puts cur's own meta scope back —
                        // exactly where the parent's atom should have been.
                        //
                        // Bash `: ~/` is the case. ST scopes the `/` as
                        // `meta.string.glob.shell string.unquoted.shell`; this
                        // emitted `meta.interpolation.tilde.shell
                        // string.unquoted.shell`, the tilde interpolation's own
                        // scope left behind by that swap. Asked of ST directly,
                        // `tools/sublime-oracle/probes/bash_tilde.probe`.
                        //
                        // Deferring it to the non-initial phase is enough: the two
                        // phases share this predicate, and the one that does not
                        // emit here emits there. Upstream's #669.
                        //
                        // TEXT-EDITOR FORK: and a multi-context `set:` is the
                        // same question one target along — see
                        // `early_set_clear`, which is what decides both phases
                        // now. A single-context `set:` is the case where its
                        // answer is `Some(0)`.
                        // TEXT-EDITOR FORK: change 31 — a version 1 `push:`
                        // of several contexts clears their sum, once, first.
                        // See `summed_push_clear`.
                        let summed = if version < 2 && !is_set {
                            summed_push_clear(context_refs, syntax_set)?
                        } else {
                            None
                        };
                        if let Some(clear_amount) = summed {
                            ops.push((index, ScopeStackOp::Clear(clear_amount)));
                        }
                        for (i, r) in context_refs.iter().enumerate() {
                            let ctx = r.resolve(syntax_set)?;

                            let emit_clear_here =
                                (!is_set && version >= 2) || early_clear == Some(i);
                            if emit_clear_here {
                                if let Some(clear_amount) = ctx.clear_scopes {
                                    ops.push((index, ScopeStackOp::Clear(clear_amount)));
                                }
                            }

                            for scope in ctx.meta_scope.iter() {
                                ops.push((index, ScopeStackOp::Push(*scope)));
                            }
                        }
                    } else {
                        // TEXT-EDITOR FORK: change 31, the other phase of it.
                        let summed = if version < 2 && !is_set {
                            summed_push_clear(context_refs, syntax_set)?
                        } else {
                            None
                        };
                        if let Some(clear_amount) = summed {
                            ops.push((index, ScopeStackOp::Clear(clear_amount)));
                        }
                        for r in context_refs.iter() {
                            let ctx = r.resolve(syntax_set)?;

                            if !is_set && version >= 2 {
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
                    // TEXT-EDITOR FORK: change 31 — the summed clear belongs to
                    // the trigger token and to nothing after it.
                    //
                    // ST scopes `abc` in the documentation's example
                    // `meta.ctx2 meta.ctx3 identifier`, with the summed clear
                    // in force, and the column *after* it
                    // `source.lang meta.ctx3` — which is where version 2's
                    // interleaved clears land. So the two phases answer
                    // differently at version 1: the initial one clears the sum
                    // and pushes the meta scopes over it, and this one puts
                    // that back and applies them one context at a time, as
                    // version 2 does throughout. `multiclear_v1`'s last three
                    // columns are the record.
                    let summed = if version < 2 && !is_set {
                        summed_push_clear(context_refs, syntax_set)?
                    } else {
                        None
                    };
                    if summed.is_some() {
                        let pushed: usize = context_refs
                            .iter()
                            .map(|r| {
                                r.resolve(syntax_set)
                                    .map(|ctx| ctx.meta_scope.len())
                                    .unwrap_or(0)
                            })
                            .sum();
                        if pushed > 0 {
                            ops.push((index, ScopeStackOp::Pop(pushed)));
                        }
                        ops.push((index, ScopeStackOp::Restore));
                        for r in context_refs.iter() {
                            let ctx = r.resolve(syntax_set)?;
                            if let Some(clear_amount) = ctx.clear_scopes {
                                ops.push((index, ScopeStackOp::Clear(clear_amount)));
                            }
                            for scope in ctx.meta_scope.iter() {
                                ops.push((index, ScopeStackOp::Push(*scope)));
                            }
                            for scope in ctx.meta_content_scope.iter() {
                                ops.push((index, ScopeStackOp::Push(*scope)));
                            }
                        }
                        return Ok(());
                    }

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
                            // For `pop: N + set:` (set_pop_count > 1) the N-1
                            // deeper contexts are unwound too — one frame at a
                            // time, below, rather than counted in here, because
                            // a frame with `clear_scopes` has to put its atoms
                            // back before the frames under it are touched. See
                            // there.
                        }

                        // TEXT-EDITOR FORK: for `pop: N + set:` with N > 1, the
                        // Restore comes *before* the Pop.
                        //
                        // Sublime documents the order: `clear_scopes` "is applied
                        // before `meta_scope` and `meta_content_scope`". So by the
                        // time the meta scopes of the frames being unwound are
                        // accounted for, the atoms `clear_scopes` took must be
                        // back where they were. With a `clear_scopes:` on the
                        // leaving context, part of what is about to be popped is
                        // sitting in `clear_stack` rather than on the live
                        // stack, so a Pop that counted it eats that many *live*
                        // atoms and takes them from frames below the ones being
                        // popped. (The frames *under* the replaced one are
                        // unwound one at a time further down, for the same
                        // reason taken one step further.)
                        //
                        // Batch File is where it shows: `cmd-set-quoted-value-
                        // inner-end` has `clear_scopes: 1` and fires
                        // `pop: 2, set: ignored-tail-outer`, and every
                        // `set "var"=...` line lost `meta.command.set.dosbatch`
                        // over its trailing content — 231 failing assertions in
                        // one file, the whole of that language's failures.
                        //
                        // Plain `set:` keeps Pop-then-Restore. There is nothing
                        // below its single frame for an over-Pop to reach, and the
                        // order there is load-bearing the other way: the restored
                        // atoms are meant to land *below* the target's upcoming
                        // meta_scope push, which is what
                        // `v2_set_to_target_with_clear_scopes_clears_parent_meta_content_scope`
                        // (the Lisp `defun` shape) pins.
                        //
                        // This is upstream's #646, taken alone.
                        // TEXT-EDITOR FORK: a target's `clear_scopes` emitted
                        // in the initial phase (see `early_set_clear`) has
                        // already eaten atoms, and when it sits past the first
                        // target those atoms are `meta_scope`s that
                        // `num_to_pop` above counts — so a Pop of the full
                        // count would take that many *live* atoms from frames
                        // below. Undo it first and let the re-push below emit
                        // it again in its place: one Clear is live either way,
                        // and the clear stack balances. `Some(0)` needs none of
                        // this, because a Clear before any of this set's own
                        // pushes ate nothing that `num_to_pop` is about.
                        let early_clear = if version >= 2 {
                            early_set_clear(is_set, cur_context, context_refs, syntax_set)?
                        } else {
                            None
                        };
                        // TEXT-EDITOR FORK: change 25 — what the initial phase
                        // has already done. See there.
                        let early_unwind = set_pop_count > 1 && early_clear.is_some();
                        //
                        // TEXT-EDITOR FORK: and `Some(0)` needs it too once the
                        // set is unwinding more than its own frame. The atom
                        // that Clear ate then belongs to one of the frames
                        // below — the one this set is replacing — and that
                        // frame's `meta_scope` is popped here, so the atom has
                        // to be back on the stack to be popped. `(?ix:` in a
                        // regex is the file: `group-start` fires
                        // `pop: 3, set: [group-body-extended, …]` into a target
                        // whose `clear_scopes: 1` had already taken the group
                        // body's `meta.mode.extended.regexp`.
                        let undo_early_clear = early_clear.is_some_and(|i| i > 0);
                        if undo_early_clear {
                            ops.push((index, ScopeStackOp::Restore));
                        }

                        // TEXT-EDITOR FORK: unless the initial phase restored
                        // it already, on its way to unwinding the frames under
                        // this one before the trigger — change 25.
                        let restore =
                            is_set && cur_context.clear_scopes.is_some() && !early_unwind;
                        let restore_first = restore && set_pop_count > 1;
                        if restore_first {
                            ops.push((index, ScopeStackOp::Restore));
                        }

                        // do all the popping as one operation
                        if num_to_pop > 0 {
                            ops.push((index, ScopeStackOp::Pop(num_to_pop)));
                        }

                        // TEXT-EDITOR FORK: then the frames a `pop: N + set:`
                        // unwinds under the one it replaces — one at a time,
                        // and each one's own `clear_scopes` put back before the
                        // frames beneath it are touched.
                        //
                        // This is change 21's ordering for the `set:` path: the
                        // atoms a `clear_scopes` took are the ones the frames
                        // below pushed, so while it is in force they sit in the
                        // clear stack and not on the live one, and unwinding
                        // those frames first Pops that many *live* atoms from
                        // under them. Counting every frame's `meta_scope` into
                        // one `Pop` — which is what this did — cannot express
                        // that, and in `RegExp (Basic)` it ate the embedded
                        // syntax's own `source.regexp` off the bottom: the
                        // corpus' last scope-stack divergence and Python's last
                        // failing assertion, both at the close of a `(?ix: … )`.
                        // TEXT-EDITOR FORK: unless the initial phase already
                        // did it, which it does when the target's own
                        // `clear_scopes` is emitted before the trigger — see
                        // `early_unwind` there, and change 25.
                        if is_set && set_pop_count > 1 && !early_unwind {
                            self.unwind_frames_below_top(
                                set_pop_count,
                                index,
                                syntax_set,
                                ops,
                            )?;
                        }

                        // Restore scopes cleared by the leaving context, now that
                        // cur.meta_scope and the initial phase's target.meta_scope
                        // push have been popped off. The restored atoms land below
                        // the target's upcoming meta_scope / meta_content_scope push.
                        if restore && !restore_first {
                            ops.push((index, ScopeStackOp::Restore));
                        }

                        // now we push meta scope and meta context scope for each context pushed
                        if version >= 2 {
                            // v2: For multi-context `set:`, Clear is emitted
                            // here so it strips the topmost just-pushed mcs
                            // atom (as Sublime does for multi-context set).
                            // Single-context `set:` emitted its Clear earlier
                            // in the initial phase (so the trigger token sees
                            // the cleared stack); re-emitting here would
                            // double-push onto clear_stack and cause Pop
                            // underflow when the context unwinds.
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
                            // The same predicate as the initial phase, so
                            // whichever phase does not emit the Clear, the other
                            // one does — see the reasoning there.
                            let mut prev_embed_scope_replaces = false;
                            for r in context_refs.iter() {
                                let id = r.id()?;
                                let ctx = syntax_set.get_context(&id)?;

                                // TEXT-EDITOR FORK: `Some(0)` is the one the
                                // initial phase emitted and left standing; any
                                // other early Clear was undone by the Restore
                                // above and is emitted here, in its place.
                                if is_set && (early_clear != Some(0) || undo_early_clear) {
                                    if let Some(clear_amount) = ctx.clear_scopes {
                                        ops.push((index, ScopeStackOp::Clear(clear_amount)));
                                    }
                                }

                                for scope in ctx.meta_scope.iter() {
                                    ops.push((index, ScopeStackOp::Push(*scope)));
                                }
                                // v2: what the previous context's `embed_scope`
                                // replaces is not pushed — the embedded
                                // syntax's own scope, and only that. See
                                // `mcs_replaced_by_embed_scope`.
                                let skipped = mcs_replaced_by_embed_scope(
                                    prev_embed_scope_replaces,
                                    &id,
                                    ctx,
                                    syntax_set,
                                );
                                for scope in ctx.meta_content_scope.iter().skip(skipped) {
                                    ops.push((index, ScopeStackOp::Push(*scope)));
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
                // TEXT-EDITOR FORK: `pop: N + embed:` is a lookahead too — the
                // reference names all three, "for `push`, `embed` and `branch`
                // actions" — so the synthetic is a `Push` carrying the count and
                // not a `Set`. ST, on `efZVVV` in `PopPush.sublime-syntax`:
                // `source meta.tail.embed keyword.trigger.embed`, with neither
                // unwound frame's `meta_scope` on the trigger.
                let synthetic = if pop_count > 0 {
                    MatchOperation::Push {
                        ctx_refs: contexts.clone(),
                        pop_count,
                    }
                } else {
                    MatchOperation::Push { ctx_refs: contexts.clone(), pop_count: 0 }
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
                // Branch acts like Push for meta ops purposes, with the count
                // when there is one (TEXT-EDITOR FORK: it used to become a `Set`
                // — see the exec-time site above for what ST says). At exec time
                // Branch is transformed into a synthetic Push before calling
                // push_meta_ops, so this arm is a safety fallback.
                let synthetic = if pop_count > 0 {
                    MatchOperation::Push {
                        ctx_refs: alternatives.clone(),
                        pop_count,
                    }
                } else {
                    MatchOperation::Push { ctx_refs: alternatives.clone(), pop_count: 0 }
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
        // TEXT-EDITOR FORK: change 34, filled in by the `Embed` arm below.
        let mut embed_holder: Option<(ContextId, usize)> = None;
        let (ctx_refs, old_proto_ids, is_embed) = match pat.operation {
            MatchOperation::Push {
                ref ctx_refs,
                pop_count,
            } => {
                // TEXT-EDITOR FORK: `pop: N + push:`. The stack effect is a
                // `Set`'s — pop N, then push — and only the scopes the trigger
                // token sees differ (see `push_meta_ops`). The prototypes are
                // *not* carried forward the way `Set` carries them: a `push` is
                // entering a new context, not replacing the one being left.
                if pop_count > 0 {
                    for _ in 0..pop_count {
                        self.stack.pop();
                    }
                    let final_len = self.stack.len() + ctx_refs.len();
                    // TEXT-EDITOR FORK: see `branch_point_is_live`. This is
                    // where a `pop: N + branch:` threw away the branch point it
                    // had just created — change 7 makes that a synthetic `Push`,
                    // and `stack_depth <= final_len` reads the pre-pop depth as
                    // if it were the alternative's.
                    self.branch_points
                        .retain(|bp| branch_point_is_live(bp, final_len));
                    self.escape_stack.retain(|e| e.stack_depth < final_len);
                }
                (ctx_refs, None, false)
            }
            MatchOperation::Embed {
                ref contexts,
                pop_count,
                ..
            } => {
                if pop_count > 0 {
                    // TEXT-EDITOR FORK: change 34. Remember the frame this
                    // `embed:` was declared in before unwinding it: its meta
                    // scopes are on the escape, and nothing on the stack will
                    // be able to say so by then.
                    embed_holder = self.stack.last().map(|l| (l.context, l.mcs_skipped));
                    for _ in 0..pop_count {
                        self.stack.pop();
                    }
                    // TEXT-EDITOR FORK: see `branch_point_is_live`. Against the
                    // length *before* the contexts go on, as upstream has it —
                    // widening this to the post-push length the way the `push`
                    // and `set` arms count hangs `JavaScript (for PHP
                    // double-quoted)` outright, so what those two do is not
                    // simply right and this is not simply an inconsistency.
                    let stack_len = self.stack.len();
                    self.branch_points
                        .retain(|bp| branch_point_is_live(bp, stack_len));
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
                // length (after the common push loop below), so a branch_point
                // captured at the pre-set depth survives a pop-1 + push-1 set
                // — the depth the bp references is still valid after the push.
                let final_len = self.stack.len() + ctx_refs.len();
                // TEXT-EDITOR FORK: see `branch_point_is_live`.
                self.branch_points
                    .retain(|bp| branch_point_is_live(bp, final_len));
                self.escape_stack.retain(|e| e.stack_depth < final_len);
                (ctx_refs, old_proto_ids, false)
            }
            MatchOperation::Pop(n) => {
                for _ in 0..n {
                    self.stack.pop();
                }
                // Invalidate branch points whose alternative this pop took
                // away. TEXT-EDITOR FORK: see `branch_point_is_live`.
                let stack_len = self.stack.len();
                self.branch_points
                    .retain(|bp| branch_point_is_live(bp, stack_len));
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

        // TEXT-EDITOR FORK: mirrors the `prev_embed_scope_replaces` walk in
        // `push_meta_ops`, which is what decides whether a frame's
        // `meta_content_scope` is pushed at all. The two loops have to agree,
        // so the flag is set here from the same predicate rather than
        // reconstructed from the stack afterwards. See `StateLevel`.
        let mut prev_embed_scope_replaces = false;

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
            let mcs_skipped = mcs_replaced_by_embed_scope(
                prev_embed_scope_replaces,
                &context_id,
                context,
                syntax_set,
            );
            prev_embed_scope_replaces = context.embed_scope_replaces;
            self.stack.push(StateLevel {
                context: context_id,
                prototypes: proto_ids,
                captures,
                mcs_skipped,
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
                    // TEXT-EDITOR FORK: change 34.
                    holder: embed_holder,
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
        match_end: usize,
        regions: &Region,
        syntax_set: &SyntaxSet,
        ops: &mut Vec<(usize, ScopeStackOp)>,
    ) -> Result<(), ParsingError> {
        let entry = &self.escape_stack[escape_idx];
        let target_depth = entry.stack_depth;
        let escape_captures = entry.captures.clone();
        // TEXT-EDITOR FORK: change 34. See below.
        let holder = entry.holder;

        // Pop all stack levels down to target_depth, emitting proper meta scope pops
        while self.stack.len() > target_depth {
            let level = &self.stack[self.stack.len() - 1];
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
            {
                // TEXT-EDITOR FORK: the frame says how much of its
                // `meta_content_scope` was pushed; the stack underneath cannot.
                // See `StateLevel::mcs_skipped`.
                let atoms = ctx.meta_content_scope.len().saturating_sub(level.mcs_skipped);
                if atoms > 0 {
                    ops.push((match_start, ScopeStackOp::Pop(atoms)));
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

        // TEXT-EDITOR FORK: change 34. A `pop: N + embed:`'s escape still wears
        // the meta scopes of the context the `embed:` was written in.
        //
        // The reference's own example under "Using pop with another action" is
        // the case: a `paragraph` context carrying `meta_scope`, whose ```py
        // fence fires `pop: 1` and embeds Python. ST scopes the closing fence
        // `source.lang meta.para` and this had it bare
        // (`probes/reference/action_pop_embed.sublime-syntax`, and the columns
        // either side of it agree — the opening fence is a lookahead and wears
        // none of it, the embedded text is outside it, and the character after
        // the escape is back in `main`).
        //
        // So the frame is *gone* for everything except this one token. It is
        // not deferred: the embedded text is scoped `source.lang m.embedded`
        // with no `m.para` in it, which is what says the `pop:` really did
        // happen at the trigger. `holder` is the frame it took off, remembered
        // at the pop for exactly this, and version 1 wants none of it — change
        // 32 is that half, and there the holder is off the stack already.
        let v1 = self.current_syntax_version(syntax_set) < 2;
        let holder_metas = match (v1, holder) {
            (false, Some((context_id, mcs_skipped))) => {
                let ctx = syntax_set.get_context(&context_id)?;
                let mut pushed = 0;
                for scope in ctx.meta_scope.iter() {
                    ops.push((match_start, ScopeStackOp::Push(*scope)));
                    pushed += 1;
                }
                for scope in ctx.meta_content_scope.iter().skip(mcs_skipped) {
                    ops.push((match_start, ScopeStackOp::Push(*scope)));
                    pushed += 1;
                }
                pushed
            }
            _ => 0,
        };

        // TEXT-EDITOR FORK: change 32. In a version 1 syntax the escape's own
        // text is outside the context that carries the `embed:` — it gets
        // neither that context's `meta_scope` nor its `meta_content_scope` —
        // and this gave it both.
        //
        // Sublime lists it under Compatibility as "Embed Escape Match and Meta
        // Scopes": the closing `'` of `'abc'` should come out `meta.group
        // meta.content punctuation.end` and in version 1 comes out
        // `punctuation.end` alone. ST 4206 agrees at both versions
        // (`tools/sublime-oracle/probes/VerEscapeV1.sublime-syntax`, the
        // `escape_v1` and `escape_v2` jobs), and it is only the token: the
        // column after it is back inside `meta.group meta.content`.
        //
        // So they come off before the escape's captures and go back on after
        // it — the shape a `set:`'s meta scopes already have around their
        // trigger — off the frame the `embed:` fired from, which is the one
        // the escape has just popped back to. `mcs_skipped` is what says how
        // much of its `meta_content_scope` was ever pushed (change 11): a v2
        // host embedding through an `embed_scope` never pushed all of it.
        let v1_host_metas = if v1 {
            match self.stack.last() {
                Some(level) => {
                    let ctx = syntax_set.get_context(&level.context)?;
                    let mcs = ctx.meta_content_scope.len().saturating_sub(level.mcs_skipped);
                    let ms = ctx.meta_scope.len();
                    if mcs > 0 {
                        ops.push((match_start, ScopeStackOp::Pop(mcs)));
                    }
                    if ms > 0 {
                        ops.push((match_start, ScopeStackOp::Pop(ms)));
                    }
                    Some((level.context, level.mcs_skipped))
                }
                None => None,
            }
        } else {
            None
        };

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

        // TEXT-EDITOR FORK: change 34 — and off again after the token.
        if holder_metas > 0 {
            ops.push((match_end, ScopeStackOp::Pop(holder_metas)));
        }

        // TEXT-EDITOR FORK: change 32 — and back on, after the escape's own
        // text, in the order they were pushed the first time.
        if let Some((context_id, mcs_skipped)) = v1_host_metas {
            let ctx = syntax_set.get_context(&context_id)?;
            for scope in ctx.meta_scope.iter() {
                ops.push((match_end, ScopeStackOp::Push(*scope)));
            }
            for scope in ctx.meta_content_scope.iter().skip(mcs_skipped) {
                ops.push((match_end, ScopeStackOp::Push(*scope)));
            }
        }

        // Remove this escape entry and any inner (later) escape entries
        self.escape_stack.truncate(escape_idx);

        // Invalidate branch points whose alternative this escape has just
        // popped away. TEXT-EDITOR FORK: asked the one way — see
        // `branch_point_is_live`.
        let stack_len = self.stack.len();
        self.branch_points
            .retain(|bp| branch_point_is_live(bp, stack_len));

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

        // The embedded SQL arrives with *one* scope of its own, not the chain of
        // its ancestors': `SQL (Basic)` extends `SQL (MySQL)` extends `SQL`, and
        // a child that inherits `main` used to keep every parent's file scope in
        // front of its own — `source.sql source.sql.mysql source.sql.basic`,
        // where Sublime Text pushes the child's alone. See the
        // `add_initial_contexts` regeneration in `resolve_extends`.
        //
        // Here that is also why nothing of the guest's is left: this is a
        // version 2 `embed_scope`, which *replaces* the embedded syntax's own
        // scope. Replacing one atom of three left two behind.
        assert_eq!(
            ops(&mut state, "wow", ss),
            vec![
                (0, Push(Scope::new("meta.string.heredoc.ruby").unwrap())),
                (0, Push(Scope::new("source.sql.embedded.ruby").unwrap()),),
                (0, Push(Scope::new("meta.column-name.sql").unwrap())),
                (3, Pop(1)),
            ]
        );

        assert_eq!(
            ops(&mut state, "SQL", ss),
            vec![
                (0, Pop(1)),
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
        // `ParseState` equality checks the stack, active branch points,
        // and the buffered `pending_lines` used for cross-line branch
        // replay. Because `class Foo {` opens a still-unresolved branch
        // (`declarations`), the literal source text is retained in
        // `pending_lines`, so two states that parsed the same syntactic
        // shape with different identifiers (e.g. `Foo` vs `Bar`) compare
        // unequal today — unlike earlier versions of this test. Keep the
        // two inputs identical here and assert the remaining invariants:
        // identical inputs -> equal states, advancing one -> divergence.
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
    fn non_consuming_pop_n_below_pre_push_depth_is_not_a_loop() {
        // Mirror of the Haskell `declaration-type-end` branch where the
        // fallback alternative is `immediately-pop2` (empty match with
        // `pop: 2`). The outer wrapper's meta_scope must come off when
        // the pop-2 fallback fires — pre-fix, the loop guard flagged
        // any non-consuming `pop` after a non-consuming push as
        // looping, so the parser advanced one char past the branch and
        // the pop-2 fired at the wrong column, leaving the wrapper's
        // scope covering the trailing `y` token.
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: open
      scope: test.open
      push: wrapper
    - match: y
      scope: test.main.y
    - match: z
      scope: test.main.z
  wrapper:
    - meta_scope: test.wrapper
    - match: ""
      branch_point: fallback
      branch:
        - try
        - give-up
  try:
    - match: x
      scope: test.try.match
    - match: (?=y)
      fail: fallback
  give-up:
    - match: ""
      pop: 2
"#;
        // With the fix, `give-up` fires pop-2 at column 4 (the fail
        // position), unwinding both `give-up` and `wrapper`; `y` is
        // then scoped by `main`'s rule. Without the fix, would_loop
        // advanced start past column 4, the pop-2 fired at column 5,
        // and `y` stayed inside the wrapper and never matched
        // `test.main.y`.
        expect_scope_stacks("openyz", &["<source.test>, <test.main.y>"], syntax);
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

    /// Ruby's `?\u{012ACF 0gxs}`: `\h{0,6}` can match zero-width at the
    /// space. Without the `FIND_NOT_EMPTY` engine option, the zero-width
    /// match wins and hides the later non-empty match of `0`, which then
    /// falls through to the `\S` fallback. With the option on
    /// `MatchOperation::None` patterns, the engine retries past the
    /// zero-width position and matches `0` as `number.hex`.
    #[test]
    fn scope_only_pattern_that_matches_zero_width_finds_later_non_empty() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: \h{0,6}
      scope: number.hex
    - match: \S
      scope: invalid.illegal
"#;

        let line = "012ACF 0gxs";
        let expect = [
            "<source.test>, <number.hex>",      // "012ACF" and "0"
            "<source.test>, <invalid.illegal>", // "g", "x", "s"
        ];
        expect_scope_stacks(line, &expect, syntax);
    }

    /// Cabal's `\|\||&&||!` operator regex has a stray empty alternative
    /// between `&&` and `!`. Under leftmost-first matching the empty alt
    /// wins zero-width at the `!` position. With `FIND_NOT_EMPTY`, the
    /// engine rejects the empty alt and matches `!` via the real
    /// alternative.
    #[test]
    fn scope_only_pattern_with_middle_empty_alt_matches_bang() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: \|\||&&||!
      scope: keyword.operator
"#;
        let line = "!";
        let expect = ["<source.test>, <keyword.operator>"];
        expect_scope_stacks(line, &expect, syntax);
    }

    /// Rust's `prelude_types: (?x:|Box|Option|…)` puts a `|` before every
    /// alternative, including the first. Under leftmost-first the leading
    /// empty alt wins zero-width at every position, so `\b(?x:|Box|Vec)\b`
    /// never matches `Box` or `Vec`. With `FIND_NOT_EMPTY`, the engine
    /// rejects the zero-width alt and matches `Vec` via the real
    /// alternative.
    #[test]
    fn scope_only_pattern_with_leading_empty_alt_in_group_matches_name() {
        let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: \b(?x:|Box|Vec)\b
      scope: support.type
"#;
        let line = "Vec";
        let expect = ["<source.test>, <support.type>"];
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
    /// Regression guard for the "`meta_append` / `meta_prepend` resets
    /// `meta_include_prototype` to its default" bug: the SQL base
    /// declares `inside-like-single-quoted-string` with
    /// `meta_include_prototype: false` so its `--` comment rule (from
    /// the SQL prototype) does NOT fire inside LIKE strings. TSQL extends
    /// that context with `meta_append: true` to add a `[…]` character-set
    /// rule, but doesn't restate `meta_include_prototype: false`. Before
    /// the fix, the merge in `syntax_set.rs` left the child's default
    /// `meta_include_prototype: true`, so the SQL prototype attached to
    /// the merged context, and `--` inside LIKE strings was scoped as
    /// a comment — 4,918 cascading assertion failures in
    /// `syntax_test_tsql.sql`.
    ///
    /// Synthetic shape: a parent with a `prototype` matching `--` as a
    /// comment, a base context with `meta_include_prototype: false`,
    /// and a child that extends the parent and `meta_append`s a single
    /// rule to that base context without restating
    /// `meta_include_prototype`. After merge, `--` inside the base
    /// context's matched span must NOT take a `comment.*` scope.
    #[test]
    fn meta_append_inherits_meta_include_prototype_from_parent() {
        use crate::parsing::syntax_set::SyntaxSetBuilder;

        let dir =
            std::env::temp_dir().join(format!("syntect-meta-append-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("parent.sublime-syntax"),
            r#"
name: Parent
scope: source.parent
file_extensions: [parent]
contexts:
  prototype:
    - match: '--'
      scope: punctuation.definition.comment
      push: comment-body
  comment-body:
    - meta_scope: comment.line
    - match: $
      pop: 1
  main:
    - match: \bopen\b
      push: inside
  inside:
    - meta_include_prototype: false
    - meta_scope: meta.inside
    - match: \bclose\b
      pop: 1
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("child.sublime-syntax"),
            r#"
name: Child
scope: source.child
file_extensions: [child]
extends: parent.sublime-syntax
contexts:
  inside:
    - meta_append: true
    - match: '!'
      scope: punctuation.bang.child
"#,
        )
        .unwrap();
        let mut builder = SyntaxSetBuilder::new();
        builder.add_from_folder(&dir, true).unwrap();
        let ss = builder.build();
        let syntax = ss
            .find_syntax_by_scope(Scope::new("source.child").unwrap())
            .unwrap();
        let mut state = ParseState::new(syntax);
        let o = ops(&mut state, "open -- close\n", &ss);
        let _ = std::fs::remove_dir_all(&dir);
        let comment_pushes = o
            .iter()
            .filter(|(_, op)| {
                matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("comment"))
            })
            .count();
        assert_eq!(
            comment_pushes, 0,
            "`--` inside the inside context (meta_include_prototype: false in parent) \
             must not match the parent's prototype comment rule after meta_append merge; \
             ops were: {:?}",
            o
        );
    }

    /// `main` must be idempotent.
    #[test]
    fn extending_syntax_does_not_double_push_top_level_scope() {
        use crate::parsing::SyntaxSet;
        let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        // Git Diff extends Diff (Basic) — a concrete case of the bug.
        let syntax = ss.find_syntax_by_name("Git Diff").unwrap();
        let mut state = ParseState::new(syntax);
        let o = ops(
            &mut state,
            "From 1234567890 Mon Sep 17 00:00:00 2001\n",
            &ss,
        );
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

    /// Regression guard for "branch_point fail-retry drops the
    /// trigger match's `captures:` scopes". The non-fail path emits
    /// capture Push/Pop ops inside the pat_scope brackets; the
    /// same-line fail re-emit must do the same — otherwise the
    /// inner capture scopes are truncated off `ops` together with
    /// alt[0]'s subsequent work and never replayed. Observed on
    /// Haskell's `data CtxCls ctx => ModId.QTyCls`, where the
    /// `(data)(?:\s+(family|instance))?` branch_point match's first
    /// capture `keyword.declaration.data.haskell` was dropped from
    /// the `data` token whenever `data-signature` failed into
    /// `data-context` — 22 assertion failures in
    /// `syntax_test_haskell.hs`.
    #[test]
    fn branch_point_capture_scopes_survive_fail_retry() {
        // The `(word)\s` branch_point match carries both `scope:`
        // and `captures:`. Alt[0] fails on the `!` lookahead,
        // forcing replay into alt[1]. `inner.capture` on the first
        // capture group must remain on the stack over `word`.
        expect_scope_stacks(
            "word !",
            &["<outer.match>, <inner.capture>"],
            r#"
                name: Branch Capture Re-emit Test
                scope: source.test
                contexts:
                  main:
                    - match: (word)\s
                      scope: outer.match
                      captures:
                        1: inner.capture
                      branch_point: bp
                      branch:
                        - alt-fails
                        - alt-succeeds
                  alt-fails:
                    - match: (?=!)
                      fail: bp
                  alt-succeeds:
                    - match: \S+
                      scope: ok.test
                      pop: 1
                "#,
        );
    }

    /// Regression guard for "branch_point fail-retry drops the new
    /// alternative's `meta_scope` from the trigger character". The
    /// non-fail push path emits the new context's `meta_scope` at
    /// `match_start` so the matched text sees it. The same-line
    /// fail re-emit must do the same — emit `meta_scope` (and any
    /// `clear_scopes`) at `trigger_match_start`, before the
    /// trigger's `pat.scope`. Placing them after the match meant
    /// `for (var i = 0; …)` parsed the `(` with
    /// `[meta.for.js, punctuation.section.group.begin.js]` instead
    /// of `[meta.for.js, meta.group.js, punctuation.section.group.begin.js]`,
    /// failing eight assertions in `syntax_test_js_control.js`.
    #[test]
    fn branch_point_fail_retry_applies_meta_scope_to_trigger() {
        // Mirrors the JS for-loop shape: `\(` triggers a branch with
        // `pop: 1`; alt 0 fails, alt 1 succeeds; alt 1 has a
        // `meta_scope` that must wrap the `(` itself.
        expect_scope_stacks(
            "(x",
            &["<meta.group.test>, <punctuation.test>"],
            r#"
                name: Branch Meta Scope Test
                scope: source.test
                contexts:
                  main:
                    - match: ''
                      push: trigger
                  trigger:
                    - match: \(
                      scope: punctuation.test
                      branch_point: g
                      branch:
                        - alt-fails
                        - alt-succeeds
                      pop: 1
                  alt-fails:
                    - meta_scope: meta.group.test
                    - match: (?=\S)
                      fail: g
                  alt-succeeds:
                    - meta_scope: meta.group.test
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
        let final_scopes: Vec<String> = stack
            .as_slice()
            .iter()
            .map(|s| format!("{:?}", s))
            .collect();
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

    /// Regression guard for the "cross-line branch_point exhaustion
    /// leaves contexts on the stack forever" bug: when ALL
    /// alternatives of a `branch_point` fail on a line *after* the
    /// branch was created, the parser must restore the pre-branch
    /// snapshot, truncate ops, and replay the buffered lines under
    /// the restored state. Pre-fix, the cross-line exhaustion path
    /// silently removed the branch record while leaving the last
    /// alternative's pushed contexts on the state stack — 274
    /// assertion failures in `syntax_test_typescript.ts` and
    /// another 10 in `syntax_test_C#9.cs` cascaded from that ghost
    /// state (`sublimehq/Packages#3598`'s incomplete
    /// `type x = { bar: (cb: ( };` was the minimal reproducer).
    ///
    /// Shape: a `branch_point` with two alternatives, each with a
    /// distinctive `meta_scope` and a `\w+` rule scoped by the
    /// alternative. Line 1 fires the branch; alt[0] consumes the
    /// newline and stays active. Line 2 fires `fail: bp` from
    /// alt[0] (cross-line retry into alt[1]), then the replay puts
    /// alt[1] on the stack, re-parses line 2, and alt[1] also fires
    /// `fail: bp` — cross-line exhaustion. After line 2:
    ///   - `is_speculative` must be false (branch record gone);
    ///   - a subsequent benign line must parse under the pre-branch
    ///     context (`main`), not under a leaked alternative. Pre-fix,
    ///     `beta` remained on the stack and the next line's `\w+`
    ///     scoped as `beta.word.cle` instead of `main.word.cle`.
    #[test]
    fn cross_line_branch_exhaustion_unwinds_state() {
        let syntax_str = r#"
name: CrossLineExhaustion
scope: source.cle
contexts:
  main:
    - match: 'TRY'
      scope: trigger.cle
      branch_point: bp
      branch: [alpha, beta]
    - match: '\w+'
      scope: main.word.cle
  alpha:
    - meta_scope: meta.alpha.cle
    - match: '\n'
    - match: 'FAIL'
      fail: bp
    - match: '\w+'
      scope: alpha.word.cle
  beta:
    - meta_scope: meta.beta.cle
    - match: '\n'
    - match: 'FAIL'
      fail: bp
    - match: '\w+'
      scope: beta.word.cle
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // Line 1: `TRY` fires branch `bp`; alt[0] `alpha` is pushed
        // and consumes the trailing newline, staying on the stack.
        let _out1 = state.parse_line("TRY\n", &ss).expect("parse line 1");

        // Line 2: alpha's `FAIL` rule fires `fail: bp` — cross-line
        // retry into `beta`. The beta replay leaves beta on the
        // stack; the re-parse of line 2 under beta hits beta's
        // `FAIL` rule, firing `fail: bp` again with no alternatives
        // left — cross-line exhaustion.
        let out2 = state.parse_line("FAIL\n", &ss).expect("parse line 2");

        // Exhaustion must clear every branch_point record.
        assert!(
            !state.is_speculative(),
            "cross-line exhaustion must drop all branch_point records"
        );

        // The exhaustion path replays buffered lines under the
        // restored pre-branch state, so `replayed` is non-empty.
        assert!(
            !out2.replayed.is_empty(),
            "cross-line exhaustion must emit replayed ops for the pre-branch state"
        );

        // Strong invariant: the subsequent line must be parsed under
        // `main` (the pre-branch context) — not under whichever
        // alternative was last active. Pre-fix, `beta` stayed on the
        // stack and `benign` would have scoped as `beta.word.cle`.
        let out3 = state.parse_line("benign\n", &ss).expect("parse line 3");
        let pushed: Vec<String> = out3
            .ops
            .iter()
            .filter_map(|(_, op)| match op {
                ScopeStackOp::Push(s) => Some(format!("{:?}", s)),
                _ => None,
            })
            .collect();

        assert!(
            pushed.iter().any(|s| s.contains("main.word.cle")),
            "post-exhaustion line must be scoped under main; got pushes: {:?}",
            pushed
        );
        for leaked in [
            "meta.alpha.cle",
            "meta.beta.cle",
            "alpha.word.cle",
            "beta.word.cle",
        ] {
            assert!(
                !pushed.iter().any(|s| s.contains(leaked)),
                "{} leaked into post-exhaustion line; got pushes: {:?}",
                leaked,
                pushed
            );
        }
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
        let final_scopes: Vec<String> = stack
            .as_slice()
            .iter()
            .map(|s| format!("{:?}", s))
            .collect();
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
        let after_assignment: Vec<String> = stack
            .as_slice()
            .iter()
            .map(|s| format!("{:?}", s))
            .collect();
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
        let final_scopes: Vec<String> = stack
            .as_slice()
            .iter()
            .map(|s| format!("{:?}", s))
            .collect();
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
        let has_fallback = out2.replayed[0].1.iter().any(|(_, op)| {
            matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("fallback.content"))
        });
        assert!(
            has_fallback,
            "expected fallback.content scope in replayed line 1 ops, got: {:?}",
            out2.replayed[0]
        );
        // The try.word scope must NOT appear in the replayed ops.
        let has_try_word = out2.replayed[0].1.iter().any(|(_, op)| {
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
    fn cross_line_fail_preserves_pre_branch_prefix_ops() {
        // Replay of the first buffered line on a cross-line fail must
        // preserve the pre-branch prefix ops (which were correctly emitted
        // under the pre-branch state) rather than re-parsing the whole
        // line under the new alternative.
        //
        // Reduced from multi-line SQL `LIKE '…' ESCAPE '…'`: the first
        // buffered line contains a prefix (`prefix `) before the branch
        // trigger (`TRY`). Under the fallback alternative's rules, `prefix`
        // would be scoped as fallback.content from column 0 — but the
        // test expects the original `prefix.word` scope to survive the
        // replay because those characters were parsed under the pre-branch
        // (main) context.
        let syntax_str = r#"
name: CrossLinePrefix
scope: source.clp
contexts:
  main:
    - match: 'prefix'
      scope: prefix.word.clp
    - match: 'TRY'
      branch_point: bp
      branch: [try-ctx, fallback-ctx]
    - match: '\s+'
  try-ctx:
    - match: 'END'
      pop: true
    - match: 'FAIL'
      fail: bp
    - match: '\w+'
      scope: try.word.clp
    - match: '\s+'
  fallback-ctx:
    - match: 'END'
      pop: true
    - match: '\w+'
      scope: fallback.content.clp
    - match: '\s+'
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // Line 1: "prefix TRY post\n" — prefix scoped by main, TRY triggers
        // branch, post is parsed under the chosen alternative.
        let _out1 = state
            .parse_line("prefix TRY post\n", &ss)
            .expect("parse line 1 failed");

        // Line 2: "FAIL\n" — cross-line fail triggers replay of line 1.
        let out2 = state
            .parse_line("FAIL\n", &ss)
            .expect("parse line 2 failed");
        assert_eq!(
            out2.replayed.len(),
            1,
            "expected one replayed line, got {:?}",
            out2.replayed
        );
        // The replayed ops for line 1 must still push prefix.word at col 0
        // (from prefix_ops, emitted pre-branch), not overwrite with
        // fallback.content.
        let replayed_has_prefix = out2.replayed[0].1.iter().any(|(_, op)| {
            matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("prefix.word"))
        });
        assert!(
            replayed_has_prefix,
            "replayed line must preserve prefix.word from pre-branch parse, got: {:?}",
            out2.replayed[0]
        );
        // fallback.content should appear for the post-TRY remainder.
        let replayed_has_fallback = out2.replayed[0].1.iter().any(|(_, op)| {
            matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("fallback.content"))
        });
        assert!(
            replayed_has_fallback,
            "replayed line must apply fallback.content for post-branch remainder, got: {:?}",
            out2.replayed[0]
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
        let has_fallback = out_fail.replayed[0].1.iter().any(|(_, op)| {
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

    /// Regression guard for the Haskell raw-string quasi-quote bug.
    ///
    /// Haskell's `brackets` context routes `[` through
    /// `branch_point: list-or-quasiquote` with alternatives
    /// `[list, quasi-quote]`. The `list` alternative matches `[` and
    /// `set: list-body`; `list-body` includes `list-fail` whose
    /// `\|\]` rule fires `fail: list-or-quasiquote` to fall back to
    /// the quasi-quote alternative. When a quasi-quote body contains
    /// a bracket expression (e.g. the regex raw string
    /// `[r|[a-zA-Z]+|]`), the nested `[` reopens `brackets`, creating
    /// a second `branch_point` with the same name. Its `list`
    /// alternative resolves cleanly when the nested `]` fires. Before
    /// the fix, the inner branch_point record stayed in the vec
    /// (the Pop retain predicate was `bp.stack_depth <= stack.len()`,
    /// non-strict), and a later `fail: list-or-quasiquote` from the
    /// outer list-body `rposition`'d onto the stale inner record,
    /// rewinding to the inner `[` instead of the outer one. The outer
    /// list's meta_scope stayed on the stack and the quasi-quote
    /// alternative never fired — cascading into ~90 col-weighted
    /// syntest failures across the raw-string QQ examples in
    /// `syntax_test_haskell.hs`.
    ///
    /// Shape mirrors Haskell: `brackets` is the branch point,
    /// alternatives are thin wrappers that `set:` onto their real
    /// bodies, so the bp's `stack_depth` lines up with the eventual
    /// content-body depth.
    #[test]
    fn nested_same_name_branch_point_outer_fail_replays_outer() {
        let syntax_str = r#"
name: NestedSameNameBranch
scope: source.nested-same-name-branch
contexts:
  main:
    - include: brackets

  brackets:
    - match: '(?=\[)'
      branch_point: bp
      branch: [list, quasi]

  list:
    - match: '\['
      scope: list.open
      set: list-body

  list-body:
    - meta_scope: list.body
    - match: '\|\]'
      fail: bp
    - match: '\]'
      scope: list.close
      pop: true
    - include: brackets
    - match: '\w+'
      scope: list.word

  quasi:
    - match: '\['
      scope: quasi.open
      set: quasi-body

  quasi-body:
    - meta_scope: quasi.body
    - match: '\|\]'
      scope: quasi.close
      pop: true
    - match: '.'
      scope: quasi.char
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);

        // `[x[y]|]` — outer `[` opens bp (list first). list sets
        // list-body. Inside, `x` is a word, then nested `[y]` opens a
        // second bp whose list alternative resolves via `]`. Outer
        // list-body then hits `|]` and fires `fail: bp`. Expected:
        // outer's quasi alternative takes over — everything inside
        // `[...|]` ends up as `quasi.body` / `quasi.char`, with no
        // `list.body` meta_scope leaking past the replay.
        let line_ops = ops(&mut state, "[x[y]|]\n", &ss);
        let states = stack_states(line_ops);
        assert!(
            states.iter().any(|s| s.contains("quasi.body")),
            "expected quasi.body after outer fail replay, got: {:?}",
            states
        );
        assert!(
            !states.iter().any(|s| s.contains("list.body")),
            "list.body meta_scope leaked past outer fail replay, got: {:?}",
            states
        );
    }

    /// A `fail` naming a branch point nobody opened is an ordinary rule: its
    /// text is consumed, its `scope` applies, and the line carries on.
    ///
    /// The reference: "If a `fail` action specifies a `branch_point` that was
    /// never pushed on the stack, or has already been popped off of it, it will
    /// have no effect." It is the *action* that has no effect.
    ///
    /// `handle_fail` answered an unresolvable name with `Ok(false)`, which is
    /// what `parse_next_token` returns to mean *stop parsing this line*, so
    /// everything after the match came out unscoped. CSS has such a rule —
    /// `- match: ';' fail: property-or-selector` — reachable outside any branch.
    #[test]
    fn fail_for_a_branch_point_nobody_opened_has_no_effect() {
        let syntax_str = r#"
name: StrayFail
scope: source.stray-fail
contexts:
  main:
    - match: 'a'
      scope: keyword.a
    - match: ';'
      scope: punctuation.semi
      fail: nobody
    - match: 'b'
      scope: keyword.b
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let states = stack_states(ops(&mut state, "a;b\n", &ss));
        assert!(
            states.iter().any(|s| s.contains("punctuation.semi")),
            "the rule's own scope was dropped: {:?}",
            states
        );
        assert!(
            states.iter().any(|s| s.contains("keyword.b")),
            "the rest of the line after the stray fail was never parsed: {:?}",
            states
        );
    }
    /// Version 1: a capture that begins before the last one that applied gets
    /// no scope. Version 2 applies all of them.
    ///
    /// Compatibility, "Regex Capture Group Order". The documentation states the
    /// result the wrong way round — its example is `(?:(x)|(y))+` over `yx` and
    /// it says the `x` loses its scope, where ST 4206 drops the `y` and keeps
    /// the `x` — so this is asked of the application, not read. The rule is a
    /// cursor over the groups in capture-index order, and the cursor is the
    /// last applied group's *start*: against its end, `((a)b)` would lose its
    /// inner capture, which is most captures in most definitions, and ST keeps
    /// it.
    #[test]
    fn version_1_capture_that_goes_backwards_gets_no_scope() {
        fn pushed(version: u32, body: &str, line: &str) -> Vec<String> {
            let syntax_str = format!(
                "name: Ver\nscope: source.ver\nversion: {}\ncontexts:\n{}",
                version, body
            );
            let syntax = SyntaxDefinition::load_from_str(&syntax_str, true, None).unwrap();
            let ss = link(syntax);
            let mut state = ParseState::new(&ss.syntaxes()[0]);
            let mut names = Vec::new();
            for (_, op) in ops(&mut state, line, &ss) {
                if let ScopeStackOp::Push(scope) = op {
                    names.push(format!("{:?}", scope));
                }
            }
            names
        }

        let alternating = "  main:\n    - match: '(?:(x)|(y))+'\n      captures:\n        1: identifier.x\n        2: identifier.y\n";
        let v1 = pushed(1, alternating, "yx\n");
        assert!(
            v1.iter().any(|s| s.contains("identifier.x")),
            "the group that moved forward lost its scope: {:?}",
            v1
        );
        assert!(
            !v1.iter().any(|s| s.contains("identifier.y")),
            "the group that begins behind the cursor was still scoped: {:?}",
            v1
        );
        let v2 = pushed(2, alternating, "yx\n");
        assert!(
            v2.iter().any(|s| s.contains("identifier.x")) && v2.iter().any(|s| s.contains("identifier.y")),
            "version 2 applies both: {:?}",
            v2
        );

        // The cursor is carried from group to group: `c` at column 0 is dropped
        // after groups 1 and 2 have applied at columns 1 and 2.
        let three = "  main:\n    - match: '(?:(a)|(b)|(c))+'\n      captures:\n        1: identifier.a\n        2: identifier.b\n        3: identifier.c\n";
        let v1 = pushed(1, three, "cab\n");
        assert!(
            v1.iter().any(|s| s.contains("identifier.a")) && v1.iter().any(|s| s.contains("identifier.b")),
            "{:?}",
            v1
        );
        assert!(
            !v1.iter().any(|s| s.contains("identifier.c")),
            "the cursor was not carried past the group before: {:?}",
            v1
        );

        // And a nested capture is not a step backwards, at either version.
        let nested = "  main:\n    - match: '((a)b)'\n      captures:\n        1: outer.one\n        2: inner.two\n";
        let v1 = pushed(1, nested, "ab\n");
        assert!(
            v1.iter().any(|s| s.contains("inner.two")),
            "nesting is not what the gate is about: {:?}",
            v1
        );
        assert_eq!(v1, pushed(2, nested, "ab\n"));
    }
    /// A version 1 `push:` of several contexts clears their `clear_scopes`
    /// added up, for the trigger token and for nothing after it.
    ///
    /// Compatibility, "Multiple Target Push Actions with `clear_scopes`", with
    /// the documentation's own example. Both phases are asserted because they
    /// disagree, and ST 4206 says so: the trigger sees the sum applied before
    /// either `meta_scope`, and the column after it sees version 2's
    /// interleaving. Getting only the first half right leaves every line after
    /// the push inside a scope the file had cleared.
    #[test]
    fn version_1_push_clears_the_sum_for_its_trigger_alone() {
        fn columns(version: u32, line: &str) -> Vec<String> {
            let syntax_str = format!(
                r#"
name: Ver
scope: source.lang
version: {}
contexts:
  main:
    - meta_content_scope: meta.main
    - match: '[a-zA-Z0-9]+\b'
      scope: identifier
      push:
        - context2
        - context3
  context2:
    - meta_scope: meta.ctx2
    - clear_scopes: 1
  context3:
    - meta_scope: meta.ctx3
    - clear_scopes: 1
    - match: \n
      pop: true
"#,
                version
            );
            let syntax = SyntaxDefinition::load_from_str(&syntax_str, true, None).unwrap();
            let ss = link(syntax);
            let mut state = ParseState::new(&ss.syntaxes()[0]);
            let line_ops = ops(&mut state, line, &ss);
            let mut out = Vec::new();
            let mut stack = ScopeStack::new();
            let mut iter = line_ops.into_iter().peekable();
            let text = line.trim_end_matches('\n');
            for column in 0..=text.len() {
                while iter.peek().is_some_and(|(at, _)| *at <= column) {
                    let (_, op) = iter.next().unwrap();
                    stack.apply(&op).unwrap();
                }
                out.push(
                    stack
                        .as_slice()
                        .iter()
                        .map(|s| s.build_string())
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
            out
        }

        let one = columns(1, "abc 1\n");
        assert_eq!(
            one[0], "meta.ctx2 meta.ctx3 identifier",
            "the two clears were not added up and applied before either meta scope"
        );
        assert_eq!(
            one[3], "source.lang meta.ctx3",
            "the summed clear outlived the token it belongs to"
        );

        let two = columns(2, "abc 1\n");
        assert_eq!(two[0], "source.lang meta.ctx3 identifier");
        assert_eq!(two[3], "source.lang meta.ctx3");
    }
    /// A version 1 `embed:`'s escape text is outside the context that holds
    /// it, and a version 2 one is inside it.
    ///
    /// Compatibility, "Embed Escape Match and Meta Scopes", with the
    /// documentation's own example. ST 4206 agrees at both versions and adds
    /// the part the sentence does not say: it is *only* the escape's token —
    /// the column after it is back inside both scopes, which is what makes this
    /// a pop and a re-push around the token rather than the context ending
    /// early.
    #[test]
    fn version_1_embed_escape_is_outside_the_context_that_holds_it() {
        fn columns(version: u32, line: &str) -> Vec<String> {
            let syntax_str = format!(
                r#"
name: Ver
scope: source.lang
version: {}
contexts:
  main:
    - match: (?=')
      push: context1
  context1:
    - meta_scope: meta.group
    - meta_content_scope: meta.content
    - match: \'
      scope: punctuation.begin
      embed: embed
      escape: \'
      escape_captures:
        0: punctuation.end
  embed:
    - match: '[a-z]+'
      scope: word
"#,
                version
            );
            let syntax = SyntaxDefinition::load_from_str(&syntax_str, true, None).unwrap();
            let ss = link(syntax);
            let mut state = ParseState::new(&ss.syntaxes()[0]);
            let line_ops = ops(&mut state, line, &ss);
            let mut out = Vec::new();
            let mut stack = ScopeStack::new();
            let mut iter = line_ops.into_iter().peekable();
            let text = line.trim_end_matches('\n');
            for column in 0..=text.len() {
                while iter.peek().is_some_and(|(at, _)| *at <= column) {
                    let (_, op) = iter.next().unwrap();
                    stack.apply(&op).unwrap();
                }
                out.push(
                    stack
                        .as_slice()
                        .iter()
                        .map(|s| s.build_string())
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
            out
        }

        let one = columns(1, "'abc'\n");
        assert_eq!(
            one[4], "source.lang punctuation.end",
            "the escape's text kept the scopes of the context holding the embed"
        );
        assert_eq!(
            one[5], "source.lang meta.group meta.content",
            "the context the embed fired from did not end at its escape"
        );

        let two = columns(2, "'abc'\n");
        assert_eq!(two[4], "source.lang meta.group meta.content punctuation.end");
        assert_eq!(one[0], two[0], "and the opening quote is the same at both");
    }
    /// A `pop: N + embed:`'s escape still wears the meta scopes of the context
    /// the `embed:` was declared in.
    ///
    /// The reference's own example under "Using pop with another action" is a
    /// `paragraph` context with a `meta_scope` whose ```` ```py ```` fence
    /// fires `pop: 1` and embeds Python; nothing in it says what the closing
    /// fence gets, so ST 4206 was asked. Its columns for `p<a>t` are what make
    /// this a retarget rather than a deferral — the frame is off the stack for
    /// everything except its own escape:
    ///
    /// | column | ST |
    /// |---|---|
    /// | `p`, the push trigger | `m.para` |
    /// | `<`, firing `pop: 1 + embed:` | no `m.para` — the pop is a lookahead |
    /// | `a`, the embedded text | `m.embedded`, no `m.para` |
    /// | `>`, the escape | **`m.para`** |
    /// | `t`, after it | none |
    #[test]
    fn pop_n_embed_escape_wears_the_context_it_left() {
        let syntax_str = r#"
name: PopEmbed
scope: source.pop-embed
version: 2
contexts:
  main:
    - match: 'p'
      push: para
  para:
    - meta_scope: m.para
    - match: '<'
      scope: k.open
      pop: 1
      embed: inner
      embed_scope: m.embedded
      escape: '>'
  inner:
    - match: 'a'
      scope: k.a
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let line_ops = ops(&mut state, "p<a>t\n", &ss);

        let mut columns = Vec::new();
        let mut stack = ScopeStack::new();
        let mut iter = line_ops.into_iter().peekable();
        for column in 0..5 {
            while iter.peek().is_some_and(|(at, _)| *at <= column) {
                let (_, op) = iter.next().unwrap();
                stack.apply(&op).unwrap();
            }
            columns.push(
                stack
                    .as_slice()
                    .iter()
                    .map(|s| s.build_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }

        // Column 1, the trigger, is left out on purpose: ST has no `m.para` on
        // it, because the pop of a `pop: N + embed:` is a lookahead and unwinds
        // before the match takes its scopes. That is a different rule, and a
        // different change — see #687, which is about exactly that phase.
        assert_eq!(
            columns[0], "source.pop-embed m.para",
            "the push trigger is inside the context it pushes"
        );
        assert_eq!(
            columns[2], "source.pop-embed m.embedded k.a",
            "the pop really did happen at the trigger"
        );
        assert_eq!(
            columns[3], "source.pop-embed m.para",
            "the escape did not wear the frame the pop took off"
        );
        assert_eq!(
            columns[4], "source.pop-embed",
            "and it kept it past its own token"
        );
    }
    /// A cross-line `fail` says *which* line each correction is for.
    ///
    /// `d` opens a branch, `q` is buffered under the first alternative, and `z`
    /// on the third line fails it — which must correct the first two lines, and
    /// name them.
    #[test]
    fn a_cross_line_fail_names_the_lines_it_corrects() {
        let syntax_str = r#"
name: Late
scope: source.late
contexts:
  main:
    - match: 'd'
      scope: keyword.d
      branch_point: bp
      branch:
        - alt-a
        - alt-b
  alt-a:
    - meta_scope: meta.a
    - match: '(?=z)'
      fail: bp
    - match: '\S'
      scope: other.a
  alt-b:
    - meta_scope: meta.b
    - match: '\S'
      scope: other.b
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let mut corrected: Vec<(usize, Vec<usize>)> = Vec::new();
        for (n, line) in "d\nq\nz\n".split_inclusive('\n').enumerate() {
            let out = state.parse_line(line, &ss).expect("parses");
            if !out.replayed.is_empty() {
                corrected.push((n, out.replayed.iter().map(|(at, _)| *at).collect()));
            }
        }
        assert_eq!(
            corrected,
            vec![(2, vec![1, 2])],
            "the fail on the third line should correct the first two, and name them"
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
        let has_fallback = out4.replayed[0].1.iter().any(|(_, op)| {
            matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("fallback.content"))
        });
        assert!(
            has_fallback,
            "replayed line 0 missing fallback.content, got: {:?}",
            out4.replayed[0]
        );

        // No replayed line should have try.word (all are under fallback path)
        for (i, (_, line_ops)) in out4.replayed.iter().enumerate() {
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
        let replay_has_fallback = out2.replayed[0].1.iter().any(|(_, op)| {
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
    fn v2_set_from_context_with_clear_scopes_restores_cleared_atoms() {
        // When a `set:` fires from a context that had `clear_scopes` of its
        // own (e.g. JSON's `object-value-body`), the cleared scopes must be
        // restored at the correct position on the scope stack: below the
        // target's pushed meta_scope, not on top of it.
        //
        // Previously the Restore fired in the initial phase, before the
        // non-initial Pop of (cur.meta_scope + target.meta_scope). The Pop
        // then removed the restored atoms instead of the intended meta_scopes,
        // dropping cur's cleared state on the floor. This surfaced in the
        // JSON test as duplicate `meta.mapping.value.json` atoms in nested
        // objects — e.g. `[source.json, meta.mapping.value.json,
        // meta.mapping.value.json]` instead of `[source.json,
        // meta.mapping.value.json, meta.mapping.json]`.
        //
        // Reduced JSON-like repro: outer mapping pushes an inner value-body
        // that clears the outer mapping scope, then the inner value-body
        // `set`s to a follow-up context. The follow-up context's matched
        // text must see the outer mapping scope restored below it.
        let syntax_str = r#"
name: V2SetRestore
scope: source.v2setrestore
version: 2
contexts:
  main:
    - meta_scope: meta.outer.v2setrestore
    - match: '\{'
      push: value-body

  value-body:
    - clear_scopes: 1
    - meta_scope: meta.value.v2setrestore
    - match: 'x'
      scope: keyword.x.v2setrestore
      set: follow-up

  follow-up:
    - match: '\w+'
      scope: word.follow.v2setrestore
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let raw_ops = ops(&mut state, "{xhello\n", &ss);

        let states = stack_states(raw_ops);
        // Find the state while parsing "hello" in follow-up.
        let follow_states: Vec<_> = states
            .iter()
            .filter(|s| s.contains("word.follow.v2setrestore"))
            .collect();
        assert!(
            !follow_states.is_empty(),
            "expected to enter follow-up context, got states: {:?}",
            states
        );
        // The outer meta.outer scope must be restored below follow-up's word
        // scope. If the Restore landed above the target's meta_scope push (or
        // was dropped by the non-initial Pop), meta.outer would be missing.
        assert!(
            follow_states
                .iter()
                .any(|s| s.contains("meta.outer.v2setrestore")),
            "meta.outer must be restored after leaving value-body (which cleared it): {:?}",
            follow_states
        );
        // meta.value (from the cleared context) must NOT persist.
        assert!(
            !follow_states
                .iter()
                .any(|s| s.contains("meta.value.v2setrestore")),
            "meta.value (from the exited context) must not leak into follow-up: {:?}",
            follow_states
        );
    }

    #[test]
    fn v2_set_to_target_with_clear_scopes_clears_parent_meta_content_scope() {
        // Reduced from Lisp `function-parameter-list` → `function-parameter-list-body`:
        // the enclosing `function-body` supplies `meta_content_scope:
        // meta.function.lisp`; the inner parameter-list-body declares
        // `clear_scopes: 1` so the `(` and the parameter identifiers inside
        // are not double-scoped with the outer `meta.function`.
        //
        // The `(` token itself (the `set:` trigger) should see the cleared
        // stack — i.e. `meta.function.lisp` is already gone at that column.
        // Previously the v2 initial phase for Set pushed target.meta_scope
        // above the outer mcs without clearing first, so the trigger token
        // reported `[..., meta.function.lisp, meta.function.parameters.lisp,
        // punctuation...]` instead of `[..., meta.function.parameters.lisp,
        // punctuation...]`.
        let syntax_str = r#"
name: V2SetTargetClear
scope: source.v2settargetclear
version: 2
contexts:
  main:
    - match: '\('
      scope: punctuation.section.parens.begin.v2settargetclear
      push: [body, params-open]

  body:
    - meta_content_scope: meta.function.v2settargetclear
    - match: '\)'
      scope: punctuation.section.parens.end.v2settargetclear
      pop: 1

  params-open:
    - match: '\('
      scope: punctuation.section.parameters.begin.v2settargetclear
      set: params-body
    - include: else-pop

  params-body:
    - clear_scopes: 1
    - meta_scope: meta.function.parameters.v2settargetclear
    - match: '\)'
      scope: punctuation.section.parameters.end.v2settargetclear
      pop: 1
    - match: '\w+'
      scope: variable.parameter.v2settargetclear
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        // Mirrors `(defun averagenum (n1 n2))`: outer `(...)` carries
        // meta.function; inner `(...)` is parameter list.
        let raw_ops = ops(&mut state, "( (n1 n2))\n", &ss);

        let states = stack_states(raw_ops);

        // Find the state covering the inner `(` at column 2 (the set trigger).
        // Every state recorded once the parameters context has been entered
        // must NOT still carry the outer meta.function atom.
        let param_states: Vec<_> = states
            .iter()
            .filter(|s| s.contains("meta.function.parameters.v2settargetclear"))
            .collect();
        assert!(
            !param_states.is_empty(),
            "expected to enter params-body context, got states: {:?}",
            states
        );
        // The outer `meta.function` atom (from `body`'s meta_content_scope)
        // must be absent on every state where params-body is active. Match
        // the exact atom name — not a prefix — so that
        // `meta.function.parameters.v2settargetclear` doesn't trigger.
        let outer = "<meta.function.v2settargetclear>";
        for s in &param_states {
            assert!(
                !s.contains(outer),
                "outer meta.function must be cleared under params-body, \
                 but found it alongside meta.function.parameters: {:?}",
                s
            );
        }

        // After the inner `)` pops params-body the clear must Restore, so
        // the outer `meta.function` atom reappears before the outer `)`.
        let after_inner_close: Vec<_> = states
            .iter()
            .rev()
            .take_while(|s| !s.contains("meta.function.parameters.v2settargetclear"))
            .collect();
        assert!(
            after_inner_close
                .iter()
                .any(|s| s.contains("meta.function.v2settargetclear")),
            "meta.function must be restored after params-body pops, got trailing states: {:?}",
            after_inner_close
        );
    }

    #[test]
    fn v2_set_clear_scopes_applies_from_every_context() {
        // v2: when `set:` lists multiple contexts, `clear_scopes` on any of
        // them — not just the topmost — applies at that context's own
        // position in the stack. The canonical real-world case is Bash's
        //   set: [def-function-body, def-function-params, def-function-name]
        // where `def-function-params` (the middle context) carries
        // `clear_scopes: 1`. The Clear strips the atom that the preceding
        // context's meta_content_scope just pushed, matching Sublime
        // Text's observed behaviour. Previously this test guessed
        // Sublime pinned Clear to the topmost context only; running real
        // v2 syntaxes (Bash function definitions, among others) refuted
        // that guess.
        let syntax_str = r#"
name: V2ClearMid
scope: source.v2clear
version: 2
contexts:
  main:
    - meta_scope: meta.main.v2clear
    - match: 'GO'
      set: [ctx-bottom, ctx-middle, ctx-top]
  ctx-bottom:
    - meta_content_scope: mcs.bottom.v2clear
  ctx-middle:
    - clear_scopes: 1
    - meta_content_scope: mcs.middle.v2clear
  ctx-top:
    - meta_content_scope: mcs.top.v2clear
    - match: '\w+'
      scope: word.top.v2clear
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let ss = link(syntax);
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let raw_ops = ops(&mut state, "GO hello\n", &ss);
        let states = stack_states(raw_ops);

        // "hello" matches in ctx-top. At that point ctx-middle's
        // clear_scopes: 1 must have stripped ctx-bottom's
        // meta_content_scope atom.
        let hello_states: Vec<_> = states
            .iter()
            .filter(|s| s.contains("word.top.v2clear"))
            .collect();
        assert!(
            !hello_states.is_empty(),
            "expected word.top.v2clear, got states: {:?}",
            states
        );
        for s in &hello_states {
            assert!(
                !s.contains("mcs.bottom.v2clear"),
                "ctx-bottom's mcs should have been cleared by ctx-middle's \
                 clear_scopes: 1 before ctx-top's match, got: {:?}",
                s
            );
            assert!(
                s.contains("mcs.middle.v2clear"),
                "ctx-middle's mcs should be on the stack during ctx-top's match, \
                 got: {:?}",
                s
            );
        }

        // Reaching this point without a panic proves the Restore emitted on
        // ctx-middle's pop didn't underflow the clear_stack — the
        // regression the Bash `func () {}` minimal reproducer uncovered.
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
    fn v2_host_embedding_v1_guest_skips_meta_content_pop_on_escape() {
        // Regression for the Rails html.erb syntest cluster: when a v2 host
        // uses `embed:` + `embed_scope:` to pull in a v1 guest grammar (e.g.
        // Rails/HTML embedding Ruby), `embed_scope_replaces` is set on the
        // wrapper context. On escape, the embedded guest's meta_content_scope
        // must be skipped — it was never pushed on the way in.
        //
        // The exec_escape skip logic was gated on
        // `current_syntax_version() >= 2`, which reads the version from the
        // top-of-stack context. That is the *guest* (Ruby, v1), not the host,
        // so the gate evaluated false and a spurious Pop fired for a scope
        // that was never pushed, misaligning every scope on the stack for
        // the remainder of the host context.
        use crate::parsing::ScopeStack;

        let host = SyntaxDefinition::load_from_str(
            r#"
name: V2HostV1Guest
scope: source.v2host
file_extensions: [v2host]
version: 2
contexts:
  main:
    - match: '<<'
      embed: scope:source.v1guest
      embed_scope: meta.embedded.v2host
      escape: '>>'
      escape_captures:
        0: punctuation.end.v2host
    - match: '\w+'
      scope: word.v2host
"#,
            true,
            None,
        )
        .unwrap();

        // Guest omits `version:` — defaults to 1. Its `scope:` lands in the
        // main context's meta_content_scope (source.v1guest), which the
        // v2 embed_scope_replaces suppresses on push. The escape must
        // symmetrically suppress it on pop.
        let guest = SyntaxDefinition::load_from_str(
            r#"
name: V1Guest
scope: source.v1guest
file_extensions: [v1guest]
contexts:
  main:
    - match: '\w+'
      scope: keyword.v1guest
"#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(host);
        builder.add(guest);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("V2HostV1Guest").unwrap();
        let mut state = ParseState::new(syntax);
        let raw_ops = state.parse_line("<<x>> hello\n", &ss).unwrap().ops;

        // Before the fix, the escape emits a Pop for guest main's mcs even
        // though it was never pushed. Subsequent Pops then strip scopes
        // that should have survived. Applying the op stream must not fail,
        // and source.v2host must remain on the stack at the end.
        let mut scope_stack = ScopeStack::new();
        for (_, op) in &raw_ops {
            scope_stack.apply(op).expect(
                "applying op stream must succeed — a spurious Pop indicates the skip was gated \
                 on the guest's syntax version instead of the embed_scope_replaces flag",
            );
        }
        let final_scopes: Vec<String> = scope_stack
            .as_slice()
            .iter()
            .map(|s| format!("{:?}", s))
            .collect();
        assert!(
            final_scopes.iter().any(|s| s.contains("source.v2host")),
            "source.v2host should remain after escape; got: {:?}",
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
