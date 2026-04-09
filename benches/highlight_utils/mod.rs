use syntect::highlighting::{HighlightIterator, HighlightState, Highlighter, Theme};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// Common helper for benchmarking highlighting.
pub fn do_highlight(
    s: &str,
    syntax_set: &SyntaxSet,
    syntax: &SyntaxReference,
    theme: &Theme,
) -> usize {
    let highlighter = Highlighter::new(theme);
    let mut highlight_state = HighlightState::new(&highlighter, ScopeStack::new());
    let mut parse_state = ParseState::new(syntax);
    let mut count = 0;
    for line in s.lines() {
        let ops = parse_state.parse_line(line, syntax_set).unwrap().ops;
        let regions: Vec<_> =
            HighlightIterator::new(&mut highlight_state, &ops[..], line, &highlighter).collect();
        count += regions.len();
    }
    count
}
