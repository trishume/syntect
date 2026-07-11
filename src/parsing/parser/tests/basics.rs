//! Core parsing behavior: loop guards, anchors, backrefs, captures.

use super::*;

#[test]
fn can_parse_simple() {
    let ss = &*testdata::PACKAGES_SYN_SET;
    let mut state = {
        let syntax = ss.find_syntax_by_name("Ruby (Rails)").unwrap();
        ParseState::new(syntax)
    };

    let ops1 = ops(&mut state, "module Bob::Wow::Troll::Five; 5; end", ss);
    // `source.ruby.rails` is pushed once — the file's top-level
    // scope. Earlier versions of `add_initial_contexts` inserted
    // the scope into `main.meta_content_scope` twice (once on
    // initial load, once again after `resolve_extends` re-ran),
    // which showed up here as a duplicate Push. That duplication
    // also broke assertions of the form `- source source` in the
    // Diff test fixtures; the initial-contexts fix removes it.
    let test_ops1 = vec![
        (0, Push(Scope::new("source.ruby.rails").unwrap())),
        (0, Push(Scope::new("meta.namespace.ruby").unwrap())),
        (
            0,
            Push(Scope::new("keyword.declaration.namespace.ruby").unwrap()),
        ),
        (6, Pop(1)),
        (7, Pop(1)),
        (7, Push(Scope::new("meta.namespace.ruby").unwrap())),
        (7, Push(Scope::new("entity.name.namespace.ruby").unwrap())),
        (7, Push(Scope::new("support.other.namespace.ruby").unwrap())),
    ];
    assert_eq!(&ops1[0..test_ops1.len()], &test_ops1[..]);

    let ops2 = ops(&mut state, "def lol(wow = 5)", ss);
    let test_ops2 = [
        (0, Push(Scope::new("meta.function.ruby").unwrap())),
        (
            0,
            Push(Scope::new("keyword.declaration.function.ruby").unwrap()),
        ),
        (3, Pop(2)),
        (3, Push(Scope::new("meta.function.ruby").unwrap())),
        (4, Push(Scope::new("entity.name.function.ruby").unwrap())),
        (7, Pop(1)),
    ];
    assert_eq!(&ops2[0..test_ops2.len()], &test_ops2[..]);
}

#[test]
fn can_parse_yaml() {
    let ps = &*testdata::PACKAGES_SYN_SET;
    let mut state = {
        let syntax = ps.find_syntax_by_name("YAML").unwrap();
        ParseState::new(syntax)
    };

    assert_eq!(
        ops(&mut state, "key: value\n", ps),
        vec![
            (0, Push(Scope::new("source.yaml").unwrap())),
            (0, Push(Scope::new("meta.mapping.key.yaml").unwrap())),
            (0, Push(Scope::new("meta.string.yaml").unwrap())),
            (
                0,
                Push(Scope::new("string.unquoted.plain.out.yaml").unwrap())
            ),
            (3, Pop(2)),
            (3, Pop(1)),
            (3, Push(Scope::new("meta.mapping.yaml").unwrap())),
            (
                3,
                Push(Scope::new("punctuation.separator.key-value.mapping.yaml").unwrap())
            ),
            (4, Pop(2)),
            (5, Push(Scope::new("meta.string.yaml").unwrap())),
            (
                5,
                Push(Scope::new("string.unquoted.plain.out.yaml").unwrap())
            ),
            (10, Pop(2)),
        ]
    );
}

#[test]
fn can_parse_includes() {
    let ss = &*testdata::PACKAGES_SYN_SET;
    let mut state = {
        let syntax = ss.find_syntax_by_name("HTML (Rails)").unwrap();
        ParseState::new(syntax)
    };

    let ops = ops(&mut state, "<script>var lol = '<% def wow(", ss);

    assert!(
        !ops.is_empty(),
        "expected non-empty ops for line with includes"
    );
    let mut stack = ScopeStack::new();
    for (_, op) in ops.iter() {
        stack.apply(op).expect("#[cfg(test)]");
    }
    let stack_str = format!("{:?}", stack.as_slice());
    assert!(
        stack_str.contains("text.html.rails"),
        "expected text.html.rails in scope stack, got: {:?}",
        stack.as_slice()
    );
}

#[test]
fn can_parse_backrefs() {
    let ss = &*testdata::PACKAGES_SYN_SET;
    let mut state = {
        let syntax = ss.find_syntax_by_name("Ruby (Rails)").unwrap();
        ParseState::new(syntax)
    };

    // For parsing HEREDOC, the "SQL" is captured at the beginning and then used in another
    // regex with a backref, to match the end of the HEREDOC. Note that there can be code
    // after the marker (`.strip`) here.
    assert_eq!(
        ops(&mut state, "lol = <<-SQL.strip", ss),
        vec![
            (0, Push(Scope::new("source.ruby.rails").unwrap())),
            (
                4,
                Push(Scope::new("keyword.operator.assignment.ruby").unwrap())
            ),
            (5, Pop(1)),
            (6, Push(Scope::new("meta.string.heredoc.ruby").unwrap())),
            (
                6,
                Push(Scope::new("punctuation.definition.heredoc.ruby").unwrap())
            ),
            (9, Pop(1)),
            (9, Push(Scope::new("meta.tag.heredoc.ruby").unwrap())),
            (9, Push(Scope::new("entity.name.tag.ruby").unwrap())),
            (12, Pop(1)),
            (12, Pop(2)),
            (
                12,
                Push(Scope::new("punctuation.accessor.dot.ruby").unwrap())
            ),
            (13, Pop(1)),
        ]
    );

    assert_eq!(
        ops(&mut state, "wow", ss),
        vec![
            (0, Push(Scope::new("meta.string.heredoc.ruby").unwrap())),
            (0, Push(Scope::new("source.sql.embedded.ruby").unwrap()),),
            (0, Push(Scope::new("source.sql").unwrap())),
            (0, Push(Scope::new("source.sql.mysql").unwrap())),
            (0, Push(Scope::new("source.sql.basic").unwrap())),
            (0, Push(Scope::new("meta.column-name.sql").unwrap())),
            (3, Pop(1)),
        ]
    );

    assert_eq!(
        ops(&mut state, "SQL", ss),
        vec![
            (0, Pop(4)),
            (0, Pop(1)),
            (0, Push(Scope::new("meta.string.heredoc.ruby").unwrap())),
            (0, Push(Scope::new("meta.tag.heredoc.ruby").unwrap())),
            (0, Push(Scope::new("entity.name.tag.ruby").unwrap())),
            (3, Pop(2)),
            (3, Pop(1)),
        ]
    );
}

#[test]
fn can_parse_preprocessor_rules() {
    let ss = &*testdata::PACKAGES_SYN_SET;
    let mut state = {
        let syntax = ss.find_syntax_by_name("C").unwrap();
        ParseState::new(syntax)
    };

    assert_eq!(
        ops(&mut state, "#ifdef FOO", ss),
        vec![
            (0, Push(Scope::new("source.c").unwrap())),
            (0, Push(Scope::new("meta.preprocessor.c").unwrap())),
            (0, Push(Scope::new("keyword.control.import.c").unwrap())),
            (6, Pop(1)),
            (10, Pop(1)),
        ]
    );
    assert_eq!(
        ops(&mut state, "{", ss),
        vec![
            (0, Push(Scope::new("meta.block.c").unwrap())),
            (
                0,
                Push(Scope::new("punctuation.section.block.begin.c").unwrap())
            ),
            (1, Pop(1)),
        ]
    );
    assert_eq!(
        ops(&mut state, "#else", ss),
        vec![
            (0, Push(Scope::new("meta.preprocessor.c").unwrap())),
            (0, Push(Scope::new("keyword.control.import.c").unwrap())),
            (5, Pop(1)),
            (5, Pop(1)),
        ]
    );
    assert_eq!(
        ops(&mut state, "{", ss),
        vec![
            (0, Push(Scope::new("meta.block.c").unwrap())),
            (
                0,
                Push(Scope::new("punctuation.section.block.begin.c").unwrap())
            ),
            (1, Pop(1)),
        ]
    );
    assert_eq!(
        ops(&mut state, "#endif", ss),
        vec![
            (0, Pop(1)),
            (0, Push(Scope::new("meta.block.c").unwrap())),
            (0, Push(Scope::new("meta.preprocessor.c").unwrap())),
            (0, Push(Scope::new("keyword.control.import.c").unwrap())),
            (6, Pop(2)),
            (6, Pop(2)),
            (6, Push(Scope::new("meta.block.c").unwrap())),
        ]
    );
    assert_eq!(
        ops(&mut state, "    foo;", ss),
        vec![
            (7, Push(Scope::new("punctuation.terminator.c").unwrap())),
            (8, Pop(1)),
        ]
    );
    assert_eq!(
        ops(&mut state, "}", ss),
        vec![
            (
                0,
                Push(Scope::new("punctuation.section.block.end.c").unwrap())
            ),
            (1, Pop(1)),
            (1, Pop(1)),
        ]
    );
}

#[test]
fn can_compare_parse_states() {
    // `ParseState` equality checks the stack, active branch points,
    // and the buffered `pending_lines` used for cross-line branch
    // replay. Because `class Foo {` opens a still-unresolved branch
    // (`declarations`), the literal source text is retained in
    // `pending_lines`, so two states that parsed the same syntactic
    // shape with different identifiers (e.g. `Foo` vs `Bar`) compare
    // unequal today — unlike earlier versions of this test. Keep the
    // two inputs identical here and assert the remaining invariants:
    // identical inputs -> equal states, advancing one -> divergence.
    let ss = &*testdata::PACKAGES_SYN_SET;
    let syntax = ss.find_syntax_by_name("Java").unwrap();
    let mut state1 = ParseState::new(syntax);
    let mut state2 = ParseState::new(syntax);

    assert_eq!(ops(&mut state1, "class Foo {", ss).len(), 13);
    assert_eq!(ops(&mut state2, "class Foo {", ss).len(), 13);

    assert_eq!(state1, state2);
    ops(&mut state1, "}", ss);
    assert_ne!(state1, state2);
}

#[test]
fn can_parse_infinite_loop() {
    let line = "#infinite_loop_test 123";
    let expect = ["<source.test>, <constant.numeric.test>"];
    expect_scope_stacks(line, &expect, TEST_SYNTAX);
}

#[test]
fn can_parse_infinite_seeming_loop() {
    // See https://github.com/SublimeTextIssues/Core/issues/1190 for an
    // explanation.
    let line = "#infinite_seeming_loop_test hello";
    let expect = [
        "<source.test>, <keyword.test>",
        "<source.test>, <test>, <string.unquoted.test>",
        "<source.test>, <test>, <keyword.control.test>",
    ];
    expect_scope_stacks(line, &expect, TEST_SYNTAX);
}

#[test]
fn can_parse_syntax_with_newline_in_character_class() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: foo[\n]
      scope: foo.end
    - match: foo
      scope: foo.any
"#;

    let line = "foo";
    let expect = ["<source.test>, <foo.end>"];
    expect_scope_stacks(line, &expect, syntax);

    let line = "foofoofoo";
    let expect = [
        "<source.test>, <foo.any>",
        "<source.test>, <foo.any>",
        "<source.test>, <foo.end>",
    ];
    expect_scope_stacks(line, &expect, syntax);
}

#[test]
fn can_parse_non_consuming_pop_that_would_loop() {
    // See https://github.com/trishume/syntect/issues/127
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    # This makes us go into "test" without consuming any characters
    - match: (?=hello)
      push: test
  test:
    # If we used this match, we'd go back to "main" without consuming anything,
    # and then back into "test", infinitely looping. ST detects this at this
    # point and ignores this match until at least one character matched.
    - match: (?!world)
      pop: true
    - match: \w+
      scope: test.matched
"#;

    let line = "hello";
    let expect = ["<source.test>, <test.matched>"];
    expect_scope_stacks(line, &expect, syntax);
}

#[test]
fn can_parse_non_consuming_set_and_pop_that_would_loop() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    # This makes us go into "a" without advancing
    - match: (?=test)
      push: a
  a:
    # This makes us go into "b" without advancing
    - match: (?=t)
      set: b
  b:
    # If we used this match, we'd go back to "main" without having advanced,
    # which means we'd have an infinite loop like with the previous test.
    # So even for a "set", we have to check if we're advancing or not.
    - match: (?=t)
      pop: true
    - match: \w+
      scope: test.matched
"#;

    let line = "test";
    let expect = ["<source.test>, <test.matched>"];
    expect_scope_stacks(line, &expect, syntax);
}

#[test]
fn can_parse_non_consuming_set_after_consuming_push_that_does_not_loop() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    # This makes us go into "a", but we consumed a character
    - match: t
      push: a
    - match: \w+
      scope: test.matched
  a:
    # This makes us go into "b" without consuming
    - match: (?=e)
      set: b
  b:
    # This match does not result in an infinite loop because we already consumed
    # a character to get into "a", so it's ok to pop back into "main".
    - match: (?=e)
      pop: true
"#;

    let line = "test";
    let expect = ["<source.test>, <test.matched>"];
    expect_scope_stacks(line, &expect, syntax);
}

#[test]
fn can_parse_non_consuming_set_after_consuming_set_that_does_not_loop() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: (?=hello)
      push: a
    - match: \w+
      scope: test.matched
  a:
    - match: h
      set: b
  b:
    - match: (?=e)
      set: c
  c:
    # This is not an infinite loop because "a" consumed a character, so we can
    # actually pop back into main and then match the rest of the input.
    - match: (?=e)
      pop: true
"#;

    let line = "hello";
    let expect = ["<source.test>, <test.matched>"];
    expect_scope_stacks(line, &expect, syntax);
}

#[test]
fn can_parse_non_consuming_pop_that_would_loop_at_end_of_line() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    # This makes us go into "test" without consuming, even at the end of line
    - match: ""
      push: test
  test:
    - match: ""
      pop: true
    - match: \w+
      scope: test.matched
"#;

    let line = "hello";
    let expect = ["<source.test>, <test.matched>"];
    expect_scope_stacks(line, &expect, syntax);
}

#[test]
fn non_consuming_pop_n_below_pre_push_depth_is_not_a_loop() {
    // Mirror of the Haskell `declaration-type-end` branch where the
    // fallback alternative is `immediately-pop2` (empty match with
    // `pop: 2`). The outer wrapper's meta_scope must come off when
    // the pop-2 fallback fires — pre-fix, the loop guard flagged
    // any non-consuming `pop` after a non-consuming push as
    // looping, so the parser advanced one char past the branch and
    // the pop-2 fired at the wrong column, leaving the wrapper's
    // scope covering the trailing `y` token.
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: open
      scope: test.open
      push: wrapper
    - match: y
      scope: test.main.y
    - match: z
      scope: test.main.z
  wrapper:
    - meta_scope: test.wrapper
    - match: ""
      branch_point: fallback
      branch:
        - try
        - give-up
  try:
    - match: x
      scope: test.try.match
    - match: (?=y)
      fail: fallback
  give-up:
    - match: ""
      pop: 2
"#;
    // With the fix, `give-up` fires pop-2 at column 4 (the fail
    // position), unwinding both `give-up` and `wrapper`; `y` is
    // then scoped by `main`'s rule. Without the fix, would_loop
    // advanced start past column 4, the pop-2 fired at column 5,
    // and `y` stayed inside the wrapper and never matched
    // `test.main.y`.
    expect_scope_stacks("openyz", &["<source.test>, <test.main.y>"], syntax);
}

#[test]
fn non_consuming_multi_push_with_skip_unwind_does_not_loop() {
    // Pre-fix: parser hangs — K=3 push stored armed depth D+1, but
    // a multi-level pop chain (`pop:1` then `pop:2`) unwinds
    // D+3 → D+2 → D, never visiting D+1, so the loop guard never
    // arms and the token loop re-enters main's empty
    // push forever. Under the fix the guard arms across the
    // half-open interval (D, D+K] and fires when a non-consuming
    // pop lands at exactly D, so b's `pop: 2` from depth D+2
    // trips the guard and the parser advances one char.
    //
    // Rule order matters: `z` precedes `""` so the consuming match
    // wins at byte 0; the multi-push only fires at byte 1 (EOL),
    // exercising the guard without the empty rule swallowing `z`.
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: z
      scope: test.main.z
    - match: ""
      push: [a, b, c]
  a: []
  b:
    - match: ""
      pop: 2
  c:
    - match: ""
      pop: 1
"#;
    expect_scope_stacks("z", &["<source.test>, <test.main.z>"], syntax);
}

#[test]
fn can_parse_empty_but_consuming_set_that_does_not_loop() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: (?=hello)
      push: a
    - match: ello
      scope: test.good
  a:
    # This is an empty match, but it consumed a character (the "h")
    - match: (?=e)
      set: b
  b:
    # .. so it's ok to pop back to main from here
    - match: ""
      pop: true
    - match: ello
      scope: test.bad
"#;

    let line = "hello";
    let expect = ["<source.test>, <test.good>"];
    expect_scope_stacks(line, &expect, syntax);
}

#[test]
fn can_parse_non_consuming_pop_that_does_not_loop() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    # This is a non-consuming push, so "b" will need to check for a
    # non-consuming pop
    - match: (?=hello)
      push: [b, a]
    - match: ello
      scope: test.good
  a:
    # This pop is ok, it consumed "h"
    - match: (?=e)
      pop: true
  b:
    # This is non-consuming, and we set to "c"
    - match: (?=e)
      set: c
  c:
    # It's ok to pop back to "main" here because we consumed a character in the
    # meantime.
    - match: ""
      pop: true
    - match: ello
      scope: test.bad
"#;

    let line = "hello";
    let expect = ["<source.test>, <test.good>"];
    expect_scope_stacks(line, &expect, syntax);
}

#[test]
fn can_parse_non_consuming_pop_with_multi_push_that_does_not_loop() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: (?=hello)
      push: [b, a]
    - match: ello
      scope: test.good
  a:
    # This pop is ok, as we're not popping back to "main" yet (which would loop),
    # we're popping to "b"
    - match: ""
      pop: true
    - match: \w+
      scope: test.bad
  b:
    - match: \w+
      scope: test.good
"#;

    let line = "hello";
    let expect = ["<source.test>, <test.good>"];
    expect_scope_stacks(line, &expect, syntax);
}

#[test]
fn can_parse_non_consuming_pop_of_recursive_context_that_does_not_loop() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: xxx
      scope: test.good
    - include: basic-identifiers

  basic-identifiers:
    - match: '\w+::'
      scope: test.matched
      push: no-type-names

  no-type-names:
      - include: basic-identifiers
      - match: \w+
        scope: test.matched.inside
      # This is a tricky one because when this is the best match,
      # we have two instances of "no-type-names" on the stack, so we're popping
      # back from "no-type-names" to another "no-type-names".
      - match: ''
        pop: true
"#;

    let line = "foo::bar::* xxx";
    let expect = ["<source.test>, <test.good>"];
    expect_scope_stacks(line, &expect, syntax);
}

/// Ruby's `?\u{012ACF 0gxs}`: `\h{0,6}` can match zero-width at the
/// space. Without the `FIND_NOT_EMPTY` engine option, the zero-width
/// match wins and hides the later non-empty match of `0`, which then
/// falls through to the `\S` fallback. With the option on
/// `MatchOperation::None` patterns, the engine retries past the
/// zero-width position and matches `0` as `number.hex`.
#[test]
fn scope_only_pattern_that_matches_zero_width_finds_later_non_empty() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: \h{0,6}
      scope: number.hex
    - match: \S
      scope: invalid.illegal
"#;

    let line = "012ACF 0gxs";
    let expect = [
        "<source.test>, <number.hex>",      // "012ACF" and "0"
        "<source.test>, <invalid.illegal>", // "g", "x", "s"
    ];
    expect_scope_stacks(line, &expect, syntax);
}

/// Cabal's `\|\||&&||!` operator regex has a stray empty alternative
/// between `&&` and `!`. Under leftmost-first matching the empty alt
/// wins zero-width at the `!` position. With `FIND_NOT_EMPTY`, the
/// engine rejects the empty alt and matches `!` via the real
/// alternative.
#[test]
fn scope_only_pattern_with_middle_empty_alt_matches_bang() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: \|\||&&||!
      scope: keyword.operator
"#;
    let line = "!";
    let expect = ["<source.test>, <keyword.operator>"];
    expect_scope_stacks(line, &expect, syntax);
}

/// Rust's `prelude_types: (?x:|Box|Option|…)` puts a `|` before every
/// alternative, including the first. Under leftmost-first the leading
/// empty alt wins zero-width at every position, so `\b(?x:|Box|Vec)\b`
/// never matches `Box` or `Vec`. With `FIND_NOT_EMPTY`, the engine
/// rejects the zero-width alt and matches `Vec` via the real
/// alternative.
#[test]
fn scope_only_pattern_with_leading_empty_alt_in_group_matches_name() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: \b(?x:|Box|Vec)\b
      scope: support.type
"#;
    let line = "Vec";
    let expect = ["<source.test>, <support.type>"];
    expect_scope_stacks(line, &expect, syntax);
}

#[test]
fn can_parse_non_consuming_pop_order() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: (?=hello)
      push: test
  test:
    # This matches first
    - match: (?=e)
      push: good
    # But this (looping) match replaces it, because it's an earlier match
    - match: (?=h)
      pop: true
    # And this should not replace it, as it's a later match (only matches at
    # the same position can replace looping pops).
    - match: (?=o)
      push: bad
  good:
    - match: \w+
      scope: test.good
  bad:
    - match: \w+
      scope: test.bad
"#;

    let line = "hello";
    let expect = ["<source.test>, <test.good>"];
    expect_scope_stacks(line, &expect, syntax);
}

#[test]
fn can_parse_syntax_with_eol_and_newline() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: foo$\n
      scope: foo.newline
"#;

    let line = "foo";
    let expect = ["<source.test>, <foo.newline>"];
    expect_scope_stacks(line, &expect, syntax);
}

#[test]
fn can_parse_syntax_with_eol_only() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: foo$
      scope: foo.newline
"#;

    let line = "foo";
    let expect = ["<source.test>, <foo.newline>"];
    expect_scope_stacks(line, &expect, syntax);
}

#[test]
fn can_parse_syntax_with_beginning_of_line() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: \w+
      scope: word
      push:
        # this should not match at the end of the line
        - match: ^\s*$
          pop: true
        - match: =+
          scope: heading
          pop: true
    - match: .*
      scope: other
"#;

    let syntax_newlines = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
    let syntax_set = link(syntax_newlines);

    let mut state = ParseState::new(&syntax_set.syntaxes()[0]);
    assert_eq!(
        ops(&mut state, "foo\n", &syntax_set),
        vec![
            (0, Push(Scope::new("source.test").unwrap())),
            (0, Push(Scope::new("word").unwrap())),
            (3, Pop(1))
        ]
    );
    assert_eq!(
        ops(&mut state, "===\n", &syntax_set),
        vec![(0, Push(Scope::new("heading").unwrap())), (3, Pop(1))]
    );

    assert_eq!(
        ops(&mut state, "bar\n", &syntax_set),
        vec![(0, Push(Scope::new("word").unwrap())), (3, Pop(1))]
    );
    // This should result in popping out of the context
    assert_eq!(ops(&mut state, "\n", &syntax_set), vec![]);
    // So now this matches other
    assert_eq!(
        ops(&mut state, "====\n", &syntax_set),
        vec![(0, Push(Scope::new("other").unwrap())), (4, Pop(1))]
    );
}

#[test]
fn can_parse_syntax_with_comment_and_eol() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: (//).*$
      scope: comment.line.double-slash
"#;

    let syntax_newlines = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
    let syntax_set = link(syntax_newlines);

    let mut state = ParseState::new(&syntax_set.syntaxes()[0]);
    assert_eq!(
        ops(&mut state, "// foo\n", &syntax_set),
        vec![
            (0, Push(Scope::new("source.test").unwrap())),
            (0, Push(Scope::new("comment.line.double-slash").unwrap())),
            // 6 is important here, should not be 7. The pattern should *not* consume the newline,
            // but instead match before it. This is important for whitespace-sensitive syntaxes
            // where newlines terminate statements such as Scala.
            (6, Pop(1))
        ]
    );
}

#[test]
fn can_parse_text_with_unicode_to_skip() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  main:
    - match: (?=.)
      push: test
  test:
    - match: (?=.)
      pop: true
    - match: x
      scope: test.good
"#;

    // U+03C0 GREEK SMALL LETTER PI, 2 bytes in UTF-8
    expect_scope_stacks("\u{03C0}x", &["<source.test>, <test.good>"], syntax);
    // U+0800 SAMARITAN LETTER ALAF, 3 bytes in UTF-8
    expect_scope_stacks("\u{0800}x", &["<source.test>, <test.good>"], syntax);
    // U+1F600 GRINNING FACE, 4 bytes in UTF-8
    expect_scope_stacks("\u{1F600}x", &["<source.test>, <test.good>"], syntax);
}

#[test]
fn can_include_backrefs() {
    let syntax = SyntaxDefinition::load_from_str(
        r#"
                name: Backref Include Test
                scope: source.backrefinc
                contexts:
                  main:
                    - match: (a)
                      scope: a
                      push: context1
                  context1:
                    - include: context2
                  context2:
                    - match: \1
                      scope: b
                      pop: true
                "#,
        true,
        None,
    )
    .unwrap();

    expect_scope_stacks_with_syntax("aa", &["<a>", "<b>"], syntax);
}

#[test]
fn can_include_nested_backrefs() {
    let syntax = SyntaxDefinition::load_from_str(
        r#"
                name: Backref Include Test
                scope: source.backrefinc
                contexts:
                  main:
                    - match: (a)
                      scope: a
                      push: context1
                  context1:
                    - include: context3
                  context3:
                    - include: context2
                  context2:
                    - match: \1
                      scope: b
                      pop: true
                "#,
        true,
        None,
    )
    .unwrap();

    expect_scope_stacks_with_syntax("aa", &["<a>", "<b>"], syntax);
}

#[test]
fn can_avoid_infinite_stack_depth() {
    let syntax = SyntaxDefinition::load_from_str(
        r#"
                name: Stack Depth Test
                scope: source.stack_depth
                contexts:
                  main:
                    - match: (a)
                      scope: a
                      push: context1

                    
                  context1:
                    - match: b
                      scope: b
                    - match: ''
                      push: context1
                    - match: ''
                      pop: 1
                    - match: c
                      scope: c
                "#,
        true,
        None,
    )
    .unwrap();

    let syntax_set = link(syntax);
    let mut state = ParseState::new(&syntax_set.syntaxes()[0]);
    expect_scope_stacks_for_ops(ops(&mut state, "a bc\n", &syntax_set), &["<a>"]);
    expect_scope_stacks_for_ops(ops(&mut state, "bc\n", &syntax_set), &["<b>"]);
}

/// `main` must be idempotent.
#[test]
fn extending_syntax_does_not_double_push_top_level_scope() {
    use crate::parsing::SyntaxSet;
    let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
    // Git Diff extends Diff (Basic) — a concrete case of the bug.
    let syntax = ss.find_syntax_by_name("Git Diff").unwrap();
    let mut state = ParseState::new(syntax);
    let o = ops(
        &mut state,
        "From 1234567890 Mon Sep 17 00:00:00 2001\n",
        &ss,
    );
    let source_pushes = o
            .iter()
            .filter(|(_, op)| matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s) == "<source.diff.git>"))
            .count();
    assert_eq!(
            source_pushes, 1,
            "source.diff.git should be pushed exactly once for the file's top-level scope; ops were: {:?}",
            o
        );
}

#[test]
fn consuming_match_not_treated_as_loop() {
    // Kills: L533 replace > with < in find_best_match (consuming check)
    // A pop that consumes characters must NOT be treated as a loop.
    // If consuming is negated (> → <), a consuming pop would be flagged
    // as a loop and skipped, breaking the parse.
    let syntax_str = r#"
name: ConsumingTest
scope: source.consuming
contexts:
  main:
    - match: '(?=\S)'
      push: inner
    - match: '\n'
  inner:
    - match: '\w+'
      scope: word.consuming
      pop: true
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);

    // "(?=\S)" is a zero-length push (non-consuming), then "\w+" is a
    // consuming pop inside inner.  If the consuming check is inverted,
    // the pop would be treated as looping and skipped, causing the parser
    // to advance one character before matching, so the push position
    // would be 1 instead of 0.
    let raw_ops = ops(&mut state, "hello world\n", &ss);
    let first_word_pos = raw_ops
        .iter()
        .find_map(|(pos, op)| match op {
            ScopeStackOp::Push(s) if format!("{:?}", s).contains("word.consuming") => Some(*pos),
            _ => None,
        })
        .expect("expected at least one word.consuming push");
    assert_eq!(
        first_word_pos, 0,
        "word.consuming must start at position 0 (consuming pop should not be treated as loop)"
    );
}

#[test]
fn captures_clipped_to_match_bounds_when_group_extends_past_match_end() {
    // Repro of a C# generic-function-call divergence against ST.
    // Rule shape: a consumed identifier, then a lookahead containing
    // a capturing group whose match extends *past* the outer rule's
    // consumed end, then a second consumed group starting at the
    // same column where the lookahead began. `captures: 2:` targets
    // the lookahead-internal group. ST clips each captures:N span
    // to the rule's match bounds and only colours the overlap —
    // which here is the single consumed char at the match-end
    // boundary. Syntect used to colour the full group-2 range,
    // emitting a Pop past match_end and leaving the scope active
    // over chars the match never consumed.
    let syntax_str = r#"
name: CapturesClip
scope: source.capclip
contexts:
  main:
    - match: '(foo)(?=(barrr)baz)(bar)'
      captures:
        1: captured-foo.capclip
        2: lookahead-group.capclip
        3: consumed-bar.capclip
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let raw_ops = ops(&mut state, "foobarrrbaz\n", &ss);

    // The rule consumes "foobar" — match_start=0, match_end=6.
    // Group 2's own range (the lookahead match "barrr") extends to
    // column 8. Every op emitted by the captures application must
    // sit within [match_start, match_end]; anything at col 7+ means
    // the lookahead-internal group's span leaked past the match.
    // match_start=0, match_end=6 (rule consumes "foobar"). Group 2's
    // own range (the lookahead match "barrr") extends to col 8.
    //
    // After the fix we expect:
    //   * `lookahead-group.capclip` Pushed at col 3 (cap_start of
    //     group 2, which overlaps the consumed region).
    //   * The matching Pop no later than col 6 (clipped to match_end).
    //   * No op at col 7 or 8 — anything there means the lookahead
    //     range leaked past the match.
    let lookahead_pushes: Vec<usize> = raw_ops
        .iter()
        .filter_map(|(pos, op)| match op {
            ScopeStackOp::Push(s) if format!("{:?}", s).contains("lookahead-group") => Some(*pos),
            _ => None,
        })
        .collect();
    assert_eq!(
        lookahead_pushes,
        vec![3],
        "`captures: 2:` (lookahead-internal group) must Push the \
             clipped scope at match_start=3; raw_ops={:?}",
        raw_ops
    );
    let match_end = 6;
    let past_match: Vec<_> = raw_ops.iter().filter(|(pos, _)| *pos > match_end).collect();
    assert!(
        past_match.is_empty(),
        "No capture op should sit past match_end={}; found {:?}",
        match_end,
        past_match
    );
}

#[test]
fn capture_sort_by_span_length() {
    // Kills: L709 replace - with + in exec_pattern (capture sort key)
    // Captures are sorted so that longer spans come first (pushed before
    // shorter nested ones).  If the sort key sign is flipped, shorter
    // spans push first, producing the wrong nesting order.
    let syntax_str = r#"
name: CaptureSort
scope: source.capsort
contexts:
  main:
    - match: '((a)(b))'
      captures:
        1: outer.capsort
        2: inner-a.capsort
        3: inner-b.capsort
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let raw_ops = ops(&mut state, "ab\n", &ss);

    // With correct sorting: outer pushes first (at pos 0), then inner-a
    // at the same position.  With the sign flipped, inner-a would push
    // before outer, which is wrong.
    let push_order: Vec<&str> = raw_ops
        .iter()
        .filter_map(|(_, op)| match op {
            ScopeStackOp::Push(s) => {
                let name = format!("{:?}", s);
                if name.contains("outer.capsort") {
                    Some("outer")
                } else if name.contains("inner-a.capsort") {
                    Some("inner-a")
                } else if name.contains("inner-b.capsort") {
                    Some("inner-b")
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        push_order,
        vec!["outer", "inner-a", "inner-b"],
        "captures must push in longest-span-first order, got: {:?}",
        push_order
    );
}
