//! Property tests for the parse-output contract: clone-resume
//! determinism, exactly-once delivery, apply-safety, and the revision
//! lag bound. Instead of a randomized property harness, these
//! exhaustively enumerate every input sequence up to a fixed length
//! from a small line alphabet chosen to exercise same-line fails,
//! cross-line fails, exhaustion, nesting, and empty lines.

use super::*;

/// Two branch points (one nested inside the other's first alternative),
/// fails reachable on both, alternatives that consume newlines and stay
/// active across lines — the shapes that drive the speculation window.
const PROPERTY_SYNTAX: &str = r#"
name: PropertyTest
scope: source.prop
contexts:
  main:
    - match: 'TRY'
      branch_point: outer
      branch: [outer-a, outer-b]
    - match: '\w+'
      scope: main.word.prop
  outer-a:
    - match: '\n'
    - match: 'INNER'
      branch_point: inner
      branch: [inner-a, inner-b]
    - match: 'FAIL'
      fail: outer
    - match: '\w+'
      scope: outer-a.word.prop
  outer-b:
    - match: '.*'
      scope: outer-b.content.prop
      pop: true
  inner-a:
    - match: '\n'
    - match: 'KILL'
      fail: inner
    - match: '\w+'
      scope: inner-a.word.prop
  inner-b:
    - match: '\n'
    - match: 'KILL'
      fail: inner
    - match: '\w+'
      scope: inner-b.word.prop
"#;

const ALPHABET: &[&str] = &["TRY\n", "INNER\n", "FAIL\n", "KILL\n", "ok\n", "\n"];
const MAX_LEN: usize = 4;

/// One line's ops plus how many earlier lines its call revised.
type RecordedOutput = (Vec<(usize, ScopeStackOp)>, Option<usize>);

fn property_syntax_set() -> SyntaxSet {
    let syntax = SyntaxDefinition::load_from_str(PROPERTY_SYNTAX, true, None).unwrap();
    link(syntax)
}

/// Every input sequence over `ALPHABET` with length in `1..=MAX_LEN`.
fn all_sequences() -> Vec<Vec<&'static str>> {
    let mut seqs: Vec<Vec<&'static str>> = vec![Vec::new()];
    let mut out = Vec::new();
    for _ in 0..MAX_LEN {
        let mut next = Vec::new();
        for seq in &seqs {
            for line in ALPHABET {
                let mut s = seq.clone();
                s.push(*line);
                out.push(s.clone());
                next.push(s);
            }
        }
        seqs = next;
    }
    out
}

/// Runs the raw protocol, asserting apply-safety and the lag bound at
/// every step, and returns the effective per-line ops.
fn run_effective(
    state: &mut ParseState,
    lines: &[&str],
    ss: &SyntaxSet,
) -> Vec<Vec<(usize, ScopeStackOp)>> {
    let mut effective: Vec<Vec<(usize, ScopeStackOp)>> = Vec::new();
    for line in lines {
        let out = state.parse_line(line, ss).expect("parse");
        if let Some(revised) = out.revised {
            // Contract: the revision never reaches past what has been
            // returned, and never past the expiry window.
            assert!(
                revised.len() <= effective.len(),
                "revision covers {} lines but only {} were returned",
                revised.len(),
                effective.len()
            );
            assert!(revised.len() <= 130, "revision exceeds the expiry window");
            let start = effective.len() - revised.len();
            effective.truncate(start);
            effective.extend(revised);
        }
        assert!(
            state.speculative_lines() <= 130,
            "speculation window exceeds the expiry bound"
        );
        effective.push(out.ops);
    }
    // Apply-safety: the effective stream folds without error.
    let mut stack = ScopeStack::new();
    for line_ops in &effective {
        for (_, op) in line_ops {
            stack.apply(op).expect("effective ops must apply cleanly");
        }
    }
    effective
}

/// Cloning the state at any `speculative_lines() == 0` boundary and
/// resuming from the clone must reproduce the primary run exactly.
#[test]
fn clone_at_commit_boundaries_resumes_deterministically() {
    let ss = property_syntax_set();
    for seq in all_sequences() {
        let mut primary = ParseState::new(&ss.syntaxes()[0]);
        let mut clones: Vec<(usize, ParseState)> = Vec::new();
        let mut outputs: Vec<RecordedOutput> = Vec::new();
        for (i, line) in seq.iter().enumerate() {
            let out = primary.parse_line(line, &ss).expect("parse");
            outputs.push((out.ops, out.revised.map(|r| r.len())));
            if primary.speculative_lines() == 0 {
                clones.push((i + 1, primary.clone()));
            }
        }
        for (resume_at, mut clone) in clones {
            for (j, line) in seq[resume_at..].iter().enumerate() {
                let out = clone.parse_line(line, &ss).expect("parse");
                let (ref ops, revised_len) = outputs[resume_at + j];
                assert_eq!(
                    &out.ops,
                    ops,
                    "clone resumed at line {} diverged on line {} of {:?}",
                    resume_at,
                    resume_at + j,
                    seq
                );
                assert_eq!(
                    out.revised.map(|r| r.len()),
                    revised_len,
                    "clone resumed at line {} revised differently on line {} of {:?}",
                    resume_at,
                    resume_at + j,
                    seq
                );
            }
            // Determinism all the way down: after replaying the tail,
            // the clone's state is indistinguishable from the primary's.
            assert_eq!(
                clone, primary,
                "clone resumed at line {} ended in a different state for {:?}",
                resume_at, seq
            );
        }
    }
}

/// `CommittedParser` yields each line exactly once, in order, and the
/// concatenation equals the raw protocol's effective ops.
#[test]
fn committed_parser_is_exactly_once_and_matches_raw_protocol() {
    let ss = property_syntax_set();
    for seq in all_sequences() {
        let mut state = ParseState::new(&ss.syntaxes()[0]);
        let effective = run_effective(&mut state, &seq, &ss);

        let mut committed = CommittedParser::new(&ss.syntaxes()[0]);
        let mut collected = Vec::new();
        for line in &seq {
            collected.extend(committed.feed(line, &ss).expect("feed"));
        }
        collected.extend(committed.finish());
        assert_eq!(
            collected.len(),
            seq.len(),
            "exactly one committed entry per input line for {:?}",
            seq
        );
        assert_eq!(
            collected, effective,
            "committed stream diverged from the raw protocol for {:?}",
            seq
        );
    }
}
