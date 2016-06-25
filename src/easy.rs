//! API wrappers for common use cases like highlighting strings and
//! files without caring about intermediate semantic representation
//! and caching.

use parsing::{ScopeStack, ParseState, SyntaxDefinition, SyntaxSet};
use highlighting::{Highlighter, HighlightState, HighlightIterator, Theme, Style};
use std::io::{self, BufReader};
use std::fs::File;
use std::path::Path;
// use util::debug_print_ops;

/// Simple way to go directly from lines of text to coloured
/// tokens.
///
/// Depending on how you load the syntaxes (see the `SyntaxSet` docs)
/// you can either pass this strings with trailing `\n`s or without.
///
/// # Examples
/// Prints coloured lines of a string to the terminal
///
/// ```
/// use syntect::easy::HighlightLines;
/// use syntect::parsing::SyntaxSet;
/// use syntect::highlighting::{ThemeSet, Style};
/// use syntect::util::as_24_bit_terminal_escaped;
///
/// // Load these once at the start of your program
/// let ps = SyntaxSet::load_defaults_nonewlines();
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
        // use util::debug_print_ops;
        // debug_print_ops(line, &ops);
        let iter =
            HighlightIterator::new(&mut self.highlight_state, &ops[..], line, &self.highlighter);
        iter.collect()
    }
}

/// Convenience struct containing everything you need to highlight a file.
/// Use the `reader` to get the lines of the file and the `highlight_lines` to highlight them.
/// See the `new` method docs for more information.
pub struct HighlightFile<'a> {
    pub reader: BufReader<File>,
    pub highlight_lines: HighlightLines<'a>,
}

impl<'a> HighlightFile<'a> {
    /// Constructs a file reader and a line highlighter to get you reading files as fast as possible.
    /// Auto-detects the syntax from the extension and constructs a `HighlightLines` with the correct syntax and theme.
    ///
    /// # Examples
    ///
    /// ```
    /// use syntect::parsing::SyntaxSet;
    /// use syntect::highlighting::{ThemeSet, Style};
    /// use syntect::util::as_24_bit_terminal_escaped;
    /// use syntect::easy::HighlightFile;
    /// use std::io::BufRead;
    ///
    /// let ss = SyntaxSet::load_defaults_nonewlines();
    /// let ts = ThemeSet::load_defaults();
    ///
    /// let mut highlighter = HighlightFile::new("testdata/highlight_test.erb", &ss, &ts.themes["base16-ocean.dark"]).unwrap();
    /// for maybe_line in highlighter.reader.lines() {
    ///     let line = maybe_line.unwrap();
    ///     let regions: Vec<(Style, &str)> = highlighter.highlight_lines.highlight(&line);
    ///     println!("{}", as_24_bit_terminal_escaped(&regions[..], true));
    /// }
    /// ```
    pub fn new<P: AsRef<Path>>(path_obj: P,
                               ss: &SyntaxSet,
                               theme: &'a Theme)
                               -> io::Result<HighlightFile<'a>> {
        let path: &Path = path_obj.as_ref();
        let f = try!(File::open(path));
        let syntax = try!(ss.find_syntax_for_file(path))
            .unwrap_or_else(|| ss.find_syntax_plain_text());

        Ok(HighlightFile {
            reader: BufReader::new(f),
            highlight_lines: HighlightLines::new(syntax, theme),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsing::SyntaxSet;
    use highlighting::ThemeSet;
    #[test]
    fn can_highlight_lines() {
        let ps = SyntaxSet::load_defaults_nonewlines();
        let ts = ThemeSet::load_defaults();
        let syntax = ps.find_syntax_by_extension("rs").unwrap();
        let mut h = HighlightLines::new(syntax, &ts.themes["base16-ocean.dark"]);
        let ranges = h.highlight("pub struct Wow { hi: u64 }");
        assert!(ranges.len() > 4);
    }

    #[test]
    fn can_highlight_file() {
        let ss = SyntaxSet::load_defaults_nonewlines();
        let ts = ThemeSet::load_defaults();
        HighlightFile::new("testdata/highlight_test.erb",
                           &ss,
                           &ts.themes["base16-ocean.dark"])
            .unwrap();
    }
}
