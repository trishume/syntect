use syntax_definition::*;
use scope::*;
use onig::{self, Region};
use std::usize;

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

#[derive(Debug)]
struct RegexMatch {
    regions: Region,
    context: ContextPtr,
    pat_index: usize,
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

    pub fn parse_line(&mut self, line: &str) -> Vec<(usize, ScopeStackOp)> {
        assert!(self.stack.len() > 0,
                "Somehow main context was popped from the stack");
        let match_start = 0;
        let mut res = Vec::new();
        while let Some(token) = self.parse_next_token(line, match_start) {
            res.push(token);
        }
        return res;
    }

    fn parse_next_token(&mut self, line: &str, start: usize) -> Option<(usize, ScopeStackOp)> {
        let cur_level = &self.stack[self.stack.len() - 1];
        let mut min_start = usize::MAX;
        let mut cur_match: Option<RegexMatch> = None;
        for (pat_context_ptr, pat_index) in context_iter(cur_level.context.clone()) {
            let pat_context = pat_context_ptr.borrow();
            let match_pat = pat_context.match_at(pat_index);

            println!("{:?}", match_pat.regex_str);
            let regex = match_pat.regex.as_ref().unwrap(); // TODO handle backrefs
            let mut regions = Region::new();
            let matched = regex.search_with_options(line,
                                                    start,
                                                    line.len(),
                                                    onig::SEARCH_OPTION_NONE,
                                                    Some(&mut regions));
            if let Some(match_start) = matched {
                if match_start < min_start {
                    min_start = match_start;
                    cur_match = Some(RegexMatch {
                        regions: regions,
                        context: pat_context_ptr.clone(),
                        pat_index: pat_index,
                    });
                }
            }
        }

        // TODO actually execute any matches and return a token
        None
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
        let ps = PackageSet::load_from_folder("testdata/Packages").unwrap();
        let syntax = ps.find_syntax_by_name("Ruby on Rails").unwrap();
        let mut state = ParseState::new(syntax);
        assert_eq!(state.parse_line("class Bob; 5; end"), vec![]);
        // assert!(false);
    }
}
