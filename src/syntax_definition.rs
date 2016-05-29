use std::collections::HashMap;
use onig::{self, Regex, Region, Syntax};
use std::rc::{Rc, Weak};
use std::cell::RefCell;
use scope::*;
use regex_syntax::quote;

pub type CaptureMapping = HashMap<usize, Vec<Scope>>;
pub type ContextPtr = Rc<RefCell<Context>>;

#[derive(Debug)]
pub struct SyntaxDefinition {
    pub name: String,
    pub file_extensions: Vec<String>,
    pub scope: Scope,
    pub first_line_match: Option<Regex>,
    pub hidden: bool,

    pub variables: HashMap<String, String>,
    pub contexts: HashMap<String, ContextPtr>,
}

#[derive(Debug)]
pub struct Context {
    pub meta_scope: Vec<Scope>,
    pub meta_content_scope: Vec<Scope>,
    pub meta_include_prototype: bool,
    pub uses_backrefs: bool,

    pub patterns: Vec<Pattern>,
}

#[derive(Debug)]
pub enum Pattern {
    Match(MatchPattern),
    Include(ContextReference),
}

#[derive(Debug)]
pub struct MatchIter {
    ctx_stack: Vec<ContextPtr>,
    index_stack: Vec<usize>,
}

#[derive(Debug)]
pub struct MatchPattern {
    pub regex_str: String,
    // present unless contains backrefs and has to be dynamically compiled
    pub regex: Option<Regex>,
    pub scope: Vec<Scope>,
    pub captures: Option<CaptureMapping>,
    pub operation: MatchOperation,
    pub with_prototype: Option<ContextPtr>,
}

#[derive(Debug)]
pub enum ContextReference {
    Named(String),
    ByScope {
        scope: Scope,
        sub_context: Option<String>,
    },
    File {
        name: String,
        sub_context: Option<String>,
    },
    Inline(ContextPtr),
    Direct(Weak<RefCell<Context>>),
}

#[derive(Debug)]
pub enum MatchOperation {
    Push(Vec<ContextReference>),
    Set(Vec<ContextReference>),
    Pop,
    None,
}

impl Iterator for MatchIter {
    type Item = (ContextPtr, usize);

    fn next(&mut self) -> Option<(ContextPtr, usize)> {
        loop {
            if self.ctx_stack.is_empty() {
                return None;
            }
            let last_index = self.ctx_stack.len() - 1;
            let context_ref = self.ctx_stack[last_index].clone();
            let context = context_ref.borrow();
            let index = self.index_stack[last_index];
            self.index_stack[last_index] = index + 1;
            if index < context.patterns.len() {
                match context.patterns[index] {
                    Pattern::Match(_) => return Some((context_ref.clone(), index)),
                    Pattern::Include(ref ctx_ref) => {
                        let ctx_ptr = match ctx_ref {
                            &ContextReference::Inline(ref ctx_ptr) => ctx_ptr.clone(),
                            &ContextReference::Direct(ref ctx_ptr) => ctx_ptr.upgrade().unwrap(),
                            _ => panic!("Can only iterate patterns after linking: {:?}", ctx_ref),
                        };
                        self.ctx_stack.push(ctx_ptr);
                        self.index_stack.push(0);
                    }
                }
            } else {
                self.ctx_stack.pop();
                self.index_stack.pop();
            }
        }
    }
}

/// Returns an iterator over all the match patterns in this context.
/// It recursively follows include directives. Can only be run on
/// contexts that have already been linked up.
pub fn context_iter(ctx: ContextPtr) -> MatchIter {
    MatchIter {
        ctx_stack: vec![ctx],
        index_stack: vec![0],
    }
}

impl Context {
    pub fn match_at(&self, index: usize) -> &MatchPattern {
        match self.patterns[index] {
            Pattern::Match(ref match_pat) => match_pat,
            _ => panic!("bad index to match_at"),
        }
    }
}

impl ContextReference {
    /// find the pointed to context, panics if ref is not linked
    pub fn resolve(&self) -> ContextPtr {
        match self {
            &ContextReference::Inline(ref ptr) => ptr.clone(),
            &ContextReference::Direct(ref ptr) => ptr.upgrade().unwrap(),
            _ => panic!("Can only call resolve on linked references: {:?}", self),
        }
    }
}

impl MatchPattern {
    /// substitutes back-refs in Regex with regions from s
    /// used for match patterns which refer to captures from the pattern
    /// that pushed them.
    pub fn regex_with_substitutes(&self, region: &Region, s: &str) -> String {
        let mut reg_str = String::new();

        let mut last_was_escape = false;
        for c in self.regex_str.chars() {
            if last_was_escape && c.is_digit(10) {
                let val = c.to_digit(10).unwrap();
                if let Some((start, end)) = region.pos(val as usize) {
                    let escaped = quote(&s[start..end]);
                    reg_str.push_str(&escaped);
                }
            } else if last_was_escape {
                reg_str.push('\\');
                reg_str.push(c);
            } else if c != '\\' {
                reg_str.push(c);
            }

            last_was_escape = c == '\\' && !last_was_escape;
        }
        reg_str
    }

    pub fn compile_with_refs(&self, region: &Region, s: &str) -> Regex {
        // TODO don't panic on invalid regex
        Regex::with_options(&self.regex_with_substitutes(region, s),
            onig::REGEX_OPTION_CAPTURE_GROUP,
            Syntax::default()).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use syntax_definition::*;
    #[test]
    fn can_compile_refs() {
        use onig::{self, Regex, Region};
        let pat = MatchPattern {
            regex_str: String::from(r"lol \\ \2 \1 '\9' \wz"),
            regex: None,
            scope: vec![],
            captures: None,
            operation: MatchOperation::None,
            with_prototype: None,
        };
        let r = Regex::new(r"(\\\[\]\(\))(b)(c)(d)(e)").unwrap();
        let mut region = Region::new();
        let s = r"\[]()bcde";
        assert!(r.match_with_options(s, 0, onig::SEARCH_OPTION_NONE, Some(&mut region)).is_some());

        let regex_res = pat.regex_with_substitutes(&region, s);
        assert_eq!(regex_res, r"lol \\ b \\\[\]\(\) '' \wz");
        pat.compile_with_refs(&region, s);
    }
}
