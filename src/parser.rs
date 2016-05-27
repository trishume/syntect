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
        let mut match_start = 0;
        let mut res = Vec::new();
        while self.parse_next_token(line, &mut match_start, &mut res) {
        }
        // apply operations to our scope to keep up
        // TODO do we even need to keep a scope stack in the parser state?
        for &(_, ref op) in res.iter() {
            self.scope_stack.apply(op);
        }
        return res;
    }

    fn parse_next_token(&mut self,
                        line: &str,
                        start: &mut usize,
                        ops: &mut Vec<(usize, ScopeStackOp)>)
                        -> bool {
        let cur_match = {
            let cur_level = &self.stack[self.stack.len() - 1];
            let mut min_start = usize::MAX;
            let mut cur_match: Option<RegexMatch> = None;
            for (pat_context_ptr, pat_index) in context_iter(cur_level.context.clone()) {
                let pat_context = pat_context_ptr.borrow();
                let match_pat = pat_context.match_at(pat_index);

                println!("{:?}", match_pat.regex_str);
                let regex = match_pat.regex.as_ref().unwrap(); // TODO handle backrefs
                let mut regions = Region::new();
                // TODO caching
                let matched = regex.search_with_options(line,
                                                        *start,
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
            cur_match
        };

        if let Some(reg_match) = cur_match {
            let (_, match_end) = reg_match.regions.pos(0).unwrap();
            *start = match_end;
            self.exec_pattern(line, reg_match, ops);
            true
        } else {
            false
        }
    }

    fn exec_pattern(&mut self,
                    line: &str,
                    reg_match: RegexMatch,
                    ops: &mut Vec<(usize, ScopeStackOp)>) {
        let (match_start, match_end) = reg_match.regions.pos(0).unwrap();
        let context = reg_match.context.borrow();
        let pat = context.match_at(reg_match.pat_index);
        for s in pat.scope.iter() {
            ops.push((match_start, ScopeStackOp::Push(s.clone())));
        }
        if let Some(ref capture_map) = pat.captures {
            // captures could appear in an arbitrary order, have to produce ops in right order
            // ex: ((bob)|(hi))* could match hibob in wrong order, and outer has to push first
            // we don't have to handle a capture matching multiple times, Sublime doesn't
            let mut map: Vec<((usize, i32), ScopeStackOp)> = Vec::new();
            for (cap_index, scopes) in capture_map.iter() {
                if let Some((cap_start, cap_end)) = reg_match.regions.pos(*cap_index) {
                    for scope in scopes.iter() {
                        map.push(((cap_start, -((cap_end - cap_start) as i32)),
                                  ScopeStackOp::Push(scope.clone())));
                    }
                    map.push(((cap_end, -1000000), ScopeStackOp::Pop(scopes.len())));
                }
            }
            map.sort_by(|a, b| a.0.cmp(&b.0));
            for ((index, _), op) in map.into_iter() {
                ops.push((index, op));
            }
        }
        if !pat.scope.is_empty() {
            ops.push((match_end, ScopeStackOp::Pop(pat.scope.len())));
        }
        // TODO perform operation (with prototype)
        // TODO apply meta scopes
    }
}

#[cfg(test)]
mod tests {
    use package_set::PackageSet;
    use parser::*;
    use scope::*;
    fn debug_print_ops(line: &str,
                       scope_repo: &ScopeRepository,
                       ops: &Vec<(usize, ScopeStackOp)>) {
        println!("{}", line);
        for &(i, ref op) in ops.iter() {
            print!("{: <1$}", "", i);
            match op {
                &ScopeStackOp::Push(s) => {
                    println!("^ +{}", scope_repo.to_string(s));
                }
                &ScopeStackOp::Pop(count) => {
                    println!("^ pop {}", count);
                }
            }
        }
    }

    #[test]
    fn can_parse() {
        let mut ps = PackageSet::load_from_folder("testdata/Packages").unwrap();
        let mut state = {
            let syntax = ps.find_syntax_by_name("Ruby on Rails").unwrap();
            ParseState::new(syntax)
        };

        let line = "module Bob::Wow::Troll::Five; 5; end";
        let ops = state.parse_line(line);
        debug_print_ops(line, &ps.scope_repo, &ops);

        let test_ops = vec![
            (0, ScopeStackOp::Push(ps.scope_repo.build("meta.module.ruby"))),
            (0, ScopeStackOp::Push(ps.scope_repo.build("keyword.control.module.ruby"))),
            (6, ScopeStackOp::Pop(1)),
            (7, ScopeStackOp::Push(ps.scope_repo.build("entity.name.type.module.ruby"))),
            (7, ScopeStackOp::Push(ps.scope_repo.build("entity.other.inherited-class.module.first.ruby"))),
            (10, ScopeStackOp::Push(ps.scope_repo.build("punctuation.separator.inheritance.ruby"))),
            (12, ScopeStackOp::Pop(1)),
            (12, ScopeStackOp::Pop(1)),
        ];
        assert_eq!(&ops[0..test_ops.len()], &test_ops[..]);
        // assert!(false);
    }
}
