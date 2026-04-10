use std::io::Write;
use syntect::highlighting::Theme;
use syntect::io::HighlightedWriter;
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::rendering::AnsiStyledOutput;

/// Common helper for benchmarking highlighting.
pub fn do_highlight(
    s: &str,
    syntax_set: &SyntaxSet,
    syntax: &SyntaxReference,
    theme: &Theme,
) -> usize {
    let mut highlight =
        HighlightedWriter::from_themed(syntax, syntax_set, theme, AnsiStyledOutput::new(false))
            .build();
    for line in s.lines() {
        writeln!(highlight, "{}", line).unwrap();
    }
    highlight.finalize().unwrap().len()
}
