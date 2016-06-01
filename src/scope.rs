// see DESIGN.md
use std::collections::HashMap;
use std::u16;
use std::sync::Mutex;
use std::fmt;
use std::str::FromStr;

lazy_static! {
    pub static ref SCOPE_REPO: Mutex<ScopeRepository> = Mutex::new(ScopeRepository::new());
}

#[derive(Clone, PartialEq, Eq, Copy)]
pub struct Scope {
    data: [u16; 8],
}

#[derive(Debug)]
pub enum ParseScopeError {
    /// Due to a limitation of the current optimized internal representation
    /// scopes can be at most 8 atoms long
    TooLong,
    /// The internal representation uses 16 bits per atom, so if all scopes ever
    /// used by the program have more than 2^16-2 atoms, things break
    TooManyAtoms,
}

#[derive(Debug)]
pub struct ScopeRepository {
    atoms: Vec<String>,
    atom_index_map: HashMap<String, usize>,
}

fn pack_as_u16s(atoms: &[usize]) -> Result<[u16; 8],ParseScopeError> {
    let mut res: [u16; 8] = [0, 0, 0, 0, 0, 0, 0, 0];

    for i in 0..(atoms.len()) {
        let n = atoms[i];
        if n >= (u16::MAX as usize) - 2 {
            return Err(ParseScopeError::TooManyAtoms);
        }
        let small = (n + 1) as u16; // +1 since we reserve 0 for unused
        res[i] = small;
    }
    Ok(res)
}

impl ScopeRepository {
    fn new() -> ScopeRepository {
        ScopeRepository {
            atoms: Vec::new(),
            atom_index_map: HashMap::new(),
        }
    }

    pub fn build(&mut self, s: &str) -> Result<Scope, ParseScopeError> {
        let parts: Vec<usize> = s.split('.').map(|a| self.atom_to_index(a)).collect();
        if parts.len() > 8 {
            return Err(ParseScopeError::TooManyAtoms);
        }
        Ok(Scope { data: try!(pack_as_u16s(&parts[..])) })
    }

    pub fn to_string(&self, scope: Scope) -> String {
        let mut s = String::new();
        for i in 0..8 {
            let atom_number = scope.data[i];
            if atom_number == 0 {
                break;
            }
            if i != 0 {
                s.push_str(".");
            }
            s.push_str(&self.atoms[(atom_number - 1) as usize]);
        }
        s
    }

    fn atom_to_index(&mut self, atom: &str) -> usize {
        if let Some(index) = self.atom_index_map.get(atom) {
            return *index;
        }
        self.atoms.push(atom.to_owned());
        let index = self.atoms.len() - 1;
        self.atom_index_map.insert(atom.to_owned(), index);
        return index;
    }
}

impl Scope {
    pub fn new(s: &str) -> Result<Scope, ParseScopeError> {
        let mut repo = SCOPE_REPO.lock().unwrap();
        repo.build(s.trim())
    }
}

impl FromStr for Scope {
    type Err = ParseScopeError;

    fn from_str(s: &str) -> Result<Scope, ParseScopeError> {
        Scope::new(s)
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let repo = SCOPE_REPO.lock().unwrap();
        let s = repo.to_string(*self);
        write!(f, "{}", s)
    }
}

impl fmt::Debug for Scope {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let repo = SCOPE_REPO.lock().unwrap();
        let s = repo.to_string(*self);
        write!(f, "<{}>", s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeStack {
    scopes: Vec<Scope>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeStackOp {
    Push(Scope),
    Pop(usize),
}

impl ScopeStack {
    pub fn new() -> ScopeStack {
        ScopeStack { scopes: Vec::new() }
    }
    pub fn push(&mut self, s: Scope) {
        self.scopes.push(s);
    }
    pub fn apply(&mut self, op: &ScopeStackOp) {
        match op {
            &ScopeStackOp::Push(scope) => self.scopes.push(scope),
            &ScopeStackOp::Pop(count) => {
                for _ in 0..count {
                    self.scopes.pop();
                }
            }
        }
    }
    pub fn debug_print(&self, repo: &ScopeRepository) {
        for s in self.scopes.iter() {
            print!("{} ", repo.to_string(*s));
        }
        println!("");
    }
}

impl FromStr for ScopeStack {
    type Err = ParseScopeError;

    fn from_str(s: &str) -> Result<ScopeStack, ParseScopeError> {
        let mut scopes = Vec::new();
        for name in s.split_whitespace() {
            scopes.push(try!(Scope::from_str(name)))
        };
        Ok(ScopeStack {scopes: scopes})
    }
}

impl fmt::Display for ScopeStack {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for s in self.scopes.iter() {
            try!(write!(f, "{} ", s));
        }
        Ok(())
    }
}

// The following code (until the tests module at the end)
// is based on code from https://github.com/defuz/sublimate/blob/master/src/core/syntax/scope.rs
// under the MIT license

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeSelector {
    path: ScopeStack,
    exclude: Option<ScopeStack>
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeSelectors {
    pub selectors: Vec<ScopeSelector>
}

impl FromStr for ScopeSelector {
    type Err = ParseScopeError;

    fn from_str(s: &str) -> Result<ScopeSelector, ParseScopeError> {
        match s.find(" - ") {
            Some(index) => {
                let (path_str, exclude_with_dash) = s.split_at(index);
                let exclude_str = &exclude_with_dash[3..];
                Ok(ScopeSelector {
                    path: try!(ScopeStack::from_str(path_str)),
                    exclude: Some(try!(ScopeStack::from_str(exclude_str))),
                })
            },
            None => {
                Ok(ScopeSelector {
                    path: try!(ScopeStack::from_str(s)),
                    exclude: None,
                })
            }
        }
    }
}

impl FromStr for ScopeSelectors {
    type Err = ParseScopeError;

    fn from_str(s: &str) -> Result<ScopeSelectors, ParseScopeError> {
        let mut selectors = Vec::new();
        for selector in s.split(',') {
            selectors.push(try!(ScopeSelector::from_str(selector)))
        };
        Ok(ScopeSelectors {
            selectors: selectors
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn misc() {
        // use std::mem;
        // use std::rc::{Rc};
        // use scope::*;
        // assert_eq!(8, mem::size_of::<Rc<Scope>>());
        // assert_eq!(Scope::new("source.php"), Scope::new("source.php"));
    }
    #[test]
    fn repo_works() {
        use scope::*;
        let mut repo = ScopeRepository::new();
        assert_eq!(repo.build("source.php").unwrap(), repo.build("source.php").unwrap());
        assert_eq!(repo.build("source.php.wow.hi.bob.troll.clock.5").unwrap(),
                   repo.build("source.php.wow.hi.bob.troll.clock.5").unwrap());
        assert_eq!(repo.build("").unwrap(), repo.build("").unwrap());
        let s1 = repo.build("").unwrap();
        assert_eq!(repo.to_string(s1), "");
        let s2 = repo.build("source.php.wow").unwrap();
        assert_eq!(repo.to_string(s2), "source.php.wow");
        assert!(repo.build("source.php").unwrap() != repo.build("source.perl").unwrap());
        assert!(repo.build("source.php").unwrap() != repo.build("source.php.wagon").unwrap());
    }
    #[test]
    fn global_repo_works() {
        use scope::*;
        use std::str::FromStr;
        assert_eq!(Scope::new("source.php").unwrap(), Scope::new("source.php").unwrap());
        assert!(Scope::from_str("1.2.3.4.5.6.7.8").is_ok());
        assert!(Scope::from_str("1.2.3.4.5.6.7.8.9").is_err());
    }
    #[test]
    fn selectors_work() {
        use scope::*;
        use std::str::FromStr;
        let sels = ScopeSelectors::from_str("source.php meta.preprocessor - string.quoted, source string").unwrap();
        assert_eq!(sels.selectors.len(), 2);
        let first_sel = &sels.selectors[0];
        assert_eq!(format!("{:?}", first_sel),
            "ScopeSelector { path: ScopeStack { scopes: [<source.php>, <meta.preprocessor>] }, exclude: Some(ScopeStack { scopes: [<string.quoted>] }) }");
    }
}
