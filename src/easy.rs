//! API wrappers for common use cases like highlighting strings and
//! files without caring about intermediate semantic representation
//! and caching.

use parsing::{ScopeStack, ParseState, SyntaxDefinition, SyntaxSet, ScopeStackOp, Scope};
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
        let ops = self.parse_state.parse_line(line);
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
    /// This example uses `reader.lines()` to get lines without a newline character.
    /// See the `syncat` example for an example of reading lines with a newline character, which gets slightly more robust
    /// and fast syntax highlighting, at the cost of a couple extra lines of code.
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
        let f = File::open(path)?;
        let syntax = ss.find_syntax_for_file(path)?
            .unwrap_or_else(|| ss.find_syntax_plain_text());

        Ok(HighlightFile {
            reader: BufReader::new(f),
            highlight_lines: HighlightLines::new(syntax, theme),
        })
    }
}

/// Iterator over the regions of a line which a given the operation from the parser applies.
///
/// To use just keep your own `ScopeStack` and then `ScopeStack.apply(op)` the operation that is yielded
/// at the top of your `for` loop over this iterator. Now you have a substring of the line and the scope stack
/// for that token.
///
/// See the `synstats.rs` example for an example of using this iterator.
///
/// **Note:** This will often return empty regions, just `continue` after applying the op if you don't want them.
#[derive(Debug)]
pub struct ScopeRegionIterator<'a> {
    ops: &'a [(usize, ScopeStackOp)],
    line: &'a str,
    index: usize,
    last_str_index: usize,
}

impl<'a> ScopeRegionIterator<'a> {
    pub fn new(ops: &'a [(usize, ScopeStackOp)], line: &'a str) -> ScopeRegionIterator<'a> {
        ScopeRegionIterator {
            ops: ops,
            line: line,
            index: 0,
            last_str_index: 0,
        }
    }
}

static NOOP_OP: ScopeStackOp = ScopeStackOp::Noop;
impl<'a> Iterator for ScopeRegionIterator<'a> {
    type Item = (&'a str, &'a ScopeStackOp);
    fn next(&mut self) -> Option<Self::Item> {
        if self.index > self.ops.len() {
            return None;
        }

        // region extends up to next operation (ops[index]) or string end if there is none
        // note the next operation may be at, last_str_index, in which case the region is empty
        let next_str_i = if self.index == self.ops.len() {
            self.line.len()
        } else {
            self.ops[self.index].0
        };
        let substr = &self.line[self.last_str_index..next_str_i];
        self.last_str_index = next_str_i;

        // the first region covers everything before the first op, which may be empty
        let op = if self.index == 0 {
            &NOOP_OP
        } else {
            &self.ops[self.index-1].1
        };

        self.index += 1;
        Some((substr, op))
    }
}

#[derive(Clone, Copy)]
pub struct SyntaxTestOutputOptions {
    pub time: bool,
    pub debug: bool,
    pub summary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxTestFileResult {
    FailedAssertions(usize, usize),
    Success(usize),
}

pub fn process_syntax_test_assertions(syntax: &SyntaxDefinition, text: &str, testtoken_start: &str, testtoken_end: Option<&str>, out_opts: &SyntaxTestOutputOptions) -> SyntaxTestFileResult {
    use std::collections::VecDeque;
    use highlighting::ScopeSelectors;

    #[derive(Debug)]
    struct SyntaxTestAssertionRange {
        test_line_offset: usize,
        line_number: usize,
        begin_char: usize,
        end_char: usize,
        scope_selector: ScopeSelectors,
        scope_selector_text: String,
    }

    fn get_syntax_test_assertions(token_start: &str, token_end: Option<&str>, text: &str) -> VecDeque<SyntaxTestAssertionRange> {
        use std::str::FromStr;
        
        let mut assertions = VecDeque::new();
        let mut test_line_offset = 0;
        //let mut test_line_len = 0;
        let mut line_number = 0;
        let mut offset = 0;
        //let mut remainder = None;
        for line in text.lines() {
            line_number += 1;
            let mut line_has_assertions = false;
            
            // if the test start token specified is on the line
            if let Some(index) = line.find(token_start) {
                let token_and_rest_of_line = line.split_at(index).1;

                let rest_of_line = &token_and_rest_of_line[token_start.len()..];
                if let Some(assertion_index) = rest_of_line.find("<-").or_else(|| rest_of_line.find('^')) {
                    let mut assertion_range = 0;
                    while rest_of_line.chars().nth(assertion_index + assertion_range) == Some('^') {
                        assertion_range += 1;
                    }
                    let skip_assertion_chars = if assertion_range == 0 { 2 } else { assertion_range };

                    let mut selector_text : String = rest_of_line.chars().skip(assertion_index + skip_assertion_chars).collect(); // get the scope selector text

                    if let Some(token) = token_end { // if there is an end token defined in the test file header
                        if let Some(end_token_pos) = selector_text.find(token) { // and there is an end token in the line
                            selector_text = selector_text.chars().take(end_token_pos).collect(); // the scope selector text ends at the end token
                        }
                    }

                    let assertion = SyntaxTestAssertionRange {
                        test_line_offset: test_line_offset,
                        line_number: line_number,
                        begin_char: index + if assertion_range > 0 { token_start.len() + assertion_index } else { 0 },
                        end_char: index + if assertion_range > 0 { token_start.len() + assertion_index + assertion_range } else { 1 },

                        // format the scope selector to include a space at the beginning, because, currently, ScopeSelector expects excludes to begin with " -"
                        // and they are sometimes in the syntax test as ^^^-comment, for example
                        scope_selector: ScopeSelectors::from_str(&format!(" {}", &selector_text)).expect(&format!("Scope selector invalid on line {}", line_number)),
                        scope_selector_text: selector_text,
                    };
                    /*if assertion.end_char > test_line_len {
                        remainder = Some(SyntaxTestAssertionRange {
                            test_line_offset: test_line_offset + test_line_len,
                            line_number: line_number,
                            begin_char: assertion.begin_char - test_line_len,
                            end_char: assertion.end_char - test_line_len,
                            scope_selector: assertion.scope_selector.clone(),
                            scope_selector_text: assertion.scope_selector_text.clone(),
                        });
                    }*/
                    assertions.push_back(assertion);
                    
                    line_has_assertions = true;
                }
            }
            if !line_has_assertions { // ST seems to ignore lines that have assertions when calculating which line the assertion tests against, regardless of whether they contain any other text
                test_line_offset = offset;
                //test_line_len = line.len() + 1;
            }
            offset += line.len() + 1; // the +1 is for the `\n`. TODO: maybe better to loop over the lines including the newline chars, using https://stackoverflow.com/a/40457615/4473405
        }
        assertions
    }
    
    #[derive(Debug)]
    struct ScopedText {
        scope: Vec<Scope>,
        char_start: usize,
        text_len: usize,
    }
    
    #[derive(Debug)]
    struct RangeTestResult {
        column_begin: usize,
        column_end: usize,
        success: bool,
        actual_scope: String,
    }
    
    fn process_assertions(assertion: &SyntaxTestAssertionRange, test_against_line_scopes: &Vec<ScopedText>) -> Vec<RangeTestResult> {
        use std::cmp::{min, max};
        // find the scope at the specified start column, and start matching the selector through the rest of the tokens on the line from there until the end column is reached
        let mut results = Vec::new();
        for scoped_text in test_against_line_scopes.iter().skip_while(|s|s.char_start + s.text_len <= assertion.begin_char).take_while(|s|s.char_start < assertion.end_char) {
            let match_value = assertion.scope_selector.does_match(scoped_text.scope.as_slice());
            let result = RangeTestResult {
                column_begin: max(scoped_text.char_start, assertion.begin_char),
                column_end: min(scoped_text.char_start + scoped_text.text_len, assertion.end_char),
                success: match_value.is_some(),
                actual_scope: format!("{:?}", scoped_text.scope.as_slice()),
            };
            results.push(result);
        }
        results
    }
    
    let mut assertions = get_syntax_test_assertions(testtoken_start, testtoken_end, &text);
    //println!("{:?}", assertions);
    use util::debug_print_ops;
    
    // iterate over the lines of the file, testing them
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();

    let mut offset = 0;
    let mut scopes_on_line_being_tested = Vec::new();
    let mut line_number = 0;
    let mut relevant_assertions = Vec::new();
    
    let mut assertion_failures: usize = 0;
    let mut total_assertions: usize = 0;

    for line_without_char in text.lines() {
        let line = &(line_without_char.to_owned() + "\n");
        line_number += 1;
        
        let eol_offset = offset + line.len();
        
        // parse the line
        let ops = state.parse_line(&line);
        // find assertions that relate to the current line
        relevant_assertions.clear();
        while let Some(assertion) = assertions.pop_front() {
            let pos = assertion.test_line_offset + assertion.begin_char;
            if pos >= offset && pos < eol_offset {
                relevant_assertions.push(assertion);
            } else {
                assertions.push_front(assertion);
                break;
            }
        }
        if !relevant_assertions.is_empty() {
            scopes_on_line_being_tested.clear();
            if out_opts.debug {
                println!("-- debugging line {} -- scope stack: {:?}", line_number, stack);
                if ops.is_empty() && !line.is_empty() {
                    println!("no operations for this line...");
                } else {
                    debug_print_ops(&line, &ops);
                }
            }
        }
        
        {
            let mut col: usize = 0;
            for (s, op) in ScopeRegionIterator::new(&ops, &line) {
                stack.apply(op);
                if s.is_empty() { // in this case we don't care about blank tokens
                    continue;
                }
                if !relevant_assertions.is_empty() {
                    let len = s.chars().count();
                    scopes_on_line_being_tested.push(
                        ScopedText {
                            char_start: col,
                            text_len: len,
                            scope: stack.as_slice().to_vec()
                        }
                    );
                    col += len;
                }
            }
        }
        
        for assertion in &relevant_assertions {
            let results = process_assertions(&assertion, &scopes_on_line_being_tested);
            
            for result in results {
                let length = result.column_end - result.column_begin;
                total_assertions += length;
                if !result.success {
                    assertion_failures += length;
                    let text: String = line.chars().skip(result.column_begin).take(length).collect();
                    if !out_opts.summary {
                        println!("  Assertion selector {:?} \
                            from line {:?} failed against line {:?}, column range {:?}-{:?} \
                            (with text {:?}) \
                            has scope {:?}",
                            &assertion.scope_selector_text.trim(),
                            &assertion.line_number, line_number, result.column_begin, result.column_end,
                            text,
                            result.actual_scope,
                        );
                    }
                }
            }
        }
        
        offset = eol_offset;
    }
    
    let res = if assertion_failures > 0 {
        SyntaxTestFileResult::FailedAssertions(assertion_failures, total_assertions)
    } else {
        SyntaxTestFileResult::Success(total_assertions)
    };
    res
}

#[cfg(all(feature = "assets", any(feature = "dump-load", feature = "dump-load-rs")))]
#[cfg(test)]
mod tests {
    use super::*;
    use parsing::{SyntaxSet, ParseState, ScopeStack};
    use highlighting::ThemeSet;
    use std::str::FromStr;

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

    #[test]
    fn can_find_regions() {
        let ss = SyntaxSet::load_defaults_nonewlines();
        let mut state = ParseState::new(ss.find_syntax_by_extension("rb").unwrap());
        let line = "lol =5+2";
        let ops = state.parse_line(line);

        let mut stack = ScopeStack::new();
        let mut token_count = 0;
        for (s, op) in ScopeRegionIterator::new(&ops, line) {
            stack.apply(op);
            if s.is_empty() { // in this case we don't care about blank tokens
                continue;
            }
            if token_count == 1 {
                assert_eq!(stack, ScopeStack::from_str("source.ruby keyword.operator.assignment.ruby").unwrap());
                assert_eq!(s, "=");
            }
            token_count += 1;
            println!("{:?} {}", s, stack);
        }
        assert_eq!(token_count, 5);
    }

    #[test]
    fn can_find_regions_with_trailing_newline() {
        let ss = SyntaxSet::load_defaults_newlines();
        let mut state = ParseState::new(ss.find_syntax_by_extension("rb").unwrap());
        let lines = ["# hello world\n", "lol=5+2\n"];
        let mut stack = ScopeStack::new();

        for line in lines.iter() {
            let ops = state.parse_line(&line);
            println!("{:?}", ops);

            let mut iterated_ops: Vec<&ScopeStackOp> = Vec::new();
            for (_, op) in ScopeRegionIterator::new(&ops, &line) {
                stack.apply(op);
                iterated_ops.push(&op);
                println!("{:?}", op);
            }

            let all_ops: Vec<&ScopeStackOp> = ops.iter().map(|t|&t.1).collect();
            assert_eq!(all_ops.len(), iterated_ops.len() - 1); // -1 because we want to ignore the NOOP
        }
    }
}
