use syntect::easy::ThemeHighlight;
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
    let mut highlight = ThemeHighlight::new(syntax, theme);
    let mut count = 0;
    for line in LinesWithEndings::from(s) {
        let regions = highlight.highlight_line(line, syntax_set).unwrap();
        count += regions.len();
    }
    count
}
