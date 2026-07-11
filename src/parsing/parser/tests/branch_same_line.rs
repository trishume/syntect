//! branch_point/fail speculation resolved within a single line.

use super::*;

/// Regression guard for the "branch_point match loses its own scope
/// on fail-retry" bug: when the keyword that triggers a
/// branch_point (e.g. `LIKE` in SQL with
/// `scope: keyword.operator.comparison.sql`) has its first
/// alternative fail, the Push/Pop for that scope was truncated off
/// `ops` along with alt[0]'s subsequent work and never re-emitted.
/// The eventual successful alternative then produced a parse where
/// the keyword carried no scope — 4,942 cascading assertion
/// failures in TSQL.
///
/// The test triggers the same shape synthetically: a `trigger` match
/// with its own scope branches into two alternatives; the first
/// fails, the second succeeds; the `trigger` token must still carry
/// the declared scope after the retry.
#[test]
fn branch_point_match_scope_survives_fail_retry() {
    // Expected: `trigger` gets `keyword.operator.test`; the
    // following word gets `ok.test` (via alt-succeeds).
    expect_scope_stacks(
        "trigger yes",
        &["<keyword.operator.test>", "<ok.test>"],
        r#"
                name: Branch Pat Scope Test
                scope: source.test
                contexts:
                  main:
                    - match: \btrigger\b
                      scope: keyword.operator.test
                      branch_point: t
                      branch:
                        - alt-fails
                        - alt-succeeds
                    - match: \S+
                      scope: text.test
                  alt-fails:
                    - match: (?=\S)
                      fail: t
                  alt-succeeds:
                    - match: \S+
                      scope: ok.test
                      pop: 1
                "#,
    );
}

/// Regression guard for "branch_point fail-retry drops the
/// trigger match's `captures:` scopes". The non-fail path emits
/// capture Push/Pop ops inside the pat_scope brackets; the
/// same-line fail re-emit must do the same — otherwise the
/// inner capture scopes are truncated off `ops` together with
/// alt[0]'s subsequent work and never replayed. Observed on
/// Haskell's `data CtxCls ctx => ModId.QTyCls`, where the
/// `(data)(?:\s+(family|instance))?` branch_point match's first
/// capture `keyword.declaration.data.haskell` was dropped from
/// the `data` token whenever `data-signature` failed into
/// `data-context` — 22 assertion failures in
/// `syntax_test_haskell.hs`.
#[test]
fn branch_point_capture_scopes_survive_fail_retry() {
    // The `(word)\s` branch_point match carries both `scope:`
    // and `captures:`. Alt[0] fails on the `!` lookahead,
    // forcing replay into alt[1]. `inner.capture` on the first
    // capture group must remain on the stack over `word`.
    expect_scope_stacks(
        "word !",
        &["<outer.match>, <inner.capture>"],
        r#"
                name: Branch Capture Re-emit Test
                scope: source.test
                contexts:
                  main:
                    - match: (word)\s
                      scope: outer.match
                      captures:
                        1: inner.capture
                      branch_point: bp
                      branch:
                        - alt-fails
                        - alt-succeeds
                  alt-fails:
                    - match: (?=!)
                      fail: bp
                  alt-succeeds:
                    - match: \S+
                      scope: ok.test
                      pop: 1
                "#,
    );
}

/// Regression guard for "branch_point fail-retry drops the new
/// alternative's `meta_scope` from the trigger character". The
/// non-fail push path emits the new context's `meta_scope` at
/// `match_start` so the matched text sees it. The same-line
/// fail re-emit must do the same — emit `meta_scope` (and any
/// `clear_scopes`) at `trigger_match_start`, before the
/// trigger's `pat.scope`. Placing them after the match meant
/// `for (var i = 0; …)` parsed the `(` with
/// `[meta.for.js, punctuation.section.group.begin.js]` instead
/// of `[meta.for.js, meta.group.js, punctuation.section.group.begin.js]`,
/// failing eight assertions in `syntax_test_js_control.js`.
#[test]
fn branch_point_fail_retry_applies_meta_scope_to_trigger() {
    // Mirrors the JS for-loop shape: `\(` triggers a branch with
    // `pop: 1`; alt 0 fails, alt 1 succeeds; alt 1 has a
    // `meta_scope` that must wrap the `(` itself.
    expect_scope_stacks(
        "(x",
        &["<meta.group.test>, <punctuation.test>"],
        r#"
                name: Branch Meta Scope Test
                scope: source.test
                contexts:
                  main:
                    - match: ''
                      push: trigger
                  trigger:
                    - match: \(
                      scope: punctuation.test
                      branch_point: g
                      branch:
                        - alt-fails
                        - alt-succeeds
                      pop: 1
                  alt-fails:
                    - meta_scope: meta.group.test
                    - match: (?=\S)
                      fail: g
                  alt-succeeds:
                    - meta_scope: meta.group.test
                    - match: \S+
                      scope: ok.test
                      pop: 1
                "#,
    );
}

/// Category A proper regression guard: a same-line `branch_point`
/// whose alternatives all `fail` must unwind to the pre-branch
/// snapshot and advance the cursor, rather than leaving the stack
/// stuck in the last attempted alternative. This was the cause of
/// the Zsh `meta.interpolation.brace.shell never pops` cascade
/// (Zsh excludes the usual `brace-interpolation-fallback` branch,
/// so `{no}` exhausted both `sequence` and `series` alternatives
/// and the parser silently left the scope stack inside
/// `brace-interpolation-series-begin`).
#[test]
fn branch_point_with_all_alternatives_failing_unwinds_state() {
    let syntax = SyntaxDefinition::load_from_str(
        r#"
                name: All Alternatives Fail Test
                scope: source.test
                contexts:
                  main:
                    - match: (?=\{)
                      branch_point: brace
                      branch:
                        - brace-strict
                        - brace-numeric
                    - match: \w+
                      scope: plain.test
                  brace-strict:
                    - meta_scope: meta.interpolation.brace.test
                    - match: \{
                      scope: punctuation.begin.test
                      push: brace-strict-body
                  brace-strict-body:
                    - meta_content_scope: inside-strict.test
                    - match: foo
                      scope: keyword.test
                    - match: \}
                      scope: punctuation.end.test
                      pop: 2
                    - match: (?=\S)
                      fail: brace
                  brace-numeric:
                    - meta_scope: meta.interpolation.brace.test
                    - match: \{
                      scope: punctuation.begin.test
                      push: brace-numeric-body
                  brace-numeric-body:
                    - meta_content_scope: inside-numeric.test
                    - match: \d+
                      scope: constant.numeric.test
                    - match: \}
                      scope: punctuation.end.test
                      pop: 2
                    - match: (?=\S)
                      fail: brace
                "#,
        true,
        None,
    )
    .unwrap();

    let syntax_set = link(syntax);
    let mut state = ParseState::new(&syntax_set.syntaxes()[0]);
    // `{no}` — neither strict (expects `foo`) nor numeric (expects
    // digits) matches, so both branches fail. Before the fix, the
    // stack stayed in `brace-numeric-body` across the `\n`.
    let o = ops(&mut state, "{no}\n", &syntax_set);
    let mut stack = ScopeStack::new();
    for (_, op) in &o {
        stack.apply(op).unwrap();
    }
    let final_scopes: Vec<String> = stack
        .as_slice()
        .iter()
        .map(|s| format!("{:?}", s))
        .collect();
    assert!(
        !final_scopes
            .iter()
            .any(|s| s.contains("meta.interpolation.brace")),
        "meta.interpolation.brace leaked past end of line; stack: {:?}",
        final_scopes
    );
    assert!(
        !final_scopes
            .iter()
            .any(|s| s.contains("inside-strict") || s.contains("inside-numeric")),
        "inside-* meta_content_scope leaked past end of line; stack: {:?}",
        final_scopes
    );
}

#[test]
fn branch_first_alternative_succeeds() {
    // "let = foo;" should parse as a let-statement (first alternative)
    let syntax = SyntaxDefinition::load_from_str(BRANCH_SYNTAX, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let ops = ops(&mut state, "let = foo;", &ss);
    let states = stack_states(ops);
    // Should contain keyword.declaration and keyword.operator.assignment
    assert!(
        states.iter().any(|s| s.contains("keyword.declaration")),
        "Expected keyword.declaration scope, got: {:?}",
        states
    );
    assert!(
        states
            .iter()
            .any(|s| s.contains("keyword.operator.assignment")),
        "Expected keyword.operator.assignment scope, got: {:?}",
        states
    );
    assert!(
        states.iter().any(|s| s.contains("constant.other")),
        "Expected constant.other scope, got: {:?}",
        states
    );
}

#[test]
fn branch_fail_backtracks_to_second_alternative() {
    // "hello;" is not a let-statement, should fail and use generic-stmt
    let syntax = SyntaxDefinition::load_from_str(BRANCH_SYNTAX, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let ops = ops(&mut state, "hello;", &ss);
    let states = stack_states(ops);
    // Should contain string.unquoted (generic-stmt), not keyword.declaration
    assert!(
        states.iter().any(|s| s.contains("string.unquoted")),
        "Expected string.unquoted scope, got: {:?}",
        states
    );
    assert!(
        !states.iter().any(|s| s.contains("keyword.declaration")),
        "Should NOT contain keyword.declaration scope, got: {:?}",
        states
    );
}

#[test]
fn branch_fail_after_partial_match() {
    // "let hello;" — starts like a let-stmt ('let' matches) but no '=' follows, so fail
    let syntax = SyntaxDefinition::load_from_str(BRANCH_SYNTAX, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let raw_ops = ops(&mut state, "let hello;", &ss);

    // After backtracking, keyword.declaration must be absent (ops.truncate removes it)
    let states = stack_states(raw_ops.clone());
    assert!(
        !states.iter().any(|s| s.contains("keyword.declaration")),
        "keyword.declaration should be absent after backtrack, got: {:?}",
        states
    );

    // After backtracking, should use generic-stmt
    assert!(
        states.iter().any(|s| s.contains("string.unquoted")),
        "Expected string.unquoted scope after backtrack, got: {:?}",
        states
    );

    // The string.unquoted push must start at position 0 (covers "let hello", not just "hello")
    let unquoted_pos = raw_ops.iter().find_map(|(pos, op)| match op {
        ScopeStackOp::Push(s) if format!("{:?}", s).contains("string.unquoted") => Some(*pos),
        _ => None,
    });
    assert_eq!(
        unquoted_pos,
        Some(0),
        "string.unquoted should start at position 0 after rewind, got: {:?}",
        unquoted_pos
    );
}

#[test]
fn branch_all_alternatives_exhausted() {
    // Test with a syntax where all alternatives fail — should not panic
    let syntax_str = r#"
scope: source.exhaust-test
contexts:
  main:
    - match: '(?=\S)'
      branch_point: bp
      branch: [alt-a, alt-b]
    - match: '\S+'
      scope: fallback.exhaust-test

  alt-a:
    - match: 'AAA'
      scope: alt-a.exhaust-test
      pop: true
    - match: '(?=\S)'
      fail: bp

  alt-b:
    - match: 'BBB'
      scope: alt-b.exhaust-test
      pop: true
    - match: '(?=\S)'
      fail: bp
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    // "xyz" matches neither AAA nor BBB
    let ops = ops(&mut state, "xyz", &ss);
    // Should not panic, and should eventually move past the input
    assert!(!ops.is_empty(), "Expected some ops, got empty");
}

#[test]
fn branch_fail_emits_meta_content_scope() {
    // The second alternative has meta_content_scope; after backtracking,
    // content inside it should have that scope applied.
    let syntax_str = r#"
scope: source.meta-test
contexts:
  main:
    - match: '(?=\S)'
      branch_point: bp
      branch: [try-special, fallback-ctx]

  try-special:
    - match: 'SPECIAL'
      scope: keyword.meta-test
      pop: true
    - match: '(?=\S)'
      fail: bp

  fallback-ctx:
    - meta_content_scope: meta.fallback.meta-test
    - match: '\w+'
      scope: variable.meta-test
      pop: true
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let ops = ops(&mut state, "hello", &ss);
    let states = stack_states(ops);
    // After backtracking to fallback-ctx, "hello" should have meta.fallback scope
    assert!(
        states.iter().any(|s| s.contains("meta.fallback")),
        "Expected meta.fallback.meta-test scope after backtrack, got: {:?}",
        states
    );
    assert!(
        states.iter().any(|s| s.contains("variable.meta-test")),
        "Expected variable.meta-test scope, got: {:?}",
        states
    );
}

#[test]
fn branch_fail_applies_with_prototype() {
    // The branch pattern has with_prototype; after backtracking to the second
    // alternative, the prototype should still be active.
    let syntax_str = r#"
scope: source.proto-test
contexts:
  main:
    - match: '(?=\S)'
      branch_point: bp
      branch: [try-num, fallback-word]
      with_prototype:
        - match: '#'
          scope: comment.proto-test
          pop: true

  try-num:
    - match: '\d+'
      scope: constant.numeric.proto-test
      pop: true
    - match: '(?=\S)'
      fail: bp

  fallback-word:
    - match: '\w+'
      scope: variable.proto-test
    - match: ';'
      pop: true
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    // "abc#" — 'abc' matches fallback-word, '#' should trigger the prototype
    let ops = ops(&mut state, "abc#", &ss);
    let states = stack_states(ops);
    assert!(
        states.iter().any(|s| s.contains("variable.proto-test")),
        "Expected variable.proto-test scope, got: {:?}",
        states
    );
    assert!(
        states.iter().any(|s| s.contains("comment.proto-test")),
        "Expected comment.proto-test from with_prototype after backtrack, got: {:?}",
        states
    );
}

#[test]
fn branch_stack_depth_invalidation() {
    // Test two scenarios:
    // 1. fail fires when stack depth == bp depth (should succeed)
    // 2. fail fires when stack depth < bp depth (should be a no-op)
    let syntax_str = r#"
name: DepthTest
scope: source.depth-test
contexts:
  main:
    - match: '(?=\S)'
      branch_point: bp
      branch: [try-ctx, fallback-ctx]
  try-ctx:
    - match: 'OK'
      scope: try.ok
      set: post-try
    - match: '(?=\S)'
      fail: bp
  post-try:
    - match: 'FAIL'
      fail: bp
    - match: '\w+'
      scope: post.word
      pop: true
  fallback-ctx:
    - match: '.*'
      scope: fallback.content
      pop: true
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);

    // Scenario 1: fail at equal depth should succeed.
    // OK matches in try-ctx, `set` to post-try (depth unchanged since set = pop+push).
    // FAIL fires in post-try at the same depth as bp → backtrack succeeds.
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let line_ops = ops(&mut state, "OK FAIL\n", &ss);
    let states = stack_states(line_ops);
    assert!(
        states.iter().any(|s| s.contains("fallback.content")),
        "fail at equal depth should trigger backtrack to fallback, got: {:?}",
        states
    );
    assert!(
        !states.iter().any(|s| s.contains("try.ok")),
        "try.ok should be absent after backtrack, got: {:?}",
        states
    );

    // Scenario 2: fail at shallower depth should be a no-op.
    // Use a syntax where the branch context pops before fail fires.
    let syntax_str2 = r#"
name: DepthTest2
scope: source.depth-test2
contexts:
  main:
    - match: 'GO'
      branch_point: bp
      branch: [try-ctx2, fallback-ctx2]
    - match: 'FAIL'
      fail: bp
    - match: '.*'
      scope: main.other
  try-ctx2:
    - match: 'OK'
      scope: try.ok2
      pop: true
    - match: '(?=\S)'
      fail: bp
  fallback-ctx2:
    - match: '.*'
      scope: fallback.content2
      pop: true
"#;
    let syntax2 = SyntaxDefinition::load_from_str(syntax_str2, true, None).unwrap();
    let ss2 = link(syntax2);
    let mut state2 = ParseState::new(&ss2.syntaxes()[0]);
    // GO pushes try-ctx2 (depth increases), OK pops back to main (depth decreases).
    // FAIL fires in main at depth < bp depth → no-op.
    let line_ops2 = ops(&mut state2, "GO OK FAIL\n", &ss2);
    let states2 = stack_states(line_ops2);
    assert!(
        !states2.iter().any(|s| s.contains("fallback.content2")),
        "fail should be a no-op when stack is shallower than branch point, got: {:?}",
        states2
    );
    assert!(
        states2.iter().any(|s| s.contains("try.ok2")),
        "expected try.ok2 from first alternative, got: {:?}",
        states2
    );
}

#[test]
fn branch_nested_overlapping_branch_points() {
    // Two branch points active simultaneously. The inner one fails,
    // the outer should remain valid.
    let syntax_str = r#"
name: NestedTest
scope: source.nested-test
contexts:
  main:
    - match: '(?=\S)'
      branch_point: outer
      branch: [outer-try, outer-fallback]
  outer-try:
    - match: 'A'
      scope: outer.a
      set: inner-branch
    - match: '(?=\S)'
      fail: outer
  inner-branch:
    - match: '(?=\S)'
      branch_point: inner
      branch: [inner-try, inner-fallback]
  inner-try:
    - match: 'X'
      scope: inner.x
      pop: true
    - match: '(?=\S)'
      fail: inner
  inner-fallback:
    - match: '\w+'
      scope: inner.fallback
      pop: true
  outer-fallback:
    - match: '.*'
      scope: outer.fallback
      pop: true
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);

    // "A B" — A matches outer-try, B fails inner → inner-fallback matches B
    let line_ops = ops(&mut state, "A B\n", &ss);
    let states = stack_states(line_ops);
    assert!(
        states.iter().any(|s| s.contains("inner.fallback")),
        "expected inner.fallback after inner branch fail, got: {:?}",
        states
    );
    assert!(
        !states.iter().any(|s| s.contains("outer.fallback")),
        "outer branch should not have failed, got: {:?}",
        states
    );
}

#[test]
fn branch_fail_nonexistent_name() {
    // `fail: nonexistent` should be a silent no-op — no panic, parsing continues.
    let syntax_str = r#"
name: NoNameTest
scope: source.noname-test
contexts:
  main:
    - match: '\w+'
      scope: word.noname-test
    - match: '(?=;)'
      fail: nonexistent
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);

    // The key assertion is that this doesn't panic
    let line_ops = ops(&mut state, "hello;\n", &ss);
    let states = stack_states(line_ops);
    assert!(
        states.iter().any(|s| s.contains("word.noname-test")),
        "expected word.noname-test, got: {:?}",
        states
    );
}

#[test]
fn is_speculative_reflects_branch_state() {
    // Kills: L306 replace is_speculative -> true / false / delete !
    // is_speculative must be true while inside a branch_point and false otherwise.
    // We use a syntax where both alternatives fail, so the branch point is
    // fully exhausted and removed.
    let syntax_str = r#"
name: SpeculativeTest
scope: source.spec-test
contexts:
  main:
    - match: '(?=\S)'
      branch_point: bp
      branch: [alt-a, alt-b]
    - match: '\S+'
      scope: fallback.spec-test

  alt-a:
    - match: 'AAA'
      scope: alt-a.spec-test
      pop: true
    - match: '(?=\S)'
      fail: bp

  alt-b:
    - match: 'BBB'
      scope: alt-b.spec-test
      pop: true
    - match: '(?=\S)'
      fail: bp
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);

    // Before any branch: not speculative
    assert!(
        !state.is_speculative(),
        "should not be speculative before branch_point is created"
    );

    // "AAA" matches first alternative, so branch stays open (could still fail)
    let _ = state.parse_line("AAA\n", &ss).unwrap();
    // Branch point is created then first alt succeeds — but bp stays until
    // explicitly removed.  Since AAA matched and popped, bp is still there.
    // Actually let's test with a failing input instead:
    let mut state2 = ParseState::new(&ss.syntaxes()[0]);
    assert!(!state2.is_speculative());

    // "xyz" matches neither AAA nor BBB: both alternatives fail, bp exhausted & removed
    let _ = state2.parse_line("xyz\n", &ss).unwrap();
    assert!(
        !state2.is_speculative(),
        "should not be speculative after all alternatives exhausted"
    );

    // Now test it IS speculative mid-branch: use a cross-line syntax
    let syntax_str2 = r#"
name: SpecCross
scope: source.spec-cross
contexts:
  main:
    - match: 'TRY'
      branch_point: bp
      branch: [try-ctx, fallback-ctx]
    - match: '.*'
      scope: main.other
  try-ctx:
    - match: '\n'
    - match: 'FAIL'
      fail: bp
    - match: '\w+'
      scope: try.word
      pop: true
  fallback-ctx:
    - match: '.*'
      scope: fallback.content
      pop: true
"#;
    let syntax2 = SyntaxDefinition::load_from_str(syntax_str2, true, None).unwrap();
    let ss2 = link(syntax2);
    let mut state3 = ParseState::new(&ss2.syntaxes()[0]);
    assert!(!state3.is_speculative());

    let _ = state3.parse_line("TRY\n", &ss2).unwrap();
    assert!(
        state3.is_speculative(),
        "should be speculative after branch_point creation"
    );
}

#[cfg(feature = "default-onig")]
#[test]
fn pop_n_branch_point_keeps_bp_so_alt_fail_unwinds_meta_scope() {
    // Real-syntax repro for the Java class-extends annotation leak:
    // `class T extends a.@b.c Foo {}`. The branch_point on
    // `annotation-qualified-identifier-name`'s `pop: 2 + branch_point:
    // annotation-qualified-parameters` was being pruned by perform_op's
    // post-Set retain (`bp.stack_depth <= final_len` ignored
    // `bp.pop_count`), so the branch's first alt — which has
    // `meta_content_scope: meta.annotation.identifier.java` — was
    // never failed-out, leaking `meta.annotation.identifier.java`
    // past every nested-annotation extends path in the Java suite.
    use crate::parsing::SyntaxSet;
    let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
    let syntax = ss
        .find_syntax_by_path("Packages/Java/Java.sublime-syntax")
        .unwrap();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    for line in ["class T extends a.@b.c Foo {}\n"] {
        let out = state.parse_line(line, &ss).expect("parse");
        for (_, op) in &out.ops {
            let _ = stack.apply(op);
        }
    }
    let ann = Scope::new("meta.annotation.identifier.java").unwrap();
    assert!(
        !stack.as_slice().contains(&ann),
        "meta.annotation.identifier.java leaked past `@b.c` annotation \
             into the outer extends path; final stack: {:?}",
        stack,
    );
}

#[cfg(feature = "default-onig")]
#[test]
fn exhausted_branch_point_falls_through_to_parent_next_rule() {
    // Java's `$x ;` at top level: the `declarations` branch_point's
    // zero-width `(?=[\p{L}_$@<])` lookahead matches `$`. All five
    // alternatives (class/enum/interface/variable/method) fail
    // because `$x` isn't a valid declaration. ST then falls through
    // to the `java` context's NEXT rule (`else-expressions`), which
    // pushes an `expression` chain that scopes `$x` as
    // `meta.variable.identifier.java variable.other.java`. Syntect
    // previously advanced one character past the lookahead, letting
    // the next iteration's regex set match `package` / `class` etc.
    // in the middle of identifiers like `package$` or `$package`.
    //
    // After this fix, exhausting a branch_point at a position
    // rewinds the cursor to that position and marks the
    // branch_point's name as skipped — so the parent context's
    // remaining rules get a chance to fire at the same cursor.
    use crate::parsing::SyntaxSet;
    let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
    let syntax = ss
        .find_syntax_by_path("Packages/Java/Java.sublime-syntax")
        .unwrap();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let dollar_id = "$x ;\n";
    let out = state.parse_line(dollar_id, &ss).expect("parse");
    for (_, op) in &out.ops {
        let _ = stack.apply(op);
    }
    let var_id = Scope::new("meta.variable.identifier.java").unwrap();
    let var_other = Scope::new("variable.other.java").unwrap();
    // `$x` itself is fully popped at `;`; reconstruct the per-byte
    // scope by walking ops up to byte 1 (`x`) and confirm the
    // identifier scope was active there.
    let mut mid = ScopeStack::new();
    for (pos, op) in &out.ops {
        if *pos > 1 {
            break;
        }
        let _ = mid.apply(op);
    }
    assert!(
        mid.as_slice().contains(&var_id),
        "expected meta.variable.identifier.java active over `$x`; got: {:?}",
        mid,
    );
    assert!(
        mid.as_slice().contains(&var_other),
        "expected variable.other.java active over `$x`; got: {:?}",
        mid,
    );
}
