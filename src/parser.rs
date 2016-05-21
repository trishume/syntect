use syntax_definition::{ContextPtr};
use scope::{ScopeStack, ScopeStackOp};

#[derive(Clone)]
pub struct ParseState {
  context_stack: Vec<ContextPtr>,
  pub scope_stack: ScopeStack,
  first_line: bool
}

impl ParseState {
  pub fn new() -> ParseState {
    ParseState {
      context_stack: vec![],
      scope_stack: ScopeStack::new(),
      first_line: true,
    }
  }
}

fn parse_line(state: &mut ParseState, line: &str) -> Vec<(usize, ScopeStackOp)> {
  return Vec::new(); // TODO actually parse
}
