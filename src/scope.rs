// see DESIGN.md
use std::collections::HashMap;
use std::u16;

#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub struct Scope {
    data: [u16; 8],
}

#[derive(Debug)]
pub struct ScopeRepository {
    atoms: Vec<String>,
    atom_index_map: HashMap<String, usize>,
}

fn pack_as_u16s(atoms: &[usize]) -> [u16; 8] {
    let mut res: [u16; 8] = [0, 0, 0, 0, 0, 0, 0, 0];

    for i in 0..(atoms.len()) {
        let n = atoms[i];
        assert!(n < (u16::MAX as usize) - 2,
                "too many unique scope atoms, there must be less than 2^16-3 for packing reasons");
        let small = (n + 1) as u16; // +1 since we reserve 0 for unused
        res[i] = small;
    }
    res
}

impl ScopeRepository {
    pub fn new() -> ScopeRepository {
        ScopeRepository {
            atoms: Vec::new(),
            atom_index_map: HashMap::new(),
        }
    }

    pub fn build(&mut self, s: &str) -> Scope {
        let parts: Vec<usize> = s.split('.').map(|a| self.atom_to_index(a)).collect();
        assert!(parts.len() <= 8,
                "scope {:?} too long to pack, currently the limit is 8 atoms",
                s);
        Scope { data: pack_as_u16s(&parts[..]) }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeStack {
    scopes: Vec<Scope>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeStackOp {
    Push(Scope),
    Pop,
}

impl ScopeStack {
    pub fn new() -> ScopeStack {
        ScopeStack { scopes: Vec::new() }
    }
    pub fn push(&mut self, s: Scope) {
        self.scopes.push(s);
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
        assert_eq!(repo.build("source.php"), repo.build("source.php"));
        assert_eq!(repo.build("source.php.wow.hi.bob.troll.clock.5"),
                   repo.build("source.php.wow.hi.bob.troll.clock.5"));
        assert_eq!(repo.build(""), repo.build(""));
        let s1 = repo.build("");
        assert_eq!(repo.to_string(s1), "");
        let s2 = repo.build("source.php.wow");
        assert_eq!(repo.to_string(s2), "source.php.wow");
        assert!(repo.build("source.php") != repo.build("source.perl"));
        assert!(repo.build("source.php") != repo.build("source.php.wagon"));
    }
}
