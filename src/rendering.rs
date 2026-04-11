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
//! - [`AnsiStyledOutput`] — 24-bit ANSI colour escapes. Pair with
//!   [`crate::io::HighlightedWriter::from_themed`] for the standard terminal
//!   output use case.
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
pub(crate) fn resolve_atom_strs(scope: Scope, repo: &ScopeRepository) -> Vec<&str> {
    (0..scope.len() as usize)
        .map(|i| repo.atom_str(scope.atom_at(i)))
        .collect()
}

/// Core rendering loop that drives a [`ScopeRenderer`].
///
/// Locks the global scope repository once per line. Applies the transparent
/// empty-scope optimization: if a scope push/pop pair contains no text, the
/// push output is truncated rather than emitting an empty element.
///
/// **Internal-only.** This function is not exposed to downstream users
/// because calling it line-by-line cannot correctly handle cross-line
/// branch-point failures: when the parser retroactively replays a span of
/// previously-emitted ops, the rendered output for those lines has already
/// been written and cannot be retracted. The only legitimate caller is
/// [`crate::io::HighlightedWriter`], which buffers rendered output during
/// speculative parsing and only invokes `render_line` once the ops are
/// final.
pub(crate) fn render_line<R: ScopeRenderer>(
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
/// Construct one indirectly via [`crate::io::HighlightedWriter::from_markup`]
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
/// [`crate::io::HighlightedWriter::from_themed`], which constructs the
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
    /// **Invariant:** `text` never contains a newline (`\n`). The
    /// [`ThemedRenderer`] adapter always splits text on newline boundaries,
    /// emits the pre-newline portion under the current style, closes the
    /// styled span via [`end_style`], writes the `'\n'` literal directly to
    /// `output`, and then resumes with the post-newline portion. This keeps
    /// every styled span self-contained on a single line — required for
    /// nested-group formats like LaTeX `\textcolor{...}{...}` where a
    /// newline inside the group breaks line-oriented tooling (e.g.
    /// fancyvrb's `Verbatim` environment) and harmless for ANSI / HTML.
    ///
    /// [`end_style`]: StyledOutput::end_style
    fn write_text(&mut self, text: &str, output: &mut String);

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
/// [`StyledOutput`] implementation. Most callers don't construct one
/// directly: [`crate::io::HighlightedWriter::from_themed`] takes a `Theme` and a
/// `StyledOutput` and wraps them in a `ThemedRenderer` automatically.
///
/// # Example
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
        // Split on '\n' so styled spans never straddle a line boundary.
        // Emit the pre-newline portion under the current style, close the
        // span, write '\n' literally to `output`, then resume with the
        // post-newline portion. This keeps formats with nested groups
        // (LaTeX `\textcolor{...}{...}`, HTML `<span>...</span>`)
        // well-formed and is harmless for ANSI escape codes.
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
/// Pair with [`crate::io::HighlightedWriter::from_themed`] (which wraps the
/// emitter in a [`ThemedRenderer`] for you) to highlight straight to a
/// terminal. Pass `AnsiStyledOutput::new(false)` for foreground-only output,
/// or `AnsiStyledOutput::new(true)` to additionally include background
/// colour escapes.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlighting::{Color, FontStyle, Style};

    /// A minimal `ScopeRenderer` that overrides only the required methods,
    /// inheriting `write_text`/`begin_line`/`end_line` from the trait
    /// defaults. Used to test those default implementations directly.
    struct BareScopeRenderer;
    impl ScopeRenderer for BareScopeRenderer {
        fn begin_scope(
            &mut self,
            _atom_strs: &[&str],
            _scope: Scope,
            _scope_stack: &[Scope],
            _output: &mut String,
        ) -> bool {
            true
        }
        fn end_scope(&mut self, _output: &mut String) {}
    }

    #[test]
    fn scope_renderer_default_write_text_passes_text_through() {
        // Catches: rendering.rs:97 ScopeRenderer::write_text default with ()
        let mut r = BareScopeRenderer;
        let mut out = String::new();
        r.write_text("hello", &mut out);
        assert_eq!(out, "hello");
    }

    /// Same idea for `ScopeMarkup`.
    struct BareScopeMarkup;
    impl ScopeMarkup for BareScopeMarkup {
        fn begin_scope(&mut self, _atom_strs: &[&str], _output: &mut String) {}
        fn end_scope(&mut self, _output: &mut String) {}
    }

    #[test]
    fn scope_markup_default_write_text_passes_text_through() {
        // Catches: rendering.rs:241 ScopeMarkup::write_text default with ()
        let mut m = BareScopeMarkup;
        let mut out = String::new();
        m.write_text("hello", &mut out);
        assert_eq!(out, "hello");
    }

    /// Minimal `StyledOutput` that overrides only the required methods,
    /// inheriting `should_merge` from the trait default.
    struct BareStyledOutput;
    impl StyledOutput for BareStyledOutput {
        fn begin_style(&mut self, _style: Style, _output: &mut String) {}
        fn end_style(&mut self, _output: &mut String) {}
        fn write_text(&mut self, _text: &str, _output: &mut String) {}
    }

    fn make_style(r: u8, g: u8, b: u8) -> Style {
        Style {
            foreground: Color { r, g, b, a: 0xff },
            background: Color {
                r: 0,
                g: 0,
                b: 0,
                a: 0xff,
            },
            font_style: FontStyle::empty(),
        }
    }

    #[test]
    fn styled_output_default_should_merge_compares_styles_for_equality() {
        // Catches all three mutants on the default impl:
        //   - L403 should_merge with true
        //   - L403 should_merge with false
        //   - L403 == replaced with !=
        let b = BareStyledOutput;
        let a = make_style(1, 2, 3);
        let same = make_style(1, 2, 3);
        let other = make_style(9, 8, 7);

        assert!(
            b.should_merge(a, same, "x"),
            "default impl must merge identical styles"
        );
        assert!(
            !b.should_merge(a, other, "x"),
            "default impl must NOT merge styles with different foreground"
        );
    }

    #[test]
    fn ansi_styled_output_include_bg_accessor_returns_constructor_arg() {
        // Catches: rendering.rs:586 AnsiStyledOutput::include_bg with true/false
        assert!(!AnsiStyledOutput::new(false).include_bg());
        assert!(AnsiStyledOutput::new(true).include_bg());
    }
}
