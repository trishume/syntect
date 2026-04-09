//! Core highlighting API: scope-based rendering, theme-aware highlighting,
//! and convenience wrappers for common use cases.
//!
//! The central type is [`HighlightLines`], which drives syntax parsing and
//! delegates rendering to a pluggable [`ScopeRenderer`]. It handles
//! branch-point backtracking transparently by buffering output during
//! speculative parsing.
//!
//! For theme-based highlighting (resolving scopes to colors),
//! [`ThemedANSIScopeRenderer`] resolves scopes to styles via the
//! [`Highlighter`](crate::highlighting::Highlighter) and emits ANSI 24-bit
//! color escape codes (the default renderer for [`HighlightLines`]).
//!
//! For HTML output, see [`crate::html::ClassedHTMLGenerator`],
//! [`crate::html::ClassedHTMLScopeRenderer`], and
//! [`crate::html::InlineHTMLScopeRenderer`].

use crate::highlighting::{Highlighter, Style, Theme};
use crate::parsing::{
    lock_global_scope_repo, BasicScopeStackOp, ParseState, Scope, ScopeRepository, ScopeStack,
    ScopeStackOp, SyntaxReference, SyntaxSet,
};
use crate::util::blend_fg_color;
use crate::Error;
use std::fmt::Write;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;

// ---------------------------------------------------------------------------
// ScopeRenderer trait and render_line (moved from renderer.rs)
// ---------------------------------------------------------------------------

/// Trait for customizing the output of scope-based syntax highlighting.
///
/// The methods receive pre-resolved scope atom strings so implementations
/// never need to interact with the scope repository directly.
///
/// See [`crate::html::ClassedHTMLScopeRenderer`] for an HTML implementation that produces
/// `<span class="...">` elements.
pub trait ScopeRenderer {
    /// Called at the start of each line, before any tokens.
    ///
    /// `line_index` is 0-based. `scope_stack` is the current scope stack carried
    /// over from the previous line.
    fn begin_line(&mut self, _line_index: usize, _scope_stack: &[Scope], _output: &mut String) {}

    /// Called at the end of each line, after all tokens.
    fn end_line(&mut self, _line_index: usize, _scope_stack: &[Scope], _output: &mut String) {}

    /// Called when a new scope is pushed onto the stack.
    ///
    /// - `atom_strs`: the individual atom strings of the scope
    ///   (e.g., `["keyword", "operator", "arithmetic", "r"]` for `keyword.operator.arithmetic.r`)
    /// - `scope`: the raw [`Scope`] value, for advanced matching
    /// - `scope_stack`: the full stack after the push
    /// - `output`: the buffer to write to
    ///
    /// Return `true` if output was written (meaning [`end_scope`] will be called
    /// to close it), or `false` to skip this scope (no matching `end_scope` call).
    ///
    /// [`end_scope`]: ScopeRenderer::end_scope
    fn begin_scope(
        &mut self,
        atom_strs: &[&str],
        scope: Scope,
        scope_stack: &[Scope],
        output: &mut String,
    ) -> bool;

    /// Called when a scope is popped, only if the corresponding [`begin_scope`]
    /// returned `true`.
    ///
    /// [`begin_scope`]: ScopeRenderer::begin_scope
    fn end_scope(&mut self, output: &mut String);

    /// Called for text content between scope operations.
    ///
    /// The default implementation passes text through unchanged.
    fn write_text(&mut self, text: &str, output: &mut String) {
        output.push_str(text);
    }
}

/// Resolve atom strings for a scope from a locked repository.
pub(crate) fn resolve_atom_strs<'a>(scope: Scope, repo: &'a ScopeRepository) -> Vec<&'a str> {
    (0..scope.len() as usize)
        .map(|i| repo.atom_str(scope.atom_at(i)))
        .collect()
}

/// Core rendering loop that drives a [`ScopeRenderer`].
///
/// Locks the global scope repository once per line. Applies the transparent
/// empty-scope optimization: if a scope push/pop pair contains no text, the
/// push output is truncated rather than emitting an empty element.
pub fn render_line<R: ScopeRenderer>(
    line: &str,
    ops: &[(usize, ScopeStackOp)],
    stack: &mut ScopeStack,
    renderer: &mut R,
    line_index: usize,
) -> Result<(String, isize), Error> {
    let mut s = String::with_capacity(line.len() + ops.len() * 8);
    let mut cur_index = 0;
    let mut scope_delta = 0;

    // Empty-scope optimization tracking
    let mut scope_empty = false;
    let mut scope_start = 0;

    // begin_line is called without the repo lock held, so renderers can
    // safely lock the repo themselves if needed.
    renderer.begin_line(line_index, &stack.scopes, &mut s);

    {
        let repo = lock_global_scope_repo();
        for &(i, ref op) in ops {
            if i > cur_index {
                scope_empty = false;
                renderer.write_text(&line[cur_index..i], &mut s);
                cur_index = i;
            }
            stack.apply_with_hook(op, |basic_op, stack_slice| match basic_op {
                BasicScopeStackOp::Push(scope) => {
                    let atom_strs = resolve_atom_strs(scope, &repo);
                    scope_start = s.len();
                    scope_empty = true;
                    let wrote = renderer.begin_scope(&atom_strs, scope, stack_slice, &mut s);
                    if wrote {
                        scope_delta += 1;
                    } else {
                        scope_empty = false;
                    }
                }
                BasicScopeStackOp::Pop => {
                    if scope_empty {
                        s.truncate(scope_start);
                    } else {
                        renderer.end_scope(&mut s);
                    }
                    scope_delta -= 1;
                    scope_empty = false;
                }
            })?;
        }
        renderer.write_text(&line[cur_index..line.len()], &mut s);
    }

    // end_line is called without the repo lock held.
    renderer.end_line(line_index, &stack.scopes, &mut s);

    Ok((s, scope_delta))
}

// ---------------------------------------------------------------------------
// HighlightLines — the main highlighting driver
// ---------------------------------------------------------------------------

/// Drives syntax parsing and delegates rendering to a [`ScopeRenderer`].
///
/// This struct parses lines of code and emits rendering events (scope push/pop,
/// text content, line boundaries) to a pluggable renderer. The output format
/// is determined entirely by the `R` parameter, which defaults to
/// [`ThemedANSIScopeRenderer`] for theme-based ANSI terminal output.
///
/// When the parser is in speculative mode (inside a branch point),
/// `HighlightLines` buffers output internally and flushes it only once the
/// speculation resolves, replaying corrected operations if a cross-line
/// `fail` occurred.
///
/// There is a [`finalize()`] method that must be called in the end in order
/// to close any open scopes.
///
/// [`finalize()`]: #method.finalize
///
/// # Examples
///
/// Theme-based highlighting (default renderer):
///
/// ```
/// use syntect::easy::HighlightLines;
/// use syntect::highlighting::ThemeSet;
/// use syntect::parsing::SyntaxSet;
/// use syntect::util::LinesWithEndings;
///
/// let ss = SyntaxSet::load_defaults_newlines();
/// let ts = ThemeSet::load_defaults();
/// let syntax = ss.find_syntax_by_extension("rs").unwrap();
///
/// let mut h = HighlightLines::new(syntax, &ss, &ts.themes["base16-ocean.dark"]);
/// for line in LinesWithEndings::from("fn main() {}\n") {
///     h.highlight_line(line).unwrap();
/// }
/// let output = String::from_utf8(h.finalize()).unwrap();
/// assert!(output.contains("\x1b[38;2;"));
/// ```
///
/// Custom renderer (CSS-classed HTML):
///
/// ```
/// use syntect::easy::HighlightLines;
/// use syntect::html::{ClassedHTMLScopeRenderer, ClassStyle};
/// use syntect::parsing::SyntaxSet;
/// use syntect::util::LinesWithEndings;
///
/// let ss = SyntaxSet::load_defaults_newlines();
/// let syntax = ss.find_syntax_by_name("R").unwrap();
/// let renderer = ClassedHTMLScopeRenderer::new(ClassStyle::Spaced);
/// let mut h = HighlightLines::new_with_renderer(syntax, &ss, renderer);
/// for line in LinesWithEndings::from("x <- 5\n") {
///     h.highlight_line(line).unwrap();
/// }
/// let html = String::from_utf8(h.finalize()).unwrap();
/// ```
pub struct HighlightLines<
    'a,
    R: ScopeRenderer = ThemedANSIScopeRenderer<'a>,
    W: io::Write = Vec<u8>,
> {
    syntax_set: &'a SyntaxSet,
    open_scopes: isize,
    parse_state: ParseState,
    scope_stack: ScopeStack,
    output: W,
    renderer: R,
    line_index: usize,
    // Branch-point buffering
    pending_lines: Vec<String>,
    pending_ops: Vec<Vec<(usize, ScopeStackOp)>>,
    scope_stack_snapshot: Option<ScopeStack>,
    open_scopes_snapshot: Option<isize>,
}

impl<'a> HighlightLines<'a> {
    /// Create a new highlighting driver with default ANSI terminal output.
    ///
    /// Uses [`ThemedANSIScopeRenderer`] to produce 24-bit color ANSI escape
    /// codes. The output is collected into a `Vec<u8>` returned by [`finalize`].
    ///
    /// [`finalize`]: HighlightLines::finalize
    pub fn new(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        theme: &'a Theme,
    ) -> HighlightLines<'a> {
        let renderer = ThemedANSIScopeRenderer::new(theme, false);
        Self::new_with_renderer_and_output(syntax_reference, syntax_set, renderer, Vec::new())
    }
}

impl<'a, R: ScopeRenderer> HighlightLines<'a, R> {
    /// Create a new highlighting driver with a custom [`ScopeRenderer`].
    ///
    /// The output is collected into a `Vec<u8>` returned by [`finalize`].
    /// Use [`new_with_renderer_and_output`] to stream to an arbitrary
    /// [`io::Write`] sink instead.
    ///
    /// [`finalize`]: HighlightLines::finalize
    /// [`new_with_renderer_and_output`]: HighlightLines::new_with_renderer_and_output
    pub fn new_with_renderer(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        renderer: R,
    ) -> HighlightLines<'a, R> {
        Self::new_with_renderer_and_output(syntax_reference, syntax_set, renderer, Vec::new())
    }
}

impl<'a, R: ScopeRenderer, W: io::Write> HighlightLines<'a, R, W> {
    /// Create a new highlighting driver that writes to the given output sink.
    ///
    /// This allows streaming rendered output directly to a file, socket,
    /// or buffered writer without intermediate allocation.
    pub fn new_with_renderer_and_output(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        renderer: R,
        output: W,
    ) -> HighlightLines<'a, R, W> {
        HighlightLines {
            syntax_set,
            open_scopes: 0,
            parse_state: ParseState::new(syntax_reference),
            scope_stack: ScopeStack::new(),
            output,
            renderer,
            line_index: 0,
            pending_lines: Vec::new(),
            pending_ops: Vec::new(),
            scope_stack_snapshot: None,
            open_scopes_snapshot: None,
        }
    }

    /// Resume highlighting from a previously saved state.
    ///
    /// This is useful for incremental highlighting where you cache the
    /// parse and scope state at checkpoints.
    pub fn from_state(
        parse_state: ParseState,
        scope_stack: ScopeStack,
        syntax_set: &'a SyntaxSet,
        mut renderer: R,
        output: W,
    ) -> HighlightLines<'a, R, W> {
        // Replay the existing scope stack to the renderer so that its
        // internal state (e.g. style stack) matches the restored scopes.
        {
            let repo = lock_global_scope_repo();
            let scopes = &scope_stack.scopes;
            for (i, &scope) in scopes.iter().enumerate() {
                let atom_strs = resolve_atom_strs(scope, &repo);
                let stack_slice = &scopes[..=i];
                let mut dummy = String::new();
                renderer.begin_scope(&atom_strs, scope, stack_slice, &mut dummy);
            }
        }
        let open_scopes = scope_stack.scopes.len() as isize;
        HighlightLines {
            syntax_set,
            open_scopes,
            parse_state,
            scope_stack,
            output,
            renderer,
            line_index: 0,
            pending_lines: Vec::new(),
            pending_ops: Vec::new(),
            scope_stack_snapshot: None,
            open_scopes_snapshot: None,
        }
    }

    /// Returns a reference to the renderer.
    pub fn renderer(&self) -> &R {
        &self.renderer
    }

    /// Returns a mutable reference to the renderer.
    pub fn renderer_mut(&mut self) -> &mut R {
        &mut self.renderer
    }

    /// Returns references to the current parse and scope state.
    pub fn state(&self) -> (&ParseState, &ScopeStack) {
        (&self.parse_state, &self.scope_stack)
    }

    /// Consume the driver and return its parts.
    ///
    /// Any pending buffered output is flushed before returning.
    pub fn into_parts(mut self) -> (ParseState, ScopeStack, R, W) {
        let _ = self.flush_pending();
        (
            self.parse_state,
            self.scope_stack,
            self.renderer,
            self.output,
        )
    }

    /// Append a character to the output.
    ///
    /// This is primarily for backward-compatibility helpers that need to
    /// inject extra characters (e.g., a trailing newline) into the output.
    pub(crate) fn push_output(&mut self, ch: char) {
        let mut buf = [0u8; 4];
        let bytes = ch.encode_utf8(&mut buf);
        let _ = self.output.write_all(bytes.as_bytes());
    }

    /// Parse a line and render it using the configured renderer.
    ///
    /// When the parser is in speculative mode (inside a branch point),
    /// the rendered output is buffered internally. Once the speculation
    /// resolves, all buffered lines are flushed — replaying any corrected
    /// operations from a cross-line `fail`.
    ///
    /// *Note:* This function requires `line` to include a newline at the end and
    /// also use of the `load_defaults_newlines` version of the syntaxes.
    pub fn highlight_line(&mut self, line: &str) -> Result<(), Error> {
        let parse_output = self.parse_state.parse_line(line, self.syntax_set)?;

        // If replayed ops arrived, patch the pending buffer.
        if !parse_output.replayed.is_empty() {
            for (i, ops) in parse_output.replayed.into_iter().enumerate() {
                if i < self.pending_ops.len() {
                    self.pending_ops[i] = ops;
                }
            }
        }

        // Fast path: not speculative, nothing buffered — render + write directly.
        if !self.parse_state.is_speculative() && self.pending_lines.is_empty() {
            let (formatted, delta) = render_line(
                line,
                &parse_output.ops,
                &mut self.scope_stack,
                &mut self.renderer,
                self.line_index,
            )?;
            self.open_scopes += delta;
            self.output.write_all(formatted.as_bytes())?;
            self.line_index += 1;
            return Ok(());
        }

        // Buffer this line.
        if self.scope_stack_snapshot.is_none() {
            self.scope_stack_snapshot = Some(self.scope_stack.clone());
            self.open_scopes_snapshot = Some(self.open_scopes);
        }
        self.pending_lines.push(line.to_owned());
        self.pending_ops.push(parse_output.ops);
        self.line_index += 1;

        // If speculation ended, flush.
        if !self.parse_state.is_speculative() {
            self.flush_pending()?;
        }

        Ok(())
    }

    fn flush_pending(&mut self) -> Result<(), Error> {
        if self.pending_lines.is_empty() {
            return Ok(());
        }
        let mut scope_stack = self.scope_stack_snapshot.take().unwrap();
        let mut open_scopes = self.open_scopes_snapshot.take().unwrap();
        let line_index_offset = self.line_index - self.pending_lines.len();

        for (i, (line, ops)) in self
            .pending_lines
            .iter()
            .zip(self.pending_ops.iter())
            .enumerate()
        {
            let (formatted, delta) = render_line(
                line,
                ops,
                &mut scope_stack,
                &mut self.renderer,
                line_index_offset + i,
            )?;
            open_scopes += delta;
            self.output.write_all(formatted.as_bytes())?;
        }

        self.scope_stack = scope_stack;
        self.open_scopes = open_scopes;
        self.pending_lines.clear();
        self.pending_ops.clear();
        Ok(())
    }

    /// Close any remaining open scopes and return the finished output sink.
    pub fn finalize(mut self) -> W {
        let _ = self.flush_pending();
        let mut buf = String::new();
        for _ in 0..self.open_scopes {
            self.renderer.end_scope(&mut buf);
        }
        let _ = self.output.write_all(buf.as_bytes());
        self.output
    }
}

// ---------------------------------------------------------------------------
// ThemedANSIScopeRenderer — theme-aware ANSI terminal rendering
// ---------------------------------------------------------------------------

/// A [`ScopeRenderer`] that resolves styles from a theme via
/// [`Highlighter`](crate::highlighting::Highlighter) and emits ANSI 24-bit
/// color escape codes.
///
/// Adjacent text tokens with the same resolved [`Style`] are automatically
/// merged — ANSI escape codes are only emitted when the style actually changes.
///
/// Foreground alpha is blended against the background color. When `include_bg`
/// is true, the background color escape code is also emitted.
///
/// # Example
///
/// ```
/// use syntect::easy::HighlightLines;
/// use syntect::highlighting::ThemeSet;
/// use syntect::parsing::SyntaxSet;
/// use syntect::util::LinesWithEndings;
///
/// let ss = SyntaxSet::load_defaults_newlines();
/// let ts = ThemeSet::load_defaults();
/// let syntax = ss.find_syntax_by_extension("rs").unwrap();
///
/// // Default ANSI output via HighlightLines::new
/// let mut h = HighlightLines::new(syntax, &ss, &ts.themes["base16-ocean.dark"]);
/// for line in LinesWithEndings::from("fn main() {}\n") {
///     h.highlight_line(line).unwrap();
/// }
/// let output = String::from_utf8(h.finalize()).unwrap();
/// assert!(output.contains("\x1b[38;2;"));
/// ```
pub struct ThemedANSIScopeRenderer<'a> {
    highlighter: Highlighter<'a>,
    style_stack: Vec<Style>,
    last_written_style: Option<Style>,
    include_bg: bool,
}

impl<'a> ThemedANSIScopeRenderer<'a> {
    /// Create a new ANSI theme renderer.
    ///
    /// If `include_bg` is true, the background color escape code is emitted.
    pub fn new(theme: &'a Theme, include_bg: bool) -> Self {
        let highlighter = Highlighter::new(theme);
        let default_style = highlighter.style_for_stack(&[]);
        Self {
            highlighter,
            style_stack: vec![default_style],
            last_written_style: None,
            include_bg,
        }
    }

    /// Returns the currently active style.
    pub fn current_style(&self) -> Style {
        self.style_stack.last().copied().unwrap_or_default()
    }
}

impl ScopeRenderer for ThemedANSIScopeRenderer<'_> {
    fn begin_scope(
        &mut self,
        _atom_strs: &[&str],
        _scope: Scope,
        scope_stack: &[Scope],
        _output: &mut String,
    ) -> bool {
        let style = self.highlighter.style_for_stack(scope_stack);
        self.style_stack.push(style);
        false
    }

    fn end_scope(&mut self, _output: &mut String) {
        self.style_stack.pop();
    }

    fn write_text(&mut self, text: &str, output: &mut String) {
        if text.is_empty() {
            return;
        }
        let style = self.current_style();
        if self.last_written_style != Some(style) {
            // ANSI: no close needed, next open overwrites the color.
            if self.include_bg {
                write!(
                    output,
                    "\x1b[48;2;{};{};{}m",
                    style.background.r, style.background.g, style.background.b
                )
                .unwrap();
            }
            let fg = blend_fg_color(style.foreground, style.background);
            write!(output, "\x1b[38;2;{};{};{}m", fg.r, fg.g, fg.b).unwrap();
            self.last_written_style = Some(style);
        }
        output.push_str(text);
    }

    fn end_line(&mut self, _line_index: usize, _scope_stack: &[Scope], _output: &mut String) {
        // Reset style tracking at line boundaries so the next line re-emits.
        self.last_written_style = None;
    }
}

// ---------------------------------------------------------------------------
// HighlightFile — convenience for file highlighting
// ---------------------------------------------------------------------------

/// Convenience struct wrapping a buffered file reader and a [`HighlightLines`]
/// driver, auto-detecting the syntax from the file extension.
///
/// Defaults to theme-based ANSI terminal output via [`ThemedANSIScopeRenderer`].
///
/// # Example
///
/// ```
/// use syntect::parsing::SyntaxSet;
/// use syntect::highlighting::ThemeSet;
/// use syntect::easy::HighlightFile;
/// use std::io::BufRead;
///
/// # use std::io;
/// # fn foo() -> io::Result<()> {
/// let ss = SyntaxSet::load_defaults_newlines();
/// let ts = ThemeSet::load_defaults();
///
/// let mut highlighter = HighlightFile::new("testdata/parser.rs", &ss, &ts.themes["base16-ocean.dark"]).unwrap();
/// let mut line = String::new();
/// while highlighter.reader.read_line(&mut line)? > 0 {
///     highlighter.highlight_line(&line).unwrap();
///     line.clear();
/// }
/// let output = String::from_utf8(highlighter.finalize()).unwrap();
/// # Ok(())
/// # }
/// ```
pub struct HighlightFile<'a, R: ScopeRenderer = ThemedANSIScopeRenderer<'a>, W: io::Write = Vec<u8>>
{
    /// The buffered file reader.
    pub reader: BufReader<File>,
    /// The highlighting driver.
    pub highlight_lines: HighlightLines<'a, R, W>,
}

impl<'a> HighlightFile<'a> {
    /// Constructs a file reader and highlighting driver with default ANSI
    /// output, auto-detecting the syntax from the file extension.
    pub fn new<P: AsRef<Path>>(
        path_obj: P,
        ss: &'a SyntaxSet,
        theme: &'a Theme,
    ) -> io::Result<HighlightFile<'a>> {
        let path: &Path = path_obj.as_ref();
        let f = File::open(path)?;
        let syntax = ss
            .find_syntax_for_file(path)?
            .unwrap_or_else(|| ss.find_syntax_plain_text());

        Ok(HighlightFile {
            reader: BufReader::new(f),
            highlight_lines: HighlightLines::new(syntax, ss, theme),
        })
    }
}

impl<'a, R: ScopeRenderer> HighlightFile<'a, R> {
    /// Constructs a file reader and highlighting driver with a custom
    /// [`ScopeRenderer`], auto-detecting the syntax from the file extension.
    pub fn new_with_renderer<P: AsRef<Path>>(
        path_obj: P,
        ss: &'a SyntaxSet,
        renderer: R,
    ) -> io::Result<HighlightFile<'a, R>> {
        let path: &Path = path_obj.as_ref();
        let f = File::open(path)?;
        let syntax = ss
            .find_syntax_for_file(path)?
            .unwrap_or_else(|| ss.find_syntax_plain_text());

        Ok(HighlightFile {
            reader: BufReader::new(f),
            highlight_lines: HighlightLines::new_with_renderer(syntax, ss, renderer),
        })
    }
}

impl<'a, R: ScopeRenderer, W: io::Write> HighlightFile<'a, R, W> {
    /// Parse and render a single line.
    ///
    /// Delegates to [`HighlightLines::highlight_line`].
    pub fn highlight_line(&mut self, line: &str) -> Result<(), Error> {
        self.highlight_lines.highlight_line(line)
    }

    /// Close any remaining open scopes and return the finished output sink.
    ///
    /// Delegates to [`HighlightLines::finalize`].
    pub fn finalize(self) -> W {
        self.highlight_lines.finalize()
    }
}

// ---------------------------------------------------------------------------
// Iterators
// ---------------------------------------------------------------------------

/// Iterator over the ranges of a line which a given the operation from the parser applies.
///
/// Use [`ScopeRegionIterator`] to obtain directly regions (`&str`s) from the line.
///
/// To use, just keep your own [`ScopeStack`] and then `ScopeStack.apply(op)` the operation that is
/// yielded at the top of your `for` loop over this iterator. Now you have a substring of the line
/// and the scope stack for that token.
///
/// See the `synstats.rs` example for an example of using this iterator.
///
/// **Note:** This will often return empty ranges, just `continue` after applying the op if you
/// don't want them.
///
/// [`ScopeStack`]: ../parsing/struct.ScopeStack.html
/// [`ScopeRegionIterator`]: ./struct.ScopeRegionIterator.html
#[derive(Debug)]
pub struct ScopeRangeIterator<'a> {
    ops: &'a [(usize, ScopeStackOp)],
    line: &'a str,
    index: usize,
    last_str_index: usize,
}

impl<'a> ScopeRangeIterator<'a> {
    pub fn new(ops: &'a [(usize, ScopeStackOp)], line: &'a str) -> ScopeRangeIterator<'a> {
        ScopeRangeIterator {
            ops,
            line,
            index: 0,
            last_str_index: 0,
        }
    }
}

static NOOP_OP: ScopeStackOp = ScopeStackOp::Noop;

impl<'a> Iterator for ScopeRangeIterator<'a> {
    type Item = (std::ops::Range<usize>, &'a ScopeStackOp);
    fn next(&mut self) -> Option<Self::Item> {
        if self.index > self.ops.len() {
            return None;
        }

        // region extends up to next operation (ops[index]) or string end if there is none
        // note the next operation may be at, last_str_index, in which case the region is empty
        let next_str_i = if self.index == self.ops.len() {
            self.line.len()
        } else {
            self.ops[self.index].0
        };
        let range = self.last_str_index..next_str_i;
        self.last_str_index = next_str_i;

        // the first region covers everything before the first op, which may be empty
        let op = if self.index == 0 {
            &NOOP_OP
        } else {
            &self.ops[self.index - 1].1
        };

        self.index += 1;
        Some((range, op))
    }
}

/// A convenience wrapper over [`ScopeRangeIterator`] to return `&str`s directly.
///
/// To use, just keep your own [`ScopeStack`] and then `ScopeStack.apply(op)` the operation that is
/// yielded at the top of your `for` loop over this iterator. Now you have a substring of the line
/// and the scope stack for that token.
///
/// See the `synstats.rs` example for an example of using this iterator.
///
/// **Note:** This will often return empty regions, just `continue` after applying the op if you
/// don't want them.
///
/// [`ScopeStack`]: ../parsing/struct.ScopeStack.html
/// [`ScopeRangeIterator`]: ./struct.ScopeRangeIterator.html
#[derive(Debug)]
pub struct ScopeRegionIterator<'a> {
    range_iter: ScopeRangeIterator<'a>,
}

impl<'a> ScopeRegionIterator<'a> {
    pub fn new(ops: &'a [(usize, ScopeStackOp)], line: &'a str) -> ScopeRegionIterator<'a> {
        ScopeRegionIterator {
            range_iter: ScopeRangeIterator::new(ops, line),
        }
    }
}

impl<'a> Iterator for ScopeRegionIterator<'a> {
    type Item = (&'a str, &'a ScopeStackOp);
    fn next(&mut self) -> Option<Self::Item> {
        let (range, op) = self.range_iter.next()?;
        Some((&self.range_iter.line[range], op))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "default-themes")]
    use crate::highlighting::ThemeSet;
    use crate::parsing::{ParseState, ScopeStack, SyntaxSet};
    use std::str::FromStr;

    #[cfg(all(feature = "default-syntaxes", feature = "default-themes"))]
    #[test]
    fn can_highlight_file() {
        let ss = SyntaxSet::load_defaults_nonewlines();
        let ts = ThemeSet::load_defaults();
        HighlightFile::new(
            "testdata/highlight_test.erb",
            &ss,
            &ts.themes["base16-ocean.dark"],
        )
        .unwrap();
    }

    #[cfg(all(feature = "default-syntaxes", feature = "default-themes"))]
    #[test]
    fn can_highlight_file_line_by_line() {
        use std::io::BufRead;
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let mut hf = HighlightFile::new(
            "testdata/highlight_test.erb",
            &ss,
            &ts.themes["base16-ocean.dark"],
        )
        .unwrap();
        let mut line = String::new();
        while hf.reader.read_line(&mut line).unwrap() > 0 {
            hf.highlight_line(&line).unwrap();
            line.clear();
        }
        let output = String::from_utf8(hf.finalize()).unwrap();
        assert!(!output.is_empty());
    }

    #[cfg(all(feature = "default-syntaxes", feature = "default-themes"))]
    #[test]
    fn theme_scope_renderer_produces_output() {
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let syntax = ss.find_syntax_by_extension("rs").unwrap();

        let mut h = HighlightLines::new(syntax, &ss, &ts.themes["base16-ocean.dark"]);
        h.highlight_line("pub struct Wow { hi: u64 }\n").unwrap();
        let output = String::from_utf8(h.finalize()).unwrap();
        assert!(!output.is_empty());
        assert!(output.contains("\x1b[38;2;"));
    }

    #[cfg(all(feature = "default-syntaxes", feature = "default-themes"))]
    #[test]
    fn style_merging_coalesces_same_style_tokens() {
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let syntax = ss.find_syntax_by_extension("rs").unwrap();

        let mut h = HighlightLines::new(syntax, &ss, &ts.themes["base16-ocean.dark"]);
        h.highlight_line("fn main() {}\n").unwrap();
        let output = String::from_utf8(h.finalize()).unwrap();

        // Style merging means we should NOT see consecutive identical ANSI
        // escape codes with no text between them.
        assert!(!output.contains("m\x1b[38;2;"));
    }

    #[cfg(all(feature = "default-syntaxes", feature = "default-themes"))]
    #[test]
    fn can_start_again_from_previous_state() {
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let theme = &ts.themes["base16-ocean.dark"];
        let mut highlighter =
            HighlightLines::new(ss.find_syntax_by_extension("py").unwrap(), &ss, theme);

        let lines = ["\"\"\"\n", "def foo():\n", "\"\"\"\n"];

        highlighter.highlight_line(lines[0]).expect("#[cfg(test)]");

        let (parse_state, scope_stack) = highlighter.state();
        let (parse_state, scope_stack) = (parse_state.clone(), scope_stack.clone());
        let first_output = String::from_utf8(highlighter.finalize()).unwrap();

        let mut other_highlighter = HighlightLines::from_state(
            parse_state,
            scope_stack,
            &ss,
            ThemedANSIScopeRenderer::new(theme, false),
            Vec::new(),
        );

        other_highlighter
            .highlight_line(lines[1])
            .expect("#[cfg(test)]");
        let second_output = String::from_utf8(other_highlighter.finalize()).unwrap();

        // The second line should be highlighted as a docstring (same style as
        // the first line's triple-quote) because the parse state carries the
        // string context forward.
        assert!(!second_output.is_empty());
        let extract_fg =
            |s: &str| -> Option<String> { s.find("\x1b[38;2;").map(|i| s[i..i + 16].to_string()) };
        assert_eq!(extract_fg(&first_output), extract_fg(&second_output));
    }

    #[cfg(feature = "default-syntaxes")]
    #[test]
    fn can_find_regions() {
        let ss = SyntaxSet::load_defaults_nonewlines();
        let mut state = ParseState::new(ss.find_syntax_by_extension("rb").unwrap());
        let line = "lol =5+2";
        let ops = state.parse_line(line, &ss).expect("#[cfg(test)]").ops;

        let mut stack = ScopeStack::new();
        let mut token_count = 0;
        for (s, op) in ScopeRegionIterator::new(&ops, line) {
            stack.apply(op).expect("#[cfg(test)]");
            if s.is_empty() {
                // in this case we don't care about blank tokens
                continue;
            }
            if token_count == 1 {
                assert_eq!(
                    stack,
                    ScopeStack::from_str("source.ruby keyword.operator.assignment.ruby").unwrap()
                );
                assert_eq!(s, "=");
            }
            token_count += 1;
            println!("{:?} {}", s, stack);
        }
        assert_eq!(token_count, 5);
    }

    #[cfg(feature = "default-syntaxes")]
    #[test]
    fn can_find_regions_with_trailing_newline() {
        let ss = SyntaxSet::load_defaults_newlines();
        let mut state = ParseState::new(ss.find_syntax_by_extension("rb").unwrap());
        let lines = ["# hello world\n", "lol=5+2\n"];
        let mut stack = ScopeStack::new();

        for line in lines.iter() {
            let ops = state.parse_line(line, &ss).expect("#[cfg(test)]").ops;
            println!("{:?}", ops);

            let mut iterated_ops: Vec<&ScopeStackOp> = Vec::new();
            for (_, op) in ScopeRegionIterator::new(&ops, line) {
                stack.apply(op).expect("#[cfg(test)]");
                iterated_ops.push(op);
                println!("{:?}", op);
            }

            let all_ops = ops.iter().map(|t| &t.1);
            assert_eq!(all_ops.count(), iterated_ops.len() - 1); // -1 because we want to ignore the NOOP
        }
    }
}
