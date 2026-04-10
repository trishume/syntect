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
//! [`AnsiStyledOutput`] — lives in [`crate::rendering`]. Construct a
//! `HighlightedWriter` by calling one of [`HighlightedWriter::from_themed`],
//! [`HighlightedWriter::from_markup`], or [`HighlightedWriter::from_renderer`] to
//! get a [`HighlightedWriterBuilder`], then chain `.with_output(...)` /
//! `.with_state(...)` as needed and finish with `.build()`.
//!
//! For HTML output, see [`crate::html::ClassedHTMLGenerator`],
//! [`crate::html::ClassedHTMLScopeRenderer`], and
//! [`crate::html::HtmlStyledOutput`].

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
/// Construct one via the [`HighlightedWriterBuilder`]. There are three
/// entry points, one per renderer category:
///
/// - [`from_themed`] — pair a [`Theme`] with any [`StyledOutput`] (the
///   standard choice for terminal colours via [`AnsiStyledOutput`],
///   inline-styled HTML, LaTeX `\textcolor`, etc.).
/// - [`from_markup`] — pass any [`ScopeMarkup`] (stateless renderer like
///   CSS-classed HTML).
/// - [`from_renderer`] — low-level escape hatch that takes a raw
///   [`ScopeRenderer`].
///
/// Each entry point returns a [`HighlightedWriterBuilder`] pre-populated
/// with a `Vec<u8>` sink and no resume state. Configure further with
/// [`Builder::with_output`] / [`Builder::with_state`] and finish with
/// [`Builder::build`].
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
/// [`from_themed`]: HighlightedWriter::from_themed
/// [`from_markup`]: HighlightedWriter::from_markup
/// [`from_renderer`]: HighlightedWriter::from_renderer
/// [`Builder::with_output`]: HighlightedWriterBuilder::with_output
/// [`Builder::with_state`]: HighlightedWriterBuilder::with_state
/// [`Builder::build`]: HighlightedWriterBuilder::build
/// [`finalize`]: HighlightedWriter::finalize
///
/// # Examples
///
/// Theme-based ANSI highlighting streamed straight to a sink, composing with
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
/// let mut w = HighlightedWriter::from_themed(
///     syntax,
///     &ss,
///     &ts.themes["base16-ocean.dark"],
///     AnsiStyledOutput::new(false),
/// )
/// .with_output(io::stdout().lock())
/// .build();
/// io::copy(&mut f, &mut w).unwrap();
/// w.finalize().unwrap();
/// ```
///
/// Highlighting an in-memory string with the default `Vec<u8>` sink:
///
/// ```
/// use std::io::Write;
/// use syntect::io::HighlightedWriter;
/// use syntect::highlighting::ThemeSet;
/// use syntect::parsing::SyntaxSet;
/// use syntect::rendering::AnsiStyledOutput;
///
/// let ss = SyntaxSet::load_defaults_newlines();
/// let ts = ThemeSet::load_defaults();
/// let syntax = ss.find_syntax_by_extension("rs").unwrap();
///
/// let mut w = HighlightedWriter::from_themed(
///     syntax,
///     &ss,
///     &ts.themes["base16-ocean.dark"],
///     AnsiStyledOutput::new(false),
/// )
/// .build();
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
    /// Start a builder for a theme-aware renderer.
    ///
    /// Pairs the given [`Theme`] with any [`StyledOutput`] (`AnsiStyledOutput`,
    /// `HtmlStyledOutput`, or your own format) and returns a
    /// [`HighlightedWriterBuilder`] that you can further configure with
    /// [`with_output`] / [`with_state`] before calling [`build`].
    ///
    /// For ANSI 24-bit terminal colours, pass `AnsiStyledOutput::new(false)`.
    ///
    /// [`with_output`]: HighlightedWriterBuilder::with_output
    /// [`with_state`]: HighlightedWriterBuilder::with_state
    /// [`build`]: HighlightedWriterBuilder::build
    pub fn from_themed<O: StyledOutput>(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        theme: &'a Theme,
        output: O,
    ) -> HighlightedWriterBuilder<'a, ThemedRenderer<'a, O>> {
        HighlightedWriterBuilder::new(
            syntax_reference,
            syntax_set,
            ThemedRenderer::new(theme, output),
        )
    }

    /// Start a builder for a stateless markup renderer.
    ///
    /// The given [`ScopeMarkup`] implementation is wrapped in an internal
    /// adapter that bridges it to the engine's low-level renderer trait.
    /// Use this for renderers that map scope structure 1:1 to output
    /// structure (e.g. [`crate::html::ClassedHTMLScopeRenderer`]).
    pub fn from_markup<M: ScopeMarkup>(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        markup: M,
    ) -> HighlightedWriterBuilder<'a, MarkupAdapter<M>> {
        HighlightedWriterBuilder::new(syntax_reference, syntax_set, MarkupAdapter::new(markup))
    }

    /// Start a builder with a low-level [`ScopeRenderer`].
    ///
    /// This is the escape hatch for advanced cases that need raw [`Scope`]
    /// / `&[Scope]` access or selective `bool` returns from `begin_scope`.
    /// Most users should reach for [`from_themed`] or [`from_markup`] instead.
    ///
    /// [`Scope`]: crate::parsing::Scope
    /// [`from_themed`]: HighlightedWriter::from_themed
    /// [`from_markup`]: HighlightedWriter::from_markup
    pub fn from_renderer<R: ScopeRenderer>(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        renderer: R,
    ) -> HighlightedWriterBuilder<'a, R> {
        HighlightedWriterBuilder::new(syntax_reference, syntax_set, renderer)
    }
}

/// Fluent builder for a [`HighlightedWriter`].
///
/// Construct one via [`HighlightedWriter::from_themed`],
/// [`HighlightedWriter::from_markup`], or
/// [`HighlightedWriter::from_renderer`] — each picks a renderer category and
/// returns a builder pre-populated with a default `Vec<u8>` output sink and
/// no resume state. Configure further with [`with_output`] (replace the sink)
/// and [`with_state`] (resume from a saved checkpoint), then call [`build`]
/// to materialise the writer.
///
/// [`with_output`]: HighlightedWriterBuilder::with_output
/// [`with_state`]: HighlightedWriterBuilder::with_state
/// [`build`]: HighlightedWriterBuilder::build
#[must_use = "HighlightedWriterBuilder produces nothing until `.build()` is called"]
pub struct HighlightedWriterBuilder<'a, R: ScopeRenderer, W: io::Write = Vec<u8>> {
    syntax_reference: &'a SyntaxReference,
    syntax_set: &'a SyntaxSet,
    renderer: R,
    output: W,
    state: Option<(ParseState, ScopeStack)>,
}

impl<'a, R: ScopeRenderer> HighlightedWriterBuilder<'a, R, Vec<u8>> {
    fn new(syntax_reference: &'a SyntaxReference, syntax_set: &'a SyntaxSet, renderer: R) -> Self {
        Self {
            syntax_reference,
            syntax_set,
            renderer,
            output: Vec::new(),
            state: None,
        }
    }
}

impl<'a, R: ScopeRenderer, W: io::Write> HighlightedWriterBuilder<'a, R, W> {
    /// Replace the default `Vec<u8>` output sink with an arbitrary
    /// [`io::Write`].
    pub fn with_output<W2: io::Write>(self, output: W2) -> HighlightedWriterBuilder<'a, R, W2> {
        HighlightedWriterBuilder {
            syntax_reference: self.syntax_reference,
            syntax_set: self.syntax_set,
            renderer: self.renderer,
            output,
            state: self.state,
        }
    }

    /// Resume highlighting from a previously saved parse + scope state.
    ///
    /// Useful for incremental highlighting where you cache the state at
    /// checkpoints and want to pick up from where a previous run left off.
    /// The renderer's internal state (e.g. a `ThemedRenderer`'s style stack)
    /// is replayed to match the restored scope stack.
    pub fn with_state(mut self, parse_state: ParseState, scope_stack: ScopeStack) -> Self {
        self.state = Some((parse_state, scope_stack));
        self
    }

    /// Materialise the configured [`HighlightedWriter`].
    pub fn build(self) -> HighlightedWriter<'a, R, W> {
        let HighlightedWriterBuilder {
            syntax_reference,
            syntax_set,
            mut renderer,
            output,
            state,
        } = self;

        match state {
            None => HighlightedWriter {
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
            },
            Some((parse_state, scope_stack)) => {
                // Replay the existing scope stack to the renderer so that
                // its internal state (e.g. a ThemedRenderer's style stack)
                // matches the restored scopes.
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
        }
    }
}

impl<'a, R: ScopeRenderer, W: io::Write> HighlightedWriter<'a, R, W> {
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
    use crate::rendering::AnsiStyledOutput;

    #[cfg(all(feature = "default-syntaxes", feature = "default-themes"))]
    #[test]
    fn theme_scope_renderer_produces_output() {
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let syntax = ss.find_syntax_by_extension("rs").unwrap();

        let mut w = HighlightedWriter::from_themed(
            syntax,
            &ss,
            &ts.themes["base16-ocean.dark"],
            AnsiStyledOutput::new(false),
        )
        .build();
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

        let mut w = HighlightedWriter::from_themed(
            syntax,
            &ss,
            &ts.themes["base16-ocean.dark"],
            AnsiStyledOutput::new(false),
        )
        .build();
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

        let mut w = HighlightedWriter::from_themed(
            syntax,
            &ss,
            &ts.themes["base16-ocean.dark"],
            AnsiStyledOutput::new(false),
        )
        .build();
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

        let mut w = HighlightedWriter::from_themed(
            syntax,
            &ss,
            &ts.themes["base16-ocean.dark"],
            AnsiStyledOutput::new(false),
        )
        .build();
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
        let mut w = HighlightedWriter::from_themed(
            ss.find_syntax_by_extension("py").unwrap(),
            &ss,
            theme,
            AnsiStyledOutput::new(false),
        )
        .build();

        let lines = ["\"\"\"\n", "def foo():\n", "\"\"\"\n"];
        w.write_all(lines[0].as_bytes()).unwrap();

        let (parse_state, scope_stack) = w.state();
        let (parse_state, scope_stack) = (parse_state.clone(), scope_stack.clone());
        let first_output = String::from_utf8(w.finalize().unwrap()).unwrap();

        let mut other = HighlightedWriter::from_themed(
            ss.find_syntax_by_extension("py").unwrap(),
            &ss,
            theme,
            AnsiStyledOutput::new(false),
        )
        .with_state(parse_state, scope_stack)
        .build();
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
