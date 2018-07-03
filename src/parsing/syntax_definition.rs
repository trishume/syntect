//! This module contains data structures for representing syntax definitions.
//! Everything is public because I want this library to be useful in super
//! integrated cases like text editors and I have no idea what kind of monkeying
//! you might want to do with the data. Perhaps parsing your own syntax format
//! into this data structure?
use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use lazycell::AtomicLazyCell;
use onig::{Regex, RegexOptions, Region, Syntax};
use super::scope::*;
use regex_syntax::escape;
use serde::{Serialize, Serializer};
use parsing::syntax_set::SyntaxSet;

pub type CaptureMapping = Vec<(usize, Vec<Scope>)>;


#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextId {
    syntax_index: usize,
    context_index: usize,
}

/// The main data structure representing a syntax definition loaded from a
/// `.sublime-syntax` file. You'll probably only need these as references
/// to be passed around to parsing code.
///
/// Some useful public fields are the `name` field which is a human readable
/// name to display in syntax lists, and the `hidden` field which means hide
/// this syntax from any lists because it is for internal use.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyntaxDefinition {
    pub name: String,
    pub file_extensions: Vec<String>,
    pub scope: Scope,
    pub first_line_match: Option<String>,
    pub hidden: bool,
    #[serde(serialize_with = "ordered_map")]
    pub variables: HashMap<String, String>,
    pub start_context: usize,
    pub prototype: Option<usize>,
    pub contexts: Vec<Context>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Context {
    pub name: String,
    pub meta_scope: Vec<Scope>,
    pub meta_content_scope: Vec<Scope>,
    /// This being set false in the syntax file implies this field being set false,
    /// but it can also be set falso for contexts that don't include the prototype for other reasons
    pub meta_include_prototype: bool,
    pub clear_scopes: Option<ClearAmount>,
    /// This is filled in by the linker at link time
    /// for contexts that have `meta_include_prototype==true`
    /// and are not included from the prototype.
    pub prototype: Option<ContextId>,
    pub uses_backrefs: bool,

    pub patterns: Vec<Pattern>,
}

impl Context {
    pub fn new(name: &str, meta_include_prototype: bool) -> Context {
        Context {
            name: name.to_string(),
            meta_scope: Vec::new(),
            meta_content_scope: Vec::new(),
            meta_include_prototype: meta_include_prototype,
            clear_scopes: None,
            uses_backrefs: false,
            patterns: Vec::new(),
            prototype: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Pattern {
    Match(MatchPattern),
    Include(ContextReference),
}

/// Used to iterate over all the match patterns in a context.
/// Basically walks the tree of patterns and include directives
/// in the correct order.
#[derive(Debug)]
pub struct MatchIter<'a> {
    syntax_set: &'a SyntaxSet,
    ctx_stack: Vec<&'a Context>,
    index_stack: Vec<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MatchPattern {
    pub has_captures: bool,
    pub regex_str: String,
    pub scope: Vec<Scope>,
    pub captures: Option<CaptureMapping>,
    pub operation: MatchOperation,
    pub with_prototype: Option<ContextReference>,

    #[serde(skip_serializing, skip_deserializing, default = "AtomicLazyCell::new")]
    regex: AtomicLazyCell<Regex>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
//    Inline(Context),
    Inline(String),
    Direct(ContextId),
}


#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MatchOperation {
    Push(Vec<ContextReference>),
    Set(Vec<ContextReference>),
    Pop,
    None,
}

impl<'a> Iterator for MatchIter<'a> {
    type Item = (&'a Context, usize);

    fn next(&mut self) -> Option<(&'a Context, usize)> {
        loop {
            if self.ctx_stack.is_empty() {
                return None;
            }
            // uncomment for debugging infinite recursion
            // println!("{:?}", self.index_stack);
            // use std::thread::sleep_ms;
            // sleep_ms(500);
            let last_index = self.ctx_stack.len() - 1;
            let context = self.ctx_stack[last_index];
            let index = self.index_stack[last_index];
            self.index_stack[last_index] = index + 1;
            if index < context.patterns.len() {
                match context.patterns[index] {
                    Pattern::Match(_) => return Some((context, index)),
                    Pattern::Include(ref ctx_ref) => {
                        let ctx_ptr = match *ctx_ref {
                            // TODO:
//                            ContextReference::Inline(ref context) => context,
                            ContextReference::Direct(ref context_id) => {
                                context_id.resolve(self.syntax_set)
                            }
                            _ => return self.next(), // skip this and move onto the next one
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
pub fn context_iter<'a>(syntax_set: &'a SyntaxSet, context: &'a Context) -> MatchIter<'a> {
    MatchIter {
        syntax_set,
        ctx_stack: vec![context],
        index_stack: vec![0],
    }
}

impl Context {
    /// Returns the match pattern at an index, panics if the thing isn't a match pattern
    pub fn match_at(&self, index: usize) -> &MatchPattern {
        match self.patterns[index] {
            Pattern::Match(ref match_pat) => match_pat,
            _ => panic!("bad index to match_at"),
        }
    }
}

impl ContextReference {
    /// find the pointed to context, panics if ref is not linked
    pub fn resolve<'a>(&self, syntax_set: &'a SyntaxSet) -> &'a Context {
        match *self {
            // TODO?
            // ContextReference::Inline(ref ptr) => ptr,
            ContextReference::Direct(ref context_id) => context_id.resolve(syntax_set),
            _ => panic!("Can only call resolve on linked references: {:?}", self),
        }
    }
}

pub(crate) fn substitute_backrefs_in_regex<F>(regex_str: &str, substituter: F) -> String
    where F: Fn(usize) -> Option<String>
{
    let mut reg_str = String::with_capacity(regex_str.len());

    let mut last_was_escape = false;
    for c in regex_str.chars() {
        if last_was_escape && c.is_digit(10) {
            let val = c.to_digit(10).unwrap() as usize;
            if let Some(sub) = substituter(val) {
                reg_str.push_str(&sub);
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

impl ContextId {
    pub fn new(syntax_index: usize, context_index: usize) -> Self {
        ContextId { syntax_index, context_index }
    }

    // TODO: maybe this should be on SyntaxSet instead?
    pub fn resolve<'a>(&self, syntax_set: &'a SyntaxSet) -> &'a Context {
        let syntax = syntax_set.get_syntax(self.syntax_index);
        &syntax.contexts[self.context_index]
    }
}

impl MatchPattern {

    pub fn new(
        has_captures: bool,
        regex_str: String,
        scope: Vec<Scope>,
        captures: Option<CaptureMapping>,
        operation: MatchOperation,
        with_prototype: Option<ContextReference>,
    ) -> MatchPattern {
        MatchPattern {
            has_captures,
            regex_str,
            scope,
            captures,
            operation,
            with_prototype,
            regex: AtomicLazyCell::new(),
        }
    }

    /// substitutes back-refs in Regex with regions from s
    /// used for match patterns which refer to captures from the pattern
    /// that pushed them.
    pub fn regex_with_substitutes(&self, region: &Region, s: &str) -> String {
        substitute_backrefs_in_regex(&self.regex_str, |i| {
            region.pos(i).map(|(start, end)| escape(&s[start..end]))
        })
    }

    /// Used by the parser to compile a regex which needs to reference
    /// regions from another matched pattern.
    pub fn regex_with_refs(&self, region: &Region, s: &str) -> Regex {
        // TODO don't panic on invalid regex
        Regex::with_options(&self.regex_with_substitutes(region, s),
                            RegexOptions::REGEX_OPTION_CAPTURE_GROUP,
                            Syntax::default())
            .unwrap()
    }

    pub fn regex(&self) -> &Regex {
        if let Some(regex) = self.regex.borrow() {
            regex
        } else {
            // TODO don't panic on invalid regex
            let regex = Regex::with_options(
                &self.regex_str,
                RegexOptions::REGEX_OPTION_CAPTURE_GROUP,
                Syntax::default(),
            ).unwrap();
            // Fill returns an error if it has already been filled. This might
            // happen if two threads race here. In that case, just use the value
            // that won and is now in the cell.
            self.regex.fill(regex).ok();
            self.regex.borrow().unwrap()
        }
    }
}

impl Clone for MatchPattern {
    fn clone(&self) -> MatchPattern {
        MatchPattern {
            has_captures: self.has_captures,
            regex_str: self.regex_str.clone(),
            scope: self.scope.clone(),
            captures: self.captures.clone(),
            operation: self.operation.clone(),
            with_prototype: self.with_prototype.clone(),
            // Can't clone Regex, will have to be recompiled when needed
            regex: AtomicLazyCell::new(),
        }
    }
}

impl Eq for MatchPattern {}

impl PartialEq for MatchPattern {
    fn eq(&self, other: &MatchPattern) -> bool {
        self.has_captures == other.has_captures &&
            self.regex_str == other.regex_str &&
            self.scope == other.scope &&
            self.captures == other.captures &&
            self.operation == other.operation &&
            self.with_prototype == other.with_prototype
    }
}



/// Serialize the provided map in natural key order, so that it's deterministic when dumping.
fn ordered_map<K, V, S>(map: &HashMap<K, V>, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer, K: Eq + Hash + Ord + Serialize, V: Serialize
{
    let ordered: BTreeMap<_, _> = map.iter().collect();
    ordered.serialize(serializer)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_compile_refs() {
        use onig::{SearchOptions, Regex, Region};
        let pat = MatchPattern {
            has_captures: true,
            regex_str: String::from(r"lol \\ \2 \1 '\9' \wz"),
            scope: vec![],
            captures: None,
            operation: MatchOperation::None,
            with_prototype: None,
            regex: AtomicLazyCell::new(),
        };
        let r = Regex::new(r"(\\\[\]\(\))(b)(c)(d)(e)").unwrap();
        let mut region = Region::new();
        let s = r"\[]()bcde";
        assert!(r.match_with_options(s, 0, SearchOptions::SEARCH_OPTION_NONE, Some(&mut region)).is_some());

        let regex_res = pat.regex_with_substitutes(&region, s);
        assert_eq!(regex_res, r"lol \\ b \\\[\]\(\) '' \wz");
        pat.regex_with_refs(&region, s);
    }

    #[test]
    fn caches_compiled_regex() {
        let pat = MatchPattern {
            has_captures: false,
            regex_str: String::from(r"\w+"),
            scope: vec![],
            captures: None,
            operation: MatchOperation::None,
            with_prototype: None,
            regex: AtomicLazyCell::new(),
        };

        assert!(pat.regex().is_match("test"));
        assert!(pat.regex.filled());
    }
}
