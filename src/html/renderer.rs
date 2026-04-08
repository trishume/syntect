use super::ClassStyle;
use crate::escape::Escape;
use crate::parsing::Scope;
use crate::renderer::ScopeRenderer;
use std::io::{self, Write};

/// An HTML renderer that produces `<span class="...">` elements with
/// CSS class names derived from scope atoms.
///
/// This produces identical output to the original `ClassedHTMLGenerator`.
pub struct HTMLScopeRenderer {
    style: ClassStyle,
}

impl HTMLScopeRenderer {
    pub fn new(style: ClassStyle) -> Self {
        Self { style }
    }
}

impl ScopeRenderer for HTMLScopeRenderer {
    fn begin_scope(
        &mut self,
        atom_strs: &[&str],
        _scope: Scope,
        _scope_stack: &[Scope],
        output: &mut Vec<u8>,
    ) -> bool {
        output.extend_from_slice(b"<span class=\"");
        for (i, atom) in atom_strs.iter().enumerate() {
            if i != 0 {
                output.push(b' ');
            }
            match self.style {
                ClassStyle::Spaced => {}
                ClassStyle::SpacedPrefixed { prefix } => {
                    output.extend_from_slice(prefix.as_bytes());
                }
            }
            output.extend_from_slice(atom.as_bytes());
        }
        output.extend_from_slice(b"\">");
        true
    }

    fn end_scope(&mut self, output: &mut Vec<u8>) {
        output.extend_from_slice(b"</span>");
    }

    fn write_text(&mut self, text: &str, output: &mut Vec<u8>) -> Result<(), io::Error> {
        write!(output, "{}", Escape(text))
    }
}
