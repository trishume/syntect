use super::syntax_definition::*;
use super::scope::*;
use super::super::LoadingError;

use std::path::Path;
use walkdir::WalkDir;
use std::io::Read;
use std::fs::File;
use std::ops::DerefMut;
use std::mem;
use std::rc::Rc;
use std::ascii::AsciiExt;

/// A syntax set holds a bunch of syntaxes and manages
/// loading them and the crucial operation of *linking*.
///
/// Linking replaces the references between syntaxes with direct
/// pointers. See `link_syntaxes` for more.
/// Linking, followed by adding more unlinked syntaxes with `load_syntaxes`
/// and then linking again is allowed.
#[derive(Debug, RustcEncodable, RustcDecodable)]
pub struct SyntaxSet {
    pub syntaxes: Vec<SyntaxDefinition>,
    pub is_linked: bool,
}

fn load_syntax_file(p: &Path,
                    lines_include_newline: bool)
                    -> Result<SyntaxDefinition, LoadingError> {
    let mut f = try!(File::open(p));
    let mut s = String::new();
    try!(f.read_to_string(&mut s));

    Ok(try!(SyntaxDefinition::load_from_str(&s, lines_include_newline)))
}

impl SyntaxSet {
    pub fn new() -> SyntaxSet {
        SyntaxSet {
            syntaxes: Vec::new(),
            is_linked: true,
        }
    }

    /// Convenience constructor calling `new` and then `load_syntaxes` on the resulting set
    /// defaults to lines given not including newline characters, see the
    /// `load_syntaxes` method docs for an explanation as to why this might not be the best.
    /// It also links all the syntaxes together, see `link_syntaxes` for what that means.
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
    pub fn load_syntaxes<P: AsRef<Path>>(&mut self,
                                         folder: P,
                                         lines_include_newline: bool)
                                         -> Result<(), LoadingError> {
        self.is_linked = false;
        for entry in WalkDir::new(folder) {
            let entry = try!(entry.map_err(|e| LoadingError::WalkDir(e)));
            if entry.path().extension().map(|e| e == "sublime-syntax").unwrap_or(false) {
                // println!("{}", entry.path().display());
                self.syntaxes.push(try!(load_syntax_file(entry.path(), lines_include_newline)));
            }
        }
        Ok(())
    }

    /// Rarely useful method that loads in a syntax with no highlighting rules for plain text.
    /// Exists mainly for adding the plain text syntax to syntax set dumps, because for some
    /// reason the default Sublime plain text syntax is still in `.tmLanguage` format.
    pub fn load_plain_text_syntax(&mut self) {
        let s = "---\nname: Plain Text\nfile_extensions: [txt]\nscope: text.plain\ncontexts: {main: []}";
        let syn = SyntaxDefinition::load_from_str(&s, false).unwrap();
        self.syntaxes.push(syn);
    }

    pub fn find_syntax_by_scope<'a>(&'a self, scope: Scope) -> Option<&'a SyntaxDefinition> {
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

    /// Finds a syntax for plain text, which usually has no highlighting rules.
    /// Good as a fallback when you can't find another syntax but you still want
    /// to use the same highlighting pipeline code.
    ///
    /// # Examples
    /// ```
    /// use syntect::parsing::SyntaxSet;
    /// let mut ss = SyntaxSet::new();
    /// ss.load_plain_text_syntax();
    /// let syntax = ss.find_syntax_by_token("rs").unwrap_or_else(|| ss.find_syntax_plain_text());
    /// assert_eq!(syntax.name, "Plain Text");
    /// ```
    pub fn find_syntax_plain_text<'a>(&'a self) -> &'a SyntaxDefinition {
        self.find_syntax_by_name("Plain Text").expect("All syntax sets ought to have a plain text syntax")
    }

    /// This links all the syntaxes in this set directly with pointers for performance purposes.
    /// It is necessary to do this before parsing anything with these syntaxes.
    /// However, it is not possible to serialize a syntax set that has been linked,
    /// which is why it isn't done by default, except by the load_from_folder constructor.
    /// This operation is idempotent, but takes time even on already linked syntax sets.
    pub fn link_syntaxes(&mut self) {
        for syntax in self.syntaxes.iter() {
            for (_, ref context_ptr) in syntax.contexts.iter() {
                let mut mut_ref = context_ptr.borrow_mut();
                self.link_context(syntax, mut_ref.deref_mut());
            }
        }
        self.is_linked = true;
    }

    fn link_context(&self, syntax: &SyntaxDefinition, context: &mut Context) {
        for pattern in context.patterns.iter_mut() {
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
            Named(ref s) => syntax.contexts.get(s),
            Inline(ref context_ptr) => {
                let mut mut_ref = context_ptr.borrow_mut();
                self.link_context(syntax, mut_ref.deref_mut());
                None
            }
            ByScope { scope, ref sub_context } => {
                let other_syntax = self.find_syntax_by_scope(scope);
                let context_name = sub_context.as_ref().map(|x| &**x).unwrap_or("main");
                other_syntax.and_then(|s| s.contexts.get(context_name))
            }
            File { ref name, ref sub_context } => {
                let other_syntax = self.find_syntax_by_name(name);
                let context_name = sub_context.as_ref().map(|x| &**x).unwrap_or("main");
                other_syntax.and_then(|s| s.contexts.get(context_name))
            }
            Direct(_) => None,
        };
        if let Some(new_context) = maybe_new_context {
            let mut new_ref = Direct(LinkerLink { link: Rc::downgrade(&new_context) });
            mem::swap(context_ref, &mut new_ref);
        }
    }

    fn link_match_pat(&self, syntax: &SyntaxDefinition, match_pat: &mut MatchPattern) {
        let maybe_context_refs = match match_pat.operation {
            MatchOperation::Push(ref mut context_refs) => Some(context_refs),
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

#[cfg(test)]
mod tests {
    use super::*;
    use parsing::{Scope, syntax_definition};
    #[test]
    fn can_load() {
        let mut ps = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        ps.load_plain_text_syntax();
        let rails_scope = Scope::new("source.ruby.rails").unwrap();
        let syntax = ps.find_syntax_by_name("Ruby on Rails").unwrap();
        ps.find_syntax_plain_text();
        assert_eq!(&ps.find_syntax_by_extension("rake").unwrap().name, "Ruby");
        assert_eq!(&ps.find_syntax_by_token("ruby").unwrap().name, "Ruby");
        // println!("{:#?}", syntax);
        assert_eq!(syntax.scope, rails_scope);
        // assert!(false);
        let main_context = syntax.contexts.get("main").unwrap();
        let count = syntax_definition::context_iter(main_context.clone()).count();
        assert_eq!(count, 91);
    }
}
