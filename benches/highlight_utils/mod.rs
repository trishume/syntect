use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

/// Common helper for benchmarking highlighting.
pub fn do_highlight(
    s: &str,
    syntax_set: &SyntaxSet,
    syntax: &SyntaxReference,
    theme: &Theme,
) -> usize {
    let mut highlight = HighlightLines::new(syntax, syntax_set, theme);
    for line in LinesWithEndings::from(s) {
        highlight.highlight_line(line).unwrap();
    }
    highlight.finalize().len()
}
