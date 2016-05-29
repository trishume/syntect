use syntax_definition::*;
use scope::*;
use onig::{self, Region};
use std::usize;
use std::i32;

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
    captures: Option<(Region, String)>,
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
            captures: None,
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
        // TODO push file syntax on first line
        // TODO set regex parameters correctly for start of file
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
            let context_chain = self.stack
                .iter()
                .filter_map(|lvl| lvl.prototype.as_ref().map(|x| x.clone()))
                .chain(Some(cur_level.context.clone()).into_iter());
            for ctx in context_chain {
                for (pat_context_ptr, pat_index) in context_iter(ctx) {
                    let pat_context = pat_context_ptr.borrow();
                    let match_pat = pat_context.match_at(pat_index);

                    // println!("{:?}", match_pat.regex_str);
                    let refs_regex = if cur_level.captures.is_some() && match_pat.regex.is_none() {
                        let &(ref region, ref s) = cur_level.captures.as_ref().unwrap();
                        Some(match_pat.compile_with_refs(region, s))
                    } else { None };
                    let regex = if let Some(ref rgx) = refs_regex {
                        rgx
                    } else {
                        match_pat.regex.as_ref().unwrap()
                    };
                    let mut regions = Region::new();
                    // TODO caching
                    let matched = regex.search_with_options(line,
                                                            *start,
                                                            line.len(),
                                                            onig::SEARCH_OPTION_NONE,
                                                            Some(&mut regions));
                    if let Some(match_start) = matched {
                        let match_end = regions.pos(0).unwrap().1;
                        // this is necessary to avoid infinite looping on dumb patterns
                        let does_something = match match_pat.operation {
                            MatchOperation::None => match_start != match_end,
                            _ => true,
                        };
                        if match_start < min_start && does_something {
                            min_start = match_start;
                            // TODO pass by immutable ref and re-use context and regions
                            cur_match = Some(RegexMatch {
                                regions: regions,
                                context: pat_context_ptr.clone(),
                                pat_index: pat_index,
                            });
                        }
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
        // println!("running pattern {}", pat.regex_str);

        self.push_meta_ops(true, match_start, &*context, &pat.operation, ops);
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
                    map.push(((cap_end, i32::MIN), ScopeStackOp::Pop(scopes.len())));
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
        self.push_meta_ops(false, match_end, &*context, &pat.operation, ops);

        self.perform_op(line, &reg_match.regions, pat);
    }

    fn push_meta_ops(&self,
                     initial: bool,
                     index: usize,
                     cur_context: &Context,
                     match_op: &MatchOperation,
                     ops: &mut Vec<(usize, ScopeStackOp)>) {
        match match_op {
            &MatchOperation::Push(ref context_refs) |
            &MatchOperation::Set(ref context_refs) => {
                for r in context_refs {
                    let ctx_ptr = r.resolve();
                    let ctx = ctx_ptr.borrow();
                    let v = if initial {
                        &ctx.meta_scope
                    } else {
                        &ctx.meta_content_scope
                    };
                    for scope in v.iter() {
                        ops.push((index, ScopeStackOp::Push(scope.clone())));
                    }
                }
            }
            &MatchOperation::Pop => {
                let v = if initial {
                    &cur_context.meta_content_scope
                } else {
                    &cur_context.meta_scope
                };
                if !v.is_empty() {
                    ops.push((index, ScopeStackOp::Pop(v.len())));
                }
            }
            &MatchOperation::None => (),
        }
    }

    fn perform_op(&mut self, line: &str, regions: &Region, pat: &MatchPattern) {
        let ctx_refs = match pat.operation {
            MatchOperation::Push(ref ctx_refs) => ctx_refs,
            MatchOperation::Set(ref ctx_refs) => {
                self.stack.pop();
                ctx_refs
            }
            MatchOperation::Pop => {
                self.stack.pop();
                return;
            }
            MatchOperation::None => return,
        };
        for (i, r) in ctx_refs.iter().enumerate() {
            let proto = if i == 0 {
                pat.with_prototype.clone()
            } else {
                None
            };
            let ctx_ptr = r.resolve();
            let captures = {
                let ctx = ctx_ptr.borrow();
                if ctx.uses_backrefs {
                    // TODO maybe move the Region instead of cloning
                    Some((regions.clone(), line.to_owned()))
                } else {
                    None
                }
            };
            self.stack.push(StateLevel {
                context: ctx_ptr,
                prototype: proto,
                captures: captures,
            });
        }
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
        for &(i, ref op) in ops.iter() {
            println!("{}", line);
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
        use scope::ScopeStackOp::{Push, Pop};
        let mut ps = PackageSet::load_from_folder("testdata/Packages").unwrap();
        let mut state = {
            let syntax = ps.find_syntax_by_name("Ruby on Rails").unwrap();
            ParseState::new(syntax)
        };
        let mut state2 = {
            let syntax = ps.find_syntax_by_name("HTML (Rails)").unwrap();
            ParseState::new(syntax)
        };

        let line = "module Bob::Wow::Troll::Five; 5; end";
        let ops = state.parse_line(line);
        debug_print_ops(line, &ps.scope_repo, &ops);

        let test_ops = vec![
            (0, Push(ps.scope_repo.build("meta.module.ruby"))),
            (0, Push(ps.scope_repo.build("keyword.control.module.ruby"))),
            (6, Pop(1)),
            (7, Push(ps.scope_repo.build("entity.name.type.module.ruby"))),
            (7, Push(ps.scope_repo.build("entity.other.inherited-class.module.first.ruby"))),
            (10, Push(ps.scope_repo.build("punctuation.separator.inheritance.ruby"))),
            (12, Pop(1)),
            (12, Pop(1)),
        ];
        assert_eq!(&ops[0..test_ops.len()], &test_ops[..]);

        let line2 = "def lol(wow = 5)";
        let ops2 = state.parse_line(line2);
        debug_print_ops(line2, &ps.scope_repo, &ops2);
        let test_ops2 =
            vec![(0, Push(ps.scope_repo.build("meta.function.method.with-arguments.ruby"))),
                 (0, Push(ps.scope_repo.build("keyword.control.def.ruby"))),
                 (3, Pop(1)),
                 (4, Push(ps.scope_repo.build("entity.name.function.ruby"))),
                 (7, Pop(1)),
                 (7, Push(ps.scope_repo.build("punctuation.definition.parameters.ruby"))),
                 (8, Pop(1)),
                 (8, Push(ps.scope_repo.build("variable.parameter.function.ruby"))),
                 (12, Push(ps.scope_repo.build("keyword.operator.assignment.ruby"))),
                 (13, Pop(1)),
                 (14, Push(ps.scope_repo.build("constant.numeric.ruby"))),
                 (15, Pop(1)),
                 (15, Pop(1)),
                 (15, Push(ps.scope_repo.build("punctuation.definition.parameters.ruby"))),
                 (16, Pop(1)),
                 (16, Pop(1))];
        assert_eq!(ops2, test_ops2);

        let line3 = "<script>var lol = '<% def wow(";
        let ops3 = state2.parse_line(line3);
        debug_print_ops(line3, &ps.scope_repo, &ops3);
        let mut test_stack = ScopeStack::new();
        test_stack.push(ps.scope_repo.build("text.html.ruby"));
        test_stack.push(ps.scope_repo.build("text.html.basic"));
        test_stack.push(ps.scope_repo.build("source.js.embedded.html"));
        test_stack.push(ps.scope_repo.build("string.quoted.single.js"));
        test_stack.push(ps.scope_repo.build("source.ruby.rails.embedded.html"));
        test_stack.push(ps.scope_repo.build("meta.function.method.with-arguments.ruby"));
        test_stack.push(ps.scope_repo.build("variable.parameter.function.ruby"));
        state2.scope_stack.debug_print(&ps.scope_repo);
        test_stack.debug_print(&ps.scope_repo);
        assert_eq!(state2.scope_stack, test_stack);

        // for testing backrefs
        let line4 = "lol = <<-END wow END";
        let ops4 = state.parse_line(line4);
        debug_print_ops(line4, &ps.scope_repo, &ops4);
        let test_ops4 = vec![
            (4, Push(ps.scope_repo.build("keyword.operator.assignment.ruby"))),
            (5, Pop(1)),
            (6, Push(ps.scope_repo.build("string.unquoted.heredoc.ruby"))),
            (6, Push(ps.scope_repo.build("punctuation.definition.string.begin.ruby"))),
            (12, Pop(1)),
            (16, Push(ps.scope_repo.build("punctuation.definition.string.end.ruby"))),
            (20, Pop(1)),
            (20, Pop(1)),
        ];
        assert_eq!(ops4, test_ops4);

        // assert!(false);
    }
}
