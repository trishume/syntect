//! Streaming syntax highlighting built on top of [`std::io::Write`].
//!
//! The central type is [`HighlightedWriter`], an implementation of
//! [`std::io::Write`] that drives syntax parsing and delegates rendering to a
//! pluggable [`ScopeRenderer`]. Bytes written into the writer are buffered
//! into complete lines and forwarded to the highlighter; rendered output is
//! streamed to the inner sink.
//!
//! For theme-based highlighting (resolving scopes to colors),
//! [`ThemedANSIScopeRenderer`] resolves scopes to styles via
//! [`Highlighter`] and emits ANSI 24-bit color escape codes (the default
//! renderer for [`HighlightedWriter`]).
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
use std::fmt::Write as FmtWrite;
use std::io::{self, Write};

// ---------------------------------------------------------------------------
// ScopeRenderer trait and render_line
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
// HighlightedWriter — io::Write-based highlighting driver
// ---------------------------------------------------------------------------

/// A streaming syntax highlighter that implements [`std::io::Write`].
///
/// Bytes written into the writer are accumulated until a newline is seen,
/// at which point each complete line is parsed, rendered through the
/// configured [`ScopeRenderer`], and forwarded to the inner [`Write`] sink.
///
/// When the parser is in speculative mode (inside a branch point),
/// `HighlightedWriter` buffers rendered output internally and flushes it
/// only once the speculation resolves, replaying corrected operations if a
/// cross-line `fail` occurred.
///
/// A trailing partial line (one that did not end with `\n`) is held in the
/// internal buffer until either another `\n` arrives or [`finalize`] is
/// called. [`finalize`] **must** be called to flush that trailing line and
/// to close any open scopes.
///
/// [`finalize`]: HighlightedWriter::finalize
///
/// # Examples
///
/// Theme-based highlighting (default renderer), composing with
/// [`std::io::copy`]:
///
/// ```no_run
/// use std::io::{self, Write};
/// use syntect::io::HighlightedWriter;
/// use syntect::highlighting::ThemeSet;
/// use syntect::parsing::SyntaxSet;
///
/// let ss = SyntaxSet::load_defaults_newlines();
/// let ts = ThemeSet::load_defaults();
/// let syntax = ss.find_syntax_by_extension("rs").unwrap();
///
/// let mut f = std::fs::File::open("examples/parsyncat.rs").unwrap();
/// let mut w = HighlightedWriter::new_with_renderer_and_output(
///     syntax,
///     &ss,
///     syntect::io::ThemedANSIScopeRenderer::new(&ts.themes["base16-ocean.dark"], false),
///     io::stdout().lock(),
/// );
/// io::copy(&mut f, &mut w).unwrap();
/// w.finalize().unwrap();
/// ```
///
/// Highlighting an in-memory string:
///
/// ```
/// use std::io::Write;
/// use syntect::io::HighlightedWriter;
/// use syntect::highlighting::ThemeSet;
/// use syntect::parsing::SyntaxSet;
///
/// let ss = SyntaxSet::load_defaults_newlines();
/// let ts = ThemeSet::load_defaults();
/// let syntax = ss.find_syntax_by_extension("rs").unwrap();
///
/// let mut w = HighlightedWriter::new(syntax, &ss, &ts.themes["base16-ocean.dark"]);
/// w.write_all(b"fn main() {}\n").unwrap();
/// let output = String::from_utf8(w.finalize().unwrap()).unwrap();
/// assert!(output.contains("\x1b[38;2;"));
/// ```
#[must_use = "HighlightedWriter holds buffered output that requires `finalize()` to flush"]
pub struct HighlightedWriter<
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
    line_buf: Vec<u8>,
    // Branch-point buffering
    pending_lines: Vec<String>,
    pending_ops: Vec<Vec<(usize, ScopeStackOp)>>,
    scope_stack_snapshot: Option<ScopeStack>,
    open_scopes_snapshot: Option<isize>,
}

impl<'a> HighlightedWriter<'a> {
    /// Create a new highlighting writer with default ANSI terminal output.
    ///
    /// Uses [`ThemedANSIScopeRenderer`] to produce 24-bit color ANSI escape
    /// codes. The output is collected into a `Vec<u8>` returned by [`finalize`].
    ///
    /// [`finalize`]: HighlightedWriter::finalize
    pub fn new(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        theme: &'a Theme,
    ) -> HighlightedWriter<'a> {
        let renderer = ThemedANSIScopeRenderer::new(theme, false);
        Self::new_with_renderer_and_output(syntax_reference, syntax_set, renderer, Vec::new())
    }
}

impl<'a, R: ScopeRenderer> HighlightedWriter<'a, R> {
    /// Create a new highlighting writer with a custom [`ScopeRenderer`].
    ///
    /// The output is collected into a `Vec<u8>` returned by [`finalize`].
    /// Use [`new_with_renderer_and_output`] to stream to an arbitrary
    /// [`io::Write`] sink instead.
    ///
    /// [`finalize`]: HighlightedWriter::finalize
    /// [`new_with_renderer_and_output`]: HighlightedWriter::new_with_renderer_and_output
    pub fn new_with_renderer(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        renderer: R,
    ) -> HighlightedWriter<'a, R> {
        Self::new_with_renderer_and_output(syntax_reference, syntax_set, renderer, Vec::new())
    }
}

impl<'a, R: ScopeRenderer, W: io::Write> HighlightedWriter<'a, R, W> {
    /// Create a new highlighting writer that writes to the given output sink.
    ///
    /// This allows streaming rendered output directly to a file, socket,
    /// or buffered writer without intermediate allocation.
    pub fn new_with_renderer_and_output(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        renderer: R,
        output: W,
    ) -> HighlightedWriter<'a, R, W> {
        HighlightedWriter {
            syntax_set,
            open_scopes: 0,
            parse_state: ParseState::new(syntax_reference),
            scope_stack: ScopeStack::new(),
            output,
            renderer,
            line_index: 0,
            line_buf: Vec::new(),
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
    ) -> HighlightedWriter<'a, R, W> {
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
        HighlightedWriter {
            syntax_set,
            open_scopes,
            parse_state,
            scope_stack,
            output,
            renderer,
            line_index: 0,
            line_buf: Vec::new(),
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

    /// Consume the writer and return its parts.
    ///
    /// Any pending buffered output is flushed before returning. The trailing
    /// partial line (if any) and any open scopes are **not** closed by this
    /// method — use [`finalize`] for that.
    ///
    /// [`finalize`]: HighlightedWriter::finalize
    pub fn into_parts(mut self) -> (ParseState, ScopeStack, R, W) {
        let _ = self.flush_pending();
        (
            self.parse_state,
            self.scope_stack,
            self.renderer,
            self.output,
        )
    }

    /// Parse one complete line and forward the rendered output to the sink.
    ///
    /// This is the inner per-line entry point used by the [`Write`]
    /// implementation once a complete `\n`-terminated line has been
    /// assembled in the internal buffer.
    fn highlight_line(&mut self, line: &str) -> Result<(), Error> {
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

    /// Flush any trailing partial line, close open scopes, and return the
    /// inner output sink.
    ///
    /// This **must** be called to drain a final line that was written without
    /// a trailing `\n`, and to emit the closing tags for any scopes still open
    /// at end-of-input.
    pub fn finalize(mut self) -> io::Result<W> {
        // Drain a trailing partial line, if any.
        if !self.line_buf.is_empty() {
            let line_bytes = std::mem::take(&mut self.line_buf);
            let line = std::str::from_utf8(&line_bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            self.highlight_line(line)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        }
        self.flush_pending()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let mut buf = String::new();
        for _ in 0..self.open_scopes {
            self.renderer.end_scope(&mut buf);
        }
        self.output.write_all(buf.as_bytes())?;
        Ok(self.output)
    }
}

impl<'a, R: ScopeRenderer, W: io::Write> Write for HighlightedWriter<'a, R, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.line_buf.extend_from_slice(buf);

        // Process every complete line in a single batch. Newlines (0x0A) cannot
        // appear inside a multi-byte UTF-8 sequence, so locating the last
        // newline is safe on the raw byte buffer.
        if let Some(last_nl) = self.line_buf.iter().rposition(|&b| b == b'\n') {
            let completed: Vec<u8> = self.line_buf.drain(..=last_nl).collect();
            let s = std::str::from_utf8(&completed)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            for line in s.split_inclusive('\n') {
                self.highlight_line(line)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            }
        }
        Ok(buf.len())
    }

    /// Forwards to the inner sink. Partial trailing lines remain buffered;
    /// they are not highlightable until terminated by `\n` or by
    /// [`finalize`](HighlightedWriter::finalize).
    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

// ---------------------------------------------------------------------------
// ThemedANSIScopeRenderer — theme-aware ANSI terminal rendering
// ---------------------------------------------------------------------------

/// A [`ScopeRenderer`] that resolves styles from a theme via
/// [`Highlighter`] and emits ANSI 24-bit
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
/// use std::io::Write;
/// use syntect::io::HighlightedWriter;
/// use syntect::highlighting::ThemeSet;
/// use syntect::parsing::SyntaxSet;
///
/// let ss = SyntaxSet::load_defaults_newlines();
/// let ts = ThemeSet::load_defaults();
/// let syntax = ss.find_syntax_by_extension("rs").unwrap();
///
/// // Default ANSI output via HighlightedWriter::new
/// let mut w = HighlightedWriter::new(syntax, &ss, &ts.themes["base16-ocean.dark"]);
/// w.write_all(b"fn main() {}\n").unwrap();
/// let output = String::from_utf8(w.finalize().unwrap()).unwrap();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlighting::ThemeSet;
    use crate::parsing::SyntaxSet;

    #[cfg(all(feature = "default-syntaxes", feature = "default-themes"))]
    #[test]
    fn theme_scope_renderer_produces_output() {
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let syntax = ss.find_syntax_by_extension("rs").unwrap();

        let mut w = HighlightedWriter::new(syntax, &ss, &ts.themes["base16-ocean.dark"]);
        w.write_all(b"pub struct Wow { hi: u64 }\n").unwrap();
        let output = String::from_utf8(w.finalize().unwrap()).unwrap();
        assert!(!output.is_empty());
        assert!(output.contains("\x1b[38;2;"));
    }

    #[cfg(all(feature = "default-syntaxes", feature = "default-themes"))]
    #[test]
    fn style_merging_coalesces_same_style_tokens() {
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let syntax = ss.find_syntax_by_extension("rs").unwrap();

        let mut w = HighlightedWriter::new(syntax, &ss, &ts.themes["base16-ocean.dark"]);
        w.write_all(b"fn main() {}\n").unwrap();
        let output = String::from_utf8(w.finalize().unwrap()).unwrap();

        // Style merging means we should NOT see consecutive identical ANSI
        // escape codes with no text between them.
        assert!(!output.contains("m\x1b[38;2;"));
    }

    #[cfg(all(feature = "default-syntaxes", feature = "default-themes"))]
    #[test]
    fn buffers_partial_line_until_newline_or_finalize() {
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let syntax = ss.find_syntax_by_extension("rs").unwrap();

        let mut w = HighlightedWriter::new(syntax, &ss, &ts.themes["base16-ocean.dark"]);
        // Write a line in three chunks, with the newline arriving last.
        w.write_all(b"fn main").unwrap();
        w.write_all(b"() {").unwrap();
        w.write_all(b"}\n").unwrap();
        // And a trailing line that finalize must flush.
        w.write_all(b"struct S;").unwrap();
        let output = String::from_utf8(w.finalize().unwrap()).unwrap();
        assert!(output.contains("fn"));
        assert!(output.contains("main"));
        assert!(output.contains("struct"));
    }

    #[cfg(all(feature = "default-syntaxes", feature = "default-themes"))]
    #[test]
    fn handles_multibyte_chars_split_across_writes() {
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let syntax = ss.find_syntax_by_extension("rs").unwrap();

        let mut w = HighlightedWriter::new(syntax, &ss, &ts.themes["base16-ocean.dark"]);
        // 'é' is two bytes (0xC3 0xA9). Split between writes.
        w.write_all(b"// caf\xC3").unwrap();
        w.write_all(b"\xA9\n").unwrap();
        let output = String::from_utf8(w.finalize().unwrap()).unwrap();
        assert!(output.contains("café"));
    }

    #[cfg(all(feature = "default-syntaxes", feature = "default-themes"))]
    #[test]
    fn can_start_again_from_previous_state() {
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let theme = &ts.themes["base16-ocean.dark"];
        let mut w = HighlightedWriter::new(ss.find_syntax_by_extension("py").unwrap(), &ss, theme);

        let lines = ["\"\"\"\n", "def foo():\n", "\"\"\"\n"];
        w.write_all(lines[0].as_bytes()).unwrap();

        let (parse_state, scope_stack) = w.state();
        let (parse_state, scope_stack) = (parse_state.clone(), scope_stack.clone());
        let first_output = String::from_utf8(w.finalize().unwrap()).unwrap();

        let mut other = HighlightedWriter::from_state(
            parse_state,
            scope_stack,
            &ss,
            ThemedANSIScopeRenderer::new(theme, false),
            Vec::new(),
        );
        other.write_all(lines[1].as_bytes()).unwrap();
        let second_output = String::from_utf8(other.finalize().unwrap()).unwrap();

        // The second line should be highlighted as a docstring (same style as
        // the first line's triple-quote) because the parse state carries the
        // string context forward.
        assert!(!second_output.is_empty());
        let extract_fg =
            |s: &str| -> Option<String> { s.find("\x1b[38;2;").map(|i| s[i..i + 16].to_string()) };
        assert_eq!(extract_fg(&first_output), extract_fg(&second_output));
    }
}
