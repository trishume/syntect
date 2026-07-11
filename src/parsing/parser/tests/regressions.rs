//! Regressions named after the issue or syntax that exposed them.

use super::*;

#[test]
fn can_parse_issue25() {
    let ss = &*testdata::PACKAGES_SYN_SET;
    let mut state = {
        let syntax = ss.find_syntax_by_name("C").unwrap();
        ParseState::new(syntax)
    };

    // test fix for issue #25
    assert_eq!(ops(&mut state, "struct{estruct", ss).len(), 10);
}

#[test]
fn can_parse_issue120() {
    let syntax = SyntaxDefinition::load_from_str(
        include_str!("../../../../testdata/embed_escape_test.sublime-syntax"),
        false,
        None,
    )
    .unwrap();

    let line1 = "\"abctest\" foobar";
    let expect1 = [
            "<meta.attribute-with-value.style.html>, <string.quoted.double>, <punctuation.definition.string.begin.html>",
            "<meta.attribute-with-value.style.html>, <source.css>",
            "<meta.attribute-with-value.style.html>, <string.quoted.double>, <punctuation.definition.string.end.html>",
            "<meta.attribute-with-value.style.html>, <source.css>, <test.embedded>",
            "<top-level.test>",
        ];

    expect_scope_stacks_with_syntax(line1, &expect1, syntax.clone());

    let line2 = ">abctest</style>foobar";
    let expect2 = [
        "<meta.tag.style.begin.html>, <punctuation.definition.tag.end.html>",
        "<source.css.embedded.html>, <test.embedded>",
        "<top-level.test>",
    ];
    expect_scope_stacks_with_syntax(line2, &expect2, syntax);
}

#[test]
fn can_parse_issue176() {
    let syntax = r#"
scope: source.dummy
contexts:
  main:
    - match: (test)(?=(foo))(f)
      captures:
        1: test
        2: ignored
        3: f
      push:
        - match: (oo)
          captures:
            1: keyword
"#;

    let syntax = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
    expect_scope_stacks_with_syntax(
        "testfoo",
        &["<test>", /*"<ignored>",*/ "<f>", "<keyword>"],
        syntax,
    );
}

/// End-to-end check that `make syntest`'s Makefile failure has no
/// harness-level cause: loads the real Packages Makefile syntax
/// and parses two lines, asserting that after `bar := $(foo)\n`
/// the scope stack no longer carries `meta.string.makefile` when
/// the next source line is parsed.
#[test]
fn makefile_meta_string_does_not_leak_past_eol() {
    use crate::parsing::SyntaxSet;
    let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
    let syntax = ss
        .find_syntax_by_path("Packages/Makefile/Makefile.sublime-syntax")
        .unwrap();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    for (_, op) in ops(&mut state, "bar := $(foo)\n", &ss) {
        stack.apply(&op).unwrap();
    }
    let after_assignment: Vec<String> = stack
        .as_slice()
        .iter()
        .map(|s| format!("{:?}", s))
        .collect();
    assert!(
        !after_assignment
            .iter()
            .any(|s| s.contains("meta.string.makefile")),
        "meta.string.makefile leaks past EOL of `bar := $(foo)\\n`; stack: {:?}",
        after_assignment
    );
}

#[test]
fn php_multi_set_target_clear_drops_extra_parent_mcs_on_trigger() {
    // Multi-context `set:` whose target body has `clear_scopes: 1` AND
    // a non-empty `meta_scope`, fired from a cur with no ms/mcs/clear.
    // ST drops the immediate parent's mcs atom (Clear(1)) AND one
    // EXTRA atom (the next-deeper mcs) on the trigger token; the body
    // content sees only Clear(1) atoms gone, so the extra atom is
    // restored. Reduced from PHP `function bye(): never {`: cur is
    // `function-return-type`; target is
    // `[function-return-type-body, type-hint-simple-type]` with
    // `function-return-type-body` declaring `clear_scopes: 1` and
    // `meta_scope: meta.function.return-type.php`; the `:` sits below
    // `function-block`'s `meta_content_scope: meta.function.php` and
    // the embed wrapper's `source.php.embedded.html`, both of which
    // ST drops on the colon and only `source.php.embedded.html` is
    // restored for the body.
    let syntax_str = r#"
name: PhpMultiSetClear
scope: source.phpmultisetclear
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
    - meta_scope: body.ms.test
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
    // `F(:foo`: F enters parent via outer, `(` enters cur via parent,
    // `:` sets [body, helper], helper else-pops, body matches "foo".
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
    assert!(
        !colon_state.contains("parent.mcs.test"),
        "trigger must drop parent.mcs (Clear(1) target): {}",
        colon_state
    );
    assert!(
        !colon_state.contains("outer.mcs.test"),
        "trigger must drop outer.mcs (the EXTRA atom anchored by body.ms): {}",
        colon_state
    );
    assert!(
        colon_state.contains("body.ms.test"),
        "trigger must carry body.ms (target's meta_scope): {}",
        colon_state
    );

    let body_state = states
        .iter()
        .find(|s| s.contains("body.word.test"))
        .unwrap_or_else(|| {
            panic!(
                "expected a state containing body.word.test, got: {:?}",
                states
            )
        });
    assert!(
        !body_state.contains("parent.mcs.test"),
        "body must drop parent.mcs (Clear(1) target): {}",
        body_state
    );
    assert!(
        body_state.contains("outer.mcs.test"),
        "body must keep outer.mcs (the extra-drop is trigger-only): {}",
        body_state
    );
    assert!(
        body_state.contains("body.ms.test"),
        "body must carry body.ms: {}",
        body_state
    );
}

/// Regression guard for the qualified-class annotation duplicate
/// atoms case (`syntax_test_java.java:10108`). The
/// `annotation-qualified-identifier-name` rule's `scope:`
/// re-states the popped `annotation-qualified-identifier`'s
/// `meta_scope` atoms while `pop: 2 + branch:` unwinds them.
/// Defended by Branch+pop's lookahead semantics — `pop:N + branch:`
/// is dispatched as `Push { pop_count }`, so the trigger token
/// excludes the popped frames' `meta_scope` per ST and the rule's
/// re-stated atoms appear exactly once. Pre-fix this same shape
/// was suppressed by `pat_scope_skip_count` masking a stacking
/// synthesis (`Set { pop_count }`).
#[cfg(feature = "default-onig")]
#[test]
fn qualified_annotation_does_not_double_identifier_path_atoms() {
    use crate::parsing::SyntaxSet;
    let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
    let syntax = ss
        .find_syntax_by_path("Packages/Java/Java.sublime-syntax")
        .unwrap();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let line = "@ClassName.FixMethodOrder( MethodSorters.NAME_ASCENDING )\n";
    let out = state.parse_line(line, &ss).expect("parse");
    // Reconstruct the running stack at byte position 11 (start of
    // `FixMethodOrder`), where the duplicated push was visible.
    for (pos, op) in &out.ops {
        if *pos > 11 {
            break;
        }
        let _ = stack.apply(op);
    }
    let identifier = Scope::new("meta.annotation.identifier.java").unwrap();
    let path = Scope::new("meta.path.java").unwrap();
    let id_count = stack
        .as_slice()
        .iter()
        .filter(|s| **s == identifier)
        .count();
    let path_count = stack.as_slice().iter().filter(|s| **s == path).count();
    assert!(
        id_count == 1 && path_count == 1,
        "meta.annotation.identifier.java pushed {} times, meta.path.java pushed {} times \
             entering `FixMethodOrder` (expected each at most 1); stack: {:?}",
        id_count,
        path_count,
        stack,
    );
}

/// Regression: in a Markdown zsh fenced block, the indented shebang
/// `   #!/usr/bin/env zsh` must enter `comment.line.shebang.shell`
/// (lenient `Bash (for Markdown).main` rule), not the regular
/// `comment.line.number-sign.shell` (strict inherited Bash main).
/// `Zsh (for Markdown)` extends both `Bash (for Markdown)` (which
/// owns a custom `main`) and `Zsh` (which inherits Bash's standard
/// strict `main`). The parent merge must prefer the own definition
/// over the inherited one.
#[test]
fn zsh_for_markdown_uses_lenient_shebang_main_from_bash_for_markdown() {
    let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
    let md = ss
        .find_syntax_by_scope(Scope::new("text.html.markdown").unwrap())
        .expect("Markdown loaded");
    let mut state = ParseState::new(md);
    let shebang = Scope::new("comment.line.shebang.shell").unwrap();
    let number_sign = Scope::new("comment.line.number-sign.shell").unwrap();
    let mut saw_shebang = false;
    let mut saw_number_sign = false;
    for line in ["```zsh\n", "   #!/usr/bin/env zsh\n"] {
        let out = state.parse_line(line, &ss).expect("parse");
        for (_, op) in &out.ops {
            if let ScopeStackOp::Push(s) = op {
                if *s == shebang {
                    saw_shebang = true;
                }
                if *s == number_sign {
                    saw_number_sign = true;
                }
            }
        }
    }
    assert!(
        saw_shebang,
        "expected a Push(comment.line.shebang.shell) op (lenient \
             Bash (for Markdown).main wins over Zsh's inherited Bash main)"
    );
    assert!(
        !saw_number_sign,
        "must not fall through to comment.line.number-sign.shell \
             (regular comments rule)"
    );
}
