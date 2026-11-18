//! What a definition is supposed to produce, each case settled against Sublime Text.

mod sublime_text {
    use serde_json::Value;
    use syntect::parsing::{
        ParseState, ScopeStack, SyntaxDefinition, SyntaxSet, SyntaxSetBuilder,
    };

    /// A set holding just these definitions, linked.
    fn set_of(definitions: &[&str]) -> SyntaxSet {
        let mut builder = SyntaxSetBuilder::new();
        builder.add_plain_text_syntax();
        for yaml in definitions {
            builder.add(
                SyntaxDefinition::load_from_str(yaml, true, None).expect("the definition loads"),
            );
        }
        builder.build()
    }

    /// A `ParseState` is only valid against the set its syntax came from, and
    /// the wrong set does not fail — it answers.
    #[test]
    fn a_parse_state_answers_to_whichever_set_it_is_given() {
        let mine = "%YAML 1.2\n---\nname: One\nscope: source.one\nfile_extensions: [one]\ncontexts:\n  main:\n    - match: 'a'\n      scope: keyword.a.mine\n      push: inner\n  inner:\n    - match: 'b'\n      scope: keyword.b.mine\n      pop: true\n";
        let theirs = "%YAML 1.2\n---\nname: One\nscope: source.one\nfile_extensions: [one]\ncontexts:\n  main:\n    - match: 'a'\n      scope: keyword.a.theirs\n      push: inner\n  inner:\n    - match: 'b'\n      scope: keyword.b.theirs\n      pop: true\n";
        let ours = set_of(&[mine]);
        let other = set_of(&[theirs]);
        let syntax = ours.find_syntax_by_name("One").expect("the syntax");

        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let mut seen = Vec::new();
        for op in state
            .parse_line("ab", &other)
            .expect("no error: that is the point")
            .ops
        {
            stack.apply(&op.1).expect("the op applies");
            seen.push(
                stack
                    .as_slice()
                    .iter()
                    .map(|scope| scope.build_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        assert!(
            seen.iter().any(|one| one.contains("keyword.a.theirs")),
            "the other set did not answer after all: {seen:?}"
        );
        assert_ne!(
            seen,
            scopes(&ours, "One", "ab"),
            "the two sets are indistinguishable, so this proves nothing"
        );

        let empty = set_of(&[]);
        let mut state = ParseState::new(syntax);
        assert!(
            state.parse_line("ab", &empty).is_err(),
            "a set too short to hold the index answered anyway"
        );
    }

    /// Every scope stack the line passes through, as strings.
    fn scopes(set: &SyntaxSet, name: &str, line: &str) -> Vec<String> {
        let syntax = set
            .find_syntax_by_name(name)
            .unwrap_or_else(|| panic!("no syntax {name}"));
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let mut seen = Vec::new();
        for op in state.parse_line(line, set).expect("the line parses").ops {
            stack.apply(&op.1).expect("the op applies");
            seen.push(
                stack
                    .as_slice()
                    .iter()
                    .map(|scope| scope.build_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        seen
    }

    /// The same definition read both ways: version 1, and version 2.
    fn both_readings(body: &str) -> (SyntaxSet, SyntaxSet) {
        let one = format!("%YAML 1.2\n---\n{body}");
        let two = format!("%YAML 1.2\n---\nversion: 2\n{body}");
        (set_of(&[&one]), set_of(&[&two]))
    }

    /// The scope stack at the first token to be given `wanted`.
    fn stack_at(set: &SyntaxSet, name: &str, text: &str, wanted: &str) -> String {
        let syntax = set
            .find_syntax_by_name(name)
            .unwrap_or_else(|| panic!("no syntax {name}"));
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        for line in text.split_inclusive('\n') {
            for op in state.parse_line(line, set).expect("the line parses").ops {
                stack.apply(&op.1).expect("the op applies");
                let here: Vec<String> = stack
                    .as_slice()
                    .iter()
                    .map(|scope| scope.build_string())
                    .collect();
                if here.iter().any(|scope| scope == wanted) {
                    return here.join(" ");
                }
            }
        }
        panic!("nothing was given {wanted} in {text:?}");
    }

    /// The scope stack at every column of a one-line document.
    fn columns(set: &SyntaxSet, name: &str, line: &str) -> Vec<String> {
        let syntax = set
            .find_syntax_by_name(name)
            .unwrap_or_else(|| panic!("no syntax {name}"));
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let mut ops = state
            .parse_line(line, set)
            .expect("the line parses")
            .ops
            .into_iter()
            .peekable();
        let text = line.trim_end_matches('\n');
        let mut out = Vec::new();
        for column in 0..=text.len() {
            while ops.peek().is_some_and(|(at, _)| *at <= column) {
                let (_, op) = ops.next().expect("peeked");
                stack.apply(&op).expect("the op applies");
            }
            out.push(
                stack
                    .as_slice()
                    .iter()
                    .map(|scope| scope.build_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        out
    }

    /// Two contexts that `set` each other without consuming anything end the
    /// line, unread, at Sublime Text's limit on such actions.
    #[test]
    fn non_consuming_sets_that_answer_each_other_end_the_line() {
        let set = set_of(&[r#"%YAML 1.2
---
name: SetLoop
scope: source.setloop
contexts:
  main:
    - match: (?=test)
      set: first
  first:
    - match: (?=t)
      set: second
    - match: \w+
      scope: word.matched
  second:
    - match: (?=t)
      set: first
    - match: \w+
      scope: word.matched
"#]);
        let seen = scopes(&set, "SetLoop", "test");
        assert!(
            seen.iter().all(|stack| !stack.contains("word.matched")),
            "the rest of the line was read: {seen:?}"
        );
    }

    /// A `push:` of several contexts, popped all the way back at a position
    /// that never moved, terminates.
    #[test]
    fn a_multi_context_push_that_pops_all_the_way_back_does_not_hang() {
        let set = set_of(&[include_str!(
            "../testdata/sublime_probes/PushPopLoop.sublime-syntax"
        )]);
        let inside = "source.pushpoploop meta.outer.ppl meta.middle.ppl meta.inner.ppl";
        assert_eq!(
            columns(&set, "PushPopLoop", "qy\n"),
            vec![inside.to_string(); 3],
            "the guard suppressed more, or less, than the pop ST suppresses"
        );
    }

    /// And a pop that lands *below* where the push started is not that loop.
    #[test]
    fn a_pop_past_where_the_push_started_is_not_a_loop() {
        let set = set_of(&[
            "%YAML 1.2
---
name: PopBelow
scope: source.popbelow
file_extensions: [pbl]
contexts:
  main:
    - match: a
      scope: keyword.a
      push: wrap
  wrap:
    - meta_scope: meta.wrap
    - match: (?=b)
      push: probe
  probe:
    - meta_scope: meta.probe
    - match: (?=b)
      pop: 2
",
        ]);
        let seen = scopes(&set, "PopBelow", "ab\n");
        assert!(
            seen.iter().any(|at| at.contains("meta.probe")),
            "the push has to happen: {seen:?}"
        );
        let last = seen.last().expect("the line produced ops");
        assert!(
            !last.contains("meta.wrap") && !last.contains("meta.probe"),
            "and the `pop: 2` has to be let through — it lands below the depth \
             the push started from, so it is not the loop: {last}"
        );
    }

    /// A `pop: N` out of a context that cleared scopes restores them *before*
    /// it unwinds the frames underneath.
    #[test]
    fn a_pop_of_two_restores_what_it_cleared_before_it_unwinds() {
        let set = set_of(&[
            "%YAML 1.2
---
name: PopClearOrder
scope: source.popclearorder
version: 2
file_extensions: [pco]
contexts:
  main:
    - match: '\\\"'
      scope: punctuation.begin
      push: body
    - match: y
      scope: keyword.tail
  body:
    - meta_scope: meta.string string.quoted
    - match: '\\\\'
      scope: punctuation.continuation
      push: cont
  cont:
    - clear_scopes: 2
    - meta_scope: meta.continuation
    - match: '\\\"'
      scope: invalid.illegal
      pop: 2
",
        ]);
        let line = "\"\\\"y\n";
        // The `\"` that gives up on the continuation: inside the continuation's
        // own scope, with the string's two atoms cleared away.
        let at = stack_at(&set, "PopClearOrder", line, "invalid.illegal");
        assert_eq!(
            at, "source.popclearorder meta.continuation invalid.illegal",
            "the clear is still in force over the token that pops out of it"
        );
        // And after `pop: 2` the line is back at the top, with the cleared
        // atoms neither lost nor left standing.
        let after = stack_at(&set, "PopClearOrder", line, "keyword.tail");
        assert_eq!(
            after, "source.popclearorder keyword.tail",
            "the Restore has to happen before the frames it is about are \
             unwound, or the pop runs off the live stack and puts the string \
             back on top of what is left"
        );
    }

    /// A branch point an embed's escape left standing is still there to fail to.
    #[test]
    fn an_escape_that_pops_nothing_keeps_the_branch_point_under_it() {
        let set = set_of(&[
            "%YAML 1.2
---
name: EscapePrune
scope: source.escapeprune
file_extensions: [epr]
contexts:
  main:
    - match: a
      scope: keyword.a
      push: holder
  holder:
    - meta_scope: meta.holder
    - match: E
      scope: keyword.embed
      embed: guest
      escape: Q
  guest:
    - meta_scope: meta.guest
    - match: (?=b)
      branch_point: BP
      branch: [alt-one, alt-two]
      pop: 3
  alt-one:
    - meta_scope: meta.alt-one
    - match: b
      scope: keyword.b-one
    - match: F
      fail: BP
  alt-two:
    - meta_scope: meta.alt-two
    - match: b
      scope: keyword.b-two
    - match: F
      scope: keyword.no-fail
",
        ]);
        let seen = scopes(&set, "EscapePrune", "aEbQF\n");
        assert!(
            seen.iter().any(|at| at.contains("keyword.b-two")),
            "the `fail: BP` after the escape has to reach the branch point and \
             swap in the second alternative; pruned, it does nothing and the \
             line stays on the first: {seen:?}"
        );
    }

    /// An `escape` ends the text the embedded syntax can see, not just where
    /// its match may land.
    #[test]
    fn an_escape_ends_the_line_the_embedded_syntax_reads() {
        let set = set_of(&[
            "%YAML 1.2
---
name: EscapeCuts
scope: source.escapecuts
file_extensions: [ecut]
contexts:
  main:
    - match: '`'
      scope: punctuation.begin
      embed: inner
      embed_scope: meta.interp
      escape: '`'
      escape_captures:
        0: punctuation.end
  inner:
    - match: 'done(?![^\\s()|&<>;])'
      scope: keyword.end
    - match: '[A-Za-z]+'
      scope: variable.other
",
        ]);
        let at = stack_at(&set, "EscapeCuts", "`done`\n", "keyword.end");
        assert!(
            at.contains("meta.interp keyword.end"),
            "the word before the escape has to be read against a line that ends \
             at the escape, so its lookahead does not see the escape itself: {at}"
        );
    }

    /// A multi-context `set:` whose `clear_scopes` sits on a later target
    /// clears *before* the trigger token, not after it.
    #[test]
    fn a_multi_context_set_clears_before_the_trigger_when_it_can() {
        let set = set_of(&[
            "%YAML 1.2
---
name: EarlySetClear
scope: source.earlysetclear
version: 2
file_extensions: [esc]
contexts:
  main:
    - match: a
      scope: keyword.start
      push: trigger
  trigger:
    - match: '<'
      scope: punctuation.begin
      set: [outer, ranged, inner]
  outer:
    - meta_scope: meta.glob string.unquoted
    - match: b
      scope: keyword.tail
    - match: $
      pop: 1
  ranged:
    - clear_scopes: 1
    - meta_content_scope: meta.range
    - match: '>'
      scope: punctuation.end
      pop: 1
  inner:
    - match: '[0-9]+'
      scope: constant.numeric
      pop: 1
",
        ]);
        let at = stack_at(&set, "EarlySetClear", "a<5>\n", "punctuation.begin");
        assert!(
            !at.contains("string.unquoted"),
            "the second target's `clear_scopes: 1` must eat the first target's \
             `string.unquoted` before the trigger token: {at}"
        );
        assert!(
            at.contains("meta.glob punctuation.begin"),
            "and only that one atom — the rest of the first target's meta_scope \
             is what the trigger still sits under: {at}"
        );
        // And the clear stack balances: what was cleared comes back exactly
        // once, when the context carrying the clear pops.
        let after = stack_at(&set, "EarlySetClear", "a<5>b\n", "keyword.tail");
        assert_eq!(
            after, "source.earlysetclear meta.glob string.unquoted keyword.tail",
            "the Restore puts the cleared atom back where it was, once"
        );
    }

    /// And a `clear_scopes` that is for a `meta_content_scope` still waits.
    #[test]
    fn a_set_whose_clear_is_for_a_meta_content_scope_still_waits() {
        let set = set_of(&[
            "%YAML 1.2
---
name: LateSetClear
scope: source.latesetclear
version: 2
file_extensions: [lsc]
contexts:
  main:
    - meta_scope: meta.outer
    - match: '='
      scope: keyword.assign
      push: trigger
  trigger:
    - match: ':'
      scope: keyword.colon
      set: [value, eat-space]
  value:
    - meta_content_scope: meta.string
    - match: $
      pop: 1
  eat-space:
    - clear_scopes: 1
    - match: '[ ]*'
      pop: 1
",
        ]);
        let at = stack_at(&set, "LateSetClear", "=: hi\n", "keyword.colon");
        assert!(
            at.contains("meta.outer"),
            "the deferred Clear is for `value`'s meta_content_scope, never for \
             the enclosing meta_scope the trigger sits under: {at}"
        );
    }

    /// A `set` whose target clears scopes clears them — the third of the six
    /// things version 2 corrects.
    #[test]
    fn version_two_clears_scopes_when_a_set_targets_a_context_that_asks() {
        let (one, two) = both_readings(
            "name: Setter
scope: source.lang
file_extensions: [setter]
contexts:
  main:
    - match: \\bdef\\b
      scope: keyword
      push:
        - function
        - function-name
  function:
    - meta_scope: meta.function
  function-name:
    - match: '[a-zA-Z0-9_]+'
      scope: variable.function
    - match: \\(
      scope: punctuation.section.group.begin
      set: function-params
  function-params:
    - meta_scope: meta.function.params
    - clear_scopes: 1
    - match: \\)
      scope: punctuation.section.group.end
      pop: 2
",
        );
        let at = |set: &SyntaxSet| {
            stack_at(set, "Setter", "def abc()\n", "punctuation.section.group.begin")
        };
        assert!(
            at(&one).contains("meta.function "),
            "version 1 is meant to keep the scope the clear should have taken: {}",
            at(&one)
        );
        assert!(
            !at(&two).contains("meta.function "),
            "the target's clear_scopes was not applied: {}",
            at(&two)
        );
        assert!(
            at(&two).contains("meta.function.params punctuation.section.group.begin"),
            "and what the target does add has to still be there: {}",
            at(&two)
        );
    }

    /// A `set` does not give the matched text the `meta_content_scope` of the
    /// context it is leaving — the second of the six.
    #[test]
    fn version_two_does_not_give_a_set_the_scope_of_the_context_it_leaves() {
        let (one, two) = both_readings(
            "name: Setter
scope: source.lang
file_extensions: [setter]
contexts:
  main:
    - match: '(?=[a-zA-Z0-9_])'
      push: function
  function:
    - meta_content_scope: meta.function
    - match: '[a-zA-Z0-9_]+'
      scope: variable.function
    - match: \\(
      scope: punctuation.section.group.begin
      set: function-params
  function-params:
    - meta_scope: meta.function.params
    - match: \\)
      scope: punctuation.section.group.end
      pop: true
",
        );
        let at = |set: &SyntaxSet| {
            stack_at(set, "Setter", "abc()\n", "punctuation.section.group.begin")
        };
        assert!(
            at(&one).contains("meta.function meta.function.params"),
            "version 1 is meant to carry the scope of the context being left: {}",
            at(&one)
        );
        assert_eq!(
            at(&two),
            "source.lang meta.function.params punctuation.section.group.begin",
            "the scope of the context the set left is still on the matched text"
        );
    }

    /// Pushing two contexts that both clear scopes clears their sum at version
    /// 1 and once each at version 2 — the fifth of the six.
    #[test]
    fn a_version_1_push_of_several_contexts_clears_their_sum() {
        let (one, two) = both_readings(
            "name: Clearer
scope: source.lang
file_extensions: [clearer]
contexts:
  main:
    - meta_content_scope: meta.main
    - match: '[a-zA-Z0-9]+\\b'
      scope: identifier
      push:
        - context2
        - context3
  context2:
    - meta_scope: meta.ctx2
    - clear_scopes: 1
  context3:
    - meta_scope: meta.ctx3
    - clear_scopes: 1
    - match: \\n
      pop: true
",
        );
        let at = |set: &SyntaxSet| columns(set, "Clearer", "abc 1\n");
        assert_eq!(
            at(&one)[0],
            "meta.ctx2 meta.ctx3 identifier",
            "the two clears were not added up and applied before either meta scope"
        );
        assert_eq!(
            at(&one)[3],
            "source.lang meta.ctx3",
            "the summed clear outlived the token it belongs to"
        );
        assert_eq!(
            at(&two)[0],
            "source.lang meta.ctx3 identifier",
            "version 2 has to come out the way Sublime says"
        );
        assert_eq!(at(&two)[3], "source.lang meta.ctx3");
    }

    /// A version 1 capture that goes backwards gets no scope — the sixth of
    /// the six.
    #[test]
    fn a_version_1_capture_that_goes_backwards_gets_no_scope() {
        let (one, two) = both_readings(
            "name: Captures
scope: source.lang
file_extensions: [captures]
contexts:
  main:
    - match: '(?:(x)|(y))+'
      captures:
        1: identifier.x
        2: identifier.y
",
        );
        // Every stack the line passes through, so "nobody was given this" is an
        // answer rather than a panic.
        let scoped = |set: &SyntaxSet, wanted: &str| {
            scopes(set, "Captures", "yx\n")
                .iter()
                .any(|stack| stack.split(' ').any(|scope| scope == wanted))
        };
        assert!(
            scoped(&one, "identifier.x"),
            "version 1: the group that moved forward lost its scope"
        );
        assert!(
            !scoped(&one, "identifier.y"),
            "version 1: the group that begins behind the cursor was still scoped"
        );
        assert!(
            scoped(&two, "identifier.x") && scoped(&two, "identifier.y"),
            "version 2 applies both of them"
        );
    }

    /// The text an embed's `escape` matches is outside the context that holds
    /// it at version 1, and inside it at version 2 — the fourth of the six.
    #[test]
    fn a_version_1_embeds_escape_is_outside_the_context_that_holds_it() {
        let (one, two) = both_readings(
            "name: Escaper
scope: source.lang
file_extensions: [escaper]
contexts:
  main:
    - match: '(?=.)'
      push: context1
  context1:
    - meta_scope: meta.group
    - meta_content_scope: meta.content
    - match: \\'
      scope: punctuation.begin
      embed: embed
      escape: \\'
      escape_captures:
        0: punctuation.end
  embed:
    - match: '[a-z]+'
      scope: word
",
        );
        let at = |set: &SyntaxSet| columns(set, "Escaper", "'abc'\n");
        assert_eq!(
            at(&one)[4],
            "source.lang punctuation.end",
            "version 1: the escape kept the scopes of the context holding the embed"
        );
        assert_eq!(
            at(&one)[5],
            "source.lang meta.group meta.content",
            "the context the embed fired from did not end at its escape"
        );
        assert_eq!(
            at(&two)[4],
            "source.lang meta.group meta.content punctuation.end",
            "version 2: the escape belongs to the context that set the embed up"
        );
        assert_eq!(
            at(&one)[0],
            at(&two)[0],
            "and the opening quote is the same at both"
        );
    }

    /// An `embed_scope` replaces the embedded syntax's own scope rather than
    /// stacking with it — the first of the six.
    #[test]
    fn version_two_lets_an_embed_scope_replace_the_embedded_syntaxs_own() {
        let other = "%YAML 1.2\n---\nname: Other\nscope: source.other\nfile_extensions: [other]\ncontexts:\n  main:\n    - match: '[a-zA-Z0-9_]+'\n      scope: identifier\n";
        let host = |version: &str| {
            format!(
                "%YAML 1.2\n---\n{version}name: Host
scope: source.lang
file_extensions: [host]
contexts:
  main:
    - match: \\(
      scope: punctuation.section.group.begin
      embed: 'scope:source.other'
      embed_scope: source.other.embedded
      escape: \\)
      escape_captures:
        0: punctuation.section.group.end
"
            )
        };
        let one = set_of(&[&host(""), other]);
        let two = set_of(&[&host("version: 2\n"), other]);

        assert_eq!(
            stack_at(&one, "Host", "(abc)\n", "identifier"),
            "source.lang source.other.embedded source.other identifier",
            "version 1 is meant to stack the embedded syntax's own scope too"
        );
        assert_eq!(
            stack_at(&two, "Host", "(abc)\n", "identifier"),
            "source.lang source.other.embedded identifier",
            "the embedded syntax's own scope is still stacking under version 2"
        );
    }

    const BRANCHY: &str = r#"%YAML 1.2
---
name: Branchy
scope: source.branchy
file_extensions: [branchy]
contexts:
  main:
    - match: '(?=\w)'
      branch_point: choice
      branch: [as_first, as_second]
    - match: '\s+'
      scope: whitespace.branchy
  as_first:
    - match: 'yes'
      scope: first.taken
      pop: true
    - match: '(?=\w)'
      fail: choice
  as_second:
    - match: '\w+'
      scope: second.taken
      pop: true
"#;

    /// The two readings, for the definition above: the shape the edge cases
    /// below are edges of.
    #[test]
    fn a_branch_takes_the_reading_that_does_not_fail() {
        let set = set_of(&[BRANCHY]);
        assert!(
            scopes(&set, "Branchy", "yes\n").iter().any(|s| s.contains("first.taken")),
            "the first reading was not taken for the word it is for"
        );
        assert!(
            scopes(&set, "Branchy", "no\n").iter().any(|s| s.contains("second.taken")),
            "the fail did not reach the second reading"
        );
    }

    /// When every reading has been tried, the line still finishes.
    #[test]
    fn a_branch_whose_readings_run_out_still_finishes_the_line() {
        let set = set_of(&[r#"%YAML 1.2
---
name: Exhausted
scope: source.exhausted
file_extensions: [exhausted]
contexts:
  main:
    - match: '(?=\w)'
      branch_point: choice
      branch: [first, second]
    - match: '\s+'
      scope: whitespace.exhausted
  first:
    - match: '(?=\w)'
      fail: choice
  second:
    - match: '(?=\w)'
      fail: choice
    - match: '\w+'
      scope: second.reached
      pop: true
"#]);
        let seen = scopes(&set, "Exhausted", "word\n");
        assert!(
            !seen.is_empty(),
            "a branch whose readings ran out produced nothing at all"
        );
        assert!(
            seen.iter().all(|s| s.starts_with("source.exhausted")),
            "the stack was left somewhere the syntax does not own: {seen:?}"
        );
    }

    /// And when they run out, the position goes back to the context the
    /// branch was taken from, not to the character after the lookahead.
    #[test]
    fn a_branch_that_runs_out_hands_the_position_back_to_the_context() {
        let set = set_of(&[r#"%YAML 1.2
---
name: Fallthrough
scope: source.fallthrough
file_extensions: [fallthrough]
contexts:
  main:
    - match: '(?=[\w$])'
      branch_point: declaration
      branch: [as-class, as-enum]
    - match: '\$\w+'
      scope: variable.other.fallthrough
    - match: '\w+'
      scope: keyword.other.fallthrough
  as-class:
    - match: 'class\b'
      scope: entity.name.class.fallthrough
      pop: true
    - match: '(?=\S)'
      fail: declaration
  as-enum:
    - match: 'enum\b'
      scope: entity.name.enum.fallthrough
      pop: true
    - match: '(?=\S)'
      fail: declaration
"#]);
        assert!(
            scopes(&set, "Fallthrough", "$name\n")
                .iter()
                .any(|s| s.contains("variable.other.fallthrough")),
            "the rule below the exhausted branch never saw the token it is for"
        );
        // And the branch itself still works when a reading does match, which
        // is what says the skip is for one position and not for the line.
        assert!(
            scopes(&set, "Fallthrough", "class\n")
                .iter()
                .any(|s| s.contains("entity.name.class.fallthrough")),
            "suppressing the exhausted branch suppressed a branch that resolves"
        );
    }

    /// And when they run out on a *later* line, the position goes back the
    /// same way — not one character past the lookahead.
    #[test]
    fn a_branch_that_runs_out_on_a_later_line_hands_the_position_back_too() {
        let set = set_of(&[r#"%YAML 1.2
---
name: LateFallthrough
scope: source.late
file_extensions: [late]
contexts:
  main:
    - match: '(?=\w)'
      branch_point: declaration
      branch: [as-declaration]
    - match: '\w+'
      scope: variable.other.late
  as-declaration:
    - meta_scope: meta.declaration.late
    - match: '\w+'
      scope: entity.name.late
      set: as-declaration-body
  as-declaration-body:
    # The declaration only continues if the next line has something on it.
    - match: '^(?=\s*$)'
      fail: declaration
    - match: '\S+'
      scope: entity.other.late
"#]);
        let final_ops = corrected(&set, "LateFallthrough", "word\n\n");
        let first: Vec<String> = final_ops[0]
            .iter()
            .map(|(at, op)| format!("{at}:{op:?}"))
            .collect();
        assert!(
            first.iter().any(|one| one.starts_with("0:Push(<variable.other.late")),
            "the rule below the exhausted branch did not see the token from its first character: {first:?}"
        );
    }

    /// The final ops of each line, corrections applied — what the file comes
    /// out as, rather than what each line looked like on the way past.
    fn corrected(
        set: &SyntaxSet,
        name: &str,
        text: &str,
    ) -> Vec<Vec<(usize, syntect::parsing::ScopeStackOp)>> {
        let syntax = set
            .find_syntax_by_name(name)
            .unwrap_or_else(|| panic!("no syntax {name}"));
        let mut state = ParseState::new(syntax);
        let mut per_line = Vec::new();
        for line in text.split_inclusive('\n') {
            let parsed = state.parse_line(line, set).expect("the line parses");
            for (at, corrections) in parsed.replayed {
                if let Some(slot) = at.checked_sub(1).and_then(|i| per_line.get_mut(i)) {
                    *slot = corrections;
                }
            }
            per_line.push(parsed.ops);
        }
        per_line
    }

    /// A definition in the shape of Markdown's link reference definition: two
    /// nested continuations, each a branch that runs out on the blank line.
    const LINK_DEF: &str = r#"%YAML 1.2
---
name: LinkDef
scope: source.linkdef
file_extensions: [linkdef]
contexts:
  main:
    - match: '\['
      scope: punctuation.definition.linkdef
      push: [def-meta, def-attr, def-title]
    - match: '\w+'
      scope: meta.paragraph.linkdef
  def-meta:
    - meta_include_prototype: false
    - meta_scope: meta.def.linkdef
    - include: immediately-pop
  def-attr:
    - match: $
      branch_point: attr-continuation
      branch: [attr-continuation, immediately-pop2]
  attr-continuation:
    - meta_include_prototype: false
    - match: '^(?!\s*\{)'
      fail: attr-continuation
    - match: ^
      pop: 1
  def-title:
    - match: '\('
      scope: punctuation.definition.string.begin.linkdef
      set: title-body
    - match: '(?=\S)'
      pop: 1
  title-body:
    - meta_scope: string.quoted.linkdef
    - match: '\)'
      pop: 1
    - match: $\n?
      branch_point: title-continuation
      branch: [title-continuation, title-termination]
  title-continuation:
    - meta_include_prototype: false
    - match: '^\s*$'
      fail: title-continuation
    - match: ^
      pop: 1
  title-termination:
    - meta_include_prototype: false
    - meta_scope: invalid.illegal.linkdef
    - include: immediately-pop2
  immediately-pop:
    - match: ''
      pop: 1
  immediately-pop2:
    - match: ''
      pop: 2
"#;

    /// A blank line that ends a construct is still inside it.
    #[test]
    fn a_blank_line_that_ends_a_construct_is_still_inside_it() {
        let set = set_of(&[LINK_DEF]);
        let per_line = corrected(&set, "LinkDef", "[ (title\nblah\n\ntext\n");
        let mut stack = ScopeStack::new();
        let here = |stack: &ScopeStack| {
            stack
                .as_slice()
                .iter()
                .map(|s| s.build_string())
                .collect::<Vec<_>>()
                .join(" ")
        };
        for line in &per_line[..2] {
            for (_, op) in line {
                stack.apply(op).expect("the op applies");
            }
        }
        // Column 0 of the blank line: everything the parser says happens *at*
        // that character, and no more.
        for (at, op) in &per_line[2] {
            if *at > 0 {
                break;
            }
            stack.apply(op).expect("the op applies");
        }
        assert!(
            here(&stack).contains("meta.def.linkdef"),
            "the definition ended before the blank line it ends on: {}",
            here(&stack)
        );
        // And it does end: the paragraph below is outside it.
        for (_, op) in per_line[2].iter().skip_while(|(at, _)| *at == 0) {
            stack.apply(op).expect("the op applies");
        }
        for (_, op) in &per_line[3] {
            stack.apply(op).expect("the op applies");
            if here(&stack).contains("meta.paragraph.linkdef") {
                assert!(
                    !here(&stack).contains("meta.def.linkdef"),
                    "the definition outlived the blank line: {}",
                    here(&stack)
                );
                return;
            }
        }
        panic!("the paragraph after the definition was never reached");
    }

    /// An empty component of a scope name is not an atom, wherever it sits.
    #[test]
    fn an_empty_component_of_a_scope_name_is_not_an_atom() {
        use syntect::parsing::Scope;
        let plain = Scope::new("keyword.foo.bar").expect("the scope parses");
        for spelling in [
            "keyword..foo.bar",
            ".keyword.foo.bar",
            "keyword.foo.bar..",
            ".keyword..foo..bar.",
        ] {
            assert_eq!(
                Scope::new(spelling).expect("the scope parses"),
                plain,
                "{spelling} is a different scope from keyword.foo.bar"
            );
        }
        assert_ne!(
            Scope::new("keyword..zzz.bar").expect("the scope parses"),
            plain,
            "dropping the empty components dropped the difference too"
        );
    }

    /// A definition in the shape of Java's qualified annotation: a branch whose
    /// second reading fires `pop: 2` and branches again.
    const POP_BRANCH: &str = r#"%YAML 1.2
---
name: PopBranch
scope: source.popbranch
file_extensions: [popbranch]
contexts:
  main:
    - match: '@'
      scope: punctuation.definition.annotation.popbranch
      push: qualified
    - match: '\w+'
      scope: entity.other.popbranch
  qualified:
    - meta_scope: meta.annotation.popbranch meta.path.popbranch
    - match: ''
      branch_point: qualified
      branch: [qualified-path, qualified-name]
  qualified-path:
    - match: '\w+'
      scope: variable.namespace.popbranch
      set: qualified-accessor
    - match: ''
      pop: 2
  qualified-accessor:
    - match: '\.'
      scope: punctuation.accessor.dot.popbranch
      pop: 1
    - match: '(?=\S)'
      fail: qualified
  qualified-name:
    - match: '\w+'
      scope: meta.annotation.popbranch meta.path.popbranch variable.annotation.popbranch
      pop: 2
      branch_point: parameters
      branch: [parameters, immediately-pop]
  parameters:
    - meta_content_scope: meta.annotation.popbranch
    - match: '\('
      scope: punctuation.section.group.begin.popbranch
      set: parameters-body
    - match: '(?=\S)'
      fail: parameters
  parameters-body:
    - match: '\)'
      scope: punctuation.section.group.end.popbranch
      pop: 1
  immediately-pop:
    - match: ''
      pop: 1
"#;

    /// A `pop: N + branch:` takes the frames it names off, and takes them off
    /// again when a `fail` swaps the alternative.
    #[test]
    fn a_pop_n_branch_unwinds_what_it_names_on_the_retry_as_well() {
        let set = set_of(&[POP_BRANCH]);
        // `b` is the last segment of the annotation's path, so the annotation
        // and the path both end at it: what follows is the parent's again.
        assert_eq!(
            stack_at(&set, "PopBranch", "@b tail\n", "variable.annotation.popbranch"),
            "source.popbranch meta.annotation.popbranch meta.path.popbranch variable.annotation.popbranch",
            "the trigger of the `pop: 2` saw the frames it was unwinding"
        );
        assert_eq!(
            stack_at(&set, "PopBranch", "@b tail\n", "entity.other.popbranch"),
            "source.popbranch entity.other.popbranch",
            "the annotation outlived the rule that popped it"
        );
        // The same through the accessor's `fail`, which is the path the real
        // definition takes: `@a . b` reaches `qualified-name` only on the retry.
        assert_eq!(
            stack_at(&set, "PopBranch", "@a . b tail\n", "entity.other.popbranch"),
            "source.popbranch entity.other.popbranch",
            "the rewind re-emitted the trigger without the unwinding that goes with it"
        );
        assert_eq!(
            stack_at(&set, "PopBranch", "@b() tail\n", "punctuation.section.group.begin.popbranch"),
            "source.popbranch meta.annotation.popbranch punctuation.section.group.begin.popbranch",
            "the `pop: 2` stopped working where it always worked"
        );
    }

    /// An embedded syntax keeps the scope the host gave it, and the host
    /// keeps its own after the escape.
    #[test]
    fn an_embedded_syntax_keeps_its_scope_and_gives_the_host_its_own_back() {
        let set = set_of(&[
            r#"%YAML 1.2
---
name: Guest
scope: source.guest
version: 2
file_extensions: [guest]
contexts:
  main:
    - meta_content_scope: source.guest
    - match: 'a'
      scope: keyword.a.guest
      set: after-a
  after-a:
    - match: 'b'
      scope: keyword.b.guest
"#,
            r#"%YAML 1.2
---
name: Host
scope: text.host
version: 2
file_extensions: [host]
contexts:
  main:
    - meta_content_scope: text.host
    - match: '<'
      scope: punctuation.begin.host
      embed: scope:source.guest
      embed_scope: meta.embedded.host
      escape: '>'
    - match: 'c'
      scope: keyword.c.host
"#,
        ]);
        let seen = scopes(&set, "Host", "<ab>c\n");
        assert!(
            seen.iter().any(|s| s.contains("keyword.b.guest")
                && s.contains("meta.embedded.host")
                && s.contains("text.host")),
            "the guest lost the scope the host embedded it under: {seen:?}"
        );
        assert!(
            seen.iter()
                .any(|s| s.contains("keyword.c.host") && s.contains("text.host")),
            "the host did not get its own scope back after the escape: {seen:?}"
        );
    }

    /// A branch point created while an earlier one is being replayed belongs
    /// to the line being replayed, not to the line the parser is on.
    #[test]
    fn a_branch_opened_by_a_replay_belongs_to_the_line_being_replayed() {
        let set = set_of(&[r#"%YAML 1.2
---
name: Nested
scope: source.nested
file_extensions: [nested]
contexts:
  main:
    - match: '(?=A)'
      branch_point: outer
      branch: [outer-first, outer-second]
  outer-first:
    - match: 'A'
      scope: first.a
      set: outer-first-body
  outer-first-body:
    - match: 'Z'
      fail: outer
    - match: '\w'
  outer-second:
    - match: 'A'
      scope: second.a
      set: inner-holder
  inner-holder:
    - match: '(?=B)'
      branch_point: inner
      branch: [inner-first, inner-second]
    - match: '\w'
  inner-first:
    - match: 'B'
      scope: inner.first
      set: inner-first-body
  inner-first-body:
    - match: 'Z'
      fail: inner
    - match: '\w'
  inner-second:
    - match: 'B'
      scope: inner.second
      set: inner-second-body
  inner-second-body:
    - match: '\w'
"#]);
        let syntax = set.find_syntax_by_name("Nested").expect("the syntax");
        let mut state = ParseState::new(syntax);
        // The last correction offered for line 1, which is the one a consumer
        // keeps: a later correction for a line replaces an earlier one.
        let mut line_one = None;
        for line in "AB\nZ\n".split_inclusive('\n') {
            let out = state.parse_line(line, &set).expect("the line parses");
            for (which, ops) in out.replayed {
                if which == 1 {
                    line_one = Some(ops);
                }
            }
        }
        let ops = line_one.expect("line 1 was never corrected");
        let mut stack = ScopeStack::new();
        let mut seen = Vec::new();
        for (_, op) in &ops {
            stack.apply(op).expect("the op applies");
            seen.extend(stack.as_slice().iter().map(|s| s.build_string()));
        }
        assert!(
            seen.iter().any(|s| s == "second.a"),
            "the outer branch's second reading never reached line 1: {seen:?}"
        );
        assert!(
            seen.iter().any(|s| s == "inner.second"),
            "the branch the replay opened rewound into the wrong line: {seen:?}"
        );
    }

    /// A `fail` naming a branch point that was never opened has no effect —
    /// and the rule it is on still matches.
    #[test]
    fn a_fail_for_a_branch_point_nobody_opened_has_no_effect() {
        let set = set_of(&[r#"%YAML 1.2
---
name: Groundless
scope: source.groundless
file_extensions: [groundless]
contexts:
  main:
    - match: 'give'
      scope: keyword.give
      fail: never-opened
    - match: 'give'
      scope: fallback.give
    - match: 'up'
      scope: keyword.up
"#]);
        let seen = scopes(&set, "Groundless", "give up\n");
        assert!(
            seen.iter().any(|s| s.contains("keyword.give")),
            "the rule the fail was on did not match: {seen:?}"
        );
        // The whole point of "no effect": the parser does not rewind, does not
        // stall, and reaches the rest of the line.
        assert!(
            seen.iter().any(|s| s.contains("keyword.up")),
            "the line stopped at the fail: {seen:?}"
        );
    }

    #[test]
    fn a_stray_fail_at_a_zero_width_match_does_not_hang() {
        let set = set_of(&[r#"%YAML 1.2
---
name: NoNameTest
scope: source.noname-test
file_extensions: [nonametest]
contexts:
  main:
    - match: '\w+'
      scope: word.noname-test
    - match: '(?=;)'
      fail: nonexistent
"#]);
        let seen = scopes(&set, "NoNameTest", "hello;\n");
        assert!(
            seen.iter().any(|s| s.contains("word.noname-test")),
            "the line was never parsed: {seen:?}"
        );
    }

    /// `embed_scope` replaces the embedded syntax's *scope*, and nothing else
    /// the context it enters happens to carry.
    #[test]
    fn an_embed_scope_replaces_the_guests_scope_and_not_its_context() {
        let set = set_of(&[
            r#"%YAML 1.2
---
name: Guest
scope: source.guest
file_extensions: [guest]
version: 2
contexts:
  main:
    - meta_content_scope: meta.extra.guest
    - match: 'z'
      scope: variable.other.guest
  body:
    - meta_content_scope: meta.body.guest
    - match: 'z'
      scope: variable.other.guest
"#,
            r#"%YAML 1.2
---
name: Host
scope: source.host
file_extensions: [host]
version: 2
contexts:
  main:
    - match: 'a'
      scope: keyword.start.host
      embed: scope:source.guest#body
      embed_scope: meta.embedded.host
      escape: '!'
    - match: 'b'
      scope: keyword.start.host
      embed: scope:source.guest
      embed_scope: meta.embedded.host
      escape: '!'
"#,
        ]);
        // Into a named context of the guest's: it keeps its own scope, and the
        // host's `embed_scope` is simply added.
        assert_eq!(
            stack_at(&set, "Host", "az!\n", "variable.other.guest"),
            "source.host meta.embedded.host meta.body.guest variable.other.guest",
        );
        // Into the guest's `main`: `source.guest` is replaced — and only
        // `source.guest`, so `main`'s own `meta_content_scope` survives.
        assert_eq!(
            stack_at(&set, "Host", "bz!\n", "variable.other.guest"),
            "source.host meta.embedded.host meta.extra.guest variable.other.guest",
        );
    }

    /// The walk that decides which contexts the prototype is kept out of stops
    /// at an `embed:`.
    #[test]
    fn the_prototype_is_kept_out_of_what_it_pushes_and_not_of_what_it_embeds() {
        let set = set_of(&[r#"%YAML 1.2
---
name: Protos
scope: source.protos
file_extensions: [protos]
contexts:
  prototype:
    - include: protorules

  protorules:
    - match: '!'
      scope: keyword.marker
    - match: 'p'
      push: viabody

  viabody:
    - match: 'e'
      embed: shared
      escape: '\#'

  shared:
    - match: 'z'
      scope: variable.shared

  main:
    - match: 'q'
      push: shared
"#]);
        // `shared` is reached from the prototype only through an embed, so it
        // keeps the prototype.
        let embedded = scopes(&set, "Protos", "q!\n");
        assert!(
            embedded.iter().any(|s| s.contains("keyword.marker")),
            "the walk followed an embed and took the prototype off: {embedded:?}"
        );
        let pushed = scopes(&set, "Protos", "p!\n");
        assert!(
            !pushed.iter().any(|s| s.contains("keyword.marker")),
            "the prototype was applied inside what it pushes: {pushed:?}"
        );
    }

    /// A parent that declares something beats one that only inherited it.
    #[test]
    fn a_parent_that_declares_it_beats_a_parent_that_inherited_it() {
        let set = set_of(&[
            r#"%YAML 1.2
---
name: VBase
scope: source.vbase
file_extensions: [vbase]
variables:
  mark: ^
contexts:
  main:
    - match: '{{mark}}(x+)'
      captures:
        1: keyword.marked
"#,
            r#"%YAML 1.2
---
name: VLoose
scope: source.vloose
file_extensions: [vloose]
extends: VBase.sublime-syntax
variables:
  mark: ^\s*
"#,
            r#"%YAML 1.2
---
name: VSibling
scope: source.vsibling
file_extensions: [vsibling]
extends: VBase.sublime-syntax
"#,
            r#"%YAML 1.2
---
name: VChild
scope: source.vchild
file_extensions: [vchild]
extends:
  - VLoose.sublime-syntax
  - VSibling.sublime-syntax
"#,
            r#"%YAML 1.2
---
name: VChildReversed
scope: source.vchildreversed
file_extensions: [vchildreversed]
extends:
  - VSibling.sublime-syntax
  - VLoose.sublime-syntax
"#,
            r#"%YAML 1.2
---
name: CBase
scope: source.cbase
file_extensions: [cbase]
contexts:
  main:
    - match: '(x+)'
      captures:
        1: keyword.base
"#,
            r#"%YAML 1.2
---
name: CLoose
scope: source.cloose
file_extensions: [cloose]
extends: CBase.sublime-syntax
contexts:
  main:
    - match: '(y+)'
      captures:
        1: keyword.loose
"#,
            r#"%YAML 1.2
---
name: CSibling
scope: source.csibling
file_extensions: [csibling]
extends: CBase.sublime-syntax
"#,
            r#"%YAML 1.2
---
name: CChild
scope: source.cchild
file_extensions: [cchild]
extends:
  - CLoose.sublime-syntax
  - CSibling.sublime-syntax
"#,
            r#"%YAML 1.2
---
name: CChildReversed
scope: source.cchildreversed
file_extensions: [cchildreversed]
extends:
  - CSibling.sublime-syntax
  - CLoose.sublime-syntax
"#,
        ]);
        // The variable: the base's `^` would not match an indented `xxx`, and
        // the parent that loosened it is the one that said anything about it.
        for name in ["VChild", "VChildReversed"] {
            let seen = scopes(&set, name, "   xxx\n");
            assert!(
                seen.iter().any(|s| s.contains("keyword.marked")),
                "{name} took the inherited variable over the declared one: {seen:?}"
            );
        }
        // And the context, which is the same question about `main`.
        for name in ["CChild", "CChildReversed"] {
            let seen = scopes(&set, name, "yyy\n");
            assert!(
                seen.iter().any(|s| s.contains("keyword.loose")),
                "{name} took the inherited context over the declared one: {seen:?}"
            );
        }
    }

    /// A `captures:` mapping survives a variable that only exists in the parent.
    #[test]
    fn a_capture_survives_a_variable_that_belongs_to_the_parent() {
        let set = set_of(&[
            r#"%YAML 1.2
---
name: Base
scope: source.base
file_extensions: [base]
variables:
  tail: '(\s*)(-->)'
contexts:
  main:
    - match: '{{tail}}'
      captures:
        1: meta.space.base
        2: keyword.end.base
"#,
            r#"%YAML 1.2
---
name: Guest
scope: source.guest
hidden: true
contexts:
  main:
    - match: '\w+'
      scope: variable.other.guest
"#,
            r#"%YAML 1.2
---
name: Child
scope: source.child
file_extensions: [child]

extends: Base.sublime-syntax

contexts:
  main:
    - match: '<'
      embed: scope:source.guest
      embed_scope: source.guest.embedded
      escape: '{{tail}}'
      escape_captures:
        1: source.guest.embedded
        2: keyword.end.child
    - match: '{{tail}}'
      captures:
        1: meta.space.child
        2: keyword.end.child
"#,
        ]);
        // The parent, which resolves its own variable, was never affected.
        let base = scopes(&set, "Base", "a -->\n");
        assert!(
            base.iter().any(|s| s.contains("keyword.end.base")),
            "the parent lost its own captures: {base:?}"
        );
        // The child, whose regex is only a name the parent gives meaning to.
        let child = scopes(&set, "Child", "a -->\n");
        assert!(
            child.iter().any(|s| s.contains("keyword.end.child")),
            "a capture was dropped against the unresolved regex: {child:?}"
        );
        assert!(
            child.iter().any(|s| s.contains("meta.space.child")),
            "the whitespace capture was dropped too: {child:?}"
        );
        // And the same mapping on an `embed`'s escape.
        let escaped = scopes(&set, "Child", "<b -->\n");
        assert!(
            escaped.iter().any(|s| s.contains("variable.other.guest")),
            "the embed did not reach the guest: {escaped:?}"
        );
        assert!(
            escaped.iter().any(|s| s.contains("keyword.end.child")),
            "an escape_capture was dropped against the unresolved regex: {escaped:?}"
        );
    }

    /// An `apply_prototype` include puts the other syntax's prototype *before*
    /// its patterns, not after them.
    #[test]
    fn an_applied_prototype_is_read_before_the_context_it_is_applied_to() {
        let set = set_of(&[
            r#"%YAML 1.2
---
name: Guest
scope: source.guest
file_extensions: [guest]
contexts:
  prototype:
    - match: '\|\s*$'
      scope: punctuation.separator.continuation.guest
  main:
    - match: '\|'
      scope: keyword.operator.bitwise.guest
"#,
            r#"%YAML 1.2
---
name: Host
scope: source.host
file_extensions: [host]
contexts:
  main:
    - include: scope:source.guest
      apply_prototype: true
"#,
        ]);
        // The guest on its own has always read its own prototype first.
        let alone = scopes(&set, "Guest", "a |\n");
        assert!(
            alone
                .iter()
                .any(|s| s.contains("punctuation.separator.continuation.guest")),
            "the guest's own prototype did not win: {alone:?}"
        );
        // And through the host's include it must read the same way.
        let through = scopes(&set, "Host", "a |\n");
        assert!(
            through
                .iter()
                .any(|s| s.contains("punctuation.separator.continuation.guest")),
            "the applied prototype was read after the context: {through:?}"
        );
        assert!(
            !through
                .iter()
                .any(|s| s.contains("keyword.operator.bitwise.guest")),
            "the context's own rule took the match the prototype should have: {through:?}"
        );
    }

    /// `clear_scopes` is applied before the meta scopes, so a `pop: N + set:`
    /// restores before it pops.
    #[test]
    fn a_pop_n_set_restores_cleared_scopes_before_it_pops() {
        let set = set_of(&[r#"%YAML 1.2
---
name: PopSet
scope: source.popset
version: 2
file_extensions: [popset]
contexts:
  main:
    - match: 'c'
      scope: keyword.open
      push: outer
  outer:
    - meta_content_scope: meta.command
    - match: 's'
      push: mid
  mid:
    - meta_content_scope: meta.mid
    - match: 'q'
      push: inner-end
  inner-end:
    - clear_scopes: 1
    - match: '"'
      scope: punctuation.end
      pop: 2
      set: tail
  tail:
    - match: 'T+'
      scope: variable.tail
"#]);
        let seen = scopes(&set, "PopSet", "csq\"TTT\n");
        let tail: Vec<_> = seen.iter().filter(|s| s.contains("variable.tail")).collect();
        assert!(!tail.is_empty(), "the tail was never reached: {seen:?}");
        assert!(
            tail.iter().all(|s| s.contains("meta.command")),
            "the frame below the popped ones lost its meta_content_scope: {tail:?}"
        );
    }

    #[test]
    fn a_pop_n_set_unwinds_a_cleared_frame_without_eating_the_one_below() {
        let set = set_of(&[r#"%YAML 1.2
---
name: ModeSet
scope: source.modeset
version: 2
file_extensions: [modeset]
contexts:
  main:
    - match: 'e'
      push: guest
  guest:
    - meta_content_scope: source.guest
    - match: 'b'
      push: base
  base:
    - meta_scope: meta.mode
    - match: '\('
      scope: punctuation.begin
      push: [body, mid, start]
  start:
    - meta_include_prototype: false
    - match: 'x:'
      scope: storage.modifier
      pop: 3
      set: [body, mid]
    - match: ''
      pop: 1
  body:
    - clear_scopes: 1
    - meta_scope: meta.group meta.mode
    - match: 'z'
      scope: variable.z
  mid:
    - match: '(?=z)'
      pop: 1
"#]);
        let seen = scopes(&set, "ModeSet", "eb(x:z\n");
        let z: Vec<_> = seen.iter().filter(|s| s.contains("variable.z")).collect();
        assert!(!z.is_empty(), "the body was never reached: {seen:?}");
        assert!(
            z.iter().all(|s| s.contains("source.guest")),
            "the frame under the one being replaced was eaten: {z:?}"
        );
        // And the target replaced the frame rather than stacking on it: one
        // `meta.group`, not two.
        assert!(
            z.iter()
                .all(|s| s.matches("meta.group").count() == 1),
            "the target's meta_scope was pushed over the leaving frame's: {z:?}"
        );
    }

    /// The trigger of a `pop: N + set:` whose target clears sees the unwound
    /// stack, not the leaving frame's `meta_scope` under the target's.
    #[test]
    fn a_pop_n_set_trigger_sees_the_frames_its_target_clears_already_unwound() {
        let set = set_of(&[r#"%YAML 1.2
---
name: ModeSetTrigger
scope: source.modeset
version: 2
file_extensions: [modeset]
contexts:
  main:
    - match: 'e'
      push: guest
  guest:
    - meta_content_scope: source.guest
    - match: 'b'
      push: base
  base:
    - meta_scope: meta.mode
    - match: '\('
      scope: punctuation.begin
      push: [body, mid, start]
  start:
    - meta_include_prototype: false
    - match: 'x:'
      scope: storage.modifier
      pop: 3
      set: [body, mid]
    - match: ''
      pop: 1
  body:
    - clear_scopes: 1
    - meta_scope: meta.group meta.mode
    - match: 'z'
      scope: variable.z
  mid:
    - match: '(?=z)'
      pop: 1
"#]);
        let trigger = stack_at(&set, "ModeSetTrigger", "eb(x:z\n", "storage.modifier");
        assert_eq!(
            trigger, "source.modeset source.guest meta.group meta.mode storage.modifier",
            "the target's meta_scope was pushed over the leaving frame's"
        );
    }

    /// `pop: N` + `push:` is a lookahead; `pop: N` + `set:` stacks.
    #[test]
    fn a_pop_n_push_is_a_lookahead_and_a_pop_n_set_is_not() {
        let set = set_of(&[include_str!(
            "../testdata/sublime_probes/PopPush.sublime-syntax"
        )]);

        let push = scopes(&set, "PopPush", "pmXTTT\n");
        assert!(
            push.iter().any(|s| s == "source.poppush meta.tail.push keyword.trigger.push"),
            "the `pop: 2 + push:` trigger is not a lookahead: {push:?}"
        );
        // And the push happened at all, which is the other half of it.
        assert!(
            push.iter().any(|s| s.contains("variable.tail.push")),
            "the push was dropped, so the tail context was never entered: {push:?}"
        );

        // `embed:` and `branch:` are the same rule — the reference names all
        // three — and ST answers the same way for both.
        for (input, kind) in [("efZVVV\n", "embed"), ("bgWQQQ\n", "branch")] {
            let seen = scopes(&set, "PopPush", input);
            assert!(
                seen.iter()
                    .any(|s| *s == format!("source.poppush meta.tail.{kind} keyword.trigger.{kind}")),
                "the `pop: 2 + {kind}:` trigger is not a lookahead: {seen:?}"
            );
            assert!(
                seen.iter().any(|s| s.contains(&format!("variable.tail.{kind}"))),
                "`pop: 2 + {kind}:` never entered its target: {seen:?}"
            );
        }

        let set_shape = scopes(&set, "PopPush", "snYUUU\n");
        assert!(
            set_shape.iter().any(|s| s
                == "source.poppush meta.outer.set meta.mid.set meta.tail.set keyword.trigger.set"),
            "the `pop: 2 + set:` trigger lost a frame it unwinds: {set_shape:?}"
        );
    }

    /// A cross-line `fail` says *which* line each correction is for.
    #[test]
    fn a_cross_line_fail_names_the_lines_it_corrects() {
        use syntect::parsing::ParseState;
        let set = set_of(&[r#"%YAML 1.2
---
name: Late
scope: source.late
file_extensions: [late]
contexts:
  main:
    - match: 'd'
      scope: keyword.d
      branch_point: bp
      branch:
        - alt-a
        - alt-b
  alt-a:
    - meta_scope: meta.a
    - match: '(?=z)'
      fail: bp
    - match: '\S'
      scope: other.a
  alt-b:
    - meta_scope: meta.b
    - match: '\S'
      scope: other.b
"#]);
        let syntax = set.find_syntax_by_name("Late").expect("late");
        let mut state = ParseState::new(syntax);
        let mut corrected: Vec<(usize, Vec<usize>)> = Vec::new();
        for (n, line) in "d\nq\nz\n".split_inclusive('\n').enumerate() {
            let out = state.parse_line(line, &set).expect("parses");
            if !out.replayed.is_empty() {
                corrected.push((n + 1, out.replayed.iter().map(|(at, _)| *at).collect()));
            }
        }
        assert_eq!(
            corrected,
            vec![(3, vec![1, 2])],
            "the fail on line 3 should correct lines 1 and 2, and name them"
        );
    }

    /// More branch points on one line than the parser will hold.
    #[test]
    fn a_line_may_open_more_branch_points_than_the_parser_keeps() {
        let set = set_of(&[BRANCHY]);
        let words = 40;
        let line = format!("{}\n", vec!["no"; words].join(" "));
        let taken = scopes(&set, "Branchy", &line)
            .iter()
            .filter(|s| s.contains("second.taken"))
            .count();
        assert!(
            taken >= words,
            "only {taken} of {words} words reached the second reading"
        );
    }

    /// A pattern that can be prefiltered is.
    #[test]
    fn a_pattern_that_can_be_prefiltered_is() {
        use syntect::parsing::Regex;
        let filtered = |pattern: &str| Regex::new(pattern.to_string()).has_prefilter();

        for pattern in [
            r"\bfn\b",                         // a keyword
            r"(?:pub|impl|struct)\b",          // an alternation of them
            r"//",                             // punctuation
            r"\bmatch\s+",                     // a literal and then a class
            r"</script>",                      // what ends an embed
        ] {
            assert!(filtered(pattern), "no prefilter for {pattern}");
        }

        for pattern in [
            r"^\s*",     // can match nothing: every prefix is the empty one
            r"\w+",      // no literal anywhere in it
            r".",        // nor here
        ] {
            assert!(!filtered(pattern), "a prefilter was built for {pattern}");
        }

        for pattern in [
            r"(?=</script)",              // what ends an embed, as HTML writes it
            r"(?=\bfn\b)(?<!\.)",         // a lookbehind the parser cannot read
            r"(?=struct\b)\h++",          // `\h` and a possessive, neither of which it reads
            r"(?=[)]end)",                // a bracket inside a class, not the end of the group
        ] {
            assert!(filtered(pattern), "no prefilter for the head of {pattern}");
        }

        for pattern in [
            r"(?!given)\w++",   // negative: says what is *not* there, and rules nothing in
            r"(?=\s*)x++",      // its head can match nothing
            r"a(?=given)b++",   // not at the front, so it is about some other position
        ] {
            assert!(!filtered(pattern), "a prefilter was built for {pattern}");
        }
    }

    /// The escape of an `embed` beats an embedded rule that starts earlier in
    /// the line — which is Sublime's answer, and for a long time was not ours.
    #[test]
    fn the_escape_of_an_embed_beats_an_embedded_rule_that_starts_earlier() {
        let set = set_of(&[
            r#"%YAML 1.2
---
name: Host
scope: source.host
file_extensions: [host]
contexts:
  main:
    - match: /\*\*
      scope: comment.begin
      embed: 'scope:source.doc'
      embed_scope: comment.body
      escape: \*+/
      escape_captures:
        0: comment.end
"#,
            r#"%YAML 1.2
---
name: Doc
scope: source.doc
file_extensions: [doc]
contexts:
  main:
    - match: '^\s*(\*)'
      captures:
        1: leading.star
"#,
        ]);
        let syntax = set.find_syntax_by_name("Host").expect("Host");
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let mut seen = Vec::new();
        for line in ["/**\n", "   */\n"] {
            for (_, op) in state.parse_line(line, &set).expect("parses").ops {
                stack.apply(&op).expect("applies");
                seen.push(
                    stack
                        .as_slice()
                        .iter()
                        .map(|scope| scope.build_string())
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
        }
        assert!(
            seen.iter().any(|s| s.contains("comment.end")),
            "the escape did not end the embed: {seen:?}"
        );
        assert!(
            !seen.iter().any(|s| s.ends_with("leading.star")),
            "an embedded rule starting earlier in the line beat the escape \
             again — this is syntect #222 coming back: {seen:?}"
        );
    }

    /// A leading lookahead that governs only one branch of an alternation is
    /// not a prefilter.
    #[test]
    fn a_lookahead_that_only_governs_one_branch_is_not_a_prefilter() {
        use syntect::parsing::Regex;
        for pattern in [
            r"(?=(?:end|do)\b(?![?!]))|$",                    // Ruby
            r"(?=\b(type|and|let|end|val)\b)|(?=^\s*$)",       // OCaml
            r"(?=\b(type|val|external|class|module|end)\b)|^\s*$", // OCaml
            r"(?={)|$\n?",                                    // Groovy
        ] {
            assert!(
                !Regex::new(pattern.to_string()).has_prefilter(),
                "the second branch of {pattern} was filtered on the first branch's literals"
            );
        }

        // And the rule is about the *top level* only: an alternation inside
        // the lookahead is still something every match has to start with.
        assert!(
            Regex::new(r"(?=\b(fn|impl|struct)\b)\h++".to_string()).has_prefilter(),
            "an alternation inside the lookahead is not a reason to give up"
        );
    }

    #[test]
    fn an_extends_pushes_the_childs_scope_and_not_the_parents() {
        let set = set_of(&[
            r#"%YAML 1.2
---
name: EBase
scope: source.ebase
file_extensions: [eb]

contexts:
  main:
    - match: 'a'
      scope: keyword.a
"#,
            r#"%YAML 1.2
---
name: EChild
scope: source.echild
file_extensions: [ec]

extends: EBase.sublime-syntax

contexts:
  other:
    - match: 'b'
      scope: keyword.b
"#,
        ]);
        for stack in scopes(&set, "EChild", "a\n") {
            assert!(
                !stack.contains("source.ebase"),
                "the parent's file scope was pushed under the child's: {stack}"
            );
        }
        // And the child's own is still there, over the rule it inherited.
        assert!(
            scopes(&set, "EChild", "a\n")
                .iter()
                .any(|stack| stack == "source.echild keyword.a"),
            "the child lost either its scope or the rule it inherited"
        );
    }

    /// The scope stack at every column of one row of a document.
    fn rows_at(set: &SyntaxSet, name: &str, text: &str, row: usize) -> Vec<String> {
        let syntax = set.find_syntax_by_name(name).unwrap_or_else(|| panic!("no syntax {name}"));
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let mut state = ParseState::new(syntax);
        let mut per_line: Vec<Vec<(usize, syntect::parsing::ScopeStackOp)>> = Vec::new();
        for line in &lines {
            let parsed = state.parse_line(line, set).expect("the line parses");
            for (at, corrected) in parsed.replayed {
                if let Some(slot) = at.checked_sub(1).and_then(|i| per_line.get_mut(i)) {
                    *slot = corrected;
                }
            }
            per_line.push(parsed.ops);
        }
        let mut stack = ScopeStack::new();
        let mut out = Vec::new();
        for (n, ops) in per_line.into_iter().enumerate() {
            let body = lines[n].trim_end_matches('\n');
            let mut ops = ops.into_iter().peekable();
            let mut here = Vec::new();
            for column in 0..=body.len() {
                while ops.peek().is_some_and(|(at, _)| *at <= column) {
                    let (_, op) = ops.next().expect("peeked");
                    stack.apply(&op).expect("the op applies");
                }
                here.push(
                    stack.as_slice().iter().map(|s| s.build_string()).collect::<Vec<_>>().join(" "),
                );
            }
            for (_, op) in ops {
                stack.apply(&op).expect("the op applies");
            }
            if n == row {
                out = here;
            }
        }
        out
    }

    /// One definition, wrapped in the header the audit's cases share.
    fn one(body: &str) -> String {
        format!(
            "%YAML 1.2\n---\nname: Doc\nscope: source.lang\n\
             file_extensions: [lang]\nversion: 2\n{body}"
        )
    }

    type Case<'a> = (&'a str, &'a [&'a str], &'a str, &'a str, usize, &'a [(usize, &'a str)]);

    /// Every case, and *all* the disagreements rather than the first.
    #[track_caller]
    fn agrees(cases: &[Case<'_>]) {
        let mut bad: Vec<String> = Vec::new();
        for (rule, defs, name, text, row, want) in cases {
            let set = set_of(defs);
            let cols = rows_at(&set, name, text, *row);
            for (col, expected) in *want {
                let got = cols.get(*col).map(|s| s.as_str()).unwrap_or("<no such column>");
                if got != *expected {
                    bad.push(format!(
                        "  {rule}\n    column {col} of {:?}\n      want {expected:?}\n      got  {got:?}",
                        text.lines().nth(*row).unwrap_or("")
                    ));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "the reference says otherwise, in {} place(s):\n{}",
            bad.len(),
            bad.join("\n")
        );
    }

    /// Which pattern of a context answers, and where an `include` puts one.
    #[test]
    fn the_reference_on_finding_a_match() {
        agrees(&[
            // "When a context has multiple patterns, the leftmost one will be
            //  found."
            ("the leftmost match wins, not the first rule",
             &[&one("contexts:\n  main:\n    - match: 'b'\n      scope: k.b\n    - match: 'a'\n      scope: k.a\n")],
             "Doc", "ab\n", 0, &[(0, "source.lang k.a"), (1, "source.lang k.b")]),
            // "When multiple patterns match at the same position, the first
            //  defined pattern will be selected."
            ("and at the same position the first defined one does",
             &[&one("contexts:\n  main:\n    - match: 'ab'\n      scope: k.long\n    - match: 'a'\n      scope: k.short\n")],
             "Doc", "ab\n", 0, &[(0, "source.lang k.long"), (1, "source.lang k.long")]),
            ("whichever of the two is longer",
             &[&one("contexts:\n  main:\n    - match: 'a'\n      scope: k.short\n    - match: 'ab'\n      scope: k.long\n")],
             "Doc", "ab\n", 0, &[(0, "source.lang k.short"), (1, "source.lang")]),
            ("an include brings the rules and leaves the meta patterns",
             &[&one("contexts:\n  main:\n    - include: other\n  other:\n    - meta_scope: m.other\n    - match: 'a'\n      scope: k.a\n")],
             "Doc", "a\n", 0, &[(0, "source.lang k.a")]),
            ("an include lands where it is written",
             &[&one("contexts:\n  main:\n    - match: 'ab'\n      scope: k.long\n    - include: other\n  other:\n    - match: 'a'\n      scope: k.short\n")],
             "Doc", "ab\n", 0, &[(0, "source.lang k.long")]),
            ("and one written first is first",
             &[&one("contexts:\n  main:\n    - include: other\n    - match: 'ab'\n      scope: k.long\n  other:\n    - match: 'a'\n      scope: k.short\n")],
             "Doc", "ab\n", 0, &[(0, "source.lang k.short")]),
        ]);
    }

    /// What a context puts on the stack, and what a `clear_scopes` takes off.
    #[test]
    fn the_reference_on_meta_scopes_and_clear_scopes() {
        agrees(&[
            ("a meta_scope covers both of its triggers",
             &[&one("contexts:\n  main:\n    - match: '\"'\n      push: string\n  string:\n    - meta_scope: str\n    - match: '\"'\n      pop: true\n")],
             "Doc", "\"a\"\n", 0,
             &[(0, "source.lang str"), (1, "source.lang str"), (2, "source.lang str"), (3, "source.lang")]),
            ("a meta_content_scope covers neither",
             &[&one("contexts:\n  main:\n    - match: '\"'\n      push: string\n  string:\n    - meta_content_scope: str\n    - match: '\"'\n      pop: true\n")],
             "Doc", "\"a\"\n", 0,
             &[(0, "source.lang"), (1, "source.lang str"), (2, "source.lang"), (3, "source.lang")]),
            ("clear_scopes: true takes everything, and before the meta scopes",
             &[&one("contexts:\n  main:\n    - meta_content_scope: m.main\n    - match: 'x'\n      push: inner\n  inner:\n    - clear_scopes: true\n    - meta_scope: m.inner\n    - match: 'y'\n      pop: true\n")],
             "Doc", "xy\n", 0, &[(0, "m.inner"), (1, "m.inner"), (2, "source.lang m.main")]),
            ("and an integer takes that many",
             &[&one("contexts:\n  main:\n    - meta_content_scope: m.main\n    - match: 'x'\n      push: inner\n  inner:\n    - clear_scopes: 1\n    - meta_scope: m.inner\n    - match: 'y'\n      pop: true\n")],
             "Doc", "xy\n", 0, &[(0, "source.lang m.inner"), (1, "source.lang m.inner"), (2, "source.lang m.main")]),
            ("a misplaced meta pattern is a meta pattern all the same",
             &[&one("contexts:\n  main:\n    - match: 'x'\n      push: sub\n  sub:\n    - match: 'a'\n      scope: k.a\n    - meta_scope: m.late\n    - match: 'z'\n      pop: true\n")],
             "Doc", "xa\n", 0, &[(0, "source.lang m.late"), (1, "source.lang m.late k.a")]),
        ]);
    }

    /// The six actions a match pattern can carry, and what `pop:` does to each.
    #[test]
    fn the_reference_on_the_actions() {
        agrees(&[
            // "push: … This may be either a single context name, a list of
            //  context names, or an inline, anonymous context."
            ("an inline anonymous context is a context",
             &[&one("contexts:\n  main:\n    - match: 'a'\n      push:\n        - meta_scope: m.inline\n        - match: 'b'\n          pop: true\n")],
             "Doc", "abc\n", 0, &[(0, "source.lang m.inline"), (1, "source.lang m.inline"), (2, "source.lang")]),
            ("the rightmost context of a push list is on top",
             &[&one("contexts:\n  main:\n    - match: 'a'\n      push: [one, two]\n  one:\n    - meta_scope: m.one\n    - match: 'c'\n      scope: k.one\n  two:\n    - meta_scope: m.two\n    - match: 'b'\n      pop: true\n")],
             "Doc", "abc\n", 0, &[(0, "source.lang m.one m.two"), (2, "source.lang m.one k.one")]),
            // "pop: … An integer greater than zero will pop the corresponding
            //  number of contexts."
            ("pop: 2 pops two of them",
             &[&one("contexts:\n  main:\n    - match: 'a'\n      push: [one, two]\n  one:\n    - meta_scope: m.one\n  two:\n    - meta_scope: m.two\n    - match: 'b'\n      pop: 2\n")],
             "Doc", "abc\n", 0, &[(1, "source.lang m.one m.two"), (2, "source.lang")]),
            // "set: … Any match will receive the meta_scope of the context
            //  being popped and the context being pushed."
            ("a set's trigger wears both contexts' meta scopes",
             &[&one("contexts:\n  main:\n    - match: 'a'\n      push: one\n  one:\n    - meta_scope: m.one\n    - match: 'b'\n      scope: k.trigger\n      set: two\n  two:\n    - meta_scope: m.two\n    - match: 'c'\n      pop: true\n")],
             "Doc", "abc\n", 0, &[(1, "source.lang m.one m.two k.trigger"), (2, "source.lang m.two")]),
            ("a pop: N + push: trigger is a lookahead and wears none of them",
             &[&one("contexts:\n  main:\n    - match: 'a'\n      push: [one, two]\n  one:\n    - meta_scope: m.one\n  two:\n    - meta_scope: m.two\n    - match: 'b'\n      scope: k.trigger\n      pop: 2\n      push: three\n  three:\n    - meta_scope: m.three\n")],
             "Doc", "abc\n", 0, &[(1, "source.lang m.three k.trigger")]),
            ("and a pop: N + set: trigger wears what it is unwinding",
             &[&one("contexts:\n  main:\n    - match: 'a'\n      push: [one, two]\n  one:\n    - meta_scope: m.one\n  two:\n    - meta_scope: m.two\n    - match: 'b'\n      scope: k.trigger\n      pop: 2\n      set: three\n  three:\n    - meta_scope: m.three\n")],
             "Doc", "abc\n", 0, &[(1, "source.lang m.one m.two m.three k.trigger"), (2, "source.lang m.three")]),
            // "embed: … While similar to push, it pops out of any number of
            //  nested contexts as soon as the escape pattern is found."
            ("an escape unwinds however deep the embedded syntax got",
             &[&one("contexts:\n  main:\n    - match: '<'\n      embed: inner\n      escape: '>'\n  inner:\n    - match: 'a'\n      push: deeper\n  deeper:\n    - meta_scope: m.deep\n    - match: 'z'\n      scope: k.z\n")],
             "Doc", "<az>t\n", 0, &[(2, "source.lang m.deep k.z"), (3, "source.lang"), (4, "source.lang")]),
            // "escape: … Any backreferences in this pattern will refer to
            //  capture groups in the match regex."
            ("an escape's backreference is the trigger's capture",
             &[&one("contexts:\n  main:\n    - match: '<(\\w)'\n      embed: inner\n      escape: '\\1>'\n  inner:\n    - meta_scope: m.in\n    - match: '\\w'\n      scope: k.w\n")],
             "Doc", "<qazq>t\n", 0, &[(3, "source.lang m.in k.w"), (5, "source.lang"), (6, "source.lang")]),
            ("embed_scope is the text between, and escape_captures 0 the whole escape",
             &[&one("contexts:\n  main:\n    - match: '<'\n      scope: k.open\n      embed: inner\n      embed_scope: m.embedded\n      escape: '>'\n      escape_captures:\n        0: k.close\n  inner:\n    - match: 'a'\n      scope: k.a\n")],
             "Doc", "<a>t\n", 0,
             &[(0, "source.lang k.open"), (1, "source.lang m.embedded k.a"), (2, "source.lang k.close"), (3, "source.lang")]),
            ("a pop: 1 + embed: leaves the context it fired from, except at its escape",
             &[&one("contexts:\n  main:\n    - match: 'p'\n      push: para\n  para:\n    - meta_scope: m.para\n    - match: '<'\n      scope: k.open\n      pop: 1\n      embed: inner\n      embed_scope: m.embedded\n      escape: '>'\n  inner:\n    - match: 'a'\n      scope: k.a\n")],
             "Doc", "p<a>t\n", 0,
             &[(1, "source.lang k.open"), (2, "source.lang m.embedded k.a"), (3, "source.lang m.para"), (4, "source.lang")]),
            ("a branch takes its first reading",
             &[&one(BRANCHING)], "Doc", "az\n", 0, &[(0, "source.lang k.first"), (1, "source.lang")]),
            ("and the next one when that one fails",
             &[&one(BRANCHING)], "Doc", "ab\n", 0, &[(0, "source.lang k.second"), (1, "source.lang")]),
            ("a fail for a branch point nobody opened is a rule like any other",
             &[&one("contexts:\n  main:\n    - match: 'a'\n      fail: nobody\n    - match: 'b'\n      scope: k.b\n")],
             "Doc", "ab\n", 0, &[(0, "source.lang"), (1, "source.lang k.b")]),
            ("and so is one whose branch point has been popped off",
             &[&one("contexts:\n  main:\n    - match: '(?=a)'\n      branch_point: bp\n      branch: [only, other]\n    - match: 'b'\n      fail: bp\n    - match: 'b'\n      scope: k.b\n  only:\n    - match: 'a'\n      scope: k.a\n      pop: true\n  other:\n    - match: 'a'\n      scope: k.other\n      pop: true\n")],
             "Doc", "ab\n", 0, &[(0, "source.lang k.a"), (1, "source.lang")]),
            // "the corresponding pop pattern in the heredoc context refers to
            //  this captured text with the \1 symbol"
            ("a backreference reaches the push that opened the context",
             &[&one(HEREDOC)], "Doc", "<<<EOT\nbody\nEOT;\nafter\n", 3, &[(0, "source.lang")]),
            ("and does not close on somebody else's",
             &[&one(HEREDOC)], "Doc", "<<<EOT\nEOF;\nafter\n", 2, &[(0, "source.lang m.heredoc")]),
        ]);
    }

    /// A branch whose first reading fails, and a context to fall back to.
    const BRANCHING: &str = "contexts:\n  main:\n    - match: '(?=a)'\n      branch_point: bp\n      branch: [first, second]\n  first:\n    - match: 'a'\n      scope: k.first\n    - match: '(?=z)'\n      pop: true\n    - match: '(?=b)'\n      fail: bp\n  second:\n    - match: 'a'\n      scope: k.second\n    - match: '(?=\\S)'\n      pop: true\n";

    /// The reference's PHP heredoc, whose closing rule is a backreference to
    /// the capture that opened it.
    const HEREDOC: &str = "contexts:\n  main:\n    - match: '<<<(\\w+)'\n      push: heredoc\n  heredoc:\n    - meta_scope: m.heredoc\n    - match: '^\\1;'\n      pop: true\n";

    /// The prototype, `with_prototype`, `apply_prototype` and variables.
    #[test]
    fn the_reference_on_prototypes_and_variables() {
        let host = r#"%YAML 1.2
---
name: Host
scope: source.host
file_extensions: [host]
version: 2
contexts:
  main:
    - include: scope:source.guest
      apply_prototype: true
"#;
        let plain_host = host
            .replace("name: Host", "name: PlainHost")
            .replace("source.host", "source.plainhost")
            .replace("[host]", "[plainhost]")
            .replace("      apply_prototype: true\n", "");
        let guest = r#"%YAML 1.2
---
name: Guest
scope: source.guest
file_extensions: [guest]
version: 2
contexts:
  prototype:
    - match: 'p'
      scope: k.guestproto
  main:
    - match: 'a'
      scope: k.guest
"#;
        agrees(&[
            ("the prototype is in every context",
             &[&one("contexts:\n  prototype:\n    - match: 'p'\n      scope: k.proto\n  main:\n    - match: 'x'\n      push: sub\n  sub:\n    - meta_scope: m.sub\n")],
             "Doc", "xp\n", 0, &[(1, "source.lang m.sub k.proto")]),
            ("except the ones that say otherwise",
             &[&one("contexts:\n  prototype:\n    - match: 'p'\n      scope: k.proto\n  main:\n    - match: 'x'\n      push: sub\n  sub:\n    - meta_include_prototype: false\n    - meta_scope: m.sub\n")],
             "Doc", "xp\n", 0, &[(1, "source.lang m.sub")]),
            ("and it goes at the top, so it wins a tie",
             &[&one("contexts:\n  prototype:\n    - match: 'a'\n      scope: k.proto\n  main:\n    - match: 'a'\n      scope: k.own\n")],
             "Doc", "a\n", 0, &[(0, "source.lang k.proto")]),
            ("with_prototype ignores meta_include_prototype",
             &[&one("contexts:\n  main:\n    - match: 'x'\n      push: sub\n      with_prototype:\n        - match: 'q'\n          scope: k.wproto\n  sub:\n    - meta_include_prototype: false\n    - meta_scope: m.sub\n    - match: 'a'\n      scope: k.a\n")],
             "Doc", "xqa\n", 0, &[(1, "source.lang m.sub k.wproto"), (2, "source.lang m.sub k.a")]),
            // "(and JavaScript, by transitivity)"
            ("and reaches what the pushed context pushes",
             &[&one("contexts:\n  main:\n    - match: 'x'\n      push: sub\n      with_prototype:\n        - match: 'q'\n          scope: k.wproto\n  sub:\n    - match: 'y'\n      push: deeper\n  deeper:\n    - meta_scope: m.deep\n    - match: 'a'\n      scope: k.a\n")],
             "Doc", "xyqa\n", 0, &[(2, "source.lang m.deep k.wproto"), (3, "source.lang m.deep k.a")]),
            // "If the key/value pair apply_prototype: true is added to the
            //  include pattern, … then those patterns will also be included."
            ("apply_prototype brings the other syntax's prototype",
             &[host, guest], "Host", "ap\n", 0,
             &[(0, "source.host k.guest"), (1, "source.host k.guestproto")]),
            ("and without it the prototype stays behind",
             &[&plain_host, guest], "PlainHost", "ap\n", 0,
             &[(0, "source.plainhost k.guest"), (1, "source.plainhost")]),
            ("a variable inside a variable",
             &[&one("variables:\n  ident: '[a-z]+'\n  call: '{{ident}}\\('\ncontexts:\n  main:\n    - match: '{{call}}'\n      scope: k.call\n")],
             "Doc", "ab(\n", 0, &[(0, "source.lang k.call"), (2, "source.lang k.call")]),
            ("a brace that is not a variable stays a brace",
             &[&one("contexts:\n  main:\n    - match: 'a{2}'\n      scope: k.quant\n    - match: '\\{\\{-'\n      scope: k.braces\n")],
             "Doc", "aa{{-\n", 0, &[(0, "source.lang k.quant"), (2, "source.lang k.braces")]),
        ]);
    }

    /// What an `extends` carries down, and what it does not.
    #[test]
    fn the_reference_on_inheritance() {
        let base = r#"%YAML 1.2
---
name: EBase
scope: source.ebase
file_extensions: [ebase]
version: 2
variables:
  ident: '[a-z]+'
contexts:
  main:
    - match: '{{ident}}'
      scope: k.base
"#;
        let child = |name: &str, tail: &str| {
            format!(
                "%YAML 1.2\n---\nname: {name}\nscope: source.{}\nfile_extensions: [{}]\n\
                 version: 2\nextends: EBase.sublime-syntax\n{tail}",
                name.to_lowercase(),
                name.to_lowercase()
            )
        };
        let plain = child("EChild", "");
        let over = child("EOver", "contexts:\n  main:\n    - match: 'a'\n      scope: k.own\n");
        let prepend = child(
            "EPrepend",
            "contexts:\n  main:\n    - meta_prepend: true\n    - match: 'a'\n      scope: k.own\n",
        );
        let append = child(
            "EAppend",
            "contexts:\n  main:\n    - meta_append: true\n    - match: 'a'\n      scope: k.own\n",
        );
        let revar = child("EVar", "variables:\n  ident: '[0-9]+'\n");
        agrees(&[
            // "A syntax using inheritance will inherit the variables and
            //  contexts values from its parent syntax."
            ("a child inherits the contexts and the variables",
             &[base, &plain], "EChild", "ab\n", 0, &[(0, "source.echild k.base")]),
            // "Contexts with the same name will override contexts from the
            //  parent syntax."
            ("a context of the child's own replaces the parent's",
             &[base, &over], "EOver", "ab\n", 0,
             &[(0, "source.eover k.own"), (1, "source.eover")]),
            // "meta_prepend: true — all of the patterns in the extending syntax
            //  will be inserted before those in the parent syntax."
            ("meta_prepend puts the child's rules first",
             &[base, &prepend], "EPrepend", "ab\n", 0,
             &[(0, "source.eprepend k.own"), (1, "source.eprepend k.base")]),
            // "meta_append: true — … inserted after to any existing rules from
            //  a context with the same name in an ancestor syntax definition."
            ("meta_append puts them last, so the parent's answer first",
             &[base, &append], "EAppend", "ab\n", 0,
             &[(0, "source.eappend k.base"), (1, "source.eappend k.base")]),
            ("the child's variable reaches the parent's own rules",
             &[base, &revar], "EVar", "ab12\n", 0,
             &[(0, "source.evar"), (2, "source.evar k.base")]),
        ]);
    }

    /// Every case of the reference audit, against **ST's own columns**.
    #[test]
    fn the_reference_cases_answer_as_sublime_text_does() {
        struct Case {
            key: String,
            rule: String,
            syntax: String,
            files: Vec<String>,
            text: String,
            row: usize,
        }
        struct Answer {
            loaded: bool,
            coloured: bool,
            columns: Vec<String>,
        }
        fn text_of(value: &Value) -> String {
            value.as_str().expect("a string").to_string()
        }
        fn texts_of(value: &Value) -> Vec<String> {
            value
                .as_array()
                .map_or_else(Vec::new, |items| items.iter().map(text_of).collect())
        }

        let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/sublime_probes/reference");
        let read = |name: &str| -> Value {
            serde_json::from_str(&std::fs::read_to_string(here.join(name)).unwrap())
                .expect("the file parses")
        };
        let cases: Vec<Case> = read("cases.json")
            .as_array()
            .expect("a list of cases")
            .iter()
            .map(|case| Case {
                key: text_of(&case["key"]),
                rule: text_of(&case["rule"]),
                syntax: text_of(&case["syntax"]),
                files: texts_of(&case["files"]),
                text: text_of(&case["text"]),
                row: case["row"].as_u64().unwrap_or(0) as usize,
            })
            .collect();
        let answers: std::collections::HashMap<String, Answer> = read("answers.json")
            .as_object()
            .expect("answers by case")
            .iter()
            .map(|(key, answer)| {
                let answer = Answer {
                    loaded: answer["loaded"].as_bool().unwrap_or(false),
                    coloured: answer["coloured"].as_bool().unwrap_or(false),
                    columns: texts_of(&answer["columns"]),
                };
                (key.clone(), answer)
            })
            .collect();
        assert!(cases.len() > 60, "only {} cases", cases.len());

        let mut bad: Vec<String> = Vec::new();
        let mut compared = 0;
        let mut unanswered = 0;
        for case in &cases {
            let answer = answers
                .get(&case.key)
                .unwrap_or_else(|| panic!("no ST answer recorded for {}", case.key));
            let definitions: Vec<String> = case
                .files
                .iter()
                .map(|file| {
                    std::fs::read_to_string(here.join(format!("{file}.sublime-syntax")))
                        .unwrap_or_else(|_| panic!("{file} is in the manifest and not on disk"))
                })
                .collect();
            let borrowed: Vec<&str> = definitions.iter().map(String::as_str).collect();
            let set = set_of(&borrowed);
            let ours = rows_at(&set, &case.syntax, &case.text, case.row);
            if !answer.loaded || !answer.coloured {
                unanswered += 1;
                continue;
            }
            compared += 1;
            if ours.len() != answer.columns.len() {
                bad.push(format!(
                    "  {}: {} columns against ST's {}",
                    case.key,
                    ours.len(),
                    answer.columns.len()
                ));
                continue;
            }
            for (column, (mine, theirs)) in ours.iter().zip(&answer.columns).enumerate() {
                if mine != theirs {
                    bad.push(format!(
                        "  {} column {column}\n    {}\n    ST   {theirs:?}\n    ours {mine:?}",
                        case.key, case.rule
                    ));
                }
            }
        }
        assert!(
            compared > 55,
            "only {compared} cases had an answer to compare ({unanswered} did not)"
        );
        assert!(
            bad.is_empty(),
            "Sublime Text says otherwise, in {} place(s):\n{}",
            bad.len(),
            bad.join("\n")
        );
    }

    /// "the highlighting engine will not rewind more than 128 lines when a fail
    /// occurs" — and it is exactly 128.
    #[test]
    fn the_rewind_bound_is_the_documented_128_lines() {
        let far = one("contexts:\n  main:\n    - match: '(?=a)'\n      branch_point: bp\n      branch: [first, second]\n  first:\n    - meta_scope: m.first\n    - match: 'a'\n      scope: k.first\n    - match: 'q'\n      scope: k.q\n      fail: bp\n  second:\n    - meta_scope: m.second\n    - match: 'a'\n      scope: k.second\n");
        let set = set_of(&[&far]);
        let reading = |gap: usize| {
            let text = format!("a\n{}q\n", "n\n".repeat(gap));
            rows_at(&set, "Doc", &text, 0)[0].clone()
        };
        assert_eq!(reading(0), "source.lang m.second k.second");
        assert_eq!(reading(127), "source.lang m.second k.second", "127 lines is within the bound");
        assert_eq!(reading(128), "source.lang m.first k.first", "128 is past it");
        assert_eq!(reading(400), "source.lang m.first k.first");
    }

    #[test]
    fn a_rule_scope_that_restates_an_unwound_atom_is_not_collapsed() {
        let set = set_of(&[include_str!(
            "../testdata/sublime_probes/DupScope.sublime-syntax"
        )]);
        // The trigger is the third character of each line and the target's first
        // is the fourth, which is where the probe's carets are.
        for (line, trigger, tail) in [
            (
                "amXTTT\n",
                "source.dupscope meta.outer.dup meta.mid.dup meta.outer.dup meta.mid.dup keyword.trigger.a",
                "source.dupscope variable.tail.a",
            ),
            (
                "bnYUUU\n",
                "source.dupscope meta.outer.dup meta.mid.dup meta.tail.dup meta.outer.dup meta.mid.dup keyword.trigger.b",
                "source.dupscope meta.tail.dup variable.tail.b",
            ),
            (
                "dpZWWW\n",
                "source.dupscope meta.outer.dup meta.mid.dup meta.mid.dup keyword.trigger.d",
                "source.dupscope variable.tail.d",
            ),
            (
                "ctWQQQ\n",
                "source.dupscope meta.nest.dup meta.nest.dup meta.nest.dup keyword.trigger.c",
                "source.dupscope meta.nest.dup meta.nest.dup variable.tail.c",
            ),
        ] {
            let seen = columns(&set, "DupScope", line);
            assert_eq!(seen[2], trigger, "the trigger of {line:?}");
            assert_eq!(seen[3], tail, "the target of {line:?}");
        }
    }
}
