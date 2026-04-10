//! Scope-based syntax rendering: traits, adapters, and the core line renderer.
//!
//! This module defines the rendering layer that sits between parsed scope
//! events and a final output format. It exposes three traits, each tailored
//! to a different audience:
//!
//! - [`ScopeMarkup`] — slim trait for **stateless** renderers that map scope
//!   structure 1:1 to output structure (CSS-classed HTML, XML, …). Most
//!   markup-style renderers should implement this.
//! - [`StyledOutput`] — small trait for **theme-aware** renderers that emit
//!   per-style spans (ANSI, inline-styled HTML, LaTeX `\textcolor`, …).
//!   Implementations are wrapped in [`ThemedRenderer`] which manages the
//!   theme lookup, the style stack, and adjacent-token style merging on the
//!   implementor's behalf.
//! - [`ScopeRenderer`] — the **low-level** trait the engine actually consumes.
//!   Reach for this only when you need raw [`Scope`] / `&[Scope]` access or
//!   want fine-grained control over the empty-scope optimization. Most users
//!   should pick one of the layered traits above.
//!
//! Built-in implementations:
//! - [`AnsiStyledOutput`] — 24-bit ANSI colour escapes (the default for
//!   [`crate::io::HighlightedWriter::new`]).
//! - [`crate::html::ClassedHTMLScopeRenderer`] — CSS-classed HTML
//!   (`ScopeMarkup`).
//! - [`crate::html::HtmlStyledOutput`] — inline-styled HTML
//!   (`StyledOutput`).

use crate::highlighting::{Highlighter, Style, Theme};
use crate::parsing::{
    lock_global_scope_repo, BasicScopeStackOp, Scope, ScopeRepository, ScopeStack, ScopeStackOp,
};
use crate::util::blend_fg_color;
use crate::Error;
use std::fmt::Write as FmtWrite;

// ---------------------------------------------------------------------------
// ScopeRenderer — low-level engine trait
// ---------------------------------------------------------------------------

/// **Low-level** trait that the rendering engine consumes directly.
///
/// Most users should not implement this directly. Pick one of the layered
/// traits instead:
///
/// - [`ScopeMarkup`] for stateless renderers (like CSS-classed HTML) that map
///   scope structure 1:1 to output structure.
/// - [`StyledOutput`] (used via [`ThemedRenderer`]) for theme-aware renderers
///   that emit per-style spans with adjacent-token merging.
///
/// `ScopeRenderer` is kept public as an escape hatch for advanced cases that
/// need raw [`Scope`] / `&[Scope]` access, line-level scope-stack inspection,
/// or fine-grained control over the empty-scope truncation via the `bool`
/// return from [`begin_scope`].
///
/// [`begin_scope`]: ScopeRenderer::begin_scope
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
// ScopeMarkup — slim trait for stateless markup renderers
// ---------------------------------------------------------------------------

/// Trait for **stateless** renderers that map scope structure 1:1 to output
/// structure.
///
/// Implementors only see the scope's pre-resolved atom strings (e.g.
/// `["source", "rust"]` for `source.rust`) and the literal text between scope
/// boundaries — no [`Scope`] value, no `&[Scope]` stack, no return value from
/// [`begin_scope`]. The empty-scope optimization (truncate on a push/pop pair
/// with no text between) is applied unconditionally.
///
/// For renderers that need to resolve scopes against a theme and emit
/// per-style spans, implement [`StyledOutput`] instead and wrap it in
/// [`ThemedRenderer`].
///
/// For unusual cases that need raw [`Scope`] / `&[Scope]` access, drop down
/// to the low-level [`ScopeRenderer`] trait.
///
/// [`begin_scope`]: ScopeMarkup::begin_scope
///
/// # Example
///
/// A trivial markup renderer that wraps each scope in `<span class="...">`:
///
/// ```ignore
/// use std::fmt::Write;
/// use syntect::rendering::ScopeMarkup;
///
/// struct SimpleClassed;
///
/// impl ScopeMarkup for SimpleClassed {
///     fn begin_scope(&mut self, atom_strs: &[&str], output: &mut String) {
///         output.push_str("<span class=\"");
///         for (i, atom) in atom_strs.iter().enumerate() {
///             if i != 0 { output.push(' '); }
///             output.push_str(atom);
///         }
///         output.push_str("\">");
///     }
///     fn end_scope(&mut self, output: &mut String) {
///         output.push_str("</span>");
///     }
/// }
/// ```
pub trait ScopeMarkup {
    /// Open a markup wrapping a scope. Always paired with a later
    /// [`end_scope`]. If no text is written between the matching `end_scope`,
    /// the engine transparently truncates this output (empty-scope
    /// optimization).
    ///
    /// [`end_scope`]: ScopeMarkup::end_scope
    fn begin_scope(&mut self, atom_strs: &[&str], output: &mut String);

    /// Close the most recently opened markup.
    fn end_scope(&mut self, output: &mut String);

    /// Write literal text. Default: passthrough. Override to escape.
    fn write_text(&mut self, text: &str, output: &mut String) {
        output.push_str(text);
    }

    /// Optional line-start hook. Default: no-op.
    fn begin_line(&mut self, _line_index: usize, _output: &mut String) {}

    /// Optional line-end hook. Default: no-op.
    fn end_line(&mut self, _line_index: usize, _output: &mut String) {}
}

// ---------------------------------------------------------------------------
// MarkupAdapter — bridges ScopeMarkup to ScopeRenderer
// ---------------------------------------------------------------------------

/// Internal adapter that bridges any [`ScopeMarkup`] to the low-level
/// [`ScopeRenderer`] trait the engine consumes.
///
/// Construct one indirectly via [`crate::io::HighlightedWriter::with_markup`]
/// — there's no reason to instantiate this type by hand.
#[doc(hidden)]
pub struct MarkupAdapter<M: ScopeMarkup> {
    inner: M,
}

impl<M: ScopeMarkup> MarkupAdapter<M> {
    pub(crate) fn new(inner: M) -> Self {
        Self { inner }
    }

    /// Returns a reference to the wrapped markup renderer.
    pub fn inner(&self) -> &M {
        &self.inner
    }

    /// Returns a mutable reference to the wrapped markup renderer.
    pub fn inner_mut(&mut self) -> &mut M {
        &mut self.inner
    }

    /// Consume the adapter and return the wrapped markup renderer.
    pub fn into_inner(self) -> M {
        self.inner
    }
}

impl<M: ScopeMarkup> ScopeRenderer for MarkupAdapter<M> {
    fn begin_line(&mut self, line_index: usize, _scope_stack: &[Scope], output: &mut String) {
        self.inner.begin_line(line_index, output);
    }

    fn end_line(&mut self, line_index: usize, _scope_stack: &[Scope], output: &mut String) {
        self.inner.end_line(line_index, output);
    }

    fn begin_scope(
        &mut self,
        atom_strs: &[&str],
        _scope: Scope,
        _scope_stack: &[Scope],
        output: &mut String,
    ) -> bool {
        self.inner.begin_scope(atom_strs, output);
        true
    }

    fn end_scope(&mut self, output: &mut String) {
        self.inner.end_scope(output);
    }

    fn write_text(&mut self, text: &str, output: &mut String) {
        self.inner.write_text(text, output);
    }
}

// ---------------------------------------------------------------------------
// StyledOutput — small format-specific surface for theme-aware renderers
// ---------------------------------------------------------------------------

/// Trait for **theme-aware** renderers that emit per-style spans.
///
/// An implementor describes only how to write the format-specific pieces:
/// "open a span styled like this", "close the open span", "write some literal
/// text". The accompanying [`ThemedRenderer`] adapter takes care of the
/// scope→style resolution, the style stack, and merging adjacent text tokens
/// that resolve to the same style.
///
/// Implementations are plugged into [`crate::io::HighlightedWriter`] via
/// [`crate::io::HighlightedWriter::with_themed`], which constructs the
/// `ThemedRenderer` wrapper for you.
///
/// # Example
///
/// A minimal LaTeX `\textcolor` emitter:
///
/// ```ignore
/// use std::fmt::Write;
/// use syntect::highlighting::Style;
/// use syntect::rendering::StyledOutput;
///
/// struct LatexStyledOutput;
///
/// impl StyledOutput for LatexStyledOutput {
///     fn begin_style(&mut self, style: Style, output: &mut String) {
///         write!(output, "\\textcolor[RGB]{{{},{},{}}}{{",
///                style.foreground.r, style.foreground.g, style.foreground.b).unwrap();
///     }
///     fn end_style(&mut self, output: &mut String) {
///         output.push('}');
///     }
///     fn write_text(&mut self, text: &str, output: &mut String) {
///         for ch in text.chars() {
///             match ch {
///                 '\\' => output.push_str("\\\\"),
///                 '{'  => output.push_str("\\{"),
///                 '}'  => output.push_str("\\}"),
///                 _    => output.push(ch),
///             }
///         }
///     }
/// }
/// ```
pub trait StyledOutput {
    /// Open a span styled with this [`Style`]. Always paired with a later
    /// [`end_style`].
    ///
    /// [`end_style`]: StyledOutput::end_style
    fn begin_style(&mut self, style: Style, output: &mut String);

    /// Close the currently-open styled span.
    ///
    /// May be a no-op for formats where the next [`begin_style`] overrides
    /// the current style with no explicit close needed (e.g. ANSI escape
    /// codes).
    ///
    /// [`begin_style`]: StyledOutput::begin_style
    fn end_style(&mut self, output: &mut String);

    /// Write literal text inside the currently-open styled span. Implementors
    /// handle any format-specific escaping here.
    ///
    /// By default the text passed in may contain newlines, which are emitted
    /// verbatim inside the styled span. Override
    /// [`closes_at_line_boundaries`] to instead have the [`ThemedRenderer`]
    /// adapter split text on newline boundaries, close the styled span, and
    /// write the `'\n'` literal to `output` between spans.
    ///
    /// [`closes_at_line_boundaries`]: StyledOutput::closes_at_line_boundaries
    fn write_text(&mut self, text: &str, output: &mut String);

    /// Whether styled spans must be closed at line boundaries.
    ///
    /// When `true`, the [`ThemedRenderer`] adapter splits text on `'\n'`:
    /// it emits the pre-newline portion under the current style, calls
    /// [`end_style`] to close the span, writes the `'\n'` literal directly
    /// to `output`, then resumes with the post-newline portion. This keeps
    /// every styled span self-contained on a single line — necessary for
    /// nested-group formats like LaTeX `\textcolor{...}{...}` where a
    /// newline inside the group breaks line-oriented tooling (e.g.
    /// fancyvrb's `Verbatim` environment).
    ///
    /// The default is `false`, which is correct for ANSI escape codes and
    /// HTML `<span>` (whitespace inside a span is harmless in both formats).
    ///
    /// [`end_style`]: StyledOutput::end_style
    fn closes_at_line_boundaries(&self) -> bool {
        false
    }

    /// Whether a text token should be folded into the previously-emitted
    /// styled span without closing and reopening it. Default: only when the
    /// resolved styles are exactly equal.
    ///
    /// Override to relax the merge predicate. For example, HTML can fold
    /// whitespace into a span with a different foreground colour as long as
    /// the background colours match — whitespace doesn't reveal a foreground
    /// difference.
    fn should_merge(&self, prev: Style, next: Style, _text: &str) -> bool {
        prev == next
    }
}

// ---------------------------------------------------------------------------
// ThemedRenderer — adapter that turns a StyledOutput into a ScopeRenderer
// ---------------------------------------------------------------------------

/// Adapter that turns a [`StyledOutput`] into a [`ScopeRenderer`] by managing
/// the theme lookup, the style stack mirroring the scope stack, and merging
/// adjacent text tokens that resolve to the same style.
///
/// Construct one with [`ThemedRenderer::new`] passing a [`Theme`] and a
/// [`StyledOutput`] implementation. The result plugs into
/// [`crate::io::HighlightedWriter::with_themed`] (or any other
/// [`ScopeRenderer`] consumer).
///
/// # Example
///
/// ```
/// use std::io::Write;
/// use syntect::io::HighlightedWriter;
/// use syntect::highlighting::ThemeSet;
/// use syntect::parsing::SyntaxSet;
/// use syntect::rendering::{AnsiStyledOutput, ThemedRenderer};
///
/// let ss = SyntaxSet::load_defaults_newlines();
/// let ts = ThemeSet::load_defaults();
/// let syntax = ss.find_syntax_by_extension("rs").unwrap();
///
/// let renderer = ThemedRenderer::new(&ts.themes["base16-ocean.dark"], AnsiStyledOutput::new(false));
/// let mut w = HighlightedWriter::with_renderer(syntax, &ss, renderer);
/// w.write_all(b"fn main() {}\n").unwrap();
/// let output = String::from_utf8(w.finalize().unwrap()).unwrap();
/// assert!(output.contains("\x1b[38;2;"));
/// ```
pub struct ThemedRenderer<'a, O: StyledOutput> {
    highlighter: Highlighter<'a>,
    style_stack: Vec<Style>,
    last_written_style: Option<Style>,
    output: O,
}

impl<'a, O: StyledOutput> ThemedRenderer<'a, O> {
    /// Wrap a [`StyledOutput`] with theme-aware state.
    pub fn new(theme: &'a Theme, output: O) -> Self {
        let highlighter = Highlighter::new(theme);
        let default_style = highlighter.style_for_stack(&[]);
        Self {
            highlighter,
            style_stack: vec![default_style],
            last_written_style: None,
            output,
        }
    }

    /// Returns a reference to the wrapped [`StyledOutput`].
    pub fn output(&self) -> &O {
        &self.output
    }

    /// Returns a mutable reference to the wrapped [`StyledOutput`].
    pub fn output_mut(&mut self) -> &mut O {
        &mut self.output
    }

    /// Consume the adapter and return the wrapped [`StyledOutput`].
    pub fn into_output(self) -> O {
        self.output
    }

    fn current_style(&self) -> Style {
        self.style_stack.last().copied().unwrap_or_default()
    }
}

impl<'a, O: StyledOutput> ThemedRenderer<'a, O> {
    /// Inner write_text that assumes `text` contains no newline.
    fn write_text_chunk(&mut self, text: &str, output: &mut String) {
        if text.is_empty() {
            return;
        }
        let style = self.current_style();
        let merge = match self.last_written_style {
            Some(prev) => self.output.should_merge(prev, style, text),
            None => false,
        };
        if !merge {
            if self.last_written_style.is_some() {
                self.output.end_style(output);
            }
            self.output.begin_style(style, output);
            self.last_written_style = Some(style);
        }
        self.output.write_text(text, output);
    }
}

impl<'a, O: StyledOutput> ScopeRenderer for ThemedRenderer<'a, O> {
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
        if !self.output.closes_at_line_boundaries() {
            // Default path: pass text through verbatim, including newlines.
            self.write_text_chunk(text, output);
            return;
        }
        // Opt-in: split on '\n' so styled spans never straddle a line
        // boundary. Emit the pre-newline portion under the current style,
        // close the span, write '\n' literally, then resume.
        let mut rest = text;
        while let Some(nl) = rest.find('\n') {
            self.write_text_chunk(&rest[..nl], output);
            if self.last_written_style.take().is_some() {
                self.output.end_style(output);
            }
            output.push('\n');
            rest = &rest[nl + 1..];
        }
        self.write_text_chunk(rest, output);
    }

    fn end_line(&mut self, _line_index: usize, _scope_stack: &[Scope], output: &mut String) {
        if self.last_written_style.take().is_some() {
            self.output.end_style(output);
        }
    }
}

// ---------------------------------------------------------------------------
// AnsiStyledOutput — 24-bit ANSI colour escapes
// ---------------------------------------------------------------------------

/// A [`StyledOutput`] that emits ANSI 24-bit colour escape codes.
///
/// Wrap with [`ThemedRenderer`] to use it as a [`ScopeRenderer`]. The default
/// renderer of [`crate::io::HighlightedWriter::new`] is
/// `ThemedRenderer<AnsiStyledOutput>`.
///
/// Foreground alpha is blended against the background colour. When
/// `include_bg` is `true`, the background colour escape code is also emitted.
///
/// `end_style` is intentionally a no-op: the next [`begin_style`] simply
/// overwrites the active colour with a new escape sequence.
///
/// [`begin_style`]: StyledOutput::begin_style
pub struct AnsiStyledOutput {
    include_bg: bool,
}

impl AnsiStyledOutput {
    /// Create a new ANSI emitter.
    ///
    /// If `include_bg` is `true`, the background colour escape code is also
    /// emitted alongside the foreground.
    pub fn new(include_bg: bool) -> Self {
        Self { include_bg }
    }

    /// Whether background colour escapes are emitted.
    pub fn include_bg(&self) -> bool {
        self.include_bg
    }
}

impl StyledOutput for AnsiStyledOutput {
    fn begin_style(&mut self, style: Style, output: &mut String) {
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
    }

    fn end_style(&mut self, _output: &mut String) {
        // ANSI: next begin_style overwrites the colour, no close needed.
    }

    fn write_text(&mut self, text: &str, output: &mut String) {
        output.push_str(text);
    }
}
