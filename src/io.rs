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
/// internal buffer until either another `\n` arrives or end-of-input cleanup
/// runs. End-of-input cleanup runs in two ways:
///
/// - **Implicitly**, in [`Drop`], on a best-effort basis when the writer
///   falls out of scope. Errors are silently swallowed. This is the natural
///   choice when you don't need the inner sink back and don't care about
///   error reporting.
/// - **Explicitly**, via [`into_inner`], which consumes the writer and
///   returns the inner sink (`io::Result<W>`) so you can both inspect any
///   errors and recover the bytes (the default `Vec<u8>` sink is the common
///   case).
///
/// [`from_themed`]: HighlightedWriter::from_themed
/// [`from_markup`]: HighlightedWriter::from_markup
/// [`from_renderer`]: HighlightedWriter::from_renderer
/// [`Builder::with_output`]: HighlightedWriterBuilder::with_output
/// [`Builder::with_state`]: HighlightedWriterBuilder::with_state
/// [`Builder::build`]: HighlightedWriterBuilder::build
/// [`into_inner`]: HighlightedWriter::into_inner
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
/// w.into_inner().unwrap();
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
/// let output = String::from_utf8(w.into_inner().unwrap()).unwrap();
/// assert!(output.contains("\x1b[38;2;"));
/// ```
pub struct HighlightedWriter<
    'a,
    R: ScopeRenderer = ThemedRenderer<'a, AnsiStyledOutput>,
    W: io::Write = Vec<u8>,
> {
    syntax_set: &'a SyntaxSet,
    open_scopes: isize,
    parse_state: ParseState,
    scope_stack: ScopeStack,
    // Wrapped in `Option` so that `into_inner` can take ownership of the
    // sink via `&mut self`, and so that `Drop` can detect whether `into_inner`
    // has already been called and skip the cleanup if so.
    output: Option<W>,
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
                output: Some(output),
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
                    output: Some(output),
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

    /// Parse one complete line and forward the rendered output to the sink.
    ///
    /// This is the inner per-line entry point used by the [`Write`]
    /// implementation once a complete `\n`-terminated line has been
    /// assembled in the internal buffer.
    fn highlight_line(&mut self, line: &str) -> Result<(), Error> {
        let parse_output = self.parse_state.parse_line(line, self.syntax_set)?;

        // If replayed ops arrived, patch the pending buffer in place. The
        // parser invariant guarantees `replayed.len() <= pending_ops.len()`,
        // and `iter_mut().zip(...)` short-circuits at the shorter side, so
        // no defensive bound check is needed.
        if !parse_output.replayed.is_empty() {
            for (slot, ops) in self
                .pending_ops
                .iter_mut()
                .zip(parse_output.replayed.into_iter())
            {
                *slot = ops;
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
            self.output
                .as_mut()
                .expect("output already taken")
                .write_all(formatted.as_bytes())?;
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
            self.output
                .as_mut()
                .expect("output already taken")
                .write_all(formatted.as_bytes())?;
        }

        self.scope_stack = scope_stack;
        self.open_scopes = open_scopes;
        self.pending_lines.clear();
        self.pending_ops.clear();
        Ok(())
    }

    /// Run end-of-input cleanup and return the inner sink.
    ///
    /// This is the explicit, error-propagating counterpart to the implicit
    /// best-effort cleanup that runs in [`Drop`]. Call this when you need:
    ///
    /// - the inner sink back (the default `Vec<u8>` sink is the common case),
    /// - error handling for any failures during the trailing-line drain,
    ///   the speculative-line flush, or the close-scopes emission.
    ///
    /// Otherwise, you can let the writer fall out of scope and `Drop` will
    /// run the same cleanup on a best-effort basis (errors are silently
    /// swallowed).
    pub fn into_inner(mut self) -> io::Result<W> {
        self.finalize_inner()
    }

    /// Internal cleanup pathway shared by [`into_inner`] and `Drop::drop`.
    ///
    /// Drains the trailing partial line, flushes any pending speculatively
    /// buffered lines, emits close markers for scopes still open at EOF,
    /// then `take`s the inner sink and returns it. After this returns,
    /// `self.output` is `None`.
    ///
    /// [`into_inner`]: HighlightedWriter::into_inner
    fn finalize_inner(&mut self) -> io::Result<W> {
        // 1. Drain a trailing partial line, if any.
        if !self.line_buf.is_empty() {
            let line_bytes = std::mem::take(&mut self.line_buf);
            let line = std::str::from_utf8(&line_bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            self.highlight_line(line).map_err(io::Error::other)?;
        }

        // 2. Flush any pending speculatively-buffered lines.
        self.flush_pending().map_err(io::Error::other)?;

        // 3. Emit close markers for any scopes still open at EOF.
        let mut buf = String::new();
        for _ in 0..self.open_scopes {
            self.renderer.end_scope(&mut buf);
        }
        self.open_scopes = 0;
        self.output
            .as_mut()
            .expect("output already taken")
            .write_all(buf.as_bytes())?;

        // 4. Take the sink out, leaving the field as `None`.
        Ok(self.output.take().expect("output already taken"))
    }
}

impl<'a, R: ScopeRenderer, W: io::Write> Drop for HighlightedWriter<'a, R, W> {
    fn drop(&mut self) {
        // Best-effort cleanup. If `into_inner` has already been called,
        // `self.output` is `None` and there's nothing to do.
        if self.output.is_some() {
            let _ = self.finalize_inner();
        }
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
                self.highlight_line(line).map_err(io::Error::other)?;
            }
        }
        Ok(buf.len())
    }

    /// Forwards to the inner sink. Partial trailing lines remain buffered;
    /// they are not highlightable until terminated by `\n` or by
    /// [`into_inner`](HighlightedWriter::into_inner) (or implicitly by `Drop`).
    fn flush(&mut self) -> io::Result<()> {
        self.output.as_mut().expect("output already taken").flush()
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
        let output = String::from_utf8(w.into_inner().unwrap()).unwrap();
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
        let output = String::from_utf8(w.into_inner().unwrap()).unwrap();

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
        let output = String::from_utf8(w.into_inner().unwrap()).unwrap();
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
        let output = String::from_utf8(w.into_inner().unwrap()).unwrap();
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
        let first_output = String::from_utf8(w.into_inner().unwrap()).unwrap();

        let mut other = HighlightedWriter::from_themed(
            ss.find_syntax_by_extension("py").unwrap(),
            &ss,
            theme,
            AnsiStyledOutput::new(false),
        )
        .with_state(parse_state, scope_stack)
        .build();
        other.write_all(lines[1].as_bytes()).unwrap();
        let second_output = String::from_utf8(other.into_inner().unwrap()).unwrap();

        // The second line should be highlighted as a docstring (same style as
        // the first line's triple-quote) because the parse state carries the
        // string context forward.
        assert!(!second_output.is_empty());
        let extract_fg =
            |s: &str| -> Option<String> { s.find("\x1b[38;2;").map(|i| s[i..i + 16].to_string()) };
        assert_eq!(extract_fg(&first_output), extract_fg(&second_output));
    }

    #[cfg(all(feature = "default-syntaxes", feature = "default-themes"))]
    #[test]
    fn drop_runs_cleanup_when_into_inner_is_skipped() {
        // Verify that letting a `HighlightedWriter` fall out of scope without
        // calling `into_inner` still drains a trailing partial line and emits
        // close markers for any open scopes — i.e. that `Drop` runs the same
        // cleanup as `into_inner`. We compare the bytes a borrowed sink ends
        // up with against the bytes the same input produces under explicit
        // `into_inner`.
        let ss = SyntaxSet::load_defaults_newlines();
        let syntax = ss.find_syntax_by_extension("rs").unwrap();

        // The input deliberately ends WITHOUT a trailing newline, so the
        // last token sits in `line_buf` until cleanup. The classed-HTML
        // markup renderer also leaves the outer `<span class="source rust">`
        // open at end-of-input, exercising the close-scopes path.
        let input = b"fn main()";

        // 1. Reference: explicit `into_inner`.
        let mut explicit = HighlightedWriter::from_markup(
            syntax,
            &ss,
            crate::html::ClassedHTMLScopeRenderer::new(crate::html::ClassStyle::Spaced),
        )
        .build();
        explicit.write_all(input).unwrap();
        let explicit_bytes = explicit.into_inner().unwrap();

        // 2. Drop-only: borrow a sink, write input, let the writer fall out
        // of scope without calling `into_inner`.
        let mut implicit_buf: Vec<u8> = Vec::new();
        {
            let mut implicit = HighlightedWriter::from_markup(
                syntax,
                &ss,
                crate::html::ClassedHTMLScopeRenderer::new(crate::html::ClassStyle::Spaced),
            )
            .with_output(&mut implicit_buf)
            .build();
            implicit.write_all(input).unwrap();
            // No `into_inner` here. `implicit` drops at the end of this
            // scope and `Drop` runs cleanup against the borrowed sink.
        }

        assert_eq!(
            explicit_bytes, implicit_buf,
            "Drop should produce identical bytes to an explicit into_inner"
        );
        // Sanity-check that cleanup actually happened: the markup output
        // should contain the closing tag for the outer source scope.
        let s = String::from_utf8(implicit_buf).unwrap();
        assert!(
            s.ends_with("</span>"),
            "expected outer </span> to be emitted by cleanup, got: {s}"
        );
    }

    // ── Phase B mutation-killing tests ────────────────────────────────────

    /// Capturing `ScopeMarkup` that records the exact sequence of method
    /// invocations. Used by tests below to assert on `begin_line`/`end_line`
    /// pairing, monotonic line indices, and atom-string forwarding.
    ///
    /// Events are stored behind an `Rc<RefCell<…>>` so the test can hold its
    /// own handle to them, drop the writer (triggering the implicit Drop
    /// cleanup which itself emits more events), and then read the final
    /// event log without depending on the writer being alive.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum MarkupEvent {
        BeginLine(usize),
        EndLine(usize),
        BeginScope(Vec<String>),
        EndScope,
        Text(String),
    }

    type SharedEvents = std::rc::Rc<std::cell::RefCell<Vec<MarkupEvent>>>;

    struct CapturingMarkup {
        events: SharedEvents,
    }

    impl CapturingMarkup {
        fn new() -> (Self, SharedEvents) {
            let events: SharedEvents = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            (
                Self {
                    events: events.clone(),
                },
                events,
            )
        }
    }

    impl ScopeMarkup for CapturingMarkup {
        fn begin_line(&mut self, line_index: usize, _output: &mut String) {
            self.events
                .borrow_mut()
                .push(MarkupEvent::BeginLine(line_index));
        }
        fn end_line(&mut self, line_index: usize, _output: &mut String) {
            self.events
                .borrow_mut()
                .push(MarkupEvent::EndLine(line_index));
        }
        fn begin_scope(&mut self, atom_strs: &[&str], _output: &mut String) {
            self.events.borrow_mut().push(MarkupEvent::BeginScope(
                atom_strs.iter().map(|s| (*s).to_string()).collect(),
            ));
        }
        fn end_scope(&mut self, _output: &mut String) {
            self.events.borrow_mut().push(MarkupEvent::EndScope);
        }
        fn write_text(&mut self, text: &str, _output: &mut String) {
            self.events
                .borrow_mut()
                .push(MarkupEvent::Text(text.to_string()));
        }
    }

    #[cfg(feature = "default-syntaxes")]
    #[test]
    fn markup_receives_paired_line_hooks_with_monotonic_line_index() {
        // Three lines of Rust → exactly three begin_line/end_line pairs with
        // line indices 0, 1, 2 in order. Catches mutants that drop the calls,
        // emit a wrong line index, mutate `+= 1` on `line_index`, or replace
        // `line_index_offset + i` arithmetic in `flush_pending`.
        let ss = SyntaxSet::load_defaults_newlines();
        let syntax = ss.find_syntax_by_extension("rs").unwrap();

        let (capture, events) = CapturingMarkup::new();
        let mut w = HighlightedWriter::from_markup(syntax, &ss, capture).build();
        w.write_all(b"fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
        // Drop the writer first so any cleanup-time events are recorded
        // BEFORE we read them.
        drop(w);
        let events = events.borrow().clone();

        let line_starts: Vec<usize> = events
            .iter()
            .filter_map(|e| match e {
                MarkupEvent::BeginLine(i) => Some(*i),
                _ => None,
            })
            .collect();
        let line_ends: Vec<usize> = events
            .iter()
            .filter_map(|e| match e {
                MarkupEvent::EndLine(i) => Some(*i),
                _ => None,
            })
            .collect();
        assert_eq!(
            line_starts,
            vec![0, 1, 2],
            "begin_line must fire once per line with monotonically increasing 0-based indices"
        );
        assert_eq!(
            line_ends,
            vec![0, 1, 2],
            "end_line must fire once per line with the same indices as begin_line"
        );

        // begin_line(i) must precede the matching end_line(i) and the
        // pairing must alternate (no two starts in a row, no two ends in a
        // row, no end before its start).
        let mut open: Option<usize> = None;
        for ev in &events {
            match ev {
                MarkupEvent::BeginLine(i) => {
                    assert!(open.is_none(), "nested begin_line for {i}: prev still open");
                    open = Some(*i);
                }
                MarkupEvent::EndLine(i) => {
                    assert_eq!(open, Some(*i), "end_line({i}) without matching begin_line");
                    open = None;
                }
                _ => {}
            }
        }
        assert!(open.is_none(), "trailing begin_line with no end_line");
    }

    #[cfg(feature = "default-syntaxes")]
    #[test]
    fn markup_forwards_atom_strs_and_pairs_scopes() {
        // Catches: MarkupAdapter::begin_scope dropping atom_strs derivation,
        // MarkupAdapter::write_text being replaced with (), and
        // resolve_atom_strs returning vec![] / vec![""] / vec!["xyzzy"].
        let ss = SyntaxSet::load_defaults_newlines();
        let syntax = ss.find_syntax_by_name("R").unwrap();

        let (capture, events) = CapturingMarkup::new();
        let mut w = HighlightedWriter::from_markup(syntax, &ss, capture).build();
        w.write_all(b"x + y\n").unwrap();
        drop(w);
        let events = events.borrow().clone();

        // The first scope must be ["source", "r"].
        let first_scope = events
            .iter()
            .find_map(|e| match e {
                MarkupEvent::BeginScope(atoms) => Some(atoms.clone()),
                _ => None,
            })
            .expect("expected at least one begin_scope event");
        assert_eq!(first_scope, vec!["source".to_string(), "r".to_string()]);

        // begin_scope and end_scope must be paired (count equal).
        let begins = events
            .iter()
            .filter(|e| matches!(e, MarkupEvent::BeginScope(_)))
            .count();
        let ends = events
            .iter()
            .filter(|e| matches!(e, MarkupEvent::EndScope))
            .count();
        assert_eq!(begins, ends, "begin_scope/end_scope must be paired");
        assert!(begins > 0, "expected at least one scope to be opened");

        // The literal text "x" must be passed verbatim to write_text.
        // (Catches MarkupAdapter::write_text being replaced with ().)
        let text_concat: String = events
            .iter()
            .filter_map(|e| match e {
                MarkupEvent::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            text_concat.contains('x') && text_concat.contains('y'),
            "expected literal x and y to flow through write_text, got: {text_concat:?}"
        );
    }

    #[test]
    fn ansi_output_is_byte_exact_for_known_input() {
        // Use a hand-built one-syntax SyntaxSet and a tiny synthetic theme
        // so the test isn't sensitive to bundled-syntax/theme drift. Pin
        // the exact ANSI byte sequence emitted for a known token. This
        // locks down: that the foreground escape is emitted at all (a
        // silently-empty `begin_style` would produce just the literal
        // text), that the RGB channels appear in the right order with
        // the right separators, and that the post-token newline
        // splitting in the streaming layer advances past the newline
        // rather than getting stuck on it.
        use crate::highlighting::{
            Color, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
        };
        use crate::parsing::{SyntaxDefinition, SyntaxSetBuilder};
        use std::str::FromStr;

        let syntax_str = r#"
name: PlainPing
scope: source.ping
contexts:
  main:
    - match: 'ping'
      scope: kw.ping
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax);
        let ss = builder.build();
        let syntax_ref = &ss.syntaxes()[0];

        let theme = Theme {
            name: Some("ansi-test".into()),
            author: None,
            settings: ThemeSettings {
                foreground: Some(Color {
                    r: 200,
                    g: 100,
                    b: 50,
                    a: 0xff,
                }),
                background: Some(Color {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 0xff,
                }),
                ..Default::default()
            },
            scopes: vec![ThemeItem {
                scope: ScopeSelectors::from_str("kw").unwrap(),
                style: StyleModifier {
                    foreground: Some(Color {
                        r: 10,
                        g: 20,
                        b: 30,
                        a: 0xff,
                    }),
                    background: None,
                    font_style: Some(FontStyle::empty()),
                },
            }],
        };

        let mut w =
            HighlightedWriter::from_themed(syntax_ref, &ss, &theme, AnsiStyledOutput::new(false))
                .build();
        w.write_all(b"ping\n").unwrap();
        let output = String::from_utf8(w.into_inner().unwrap()).unwrap();

        // Exactly: open span with kw style, "ping", close span (no-op for
        // ANSI), then "\n".
        assert_eq!(output, "\x1b[38;2;10;20;30mping\n");
    }

    #[test]
    fn ansi_output_with_background_emits_both_escapes() {
        // Catches AnsiStyledOutput::begin_style with `include_bg = true` being
        // mutated (e.g. swapping the order, dropping one escape, swapping
        // 38/48 codes, channel swaps).
        use crate::highlighting::{
            Color, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
        };
        use crate::parsing::{SyntaxDefinition, SyntaxSetBuilder};
        use std::str::FromStr;

        let syntax_str = r#"
name: PlainPing
scope: source.ping
contexts:
  main:
    - match: 'ping'
      scope: kw.ping
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax);
        let ss = builder.build();
        let syntax_ref = &ss.syntaxes()[0];

        let theme = Theme {
            name: Some("ansi-bg-test".into()),
            author: None,
            settings: ThemeSettings::default(),
            scopes: vec![ThemeItem {
                scope: ScopeSelectors::from_str("kw").unwrap(),
                style: StyleModifier {
                    foreground: Some(Color {
                        r: 11,
                        g: 22,
                        b: 33,
                        a: 0xff,
                    }),
                    background: Some(Color {
                        r: 44,
                        g: 55,
                        b: 66,
                        a: 0xff,
                    }),
                    font_style: Some(FontStyle::empty()),
                },
            }],
        };

        let mut w =
            HighlightedWriter::from_themed(syntax_ref, &ss, &theme, AnsiStyledOutput::new(true))
                .build();
        w.write_all(b"ping\n").unwrap();
        let output = String::from_utf8(w.into_inner().unwrap()).unwrap();

        // The background escape (48) must come first, followed by foreground
        // (38), then the literal text, then the trailing newline. The kw
        // style ought to apply to "ping" precisely.
        assert!(
            output.contains("\x1b[48;2;44;55;66m\x1b[38;2;11;22;33mping"),
            "expected bg+fg pair around 'ping', got {output:?}"
        );
        assert!(output.ends_with('\n'));
    }

    #[cfg(feature = "default-syntaxes")]
    #[test]
    fn line_index_is_threaded_correctly_across_branch_point_replay() {
        // The hardest-to-reach gap: cross-line branch-point speculation.
        // Build a custom syntax that triggers `fail: bp` on line 2, forcing
        // the parser to retroactively replay line 1 under the fallback
        // alternative. The streaming writer must:
        //   1. Buffer line 1's output until speculation resolves.
        //   2. Render line 1 once, with the corrected ops, with line_index=0.
        //   3. Render line 2 with line_index=1.
        //   4. Emit no `try.word` scope (the wrong-alternative result).
        //   5. Pass `begin_line(0)` BEFORE `begin_line(1)` to the renderer.
        //
        // Catches mutants in highlight_line, flush_pending, line_index_offset
        // arithmetic, the speculative buffering branch, and the replay patch.
        use crate::parsing::{SyntaxDefinition, SyntaxSetBuilder};

        let syntax_str = r#"
name: CrossLineBranchTest
scope: source.clbt
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
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax);
        let ss = builder.build();
        let syntax_ref = &ss.syntaxes()[0];

        let (capture, events) = CapturingMarkup::new();
        let mut w = HighlightedWriter::from_markup(syntax_ref, &ss, capture).build();
        // Line 1 triggers branch_point. Line 2 triggers fail: bp, forcing
        // retroactive replay of line 1 under fallback-ctx.
        w.write_all(b"TRY\nFAIL\n").unwrap();
        drop(w);
        let events = events.borrow().clone();

        // 1. begin_line indices must be 0 then 1, in order.
        let line_starts: Vec<usize> = events
            .iter()
            .filter_map(|e| match e {
                MarkupEvent::BeginLine(i) => Some(*i),
                _ => None,
            })
            .collect();
        assert_eq!(
            line_starts,
            vec![0, 1],
            "begin_line should fire for both lines after speculation flush, in order"
        );

        // 2. The wrong-alternative scope `try.word` must NOT appear.
        let saw_try_word = events.iter().any(|e| match e {
            MarkupEvent::BeginScope(atoms) => atoms.iter().any(|a| a == "try"),
            _ => false,
        });
        assert!(
            !saw_try_word,
            "the speculation-failed `try.*` scope must not surface in rendered output"
        );

        // 3. The corrected `fallback.content` scope MUST appear.
        let saw_fallback = events.iter().any(|e| match e {
            MarkupEvent::BeginScope(atoms) => atoms.iter().any(|a| a == "fallback"),
            _ => false,
        });
        assert!(
            saw_fallback,
            "expected the post-replay `fallback.content` scope in rendered output"
        );

        // 4. begin_line(0) must precede begin_line(1).
        let pos0 = events
            .iter()
            .position(|e| matches!(e, MarkupEvent::BeginLine(0)))
            .expect("begin_line(0) missing");
        let pos1 = events
            .iter()
            .position(|e| matches!(e, MarkupEvent::BeginLine(1)))
            .expect("begin_line(1) missing");
        assert!(pos0 < pos1, "line 0 events must precede line 1 events");
    }

    #[cfg(feature = "default-syntaxes")]
    #[test]
    fn drop_flushes_pending_speculation_buffer() {
        // Variant of `drop_runs_cleanup_when_into_inner_is_skipped` that also
        // exercises the speculative-line flush path inside `finalize_inner`.
        // Construct a writer over a syntax with cross-line branch-point
        // failures, feed it input that leaves `pending_lines` non-empty (the
        // branch is still active), drop the writer without `into_inner`, and
        // assert that the borrowed sink contains both rendered lines.
        use crate::parsing::{SyntaxDefinition, SyntaxSetBuilder};

        let syntax_str = r#"
name: PendingDropTest
scope: source.pdt
contexts:
  main:
    - match: 'OPEN'
      branch_point: bp
      branch: [try-ctx, fallback-ctx]
    - match: '.*'
      scope: main.other
  try-ctx:
    - match: '\n'
    - match: '\w+'
      scope: try.word
      pop: true
  fallback-ctx:
    - match: '.*'
      scope: fallback.content
      pop: true
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax);
        let ss = builder.build();
        let syntax_ref = &ss.syntaxes()[0];

        // Reference output: explicit `into_inner` after writing input that
        // *does* trigger speculation but never resolves it before EOF.
        let mut explicit = HighlightedWriter::from_markup(
            syntax_ref,
            &ss,
            crate::html::ClassedHTMLScopeRenderer::new(crate::html::ClassStyle::Spaced),
        )
        .build();
        explicit.write_all(b"OPEN\n").unwrap();
        let explicit_bytes = explicit.into_inner().unwrap();

        // Drop-only path: borrowed sink, identical input, no into_inner call.
        let mut implicit_buf: Vec<u8> = Vec::new();
        {
            let mut implicit = HighlightedWriter::from_markup(
                syntax_ref,
                &ss,
                crate::html::ClassedHTMLScopeRenderer::new(crate::html::ClassStyle::Spaced),
            )
            .with_output(&mut implicit_buf)
            .build();
            implicit.write_all(b"OPEN\n").unwrap();
        }

        assert_eq!(
            explicit_bytes, implicit_buf,
            "Drop must flush pending speculative lines just like into_inner"
        );
        // Sanity: the buffer must not be empty (catches Drop replaced with ()
        // and finalize_inner replaced with `Ok(Default::default())`).
        assert!(
            !implicit_buf.is_empty(),
            "Drop cleanup produced no output; pending-line flush must have been skipped"
        );
        let s = String::from_utf8(implicit_buf).unwrap();
        assert!(
            s.contains("OPEN"),
            "expected the speculative line content to surface after Drop, got: {s}"
        );
        // Absolute span balance: every opened classed span must be balanced
        // by a closing tag in the FLUSHED output. This is the assertion that
        // catches `open_scopes += delta → -=/*=` in flush_pending — both the
        // explicit and implicit paths route through `flush_pending`, so a
        // byte-equality check would treat the mutated count as a no-op (both
        // sides produce the same wrong output). Tag balance is an absolute
        // invariant, mutation-blind.
        let opens = s.matches("<span class=\"").count();
        let closes = s.matches("</span>").count();
        assert_eq!(
            opens, closes,
            "post-flush span balance must hold (opens={opens}, closes={closes}); the cleanup-tag accumulator inside the speculative-flush path is likely wrong: {s}"
        );
    }

    #[cfg(feature = "default-syntaxes")]
    #[test]
    fn empty_scope_optimisation_truncates_push_pop_with_no_text() {
        // The empty-scope optimisation: when the parser pushes a scope and
        // immediately pops it at the same byte position with no intervening
        // text, the renderer must NOT emit a corresponding markup pair.
        // The optimisation hinges on tracking when a scope is "still
        // empty" between its push and its matching pop, and on driving
        // the open-scope count consistently in both directions; any of
        // those bookkeeping invariants going wrong would surface here as
        // empty span pairs leaking into the output.
        //
        // We use the high-level HTML generator rather than a capturing
        // markup so that we observe the *truncated* output buffer
        // directly: the optimisation happens at the rendered-string
        // level, and only the bytes that survive truncation reach the
        // inner sink.
        //
        // Empty-scope cases are triggered when a Push and a matching Pop
        // occur at the same byte position with no intervening text. The
        // bundled Rust syntax exercises this naturally for a function
        // with an empty body — there are several adjacent push/pop pairs
        // around the brace and parenthesis tokens.
        let ss = SyntaxSet::load_defaults_newlines();
        let syntax = ss.find_syntax_by_extension("rs").unwrap();
        let mut g = crate::html::ClassedHTMLGenerator::new_with_class_style(
            syntax,
            &ss,
            crate::html::ClassStyle::Spaced,
        );
        g.parse_html_for_line_which_includes_newline("fn f() {}\n")
            .unwrap();
        let html = g.finalize();

        // No empty `<span class="…"></span>` pair should appear in the
        // output (where … contains no whitespace, indicating an
        // immediately-closed pushed scope). Catches mutants that flip the
        // truncation logic in `render_line`'s `scope_empty` tracking and
        // would cause empty span pairs to leak into the output.
        //
        // The regex check is intentionally conservative: any pair of the
        // form `<span class="WORDCHARS">.</span>` would be a leak, where
        // `.` matches any single non-newline character. We use a simple
        // string contains check on the simplest possible empty pattern
        // because Rust's syntax highlighting doesn't produce empty class
        // names; if the optimisation is broken, the parser's empty scope
        // pushes/pops would surface as `<span class="X"></span>` for some
        // class `X`.
        let mut found_empty = false;
        let mut idx = 0;
        while let Some(open_start) = html[idx..].find("<span class=\"") {
            let abs_start = idx + open_start;
            let after_open_tag = abs_start + "<span class=\"".len();
            // Find the closing `">` of the open tag.
            if let Some(rel_close) = html[after_open_tag..].find("\">") {
                let content_start = after_open_tag + rel_close + 2;
                // Empty if the very next characters are "</span>".
                if html[content_start..].starts_with("</span>") {
                    found_empty = true;
                    break;
                }
                idx = content_start;
            } else {
                break;
            }
        }
        assert!(
            !found_empty,
            "empty-scope optimisation must prevent <span class=\"…\"></span> pairs from leaking, got HTML:\n{html}"
        );

        // Sanity: the output must still contain *some* nontrivial spans
        // (otherwise we'd be passing trivially because nothing was
        // emitted at all).
        assert!(
            html.contains("<span class=\""),
            "expected at least one classed span in the output, got: {html}"
        );
    }

    #[test]
    fn themed_renderer_closes_styled_span_on_trailing_partial_line() {
        // For a line that ends with a newline, the streaming layer
        // already closes the active styled span when it splits on the
        // newline character — so the end-of-line hook on the themed
        // renderer has nothing to do in that case. The hook only does
        // useful work for a *trailing partial line* (input without a
        // closing newline), which flows through end-of-input cleanup.
        // If the hook were silently a no-op, the styled span around the
        // partial line would never get its closing tag and the output
        // would be unbalanced.
        //
        // We feed input without a trailing newline and assert that the
        // output is span-balanced and ends with the closing tag. We use
        // an inline-styled HTML emitter rather than ANSI because ANSI's
        // close-style is intentionally a no-op and would not surface
        // the bug.
        use crate::highlighting::{
            Color, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
        };
        use crate::parsing::{SyntaxDefinition, SyntaxSetBuilder};
        use std::str::FromStr;

        let syntax_str = r#"
name: PingNoop
scope: source.ping
contexts:
  main:
    - match: 'ping'
      scope: kw.ping
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax);
        let ss = builder.build();
        let syntax_ref = &ss.syntaxes()[0];

        let bg = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 0xff,
        };
        let theme = Theme {
            name: Some("end-line-test".into()),
            author: None,
            settings: ThemeSettings {
                foreground: Some(Color {
                    r: 200,
                    g: 200,
                    b: 200,
                    a: 0xff,
                }),
                background: Some(bg),
                ..Default::default()
            },
            scopes: vec![ThemeItem {
                scope: ScopeSelectors::from_str("kw").unwrap(),
                style: StyleModifier {
                    foreground: Some(Color {
                        r: 11,
                        g: 22,
                        b: 33,
                        a: 0xff,
                    }),
                    background: None,
                    font_style: Some(FontStyle::empty()),
                },
            }],
        };

        let mut w = HighlightedWriter::from_themed(
            syntax_ref,
            &ss,
            &theme,
            crate::html::HtmlStyledOutput::new(bg),
        )
        .build();
        // No trailing newline → drained via finalize_inner → ThemedRenderer
        // ::end_line is the only path that can close the open span.
        w.write_all(b"ping").unwrap();
        let bytes = w.into_inner().unwrap();
        let html = String::from_utf8(bytes).unwrap();

        // The styled span around "ping" must be closed at end-of-input.
        assert!(
            html.contains("<span style=\""),
            "expected an opening styled span around 'ping', got: {html}"
        );
        assert_eq!(
            html.matches("<span").count(),
            html.matches("</span>").count(),
            "every <span ...> must be balanced by a </span>; missing close means end_line was skipped: {html}"
        );
        assert!(
            html.ends_with("</span>"),
            "expected output to end with the trailing </span>, got: {html:?}"
        );
    }

    /// `io::Write` sink that records every call. Used to assert that
    /// `HighlightedWriter::flush` actually delegates to the inner sink and
    /// that bytes propagate.
    struct CountingSink {
        inner: Vec<u8>,
        flush_count: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl io::Write for CountingSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flush_count.set(self.flush_count.get() + 1);
            Ok(())
        }
    }

    #[test]
    fn flush_delegates_to_inner_sink() {
        // The streaming writer's `flush` must call through to the inner
        // sink's `flush`, not silently succeed. A no-op `flush` would
        // leave buffered downstream sinks (e.g. a `BufWriter` wrapped
        // around a file) holding bytes after the caller had asked for
        // them to be made durable.
        use crate::parsing::{SyntaxDefinition, SyntaxSetBuilder};

        let syntax_str = r#"
name: PingNoop
scope: source.ping
contexts:
  main:
    - match: 'ping'
      scope: kw.ping
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax);
        let ss = builder.build();
        let syntax_ref = &ss.syntaxes()[0];

        let flush_count = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let sink = CountingSink {
            inner: Vec::new(),
            flush_count: flush_count.clone(),
        };
        let (capture, _events) = CapturingMarkup::new();
        let mut w = HighlightedWriter::from_markup(syntax_ref, &ss, capture)
            .with_output(sink)
            .build();
        w.write_all(b"ping\n").unwrap();
        assert_eq!(
            flush_count.get(),
            0,
            "flush should not be called by write_all"
        );

        w.flush().unwrap();
        assert_eq!(
            flush_count.get(),
            1,
            "HighlightedWriter::flush must delegate to the inner sink exactly once"
        );

        w.flush().unwrap();
        assert_eq!(flush_count.get(), 2);
    }

    #[cfg(feature = "default-syntaxes")]
    #[test]
    fn drop_emits_exact_count_of_close_tags_for_open_scopes() {
        // End-of-input cleanup must emit exactly as many close-scope
        // markers as there are scopes still open at EOF — no more, no
        // fewer. The accumulator that tracks "scopes still open" runs
        // through both the fast path and the speculative-flush path; a
        // sign or factor mutation on the accumulator would either leak
        // unclosed tags or emit phantom closes. The expected closing-tag
        // count is pinned absolutely (against the markup tag balance
        // invariant), not relative to another path that shares the same
        // bug.
        let ss = SyntaxSet::load_defaults_newlines();
        let syntax = ss.find_syntax_by_extension("rs").unwrap();

        let mut w = HighlightedWriter::from_markup(
            syntax,
            &ss,
            crate::html::ClassedHTMLScopeRenderer::new(crate::html::ClassStyle::Spaced),
        )
        .build();
        // Trailing newline so the line is fully highlighted; the only scope
        // still open at EOF is the outer source.rust meta-scope.
        w.write_all(b"fn main() {}\n").unwrap();
        let bytes = w.into_inner().unwrap();
        let html = String::from_utf8(bytes).unwrap();

        let opens = html.matches("<span class=\"").count();
        let closes = html.matches("</span>").count();
        assert!(opens > 0, "expected at least one opening span, got: {html}");
        assert_eq!(
            opens, closes,
            "every opened classed span must be balanced by a closing tag (opens={opens}, closes={closes}); cleanup tag count likely wrong"
        );

        // The output must end with </span>\n (the outer source.rust meta-scope
        // closing tag plus the trailing newline that ended the input line).
        // This is the most direct test of the cleanup-emit-end_scope path.
        assert!(
            html.ends_with("</span>\n") || html.ends_with("</span>"),
            "expected output to end with the outermost </span>, got: {html:?}"
        );
    }

    #[test]
    fn write_returns_full_buffer_length_for_nonempty_input() {
        // Pin the `io::Write` contract: `write` must return the number
        // of bytes consumed from the input buffer, which always equals
        // the input length for the streaming highlighter (it never
        // partially-accepts a write). An empty input must short-circuit
        // and return 0 without engaging the line-buffer machinery — a
        // bug there would either return a constant length, or
        // accidentally re-process the empty buffer as if a complete
        // line had arrived.
        use crate::parsing::{SyntaxDefinition, SyntaxSetBuilder};

        let syntax_str = r#"
name: PingNoop
scope: source.ping
contexts:
  main:
    - match: 'ping'
      scope: kw.ping
"#;
        let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax);
        let ss = builder.build();
        let syntax_ref = &ss.syntaxes()[0];

        let (capture, _events) = CapturingMarkup::new();
        let mut w = HighlightedWriter::from_markup(syntax_ref, &ss, capture).build();
        let buf = b"ping\n";
        let n = w.write(buf).unwrap();
        assert_eq!(n, buf.len());

        // Empty buffer must return 0 (and NOT accidentally process anything).
        let n_empty = w.write(b"").unwrap();
        assert_eq!(n_empty, 0);
    }
}
