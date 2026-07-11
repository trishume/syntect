//! Tests for the `CommittedParser` wrapper: lines come out exactly
//! once, in order, already carrying any cross-line revisions.

use super::*;

const CROSS_LINE_SYNTAX: &str = r#"
name: CommittedTest
scope: source.ct
contexts:
  main:
    - match: 'TRY'
      branch_point: bp
      branch: [try-ctx, fallback-ctx]
    - match: '\w+'
      scope: main.word.ct
  try-ctx:
    - match: '\n'
    - match: 'FAIL'
      fail: bp
    - match: '\w+'
      scope: try.word.ct
  fallback-ctx:
    - match: '.*'
      scope: fallback.content.ct
      pop: true
"#;

fn load(syntax_str: &str) -> SyntaxSet {
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    link(syntax)
}

/// Runs the raw `parse_line` protocol over `lines`, maintaining the
/// effective per-line ops the `revised` contract yields.
fn effective_ops(lines: &[&str], ss: &SyntaxSet) -> Vec<Vec<(usize, ScopeStackOp)>> {
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let mut effective: Vec<Vec<(usize, ScopeStackOp)>> = Vec::new();
    for line in lines {
        let out = state.parse_line(line, ss).expect("parse");
        if let Some(revised) = out.revised {
            let start = effective.len() - revised.len();
            effective.truncate(start);
            effective.extend(revised);
        }
        effective.push(out.ops);
    }
    effective
}

#[test]
fn committed_lines_match_raw_protocol_and_arrive_once_in_order() {
    let ss = load(CROSS_LINE_SYNTAX);
    let lines = ["TRY\n", "FAIL\n", "benign\n", "TRY\n", "ok\n"];

    let mut committed = CommittedParser::new(&ss.syntaxes()[0]);
    let mut collected: Vec<Vec<(usize, ScopeStackOp)>> = Vec::new();
    let mut batch_sizes = Vec::new();
    for line in lines {
        let batch = committed.feed(line, &ss).expect("feed");
        batch_sizes.push(batch.len());
        collected.extend(batch);
    }
    collected.extend(committed.finish());

    // While the TRY speculation is open nothing comes out; resolving it
    // releases the buffered lines together.
    assert_eq!(
        batch_sizes[0], 0,
        "line 1 opens a speculation window; nothing is final yet"
    );
    assert_eq!(
        batch_sizes[1], 2,
        "the cross-line fail resolves the window; lines 1-2 come out together"
    );

    // Exactly one entry per input line, in order, equal to what the raw
    // revised protocol yields.
    assert_eq!(collected.len(), lines.len());
    assert_eq!(collected, effective_ops(&lines, &ss));

    // The revision must be baked in: line 1 carries the fallback
    // alternative's scope, not the failed try alternative's.
    let line1_scopes: Vec<String> = collected[0]
        .iter()
        .filter_map(|(_, op)| match op {
            ScopeStackOp::Push(s) => Some(format!("{:?}", s)),
            _ => None,
        })
        .collect();
    assert!(
        line1_scopes.iter().any(|s| s.contains("fallback.content")),
        "committed line 1 must carry the revised fallback scope; got {:?}",
        line1_scopes
    );
    assert!(
        !line1_scopes.iter().any(|s| s.contains("try.word")),
        "committed line 1 must not leak the failed alternative; got {:?}",
        line1_scopes
    );
}

#[test]
fn finish_drains_an_unresolved_window() {
    let ss = load(CROSS_LINE_SYNTAX);
    let mut committed = CommittedParser::new(&ss.syntaxes()[0]);

    let batch = committed.feed("TRY\n", &ss).expect("feed");
    assert!(
        batch.is_empty(),
        "speculation window is open at end of line 1"
    );

    // EOF with the branch still unresolved: the buffered line is final
    // as parsed.
    let rest = committed.finish();
    assert_eq!(rest.len(), 1, "finish drains the one buffered line");
    assert!(committed.finish().is_empty(), "finish is idempotent");
}

#[test]
fn take_warnings_surfaces_parse_warnings() {
    let ss = load(CROSS_LINE_SYNTAX);
    let mut committed = CommittedParser::new(&ss.syntaxes()[0]);
    let _ = committed.feed("TRY\n", &ss).expect("feed");
    // try-ctx consumes newlines and stays active: feed enough filler to
    // expire the branch point.
    for _ in 0..129 {
        let _ = committed.feed("\n", &ss).expect("feed");
    }
    let warnings = committed.take_warnings();
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w, ParseWarning::BranchPointExpired { name } if name == "bp")),
        "expected the expiry warning, got {:?}",
        warnings
    );
    assert!(committed.take_warnings().is_empty(), "take_warnings drains");
}
