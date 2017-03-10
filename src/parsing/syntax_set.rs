use super::syntax_definition::*;
use super::scope::*;
#[cfg(feature = "yaml-load")]
use super::super::LoadingError;

use std::path::Path;
#[cfg(feature = "yaml-load")]
use walkdir::WalkDir;
#[cfg(feature = "yaml-load")]
use std::io::Read;
use std::io::{self, BufRead, BufReader};
use std::fs::File;
use std::ops::DerefMut;
use std::mem;
use std::rc::Rc;
use std::ascii::AsciiExt;
use std::sync::Mutex;
use onig::Regex;
use rustc_serialize::{Encodable, Encoder, Decodable, Decoder};

/// A syntax set holds a bunch of syntaxes and manages
/// loading them and the crucial operation of *linking*.
///
/// Linking replaces the references between syntaxes with direct
/// pointers. See `link_syntaxes` for more.
/// Linking, followed by adding more unlinked syntaxes with `load_syntaxes`
/// and then linking again is allowed.
#[derive(Debug)]
pub struct SyntaxSet {
    syntaxes: Vec<SyntaxDefinition>,
    pub is_linked: bool,
    first_line_cache: Mutex<FirstLineCache>,
}

#[cfg(feature = "yaml-load")]
fn load_syntax_file(p: &Path,
                    lines_include_newline: bool)
                    -> Result<SyntaxDefinition, LoadingError> {
    let mut f = try!(File::open(p));
    let mut s = String::new();
    try!(f.read_to_string(&mut s));

    Ok(try!(SyntaxDefinition::load_from_str(&s, lines_include_newline)))
}

impl Default for SyntaxSet {
    fn default() -> Self {
        SyntaxSet {
            syntaxes: Vec::new(),
            is_linked: true,
            first_line_cache: Mutex::new(FirstLineCache::new()),
        }
    }
}

impl SyntaxSet {
    pub fn new() -> SyntaxSet {
        SyntaxSet::default()
    }

    /// Convenience constructor calling `new` and then `load_syntaxes` on the resulting set
    /// defaults to lines given not including newline characters, see the
    /// `load_syntaxes` method docs for an explanation as to why this might not be the best.
    /// It also links all the syntaxes together, see `link_syntaxes` for what that means.
    #[cfg(feature = "yaml-load")]
    pub fn load_from_folder<P: AsRef<Path>>(folder: P) -> Result<SyntaxSet, LoadingError> {
        let mut ps = Self::new();
        try!(ps.load_syntaxes(folder, false));
        ps.link_syntaxes();
        Ok(ps)
    }

    /// Loads all the .sublime-syntax files in a folder into this syntax set.
    /// It does not link the syntaxes, in case you want to serialize this syntax set.
    ///
    /// The `lines_include_newline` parameter is used to work around the fact that Sublime Text normally
    /// passes line strings including newline characters (`\n`) to its regex engine. This results in many
    /// syntaxes having regexes matching `\n`, which doesn't work if you don't pass in newlines.
    /// It is recommended that if you can you pass in lines with newlines if you can and pass `true` for this parameter.
    /// If that is inconvenient pass `false` and the loader will do some hacky find and replaces on the
    /// match regexes that seem to work for the default syntax set, but may not work for any other syntaxes.
    ///
    /// In the future I might include a "slow mode" that copies the lines passed in and appends a newline if there isn't one.
    /// but in the interest of performance currently this hacky fix will have to do.
    #[cfg(feature = "yaml-load")]
    pub fn load_syntaxes<P: AsRef<Path>>(&mut self,
                                         folder: P,
                                         lines_include_newline: bool)
                                         -> Result<(), LoadingError> {
        self.is_linked = false;
        for entry in WalkDir::new(folder) {
            let entry = try!(entry.map_err(LoadingError::WalkDir));
            if entry.path().extension().map_or(false, |e| e == "sublime-syntax") {
                // println!("{}", entry.path().display());
                self.syntaxes.push(try!(load_syntax_file(entry.path(), lines_include_newline)));
            }
        }
        Ok(())
    }

    /// Add a syntax to the set. If the set was linked it is now only partially linked
    /// and you'll have to link it again for full linking.
    pub fn add_syntax(&mut self, syntax: SyntaxDefinition) {
        self.is_linked = false;
        self.syntaxes.push(syntax);
    }

    /// The list of syntaxes in the set
    pub fn syntaxes(&self) -> &[SyntaxDefinition] {
        &self.syntaxes[..]
    }

    /// Rarely useful method that loads in a syntax with no highlighting rules for plain text.
    /// Exists mainly for adding the plain text syntax to syntax set dumps, because for some
    /// reason the default Sublime plain text syntax is still in `.tmLanguage` format.
    #[cfg(feature = "yaml-load")]
    pub fn load_plain_text_syntax(&mut self) {
        let s = "---\nname: Plain Text\nfile_extensions: [txt]\nscope: text.plain\ncontexts: \
                 {main: []}";
        let syn = SyntaxDefinition::load_from_str(s, false).unwrap();
        self.syntaxes.push(syn);
    }

    /// Finds a syntax by its default scope, for example `source.regexp` finds the regex syntax.
    /// This and all similar methods below do a linear search of syntaxes, this should be fast
    /// because there aren't many syntaxes, but don't think you can call it a bajillion times per second.
    pub fn find_syntax_by_scope(&self, scope: Scope) -> Option<&SyntaxDefinition> {
        self.syntaxes.iter().find(|&s| s.scope == scope)
    }

    pub fn find_syntax_by_name<'a>(&'a self, name: &str) -> Option<&'a SyntaxDefinition> {
        self.syntaxes.iter().find(|&s| name == &s.name)
    }

    pub fn find_syntax_by_extension<'a>(&'a self, extension: &str) -> Option<&'a SyntaxDefinition> {
        self.syntaxes.iter().find(|&s| s.file_extensions.iter().any(|e| e == extension))
    }

    /// Searches for a syntax first by extension and then by case-insensitive name
    /// useful for things like Github-flavoured-markdown code block highlighting where
    /// all you have to go on is a short token given by the user
    pub fn find_syntax_by_token<'a>(&'a self, s: &str) -> Option<&'a SyntaxDefinition> {
        {
            let ext_res = self.find_syntax_by_extension(s);
            if ext_res.is_some() {
                return ext_res;
            }
        }
        let lower = s.to_ascii_lowercase();
        self.syntaxes.iter().find(|&s| lower == s.name.to_ascii_lowercase())
    }

    /// Try to find the syntax for a file based on its first line.
    /// This uses regexes that come with some sublime syntax grammars
    /// for matching things like shebangs and mode lines like `-*- Mode: C -*-`
    pub fn find_syntax_by_first_line<'a>(&'a self, s: &str) -> Option<&'a SyntaxDefinition> {
        let mut cache = self.first_line_cache.lock().unwrap();
        cache.ensure_filled(self.syntaxes());
        for &(ref reg, i) in &cache.regexes {
            if reg.find(s).is_some() {
                return Some(&self.syntaxes[i]);
            }
        }
        None
    }

    /// Convenience method that tries to find the syntax for a file path,
    /// first by extension and then by first line of the file if that doesn't work.
    /// May IO Error because it sometimes tries to read the first line of the file.
    ///
    /// # Examples
    /// When determining how to highlight a file, use this in combination with a fallback to plain text:
    ///
    /// ```
    /// use syntect::parsing::SyntaxSet;
    /// let ss = SyntaxSet::load_defaults_nonewlines();
    /// let syntax = ss.find_syntax_for_file("testdata/highlight_test.erb")
    ///     .unwrap() // for IO errors, you may want to use try!() or another plain text fallback
    ///     .unwrap_or_else(|| ss.find_syntax_plain_text());
    /// assert_eq!(syntax.name, "HTML (Rails)");
    /// ```
    pub fn find_syntax_for_file<P: AsRef<Path>>(&self,
                                                path_obj: P)
                                                -> io::Result<Option<&SyntaxDefinition>> {
        let path: &Path = path_obj.as_ref();
        let extension = path.extension().and_then(|x| x.to_str()).unwrap_or("");
        let ext_syntax = self.find_syntax_by_extension(extension);
        let line_syntax = if ext_syntax.is_none() {
            let mut line = String::new();
            let f = try!(File::open(path));
            let mut line_reader = BufReader::new(&f);
            try!(line_reader.read_line(&mut line));
            self.find_syntax_by_first_line(&line)
        } else {
            None
        };
        let syntax = ext_syntax.or(line_syntax);
        Ok(syntax)
    }

    /// Finds a syntax for plain text, which usually has no highlighting rules.
    /// Good as a fallback when you can't find another syntax but you still want
    /// to use the same highlighting pipeline code.
    ///
    /// This syntax should always be present, if not this method will panic.
    /// If the way you load syntaxes doesn't create one, use `load_plain_text_syntax`.
    ///
    /// # Examples
    /// ```
    /// use syntect::parsing::SyntaxSet;
    /// let mut ss = SyntaxSet::new();
    /// ss.load_plain_text_syntax();
    /// let syntax = ss.find_syntax_by_token("rs").unwrap_or_else(|| ss.find_syntax_plain_text());
    /// assert_eq!(syntax.name, "Plain Text");
    /// ```
    pub fn find_syntax_plain_text(&self) -> &SyntaxDefinition {
        self.find_syntax_by_name("Plain Text")
            .expect("All syntax sets ought to have a plain text syntax")
    }

    /// This links all the syntaxes in this set directly with pointers for performance purposes.
    /// It is necessary to do this before parsing anything with these syntaxes.
    /// However, it is not possible to serialize a syntax set that has been linked,
    /// which is why it isn't done by default, except by the load_from_folder constructor.
    /// This operation is idempotent, but takes time even on already linked syntax sets.
    pub fn link_syntaxes(&mut self) {
        // 2 loops necessary to satisfy borrow checker :-(
        for syntax in &mut self.syntaxes {
            if let Some(proto_ptr) = syntax.contexts.get("prototype") {
                Self::recursively_mark_no_prototype(syntax, proto_ptr.clone());
                syntax.prototype = Some((*proto_ptr).clone());
            }
        }
        for syntax in &self.syntaxes {
            for context_ptr in syntax.contexts.values() {
                let mut mut_ref = context_ptr.borrow_mut();
                self.link_context(syntax, mut_ref.deref_mut());
            }
        }
        self.is_linked = true;
    }

    /// Anything recursively included by the prototype shouldn't include the prototype.
    /// This marks them as such.
    fn recursively_mark_no_prototype(syntax: &SyntaxDefinition, context_ptr: ContextPtr) {
        if let Ok(mut mut_ref) = context_ptr.try_borrow_mut() {
            let context = mut_ref.deref_mut();
            context.meta_include_prototype = false;
            for pattern in &mut context.patterns {
                match *pattern {
                    /// Apparently inline blocks also don't include the prototype when within the prototype.
                    /// This is really weird, but necessary to run the YAML syntax.
                    Pattern::Match(ref mut match_pat) => {
                        let maybe_context_refs = match match_pat.operation {
                            MatchOperation::Push(ref context_refs) |
                            MatchOperation::Set(ref context_refs) => Some(context_refs),
                            MatchOperation::Pop | MatchOperation::None => None,
                        };
                        if let Some(context_refs) = maybe_context_refs {
                            for context_ref in context_refs.iter() {
                                if let ContextReference::Inline(ref context_ptr) = *context_ref {
                                    Self::recursively_mark_no_prototype(syntax, context_ptr.clone());
                                }
                            }
                        }
                    }
                    Pattern::Include(ContextReference::Named(ref s)) => {
                        if let Some(context_ptr) = syntax.contexts.get(s) {
                            Self::recursively_mark_no_prototype(syntax, context_ptr.clone());
                        }
                    }
                    _ => (),
                }
            }
        }
    }

    fn link_context(&self, syntax: &SyntaxDefinition, context: &mut Context) {
        if context.meta_include_prototype {
            if let Some(ref proto_ptr) = syntax.prototype {
                context.prototype = Some((*proto_ptr).clone());
            }
        }
        for pattern in &mut context.patterns {
            match *pattern {
                Pattern::Match(ref mut match_pat) => self.link_match_pat(syntax, match_pat),
                Pattern::Include(ref mut context_ref) => self.link_ref(syntax, context_ref),
            }
        }
    }

    fn link_ref(&self, syntax: &SyntaxDefinition, context_ref: &mut ContextReference) {
        // println!("{:?}", context_ref);
        use super::syntax_definition::ContextReference::*;
        let maybe_new_context = match *context_ref {
            Named(ref s) => {
                // This isn't actually correct, but it is better than nothing/crashing.
                // This is being phased out anyhow, see https://github.com/sublimehq/Packages/issues/73
                // Fixes issue #30
                if s == "$top_level_main" {
                    syntax.contexts.get("main")
                } else {
                    syntax.contexts.get(s)
                }
            }
            Inline(ref context_ptr) => {
                let mut mut_ref = context_ptr.borrow_mut();
                self.link_context(syntax, mut_ref.deref_mut());
                None
            }
            ByScope { scope, ref sub_context } => {
                let other_syntax = self.find_syntax_by_scope(scope);
                let context_name = sub_context.as_ref().map_or("main", |x| &**x);
                other_syntax.and_then(|s| s.contexts.get(context_name))
            }
            File { ref name, ref sub_context } => {
                let other_syntax = self.find_syntax_by_name(name);
                let context_name = sub_context.as_ref().map_or("main", |x| &**x);
                other_syntax.and_then(|s| s.contexts.get(context_name))
            }
            Direct(_) => None,
        };
        if let Some(new_context) = maybe_new_context {
            let mut new_ref = Direct(LinkerLink { link: Rc::downgrade(new_context) });
            mem::swap(context_ref, &mut new_ref);
        }
    }

    fn link_match_pat(&self, syntax: &SyntaxDefinition, match_pat: &mut MatchPattern) {
        let maybe_context_refs = match match_pat.operation {
            MatchOperation::Push(ref mut context_refs) |
            MatchOperation::Set(ref mut context_refs) => Some(context_refs),
            MatchOperation::Pop | MatchOperation::None => None,
        };
        if let Some(context_refs) = maybe_context_refs {
            for context_ref in context_refs.iter_mut() {
                self.link_ref(syntax, context_ref);
            }
        }
        if let Some(ref context_ptr) = match_pat.with_prototype {
            let mut mut_ref = context_ptr.borrow_mut();
            self.link_context(syntax, mut_ref.deref_mut());
        }
    }
}

#[derive(Debug)]
struct FirstLineCache {
    /// (first line regex, syntax index) pairs for all syntaxes with a first line regex
    /// built lazily on first use of `find_syntax_by_first_line`.
    regexes: Vec<(Regex, usize)>,
    /// To what extent the first line cache has been built
    cached_until: usize,
}

impl FirstLineCache {
    fn new() -> FirstLineCache {
        FirstLineCache {
            regexes: Vec::new(),
            cached_until: 0,
        }
    }

    fn ensure_filled(&mut self, syntaxes: &[SyntaxDefinition]) {
        if self.cached_until >= syntaxes.len() {
            return;
        }

        for (i, syntax) in syntaxes[self.cached_until..].iter().enumerate() {
            if let Some(ref reg_str) = syntax.first_line_match {
                if let Ok(reg) = Regex::new(reg_str) {
                    self.regexes.push((reg, i));
                }
            }
        }

        self.cached_until = syntaxes.len();
    }
}

impl Encodable for SyntaxSet {
    fn encode<S: Encoder>(&self, s: &mut S) -> Result<(), S::Error> {
        s.emit_struct("SyntaxSet", 2, |s| {
            try!(s.emit_struct_field("syntaxes", 0, |s| self.syntaxes.encode(s)));
            try!(s.emit_struct_field("is_linked", 1, |s| self.is_linked.encode(s)));
            Ok(())
        })
    }
}

impl Decodable for SyntaxSet {
    fn decode<D: Decoder>(d: &mut D) -> Result<Self, D::Error> {
        d.read_struct("SyntaxSet", 2, |d| {
            let ss = SyntaxSet {
                syntaxes: try!(d.read_struct_field("syntaxes", 0, Decodable::decode)),
                is_linked: try!(d.read_struct_field("is_linked", 1, Decodable::decode)),
                first_line_cache: Mutex::new(FirstLineCache::new()),
            };

            Ok(ss)
        })
    }
}

#[cfg(feature = "yaml-load")]
#[cfg(test)]
mod tests {
    use super::*;
    use parsing::{Scope, syntax_definition};
    #[test]
    fn can_load() {
        let mut ps = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        assert_eq!(&ps.find_syntax_by_first_line("#!/usr/bin/env node").unwrap().name,
                   "JavaScript");
        ps.load_plain_text_syntax();
        let rails_scope = Scope::new("source.ruby.rails").unwrap();
        let syntax = ps.find_syntax_by_name("Ruby on Rails").unwrap();
        ps.find_syntax_plain_text();
        assert_eq!(&ps.find_syntax_by_extension("rake").unwrap().name, "Ruby");
        assert_eq!(&ps.find_syntax_by_token("ruby").unwrap().name, "Ruby");
        assert_eq!(&ps.find_syntax_by_first_line("lol -*- Mode: C -*- such line").unwrap().name,
                   "C");
        assert_eq!(&ps.find_syntax_for_file("testdata/parser.rs").unwrap().unwrap().name,
                   "Rust");
        assert_eq!(&ps.find_syntax_for_file("testdata/test_first_line.test")
                       .unwrap()
                       .unwrap()
                       .name,
                   "Go");
        assert!(&ps.find_syntax_by_first_line("derp derp hi lol").is_none());
        // println!("{:#?}", syntax);
        assert_eq!(syntax.scope, rails_scope);
        // assert!(false);
        let main_context = syntax.contexts.get("main").unwrap();
        let count = syntax_definition::context_iter(main_context.clone()).count();
        assert_eq!(count, 108);
    }
}
