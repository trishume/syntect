//! Native embed/escape behavior, including escape/branch interactions.

use super::*;

#[test]
fn can_parse_prototype_with_embed() {
    let syntax = r#"
name: Javadoc
scope: text.html.javadoc
contexts:
  prototype:
    - match: \*
      scope: punctuation.definition.comment.javadoc

  main:
    - meta_include_prototype: false
    - match: /\*\*
      scope: comment.block.documentation.javadoc punctuation.definition.comment.begin.javadoc
      embed: contents
      embed_scope: comment.block.documentation.javadoc text.html.javadoc
      escape: \*/
      escape_captures:
        0: comment.block.documentation.javadoc punctuation.definition.comment.end.javadoc

  contents:
    - match: ''
"#;

    let syntax = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
    expect_scope_stacks_with_syntax("/** * */", &["<comment.block.documentation.javadoc>, <punctuation.definition.comment.begin.javadoc>", "<comment.block.documentation.javadoc>, <text.html.javadoc>, <punctuation.definition.comment.javadoc>", "<comment.block.documentation.javadoc>, <punctuation.definition.comment.end.javadoc>"], syntax);
}

#[test]
fn v2_embed_scope_replaces_skips_meta_content_pop_on_exit() {
    // Kills: L912 replace >= with < (version >= 2)
    //        L913 replace >= with < (stack.len() >= 2)
    //        L915 replace - with / (stack.len() - 2)
    // When a v2 syntax uses embed with embed_scope, the escape context
    // has embed_scope_replaces=true.  On pop, the meta_content_scope of
    // the escape context should NOT be popped because it was never pushed.
    // If the version check is wrong, we'd get an extra Pop.
    use crate::parsing::ScopeStack;

    // The host pushes two intermediate contexts before embedding so that
    // the stack depth is 5 when the escape fires:
    //   [main, wrapper-a, wrapper-b, escape, embedded]
    // This distinguishes stack.len()-2 (=3, escape) from stack.len()/2
    // (=2, wrapper-b), catching the L915 `-` → `/` mutation.
    let host = SyntaxDefinition::load_from_str(
        r#"
name: V2SkipHost
scope: source.v2skip
file_extensions: [v2skip]
version: 2
contexts:
  main:
    - match: '(?=<)'
      push: wrapper-a
    - match: '\w+'
      scope: word.v2skip
  wrapper-a:
    - match: '(?=<)'
      push: wrapper-b
  wrapper-b:
    - match: '<<'
      embed: scope:source.v2skipemb
      embed_scope: meta.embedded.v2skip
      escape: '>>'
      escape_captures:
        0: punctuation.end.v2skip
"#,
        true,
        None,
    )
    .unwrap();

    let embedded = SyntaxDefinition::load_from_str(
        r#"
name: V2SkipEmb
scope: source.v2skipemb
file_extensions: [v2skipemb]
version: 2
contexts:
  main:
    - meta_content_scope: content.v2skipemb
    - match: '\w+'
      scope: keyword.v2skipemb
"#,
        true,
        None,
    )
    .unwrap();

    let mut builder = SyntaxSetBuilder::new();
    builder.add(host);
    builder.add(embedded);
    let ss = builder.build();

    let syntax = ss.find_syntax_by_name("V2SkipHost").unwrap();
    let mut state = ParseState::new(syntax);
    let raw_ops = state.parse_line("<<x>> hello\n", &ss).unwrap().ops;

    // Build scope stack through all ops and verify it ends clean.
    // If the skip logic is broken (mutations on L912-L915), an extra Pop
    // for meta_content_scope is generated, which pops a scope that was
    // never pushed, corrupting the stack.
    let mut scope_stack = ScopeStack::new();
    for (_, op) in &raw_ops {
        scope_stack
            .apply(op)
            .expect("applying op should not fail — extra Pop means the skip logic is broken");
    }
    // After ">> hello\n", we should be back in main with source.v2skip
    // as the only remaining scope (everything else was popped).
    // If the skip logic is wrong, source.v2skip would be popped too.
    let final_scopes: Vec<String> = scope_stack
        .as_slice()
        .iter()
        .map(|s| format!("{:?}", s))
        .collect();
    assert!(
        final_scopes.iter().any(|s| s.contains("source.v2skip")),
        "source.v2skip should remain on stack after all ops, got: {:?}",
        final_scopes
    );
}

#[test]
fn v2_host_embedding_v1_guest_skips_meta_content_pop_on_escape() {
    // Regression for the Rails html.erb syntest cluster: when a v2 host
    // uses `embed:` + `embed_scope:` to pull in a v1 guest grammar (e.g.
    // Rails/HTML embedding Ruby), `embed_scope_replaces` is set on the
    // wrapper context. On escape, the embedded guest's meta_content_scope
    // must be skipped — it was never pushed on the way in.
    //
    // The exec_escape skip logic was gated on
    // `current_syntax_version() >= 2`, which reads the version from the
    // top-of-stack context. That is the *guest* (Ruby, v1), not the host,
    // so the gate evaluated false and a spurious Pop fired for a scope
    // that was never pushed, misaligning every scope on the stack for
    // the remainder of the host context.
    use crate::parsing::ScopeStack;

    let host = SyntaxDefinition::load_from_str(
        r#"
name: V2HostV1Guest
scope: source.v2host
file_extensions: [v2host]
version: 2
contexts:
  main:
    - match: '<<'
      embed: scope:source.v1guest
      embed_scope: meta.embedded.v2host
      escape: '>>'
      escape_captures:
        0: punctuation.end.v2host
    - match: '\w+'
      scope: word.v2host
"#,
        true,
        None,
    )
    .unwrap();

    // Guest omits `version:` — defaults to 1. Its `scope:` lands in the
    // main context's meta_content_scope (source.v1guest), which the
    // v2 embed_scope_replaces suppresses on push. The escape must
    // symmetrically suppress it on pop.
    let guest = SyntaxDefinition::load_from_str(
        r#"
name: V1Guest
scope: source.v1guest
file_extensions: [v1guest]
contexts:
  main:
    - match: '\w+'
      scope: keyword.v1guest
"#,
        true,
        None,
    )
    .unwrap();

    let mut builder = SyntaxSetBuilder::new();
    builder.add(host);
    builder.add(guest);
    let ss = builder.build();

    let syntax = ss.find_syntax_by_name("V2HostV1Guest").unwrap();
    let mut state = ParseState::new(syntax);
    let raw_ops = state.parse_line("<<x>> hello\n", &ss).unwrap().ops;

    // Before the fix, the escape emits a Pop for guest main's mcs even
    // though it was never pushed. Subsequent Pops then strip scopes
    // that should have survived. Applying the op stream must not fail,
    // and source.v2host must remain on the stack at the end.
    let mut scope_stack = ScopeStack::new();
    for (_, op) in &raw_ops {
        scope_stack.apply(op).expect(
            "applying op stream must succeed — a spurious Pop indicates the skip was gated \
                 on the guest's syntax version instead of the embed_scope_replaces flag",
        );
    }
    let final_scopes: Vec<String> = scope_stack
        .as_slice()
        .iter()
        .map(|s| format!("{:?}", s))
        .collect();
    assert!(
        final_scopes.iter().any(|s| s.contains("source.v2host")),
        "source.v2host should remain after escape; got: {:?}",
        final_scopes
    );
}

#[test]
fn nested_embed_outer_escape_wins() {
    // Inner embed's escape must not fire before outer embed's escape.
    // The outer escape at position 3 ("END") should take precedence over
    // the inner escape at position 5 ("zzz"), truncating the search region.
    let syntax = r#"
name: NestedEmbed
scope: source.nested-embed
contexts:
  main:
    - match: 'OUTER'
      embed: mid
      escape: 'END'
      escape_captures:
        0: keyword.escape.outer
    - match: '.'
      scope: main.char

  mid:
    - match: 'INNER'
      embed: deep
      escape: 'zzz'
      escape_captures:
        0: keyword.escape.inner
    - match: '.'
      scope: mid.char

  deep:
    - match: '.'
      scope: deep.char
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);

    // Line 1: enter outer embed, then inner embed
    let out1 = state.parse_line("OUTERINNER\n", &ss).expect("line 1");
    debug_print_ops("OUTERINNER\n", &out1.ops);

    // Line 2: "xxENDzzzAFTER" — outer escape "END" at pos 2 must fire before
    // inner escape "zzz" at pos 5. After outer escape fires, we're back in main.
    let out2 = state.parse_line("xxENDzzzAFTER\n", &ss).expect("line 2");
    let states = stack_states(out2.ops);
    println!("states: {:?}", states);

    // The outer escape scope must appear
    assert!(
        states.iter().any(|s| s.contains("keyword.escape.outer")),
        "outer escape must fire, got: {:?}",
        states
    );
    // The inner escape scope must NOT appear (outer wins)
    assert!(
        !states.iter().any(|s| s.contains("keyword.escape.inner")),
        "inner escape must not fire when outer escape is earlier, got: {:?}",
        states
    );
    // After the outer escape, "zzzAFTER" should be parsed in main context
    assert!(
        states.iter().any(|s| s.contains("main.char")),
        "after outer escape we should be in main, got: {:?}",
        states
    );
}

#[test]
fn embed_escape_with_backref_at_parse_time() {
    // The escape pattern uses \1 to backreference the opening delimiter.
    // Verify that the resolved regex correctly matches at parse time.
    let syntax = r#"
name: BackrefEscape
scope: source.backref-escape
contexts:
  main:
    - match: '(<<|>>)'
      scope: punctuation.open
      embed: inner
      escape: '\1'
      escape_captures:
        0: punctuation.close
    - match: '.'
      scope: main.char

  inner:
    - match: '.'
      scope: inner.char
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
    let ss = link(syntax);

    // Test 1: "<<" opens, ">>" should NOT close it, "<<" should close it
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let out1 = state.parse_line("<<>>stuff<<after\n", &ss).expect("line 1");
    let states = stack_states(out1.ops);
    println!("backref states: {:?}", states);

    // The opening << should push punctuation.open
    assert!(
        states.iter().any(|s| s.contains("punctuation.open")),
        "expected punctuation.open, got: {:?}",
        states
    );
    // ">>" should be parsed as inner.char (not as escape)
    assert!(
        states.iter().any(|s| s.contains("inner.char")),
        ">> should be inner.char since escape is <<, got: {:?}",
        states
    );
    // "<<" at pos 9 should fire as escape (punctuation.close)
    assert!(
        states.iter().any(|s| s.contains("punctuation.close")),
        "matching << should trigger escape, got: {:?}",
        states
    );
    // After escape, "after" should be in main
    assert!(
        states.iter().any(|s| s.contains("main.char")),
        "after escape we should be in main, got: {:?}",
        states
    );
}

#[test]
fn embed_escape_cross_line() {
    // Embed on line 1, content on line 2, escape on line 3.
    // Verifies that escape_stack persists across parse_line calls.
    let syntax = r#"
name: CrossLineEscape
scope: source.cross-line-escape
contexts:
  main:
    - match: 'BEGIN'
      scope: keyword.begin
      embed: body
      escape: 'STOP'
      escape_captures:
        0: keyword.stop
    - match: '.'
      scope: main.char

  body:
    - match: '.'
      scope: body.char
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);

    // Line 1: embed begins
    let out1 = state.parse_line("BEGIN\n", &ss).expect("line 1");
    let states1 = stack_states(out1.ops);
    assert!(
        states1.iter().any(|s| s.contains("keyword.begin")),
        "line 1 should have keyword.begin, got: {:?}",
        states1
    );

    // Line 2: content inside the embed
    let out2 = state.parse_line("hello\n", &ss).expect("line 2");
    let states2 = stack_states(out2.ops);
    assert!(
        states2.iter().any(|s| s.contains("body.char")),
        "line 2 should be body content, got: {:?}",
        states2
    );

    // Line 3: escape fires
    let out3 = state.parse_line("STOPafter\n", &ss).expect("line 3");
    let states3 = stack_states(out3.ops);
    assert!(
        states3.iter().any(|s| s.contains("keyword.stop")),
        "line 3 should have escape keyword.stop, got: {:?}",
        states3
    );
    assert!(
        states3.iter().any(|s| s.contains("main.char")),
        "after escape on line 3, should be in main, got: {:?}",
        states3
    );
}

#[test]
fn embed_inside_branch_then_fail_restores_escape_stack() {
    // An embed inside a branch alternative pushes to escape_stack.
    // When fail fires, the escape_stack must be restored (the embed's
    // escape entry must be removed). The fallback alternative stays on
    // the stack (no pop) so any stale escape entry would survive to
    // the next line, where it would incorrectly fire.
    let syntax = r#"
name: EmbedBranchFail
scope: source.embed-branch
contexts:
  main:
    - match: 'START'
      branch_point: bp
      branch: [try-embed, fallback]
    - match: '.'
      scope: main.char

  try-embed:
    - match: 'EMB'
      embed: embedded
      escape: 'ESC'
      escape_captures:
        0: keyword.escape

  fallback:
    # No pop — stays on the stack so a stale escape entry would persist
    - match: '\w+'
      scope: fallback.matched
    - match: '\n'

  embedded:
    - match: 'FAIL'
      fail: bp
    - match: '.'
      scope: embedded.char
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);

    // "START" triggers branch, tries try-embed first.
    // "EMB" enters the embed (pushing escape entry for "ESC").
    // "FAIL" fires `fail: bp`, which must restore escape_stack and
    // replay under fallback alternative.
    let out = state
        .parse_line("STARTEMBFAIL\n", &ss)
        .expect("parse failed");
    let states = stack_states(out.ops);
    println!("embed+branch+fail states: {:?}", states);

    // After backtracking, fallback should match
    assert!(
        states.iter().any(|s| s.contains("fallback.matched")),
        "fallback should match after fail, got: {:?}",
        states
    );

    // Parse another line — "ESC" must NOT trigger the escape (it was
    // from a reverted branch). Without proper escape_stack restoration,
    // the stale escape entry would fire here.
    let out2 = state.parse_line("xESCy\n", &ss).expect("line 2");
    let states2 = stack_states(out2.ops);
    assert!(
        !states2.iter().any(|s| s.contains("keyword.escape")),
        "stale escape must not fire after branch revert, got: {:?}",
        states2
    );
}

#[test]
fn zero_width_escape_at_branch_fail_terminates() {
    // Sentinel for #650. The unbounded cycle reproduced on
    // testdata/Packages/Perl/syntax_test_perl.pl (zero-width
    // POD escape inside embedded JSON/HTML/SQL branch points
    // wedging the parser for 40+ minutes) only emerges across
    // multi-line embedded-branch cascades on real fixtures —
    // single-line synthetic shapes don't trigger it because
    // escape takes strict precedence and pops the embed on
    // first hit. This test covers the basic bookkeeping
    // (per-iteration clear, escape application, threshold
    // counting) for the embed + zero-width escape + branch
    // fail combination. The Perl-file syntest run is the
    // integration witness for the unbounded-cycle fix.
    let syntax = r#"
name: ZeroWidthEscapeLoop
scope: source.zwe
contexts:
  main:
    - match: 'BEGIN'
      scope: keyword.begin
      embed: body
      escape: '(?=END)'
    - match: '.'
      scope: main.char

  body:
    - match: 'X'
      branch_point: bp
      branch: [try-fail, fallback]
    - match: '.'
      scope: body.char

  try-fail:
    - match: 'F'
      fail: bp

  fallback:
    - match: '\w+'
      scope: fallback.matched
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);

    // "BEGIN" enters embed. "X" opens branch. "F" fires fail, which
    // restores the escape_stack snapshot. Position then re-encounters
    // the zero-width escape lookahead at "END". Without the fix, the
    // exec_escape ↔ branch-fail cycle does not advance.
    let out = state.parse_line("BEGINXFEND\n", &ss).expect("parse failed");
    let states = stack_states(out.ops);
    assert!(
        states.iter().any(|s| s.contains("fallback.matched")),
        "fallback alt should match after escape-induced cycle is broken; got: {:?}",
        states
    );
}

#[test]
fn erb_escape_captures() {
    let ss = SyntaxSet::load_defaults_newlines();
    let syntax = ss.find_syntax_by_extension("erb").unwrap();
    let mut state = ParseState::new(syntax);
    let mut scope_stack = ScopeStack::new();
    let ops = state.parse_line("<%= puts \"hi\" %>\n", &ss).unwrap();
    eprintln!("ERB line ops:");
    for (pos, op) in &ops.ops {
        scope_stack.apply(op).ok();
        eprintln!(
            "  pos={} op={:?}  stack={:?}",
            pos,
            op,
            scope_stack.as_slice()
        );
    }
    let stack_str = format!("{:?}", scope_stack.as_slice());
    // After the line, the %> should have fired the escape
    // and we should be back in HTML context
    assert!(
        !stack_str.contains("source.ruby"),
        "Expected Ruby embed to have ended, got: {}",
        stack_str
    );
}

#[test]
fn embed_js_in_html() {
    let ss = SyntaxSet::load_defaults_newlines();

    for ext in &["html", "erb"] {
        let syntax = ss.find_syntax_by_extension(ext).unwrap();
        let mut state = ParseState::new(syntax);
        let mut scope_stack = ScopeStack::new();
        state
            .parse_line("<script type=\"text/javascript\">\n", &ss)
            .unwrap()
            .ops
            .iter()
            .for_each(|(_, op)| {
                scope_stack.apply(op).ok();
            });
        let ops = state.parse_line("var x = 5;\n", &ss).unwrap();
        for (_, op) in &ops.ops {
            scope_stack.apply(op).ok();
        }
        let stack_str = format!("{:?}", scope_stack.as_slice());
        assert!(
            stack_str.contains("source.js"),
            "Extension {}: expected source.js in scope stack, got: {}",
            ext,
            stack_str
        );
    }
}

#[test]
fn v2_pop_embed_suppresses_cur_meta_scope_on_match() {
    // `pop: N + embed:` trigger text must NOT carry the popped context's
    // `meta_scope` through, unlike `pop: N + set:` which preserves both
    // cur's and target's meta_scope on the match. Probe against ST confirms:
    //
    //   <tag>hi</tag>            (pop+embed)
    //   col 4 '>'                -> ['source.host', 'end.scope']            (cur ms gone)
    //   col 5 'h' (body)         -> ['source.host', 'embed.scope', 'guest.meta']
    //
    //   <tag>after               (pop+set — contrast)
    //   col 4 '>'                -> ['source.host', 'meta.a', 'after.meta', 'end.scope']
    //
    // Without this guard, syntect emitted
    //   [source.host, meta.a, meta.a, end.scope]
    // because the rule's explicit scope atom shadowed cur.meta_scope onto
    // itself on the trigger text — observed as 5 duplicated
    // `meta.tag.jsp.*.begin.html` atoms on `<jsp:declaration>`/ expression/
    // scriptlet's `>` in `syntax_test_jsp.jsp`.
    let host = SyntaxDefinition::load_from_str(
        r#"
name: PopEmbedHost
scope: source.popembed
file_extensions: [popembed]
version: 2
contexts:
  main:
    - match: '<tag'
      scope: begin.scope
      push: tag-attrs
  tag-attrs:
    - meta_include_prototype: false
    - meta_scope: meta.a
    - match: '>'
      scope: meta.a end.scope
      pop: 1
      embed: scope:source.popembedguest
      embed_scope: embed.scope
      escape: '(?=</tag)'
"#,
        true,
        None,
    )
    .unwrap();
    let guest = SyntaxDefinition::load_from_str(
        r#"
name: PopEmbedGuest
scope: source.popembedguest
version: 2
hidden: true
contexts:
  main:
    - meta_scope: guest.meta
    - match: '\w+'
      scope: word.guest
"#,
        true,
        None,
    )
    .unwrap();

    let mut builder = SyntaxSetBuilder::new();
    builder.add(host);
    builder.add(guest);
    let ss = builder.build();
    let syntax = ss.find_syntax_by_name("PopEmbedHost").unwrap();
    let mut state = ParseState::new(syntax);
    let ops = state.parse_line("<tag>hi</tag>\n", &ss).unwrap().ops;

    // Walk (range, op) pairs; after applying each op, snapshot the stack
    // keyed by the character position we're at. The `>` match occupies
    // col 4, so we expect the post-op snapshot at that position to have
    // exactly ONE `meta.a` atom, not two.
    use crate::easy::ScopeRangeIterator;
    let line = "<tag>hi</tag>\n";
    let mut stack = ScopeStack::new();
    let mut at_gt: Option<Vec<String>> = None;
    for (range, op) in ScopeRangeIterator::new(&ops, line) {
        stack.apply(op).expect("op stream must apply cleanly");
        // Capture the stack state for the character range covering the `>`
        // trigger (col 4..5, the match text of the pop+embed rule).
        if range.start <= 4 && 4 < range.end {
            at_gt = Some(
                stack
                    .as_slice()
                    .iter()
                    .map(|s| format!("{:?}", s))
                    .collect(),
            );
        }
    }
    let at_gt = at_gt.expect("range covering `>` must exist in op stream");
    let meta_a_count = at_gt.iter().filter(|s| s.contains("meta.a")).count();
    assert_eq!(
        meta_a_count, 1,
        "match text of pop+embed must carry exactly one `meta.a` atom \
             (from the rule's explicit scope); cur_context.meta_scope must \
             not stack a second copy on top. Got stack: {:?}",
        at_gt
    );
    // And `end.scope` must be the top of the stack (the match's second
    // explicit atom) — if the ordering shifted we'd see a different trailer.
    assert!(
        at_gt
            .last()
            .map(|s| s.contains("end.scope"))
            .unwrap_or(false),
        "stack top on `>` must be `end.scope`, got: {:?}",
        at_gt
    );
}

#[test]
fn pop_n_embed_drops_deeper_meta_scope_at_trigger() {
    // `pop: N + embed:` is **lookahead** per ST docs: "for `push`,
    // `embed` and `branch` actions, the pop treats the match as if
    // it were a lookahead." With stack `main -> mid -> inner` (each
    // with non-empty meta_scope), a `pop: 2 + embed:` rule's trigger
    // token must NOT carry `meta.mid` or `meta.inner` — both popped
    // frames' meta_scope are excluded.
    //
    // Companion to `v2_pop_embed_suppresses_cur_meta_scope_on_match`
    // (which covers single-pop cur suppression) and to bug-#1's
    // `pop_n_push_with_target_meta_scope_drops_deeper_meta_scope_at_trigger`
    // (the analogous Push case). Push, Branch and Embed share the
    // same lookahead semantics.
    let host = SyntaxDefinition::load_from_str(
        r#"
name: PopEmbedDeep
scope: source.popembeddeep
file_extensions: [popembeddeep]
version: 2
contexts:
  main:
    - match: 'a'
      scope: p.a
      push: mid

  mid:
    - meta_scope: meta.mid
    - match: 'b'
      scope: p.b
      push: inner

  inner:
    - meta_include_prototype: false
    - meta_scope: meta.inner
    - match: 'c'
      scope: p.c
      pop: 2
      embed: scope:source.popembeddeepguest
      escape: '(?=$)'
"#,
        true,
        None,
    )
    .unwrap();
    let guest = SyntaxDefinition::load_from_str(
        r#"
name: PopEmbedDeepGuest
scope: source.popembeddeepguest
hidden: true
version: 2
contexts:
  main:
    - meta_scope: meta.guest
    - match: '\w+'
      scope: word.guest
"#,
        true,
        None,
    )
    .unwrap();

    let mut builder = SyntaxSetBuilder::new();
    builder.add(host);
    builder.add(guest);
    let ss = builder.build();
    let syntax = ss.find_syntax_by_name("PopEmbedDeep").unwrap();
    let mut state = ParseState::new(syntax);
    let line = "abc\n";
    let ops = state.parse_line(line, &ss).unwrap().ops;

    use crate::easy::ScopeRangeIterator;
    let mut stack = ScopeStack::new();
    let mut at_c: Option<Vec<String>> = None;
    for (range, op) in ScopeRangeIterator::new(&ops, line) {
        stack.apply(op).expect("op stream must apply cleanly");
        if range.start <= 2 && 2 < range.end {
            at_c = Some(
                stack
                    .as_slice()
                    .iter()
                    .map(|s| format!("{:?}", s))
                    .collect(),
            );
        }
    }
    let at_c = at_c.expect("range covering `c` must exist in op stream");
    assert!(
        !at_c.iter().any(|s| s.contains("meta.mid")),
        "deeper popped frame's meta_scope (meta.mid) must be \
             excluded from the `c` trigger of `pop: 2 + embed:` (ST \
             lookahead semantics): {:?}",
        at_c
    );
    assert!(
        !at_c.iter().any(|s| s.contains("meta.inner")),
        "cur context's meta_scope (meta.inner) must be excluded \
             from the `c` trigger of `pop: 2 + embed:` (ST embed \
             quirk + lookahead): {:?}",
        at_c
    );
    assert!(
        at_c.iter().any(|s| s.contains("p.c")),
        "rule's explicit scope (p.c) must be present at the `c` \
             trigger: {:?}",
        at_c
    );
}

#[test]
fn escape_prune_keeps_pop_n_branch_point_so_fail_still_rewinds() {
    // exec_escape variant of the `pop: N + branch_point` false-prune
    // (same class as the perform_op post-Set retain fixed by the test
    // above): `bp.stack_depth` snapshots the *pre-pop* depth, so after
    // an embed escape pops back to the alt frame's depth
    // (`stack_depth - pop_count + 1`), the old exec_escape retain
    // (`bp.stack_depth <= stack.len()`) dropped the still-valid bp.
    // The subsequent `fail` then found no record and became a silent
    // no-op — alt 0's meta_content_scope leaked and alt 1 never ran.
    //
    // Shape: `pop: 2 + branch_point` from depth 3 leaves the alt at
    // depth 2; alt 0 enters an embed (escape entry depth 2, body at
    // depth 3); the escape fires and exec_escape pops to depth 2,
    // where `stack.len() (2) < bp.stack_depth (3)` but the alt frame
    // is still present (`2 > 3 - 2`); then `fail: bp` must rewind
    // onto alt 1.
    let syntax_str = r#"
name: EscapePrunePopN
scope: source.escprunepopn
version: 2
contexts:
  main:
    - match: 'a'
      scope: p.a
      push: outer

  outer:
    - meta_scope: outer.test
    - match: 'b'
      scope: p.b
      push: inner

  inner:
    - match: 'X'
      pop: 2
      branch_point: bp
      branch: [alt0, alt1]
    - match: '.'
      scope: inner.char

  alt0:
    - meta_content_scope: leak.meta
    - match: 'BEGIN'
      embed: body
      escape: 'END'
    - match: 'F'
      fail: bp
    - match: '.'
      scope: alt0.char

  alt1:
    - match: '\w+'
      scope: alt1.matched
    - match: '.'
      scope: alt1.char

  body:
    - match: '.'
      scope: body.char
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);

    // "ab" builds depth 3 (main → outer → inner). "X" fires
    // `pop: 2 + branch_point: bp`, pushing alt0 at depth 2. "BEGIN"
    // enters the embed, "y" is body content, "END" fires the escape
    // (exec_escape pops back to depth 2 — the prune under test).
    // "F" fires `fail: bp`: the rewind re-parses "BEGINyENDF" under
    // alt1, whose `\w+` swallows it as `alt1.matched`, and the ops
    // truncation removes every trace of alt0's `leak.meta`.
    let line_ops = ops(&mut state, "abXBEGINyENDF\n", &ss);
    let states = stack_states(line_ops);
    assert!(
        states.iter().any(|s| s.contains("alt1.matched")),
        "fail: bp after the embed escape must rewind onto alt1 \
             (bp falsely pruned by exec_escape?); got: {:?}",
        states
    );
    assert!(
        !states.iter().any(|s| s.contains("leak.meta")),
        "alt0's meta_content_scope leaked past the fail rewind; got: {:?}",
        states
    );
}

/// Regression guard for the `embed_scope`-replaces / inner Set
/// interaction. With a wrapper context that pushes 3 mcs scopes
/// and `embed_scope_replaces=true`, the embedded syntax's first
/// `set:` rule must not drop the topmost wrapper scope — its mcs
/// pop must be skipped because the embedded main's mcs was never
/// pushed.
#[test]
fn embed_scope_replaces_preserves_wrapper_mcs_across_inner_set() {
    let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
    let md = ss
        .find_syntax_by_scope(Scope::new("text.html.markdown").unwrap())
        .expect("Markdown loaded");
    let mut state = ParseState::new(md);
    let mut stack = ScopeStack::new();
    for line in ["```bash\n", "#!/usr/bin/env bash\n"] {
        let out = state.parse_line(line, &ss).expect("parse");
        for (_, op) in &out.ops {
            let _ = stack.apply(op);
        }
    }
    // Last `embed_scope` atom of Markdown's `fenced-code-block-bash-content`
    // as of Packages v4202.
    let bash = Scope::new("source.shell.bash.embedded.markdown").unwrap();
    assert!(
        stack.as_slice().contains(&bash),
        "source.shell.bash.embedded.markdown (wrapper's last embed_scope \
             token) must be on the stack after the embedded syntax's first \
             `set:` fires; stack: {:?}",
        stack
    );
}

/// Regression: an `embed: scope:source.guest#leaf` (with `#fragment`)
/// must NOT mark the wrapper as `embed_scope_replaces`. The fragment
/// context's `meta_content_scope` is independent of the syntax's
/// top-level scope; suppressing it strips a real grammar atom and the
/// next `clear_scopes:` then bites the wrapper instead. Observed on
/// Python's PEP 723 inline TOML (`#toml`).
#[test]
fn fragment_embed_preserves_target_meta_content_scope() {
    use crate::parsing::ScopeStack;

    let host = SyntaxDefinition::load_from_str(
        r#"
name: FragHost
scope: source.fraghost
file_extensions: [fraghost]
version: 2
contexts:
  main:
    - match: '<<'
      embed: scope:source.fragguest#leaf
      embed_scope: wrapper.atom
      escape: '>>'
"#,
        true,
        None,
    )
    .unwrap();

    let guest = SyntaxDefinition::load_from_str(
        r#"
name: FragGuest
scope: source.fragguest
file_extensions: [fragguest]
version: 2
hidden: true
contexts:
  main:
    - match: ''
      pop: true
  leaf:
    - meta_content_scope: leaf.mcs.atom
    - match: '\w+'
      scope: keyword.fragguest
"#,
        true,
        None,
    )
    .unwrap();

    let mut builder = SyntaxSetBuilder::new();
    builder.add(host);
    builder.add(guest);
    let ss = builder.build();

    let syntax = ss.find_syntax_by_name("FragHost").unwrap();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    // Parse a line that opens the embed but never closes it, so the
    // wrapper + leaf stay on the final stack for inspection.
    let out = state.parse_line("<<word\n", &ss).expect("parse");
    for (_, op) in &out.ops {
        stack.apply(op).expect("apply");
    }

    let scopes: Vec<String> = stack
        .as_slice()
        .iter()
        .map(|s| format!("{:?}", s))
        .collect();
    let wrapper = Scope::new("wrapper.atom").unwrap();
    let leaf = Scope::new("leaf.mcs.atom").unwrap();
    assert!(
        stack.as_slice().contains(&wrapper),
        "wrapper.atom must remain visible inside fragment embed; got: {:?}",
        scopes
    );
    assert!(
        stack.as_slice().contains(&leaf),
        "leaf.mcs.atom (fragment target's meta_content_scope) must be \
             pushed and visible; got: {:?}",
        scopes
    );
}

/// Regression gate: `embed: scope:source.guest` (NO fragment) still
/// keeps the `embed_scope_replaces` suppression. The wrapper's last
/// embed_scope atom equals the guest syntax's top-level scope (the
/// auto-insert at `yaml_load.rs:706-713`); without suppression the
/// scope would appear twice on the stack.
#[test]
fn non_fragment_embed_still_suppresses_main_mcs() {
    use crate::parsing::ScopeStack;

    let host = SyntaxDefinition::load_from_str(
        r#"
name: NonFragHost
scope: source.nonfraghost
file_extensions: [nonfraghost]
version: 2
contexts:
  main:
    - match: '<<'
      embed: scope:source.nonfragguest
      embed_scope: wrapper2.atom source.nonfragguest
      escape: '>>'
"#,
        true,
        None,
    )
    .unwrap();

    let guest = SyntaxDefinition::load_from_str(
        r#"
name: NonFragGuest
scope: source.nonfragguest
file_extensions: [nonfragguest]
version: 2
hidden: true
contexts:
  main:
    - match: '\w+'
      scope: keyword.nonfragguest
"#,
        true,
        None,
    )
    .unwrap();

    let mut builder = SyntaxSetBuilder::new();
    builder.add(host);
    builder.add(guest);
    let ss = builder.build();

    let syntax = ss.find_syntax_by_name("NonFragHost").unwrap();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    // Parse a line that opens the embed but never closes it, so the
    // wrapper + guest scopes stay on the final stack for inspection.
    let out = state.parse_line("<<word\n", &ss).expect("parse");
    for (_, op) in &out.ops {
        stack.apply(op).expect("apply");
    }

    let guest_scope = Scope::new("source.nonfragguest").unwrap();
    let count = stack
        .as_slice()
        .iter()
        .filter(|s| **s == guest_scope)
        .count();
    assert_eq!(
        count, 1,
        "source.nonfragguest must appear exactly once (wrapper's last \
             embed_scope atom; guest main's auto-inserted top-level scope \
             must be suppressed); stack: {:?}",
        stack
    );
}
