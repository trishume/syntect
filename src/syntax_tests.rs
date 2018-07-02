//! API for running syntax tests.
//! See http://www.sublimetext.com/docs/3/syntax.html#testing

use parsing::{ScopeStack, ParseState, SyntaxDefinition, Scope};
//use std::io::Write;
use std::str::FromStr;
use util::debug_print_ops;
use easy::{ScopeRegionIterator};

#[derive(Clone, Copy)]
pub struct SyntaxTestOutputOptions {
    pub time: bool,
    pub debug: bool,
    pub summary: bool,
    //pub output: &'a Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxTestFileResult {
    FailedAssertions(usize, usize),
    Success(usize),
}

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

/// Process the syntax test assertions in the given text, for the given syntax definition, using the test token(s) specified.
/// `text` is the code containing syntax test assertions to be parsed and checked.
/// `testtoken_start` is the token (normally a comment in the given syntax) that represents that assertions could be on the line.
/// `testtoken_end` is an optional token that will be stripped from the line when retrieving the scope selector. Useful for syntaxes when the start token represents a block comment, to make the tests easier to construct.
pub fn process_syntax_test_assertions(syntax: &SyntaxDefinition, text: &str, testtoken_start: &str, testtoken_end: Option<&str>, out_opts: &SyntaxTestOutputOptions) -> SyntaxTestFileResult {
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

// #[cfg(test)]
// mod tests {
    #[test]
    fn can_find_test_assertions() {
        let result = get_syntax_test_assertions(&"//", None,
            "
            hello world
            // <- assertion1
            // ^^ assertion2
            
            foobar
            //    ^ - assertion3
            ");
        
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].line_number, 3);
        assert_eq!(result[1].line_number, 4);
        assert_eq!(result[2].line_number, 7);
        assert_eq!(result[0].test_line_offset, result[1].test_line_offset);
        assert!(result[2].test_line_offset > result[0].test_line_offset);
    }
// }
