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

    fn parse_line(&mut self, line: &str) -> Vec<(usize, ScopeStackOp)> {
        assert!(self.stack.len() > 0,
                "Somehow main context was popped from the stack");
        let cur_level = &self.stack[self.stack.len() - 1];
        let cur_context = cur_level.context.borrow();
        return Vec::new(); // TODO actually parse
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn can_parse() {
        // use syntax_definition::*;
        // use scope::*;
        use package_set::PackageSet;
        use parser::*;
        let mut ps = PackageSet::load_from_folder("testdata/Packages").unwrap();
        let syntax = ps.find_syntax_by_name("Ruby on Rails").unwrap();
        let mut state = ParseState::new(syntax);
        assert_eq!(state.parse_line("puts 'hi'"), vec![]);
    }
}

