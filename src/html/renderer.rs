use super::ClassStyle;
use crate::escape::Escape;
use crate::parsing::Scope;
use crate::renderer::ScopeRenderer;
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
