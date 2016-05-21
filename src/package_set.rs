use syntax_definition::{SyntaxDefinition, ParseError};
use scope::ScopeRepository;
use std::path::Path;
use walkdir::WalkDir;
use std::io::{Read, self};
use std::fs::File;
use walkdir;

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

fn load_syntax_file(p: &Path, scope_repo: &mut ScopeRepository) -> Result<SyntaxDefinition, PackageLoadError> {
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
                println!("{}", entry.path().display());
                syntaxes.push(try!(load_syntax_file(entry.path(), &mut repo)));
            }
        }
        Ok(PackageSet {
            syntaxes: syntaxes,
            scope_repo: repo,
        })
    }
}


#[cfg(test)]
mod tests {
    #[test]
    fn can_load() {
        use scope::Scope;
        use package_set::{PackageSet};
        let mut ps = PackageSet::load_from_folder("testdata/Packages").unwrap();
        let actionscript = ps.syntaxes.iter().find(|s| s.name == "ActionScript").unwrap();
        // println!("{:#?}", actionscript);
        assert_eq!(actionscript.scope, ps.scope_repo.build("source.actionscript.2"));
        // assert!(false);
    }
}
