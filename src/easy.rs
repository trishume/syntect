//! Core highlighting API: scope-based rendering, theme-aware highlighting,
//! and convenience wrappers for common use cases.
//!
//! The central type is [`HighlightLines`], which drives syntax parsing and
//! delegates rendering to a pluggable [`ScopeRenderer`]. It handles
//! branch-point backtracking transparently by buffering output during
//! speculative parsing.
//!
//! For theme-based highlighting (resolving scopes to colors),
//! [`ThemeScopeRenderer`] bridges the gap between scope-based rendering
//! and the [`Highlighter`](crate::highlighting::Highlighter).
//!
//! For HTML output with CSS classes, see [`crate::html::ClassedHTMLGenerator`]
//! and [`crate::html::HTMLScopeRenderer`].

use crate::highlighting::{HighlightIterator, HighlightState, Highlighter, Style, Theme};
use crate::parsing::{
    lock_global_scope_repo, BasicScopeStackOp, ParseState, Scope, ScopeRepository, ScopeStack,
    ScopeStackOp, SyntaxReference, SyntaxSet,
};
use crate::Error;
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
/// See [`crate::html::HTMLScopeRenderer`] for an HTML implementation that produces
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
/// is determined entirely by the `R` parameter.
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
/// # Example
///
/// ```
/// use syntect::easy::HighlightLines;
/// use syntect::html::HTMLScopeRenderer;
/// use syntect::html::ClassStyle;
/// use syntect::parsing::SyntaxSet;
/// use syntect::util::LinesWithEndings;
///
/// let current_code = r#"
/// x <- 5
/// y <- 6
/// x + y
/// "#;
///
/// let syntax_set = SyntaxSet::load_defaults_newlines();
/// let syntax = syntax_set.find_syntax_by_name("R").unwrap();
/// let renderer = HTMLScopeRenderer::new(ClassStyle::Spaced);
/// let mut generator = HighlightLines::new(syntax, &syntax_set, renderer);
/// for line in LinesWithEndings::from(current_code) {
///     generator.highlight_line(line);
/// }
/// let output: Vec<u8> = generator.finalize();
/// let html = String::from_utf8(output).unwrap();
/// ```
pub struct HighlightLines<'a, R: ScopeRenderer, W: io::Write = Vec<u8>> {
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

impl<'a, R: ScopeRenderer> HighlightLines<'a, R> {
    /// Create a new highlighting driver with a custom renderer.
    ///
    /// The output is collected into a `Vec<u8>` that is returned by
    /// [`finalize`]. Use [`new_with_output`] to stream output to an
    /// arbitrary [`io::Write`] sink instead.
    ///
    /// [`finalize`]: HighlightLines::finalize
    /// [`new_with_output`]: HighlightLines::new_with_output
    pub fn new(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        renderer: R,
    ) -> HighlightLines<'a, R> {
        Self::new_with_output(syntax_reference, syntax_set, renderer, Vec::new())
    }
}

impl<'a, R: ScopeRenderer, W: io::Write> HighlightLines<'a, R, W> {
    /// Create a new highlighting driver that writes to the given output sink.
    ///
    /// This allows streaming rendered output directly to a file, socket,
    /// or buffered writer without intermediate allocation.
    pub fn new_with_output(
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
        renderer: R,
        output: W,
    ) -> HighlightLines<'a, R, W> {
        HighlightLines {
            syntax_set,
            open_scopes: 0,
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
        (self.parse_state, self.scope_stack, self.renderer, self.output)
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
// ThemeScopeRenderer — theme-aware highlighting via ScopeRenderer
// ---------------------------------------------------------------------------

/// A [`ScopeRenderer`] that resolves styles from a theme via
/// [`Highlighter`](crate::highlighting::Highlighter).
///
/// The output format is determined by the `F` closure, which receives the
/// resolved [`Style`] and text for each token. This makes `ThemeScopeRenderer`
/// usable for ANSI terminal output, inline-styled HTML, or any other format.
///
/// # Example
///
/// ```
/// use syntect::easy::{HighlightLines, ThemeScopeRenderer};
/// use syntect::highlighting::ThemeSet;
/// use syntect::parsing::SyntaxSet;
/// use syntect::util::LinesWithEndings;
/// use std::fmt::Write;
///
/// let ss = SyntaxSet::load_defaults_newlines();
/// let ts = ThemeSet::load_defaults();
/// let syntax = ss.find_syntax_by_extension("rs").unwrap();
///
/// // ANSI 24-bit color output
/// let renderer = ThemeScopeRenderer::new(&ts.themes["base16-ocean.dark"], |style, text, output| {
///     let fg = style.foreground;
///     write!(output, "\x1b[38;2;{};{};{}m{}", fg.r, fg.g, fg.b, text).unwrap();
/// });
/// let mut h = HighlightLines::new(syntax, &ss, renderer);
/// for line in LinesWithEndings::from("fn main() {}\n") {
///     h.highlight_line(line).unwrap();
/// }
/// let output = String::from_utf8(h.finalize()).unwrap();
/// ```
pub struct ThemeScopeRenderer<'a, F: FnMut(Style, &str, &mut String)> {
    highlighter: Highlighter<'a>,
    style_stack: Vec<Style>,
    format_text: F,
}

impl<'a, F: FnMut(Style, &str, &mut String)> ThemeScopeRenderer<'a, F> {
    /// Create a new theme-aware renderer.
    ///
    /// The `format_text` closure is called for each text token with the
    /// resolved [`Style`] and should write the formatted output.
    pub fn new(theme: &'a Theme, format_text: F) -> Self {
        let highlighter = Highlighter::new(theme);
        let default_style = highlighter.style_for_stack(&[]);
        Self {
            highlighter,
            style_stack: vec![default_style],
            format_text,
        }
    }

    /// Returns the currently active style.
    pub fn current_style(&self) -> Style {
        self.style_stack.last().copied().unwrap_or_default()
    }
}

impl<F: FnMut(Style, &str, &mut String)> ScopeRenderer for ThemeScopeRenderer<'_, F> {
    fn begin_scope(
        &mut self,
        _atom_strs: &[&str],
        _scope: Scope,
        scope_stack: &[Scope],
        _output: &mut String,
    ) -> bool {
        let style = self.highlighter.style_for_stack(scope_stack);
        self.style_stack.push(style);
        // Return false: we don't write scope delimiters. Styling is applied
        // per-token in write_text. Note: end_scope is still called for
        // non-empty scopes due to the render_line implementation, which
        // keeps our style_stack in sync.
        false
    }

    fn end_scope(&mut self, _output: &mut String) {
        self.style_stack.pop();
    }

    fn write_text(&mut self, text: &str, output: &mut String) {
        let style = self.current_style();
        (self.format_text)(style, text, output);
    }
}

// ---------------------------------------------------------------------------
// ThemeHighlight — theme-based line-by-line highlighting
// ---------------------------------------------------------------------------

/// Theme-based highlighter that parses lines and returns styled regions.
///
/// This is the high-level API for highlighting strings with a theme. For each
/// line, it parses the syntax and resolves scopes to [`Style`] values using
/// the theme's [`Highlighter`](crate::highlighting::Highlighter).
///
/// For file highlighting, see [`HighlightFile`] which wraps this with a
/// buffered file reader.
///
/// # Example
///
/// ```
/// use syntect::easy::ThemeHighlight;
/// use syntect::highlighting::{ThemeSet, Style};
/// use syntect::parsing::SyntaxSet;
/// use syntect::util::LinesWithEndings;
///
/// let ss = SyntaxSet::load_defaults_newlines();
/// let ts = ThemeSet::load_defaults();
/// let syntax = ss.find_syntax_by_extension("rs").unwrap();
///
/// let mut h = ThemeHighlight::new(syntax, &ts.themes["base16-ocean.dark"]);
/// for line in LinesWithEndings::from("fn main() {}\n") {
///     let regions: Vec<(Style, &str)> = h.highlight_line(line, &ss).unwrap();
///     assert!(!regions.is_empty());
/// }
/// ```
pub struct ThemeHighlight<'a> {
    highlighter: Highlighter<'a>,
    parse_state: ParseState,
    highlight_state: HighlightState,
}

impl<'a> ThemeHighlight<'a> {
    /// Create a new theme-based highlighter for the given syntax and theme.
    pub fn new(syntax: &SyntaxReference, theme: &'a Theme) -> ThemeHighlight<'a> {
        let highlighter = Highlighter::new(theme);
        let highlight_state = HighlightState::new(&highlighter, ScopeStack::new());
        ThemeHighlight {
            highlighter,
            parse_state: ParseState::new(syntax),
            highlight_state,
        }
    }

    /// Highlights a single line, returning styled regions.
    ///
    /// Parses the line and resolves scopes to [`Style`] values using the theme.
    pub fn highlight_line<'b>(
        &mut self,
        line: &'b str,
        syntax_set: &SyntaxSet,
    ) -> Result<Vec<(Style, &'b str)>, Error> {
        let ops = self.parse_state.parse_line(line, syntax_set)?.ops;
        let iter = HighlightIterator::new(
            &mut self.highlight_state,
            &ops[..],
            line,
            &self.highlighter,
        );
        Ok(iter.collect())
    }
}

// ---------------------------------------------------------------------------
// HighlightFile — convenience for theme-based file highlighting
// ---------------------------------------------------------------------------

/// Convenience struct containing everything you need to highlight a file
/// using theme-based highlighting.
///
/// Wraps a [`ThemeHighlight`] with a buffered file reader, auto-detecting the
/// syntax from the file extension.
///
/// # Example
///
/// ```
/// use syntect::parsing::SyntaxSet;
/// use syntect::highlighting::{ThemeSet, Style};
/// use syntect::util::as_24_bit_terminal_escaped;
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
///     {
///         let regions: Vec<(Style, &str)> = highlighter.highlight_line(&line, &ss).unwrap();
///         print!("{}", as_24_bit_terminal_escaped(&regions[..], true));
///     }
///     line.clear();
/// }
/// # Ok(())
/// # }
/// ```
pub struct HighlightFile<'a> {
    /// The buffered file reader.
    pub reader: BufReader<File>,
    /// The theme-based highlighter.
    pub highlight: ThemeHighlight<'a>,
}

impl<'a> HighlightFile<'a> {
    /// Constructs a file reader and highlighter, auto-detecting the syntax
    /// from the file extension.
    pub fn new<P: AsRef<Path>>(
        path_obj: P,
        ss: &SyntaxSet,
        theme: &'a Theme,
    ) -> io::Result<HighlightFile<'a>> {
        let path: &Path = path_obj.as_ref();
        let f = File::open(path)?;
        let syntax = ss
            .find_syntax_for_file(path)?
            .unwrap_or_else(|| ss.find_syntax_plain_text());

        Ok(HighlightFile {
            reader: BufReader::new(f),
            highlight: ThemeHighlight::new(syntax, theme),
        })
    }

    /// Highlights a single line, returning styled regions.
    ///
    /// This delegates to [`ThemeHighlight::highlight_line`].
    pub fn highlight_line<'b>(
        &mut self,
        line: &'b str,
        syntax_set: &SyntaxSet,
    ) -> Result<Vec<(Style, &'b str)>, Error> {
        self.highlight.highlight_line(line, syntax_set)
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
            let regions = hf.highlight_line(&line, &ss).unwrap();
            assert!(!regions.is_empty());
            line.clear();
        }
    }

    #[cfg(all(feature = "default-syntaxes", feature = "default-themes"))]
    #[test]
    fn theme_scope_renderer_produces_output() {
        use std::fmt::Write;
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let syntax = ss.find_syntax_by_extension("rs").unwrap();

        let renderer =
            ThemeScopeRenderer::new(&ts.themes["base16-ocean.dark"], |style, text, output| {
                let fg = style.foreground;
                write!(output, "\x1b[38;2;{};{};{}m{}", fg.r, fg.g, fg.b, text).unwrap();
            });
        let mut h = HighlightLines::new(syntax, &ss, renderer);
        h.highlight_line("pub struct Wow { hi: u64 }\n")
            .unwrap();
        let output = String::from_utf8(h.finalize()).unwrap();
        assert!(!output.is_empty());
        assert!(output.contains("\x1b[38;2;"));
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
