//! An example of using syntect for testing syntax definitions.
//! Basically exactly the same as what Sublime Text can do,
//! but without needing ST installed
extern crate syntect;
extern crate walkdir;
#[macro_use]
extern crate lazy_static;
extern crate regex;
//extern crate onig;
use syntect::parsing::{SyntaxSet, ParseState, ScopeStack, Scope};
use syntect::highlighting::ScopeSelectors;
use syntect::easy::{ScopeRegionIterator};

use std::path::Path;
use std::io::{BufRead, BufReader};
use std::fs::File;
use std::cmp::{min, max};
use walkdir::{DirEntry, WalkDir, WalkDirIterator};
use std::str::FromStr;
use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxTestHeaderError {
    MalformedHeader,
    SyntaxDefinitionNotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxTestFileResult {
    FailedAssertions(usize, usize),
    Success(usize),
}

lazy_static! {
    pub static ref SYNTAX_TEST_HEADER_PATTERN: Regex = Regex::new(r#"(?xm)
            ^(?P<testtoken_start>\s*\S+)
            \s+SYNTAX\sTEST\s+
            "(?P<syntax_file>[^"]+)"
            \s*(?P<testtoken_end>\S+)?$
        "#).unwrap();
    pub static ref SYNTAX_TEST_ASSERTION_PATTERN: Regex = Regex::new(r#"(?xm)
        \s*(?:
            (?P<begin_of_token><-)|(?P<range>\^+)
        )(.+)$"#).unwrap();
}

#[derive(Debug)]
struct AssertionRange<'a> {
    begin_char: usize,
    end_char: usize,
    scope_selector_text: &'a str
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
}

fn get_line_assertion_details<'a>(testtoken_start: &str, testtoken_end: Option<&str>, line: &'a str) -> Option<AssertionRange<'a>> {
    // if the test start token specified in the test file's header is on the line
    if let Some(index) = line.find(testtoken_start) {
        let (before_token_start, token_and_rest_of_line) = line.split_at(index);
        if before_token_start.trim_left().is_empty() { // if only whitespace precedes the test token on the line
            if let Some(captures) = SYNTAX_TEST_ASSERTION_PATTERN.captures(&token_and_rest_of_line[testtoken_start.len()..]) {
                let mut sst = captures.get(3).unwrap().as_str().trim_right(); // get the scope selector text
                if let Some(token) = testtoken_end { // if there is an end token defined in the test file header
                    sst = sst.trim_right_matches(&token); // trim it from the scope selector text
                }
                return Some(AssertionRange {
                    begin_char: index + if captures.get(2).is_some() { testtoken_start.len() + captures.get(2).unwrap().start() } else { 0 },
                    end_char: index + if captures.get(2).is_some() { testtoken_start.len() + captures.get(2).unwrap().end() } else { 1 },
                    scope_selector_text: sst
                })
            }
        }
    }
    None
}

fn process_assertions(assertion: &AssertionRange, test_against_line_scopes: &Vec<ScopedText>) -> Vec<RangeTestResult> {
    let selector = ScopeSelectors::from_str(assertion.scope_selector_text).unwrap();
    // find the scope at the specified start column, and start matching the selector through the rest of the tokens on the line from there until the end column is reached
    let mut results = Vec::new();
    for scoped_text in test_against_line_scopes.iter().skip_while(|s|s.char_start + s.text_len <= assertion.begin_char).take_while(|s|s.char_start < assertion.end_char) {
        let match_value = selector.does_match(scoped_text.scope.as_slice());
        let result = RangeTestResult {
            column_begin: max(scoped_text.char_start, assertion.begin_char),
            column_end: min(scoped_text.char_start + scoped_text.text_len, assertion.end_char),
            success: match_value.is_some()
        };
        results.push(result);
    }
    // don't ignore assertions after the newline, they should be treated as though they are asserting against the newline
    let last = test_against_line_scopes.last().unwrap();
    if last.char_start + last.text_len < assertion.end_char {
        let match_value = selector.does_match(last.scope.as_slice());
        let result = RangeTestResult {
            column_begin: max(last.char_start + last.text_len, assertion.begin_char),
            column_end: assertion.end_char,
            success: match_value.is_some()
        };
        results.push(result);
    }
    results
}

fn test_file(ss: &SyntaxSet, path: &Path, parse_test_lines: bool) -> Result<SyntaxTestFileResult, SyntaxTestHeaderError> {
    let f = File::open(path).unwrap();
    let mut reader = BufReader::new(f);
    let mut line = String::new();
    
    // read the first line from the file - if we have reached EOF already, it's an invalid file
    if reader.read_line(&mut line).unwrap() == 0 {
        return Err(SyntaxTestHeaderError::MalformedHeader);
    }
    
    line = line.replace("\r", &"");
    
    // parse the syntax test header in the first line of the file
    match SYNTAX_TEST_HEADER_PATTERN.captures(&line.to_string()) {
        Some(captures) => {
            let testtoken_start = captures.name("testtoken_start").unwrap().as_str();
            let testtoken_end = captures.name("testtoken_end").map_or(None, |c|Some(c.as_str()));
            let syntax_file = captures.name("syntax_file").unwrap().as_str();
            
            // find the relevant syntax definition to parse the file with - case is important!
            println!("The test file references syntax definition file: {}", syntax_file);
            let syntax = match ss.find_syntax_by_path(syntax_file) {
                Some(syntax) => syntax,
                None => return Err(SyntaxTestHeaderError::SyntaxDefinitionNotFound)
            };
            
            let mut state = ParseState::new(syntax);
            let mut stack = ScopeStack::new();
            
            let mut current_line_number = 1;
            let mut test_against_line_number = 1;
            let mut scopes_on_line_being_tested = Vec::new();
            let mut previous_non_assertion_line = line.to_string();
            
            let mut assertion_failures: usize = 0;
            let mut total_assertions: usize = 0;
            
            loop {
                let mut line_has_assertion = false;
                if let Some(assertion) = get_line_assertion_details(testtoken_start, testtoken_end, &line) {
                    let result = process_assertions(&assertion, &scopes_on_line_being_tested);
                    total_assertions += &assertion.end_char - &assertion.begin_char;
                    for failure in result.iter().filter(|r|!r.success) {
                        let chars: Vec<char> = previous_non_assertion_line.chars().skip(failure.column_begin).take(failure.column_end - failure.column_begin).collect();
                        println!("  Assertion selector {:?} \
                            from line {:?} failed against line {:?}, column range {:?}-{:?} \
                            (with text {:?}) \
                            has scope {:?}",
                            assertion.scope_selector_text.trim(),
                            current_line_number, test_against_line_number, failure.column_begin, failure.column_end,
                            chars,
                            scopes_on_line_being_tested.iter().skip_while(|s|s.char_start + s.text_len <= failure.column_begin).next().unwrap_or(scopes_on_line_being_tested.last().unwrap()).scope
                        );
                        assertion_failures += failure.column_end - failure.column_begin;
                    }
                    line_has_assertion = true;
                }
                if !line_has_assertion || parse_test_lines {
                    if !line_has_assertion {
                        scopes_on_line_being_tested.clear();
                        test_against_line_number = current_line_number;
                        previous_non_assertion_line = line.to_string();
                    }
                    let ops = state.parse_line(&line);
                    let mut col: usize = 0;
                    for (s, op) in ScopeRegionIterator::new(&ops, &line) {
                        stack.apply(op);
                        if s.is_empty() { // in this case we don't care about blank tokens
                            continue;
                        } else if !line_has_assertion {
                            // if the line has no assertions on it, remember the scopes on the line so we can test against them later
                            let len = s.chars().count();
                            scopes_on_line_being_tested.push(
                                ScopedText {
                                    char_start: col,
                                    text_len: len,
                                    scope: stack.as_slice().to_vec()
                                }
                            );
                            // TODO: warn when there are duplicate adjacent (non-meta?) scopes, as it is almost always undesired
                            col += len;
                        }
                    }
                }
                
                line.clear();
                current_line_number += 1;
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                line = line.replace("\r", &"");
            }
            Ok(
                if assertion_failures > 0 {
                    SyntaxTestFileResult::FailedAssertions(assertion_failures, total_assertions)
                } else {
                    SyntaxTestFileResult::Success(total_assertions)
                }
            )
        },
        None => Err(SyntaxTestHeaderError::MalformedHeader)
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let tests_path = if args.len() < 2 {
        "."
    } else {
        &args[1]
    };
    let syntaxes_path = if args.len() == 3 {
        &args[2]
    } else {
        ""
    };

    // load the syntaxes from disk if told to
    // (as opposed to from the binary dumps)
    // this helps to ensure that a recompile isn't needed
    // when using this for syntax development
    let mut ss = if syntaxes_path.is_empty() {
        SyntaxSet::load_defaults_newlines() // note we load the version with newlines
    } else {
        SyntaxSet::new()
    };
    if !syntaxes_path.is_empty() {
        println!("loading syntax definitions from {}", syntaxes_path);
        ss.load_syntaxes(&syntaxes_path, true).unwrap(); // note that we load the version with newlines
        ss.link_syntaxes();
    }

    let exit_code = recursive_walk(&ss, &tests_path);
    println!("exiting with code {}", exit_code);
    std::process::exit(exit_code);

}


fn recursive_walk(ss: &SyntaxSet, path: &str) -> i32 {
    let mut exit_code: i32 = 0; // exit with code 0 by default, if all tests pass
    let walker = WalkDir::new(path).into_iter();
    for entry in walker.filter_entry(|e|e.file_type().is_dir() || is_a_syntax_test_file(e)) {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            println!("Testing file {}", entry.path().display());
            let result = test_file(&ss, entry.path(), true);
            println!("{:?}", result);
            if exit_code != 2 { // leave exit code 2 if there was an error
                if let Err(_) = result { // set exit code 2 if there was an error
                    exit_code = 2;
                } else if let Ok(ok) = result {
                    if let SyntaxTestFileResult::FailedAssertions(_, _) = ok {
                        exit_code = 1; // otherwise, if there were failures, exit with code 1
                    }
                }
            }
        }
    }
    exit_code
}

fn is_a_syntax_test_file(entry: &DirEntry) -> bool {
    entry.file_name()
         .to_str()
         .map(|s| s.starts_with("syntax_test_"))
         .unwrap_or(false)
}
