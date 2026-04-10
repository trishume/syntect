use std::io::Write;
use syntect::highlighting::Theme;
use syntect::io::HighlightedWriter;
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// Common helper for benchmarking highlighting.
pub fn do_highlight(
    s: &str,
    syntax_set: &SyntaxSet,
    syntax: &SyntaxReference,
    theme: &Theme,
) -> usize {
    let mut highlight = HighlightedWriter::new(syntax, syntax_set, theme);
    highlight.write_all(s.as_bytes()).unwrap();
    highlight.finalize().unwrap().len()
}
