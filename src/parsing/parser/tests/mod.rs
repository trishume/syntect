//! Tests for the parser, split by theme. Shared fixtures and helpers
//! live here; the themed submodules pull them in via `use super::*`.

mod basics;
mod branch_cross_line;
mod branch_same_line;
mod embed;
mod meta_ops;
mod regressions;

use super::*;

use crate::parsing::ScopeStackOp::{Pop, Push};

use crate::parsing::{Scope, ScopeStack, SyntaxSet, SyntaxSetBuilder};

use crate::util::debug_print_ops;

use crate::utils::testdata;

const TEST_SYNTAX: &str = include_str!("../../../../testdata/parser_tests.sublime-syntax");

fn expect_scope_stacks(line_without_newline: &str, expect: &[&str], syntax: &str) {
    println!("Parsing with newlines");
    let line_with_newline = format!("{}\n", line_without_newline);
    let syntax_newlines = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
    expect_scope_stacks_with_syntax(&line_with_newline, expect, syntax_newlines);

    println!("Parsing without newlines");
    let syntax_nonewlines = SyntaxDefinition::load_from_str(syntax, false, None).unwrap();
    expect_scope_stacks_with_syntax(line_without_newline, expect, syntax_nonewlines);
}

fn expect_scope_stacks_with_syntax(line: &str, expect: &[&str], syntax: SyntaxDefinition) {
    // check that each expected scope stack appears at least once while parsing the given test line

    let syntax_set = link(syntax);
    let mut state = ParseState::new(&syntax_set.syntaxes()[0]);
    let ops = ops(&mut state, line, &syntax_set);
    expect_scope_stacks_for_ops(ops, expect);
}

fn expect_scope_stacks_for_ops(ops: Vec<(usize, ScopeStackOp)>, expect: &[&str]) {
    let mut criteria_met = Vec::new();
    for stack_str in stack_states(ops) {
        println!("{}", stack_str);
        for expectation in expect.iter() {
            if stack_str.contains(expectation) {
                criteria_met.push(expectation);
            }
        }
    }
    if let Some(missing) = expect.iter().find(|e| !criteria_met.contains(e)) {
        panic!("expected scope stack '{}' missing", missing);
    }
}

fn parse(line: &str, syntax: &str) -> Vec<(usize, ScopeStackOp)> {
    let syntax = SyntaxDefinition::load_from_str(syntax, true, None).unwrap();
    let syntax_set = link(syntax);

    let mut state = ParseState::new(&syntax_set.syntaxes()[0]);
    ops(&mut state, line, &syntax_set)
}

fn link(syntax: SyntaxDefinition) -> SyntaxSet {
    let mut builder = SyntaxSetBuilder::new();
    builder.add(syntax);
    builder.build()
}

fn ops(state: &mut ParseState, line: &str, syntax_set: &SyntaxSet) -> Vec<(usize, ScopeStackOp)> {
    let output = state.parse_line(line, syntax_set).expect("#[cfg(test)]");
    debug_print_ops(line, &output.ops);
    output.ops
}

fn stack_states(ops: Vec<(usize, ScopeStackOp)>) -> Vec<String> {
    let mut states = Vec::new();
    let mut stack = ScopeStack::new();
    for (_, op) in ops.iter() {
        stack.apply(op).expect("#[cfg(test)]");
        let scopes: Vec<String> = stack
            .as_slice()
            .iter()
            .map(|s| format!("{:?}", s))
            .collect();
        let stack_str = scopes.join(", ");
        states.push(stack_str);
    }
    states
}

const BRANCH_SYNTAX: &str = r#"
scope: source.branch-test
contexts:
  main:
    - match: '(?=\S)'
      branch_point: stmt
      branch: [let-stmt, generic-stmt]

  let-stmt:
    - match: 'let'
      scope: keyword.declaration.branch-test
      set: let-assign
    - match: '(?=\S)'
      fail: stmt

  let-assign:
    - match: '='
      scope: keyword.operator.assignment.branch-test
      set: let-value
    - match: '(?=\S)'
      fail: stmt

  let-value:
    - match: '\w+'
      scope: constant.other.branch-test
    - match: ';'
      scope: punctuation.terminator.branch-test
      pop: true

  generic-stmt:
    - match: '[^;]+'
      scope: string.unquoted.branch-test
    - match: ';'
      scope: punctuation.terminator.branch-test
      pop: true
"#;
