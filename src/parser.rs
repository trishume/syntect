use syntax_definition::*;
use scope::*;

#[derive(Debug, Clone)]
pub struct ParseState {
    stack: Vec<StateLevel>,
    pub scope_stack: ScopeStack,
    first_line: bool,
}

#[derive(Debug, Clone)]
struct StateLevel {
    context: ContextPtr,
    prototype: Option<ContextPtr>,
    scopes_pushed: usize,
}

impl ParseState {
    pub fn new(syntax: &SyntaxDefinition) -> ParseState {
        let start_state = StateLevel {
            context: syntax.contexts["main"].clone(),
            prototype: None,
            scopes_pushed: 1,
        };
        let mut scope_stack = ScopeStack::new();
        scope_stack.push(syntax.scope);
        ParseState {
            stack: vec![start_state],
            scope_stack: scope_stack,
            first_line: true,
        }
    }
}

fn parse_line(state: &mut ParseState, line: &str) -> Vec<(usize, ScopeStackOp)> {
    return Vec::new(); // TODO actually parse
}
