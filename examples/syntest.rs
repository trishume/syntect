//! An example of using syntect for testing syntax definitions.
//! Basically exactly the same as what Sublime Text can do,
//! but without needing ST installed
// To run tests only for a particular package, while showing the operations, you could use:
// cargo run --example syntest -- --debug testdata/Packages/Makefile/
// to specify that the syntax definitions should be parsed instead of loaded from the dump file,
// you can tell it where to parse them from - the following will execute only 1 syntax test after
// parsing the sublime-syntax files in the JavaScript folder:
// cargo run --example syntest testdata/Packages/JavaScript/syntax_test_json.json testdata/Packages/JavaScript/

use syntect::easy::ScopeRegionIterator;
use syntect::highlighting::ScopeSelectors;
use syntect::parsing::{
    ParseLineOutput, ParseState, Scope, ScopeStack, SyntaxSet, SyntaxSetBuilder,
};

use std::cmp::{max, min};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::str::FromStr;
use std::time::Instant;

use getopts::Options;
use regex::Regex;
use std::sync::LazyLock;
use walkdir::{DirEntry, WalkDir};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxTestHeaderError {
    MalformedHeader,
    SyntaxDefinitionNotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxTestFileResult {
    FailedAssertions(usize, usize),
    Success(usize),
    /// File declares a non-scope Sublime test variant (reindent,
    /// reindent-unchanged, partial-symbols, …) — captured as the
    /// modifier between `SYNTAX TEST` and the quoted syntax file.
    /// syntect doesn't implement those test kinds, so the file is
    /// skipped without contributing to failures or exit status.
    Skipped(String),
}

// The header-line shape Sublime Text accepts is:
//   <testtoken_start> SYNTAX TEST [<modifier>] "<syntax_file>" [<testtoken_end>]
//
// * `testtoken_start` is the line's comment marker (e.g. `//`, `#`,
//   `/*`, `<!--`, `T:` for MultiMarkdown). Matched lazily so
//   zero-whitespace separators like `T:SYNTAX TEST …` work; without
//   laziness a greedy `\S+` would swallow `T:SYNTAX` and never find
//   the literal `SYNTAX`.
// * Optional `modifier` (`reindent`, `reindent-unchanged`,
//   `partial-symbols`, future variants) sits between `SYNTAX TEST`
//   and the quoted path. Capturing is unrestricted — any non-quote
//   non-whitespace token counts — so we don't need to track
//   Sublime's evolving modifier list here. A present modifier is
//   the signal that the file is not a scope test (see
//   `SyntaxTestFileResult::Skipped`).
// * `testtoken_end` is whatever follows the closing quote, with
//   surrounding whitespace stripped. For block-comment syntaxes it
//   is the matching closing marker (`*/`, `-->`, `%>`, `}}`, …);
//   for line-comment shebang headers it is the entire trailing tail
//   (`dmd`, `clojure`, `dotnet run`, `tclsh8.6`, …) — alphabetic
//   and multi-word tails included. ST searches each assertion line
//   for this token and clips the scope-selector text at the first
//   substring match, so capturing the full tail is required to keep
//   selectors like `<- keyword.operator.logical.d dmd` from leaking
//   the trailing `dmd` into the selector.
pub static SYNTAX_TEST_HEADER_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?xm)
        ^(?P<testtoken_start>\s*\S+?)
        \s*SYNTAX\sTEST\s+
        (?:(?P<modifier>[^\s"]+)\s+)?
        "(?P<syntax_file>[^"]+)"
        (?:\s+(?P<testtoken_end>\S(?:.*\S)?))?
        \s*$
    "#,
    )
    .unwrap()
});
// Recognises the four syntax-test annotation markers used by Sublime Text:
//   `<-`  start-of-line scope assertion
//   `^+`  column-range scope assertion
//   `@+`  reference label (names columns on the line above)
//   `>`   reference-based scope assertion against a previously-named label
// The first two carry scope selectors and are checked against the parser's
// output; the last two exist so the parser can support cross-line references
// but currently act as annotation-only markers (no scope check is performed).
// Recognising them here ensures they do not advance `test_against_line_number`
// in the main loop — without this, a line like `//  @@@ definition` is treated
// as plain source and subsequent `^` assertions get mapped to it instead of
// the previous real source line, producing spurious failures whose reported
// scope is always the test-annotation prefix's comment scope.
pub static SYNTAX_TEST_ASSERTION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // `^` anchors at the start of the post-`testtoken_start` substring so
    // that plain-text occurrences of the marker characters deeper in the
    // line cannot trigger a match (e.g. the `>` of `-->` in the Textile
    // header `<!-- SYNTAX TEST "..." -->`).
    //
    // The reference-label (`@+`) and reference-assertion (`>`) alternatives
    // also require that the marker be followed by whitespace or end-of-line;
    // that check is done in Rust code after the regex match (the `regex`
    // crate in use here does not support look-ahead). Without it, `@` and
    // `>` inside real source (Ruby `#@var`, JSDoc `// @param`, generics
    // `<T>`) would mis-classify those lines as syntax-test annotations.
    Regex::new(
        r#"(?xm)
    ^\s*(?:
        (?P<begin_of_token><-)
        | (?P<range>\^+)
        | (?P<reference_label>@+)
        | (?P<reference_assertion>>)
    )(.*)$"#,
    )
    .unwrap()
});

#[derive(Clone, Copy)]
struct OutputOptions {
    time: bool,
    debug: bool,
    summary: bool,
}

#[derive(Debug)]
struct AssertionRange<'a> {
    begin_char: usize,
    end_char: usize,
    scope_selector_text: &'a str,
    is_pure_assertion_line: bool,
    /// True for `@+` label definitions and `>` reference-based assertions.
    /// These annotation-only markers do not drive scope checks here but must
    /// still be recognised so they are not mistaken for source-code lines.
    is_reference: bool,
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

/// A failure message buffered until the parser commits (leaves speculative state).
struct BufferedFailureMessage {
    selector_text: String,
    assertion_line_number: usize,
    test_against_line_number: usize,
    column_begin: usize,
    column_end: usize,
    text: String,
    scope: Vec<Scope>,
}

fn print_failure(msg: &BufferedFailureMessage) {
    println!(
        "  Assertion selector {:?} \
        from line {:?} failed against line {:?}, column range {:?}-{:?} \
        (with text {:?}) \
        has scope {:?}",
        msg.selector_text,
        msg.assertion_line_number,
        msg.test_against_line_number,
        msg.column_begin,
        msg.column_end,
        msg.text,
        msg.scope,
    );
}

fn flush_pending_messages(messages: &mut Vec<BufferedFailureMessage>, summary: bool) {
    if !summary {
        for msg in messages.iter() {
            print_failure(msg);
        }
    }
    messages.clear();
}

/// Assertion details buffered for potential re-evaluation after cross-line backtracking.
struct BufferedAssertion {
    begin_char: usize,
    end_char: usize,
    scope_selector_text: String,
    assertion_line_number: usize,
}

/// Tracks state for a non-assertion line so its assertions can be re-evaluated
/// if cross-line `fail` replays corrected ops.
struct NonAssertionData {
    assertions: Vec<BufferedAssertion>,
    assertion_failures: usize,
}

/// Record of a line that was sent to `parse_line`, kept for replay handling.
struct ParsedLineRecord {
    line_text: String,
    line_number: usize,
    stack_before: ScopeStack,
    /// Present only for non-assertion lines.
    non_assertion_data: Option<NonAssertionData>,
}

fn get_line_assertion_details<'a>(
    testtoken_start: &str,
    testtoken_end: Option<&str>,
    line: &'a str,
) -> Option<AssertionRange<'a>> {
    // if the test start token specified in the test file's header is on the line
    if let Some(index) = line.find(testtoken_start) {
        let (before_token_start, token_and_rest_of_line) = line.split_at(index);

        if let Some(captures) =
            SYNTAX_TEST_ASSERTION_PATTERN.captures(&token_and_rest_of_line[testtoken_start.len()..])
        {
            // Post-match validation for `@+` and `>` alternatives: the marker
            // must be followed by whitespace or end-of-line. See the pattern's
            // comment for why this isn't encoded in the regex itself.
            let marker_match = captures
                .name("reference_label")
                .or_else(|| captures.name("reference_assertion"));
            if let Some(m) = marker_match {
                let after = &token_and_rest_of_line[testtoken_start.len() + m.end()..];
                if !after.is_empty() && !after.chars().next().unwrap().is_whitespace() {
                    return None;
                }
            }
            // The trailing `(.*)$` group comes after the four named alternatives,
            // so use the last group to stay robust against future marker additions.
            let mut sst = captures.get(captures.len() - 1).unwrap().as_str();

            if let Some(token) = testtoken_end {
                // ST clips the scope-selector text at the first occurrence of
                // testtoken_end (substring match) — necessary so block-comment
                // closers (`*/`, `-->`) and shebang tails (`dmd`, `clojure`,
                // `dotnet run`) don't leak into the selector. Note this fires
                // even when the token is a substring of a scope name (the
                // `clojure` inside `comment.line.shebang.clojure` for the
                // Clojure shebang test); ST relies on the test author not
                // picking selectors whose first dotted atom shares a prefix
                // with the closing token.
                if let Some(end_token_pos) = sst.find(token) {
                    let (ss, _) = sst.split_at(end_token_pos);
                    sst = ss;
                }
            }
            // A column-range marker (`^+` or `@+`) spans the columns it covers;
            // `<-` and `>` are logically anchored at the start of the line above.
            let range_match = captures
                .name("range")
                .or_else(|| captures.name("reference_label"));
            let (begin_char, end_char) = if let Some(m) = range_match {
                (
                    index + testtoken_start.len() + m.start(),
                    index + testtoken_start.len() + m.end(),
                )
            } else {
                (index, index + 1)
            };
            let is_reference = captures.name("reference_label").is_some()
                || captures.name("reference_assertion").is_some();
            // ST's syntax-test format requires assertions on dedicated
            // comment-only lines: only whitespace before testtoken_start.
            // When source code precedes the testtoken (e.g. bash `: ${#^pat}`,
            // where `#` is the parameter-length operator and `^pat` is
            // substitution), the markers that follow are coincidental code,
            // not test-author intent. Such lines must not be treated as
            // assertion-bearing — otherwise spurious `process_assertions`
            // failures pile up against the wrong target line, and the
            // misclassification also blocks `test_against_line_number` from
            // advancing onto the source line, so the *next* genuine
            // assertion tests against stale scopes. We don't gate on what
            // follows testtoken_end: ST itself processes such lines as
            // assertions (the trailing content is just ignored), and our
            // tests don't include lines that mix an assertion with real
            // source code after the closing token.
            let is_pure_assertion_line = before_token_start.trim_start().is_empty();
            if !is_pure_assertion_line {
                return None;
            }
            return Some(AssertionRange {
                begin_char,
                end_char,
                scope_selector_text: sst,
                is_pure_assertion_line,
                is_reference,
            });
        }
    }
    None
}

fn process_assertions(
    assertion: &AssertionRange<'_>,
    test_against_line_scopes: &[ScopedText],
    next_line_scopes: Option<&[ScopedText]>,
) -> Vec<RangeTestResult> {
    // format the scope selector to include a space at the beginning, because, currently, ScopeSelector expects excludes to begin with " -"
    // and they are sometimes in the syntax test as ^^^-comment, for example
    let selector =
        ScopeSelectors::from_str(&format!(" {}", &assertion.scope_selector_text)).unwrap();
    // find the scope at the specified start column, and start matching the selector through the rest of the tokens on the line from there until the end column is reached
    let mut results = Vec::new();
    for scoped_text in test_against_line_scopes
        .iter()
        .skip_while(|s| s.char_start + s.text_len <= assertion.begin_char)
        .take_while(|s| s.char_start < assertion.end_char)
    {
        let match_value = selector.does_match(scoped_text.scope.as_slice());
        let result = RangeTestResult {
            column_begin: max(scoped_text.char_start, assertion.begin_char),
            column_end: min(
                scoped_text.char_start + scoped_text.text_len,
                assertion.end_char,
            ),
            success: match_value.is_some(),
        };
        results.push(result);
    }
    // Past-EOL columns: ST's `view.text_point(row, col)` overflows into the
    // next row when `col` exceeds the current line's char count, so its
    // syntax-test framework evaluates past-EOL assertions against the
    // corresponding column on the line below. Mirror that here when we have
    // the next line's scopes; otherwise fall back to the last char's scope.
    let last = test_against_line_scopes.last().unwrap();
    let last_end = last.char_start + last.text_len;
    if last_end < assertion.end_char {
        let past_eol_begin = max(last_end, assertion.begin_char);
        let past_eol_end = assertion.end_char;
        if let Some(next_scopes) = next_line_scopes.filter(|s| !s.is_empty()) {
            // Wrap formula: position `col` past EOL of a line of total length
            // `last_end` (chars including trailing `\n`) lands on column
            // `col - last_end` of the next line.
            let wrap_begin = past_eol_begin - last_end;
            let wrap_end = past_eol_end - last_end;
            let mut covered = wrap_begin;
            for scoped_text in next_scopes
                .iter()
                .skip_while(|s| s.char_start + s.text_len <= wrap_begin)
                .take_while(|s| s.char_start < wrap_end)
            {
                let next_begin = max(scoped_text.char_start, wrap_begin);
                let next_end = min(scoped_text.char_start + scoped_text.text_len, wrap_end);
                let match_value = selector.does_match(scoped_text.scope.as_slice());
                results.push(RangeTestResult {
                    column_begin: next_begin + last_end,
                    column_end: next_end + last_end,
                    success: match_value.is_some(),
                });
                covered = next_end;
            }
            // Wrap target extends past the next line's content too — recursive
            // wrap is not yet implemented; fall back to the next line's last
            // scope so the assertion still gets a defined verdict instead of
            // silently passing.
            if covered < wrap_end {
                let next_last = next_scopes.last().unwrap();
                let match_value = selector.does_match(next_last.scope.as_slice());
                results.push(RangeTestResult {
                    column_begin: covered + last_end,
                    column_end: past_eol_end,
                    success: match_value.is_some(),
                });
            }
        } else {
            let match_value = selector.does_match(last.scope.as_slice());
            results.push(RangeTestResult {
                column_begin: past_eol_begin,
                column_end: past_eol_end,
                success: match_value.is_some(),
            });
        }
    }
    results
}

/// If `parse_test_lines` is `false` then lines that only contain assertions are not parsed
fn test_file(
    ss: &SyntaxSet,
    path: &Path,
    parse_test_lines: bool,
    out_opts: OutputOptions,
) -> Result<SyntaxTestFileResult, SyntaxTestHeaderError> {
    use syntect::util::debug_print_ops;
    let f = File::open(path).unwrap();
    let mut reader = BufReader::new(f);
    let mut line = String::new();

    // read the first line from the file - if we have reached EOF already, it's an invalid file
    if reader.read_line(&mut line).unwrap() == 0 {
        return Err(SyntaxTestHeaderError::MalformedHeader);
    }

    line = line.replace('\r', "");

    // parse the syntax test header in the first line of the file
    let header_line = line.clone();
    let search_result = SYNTAX_TEST_HEADER_PATTERN.captures(&header_line);
    let captures = search_result.ok_or(SyntaxTestHeaderError::MalformedHeader)?;

    let testtoken_start = captures.name("testtoken_start").unwrap().as_str();
    let testtoken_end = captures.name("testtoken_end").map(|c| c.as_str());
    let syntax_file = captures.name("syntax_file").unwrap().as_str();

    // Non-scope Sublime test variants (reindent, reindent-unchanged,
    // partial-symbols, …) carry a modifier between `SYNTAX TEST` and
    // the quoted syntax file. syntect doesn't implement those kinds
    // of tests, so bow out before touching the parser.
    if let Some(modifier) = captures.name("modifier").map(|c| c.as_str()) {
        return Ok(SyntaxTestFileResult::Skipped(modifier.to_owned()));
    }

    // find the relevant syntax definition to parse the file with - case is important!
    if !out_opts.summary {
        println!(
            "The test file references syntax definition file: {}",
            syntax_file
        );
    }
    let syntax = ss
        .find_syntax_by_path(syntax_file)
        .ok_or(SyntaxTestHeaderError::SyntaxDefinitionNotFound)?;

    // iterate over the lines of the file, testing them
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();

    let mut current_line_number = 1;
    let mut test_against_line_number = 1;
    let mut scopes_on_line_being_tested: Vec<ScopedText> = Vec::new();
    // Scopes of the first line that follows the current target line. ST's
    // syntax-test framework evaluates past-EOL assertion columns against the
    // corresponding column on the next line (because `text_point(row, col)`
    // overflows into the next row when `col` exceeds the row's length); we
    // mirror that by remembering the next line's scopes once and feeding
    // them into `process_assertions`. Reset whenever the target line changes.
    let mut next_target_line_scopes: Option<Vec<ScopedText>> = None;
    let mut previous_non_assertion_line = line.to_string();

    let mut assertion_failures: usize = 0;
    let mut total_assertions: usize = 0;

    // Buffer for handling cross-line backtracking (replayed ops)
    let mut parsed_line_buffer: Vec<ParsedLineRecord> = Vec::new();
    let mut current_test_line_buffer_idx: Option<usize> = None;

    // Failure messages deferred until the parser commits (exits speculative state)
    let mut pending_messages: Vec<BufferedFailureMessage> = Vec::new();

    loop {
        // over lines of file, starting with the header line
        let assertion_opt = get_line_assertion_details(testtoken_start, testtoken_end, &line);
        let line_has_assertion = assertion_opt.is_some();
        let line_only_has_assertion = assertion_opt
            .as_ref()
            .map(|a| a.is_pure_assertion_line)
            .unwrap_or(false);

        // Parse first so the just-parsed scopes are available when the
        // assertion runs immediately after — needed for past-EOL wrap on the
        // first assertion line after a target.
        if !line_only_has_assertion || parse_test_lines {
            if !line_has_assertion {
                // ST seems to ignore lines that have assertions when calculating which line the assertion tests against
                scopes_on_line_being_tested.clear();
                next_target_line_scopes = None;
                test_against_line_number = current_line_number;
                previous_non_assertion_line = line.to_string();
            }
            if out_opts.debug && !line_only_has_assertion {
                println!(
                    "-- debugging line {} -- scope stack: {:?}, -- parse state: {:?}",
                    current_line_number, stack, state
                );
            }
            let output = match state.parse_line(&line, ss) {
                Ok(output) => output,
                Err(e) => {
                    if !out_opts.summary {
                        eprintln!("  Parse error on line {}: {}", current_line_number, e);
                    }
                    // Treat parse errors as total test failure
                    return Ok(SyntaxTestFileResult::FailedAssertions(
                        total_assertions.max(1),
                        total_assertions.max(1),
                    ));
                }
            };
            let ParseLineOutput {
                ops,
                replayed,
                warnings,
            } = output;

            for warning in &warnings {
                eprintln!("Warning: {}", warning);
            }

            // Handle cross-line backtracking: when `replayed` is non-empty, the
            // parser has corrected ops for previously-parsed lines. Re-evaluate
            // any assertions that were tested against those lines.
            if !replayed.is_empty() {
                if out_opts.debug {
                    println!(
                        "  replayed {} line(s) due to cross-line backtracking",
                        replayed.len()
                    );
                }
                let buf_len = parsed_line_buffer.len();
                let start_idx = buf_len - replayed.len();

                // Collect replayed line numbers for pruning pending messages
                let replayed_line_numbers: Vec<usize> = (start_idx..buf_len)
                    .map(|i| parsed_line_buffer[i].line_number)
                    .collect();
                // Remove pending messages whose test_against_line_number
                // matches any replayed line — they will be regenerated below
                pending_messages
                    .retain(|m| !replayed_line_numbers.contains(&m.test_against_line_number));

                // Reset stack to the state before the first replayed line
                stack = parsed_line_buffer[start_idx].stack_before.clone();

                // Collect corrected baselines to apply post-loop, since the
                // mutable record borrow inside the loop forbids touching
                // sibling buffer entries. After applying replayed[i],
                // `stack` is the corrected end-of-line for the buffered
                // record at start_idx + i, i.e. the corrected
                // start-of-line baseline for record at start_idx + i + 1.
                // Overwriting that baseline prevents a future replay from
                // resurrecting any meta_scope the prior replay had unwound
                // (observed as meta.link.reference.def.markdown leak past
                // back-to-back Markdown link reference definitions).
                let buf_len = parsed_line_buffer.len();
                let mut corrected_baselines: Vec<(usize, ScopeStack)> = Vec::new();

                for (i, replayed_ops) in replayed.iter().enumerate() {
                    let record = &mut parsed_line_buffer[start_idx + i];
                    let has_non_assertion = record.non_assertion_data.is_some();

                    // Advance the stack through the corrected ops, building
                    // scoped text for non-assertion lines
                    let mut new_scoped = Vec::new();
                    let mut col: usize = 0;
                    for (s, op) in ScopeRegionIterator::new(replayed_ops, &record.line_text) {
                        stack.apply(op).unwrap();
                        if !s.is_empty() && has_non_assertion {
                            let len = s.chars().count();
                            new_scoped.push(ScopedText {
                                char_start: col,
                                text_len: len,
                                scope: stack.as_slice().to_vec(),
                            });
                            col += len;
                        }
                    }
                    let next_idx = start_idx + i + 1;
                    if next_idx < buf_len {
                        corrected_baselines.push((next_idx, stack.clone()));
                    }

                    if let Some(ref mut data) = record.non_assertion_data {
                        if !data.assertions.is_empty() {
                            // Re-evaluate all assertions against corrected scopes
                            let old_failures = data.assertion_failures;
                            let mut new_failures: usize = 0;
                            for buffered in &data.assertions {
                                let temp_assertion = AssertionRange {
                                    begin_char: buffered.begin_char,
                                    end_char: buffered.end_char,
                                    scope_selector_text: &buffered.scope_selector_text,
                                    is_pure_assertion_line: true,
                                    // Only non-reference assertions are buffered
                                    // above, so replays never hit reference lines.
                                    is_reference: false,
                                };
                                // Replay path: fall back to the previous
                                // past-EOL semantics by passing no next-line
                                // scopes. Replays are rare and per-target;
                                // recomputing the next line's scopes here
                                // would require also replaying its ops.
                                let result = process_assertions(&temp_assertion, &new_scoped, None);
                                for failure in result.iter().filter(|r| !r.success) {
                                    let length = failure.column_end - failure.column_begin;
                                    let text: String = record
                                        .line_text
                                        .chars()
                                        .skip(failure.column_begin)
                                        .take(length)
                                        .collect();
                                    pending_messages.push(BufferedFailureMessage {
                                        selector_text: buffered
                                            .scope_selector_text
                                            .trim()
                                            .to_string(),
                                        assertion_line_number: buffered.assertion_line_number,
                                        test_against_line_number: record.line_number,
                                        column_begin: failure.column_begin,
                                        column_end: failure.column_end,
                                        text,
                                        scope: new_scoped
                                            .iter()
                                            .find(|s| {
                                                s.char_start + s.text_len > failure.column_begin
                                            })
                                            .unwrap_or_else(|| new_scoped.last().unwrap())
                                            .scope
                                            .clone(),
                                    });
                                    new_failures += failure.column_end - failure.column_begin;
                                }
                            }
                            assertion_failures = assertion_failures - old_failures + new_failures;
                            data.assertion_failures = new_failures;
                        }

                        // Update scopes if this is the line currently being tested
                        if record.line_number == test_against_line_number {
                            scopes_on_line_being_tested = new_scoped;
                            previous_non_assertion_line = record.line_text.clone();
                        }
                    }
                }
                for (idx, corrected) in corrected_baselines {
                    parsed_line_buffer[idx].stack_before = corrected;
                }
            }

            if out_opts.debug && !line_only_has_assertion {
                if ops.is_empty() && !line.is_empty() {
                    println!("no operations for this line...");
                } else {
                    debug_print_ops(&line, &ops);
                }
            }
            // Snapshot the stack now (post-replays, pre-current-ops) — this
            // becomes the buffered `stack_before` for the current line, so a
            // future replay covering it resets to the corrected baseline
            // rather than the stale pre-parse value.
            let stack_before = stack.clone();
            // Build the just-parsed line's scopes. For non-assertion (target)
            // lines they go into `scopes_on_line_being_tested`; for the FIRST
            // assertion line that follows a target they go into a fresh vec
            // that becomes `next_target_line_scopes` (used for past-EOL wrap).
            // Subsequent assertion lines don't need their scopes captured.
            let capture_for_next_target = line_has_assertion && next_target_line_scopes.is_none();
            let mut next_target_buffer: Vec<ScopedText> = Vec::new();
            let mut col: usize = 0;
            for (s, op) in ScopeRegionIterator::new(&ops, &line) {
                if let Err(_) = stack.apply(op) {
                    // Scope stack error (e.g. NoClearedScopesToRestore) - treat as test failure
                    if !out_opts.summary {
                        eprintln!("  Scope stack error on line {}", current_line_number);
                    }
                    return Ok(SyntaxTestFileResult::FailedAssertions(
                        total_assertions.max(1),
                        total_assertions.max(1),
                    ));
                }
                if s.is_empty() {
                    // in this case we don't care about blank tokens
                    continue;
                }
                let len = s.chars().count();
                let scoped = ScopedText {
                    char_start: col,
                    text_len: len,
                    scope: stack.as_slice().to_vec(),
                };
                if !line_has_assertion {
                    // if the line has no assertions on it, remember the scopes on the line so we can test against them later
                    scopes_on_line_being_tested.push(scoped);
                } else if capture_for_next_target {
                    next_target_buffer.push(scoped);
                }
                // TODO: warn when there are duplicate adjacent (non-meta?) scopes, as it is almost always undesired
                col += len;
            }
            if capture_for_next_target {
                next_target_line_scopes = Some(next_target_buffer);
            }

            // Buffer this parsed line for potential future replay
            let non_assertion_data = if !line_has_assertion {
                Some(NonAssertionData {
                    assertions: Vec::new(),
                    assertion_failures: 0,
                })
            } else {
                None
            };
            parsed_line_buffer.push(ParsedLineRecord {
                line_text: line.to_string(),
                line_number: current_line_number,
                stack_before,
                non_assertion_data,
            });
            if !line_has_assertion {
                current_test_line_buffer_idx = Some(parsed_line_buffer.len() - 1);
            }
        }

        // Process the assertion (after parsing, so past-EOL wrap can see the
        // current line's scopes via `next_target_line_scopes`).
        if let Some(assertion) = assertion_opt {
            // `@+` and `>` lines are annotation-only (reference labels /
            // reference assertions). They must be recognised so they do not
            // drive `test_against_line_number`, but we do not yet implement
            // cross-line label lookups, so no scope checks run here.
            let mut current_assertion_failures: usize = 0;
            if !assertion.is_reference {
                let result = process_assertions(
                    &assertion,
                    &scopes_on_line_being_tested,
                    next_target_line_scopes.as_deref(),
                );
                total_assertions += assertion.end_char - assertion.begin_char;
                for failure in result.iter().filter(|r| !r.success) {
                    let length = failure.column_end - failure.column_begin;
                    let text: String = previous_non_assertion_line
                        .chars()
                        .skip(failure.column_begin)
                        .take(length)
                        .collect();
                    pending_messages.push(BufferedFailureMessage {
                        selector_text: assertion.scope_selector_text.trim().to_string(),
                        assertion_line_number: current_line_number,
                        test_against_line_number,
                        column_begin: failure.column_begin,
                        column_end: failure.column_end,
                        text,
                        scope: scopes_on_line_being_tested
                            .iter()
                            .find(|s| s.char_start + s.text_len > failure.column_begin)
                            .unwrap_or_else(|| scopes_on_line_being_tested.last().unwrap())
                            .scope
                            .clone(),
                    });
                    assertion_failures += failure.column_end - failure.column_begin;
                    current_assertion_failures += failure.column_end - failure.column_begin;
                }
                // Buffer this assertion for re-evaluation if backtracking replays the target line
                if let Some(idx) = current_test_line_buffer_idx {
                    if let Some(ref mut data) = parsed_line_buffer[idx].non_assertion_data {
                        data.assertions.push(BufferedAssertion {
                            begin_char: assertion.begin_char,
                            end_char: assertion.end_char,
                            scope_selector_text: assertion.scope_selector_text.to_string(),
                            assertion_line_number: current_line_number,
                        });
                        data.assertion_failures += current_assertion_failures;
                    }
                }
            }
        }

        // Flush buffered failure messages once the parser commits
        if !state.is_speculative() {
            flush_pending_messages(&mut pending_messages, out_opts.summary);
        }

        line.clear();
        current_line_number += 1;
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }
        line = line.replace('\r', "");
    }
    // Flush any remaining buffered messages at EOF — parsing is done
    flush_pending_messages(&mut pending_messages, out_opts.summary);

    if assertion_failures > 0 {
        Ok(SyntaxTestFileResult::FailedAssertions(
            assertion_failures,
            total_assertions,
        ))
    } else {
        Ok(SyntaxTestFileResult::Success(total_assertions))
    }
}

fn report_file_result(
    path: &Path,
    result: &Result<SyntaxTestFileResult, SyntaxTestHeaderError>,
    out_opts: OutputOptions,
) {
    if out_opts.summary {
        // Don't print total assertion count so that diffs don't pick up new succeeding tests
        match result {
            Ok(SyntaxTestFileResult::FailedAssertions(failures, _)) => {
                println!("FAILED {}: {}", path.display(), failures);
            }
            Err(SyntaxTestHeaderError::MalformedHeader) => {
                println!("FAILED {}: malformed header", path.display());
            }
            Err(SyntaxTestHeaderError::SyntaxDefinitionNotFound) => {
                println!("FAILED {}: syntax definition not found", path.display());
            }
            // Skipped files (non-scope variants like reindent /
            // partial-symbols) don't contribute to the baseline:
            // silent in summary mode so `known_syntest_failures.txt`
            // stays focused on actual scope-test failures.
            Ok(SyntaxTestFileResult::Skipped(_)) => {}
            Ok(SyntaxTestFileResult::Success(_)) => {}
        }
    } else {
        println!("{:?}", result);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut opts = Options::new();
    opts.optflag("d", "debug", "Show parsing results for each test line");
    opts.optflag(
        "t",
        "time",
        "Time execution as a more broad-ranging benchmark",
    );
    opts.optflag("s", "summary", "Print only summary of test failures");

    let matches = match opts.parse(&args[1..]) {
        Ok(m) => m,
        Err(f) => {
            panic!("{}", f.to_string())
        }
    };

    let tests_path = if matches.free.is_empty() {
        "."
    } else {
        &args[1]
    };

    let syntaxes_path = if matches.free.len() < 2 { "" } else { &args[2] };

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
        let mut builder = SyntaxSetBuilder::new();
        builder.add_from_folder(syntaxes_path, true).unwrap(); // note that we load the version with newlines
        for warning in builder.warnings() {
            eprintln!("Warning: {}", warning);
        }
        ss = builder.build();
        for warning in ss.warnings() {
            eprintln!("Warning: {}", warning);
        }
    }

    let out_opts = OutputOptions {
        debug: matches.opt_present("debug"),
        time: matches.opt_present("time"),
        summary: matches.opt_present("summary"),
    };

    let exit_code = recursive_walk(&ss, tests_path, out_opts);
    println!("exiting with code {}", exit_code);
    std::process::exit(exit_code);
}

fn recursive_walk(ss: &SyntaxSet, path: &str, out_opts: OutputOptions) -> i32 {
    let mut exit_code: i32 = 0; // exit with code 0 by default, if all tests pass
    let walker = WalkDir::new(path).into_iter();

    // accumulate and sort for consistency of diffs across machines
    let mut files = Vec::new();
    for entry in walker.filter_entry(|e| e.file_type().is_dir() || is_a_syntax_test_file(e)) {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            files.push(entry.path().to_owned());
        }
    }
    files.sort();

    for path in &files {
        if !out_opts.summary {
            println!("Testing file {}", path.display());
        }
        let start = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            test_file(ss, path, true, out_opts)
        }))
        .unwrap_or_else(|_| {
            eprintln!("PANIC while testing {}", path.display());
            Ok(SyntaxTestFileResult::FailedAssertions(1, 1))
        });
        report_file_result(path, &result, out_opts);
        let elapsed = start.elapsed();
        if out_opts.time {
            let ms = (elapsed.as_secs() * 1_000) + elapsed.subsec_millis() as u64;
            println!("{} ms for file {}", ms, path.display());
        }
        if exit_code != 2 {
            // leave exit code 2 if there was an error
            if result.is_err() {
                // set exit code 2 if there was an error
                exit_code = 2;
            } else if let Ok(SyntaxTestFileResult::FailedAssertions(_, _)) = result {
                exit_code = 1; // otherwise, if there were failures, exit with code 1
            }
        }
    }

    exit_code
}

fn is_a_syntax_test_file(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with("syntax_test_"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    //! Unit tests for `get_line_assertion_details`.
    //!
    //! These capture the root cause found while triaging the `syntest`
    //! failures on the bumped upstream Packages: reference-label lines
    //! (`// @@@ name`) and reference-based assertions (`// > name sel`)
    //! were not recognised, so `test_against_line_number` silently drifted
    //! onto those annotation lines and subsequent `^` assertions got
    //! mapped to the test-comment scope instead of the intended source
    //! line. The fix also needs to avoid false positives where the marker
    //! characters appear in real source (Ruby `#@var`, JSDoc `// @param`,
    //! HTML-style comment-end `-->`).
    use super::*;
    const CC: &str = "//"; // C-style test-token-start
    const HASH: &str = "#"; // Ruby / Shell
    const XML_START: &str = "<!--";
    const XML_END: Option<&str> = Some("-->");
    fn details<'a>(start: &str, end: Option<&str>, line: &'a str) -> Option<AssertionRange<'a>> {
        get_line_assertion_details(start, end, line)
    }

    #[test]
    fn column_range_assertion_is_recognised() {
        let a = details(CC, None, "//     ^^^ keyword.control").unwrap();
        assert!(!a.is_reference);
        assert!(a.is_pure_assertion_line);
        assert_eq!(a.begin_char, 7);
        assert_eq!(a.end_char, 10);
    }
    #[test]
    fn start_of_line_assertion_is_recognised() {
        let a = details(CC, None, "//  <- keyword.control").unwrap();
        assert!(!a.is_reference);
        assert!(a.is_pure_assertion_line);
        assert_eq!(a.begin_char, 0);
        assert_eq!(a.end_char, 1);
    }
    #[test]
    fn reference_label_is_recognised() {
        let a = details(CC, None, "//   @@@ my-label").unwrap();
        assert!(a.is_reference);
        assert!(a.is_pure_assertion_line);
        // Label column range covers the `@@@` glyphs themselves.
        assert_eq!(a.begin_char, 5);
        assert_eq!(a.end_char, 8);
    }
    #[test]
    fn reference_assertion_is_recognised() {
        let a = details(CC, None, "//  > my-label keyword.control").unwrap();
        assert!(a.is_reference);
        assert!(a.is_pure_assertion_line);
    }
    #[test]
    fn ruby_instance_variable_is_not_mistaken_for_reference_label() {
        // `#@var` is Ruby syntax; after the `#` test-token-start the rest
        // is `@var`, which must not match `@+` as a reference label.
        assert!(details(HASH, None, "#@var").is_none());
    }
    #[test]
    fn jsdoc_at_tags_are_not_mistaken_for_reference_labels() {
        // `// @param` is JSDoc; after `//` the rest is ` @param foo`,
        // which must not match because `@` is followed by `p`, not by
        // whitespace or end-of-line.
        assert!(details(CC, None, "// @param foo").is_none());
    }
    #[test]
    fn textile_header_comment_end_is_not_mistaken_for_reference_assertion() {
        // The XML-comment end token `-->` contains a `>`; the anchored
        // start of the regex prevents that `>` from matching as a
        // reference assertion on the header line.
        assert!(details(
            XML_START,
            XML_END,
            "<!-- SYNTAX TEST \"Packages/Textile/Textile.sublime-syntax\" -->"
        )
        .is_none());
    }
    #[test]
    fn generic_closing_angle_is_not_mistaken_for_reference_assertion() {
        // `// Vec<T>` in source: after `//` remainder is ` Vec<T>`. The
        // regex is anchored to the start so the `>` cannot match.
        assert!(details(CC, None, "// Vec<T>").is_none());
    }
    #[test]
    fn single_at_reference_label_with_trailing_space_matches() {
        // A single-column label (`@ name`) is valid and used upstream for
        // fine-grained labels; ensure `@+` still matches for a single `@`.
        let a = details(CC, None, "//    @ definition").unwrap();
        assert!(a.is_reference);
    }
    #[test]
    fn reference_label_at_end_of_line_without_trailing_name_matches() {
        // Anchor-only `@@@` with no label name or following whitespace is
        // rare but should still be recognised (end-of-line satisfies the
        // marker-boundary requirement).
        let a = details(CC, None, "//   @@@").unwrap();
        assert!(a.is_reference);
    }
    #[test]
    fn shell_parameter_length_is_not_mistaken_for_column_range() {
        // bash `: ${#^pattern}` — `#` is the parameter-length operator, not
        // a comment. Source code precedes the testtoken, so the line is not
        // a pure-assertion line and must not be processed as an assertion.
        assert!(details(HASH, None, ": ${#^pattern}").is_none());
        assert!(details(HASH, None, ": ${#^^pattern}").is_none());
    }
    #[test]
    fn trailing_comment_with_left_arrow_is_not_mistaken_for_assertion() {
        // bash `[ <<doc ]  # <- ]` — a real comment whose body happens to
        // start with `<-`. Because source code precedes the testtoken, the
        // `<-` is not author-intended as an assertion marker.
        assert!(details(HASH, None, "[ <<doc ]  # <- ]").is_none());
    }

    /// Build a `ScopedText` from a string of space-separated scope atoms.
    fn st(char_start: usize, text_len: usize, scopes: &str) -> ScopedText {
        ScopedText {
            char_start,
            text_len,
            scope: scopes
                .split_whitespace()
                .map(|s| Scope::new(s).unwrap())
                .collect(),
        }
    }
    fn assertion_range(begin_char: usize, end_char: usize, sel: &str) -> AssertionRange<'_> {
        AssertionRange {
            begin_char,
            end_char,
            scope_selector_text: sel,
            is_pure_assertion_line: true,
            is_reference: false,
        }
    }

    #[test]
    fn past_eol_negative_assertion_uses_next_line_wrap() {
        // git_config-shaped: the consumed `\n` of the target line still
        // carries the parent meta_scope (`meta.section`), but the next line
        // is a comment whose scope does not include `meta.section`.
        // Negative past-EOL assertion `- meta.section` must pass.
        let target = vec![
            st(0, 1, "text meta.section punctuation.section.brackets.begin"),
            st(
                28,
                1,
                "text meta.section meta.brackets invalid.illegal.unexpected.eol",
            ),
        ];
        let next = vec![
            st(0, 1, "text comment.line punctuation.definition.comment"),
            st(1, 1, "text comment.line"),
        ];
        let a = assertion_range(29, 30, "- meta.section");
        let r = process_assertions(&a, &target, Some(&next));
        assert_eq!(r.len(), 1);
        assert!(r[0].success, "expected pass via wrap to next line: {r:?}");
    }

    #[test]
    fn past_eol_positive_assertion_matches_next_line_scope() {
        // Past-EOL assertion expecting `comment.line` finds it on the next
        // line — both git_config (lines 554-555) and Clojure (line 33) work
        // this way in ST.
        let target = vec![
            st(
                0,
                1,
                "text meta.mapping.value string.quoted.double punctuation.definition.string.end",
            ),
            st(81, 1, "text"),
        ];
        let next = vec![
            st(0, 1, "text comment.line punctuation.definition.comment"),
            st(1, 14, "text comment.line"),
        ];
        let a = assertion_range(82, 97, "comment.line");
        let r = process_assertions(&a, &target, Some(&next));
        assert!(
            r.iter().all(|res| res.success),
            "expected all wrap segments to pass: {r:?}",
        );
    }

    #[test]
    fn past_eol_falls_back_when_next_line_unavailable() {
        // Without next-line scopes (end-of-file or replay path), keep the
        // previous behaviour of testing against the last char's scope.
        let target = vec![st(0, 5, "text constant.numeric"), st(5, 1, "text")];
        let a_pass = assertion_range(6, 7, "- constant.numeric");
        let r_pass = process_assertions(&a_pass, &target, None);
        assert_eq!(r_pass.len(), 1);
        assert!(r_pass[0].success);
        let a_fail = assertion_range(6, 7, "constant.numeric");
        let r_fail = process_assertions(&a_fail, &target, None);
        assert_eq!(r_fail.len(), 1);
        assert!(!r_fail[0].success);
    }

    #[test]
    fn past_eol_wrap_overshooting_next_line_uses_next_line_last_scope() {
        // When the wrap target extends past the next line's content too,
        // fall back to the next line's last scope (recursive wrap is a v2
        // concern). The verdict must still be defined, not silently skipped.
        let target = vec![st(0, 1, "text"), st(1, 1, "text")];
        let next = vec![st(0, 3, "text source.x")];
        // Wrap range: cols [2, 8) on target → cols [0, 6) on next.
        // Cols [3, 6) on next overshoot the 3-char `next` and should fall
        // back to the next-line last scope (`text source.x`).
        let a = assertion_range(2, 8, "source.x");
        let r = process_assertions(&a, &target, Some(&next));
        assert!(r.iter().all(|res| res.success), "got: {r:?}");
    }
}
