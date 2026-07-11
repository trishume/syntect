//! Meta-scope, clear_scopes, set/pop-N, and prototype semantics.

use super::*;

#[test]
fn can_parse_non_nested_clear_scopes() {
    let line = "'hello #simple_cleared_scopes_test world test \\n '";
    let expect = [
            "<source.test>, <example.meta-scope.after-clear-scopes.example>, <example.pushes-clear-scopes.example>",
            "<source.test>, <example.meta-scope.after-clear-scopes.example>, <example.pops-clear-scopes.example>",
            "<source.test>, <string.quoted.single.example>, <constant.character.escape.example>",
        ];
    expect_scope_stacks(line, &expect, TEST_SYNTAX);
}

#[test]
fn can_parse_non_nested_too_many_clear_scopes() {
    let line = "'hello #too_many_cleared_scopes_test world test \\n '";
    let expect = [
        "<example.meta-scope.after-clear-scopes.example>, <example.pushes-clear-scopes.example>",
        "<example.meta-scope.after-clear-scopes.example>, <example.pops-clear-scopes.example>",
        "<source.test>, <string.quoted.single.example>, <constant.character.escape.example>",
    ];
    expect_scope_stacks(line, &expect, TEST_SYNTAX);
}

#[test]
fn can_parse_nested_clear_scopes() {
    let line = "'hello #nested_clear_scopes_test world foo bar test \\n '";
    let expect = [
            "<source.test>, <example.meta-scope.after-clear-scopes.example>, <example.pushes-clear-scopes.example>",
            "<source.test>, <example.meta-scope.cleared-previous-meta-scope.example>, <foo>",
            "<source.test>, <example.meta-scope.after-clear-scopes.example>, <example.pops-clear-scopes.example>",
            "<source.test>, <string.quoted.single.example>, <constant.character.escape.example>",
        ];
    expect_scope_stacks(line, &expect, TEST_SYNTAX);
}

#[test]
fn can_parse_prototype_that_pops_main() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  prototype:
    # This causes us to pop out of the main context. Sublime Text handles that
    # by pushing main back automatically.
    - match: (?=!)
      pop: true
  main:
    - match: foo
      scope: test.good
"#;

    let line = "foo!";
    let expect = ["<source.test>, <test.good>"];
    expect_scope_stacks(line, &expect, syntax);
}

#[test]
fn can_parse_prototype_that_pops_multiple_context() {
    let syntax = r#"
name: test
scope: source.test
contexts:
  prototype:
    - match: "!"
      pop: 2
  bar:
    - match: \bbaz\b
      push: baz
      scope: main.baz
  foo:
    - match: \bbar\b
      push: bar
      scope: test.bar
    - match: \bgood\b
      push: baz
      scope: test.good
  baz: []
    
  main:
    - match: \bfoo\b
      push: foo
      scope: test.foo
"#;

    let line = "foo bar baz ! good";
    let expect = ["<source.test>, <test.good>"];
    expect_scope_stacks(line, &expect, syntax);
}

#[test]
fn can_parse_context_included_in_prototype_via_named_reference() {
    let syntax = r#"
scope: source.test
contexts:
  prototype:
    - match: a
      push: a
    - match: b
      scope: test.bad
  main:
    - match: unused
  # This context is included in the prototype (see `push: a`).
  # Because of that, ST doesn't apply the prototype to this context, so if
  # we're in here the "b" shouldn't match.
  a:
    - match: a
      scope: test.good
"#;

    let stack_states = stack_states(parse("aa b", syntax));
    assert_eq!(
        stack_states,
        vec![
            "<source.test>",
            "<source.test>, <test.good>",
            "<source.test>",
        ],
        "Expected test.bad to not match"
    );
}

#[test]
fn can_parse_with_prototype_set() {
    let syntax = r#"%YAML 1.2
---
scope: source.test-set-with-proto
contexts:
  main:
    - match: a
      scope: a
      set: next1
      with_prototype:
        - match: '1'
          scope: '1'
        - match: '2'
          scope: '2'
        - match: '3'
          scope: '3'
        - match: '4'
          scope: '4'
    - match: '5'
      scope: '5'
      set: [next3, next2]
      with_prototype:
        - match: c
          scope: cwith
  next1:
    - match: b
      scope: b
      set: next2
  next2:
    - match: c
      scope: c
      push: next3
    - match: e
      scope: e
      pop: true
    - match: f
      scope: f
      set: [next1, next2]
  next3:
    - match: d
      scope: d
    - match: (?=e)
      pop: true
    - match: c
      scope: cwithout
"#;

    expect_scope_stacks_with_syntax(
        "a1b2c3d4e5",
        &[
            "<a>", "<1>", "<b>", "<2>", "<c>", "<3>", "<d>", "<4>", "<e>", "<5>",
        ],
        SyntaxDefinition::load_from_str(syntax, true, None).unwrap(),
    );
    expect_scope_stacks_with_syntax(
        "5cfcecbedcdea",
        &[
            "<5>",
            "<cwith>",
            "<f>",
            "<e>",
            "<b>",
            "<d>",
            "<cwithout>",
            "<a>",
        ],
        SyntaxDefinition::load_from_str(syntax, true, None).unwrap(),
    );
}

#[test]
fn can_parse_two_with_prototypes_at_same_stack_level() {
    let syntax_yamlstr = r#"
%YAML 1.2
---
# See http://www.sublimetext.com/docs/3/syntax.html
scope: source.example-wp
contexts:
  main:
    - match: a
      scope: a
      push:
        - match: b
          scope: b
          set:
            - match: c
              scope: c
          with_prototype:
            - match: '2'
              scope: '2'
      with_prototype:
        - match: '1'
          scope: '1'
"#;

    let syntax = SyntaxDefinition::load_from_str(syntax_yamlstr, true, None).unwrap();
    expect_scope_stacks_with_syntax("abc12", &["<1>", "<2>"], syntax);
}

#[test]
fn can_parse_two_with_prototypes_at_same_stack_level_set_multiple() {
    let syntax_yamlstr = r#"
%YAML 1.2
---
# See http://www.sublimetext.com/docs/3/syntax.html
scope: source.example-wp
contexts:
  main:
    - match: a
      scope: a
      push:
        - match: b
          scope: b
          set: [context1, context2, context3]
          with_prototype:
            - match: '2'
              scope: '2'
      with_prototype:
        - match: '1'
          scope: '1'
    - match: '1'
      scope: digit1
    - match: '2'
      scope: digit2
  context1:
    - match: e
      scope: e
      pop: true
    - match: '2'
      scope: digit2
  context2:
    - match: d
      scope: d
      pop: true
    - match: '2'
      scope: digit2
  context3:
    - match: c
      scope: c
      pop: true
"#;

    let syntax = SyntaxDefinition::load_from_str(syntax_yamlstr, true, None).unwrap();
    expect_scope_stacks_with_syntax("ab12", &["<1>", "<2>"], syntax.clone());
    expect_scope_stacks_with_syntax("abc12", &["<1>", "<digit2>"], syntax.clone());
    expect_scope_stacks_with_syntax("abcd12", &["<1>", "<digit2>"], syntax.clone());
    expect_scope_stacks_with_syntax("abcde12", &["<digit1>", "<digit2>"], syntax);
}

#[test]
fn can_parse_two_with_prototypes_at_same_stack_level_updated_captures() {
    let syntax_yamlstr = r#"
%YAML 1.2
---
# See http://www.sublimetext.com/docs/3/syntax.html
scope: source.example-wp
contexts:
  main:
    - match: (a)
      scope: a
      push:
        - match: (b)
          scope: b
          set:
            - match: c
              scope: c
          with_prototype:
            - match: d
              scope: d
      with_prototype:
        - match: \1
          scope: '1'
          pop: true
"#;

    let syntax = SyntaxDefinition::load_from_str(syntax_yamlstr, true, None).unwrap();
    expect_scope_stacks_with_syntax("aa", &["<a>", "<1>"], syntax.clone());
    expect_scope_stacks_with_syntax("abcdb", &["<a>", "<b>", "<c>", "<d>", "<1>"], syntax);
}

#[test]
fn can_parse_two_with_prototypes_at_same_stack_level_updated_captures_ignore_unexisting() {
    let syntax_yamlstr = r#"
%YAML 1.2
---
# See http://www.sublimetext.com/docs/3/syntax.html
scope: source.example-wp
contexts:
  main:
    - match: (a)(-)
      scope: a
      push:
        - match: (b)
          scope: b
          set:
            - match: c
              scope: c
          with_prototype:
            - match: d
              scope: d
      with_prototype:
        - match: \2
          scope: '2'
          pop: true
        - match: \1
          scope: '1'
          pop: true
"#;

    let syntax = SyntaxDefinition::load_from_str(syntax_yamlstr, true, None).unwrap();
    expect_scope_stacks_with_syntax("a--", &["<a>", "<2>"], syntax.clone());
    // it seems that when ST encounters a non existing pop backreference, it just pops back to the with_prototype's original parent context - i.e. cdb is unscoped
    // TODO: it would be useful to have syntest functionality available here for easier testing and clarity
    expect_scope_stacks_with_syntax("a-bcdba-", &["<a>", "<b>"], syntax);
}

/// Regression guard for the "extends double-inserts top_level_scope"
/// bug: `add_initial_contexts` runs once during initial YAML load and
/// again from `resolve_extends` after a child inherits its parent's
/// contexts. On the second run `main.meta_content_scope` already
/// begins with the child's top-level scope from the first run; if the
/// code naively re-inserts at position 0 and re-copies to `__main`,
/// the file scope ends up pushed twice at the start of every parse
/// (observed as `[source.diff.git, source.diff.git]` on Git Diff and
/// all the Rails (Rails) syntaxes, which broke assertions of the
/// form `- source source`). The copy to `__main` must strip an
/// already-present top_level_scope prefix, and the insert into
/// Regression guard for the "`meta_append` / `meta_prepend` resets
/// `meta_include_prototype` to its default" bug: the SQL base
/// declares `inside-like-single-quoted-string` with
/// `meta_include_prototype: false` so its `--` comment rule (from
/// the SQL prototype) does NOT fire inside LIKE strings. TSQL extends
/// that context with `meta_append: true` to add a `[…]` character-set
/// rule, but doesn't restate `meta_include_prototype: false`. Before
/// the fix, the merge in `syntax_set.rs` left the child's default
/// `meta_include_prototype: true`, so the SQL prototype attached to
/// the merged context, and `--` inside LIKE strings was scoped as
/// a comment — 4,918 cascading assertion failures in
/// `syntax_test_tsql.sql`.
///
/// Synthetic shape: a parent with a `prototype` matching `--` as a
/// comment, a base context with `meta_include_prototype: false`,
/// and a child that extends the parent and `meta_append`s a single
/// rule to that base context without restating
/// `meta_include_prototype`. After merge, `--` inside the base
/// context's matched span must NOT take a `comment.*` scope.
#[test]
fn meta_append_inherits_meta_include_prototype_from_parent() {
    use crate::parsing::syntax_set::SyntaxSetBuilder;

    let dir = std::env::temp_dir().join(format!("syntect-meta-append-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("parent.sublime-syntax"),
        r#"
name: Parent
scope: source.parent
file_extensions: [parent]
contexts:
  prototype:
    - match: '--'
      scope: punctuation.definition.comment
      push: comment-body
  comment-body:
    - meta_scope: comment.line
    - match: $
      pop: 1
  main:
    - match: \bopen\b
      push: inside
  inside:
    - meta_include_prototype: false
    - meta_scope: meta.inside
    - match: \bclose\b
      pop: 1
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("child.sublime-syntax"),
        r#"
name: Child
scope: source.child
file_extensions: [child]
extends: parent.sublime-syntax
contexts:
  inside:
    - meta_append: true
    - match: '!'
      scope: punctuation.bang.child
"#,
    )
    .unwrap();
    let mut builder = SyntaxSetBuilder::new();
    builder.add_from_folder(&dir, true).unwrap();
    let ss = builder.build();
    let syntax = ss
        .find_syntax_by_scope(Scope::new("source.child").unwrap())
        .unwrap();
    let mut state = ParseState::new(syntax);
    let o = ops(&mut state, "open -- close\n", &ss);
    let _ = std::fs::remove_dir_all(&dir);
    let comment_pushes = o
        .iter()
        .filter(
            |(_, op)| matches!(op, ScopeStackOp::Push(s) if format!("{:?}", s).contains("comment")),
        )
        .count();
    assert_eq!(
        comment_pushes, 0,
        "`--` inside the inside context (meta_include_prototype: false in parent) \
             must not match the parent's prototype comment rule after meta_append merge; \
             ops were: {:?}",
        o
    );
}

/// Minimal repro of the Category A "pop: N loses deeper contexts'
/// scopes" bug. Two pushed contexts A and B (with B on top): A has
/// `meta_scope: outer`, B has `meta_content_scope: inner`. When B
/// fires `pop: 2`, the scope stack must come fully back to the base
/// — before the fix, A's `outer` was orphaned on the scope stack
/// because `push_meta_ops` only emitted pops for the top context.
/// Checked against the scope stack produced by the ops (the
/// context-stack pop already worked; the scope-stack pop did not).
#[test]
fn pop_n_unwinds_all_n_contexts_meta_scopes() {
    let syntax = SyntaxDefinition::load_from_str(
        r#"
                name: Pop N Test
                scope: source.test
                contexts:
                  main:
                    - match: \(
                      scope: open
                      push: [outer, inner]
                  outer:
                    - meta_scope: outer.test
                  inner:
                    - meta_content_scope: inner.test
                    - match: \)
                      scope: close
                      pop: 2
                "#,
        true,
        None,
    )
    .unwrap();

    let syntax_set = link(syntax);
    let mut state = ParseState::new(&syntax_set.syntaxes()[0]);
    let o = ops(&mut state, "(x)\n", &syntax_set);
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
        !final_scopes.iter().any(|s| s.contains("outer.test")),
        "outer.test meta_scope leaked past pop: 2; final stack: {:?}",
        final_scopes
    );
    assert!(
        !final_scopes.iter().any(|s| s.contains("inner.test")),
        "inner.test meta_content_scope leaked past pop: 2; final stack: {:?}",
        final_scopes
    );
}

/// Triage repro for Category A (Zsh/TSQL/Makefile "context never
/// pops" cascade) — models the shape used by Makefile's variable
/// definitions: a lookahead push, then `set: [value, eat]` with a
/// zero-width match inside `value` that `set`s to a third context
/// carrying `meta_content_scope` and `include`ing an EOL popper.
///
/// On `bar\n`, after the line terminates the stack should hold no
/// atoms of `meta.string.test`; without the fix the scope leaks to
/// the next line because the chained `set`s leave the popper
/// without a valid non-consuming push recorded for loop protection,
/// so the zero-width `$` match ends up guarded as a potential loop.
#[test]
fn chained_set_with_included_eol_popper_pops_at_line_boundary() {
    let syntax = SyntaxDefinition::load_from_str(
        r#"
                name: EOL Pop Chained Test
                scope: source.test
                contexts:
                  main:
                    - match: (?=\S)
                      push: outer
                  outer:
                    - match: ''
                      set: [value-body, eat-whitespace-then-pop]
                  eat-whitespace-then-pop:
                    - match: \s*
                      pop: 1
                  value-body:
                    - match: ''
                      set: value-content
                  value-content:
                    - meta_content_scope: meta.string.test
                    - include: pop-on-eol
                  pop-on-eol:
                    - match: $
                      pop: 1
                "#,
        true,
        None,
    )
    .unwrap();

    let syntax_set = link(syntax);
    let mut state = ParseState::new(&syntax_set.syntaxes()[0]);
    let o = ops(&mut state, "bar\n", &syntax_set);

    // Apply ops against a fresh ScopeStack and check the final set
    // of live scope atoms — the meta_content_scope must not survive
    // across the `\n` boundary.
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
        !final_scopes.iter().any(|s| s.contains("meta.string.test")),
        "meta.string.test leaked past EOL; final scope stack: {:?}",
        final_scopes
    );
}

#[test]
fn v2_set_pops_meta_content_scope_from_matched_text() {
    // Kills: L1009 replace += with -= or *= in push_meta_ops
    // When a v2 syntax uses `set`, num_to_pop must include
    // cur_context.meta_scope.len() so that the old meta scope is removed.
    let syntax_str = r#"
name: V2SetMeta
scope: source.v2setmeta
version: 2
contexts:
  main:
    - match: '(?=\S)'
      push: ctx-a
  ctx-a:
    - meta_scope: meta.a.v2setmeta
    - match: 'GO'
      scope: keyword.go.v2setmeta
      set: ctx-b
    - match: '\w+'
      scope: word.a.v2setmeta
  ctx-b:
    - meta_scope: meta.b.v2setmeta
    - match: '\w+'
      scope: word.b.v2setmeta
      pop: true
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let raw_ops = ops(&mut state, "GO hello\n", &ss);
    let states = stack_states(raw_ops);

    // After "GO" triggers `set: ctx-b`, meta.a should be popped and
    // meta.b should be active on "hello".  If num_to_pop is wrong
    // (e.g. subtracted instead of added), meta.a would persist.
    let last_state = states.last().expect("expected some states");
    assert!(
        !last_state.contains("meta.a.v2setmeta"),
        "meta.a should have been popped after set, got: {:?}",
        last_state
    );
}

#[test]
fn v2_set_from_context_with_clear_scopes_restores_cleared_atoms() {
    // When a `set:` fires from a context that had `clear_scopes` of its
    // own (e.g. JSON's `object-value-body`), the cleared scopes must be
    // restored at the correct position on the scope stack: below the
    // target's pushed meta_scope, not on top of it.
    //
    // Previously the Restore fired in the initial phase, before the
    // non-initial Pop of (cur.meta_scope + target.meta_scope). The Pop
    // then removed the restored atoms instead of the intended meta_scopes,
    // dropping cur's cleared state on the floor. This surfaced in the
    // JSON test as duplicate `meta.mapping.value.json` atoms in nested
    // objects — e.g. `[source.json, meta.mapping.value.json,
    // meta.mapping.value.json]` instead of `[source.json,
    // meta.mapping.value.json, meta.mapping.json]`.
    //
    // Reduced JSON-like repro: outer mapping pushes an inner value-body
    // that clears the outer mapping scope, then the inner value-body
    // `set`s to a follow-up context. The follow-up context's matched
    // text must see the outer mapping scope restored below it.
    let syntax_str = r#"
name: V2SetRestore
scope: source.v2setrestore
version: 2
contexts:
  main:
    - meta_scope: meta.outer.v2setrestore
    - match: '\{'
      push: value-body

  value-body:
    - clear_scopes: 1
    - meta_scope: meta.value.v2setrestore
    - match: 'x'
      scope: keyword.x.v2setrestore
      set: follow-up

  follow-up:
    - match: '\w+'
      scope: word.follow.v2setrestore
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let raw_ops = ops(&mut state, "{xhello\n", &ss);

    let states = stack_states(raw_ops);
    // Find the state while parsing "hello" in follow-up.
    let follow_states: Vec<_> = states
        .iter()
        .filter(|s| s.contains("word.follow.v2setrestore"))
        .collect();
    assert!(
        !follow_states.is_empty(),
        "expected to enter follow-up context, got states: {:?}",
        states
    );
    // The outer meta.outer scope must be restored below follow-up's word
    // scope. If the Restore landed above the target's meta_scope push (or
    // was dropped by the non-initial Pop), meta.outer would be missing.
    assert!(
        follow_states
            .iter()
            .any(|s| s.contains("meta.outer.v2setrestore")),
        "meta.outer must be restored after leaving value-body (which cleared it): {:?}",
        follow_states
    );
    // meta.value (from the cleared context) must NOT persist.
    assert!(
        !follow_states
            .iter()
            .any(|s| s.contains("meta.value.v2setrestore")),
        "meta.value (from the exited context) must not leak into follow-up: {:?}",
        follow_states
    );
}

#[test]
fn v2_set_to_target_with_clear_scopes_clears_parent_meta_content_scope() {
    // Reduced from Lisp `function-parameter-list` → `function-parameter-list-body`:
    // the enclosing `function-body` supplies `meta_content_scope:
    // meta.function.lisp`; the inner parameter-list-body declares
    // `clear_scopes: 1` so the `(` and the parameter identifiers inside
    // are not double-scoped with the outer `meta.function`.
    //
    // The `(` token itself (the `set:` trigger) should see the cleared
    // stack — i.e. `meta.function.lisp` is already gone at that column.
    // Previously the v2 initial phase for Set pushed target.meta_scope
    // above the outer mcs without clearing first, so the trigger token
    // reported `[..., meta.function.lisp, meta.function.parameters.lisp,
    // punctuation...]` instead of `[..., meta.function.parameters.lisp,
    // punctuation...]`.
    let syntax_str = r#"
name: V2SetTargetClear
scope: source.v2settargetclear
version: 2
contexts:
  main:
    - match: '\('
      scope: punctuation.section.parens.begin.v2settargetclear
      push: [body, params-open]

  body:
    - meta_content_scope: meta.function.v2settargetclear
    - match: '\)'
      scope: punctuation.section.parens.end.v2settargetclear
      pop: 1

  params-open:
    - match: '\('
      scope: punctuation.section.parameters.begin.v2settargetclear
      set: params-body
    - include: else-pop

  params-body:
    - clear_scopes: 1
    - meta_scope: meta.function.parameters.v2settargetclear
    - match: '\)'
      scope: punctuation.section.parameters.end.v2settargetclear
      pop: 1
    - match: '\w+'
      scope: variable.parameter.v2settargetclear
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    // Mirrors `(defun averagenum (n1 n2))`: outer `(...)` carries
    // meta.function; inner `(...)` is parameter list.
    let raw_ops = ops(&mut state, "( (n1 n2))\n", &ss);

    let states = stack_states(raw_ops);

    // Find the state covering the inner `(` at column 2 (the set trigger).
    // Every state recorded once the parameters context has been entered
    // must NOT still carry the outer meta.function atom.
    let param_states: Vec<_> = states
        .iter()
        .filter(|s| s.contains("meta.function.parameters.v2settargetclear"))
        .collect();
    assert!(
        !param_states.is_empty(),
        "expected to enter params-body context, got states: {:?}",
        states
    );
    // The outer `meta.function` atom (from `body`'s meta_content_scope)
    // must be absent on every state where params-body is active. Match
    // the exact atom name — not a prefix — so that
    // `meta.function.parameters.v2settargetclear` doesn't trigger.
    let outer = "<meta.function.v2settargetclear>";
    for s in &param_states {
        assert!(
            !s.contains(outer),
            "outer meta.function must be cleared under params-body, \
                 but found it alongside meta.function.parameters: {:?}",
            s
        );
    }

    // After the inner `)` pops params-body the clear must Restore, so
    // the outer `meta.function` atom reappears before the outer `)`.
    let after_inner_close: Vec<_> = states
        .iter()
        .rev()
        .take_while(|s| !s.contains("meta.function.parameters.v2settargetclear"))
        .collect();
    assert!(
        after_inner_close
            .iter()
            .any(|s| s.contains("meta.function.v2settargetclear")),
        "meta.function must be restored after params-body pops, got trailing states: {:?}",
        after_inner_close
    );
}

#[test]
fn pop_n_set_with_cur_clear_scopes_restores_before_popping_deeper_frames() {
    // `pop: N + set: X` fired from a context that itself declares
    // `clear_scopes` at the context level: the deeper popped frame's
    // meta_content_scope is partly on the live scope stack and partly
    // in `clear_stack` (stripped by cur's Clear on entry). Emitting the
    // compound Pop before Restore makes Pop eat atoms from below the
    // intended popped range, dropping the outer frame's meta_scope.
    // Shape mirrors Batch File's `cmd-set-quoted-value-inner-end`
    // (`clear_scopes: 1`) firing `pop: 2, set: ignored-tail-outer` below
    // a `cmd-set-quoted-value-inner` that carries a 2-atom
    // meta_content_scope.
    let syntax_str = r#"
name: PopNSetClear
scope: source.popnsetclear
version: 2
contexts:
  main:
    - match: 'a'
      scope: p.a
      push: [outer, middle]

  outer:
    - meta_scope: outer.test
    - match: 'z'
      pop: 1

  middle:
    - meta_content_scope: mid1.test mid2.test
    - match: 'b'
      scope: p.b
      push: top

  top:
    - clear_scopes: 1
    - match: 'c'
      scope: p.c
      pop: 2
      set: target

  target:
    - meta_scope: target.test
    - match: 'd'
      scope: p.d
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let raw_ops = ops(&mut state, "abcd", &ss);
    let states = stack_states(raw_ops);

    // The scope stack state recorded on the `d` token (in `target`):
    // must contain `outer.test` (outer's meta_scope still on the stack)
    // and `target.test` (target's meta_scope, pushed by the pop:2+set:)
    // and must NOT contain `mid1.test` or `mid2.test` (middle was popped
    // by pop:2 and its meta_content_scope atoms — one live, one in
    // clear_stack — should both be gone).
    let d_state = states
        .iter()
        .find(|s| s.contains("p.d"))
        .unwrap_or_else(|| panic!("expected a state containing `p.d`, got: {:?}", states));
    assert!(
        d_state.contains("outer.test"),
        "outer.test must survive pop:2+set: (it sits below the popped range): {}",
        d_state
    );
    assert!(
        d_state.contains("target.test"),
        "target.test must be on the stack (target was pushed by set:): {}",
        d_state
    );
    assert!(
        !d_state.contains("mid1.test") && !d_state.contains("mid2.test"),
        "middle's meta_content_scope atoms must not linger after pop:2+set:, \
             including the atom that was in clear_stack: {}",
        d_state
    );
}

#[test]
fn pop_n_set_restores_deeper_frame_clear_scopes() {
    // `pop: N + set: [...]` (set_pop_count > 1) where one of the
    // popped DEEPER frames has `clear_scopes`: the deeper frame's
    // cleared atoms must be restored as part of the unwind, otherwise
    // they linger in `clear_stack` and the per-target Clear that
    // follows bites one atom too deep on the visible stack.
    //
    // Shape mirrors the Python `r'''(?ix:...)` triple-quoted-string
    // regex case: the regex embed pushes `base-literal-extended`
    // (a 1-atom meta_scope), then `(` matches `groups-extended` and
    // pushes `[group-body-extended_outer, maybe-unexpected-quantifiers,
    // group-start]`. `group-body-extended` has `clear_scopes: 1` (it
    // clears `base-literal-extended`'s ms atom) and a 2-atom meta_scope.
    // `(?ix:` then matches `group-start`'s `pop: 3 + set:
    // [group-body-extended_target, maybe-unexpected-quantifiers]` —
    // a multi-context set that unwinds the three pushed frames and
    // re-pushes [group-body-extended_target, maybe-unexpected-quantifiers].
    // Without the per-depth Restore in the unwind,
    // `group-body-extended_outer`'s cleared atom stayed in clear_stack
    // and the new target's `clear_scopes: 1` then cleared the
    // `source.regexp.python` mcs atom from below — leaking it from the
    // body content scope.
    let syntax_str = r#"
name: PopNSetDeeperClear
scope: source.popnsetdeepclear
version: 2
contexts:
  main:
    - match: 'a'
      scope: p.a
      push: outer

  outer:
    - meta_content_scope: keep-me.test
    - match: 'b'
      scope: p.b
      push: lit

  lit:
    - meta_scope: clear-me.test
    - match: 'c'
      scope: p.c
      push: [frame, inner]

  frame:
    - clear_scopes: 1
    - meta_scope: fra.test frb.test
    - match: 'z'
      pop: 1

  inner:
    - match: 'd'
      scope: p.d
      pop: 2
      set: [target, popper]

  target:
    - clear_scopes: 1
    - meta_scope: tgt.test
    - match: 'e'
      scope: p.e
      pop: 1

  popper:
    - match: ''
      pop: 1
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let raw_ops = ops(&mut state, "abcde", &ss);
    let states = stack_states(raw_ops);

    // The state recorded on the body token `e` (in `target`) must
    // contain `keep-me.test` (outer's mcs, which sits below the
    // popped frames) and `tgt.test` (target's ms). It must NOT
    // contain `fra.test`/`frb.test` (frame's ms — the popped frame)
    // or `clear-me.test` (cleared by frame on entry, restored by
    // the deeper-clear unwind, then cleared again by target's
    // own `clear_scopes: 1`).
    let e_state = states
        .iter()
        .find(|s| s.contains("p.e"))
        .unwrap_or_else(|| panic!("expected a state containing `p.e`, got: {:?}", states));
    assert!(
        e_state.contains("keep-me.test"),
        "keep-me.test must survive pop:3+set: with deeper clear_scopes \
             (without per-depth Restore, target's clear ate this atom): {}",
        e_state
    );
    assert!(
        e_state.contains("tgt.test"),
        "tgt.test must be on the stack (target was pushed by set:): {}",
        e_state
    );
    assert!(
        !e_state.contains("fra.test") && !e_state.contains("frb.test"),
        "frame's meta_scope must not linger after pop:3+set:: {}",
        e_state
    );
    assert!(
        !e_state.contains("clear-me.test"),
        "clear-me.test must be cleared by target's clear_scopes:1 \
             (it was first cleared by frame on entry, restored by the \
             deeper-clear unwind, then cleared again by target): {}",
        e_state
    );
}

#[test]
fn pop_n_set_with_stacked_meta_scopes_keeps_deeper_meta_scope_at_trigger() {
    // `pop: N + set:` is the documented ST exception to the "pop is
    // lookahead" rule — it is **stacking**: the trigger token receives
    // BOTH the popped frames' meta_scope AND the target's meta_scope.
    // Probe-witnessed against ST 4200 with the equivalent
    // `PopFirstProbe.sublime-syntax` (input `[()]`, rule
    // `pop: 2, set: target` on `)` with mid/inner/target each carrying
    // meta_scope) — ST emits
    // `source.X meta.mid meta.inner meta.target punctuation.close.paren`
    // at the `)` trigger. See
    // ~/.claude/.../project_syntect_st_divergences.md.
    let syntax_str = r#"
name: PopNSetStacked
scope: source.popnsetstacked
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
    - meta_scope: meta.inner
    - match: 'c'
      scope: p.c
      pop: 2
      set: target

  target:
    - meta_scope: meta.target
    - match: 'd'
      scope: p.d
      pop: 1
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let raw_ops = ops(&mut state, "abcd", &ss);
    let states = stack_states(raw_ops);

    let c_state = states
        .iter()
        .find(|s| s.contains("p.c"))
        .unwrap_or_else(|| panic!("expected a state containing `p.c`, got: {:?}", states));
    // ST stacking: the trigger token receives the popped frames'
    // meta_scope (mid + inner) AND the pushed target's meta_scope.
    assert!(
        c_state.contains("meta.mid"),
        "meta.mid (deeper popped frame's meta_scope) must remain visible \
             at the `c` trigger of `pop: 2 + set:` (ST stacking semantics): {}",
        c_state
    );
    assert!(
        c_state.contains("meta.inner"),
        "meta.inner (cur context's meta_scope) must remain visible at \
             the `c` trigger of `pop: 2 + set:`: {}",
        c_state
    );
    assert!(
        c_state.contains("meta.target"),
        "meta.target (pushed target's meta_scope) must be visible at \
             the `c` trigger of `pop: 2 + set:`: {}",
        c_state
    );

    // After the trigger, the stack must collapse to main + target so
    // subsequent body tokens see only `meta.target`.
    let d_state = states
        .iter()
        .find(|s| s.contains("p.d"))
        .unwrap_or_else(|| panic!("expected a state containing `p.d`, got: {:?}", states));
    assert!(
        !d_state.contains("meta.mid") && !d_state.contains("meta.inner"),
        "popped frames' meta_scope must not linger past the trigger: {}",
        d_state
    );
    assert!(
        d_state.contains("meta.target"),
        "meta.target must remain on the stack for subsequent tokens: {}",
        d_state
    );
}

#[test]
fn pop_n_set_with_leading_atom_keeps_duplicate_at_trigger() {
    // ST stacking for `pop:N + set:` keeps the popped frames'
    // `meta_scope` AND the rule's `scope:` even when the rule's
    // leading atom is identical to the popped frame's `meta_scope`.
    // Probe-witnessed against ST 4200 (SetDedupProbe): outer with
    // `meta_scope: meta.outer`, rule
    // `match: ';' scope: meta.outer punctuation.semi set: target`.
    // ST emits `source.X meta.outer meta.target meta.outer
    // punctuation.semi` at the `;` trigger — `meta.outer` appears
    // twice. Pre-fix syntect collapsed the duplicate via
    // `pat_scope_skip_count`. See
    // `~/.claude/.../project_syntect_st_divergences.md`.
    let syntax_str = r#"
name: SetDedupProbe
scope: source.setdedupprobe
version: 2
contexts:
  main:
    - match: 'a'
      scope: p.a
      push: outer

  outer:
    - meta_scope: meta.outer
    - match: ';'
      scope: meta.outer punctuation.semi
      set: target

  target:
    - meta_scope: meta.target
    - match: 'd'
      scope: p.d
      pop: 1
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let raw_ops = ops(&mut state, "a;d", &ss);
    let states = stack_states(raw_ops);

    let semi_state = states
        .iter()
        .find(|s| s.contains("punctuation.semi"))
        .unwrap_or_else(|| {
            panic!(
                "expected a state containing `punctuation.semi`, got: {:?}",
                states
            )
        });
    // ST stacking with leading-atom duplicate: meta.outer appears
    // twice (once from popped meta_scope, once from rule scope).
    let outer_count = semi_state.matches("meta.outer").count();
    assert_eq!(
        outer_count, 2,
        "meta.outer must appear twice on the `;` trigger of \
             `pop: 1 + set:` whose rule scope leads with the popped \
             frame's meta_scope (ST stacking — no leading-atom collapse): {}",
        semi_state
    );
    assert!(
        semi_state.contains("meta.target"),
        "meta.target (pushed target's meta_scope) must be visible \
             at the `;` trigger: {}",
        semi_state
    );
    assert!(
        semi_state.contains("punctuation.semi"),
        "punctuation.semi (rule's last scope atom) must be visible \
             at the `;` trigger: {}",
        semi_state
    );
}

#[test]
fn pop_n_set_without_deeper_clear_scopes_unaffected() {
    // Same shape as the test above but with no `clear_scopes` on the
    // deeper popped frame. Verifies the head-pop split doesn't change
    // behavior when the per-depth Restore would be a no-op — defends
    // against regression of TS's
    // `(?:get|set|async){{identifier_break}} pop: 2 + set:` in
    // `object-property-name` and similar genuine `pop:N + set:` shapes
    // where deeper frames have no clears. (Java's
    // `pop: 2 + push: annotation-parameters-body` was previously cited
    // here but now parses as `Push { pop_count: 2 }` and exercises a
    // distinct lookahead path.)
    let syntax_str = r#"
name: PopNSetNoDeeperClear
scope: source.popnsetnodeepclear
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
      push: [frame, inner]

  frame:
    - meta_scope: fra.test frb.test
    - match: 'z'
      pop: 1

  inner:
    - match: 'c'
      scope: p.c
      pop: 2
      set: target

  target:
    - meta_scope: tgt.test
    - match: 'd'
      scope: p.d
      pop: 1
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let raw_ops = ops(&mut state, "abcd", &ss);
    let states = stack_states(raw_ops);

    let d_state = states
        .iter()
        .find(|s| s.contains("p.d"))
        .unwrap_or_else(|| panic!("expected a state containing `p.d`, got: {:?}", states));
    assert!(
        d_state.contains("outer.test"),
        "outer.test must survive pop:2+set: (sits below the popped range): {}",
        d_state
    );
    assert!(
        d_state.contains("tgt.test"),
        "tgt.test must be on the stack (target was pushed by set:): {}",
        d_state
    );
    assert!(
        !d_state.contains("fra.test") && !d_state.contains("frb.test"),
        "frame's meta_scope must not linger after pop:2+set:: {}",
        d_state
    );
}

#[test]
fn cur_meta_scope_set_to_target_with_clear_scopes() {
    // Plain `set:` (no pop_count) from a context that itself carries
    // `clear_scopes` AND `meta_scope`, into a target that carries
    // `clear_scopes` AND `meta_content_scope`: the initial-phase Clear
    // for target ordinarily emitted by single-context-set previously
    // hid cur's meta_scope (which sits on top of the visible stack at
    // that point). The non-initial Pop then ate the wrong atom (parent's
    // last meta atom) and the trailing Restore resurrected cur.ms back
    // onto the stack instead of the parent atom the Clear was meant to
    // hide. The shape mirrors Bash's tilde-interpolation:
    //   maybe-tilde-interp  -> tilde-modifier (clear+ms)
    //   tilde-modifier (''-empty) -> tilde-modifier-username (clear+mcs)
    //   username pops on lookahead, leaving the parent intact.
    // After this fix, single-context-set defers the target Clear to the
    // non-initial phase whenever cur has ms/mcs, so Pop and Restore
    // operate on the correct atoms.
    //
    // Counterpart to `v2_set_to_target_with_clear_scopes_clears_parent_meta_content_scope`,
    // which exercises the cur-empty case (initial Clear stays as-is so
    // the trigger token sees the cleared stack).
    let syntax_str = r#"
name: CurMsSetTargetClear
scope: source.curmssettargetclear
version: 2
contexts:
  main:
    - match: 'p'
      scope: p.p
      push: parent

  parent:
    - meta_scope: parent1.test parent2.test
    - match: '~'
      scope: keyword.tilde
      set: cur

  cur:
    - clear_scopes: 1
    - meta_scope: cur.test
    - match: ''
      set: target

  target:
    - clear_scopes: 1
    - meta_content_scope: target.mcs1.test target.mcs2.test
    - match: '(?=/)'
      pop: 1
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    // "p~/" — `p` enters parent, `~` sets cur, `''` (zero-width) sets
    // target, `(?=/)` pops target leaving the parser back in `parent`.
    // The trailing literal char is `x` so we get a recorded token after
    // target has popped.
    let raw_ops = ops(&mut state, "p~/x\n", &ss);
    let states = stack_states(raw_ops);

    // Find the state covering `x` after the username has popped: parent
    // was never replaced (set was on parent's `~` rule, but that set: only
    // replaces parent's wrapper... actually parent IS replaced. After
    // pop from target and target's cur (set chain), nothing in `parent`
    // is on the stack — only `main`. So `x` is matched at `main`.)
    // What we really want to assert: between target's pop and any later
    // content, the visible stack must NOT contain `cur.test` — that's
    // the leak this fix targets.
    let leaked: Vec<_> = states.iter().filter(|s| s.contains("cur.test")).collect();
    // cur.test may legitimately appear during the `~` token (cur's
    // meta_scope applies to the trigger of the set). It must NOT appear
    // in any state recorded AFTER target was entered, because at that
    // point cur is gone from the stack.
    // Identify the cutoff: the first state that contains
    // `target.mcs1.test` marks the target-active region; from there
    // onward, `cur.test` must not appear.
    let target_first = states.iter().position(|s| s.contains("target.mcs1.test"));
    if let Some(idx) = target_first {
        for (i, s) in states.iter().enumerate().skip(idx) {
            assert!(
                !s.contains("cur.test"),
                "cur.test must not linger from index {} onward (target entered at {}): {}",
                i,
                idx,
                s
            );
        }
    } else {
        // If target's mcs never landed on any recorded state, the test
        // can't pin the lifetime — fall back to the simpler invariant.
        assert!(
            leaked.is_empty(),
            "cur.test leaked into recorded states after the SET chain: {:?}",
            leaked
        );
    }
}

#[test]
fn multi_set_target_clear_with_target_mcs_only_does_not_extra_drop() {
    // Companion to `php_multi_set_target_clear_drops_extra_parent_mcs_on_trigger`:
    // the same shape but the clear-bearing target has only
    // `meta_content_scope` (no `meta_scope`). ST does NOT drop any
    // extra atom on the trigger here; the trigger keeps both parent
    // mcs atoms. Real-world: Zsh's
    // `zsh-redirection-glob-range-end` (`clear_scopes: 1` +
    // `meta_content_scope: meta.range.shell.zsh`, no meta_scope) at
    // the head of a 5-context set from `zsh-redirection-glob-range-begin`.
    // Without this gate, the fix above strips
    // `meta.function-call.arguments.shell` and even
    // `source.shell.zsh` on the `<` trigger.
    let syntax_str = r#"
name: ZshLikeMultiSetTargetMcsOnly
scope: source.zshlikemulti
version: 2
contexts:
  main:
    - match: '(?=\S)'
      push: outer

  outer:
    - meta_content_scope: outer.mcs.test
    - match: 'F'
      scope: keyword.f.test
      push: parent

  parent:
    - meta_content_scope: parent.mcs.test
    - match: '\('
      scope: parent.lparen.test
      push: cur

  cur:
    - match: ':'
      scope: trigger.colon.test
      set: [body, helper]
    - match: '(?=\S)'
      pop: 1

  body:
    - clear_scopes: 1
    - meta_content_scope: body.mcs.test
    - match: '\w+'
      scope: body.word.test
      pop: 1

  helper:
    - match: '(?=\S)'
      pop: 1
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let raw_ops = ops(&mut state, "F(:foo\n", &ss);
    let states = stack_states(raw_ops);

    let colon_state = states
        .iter()
        .find(|s| s.contains("trigger.colon.test"))
        .unwrap_or_else(|| {
            panic!(
                "expected a state containing trigger.colon.test, got: {:?}",
                states
            )
        });
    // No extra-drop must apply: both parent atoms must remain on the
    // trigger token. The Clear(1) for body's clear_scopes is
    // post-match, so even parent.mcs is still on the trigger.
    assert!(
        colon_state.contains("parent.mcs.test"),
        "trigger must keep parent.mcs (target has no meta_scope, no extra-drop): {}",
        colon_state
    );
    assert!(
        colon_state.contains("outer.mcs.test"),
        "trigger must keep outer.mcs (target has no meta_scope, no extra-drop): {}",
        colon_state
    );
}

#[test]
fn v2_set_clear_scopes_applies_from_every_context() {
    // v2: when `set:` lists multiple contexts, `clear_scopes` on any of
    // them — not just the topmost — applies at that context's own
    // position in the stack. The canonical real-world case is Bash's
    //   set: [def-function-body, def-function-params, def-function-name]
    // where `def-function-params` (the middle context) carries
    // `clear_scopes: 1`. The Clear strips the atom that the preceding
    // context's meta_content_scope just pushed, matching Sublime
    // Text's observed behaviour. Previously this test guessed
    // Sublime pinned Clear to the topmost context only; running real
    // v2 syntaxes (Bash function definitions, among others) refuted
    // that guess.
    let syntax_str = r#"
name: V2ClearMid
scope: source.v2clear
version: 2
contexts:
  main:
    - meta_scope: meta.main.v2clear
    - match: 'GO'
      set: [ctx-bottom, ctx-middle, ctx-top]
  ctx-bottom:
    - meta_content_scope: mcs.bottom.v2clear
  ctx-middle:
    - clear_scopes: 1
    - meta_content_scope: mcs.middle.v2clear
  ctx-top:
    - meta_content_scope: mcs.top.v2clear
    - match: '\w+'
      scope: word.top.v2clear
      pop: true
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let raw_ops = ops(&mut state, "GO hello\n", &ss);
    let states = stack_states(raw_ops);

    // "hello" matches in ctx-top. At that point ctx-middle's
    // clear_scopes: 1 must have stripped ctx-bottom's
    // meta_content_scope atom.
    let hello_states: Vec<_> = states
        .iter()
        .filter(|s| s.contains("word.top.v2clear"))
        .collect();
    assert!(
        !hello_states.is_empty(),
        "expected word.top.v2clear, got states: {:?}",
        states
    );
    for s in &hello_states {
        assert!(
            !s.contains("mcs.bottom.v2clear"),
            "ctx-bottom's mcs should have been cleared by ctx-middle's \
                 clear_scopes: 1 before ctx-top's match, got: {:?}",
            s
        );
        assert!(
            s.contains("mcs.middle.v2clear"),
            "ctx-middle's mcs should be on the stack during ctx-top's match, \
                 got: {:?}",
            s
        );
    }

    // Reaching this point without a panic proves the Restore emitted on
    // ctx-middle's pop didn't underflow the clear_stack — the
    // regression the Bash `func () {}` minimal reproducer uncovered.
}

#[test]
fn v2_multi_set_non_topmost_clear_scopes_strips_preceding_meta_scope_at_trigger() {
    // v2: in a multi-context `set:` whose non-topmost target declares
    // `clear_scopes: N` + a non-empty `meta_content_scope` (and an
    // empty `meta_scope`), the Clear must strip atoms that EARLIER
    // contexts in the set list pushed via their `meta_scope` — and
    // the strip must be visible to the TRIGGER match's own scopes
    // (top-level `scope:` and capture scopes).
    //
    // Real-world repro: Zsh's `zsh-redirection-glob-range-begin` is
    // entered via a pop+branch from `redirection-input`. Its match
    // `(\d*)(<)` runs `set: [string-path-pattern-body,
    // zsh-redirection-glob-range-end, zsh-glob-range-number,
    // zsh-redirection-glob-range-operator, zsh-glob-range-number]`.
    // `string-path-pattern-body` has
    // `meta_scope: meta.string.glob.shell string.unquoted.shell`, and
    // `zsh-redirection-glob-range-end` has
    // `clear_scopes: 1` + `meta_content_scope: meta.range.shell.zsh`.
    // The capture-2 scope `meta.range.shell.zsh
    // punctuation.definition.range.begin.shell.zsh` is asserted with
    // `- string` exclusion, so `string.unquoted.shell` must be hidden
    // at the `<` token.
    let syntax_str = r#"
name: V2MultiSetNonTopClear
scope: source.v2mscstrigger
version: 2
contexts:
  main:
    - match: '\['
      scope: punctuation.section.brackets.begin
      set: middle
  middle:
    - meta_include_prototype: false
    - match: '(<)'
      captures:
        1: meta.range.begin punctuation.definition.range.begin
      set:
        - body
        - end
        - top
  body:
    - meta_include_prototype: false
    - meta_scope: meta.glob string.unquoted
    - match: '\]'
      scope: punctuation.section.brackets.end
      pop: true
  end:
    - clear_scopes: 1
    - meta_include_prototype: false
    - meta_content_scope: meta.range
    - match: '>'
      scope: punctuation.definition.range.end
      pop: 1
  top:
    - meta_include_prototype: false
    - match: '\d+'
      scope: constant.numeric
"#;
    let syntax = SyntaxDefinition::load_from_str(syntax_str, true, None).unwrap();
    let ss = link(syntax);
    let mut state = ParseState::new(&ss.syntaxes()[0]);
    let raw_ops = ops(&mut state, "[<1>x]\n", &ss);
    let states = stack_states(raw_ops);

    // The `<` token (capture 1) must carry meta.glob + meta.range.begin
    // + punctuation.definition.range.begin, with `string.unquoted`
    // CLEARED by end's `clear_scopes: 1`.
    let begin_states: Vec<_> = states
        .iter()
        .filter(|s| s.contains("punctuation.definition.range.begin"))
        .collect();
    assert!(
        !begin_states.is_empty(),
        "expected punctuation.definition.range.begin in some state, got: {:?}",
        states
    );
    for s in &begin_states {
        assert!(
            s.contains("meta.glob"),
            "meta.glob should remain on the stack at the `<` token, got: {:?}",
            s
        );
        assert!(
            !s.contains("string.unquoted"),
            "string.unquoted should have been cleared by end's \
                 `clear_scopes: 1` on entry, got: {:?}",
            s
        );
        assert!(
            s.contains("meta.range.begin"),
            "meta.range.begin (capture scope) must be on the stack at the \
                 `<` token, got: {:?}",
            s
        );
    }

    // After the trigger, body content (`1`) sees end's
    // meta_content_scope (meta.range) and NOT string.unquoted.
    let digit_states: Vec<_> = states
        .iter()
        .filter(|s| s.contains("constant.numeric"))
        .collect();
    assert!(
        !digit_states.is_empty(),
        "expected constant.numeric in some state, got: {:?}",
        states
    );
    for s in &digit_states {
        assert!(
            !s.contains("string.unquoted"),
            "string.unquoted must stay cleared while inside the range body, \
                 got: {:?}",
            s
        );
        assert!(
            s.contains("meta.range"),
            "end's meta_content_scope (meta.range) must be on the body \
                 content's stack, got: {:?}",
            s
        );
    }
}

#[cfg(feature = "default-onig")]
#[test]
fn pop_n_push_with_target_meta_scope_drops_deeper_meta_scope_at_trigger() {
    // Java's `@RunWith(JUnit4.class)` inside a class block:
    // `annotation-unqualified-parameters`'s
    // `match: \( pop: 2 push: annotation-parameters-body` becomes
    // `Push { pop_count: 2, ctx_refs: [annotation-parameters-body] }`
    // per yaml_load. ST treats `pop:N + push:` as **lookahead** — the
    // popped frames' meta_scope atoms are dropped from the visible
    // stack before the trigger token records its scope. Without this,
    // `annotation-unqualified-identifier`'s `meta.annotation.identifier.java`
    // (the deeper popped frame's meta_scope) leaks onto the `(` trigger.
    use crate::parsing::SyntaxSet;
    let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
    let syntax = ss
        .find_syntax_by_path("Packages/Java/Java.sublime-syntax")
        .unwrap();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let lines = [
        "class MethodDeclarationTests {\n",
        "  @RunWith(JUnit4.class)\n",
    ];
    let ann = Scope::new("meta.annotation.identifier.java").unwrap();
    let params = Scope::new("meta.annotation.parameters.java").unwrap();
    let mut probed = false;
    for (line_idx, line) in lines.iter().enumerate() {
        let out = state.parse_line(line, &ss).expect("parse");
        for (idx, op) in &out.ops {
            let _ = stack.apply(op);
            if line_idx == 1 && *idx == 10 {
                let slice = stack.as_slice();
                if slice.contains(&params) {
                    assert!(
                        !slice.contains(&ann),
                        "meta.annotation.identifier.java leaked onto `(` trigger \
                             of `@RunWith(JUnit4.class)`; stack at idx 10: {:?}",
                        stack
                    );
                    probed = true;
                }
            }
        }
    }
    assert!(
        probed,
        "test never reached `(` trigger op at line 2 idx 10 — did the \
             ops layout change? final stack: {:?}",
        stack
    );
}

#[cfg(feature = "default-onig")]
#[test]
fn pop_n_restores_clear_before_unwinding_deeper_meta_scopes() {
    // Java's `case DayType when -> "incomplete";` lands in
    // `case-label-expression` (clear_scopes:1, mcs: case.label).
    // The `clear_scopes:1` hides the parent `case-label`'s
    // `meta_scope` (meta.case). When `case-label-end` matches
    // `->` with `pop: 2`, the rule must unwind both
    // `case-label-expression` AND `case-label`. With the deeper
    // meta_scope Pop emitted before the cur_context's clear was
    // restored, the consumer popped the wrong (still-visible)
    // scope — the surrounding `meta.block.java` (switch's block)
    // — leaving `meta.case.java` orphaned past the `->`.
    //
    // Restoring cur_context's clear BEFORE the depth-loop's
    // deeper-meta_scope pops makes the previously-cleared atom
    // visible again so it can be popped correctly.
    use crate::parsing::SyntaxSet;
    let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
    let syntax = ss
        .find_syntax_by_path("Packages/Java/Java.sublime-syntax")
        .unwrap();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    for line in [
        "class C {\n",
        "  void f(Object o) {\n",
        "    return switch (o) {\n",
        "       case DayType when -> \"incomplete\";\n",
    ] {
        let out = state.parse_line(line, &ss).expect("parse");
        for (_, op) in &out.ops {
            let _ = stack.apply(op);
        }
    }
    let case = Scope::new("meta.statement.conditional.case.java").unwrap();
    let label = Scope::new("meta.statement.conditional.case.label.java").unwrap();
    assert!(
        !stack.as_slice().contains(&case),
        "meta.statement.conditional.case.java leaked past `->`; stack: {:?}",
        stack,
    );
    assert!(
        !stack.as_slice().contains(&label),
        "meta.statement.conditional.case.label.java leaked past `->`; stack: {:?}",
        stack,
    );
}
