use super::ClassStyle;
use crate::escape::Escape;
use crate::parsing::{lock_global_scope_repo, Scope, ScopeRepository};
use crate::renderer::ScopeRenderer;
use std::collections::HashSet;
use std::fmt::Write;

/// An HTML renderer that produces `<span class="...">` elements with
/// CSS class names derived from scope atoms.
///
/// This produces identical output to the original `ClassedHTMLGenerator`.
pub struct HtmlScopeRenderer {
    style: ClassStyle,
}

impl HtmlScopeRenderer {
    pub fn new(style: ClassStyle) -> Self {
        Self { style }
    }
}

impl ScopeRenderer for HtmlScopeRenderer {
    fn begin_scope(
        &mut self,
        atom_strs: &[&str],
        _scope: Scope,
        _scope_stack: &[Scope],
        output: &mut String,
    ) -> bool {
        output.push_str("<span class=\"");
        for (i, atom) in atom_strs.iter().enumerate() {
            if i != 0 {
                output.push(' ');
            }
            match self.style {
                ClassStyle::Spaced => {}
                ClassStyle::SpacedPrefixed { prefix } => {
                    output.push_str(prefix);
                }
            }
            output.push_str(atom);
        }
        output.push_str("\">");
        true
    }

    fn end_scope(&mut self, output: &mut String) {
        output.push_str("</span>");
    }

    fn write_text(&mut self, text: &str, output: &mut String) -> Result<(), std::fmt::Error> {
        write!(output, "{}", Escape(text))
    }
}

/// A composable renderer wrapper that highlights specific lines by wrapping
/// them in `<span class="hl">` (or `<span class="prefix-hl">`).
///
/// It correctly closes and reopens any scope spans that cross line boundaries,
/// keeping the HTML well-nested.
///
/// # Example
///
/// ```
/// use syntect::html::{ClassedHighlighter, ClassStyle, HtmlScopeRenderer, LineHighlightingRenderer};
/// use syntect::parsing::SyntaxSet;
/// use syntect::util::LinesWithEndings;
///
/// let code = "x <- 5\ny <- 6\nx + y\n";
/// let syntax_set = SyntaxSet::load_defaults_newlines();
/// let syntax = syntax_set.find_syntax_by_name("R").unwrap();
/// let style = ClassStyle::Spaced;
/// let renderer = LineHighlightingRenderer::new(
///     HtmlScopeRenderer::new(style),
///     &[1], // 0-indexed: highlight the second line
///     style,
/// );
/// let mut gen = ClassedHighlighter::new_with_renderer(syntax, &syntax_set, renderer);
/// for line in LinesWithEndings::from(code) {
///     gen.parse_html_for_line_which_includes_newline(line).unwrap();
/// }
/// let html = gen.finalize();
/// assert!(html.contains("<span class=\"hl\">"));
/// ```
pub struct LineHighlightingRenderer<R: ScopeRenderer> {
    inner: R,
    highlighted_lines: HashSet<usize>,
    style: ClassStyle,
}

impl<R: ScopeRenderer> LineHighlightingRenderer<R> {
    pub fn new(inner: R, highlighted_lines: &[usize], style: ClassStyle) -> Self {
        Self {
            inner,
            highlighted_lines: highlighted_lines.iter().copied().collect(),
            style,
        }
    }
}

impl<R: ScopeRenderer> ScopeRenderer for LineHighlightingRenderer<R> {
    fn begin_line(&mut self, line_index: usize, scope_stack: &[Scope], output: &mut String) {
        if self.highlighted_lines.contains(&line_index) {
            // Close all open scope spans so the highlight wrapper is well-nested
            for _ in scope_stack {
                output.push_str("</span>");
            }
            // Open the highlight wrapper
            output.push_str("<span class=\"");
            if let ClassStyle::SpacedPrefixed { prefix } = self.style {
                output.push_str(prefix);
            }
            output.push_str("hl\">");
            // Reopen scope spans inside the highlight wrapper
            let repo = lock_global_scope_repo();
            for &scope in scope_stack {
                write_scope_open(output, scope, self.style, &repo);
            }
        }
        self.inner.begin_line(line_index, scope_stack, output);
    }

    fn end_line(&mut self, line_index: usize, scope_stack: &[Scope], output: &mut String) {
        self.inner.end_line(line_index, scope_stack, output);
        if self.highlighted_lines.contains(&line_index) {
            // Close all scope spans that were reopened inside the highlight wrapper
            for _ in scope_stack {
                output.push_str("</span>");
            }
            // Close the highlight wrapper
            output.push_str("</span>");
            // Reopen scope spans for the next line
            let repo = lock_global_scope_repo();
            for &scope in scope_stack {
                write_scope_open(output, scope, self.style, &repo);
            }
        }
    }

    fn begin_scope(
        &mut self,
        atom_strs: &[&str],
        scope: Scope,
        scope_stack: &[Scope],
        output: &mut String,
    ) -> bool {
        self.inner
            .begin_scope(atom_strs, scope, scope_stack, output)
    }

    fn end_scope(&mut self, output: &mut String) {
        self.inner.end_scope(output);
    }

    fn write_text(&mut self, text: &str, output: &mut String) -> Result<(), std::fmt::Error> {
        self.inner.write_text(text, output)
    }
}

/// Write an opening `<span class="...">` tag for a scope using atom strings from a repo.
fn write_scope_open(output: &mut String, scope: Scope, style: ClassStyle, repo: &ScopeRepository) {
    output.push_str("<span class=\"");
    for i in 0..(scope.len()) {
        let atom_s = repo.atom_str(scope.atom_at(i as usize));
        if i != 0 {
            output.push(' ');
        }
        match style {
            ClassStyle::Spaced => {}
            ClassStyle::SpacedPrefixed { prefix } => {
                output.push_str(prefix);
            }
        }
        output.push_str(atom_s);
    }
    output.push_str("\">");
}
