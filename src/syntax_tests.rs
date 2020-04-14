//! API for running syntax tests.
//! See http://www.sublimetext.com/docs/3/syntax.html#testing

//use std::io::Write;
use std::str::FromStr;
use crate::parsing::{ScopeStack, ParseState, SyntaxReference, SyntaxSet, Scope};
use crate::util::debug_print_ops;
use crate::easy::ScopeRegionIterator;
use crate::highlighting::ScopeSelectors;

#[derive(Clone, Copy)]
pub struct SyntaxTestOutputOptions {
    pub time: bool,
    pub debug: bool,
    pub summary: bool,
    pub failfast: bool,
    //pub output: &'a Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxTestFileResult {
    FailedAssertions(usize, usize),
    Success(usize),
}

#[derive(Debug)]
pub struct SyntaxTestAssertionRange {
    pub test_line_offset: usize,
    pub line_number: usize,
    pub begin_char: usize,
    pub end_char: usize,
    pub scope_selector: ScopeSelectors,
    pub scope_selector_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxTestHeader<'a> {
    pub testtoken_start: &'a str,
    pub testtoken_end: Option<&'a str>,
    pub syntax_file: &'a str,
}

pub fn parse_syntax_test_header_line(header_line: &str) -> Option<SyntaxTestHeader> { // TODO: use a "impl<'a> From<&'a str> for SyntaxTestHeader<'a>" instead?
    if let Some(pos) = &header_line.find(&" SYNTAX TEST \"") {
        let filename_part = &header_line[*pos + " SYNTAX TEST \"".len()..];
        if let Some(close_pos) = filename_part.find(&"\"") {
            let end_token = filename_part[close_pos + 1..].trim();
            Some(SyntaxTestHeader {
                testtoken_start: &header_line[0..*pos],
                testtoken_end: if end_token.len() == 0 { None } else { Some(end_token) },
                syntax_file: &filename_part[0..close_pos],
            })
        } else {
            None
        }
    } else {
        None
    }
}

/// Given a start token, option end token and text, parse the syntax tests in the text
/// that follow the format described at http://www.sublimetext.com/docs/3/syntax.html#testing
/// and return the scope selector assertions found, so that when the text is parsed,
/// the assertions can be checked
pub fn get_syntax_test_assertions(token_start: &str, token_end: Option<&str>, text: &str) -> Vec<SyntaxTestAssertionRange> {
    let mut assertions = Vec::new();
    let mut test_line_offset = 0;
    let mut test_line_len = 0;
    let mut line_number = 0;
    let mut offset = 0;

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

                let mut assertion = SyntaxTestAssertionRange {
                    test_line_offset: test_line_offset,
                    line_number: line_number,
                    begin_char: index + if assertion_range > 0 { token_start.len() + assertion_index } else { 0 },
                    end_char: index + if assertion_range > 0 { token_start.len() + assertion_index + assertion_range } else { 1 },

                    // format the scope selector to include a space at the beginning, because, currently, ScopeSelector expects excludes to begin with " -"
                    // and they are sometimes in the syntax test as ^^^-comment, for example
                    scope_selector: ScopeSelectors::from_str(&format!(" {}", &selector_text)).expect(&format!("Scope selector invalid on line {}", line_number)),
                    scope_selector_text: selector_text,
                };
                // if the assertion spans over the line being tested
                if assertion.end_char > test_line_len {
                    // calculate where on the next line the assertions will occur
                    let remainder = SyntaxTestAssertionRange {
                        test_line_offset: test_line_offset + test_line_len,
                        line_number: line_number,
                        begin_char: 0,
                        end_char: assertion.end_char - test_line_len,
                        scope_selector: assertion.scope_selector.clone(),
                        scope_selector_text: assertion.scope_selector_text.clone(),
                    };
                    assertion.end_char = test_line_len;
                    assertions.push(assertion);
                    assertions.push(remainder);
                } else {
                    assertions.push(assertion);
                }

                line_has_assertions = true;
            }
        }
        if !line_has_assertions { // ST seems to ignore lines that have assertions when calculating which line the assertion tests against, regardless of whether they contain any other text
            test_line_offset = offset;
            test_line_len = line.len() + 1;
        }
        offset += line.len() + 1; // the +1 is for the `\n`. TODO: maybe better to loop over the lines including the newline chars, using https://stackoverflow.com/a/40457615/4473405
    }
    assertions
}

/// Process the syntax test assertions in the given text, for the given syntax definition, using the test token(s) specified.
/// It works by finding all the syntax test assertions, then parsing the text line by line. If the line has some assertions against it, those are checked.
/// Assertions are counted according to their status - succeeded or failed. Failures are also logged to stdout, depending on the output options.
/// When there are no assertions left to check, it returns those counts.
/// `text` is the code containing syntax test assertions to be parsed and checked.
/// `testtoken_start` is the token (normally a comment in the given syntax) that represents that assertions could be on the line.
/// `testtoken_end` is an optional token that will be stripped from the line when retrieving the scope selector. Useful for syntaxes when the start token represents a block comment, to make the tests easier to construct.
pub/*(crate)*/ fn process_syntax_test_assertions(syntax_set: &SyntaxSet, syntax: &SyntaxReference, text: &str, testtoken_start: &str, testtoken_end: Option<&str>, out_opts: &SyntaxTestOutputOptions) -> SyntaxTestFileResult {
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

    let assertions = get_syntax_test_assertions(testtoken_start, testtoken_end, &text);
    //println!("{:?}", assertions);

    // iterate over the lines of the file, testing them
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();

    let mut offset = 0;
    let mut scopes_on_line_being_tested = Vec::new();
    let mut line_number = 0;
    let mut relevant_assertions = Vec::new();
    let mut assertion_index = 0;

    let mut assertion_failures: usize = 0;
    let mut total_assertions: usize = 0;

    for line_without_char in text.lines() {
        let line = &(line_without_char.to_owned() + "\n");
        line_number += 1;

        let eol_offset = offset + line.len();

        // parse the line
        let ops = state.parse_line(&line, &syntax_set);
        // find all the assertions that relate to the current line
        relevant_assertions.clear();
        while assertion_index < assertions.len() {
            let assertion = &assertions[assertion_index];
            let pos = assertion.test_line_offset + assertion.begin_char;
            if pos >= offset && pos < eol_offset {
                relevant_assertions.push(assertion);
                assertion_index += 1;
            } else {
                break;
            }
        }
        if !relevant_assertions.is_empty() {
            // if there are assertions for the line, show the operations for debugging purposes if specified in the output options
            // (if there are no assertions, or the line contains assertions, debugging it would probably just add noise)
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
                // if there are assertions against this line, store the scopes for comparison with the assertions
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
                    if !out_opts.summary {
                        let text: String = line.chars().skip(result.column_begin).take(length).collect();
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
        
        // no point continuing to parse the file if there are no syntax test assertions left
        // (unless we want to prove that no panics etc. occur while parsing the rest of the file ofc...)
        if assertion_index == assertions.len() || (assertion_failures > 0 && out_opts.failfast) {
            // NOTE: the total counts only really show how many assertions were checked when failing fast
            //       - they are not accurate total counts
            break;
        }
    }

    let res = if assertion_failures > 0 {
        SyntaxTestFileResult::FailedAssertions(assertion_failures, total_assertions)
    } else {
        SyntaxTestFileResult::Success(total_assertions)
    };
    res
}

// #[cfg(test)]
// mod tests {
    #[test]
    fn can_find_test_assertions() {
        let text = "\
            hello world\n\
            // <- assertion1\n\
            // ^^ assertion2\n\
            \n\
            foobar\n\
            //    ^ - assertion3\n\
            ";
        let result = get_syntax_test_assertions(&"//", None, &text);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].line_number, 2);
        assert_eq!(result[1].line_number, 3);
        assert_eq!(result[2].line_number, 6);
        assert_eq!(result[0].test_line_offset, result[1].test_line_offset);
        assert_eq!(result[2].test_line_offset, text.find("foobar").unwrap());
        assert_eq!(result[0].scope_selector_text, " assertion1");
        assert_eq!(result[1].scope_selector_text, " assertion2");
        assert_eq!(result[2].scope_selector_text, " - assertion3");
        assert_eq!(result[0].begin_char, 0);
        assert_eq!(result[0].end_char, 1);
        assert_eq!(result[1].begin_char, 3);
        assert_eq!(result[1].end_char, 5);
        assert_eq!(result[2].begin_char, 6);
        assert_eq!(result[2].end_char, 7);
    }

    #[test]
    fn can_find_test_assertions_with_end_tokens() {
        let text = "
hello world
 <!-- <- assertion1 -->
<!--  ^^assertion2

foobar
<!-- ^ - assertion3 -->
";
        let result = get_syntax_test_assertions(&"<!--", Some(&"-->"), &text);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].line_number, 3);
        assert_eq!(result[1].line_number, 4);
        assert_eq!(result[2].line_number, 7);
        assert_eq!(result[0].test_line_offset, result[1].test_line_offset);
        assert_eq!(result[2].test_line_offset, text.find("foobar").unwrap());
        assert_eq!(result[0].scope_selector_text, " assertion1 ");
        assert_eq!(result[1].scope_selector_text, "assertion2");
        assert_eq!(result[2].scope_selector_text, " - assertion3 ");
        assert_eq!(result[0].begin_char, 1);
        assert_eq!(result[0].end_char, 2);
        assert_eq!(result[1].begin_char, 6);
        assert_eq!(result[1].end_char, 8);
        assert_eq!(result[2].begin_char, 5);
        assert_eq!(result[2].end_char, 6);
    }

    #[test]
    fn can_find_test_assertions_that_spans_lines() {
        let text = "
hello world
<!--  ^^^^^^^^^ assertion1
<!--    ^^^^^^^ assertion2 -->
foobar
<!-- ^^^ -assertion3-->
";
        let result = get_syntax_test_assertions(&"<!--", Some(&"-->"), &text);
        println!("{:?}", result);

        assert_eq!(result.len(), 6);
        assert_eq!(result[0].line_number, 3);
        assert_eq!(result[1].line_number, 3);
        assert_eq!(result[2].line_number, 4);
        assert_eq!(result[3].line_number, 4);
        assert_eq!(result[4].line_number, 6);
        assert_eq!(result[5].line_number, 6);
        assert_eq!(result[0].scope_selector_text, " assertion1");
        assert_eq!(result[1].scope_selector_text, " assertion1");
        assert_eq!(result[2].scope_selector_text, " assertion2 ");
        assert_eq!(result[3].scope_selector_text, " assertion2 ");
        assert_eq!(result[4].scope_selector_text, " -assertion3");
        assert_eq!(result[5].scope_selector_text, " -assertion3");
        assert_eq!(result[0].begin_char, 6);
        assert_eq!(result[0].end_char, 12);
        assert_eq!(result[0].test_line_offset, 1);
        assert_eq!(result[1].begin_char, 0);
        assert_eq!(result[1].end_char, 3);
        assert_eq!(result[1].test_line_offset, "\nhello world\n".len());

        assert_eq!(result[2].begin_char, 8);
        assert_eq!(result[2].end_char, 12);
        assert_eq!(result[2].test_line_offset, 1);
        assert_eq!(result[3].begin_char, 0);
        assert_eq!(result[3].end_char, 3);
        assert_eq!(result[3].test_line_offset, "\nhello world\n".len());

        assert_eq!(result[4].begin_char, 5);
        assert_eq!(result[4].end_char, 7);
        assert_eq!(result[5].begin_char, 0);
        assert_eq!(result[5].end_char, 1);
    }

    #[test]
    fn can_parse_syntax_test_header_with_end_token() {
        let header = parse_syntax_test_header_line(&"<!-- SYNTAX TEST \"XML.sublime-syntax\" -->").unwrap();
        assert_eq!(&header.testtoken_start, &"<!--");
        assert_eq!(&header.testtoken_end.unwrap(), &"-->");
        assert_eq!(&header.syntax_file, &"XML.sublime-syntax");
    }

    #[test]
    fn can_parse_syntax_test_header_with_end_token_and_carriage_return() {
        let header = parse_syntax_test_header_line(&"<!-- SYNTAX TEST \"XML.sublime-syntax\" -->\r\n").unwrap();
        assert_eq!(&header.testtoken_start, &"<!--");
        assert_eq!(&header.testtoken_end.unwrap(), &"-->");
        assert_eq!(&header.syntax_file, &"XML.sublime-syntax");
    }

    #[test]
    fn can_parse_syntax_test_header_with_no_end_token() {
        let header = parse_syntax_test_header_line(&"// SYNTAX TEST \"Packages/Example/Example.sublime-syntax\"\n").unwrap();
        assert_eq!(&header.testtoken_start, &"//");
        assert!(!header.testtoken_end.is_some());
        assert_eq!(&header.syntax_file, &"Packages/Example/Example.sublime-syntax");
    }

    #[test]
    fn can_parse_syntax_test_header_with_no_end_token_and_carriage_return() {
        let header = parse_syntax_test_header_line(&"// SYNTAX TEST \"Packages/Example/Example.sublime-syntax\"\r").unwrap();
        assert_eq!(&header.testtoken_start, &"//");
        assert!(header.testtoken_end.is_none());
        assert_eq!(&header.syntax_file, &"Packages/Example/Example.sublime-syntax");
    }
// }
