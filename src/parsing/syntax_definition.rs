//! This module contains data structures for representing syntax definitions.
//! Everything is public because I want this library to be useful in super
//! integrated cases like text editors and I have no idea what kind of monkeying
//! you might want to do with the data. Perhaps parsing your own syntax format
//! into this data structure?
use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use onig::{Regex, RegexOptions, Region, Syntax};
use std::rc::{Rc, Weak};
use std::cell::RefCell;
use super::scope::*;
use regex_syntax::escape;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::metadata::Metadata;

pub type CaptureMapping = Vec<(usize, Vec<Scope>)>;
pub type ContextPtr = Rc<RefCell<Context>>;

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
    /// Filled in at link time to avoid serializing it multiple times
    #[serde(skip_serializing, skip_deserializing)]
    pub prototype: Option<ContextPtr>,

    #[serde(serialize_with = "ordered_map")]
    pub variables: HashMap<String, String>,
    #[serde(serialize_with = "ordered_map")]
    pub contexts: HashMap<String, ContextPtr>,
    #[serde(skip_serializing, skip_deserializing)]
    pub metadata: Option<Metadata>,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Context {
    pub meta_scope: Vec<Scope>,
    pub meta_content_scope: Vec<Scope>,
    /// This being set false in the syntax file implies this field being set false,
    /// but it can also be set falso for contexts that don't include the prototype for other reasons
    pub meta_include_prototype: bool,
    pub clear_scopes: Option<ClearAmount>,
    /// This is filled in by the linker at link time
    /// for contexts that have `meta_include_prototype==true`
    /// and are not included from the prototype.
    #[serde(skip_serializing, skip_deserializing)]
    pub prototype: Option<ContextPtr>,
    pub uses_backrefs: bool,

    pub patterns: Vec<Pattern>,
}

impl Context {
    pub fn new(meta_include_prototype: bool) -> Context {
        Context {
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

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Pattern {
    Match(MatchPattern),
    Include(ContextReference),
}

/// Used to iterate over all the match patterns in a context.
/// Basically walks the tree of patterns and include directives
/// in the correct order.
#[derive(Debug)]
pub struct MatchIter {
    ctx_stack: Vec<ContextPtr>,
    index_stack: Vec<usize>,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MatchPattern {
    pub has_captures: bool,
    pub regex_str: String,
    #[serde(skip_serializing, skip_deserializing)]
    pub regex: Option<Regex>,
    pub scope: Vec<Scope>,
    pub captures: Option<CaptureMapping>,
    pub operation: MatchOperation,
    pub with_prototype: Option<ContextPtr>,
}

/// This wrapper only exists so that I can implement a serialization
/// trait that crashes if you try and serialize this.
#[derive(Debug)]
pub struct LinkerLink {
    pub link: Weak<RefCell<Context>>,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
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
    Direct(LinkerLink),
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
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
            // uncomment for debugging infinite recursion
            // println!("{:?}", self.index_stack);
            // use std::thread::sleep_ms;
            // sleep_ms(500);
            let last_index = self.ctx_stack.len() - 1;
            let context_ref = self.ctx_stack[last_index].clone();
            let context = context_ref.borrow();
            let index = self.index_stack[last_index];
            self.index_stack[last_index] = index + 1;
            if index < context.patterns.len() {
                match context.patterns[index] {
                    Pattern::Match(_) => return Some((context_ref.clone(), index)),
                    Pattern::Include(ref ctx_ref) => {
                        let ctx_ptr = match *ctx_ref {
                            ContextReference::Inline(ref ctx_ptr) => ctx_ptr.clone(),
                            ContextReference::Direct(ref ctx_ptr) => {
                                ctx_ptr.link.upgrade().unwrap()
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
pub fn context_iter(ctx: ContextPtr) -> MatchIter {
    MatchIter {
        ctx_stack: vec![ctx],
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

    /// Returns a mutable reference, otherwise like `match_at`
    pub fn match_at_mut(&mut self, index: usize) -> &mut MatchPattern {
        match self.patterns[index] {
            Pattern::Match(ref mut match_pat) => match_pat,
            _ => panic!("bad index to match_at"),
        }
    }
}

impl ContextReference {
    /// find the pointed to context, panics if ref is not linked
    pub fn resolve(&self) -> ContextPtr {
        match *self {
            ContextReference::Inline(ref ptr) => ptr.clone(),
            ContextReference::Direct(ref ptr) => ptr.link.upgrade().unwrap(),
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

impl MatchPattern {

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
    pub fn compile_with_refs(&self, region: &Region, s: &str) -> Regex {
        // TODO don't panic on invalid regex
        Regex::with_options(&self.regex_with_substitutes(region, s),
                            RegexOptions::REGEX_OPTION_CAPTURE_GROUP,
                            Syntax::default())
            .unwrap()
    }

    fn compile_regex(&mut self) {
        // TODO don't panic on invalid regex
        let compiled = Regex::with_options(&self.regex_str,
                                           RegexOptions::REGEX_OPTION_CAPTURE_GROUP,
                                           Syntax::default())
            .unwrap();
        self.regex = Some(compiled);
    }

    /// Makes sure the regex is compiled if it doesn't have captures.
    /// May compile the regex if it isn't, panicing if compilation fails.
    #[inline]
    pub fn ensure_compiled_if_possible(&mut self) {
        if self.regex.is_none() && !self.has_captures {
            self.compile_regex();
        }
    }
}

impl Eq for LinkerLink {}

impl PartialEq for LinkerLink {
    fn eq(&self, other: &LinkerLink) -> bool {
        self.link.upgrade() == other.link.upgrade()
    }
}


/// Just panics, we can't do anything with linked up syntaxes
impl Serialize for LinkerLink {
    fn serialize<S>(&self, _: S) -> Result<S::Ok, S::Error> where S: Serializer {
        panic!("Can't serialize syntax definitions which have been linked");
    }
}

/// Just panics, we can't do anything with linked up syntaxes
impl<'de> Deserialize<'de> for LinkerLink {
    fn deserialize<D>(_: D) -> Result<Self, D::Error> where D: Deserializer<'de> {
        panic!("No linked syntax should ever have gotten serialized");
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
            regex: None,
            scope: vec![],
            captures: None,
            operation: MatchOperation::None,
            with_prototype: None,
        };
        let r = Regex::new(r"(\\\[\]\(\))(b)(c)(d)(e)").unwrap();
        let mut region = Region::new();
        let s = r"\[]()bcde";
        assert!(r.match_with_options(s, 0, SearchOptions::SEARCH_OPTION_NONE, Some(&mut region)).is_some());

        let regex_res = pat.regex_with_substitutes(&region, s);
        assert_eq!(regex_res, r"lol \\ b \\\[\]\(\) '' \wz");
        pat.compile_with_refs(&region, s);
    }
}
