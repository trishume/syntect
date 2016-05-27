use syntax_definition::*;
use scope::*;
use std::path::Path;
use walkdir::WalkDir;
use std::io::{self, Read};
use std::fs::File;
use walkdir;
use std::ops::DerefMut;
use std::mem;
use std::rc::Rc;

#[derive(Debug)]
pub struct PackageSet {
    pub syntaxes: Vec<SyntaxDefinition>,
    pub scope_repo: ScopeRepository,
}

#[derive(Debug)]
pub enum PackageLoadError {
    WalkDir(walkdir::Error),
    IOErr(io::Error),
    Parsing(ParseError),
}

fn load_syntax_file(p: &Path,
                    scope_repo: &mut ScopeRepository)
                    -> Result<SyntaxDefinition, PackageLoadError> {
    let mut f = try!(File::open(p).map_err(|e| PackageLoadError::IOErr(e)));
    let mut s = String::new();
    try!(f.read_to_string(&mut s).map_err(|e| PackageLoadError::IOErr(e)));

    SyntaxDefinition::load_from_str(&s, scope_repo).map_err(|e| PackageLoadError::Parsing(e))
}

impl PackageSet {
    pub fn load_from_folder<P: AsRef<Path>>(folder: P) -> Result<PackageSet, PackageLoadError> {
        let mut repo = ScopeRepository::new();
        let mut syntaxes = Vec::new();
        for entry in WalkDir::new(folder) {
            let entry = try!(entry.map_err(|e| PackageLoadError::WalkDir(e)));
            if entry.path().extension().map(|e| e == "sublime-syntax").unwrap_or(false) {
                // println!("{}", entry.path().display());
                syntaxes.push(try!(load_syntax_file(entry.path(), &mut repo)));
            }
        }
        let mut ps = PackageSet {
            syntaxes: syntaxes,
            scope_repo: repo,
        };
        ps.link_syntaxes();
        Ok(ps)
    }

    pub fn find_syntax_by_scope<'a>(&'a self, scope: Scope) -> Option<&'a SyntaxDefinition> {
        self.syntaxes.iter().find(|&s| s.scope == scope)
    }

    pub fn find_syntax_by_name<'a>(&'a self, name: &str) -> Option<&'a SyntaxDefinition> {
        self.syntaxes.iter().find(|&s| name == &s.name)
    }

    fn link_syntaxes(&mut self) {
        for syntax in self.syntaxes.iter() {
            for (_, ref context_ptr) in syntax.contexts.iter() {
                let mut mut_ref = context_ptr.borrow_mut();
                self.link_context(syntax, mut_ref.deref_mut());
            }
        }
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
        use syntax_definition::ContextReference::*;
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
            let mut new_ref = Direct(Rc::downgrade(&new_context));
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
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn can_load() {
        use package_set::PackageSet;
        use syntax_definition::*;
        let mut ps = PackageSet::load_from_folder("testdata/Packages").unwrap();
        let rails_scope = ps.scope_repo.build("source.ruby.rails");
        let syntax = ps.find_syntax_by_name("Ruby on Rails").unwrap();
        // println!("{:#?}", syntax);
        assert_eq!(syntax.scope, rails_scope);
        // assert!(false);
        let main_context = syntax.contexts.get("main").unwrap();
        let count = context_iter(main_context.clone()).count();
        assert_eq!(count, 91);
    }
}
