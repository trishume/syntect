//! Streaming syntax highlighting built on top of [`std::io::Write`].
//!
//! The central type is [`HighlightedWriter`], an implementation of
//! [`std::io::Write`] that drives syntax parsing and delegates rendering to a
//! pluggable [`ScopeRenderer`]. Bytes written into the writer are buffered
//! into complete lines and forwarded to the highlighter; rendered output is
//! streamed to the inner sink.
//!
//! Renderer plumbing — the [`ScopeRenderer`] / [`ScopeMarkup`] /
//! [`StyledOutput`] traits, the [`ThemedRenderer`] adapter, and the built-in
//! [`AnsiStyledOutput`] — lives in [`crate::rendering`]. Most users construct
//! a `HighlightedWriter` via one of the convenience constructors below
//! ([`new`], [`with_markup`], [`with_themed`]) without touching the
//! lower-level types directly.
//!
//! For HTML output, see [`crate::html::ClassedHTMLGenerator`],
//! [`crate::html::ClassedHTMLScopeRenderer`], and
//! [`crate::html::HtmlStyledOutput`].
//!
//! [`new`]: HighlightedWriter::new
//! [`with_markup`]: HighlightedWriter::with_markup
//! [`with_themed`]: HighlightedWriter::with_themed

use crate::highlighting::Theme;
use crate::parsing::{
    lock_global_scope_repo, ParseState, ScopeStack, ScopeStackOp, SyntaxReference, SyntaxSet,
};
use crate::rendering::{
    render_line, resolve_atom_strs, AnsiStyledOutput, MarkupAdapter, ScopeMarkup, ScopeRenderer,
    StyledOutput, ThemedRenderer,
};
use crate::Error;
use std::io::{self, Write};

// ---------------------------------------------------------------------------
// HighlightedWriter — io::Write-based highlighting driver
// ---------------------------------------------------------------------------

/// A streaming syntax highlighter that implements [`std::io::Write`].
///
/// Bytes written into the writer are accumulated until a newline is seen,
/// at which point each complete line is parsed, rendered through the
/// configured renderer, and forwarded to the inner [`Write`] sink.
///
/// Most users construct a `HighlightedWriter` via one of the convenience
/// constructors:
///
/// - [`new`] — ANSI 24-bit colour output for a given [`Theme`] (the default).
/// - [`with_markup`] — pass any [`ScopeMarkup`] (stateless markup renderer
///   like CSS-classed HTML).
/// - [`with_themed`] — pass any [`StyledOutput`] (theme-aware emitter like
///   inline-styled HTML or LaTeX `\textcolor`).
/// - [`with_renderer`] / [`with_renderer_and_output`] — low-level escape
///   hatch that takes a raw [`ScopeRenderer`].
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
/// [`new`]: HighlightedWriter::new
/// [`with_markup`]: HighlightedWriter::with_markup
/// [`with_themed`]: HighlightedWriter::with_themed
/// [`with_renderer`]: HighlightedWriter::with_renderer
/// [`with_renderer_and_output`]: HighlightedWriter::with_renderer_and_output
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
/// use syntect::rendering::AnsiStyledOutput;
///
/// let ss = SyntaxSet::load_defaults_newlines();
/// let ts = ThemeSet::load_defaults();
/// let syntax = ss.find_syntax_by_extension("rs").unwrap();
///
/// let mut f = std::fs::File::open("examples/parsyncat.rs").unwrap();
/// let mut w = HighlightedWriter::with_themed(
///     syntax,
///     &ss,
///     &ts.themes["base16-ocean.dark"],
///     AnsiStyledOutput::new(false),
/// );
/// io::copy(&mut f, &mut w).unwrap();
/// w.finalize().unwrap();
/// ```
///
/// Highlighting an in-memory string with the convenience default:
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
    R: ScopeRenderer = ThemedRenderer<'a, AnsiStyledOutput>,
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
    /// Internally constructs `ThemedRenderer::new(theme, AnsiStyledOutput::new(false))`
    /// to produce 24-bit colour ANSI escape codes. The output is collected
    /// into a `Vec<u8>` returned by [`finalize`].
    ///
    /// [`finalize`]: HighlightedWriter::finalize
    pub fn new(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        theme: &'a Theme,
    ) -> HighlightedWriter<'a, ThemedRenderer<'a, AnsiStyledOutput>> {
        Self::with_themed(
            syntax_reference,
            syntax_set,
            theme,
            AnsiStyledOutput::new(false),
        )
    }
}

impl<'a, M: ScopeMarkup> HighlightedWriter<'a, MarkupAdapter<M>> {
    /// Create a new highlighting writer driven by a [`ScopeMarkup`]
    /// implementation.
    ///
    /// The markup renderer is wrapped in an internal adapter that bridges it
    /// to the engine's [`ScopeRenderer`] trait. Output is collected into a
    /// `Vec<u8>` returned by [`finalize`].
    ///
    /// Use this for stateless renderers that map scope structure 1:1 to
    /// output structure (e.g. [`crate::html::ClassedHTMLScopeRenderer`]).
    ///
    /// [`finalize`]: HighlightedWriter::finalize
    pub fn with_markup(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        markup: M,
    ) -> HighlightedWriter<'a, MarkupAdapter<M>> {
        Self::with_renderer(syntax_reference, syntax_set, MarkupAdapter::new(markup))
    }
}

impl<'a, O: StyledOutput> HighlightedWriter<'a, ThemedRenderer<'a, O>> {
    /// Create a new highlighting writer driven by a [`StyledOutput`]
    /// implementation paired with a [`Theme`].
    ///
    /// The styled output is wrapped in a [`ThemedRenderer`] that resolves
    /// scopes to styles via the theme and merges adjacent same-styled
    /// tokens. Output is collected into a `Vec<u8>` returned by [`finalize`].
    ///
    /// Use this for theme-aware renderers like [`AnsiStyledOutput`],
    /// [`crate::html::HtmlStyledOutput`], or your own format.
    ///
    /// [`finalize`]: HighlightedWriter::finalize
    pub fn with_themed(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        theme: &'a Theme,
        output: O,
    ) -> HighlightedWriter<'a, ThemedRenderer<'a, O>> {
        Self::with_renderer(
            syntax_reference,
            syntax_set,
            ThemedRenderer::new(theme, output),
        )
    }
}

impl<'a, R: ScopeRenderer> HighlightedWriter<'a, R> {
    /// Create a new highlighting writer with a custom [`ScopeRenderer`].
    ///
    /// This is the **low-level** escape hatch — most users want
    /// [`with_markup`] (for stateless markup) or [`with_themed`] (for
    /// theme-aware emitters) instead.
    ///
    /// The output is collected into a `Vec<u8>` returned by [`finalize`].
    /// Use [`with_renderer_and_output`] to stream to an arbitrary
    /// [`io::Write`] sink instead.
    ///
    /// [`with_markup`]: HighlightedWriter::with_markup
    /// [`with_themed`]: HighlightedWriter::with_themed
    /// [`finalize`]: HighlightedWriter::finalize
    /// [`with_renderer_and_output`]: HighlightedWriter::with_renderer_and_output
    pub fn with_renderer(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        renderer: R,
    ) -> HighlightedWriter<'a, R> {
        Self::with_renderer_and_output(syntax_reference, syntax_set, renderer, Vec::new())
    }
}

impl<'a, R: ScopeRenderer, W: io::Write> HighlightedWriter<'a, R, W> {
    /// Create a new highlighting writer that writes to the given output sink.
    ///
    /// This is the **low-level** escape hatch with explicit output sink —
    /// most users should reach for [`with_markup`] or [`with_themed`].
    ///
    /// [`with_markup`]: HighlightedWriter::with_markup
    /// [`with_themed`]: HighlightedWriter::with_themed
    pub fn with_renderer_and_output(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlighting::ThemeSet;
    use crate::parsing::SyntaxSet;
    use crate::rendering::{AnsiStyledOutput, ThemedRenderer};

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
            ThemedRenderer::new(theme, AnsiStyledOutput::new(false)),
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
