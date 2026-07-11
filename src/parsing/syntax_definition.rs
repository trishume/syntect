//! Data structures for representing syntax definitions
//!
//! Everything here is public becaues I want this library to be useful in super integrated cases
//! like text editors and I have no idea what kind of monkeying you might want to do with the data.
//! Perhaps parsing your own syntax format into this data structure?

use super::regex::{Regex, Region};
use super::{scope::*, ParsingError};
use crate::parsing::syntax_set::SyntaxSet;
use regex_syntax::escape;
use serde::ser::{Serialize, Serializer};
use serde_derive::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

pub type CaptureMapping = Vec<(usize, Vec<Scope>)>;

/// Information about the escape pattern for an `embed` operation.
/// The escape regex takes strict precedence over all other patterns,
/// unlike `with_prototype` where patterns compete equally.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EscapeInfo {
    pub escape_regex: Regex,
    pub has_captures: bool,
    pub escape_captures: Option<CaptureMapping>,
    /// Raw escape regex string as written in YAML, before variable resolution.
    /// Stored to enable re-resolution when variables are merged via extends.
    #[serde(skip)]
    pub(crate) raw_escape_regex_str: Option<String>,
}

/// An opaque ID for a [`Context`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ContextId {
    /// Index into [`SyntaxSet::syntaxes`]
    pub(crate) syntax_index: usize,

    /// Index into [`crate::parsing::LazyContexts::contexts`] for the [`Self::syntax_index`] syntax
    pub(crate) context_index: usize,
}

/// The main data structure representing a syntax definition loaded from a
/// `.sublime-syntax` file
///
/// You'll probably only need these as references to be passed around to parsing code.
///
/// Some useful public fields are the `name` field which is a human readable name to display in
/// syntax lists, and the `hidden` field which means hide this syntax from any lists because it is
/// for internal use.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyntaxDefinition {
    pub name: String,
    pub file_extensions: Vec<String>,
    pub scope: Scope,
    pub first_line_match: Option<String>,
    pub hidden: bool,
    #[serde(serialize_with = "ordered_map")]
    pub variables: HashMap<String, String>,
    #[serde(serialize_with = "ordered_map")]
    pub contexts: HashMap<String, Context>,
    /// The syntax(es) this definition extends (e.g., "Packages/C/C.sublime-syntax").
    /// Can be a single path or a list of paths for multiple inheritance.
    #[serde(default)]
    pub extends: Vec<String>,
    /// The version of the sublime-syntax format (1 or 2). Default is 1.
    #[serde(default = "default_version")]
    pub version: u32,
}

fn default_version() -> u32 {
    1
}

fn one() -> usize {
    1
}

/// How a child context should merge with its parent during extends resolution.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ContextMergeMode {
    /// Replace the parent context entirely (default behavior).
    #[default]
    Replace,
    /// Prepend child patterns before parent patterns.
    Prepend,
    /// Append child patterns after parent patterns.
    Append,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Context {
    pub meta_scope: Vec<Scope>,
    pub meta_content_scope: Vec<Scope>,
    /// Whether this context includes its syntax's prototype. `Some(bool)` carries the value
    /// set by the YAML (or inherited from a parent on an `extends:` / `meta_append` /
    /// `meta_prepend` merge). `None` means the field was never set explicitly and callers
    /// should treat it as `true` (Sublime's default — use `.unwrap_or(true)` at consumption
    /// points). Tracking unset separately lets an `extends` merge inherit the parent's
    /// value when the child doesn't restate it — the case that was miscompiling TSQL's
    /// `inside-like-single-quoted-string` `meta_append` against the SQL base that sets
    /// `meta_include_prototype: false`.
    pub meta_include_prototype: Option<bool>,
    pub clear_scopes: Option<ClearAmount>,
    /// This is filled in by the linker at link time
    /// for contexts that have `meta_include_prototype==true`
    /// and are not included from the prototype.
    pub prototype: Option<ContextId>,
    pub uses_backrefs: bool,

    pub patterns: Vec<Pattern>,

    /// How this context should be merged with a parent context during extends resolution.
    #[serde(skip)]
    pub(crate) merge_mode: ContextMergeMode,

    /// When true, this context's `meta_content_scope` (from embed_scope) should replace
    /// the embedded syntax's top-level scope rather than stacking with it. (v2 behavior)
    #[serde(default)]
    pub(crate) embed_scope_replaces: bool,
}

impl Context {
    pub fn new(meta_include_prototype: Option<bool>) -> Context {
        Context {
            meta_scope: Vec::new(),
            meta_content_scope: Vec::new(),
            meta_include_prototype,
            clear_scopes: None,
            uses_backrefs: false,
            patterns: Vec::new(),
            prototype: None,
            merge_mode: ContextMergeMode::default(),
            embed_scope_replaces: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Pattern {
    Match(MatchPattern),
    Include(ContextReference),
    /// Like `Include`, but also applies the target syntax's `prototype` context.
    /// Created when `apply_prototype: true` is used with an `include` directive.
    IncludeWithPrototype(ContextReference),
}

/// Used to iterate over all the match patterns in a context
///
/// Basically walks the tree of patterns and include directives in the correct order.
#[derive(Debug)]
pub struct MatchIter<'a> {
    syntax_set: &'a SyntaxSet,
    ctx_stack: Vec<&'a Context>,
    index_stack: Vec<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MatchPattern {
    pub has_captures: bool,
    pub regex: Regex,
    pub scope: Vec<Scope>,
    pub captures: Option<CaptureMapping>,
    pub operation: MatchOperation,
    pub with_prototype: Option<ContextReference>,
    /// Raw regex string as written in YAML, before variable resolution and transforms.
    /// Stored to enable re-resolution when variables are overridden via extends.
    #[serde(skip)]
    pub(crate) raw_regex_str: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ContextReference {
    #[non_exhaustive]
    Named(String),
    #[non_exhaustive]
    ByScope {
        scope: Scope,
        sub_context: Option<String>,
        /// `true` if this reference by scope is part of an `embed` for which
        /// there is an `escape`. In other words a reference for a context for
        /// which there "always is a way out". Enables falling back to `Plain
        /// Text` syntax in case the referenced scope is missing.
        with_escape: bool,
    },
    #[non_exhaustive]
    File {
        name: String,
        sub_context: Option<String>,
        /// Same semantics as for [`Self::ByScope::with_escape`].
        with_escape: bool,
    },
    #[non_exhaustive]
    Inline(String),
    #[non_exhaustive]
    Direct(ContextId),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MatchOperation {
    /// Pushes `ctx_refs`, optionally popping `pop_count` contexts first.
    /// A plain `push:` is `pop_count == 0`; `pop: N + push:` is
    /// `pop_count == N`. Per ST, `pop:N + push:` is **lookahead**: the
    /// trigger token does not inherit the popped frames' meta_scope.
    Push {
        ctx_refs: Vec<ContextReference>,
        #[serde(default)]
        pop_count: usize,
    },
    /// Pops `pop_count` contexts off the stack, then pushes `ctx_refs`.
    /// A plain `set:` is `pop_count == 1`; `pop: N + set:` is `pop_count == N`.
    /// Per ST, `pop:N + set:` is **stacking**: the trigger token receives
    /// both the popped frames' meta_scope and the pushed meta_scope.
    Set {
        ctx_refs: Vec<ContextReference>,
        #[serde(default = "one")]
        pop_count: usize,
    },
    Pop(usize),
    None,
    /// Branch with backtracking.
    /// Acts like Push for the first alternative, saving a checkpoint.
    /// If a `Fail` with the same name fires later, the next alternative is tried.
    Branch {
        name: String,
        alternatives: Vec<ContextReference>,
        /// Number of contexts to pop before pushing the branch alternative.
        /// In Sublime Text, `pop: N` combined with `branch:` pops N contexts
        /// then sets up the branch point.
        #[serde(default)]
        pop_count: usize,
    },
    /// Trigger backtracking to the named branch point.
    Fail(String),
    /// Embed contexts with a prioritized escape pattern.
    /// Unlike Push+with_prototype, the escape regex takes strict precedence
    /// over all other patterns — it is checked first and truncates the search
    /// region for normal patterns.
    Embed {
        contexts: Vec<ContextReference>,
        escape: EscapeInfo,
        /// Number of contexts to pop before pushing the embedded contexts.
        /// In Sublime Text, `pop: N` combined with `embed:` pops N contexts
        /// then pushes the embedded syntax with escape priority.
        #[serde(default)]
        pop_count: usize,
    },
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
                    Pattern::Match(_) => {
                        return Some((context, index));
                    }
                    Pattern::Include(ref ctx_ref) => {
                        let ctx_ptr = match *ctx_ref {
                            ContextReference::Direct(ref context_id) => {
                                self.syntax_set.get_context(context_id).unwrap()
                            }
                            _ => return self.next(), // skip this and move onto the next one
                        };
                        self.ctx_stack.push(ctx_ptr);
                        self.index_stack.push(0);
                    }
                    Pattern::IncludeWithPrototype(ref ctx_ref) => {
                        let context_id = match *ctx_ref {
                            ContextReference::Direct(ref id) => id,
                            _ => return self.next(),
                        };
                        let ctx_ptr = self.syntax_set.get_context(context_id).unwrap();
                        // Push target first, then external prototype on top so
                        // its patterns are iterated first: `MatchIter::next`
                        // reads the stack top, and ST's `apply_prototype` runs
                        // the prototype ahead of the included context — mirror
                        // of `ParseState::find_best_match`'s prototype chaining.
                        self.ctx_stack.push(ctx_ptr);
                        self.index_stack.push(0);
                        if ctx_ptr.meta_include_prototype.unwrap_or(true) {
                            if let Some(ref proto_id) = ctx_ptr.prototype {
                                let proto_ctx = self.syntax_set.get_context(proto_id).unwrap();
                                self.ctx_stack.push(proto_ctx);
                                self.index_stack.push(0);
                            }
                        }
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
///
/// It recursively follows include directives. Can only be run on contexts that have already been
/// linked up.
pub fn context_iter<'a>(syntax_set: &'a SyntaxSet, context: &'a Context) -> MatchIter<'a> {
    MatchIter {
        syntax_set,
        ctx_stack: vec![context],
        index_stack: vec![0],
    }
}

impl Context {
    /// Returns the match pattern at an index
    pub fn match_at(&self, index: usize) -> Result<&MatchPattern, ParsingError> {
        match self.patterns[index] {
            Pattern::Match(ref match_pat) => Ok(match_pat),
            _ => Err(ParsingError::BadMatchIndex(index)),
        }
    }
}

impl ContextReference {
    /// find the pointed to context
    pub fn resolve<'a>(&self, syntax_set: &'a SyntaxSet) -> Result<&'a Context, ParsingError> {
        match *self {
            ContextReference::Direct(ref context_id) => syntax_set.get_context(context_id),
            _ => Err(ParsingError::UnresolvedContextReference(self.clone())),
        }
    }

    /// get the context ID this reference points to
    pub fn id(&self) -> Result<ContextId, ParsingError> {
        match *self {
            ContextReference::Direct(ref context_id) => Ok(*context_id),
            _ => Err(ParsingError::UnresolvedContextReference(self.clone())),
        }
    }
}

pub(crate) fn substitute_backrefs_in_regex<F>(regex_str: &str, substituter: F) -> String
where
    F: Fn(usize) -> Option<String>,
{
    let mut reg_str = String::with_capacity(regex_str.len());

    let mut last_was_escape = false;
    for c in regex_str.chars() {
        if last_was_escape && c.is_ascii_digit() {
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
    if last_was_escape {
        reg_str.push('\\');
    }
    reg_str
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
            regex: Regex::new(regex_str),
            scope,
            captures,
            operation,
            with_prototype,
            raw_regex_str: None,
        }
    }

    pub(crate) fn new_with_raw(
        has_captures: bool,
        regex_str: String,
        raw_regex_str: String,
        scope: Vec<Scope>,
        captures: Option<CaptureMapping>,
        operation: MatchOperation,
        with_prototype: Option<ContextReference>,
    ) -> MatchPattern {
        MatchPattern {
            has_captures,
            regex: Regex::new(regex_str),
            scope,
            captures,
            operation,
            with_prototype,
            raw_regex_str: Some(raw_regex_str),
        }
    }

    /// Used by the parser to compile a regex which needs to reference
    /// regions from another matched pattern.
    pub fn regex_with_refs(&self, region: &Region, text: &str) -> Regex {
        let new_regex = substitute_backrefs_in_regex(self.regex.regex_str(), |i| {
            region.pos(i).map(|(start, end)| escape(&text[start..end]))
        });

        Regex::new(new_regex)
    }

    pub fn regex(&self) -> &Regex {
        &self.regex
    }
}

/// Serialize the provided map in natural key order, so that it's deterministic when dumping.
pub(crate) fn ordered_map<K, V, S>(map: &HashMap<K, V>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    K: Eq + Hash + Ord + Serialize,
    V: Serialize,
{
    let ordered: BTreeMap<_, _> = map.iter().collect();
    ordered.serialize(serializer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_compile_refs() {
        let pat = MatchPattern {
            has_captures: true,
            regex: Regex::new(r"lol \\ \2 \1 '\9' \wz".into()),
            scope: vec![],
            captures: None,
            operation: MatchOperation::None,
            with_prototype: None,
            raw_regex_str: None,
        };
        let r = Regex::new(r"(\\\[\]\(\))(b)(c)(d)(e)".into());
        let s = r"\[]()bcde";
        let mut region = Region::new();
        let matched = r.search(s, 0, s.len(), Some(&mut region), true);
        assert!(matched);

        let regex_with_refs = pat.regex_with_refs(&region, s);
        assert_eq!(regex_with_refs.regex_str(), r"lol \\ b \\\[\]\(\) '' \wz");
    }
}
