use scope::ScopeStack;
use parser::*;
use theme::highlighter::*;
use theme::style::*;
use theme::theme::Theme;
use syntax_definition::SyntaxDefinition;
// use util::debug_print_ops;

/// Simple way to go directly from lines of text to coloured
/// tokens.
///
/// Depending on how you load the packages (see the `PackageSet` docs)
/// you can either pass this strings with trailing `\n`s or without.
///
/// # Example
/// Prints coloured lines of a string to the terminal
///
/// ```
/// use syntect::easy::HighlightLines;
/// use syntect::package_set::PackageSet;
/// use syntect::theme_set::ThemeSet;
/// use syntect::theme::style::Style;
/// use syntect::util::as_24_bit_terminal_escaped;
///
/// // Load these once at the start of your program
/// let ps = PackageSet::load_defaults_nonewlines();
/// let ts = ThemeSet::load_defaults();
///
/// let syntax = ps.find_syntax_by_extension("rs").unwrap();
/// let mut h = HighlightLines::new(syntax, &ts.themes["base16-ocean.dark"]);
/// let s = "pub struct Wow { hi: u64 }\nfn blah() -> u64 {}";
/// for line in s.lines() {
///     let ranges: Vec<(Style, &str)> = h.highlight(line);
///     let escaped = as_24_bit_terminal_escaped(&ranges[..], true);
///     println!("{}", escaped);
/// }
/// ```
pub struct HighlightLines<'a> {
    highlighter: Highlighter<'a>,
    parse_state: ParseState,
    highlight_state: HighlightState,
}

impl<'a> HighlightLines<'a> {
    pub fn new(syntax: &SyntaxDefinition, theme: &'a Theme) -> HighlightLines<'a> {
        let highlighter = Highlighter::new(theme);
        let hstate = HighlightState::new(&highlighter, ScopeStack::new());
        HighlightLines {
            highlighter: highlighter,
            parse_state: ParseState::new(syntax),
            highlight_state: hstate,
        }
    }

    /// Highlights a line of a file
    pub fn highlight<'b>(&mut self, line: &'b str) -> Vec<(Style, &'b str)> {
        // println!("{}", self.highlight_state.path);
        let ops = self.parse_state.parse_line(&line);
        // debug_print_ops(line, &ops);
        let iter = HighlightIterator::new(&mut self.highlight_state, &ops[..], line, &self.highlighter);
        iter.collect()
    }
}

#[cfg(test)]
mod tests {
    use easy::*;
    #[test]
    fn can_highlight_lines() {
        use package_set::PackageSet;
        use theme_set::ThemeSet;
        let ps = PackageSet::load_defaults_nonewlines();
        let ts = ThemeSet::load_defaults();
        let syntax = ps.find_syntax_by_extension("rs").unwrap();
        let mut h = HighlightLines::new(syntax, &ts.themes["base16-ocean.dark"]);
        let ranges = h.highlight("pub struct Wow { hi: u64 }");
        assert!(ranges.len() > 4);
    }
}
