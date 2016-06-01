use syntax_definition::*;
use scope::*;
use yaml_load::*;
use theme::theme::{Theme, ParseThemeError};
use theme::settings::*;

use std::path::{Path, PathBuf};
use std::io::{Error as IoError, BufReader};
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
}

#[derive(Debug)]
pub enum PackageError {
    WalkDir(walkdir::Error),
    Io(io::Error),
    ParseSyntax(ParseSyntaxError),
    ParseTheme(ParseThemeError),
    ReadSettings(SettingsError),
}

impl From<SettingsError> for PackageError {
    fn from(error: SettingsError) -> PackageError {
        PackageError::ReadSettings(error)
    }
}

impl From<IoError> for PackageError {
    fn from(error: IoError) -> PackageError {
        PackageError::Io(error)
    }
}

impl From<ParseThemeError> for PackageError {
    fn from(error: ParseThemeError) -> PackageError {
        PackageError::ParseTheme(error)
    }
}

impl From<ParseSyntaxError> for PackageError {
    fn from(error: ParseSyntaxError) -> PackageError {
        PackageError::ParseSyntax(error)
    }
}

fn load_syntax_file(p: &Path)
                    -> Result<SyntaxDefinition, PackageError> {
    let mut f = try!(File::open(p));
    let mut s = String::new();
    try!(f.read_to_string(&mut s));

    Ok(try!(SyntaxDefinition::load_from_str(&s)))
}

impl PackageSet {
    pub fn load_from_folder<P: AsRef<Path>>(folder: P) -> Result<PackageSet, PackageError> {
        let mut syntaxes = Vec::new();
        for entry in WalkDir::new(folder) {
            let entry = try!(entry.map_err(|e| PackageError::WalkDir(e)));
            if entry.path().extension().map(|e| e == "sublime-syntax").unwrap_or(false) {
                // println!("{}", entry.path().display());
                syntaxes.push(try!(load_syntax_file(entry.path())));
            }
        }
        let mut ps = PackageSet {
            syntaxes: syntaxes,
        };
        ps.link_syntaxes();
        Ok(ps)
    }

    pub fn discover_themes<P: AsRef<Path>>(folder: P) -> Result<Vec<PathBuf>, PackageError> {
        let mut themes = Vec::new();
        for entry in WalkDir::new(folder) {
            let entry = try!(entry.map_err(|e| PackageError::WalkDir(e)));
            if entry.path().extension().map(|e| e == "tmTheme").unwrap_or(false) {
                themes.push(entry.path().to_owned());
            }
        }
        Ok(themes)
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
        if let Some(ref context_ptr) = match_pat.with_prototype {
            let mut mut_ref = context_ptr.borrow_mut();
            self.link_context(syntax, mut_ref.deref_mut());
        }
    }

    fn read_file(path: &Path) -> Result<BufReader<File>, PackageError> {
        let reader = try!(File::open(path));
        Ok(BufReader::new(reader))
    }

    fn read_plist(path: &Path) -> Result<Settings, PackageError> {
        Ok(try!(read_plist(try!(Self::read_file(path)))))
    }

    pub fn get_theme<P: AsRef<Path>>(path: P) -> Result<Theme, PackageError> {
        Ok(try!(Theme::parse_settings(try!(Self::read_plist(path.as_ref())))))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn can_load() {
        use package_set::PackageSet;
        use syntax_definition::*;
        use scope::*;
        let ps = PackageSet::load_from_folder("testdata/Packages").unwrap();
        let rails_scope = Scope::new("source.ruby.rails").unwrap();
        let syntax = ps.find_syntax_by_name("Ruby on Rails").unwrap();
        // println!("{:#?}", syntax);
        assert_eq!(syntax.scope, rails_scope);
        // assert!(false);
        let main_context = syntax.contexts.get("main").unwrap();
        let count = context_iter(main_context.clone()).count();
        assert_eq!(count, 91);
    }
    #[test]
    fn can_parse_common_themes() {
        use package_set::PackageSet;
        let theme_paths = PackageSet::discover_themes("testdata/themes.tmbundle").unwrap();
        for theme_path in theme_paths.iter() {
            println!("{:?}", theme_path);
            if theme_path.ends_with("8-Colour-Dark.tmTheme") { continue; }
            let theme = PackageSet::get_theme(theme_path).unwrap();
            println!("{:?}", theme.name);
        }
        // assert!(false);
    }
}
