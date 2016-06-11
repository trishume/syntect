// see DESIGN.md
use std::collections::HashMap;
use std::u16;
use std::sync::Mutex;
use std::fmt;
use std::str::FromStr;
use std::u64;
use rustc_serialize::{Encodable, Encoder, Decodable, Decoder};

lazy_static! {
    pub static ref SCOPE_REPO: Mutex<ScopeRepository> = Mutex::new(ScopeRepository::new());
}

#[derive(Clone, PartialEq, Eq, Copy, Default)]
pub struct Scope {
    a: u64,
    b: u64,
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScopeStack {
    scopes: Vec<Scope>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeStackOp {
    Push(Scope),
    Pop(usize),
    Noop,
}

fn pack_as_u16s(atoms: &[usize]) -> Result<Scope, ParseScopeError> {
    let mut res = Scope { a: 0, b: 0 };

    for i in 0..(atoms.len()) {
        let n = atoms[i];
        if n >= (u16::MAX as usize) - 2 {
            return Err(ParseScopeError::TooManyAtoms);
        }
        let small = n + 1; // +1 since we reserve 0 for unused

        if i < 4 {
            let shift = (3 - i) * 16;
            res.a |= (small << shift) as u64;
        } else {
            let shift = (7 - i) * 16;
            res.b |= (small << shift) as u64;
        }
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
        if s.is_empty() {
            return Ok(Scope { a: 0, b: 0 });
        }
        let parts: Vec<usize> = s.split('.').map(|a| self.atom_to_index(a)).collect();
        if parts.len() > 8 {
            return Err(ParseScopeError::TooManyAtoms);
        }
        pack_as_u16s(&parts[..])
    }

    pub fn to_string(&self, scope: Scope) -> String {
        let mut s = String::new();
        for i in 0..8 {
            let atom_number = scope.atom_at(i);
            // println!("atom {} of {:x}-{:x} = {:x}",
            //     i, scope.a, scope.b, atom_number);
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

    pub fn atom_at(self, index: usize) -> u16 {
        let shifted = if index < 4 {
            (self.a >> ((3 - index) * 16))
        } else if index < 8 {
            (self.b >> ((7 - index) * 16))
        } else {
            panic!("atom index out of bounds {:?}", index);
        };
        (shifted & 0xFFFF) as u16
    }

    #[inline]
    fn missing_atoms(self) -> u32 {
        let trail = if self.b == 0 {
            self.a.trailing_zeros() + 64
        } else {
            self.b.trailing_zeros()
        };
        trail / 16
    }

    /// return the number of atoms in the scope
    #[inline(always)]
    pub fn len(self) -> u32 {
        8 - self.missing_atoms()
    }

    /// returns a string representation of this scope, this requires locking a
    /// global repo and shouldn't be done frequently.
    fn build_string(self) -> String {
        let repo = SCOPE_REPO.lock().unwrap();
        repo.to_string(self)
    }

    /// Tests if this scope is a prefix of another scope.
    /// Note that the empty scope is always a prefix.
    ///
    /// This operation uses bitwise operations and is very fast
    /// # Examples
    ///
    /// ```
    /// use syntect::scope::Scope;
    /// assert!( Scope::new("string").unwrap()
    ///         .is_prefix_of(Scope::new("string.quoted").unwrap()));
    /// assert!( Scope::new("string.quoted").unwrap()
    ///         .is_prefix_of(Scope::new("string.quoted").unwrap()));
    /// assert!( Scope::new("").unwrap()
    ///         .is_prefix_of(Scope::new("meta.rails.controller").unwrap()));
    /// assert!(!Scope::new("source.php").unwrap()
    ///         .is_prefix_of(Scope::new("source").unwrap()));
    /// assert!(!Scope::new("source.php").unwrap()
    ///         .is_prefix_of(Scope::new("source.ruby").unwrap()));
    /// assert!(!Scope::new("meta.php").unwrap()
    ///         .is_prefix_of(Scope::new("source.php").unwrap()));
    /// assert!(!Scope::new("meta.php").unwrap()
    ///         .is_prefix_of(Scope::new("source.php.wow").unwrap()));
    /// ```
    pub fn is_prefix_of(self, s: Scope) -> bool {
        let pref_missing = self.missing_atoms();

        // TODO: test optimization - use checked shl and then mult carry flag as int by -1
        let mask: (u64, u64) = if pref_missing == 8 {
            (0, 0)
        } else if pref_missing == 4 {
            (u64::MAX, 0)
        } else if pref_missing > 4 {
            (u64::MAX << ((pref_missing - 4) * 16), 0)
        } else {
            (u64::MAX, u64::MAX << (pref_missing * 16))
        };

        // xor to find the difference
        let ax = (self.a ^ s.a) & mask.0;
        let bx = (self.b ^ s.b) & mask.1;
        // println!("{:x}-{:x} is_pref {:x}-{:x}: missing {} mask {:x}-{:x} xor {:x}-{:x}",
        //     self.a, self.b, s.a, s.b, pref_missing, mask.0, mask.1, ax, bx);

        ax == 0 && bx == 0
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
        let s = self.build_string();
        write!(f, "{}", s)
    }
}

impl fmt::Debug for Scope {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = self.build_string();
        write!(f, "<{}>", s)
    }
}

impl Encodable for Scope {
    fn encode<S: Encoder>(&self, s: &mut S) -> Result<(), S::Error> {
        let st = self.build_string();
        s.emit_str(&st)
    }
}

impl Decodable for Scope {
    fn decode<D: Decoder>(d: &mut D) -> Result<Scope, D::Error> {
        let s: String = try!(d.read_str());
        Ok(Scope::new(&s).unwrap())
    }
}

impl ScopeStack {
    pub fn new() -> ScopeStack {
        ScopeStack { scopes: Vec::new() }
    }
    pub fn from_vec(v: Vec<Scope>) -> ScopeStack {
        ScopeStack { scopes: v }
    }
    pub fn push(&mut self, s: Scope) {
        self.scopes.push(s);
    }
    pub fn pop(&mut self) {
        self.scopes.pop();
    }
    pub fn apply(&mut self, op: &ScopeStackOp) {
        match op {
            &ScopeStackOp::Push(scope) => self.scopes.push(scope),
            &ScopeStackOp::Pop(count) => {
                for _ in 0..count {
                    self.scopes.pop();
                }
            }
            &ScopeStackOp::Noop => (),
        }
    }
    pub fn debug_print(&self, repo: &ScopeRepository) {
        for s in self.scopes.iter() {
            print!("{} ", repo.to_string(*s));
        }
        println!("");
    }

    /// Return the bottom n elements of the stack.
    /// Equivalent to &scopes[0..n] on a Vec
    pub fn bottom_n(&self, n: usize) -> &[Scope] {
        &self.scopes[0..n]
    }

    /// Return a slice of the scopes in this stack
    pub fn as_slice(&self) -> &[Scope] {
        &self.scopes[..]
    }

    /// Return the height/length of this stack
    pub fn len(&self) -> usize {
        self.scopes.len()
    }

    /// checks if this stack as a selector matches the given stack
    /// if so it returns a match score, higher match scores are stronger
    /// matches. Scores are ordered according to the rules found
    /// at https://manual.macromates.com/en/scope_selectors
    ///
    /// It accomplishes this ordering through some bit manipulation
    /// ensuring deeper and longer matches matter.
    /// Unfortunately it currently will only return reasonable results
    /// up to stack depths of 16.
    /// # Examples
    /// ```
    /// use syntect::scope::ScopeStack;
    /// use std::str::FromStr;
    /// assert_eq!(ScopeStack::from_str("a.b c e.f").unwrap()
    ///     .does_match(ScopeStack::from_str("a.b c.d e.f.g").unwrap().as_slice()),
    ///     Some(0x212));
    /// assert_eq!(ScopeStack::from_str("a c.d.e").unwrap()
    ///     .does_match(ScopeStack::from_str("a.b c.d e.f.g").unwrap().as_slice()),
    ///     None);
    /// ```
    pub fn does_match(&self, stack: &[Scope]) -> Option<u64> {
        const ATOM_LEN_BITS: u16 = 4;
        let mut sel_index: usize = 0;
        let mut score: u64 = 0;
        for (i, scope) in stack.iter().enumerate() {
            let sel_scope = self.scopes[sel_index];
            if sel_scope.is_prefix_of(*scope) {
                let len = sel_scope.len();
                // TODO this breaks on stacks larger than 16 things, maybe use doubles?
                score |= (len as u64) << (ATOM_LEN_BITS * (i as u16));
                sel_index += 1;
                if sel_index >= self.scopes.len() {
                    return Some(score);
                }
            }
        }
        None
    }
}

impl FromStr for ScopeStack {
    type Err = ParseScopeError;

    fn from_str(s: &str) -> Result<ScopeStack, ParseScopeError> {
        let mut scopes = Vec::new();
        for name in s.split_whitespace() {
            scopes.push(try!(Scope::from_str(name)))
        }
        Ok(ScopeStack { scopes: scopes })
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
        assert_eq!(repo.build("source.php").unwrap(),
                   repo.build("source.php").unwrap());
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
        assert_eq!(Scope::new("source.php").unwrap(),
                   Scope::new("source.php").unwrap());
        assert!(Scope::from_str("1.2.3.4.5.6.7.8").is_ok());
        assert!(Scope::from_str("1.2.3.4.5.6.7.8.9").is_err());
    }
    #[test]
    fn prefixes_work() {
        use scope::Scope;
        assert!(Scope::new("1.2.3.4.5.6.7.8")
            .unwrap()
            .is_prefix_of(Scope::new("1.2.3.4.5.6.7.8").unwrap()));
        assert!(Scope::new("1.2.3.4.5.6")
            .unwrap()
            .is_prefix_of(Scope::new("1.2.3.4.5.6.7.8").unwrap()));
        assert!(Scope::new("1.2.3.4")
            .unwrap()
            .is_prefix_of(Scope::new("1.2.3.4.5.6.7.8").unwrap()));
        assert!(!Scope::new("1.2.3.4.5.6.a")
            .unwrap()
            .is_prefix_of(Scope::new("1.2.3.4.5.6.7.8").unwrap()));
        assert!(!Scope::new("1.2.a.4.5.6.7")
            .unwrap()
            .is_prefix_of(Scope::new("1.2.3.4.5.6.7.8").unwrap()));
        assert!(!Scope::new("1.2.a.4.5.6.7")
            .unwrap()
            .is_prefix_of(Scope::new("1.2.3.4.5").unwrap()));
        assert!(!Scope::new("1.2.a")
            .unwrap()
            .is_prefix_of(Scope::new("1.2.3.4.5.6.7.8").unwrap()));
    }
    #[test]
    fn matching_works() {
        use scope::*;
        use std::str::FromStr;
        assert_eq!(ScopeStack::from_str("string")
                       .unwrap()
                       .does_match(ScopeStack::from_str("string.quoted").unwrap().as_slice()),
                   Some(1));
        assert_eq!(ScopeStack::from_str("source")
                       .unwrap()
                       .does_match(ScopeStack::from_str("string.quoted").unwrap().as_slice()),
                   None);
        assert_eq!(ScopeStack::from_str("a.b e.f")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d e.f.g").unwrap().as_slice()),
                   Some(0x202));
        assert_eq!(ScopeStack::from_str("c e.f")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d e.f.g").unwrap().as_slice()),
                   Some(0x210));
        assert_eq!(ScopeStack::from_str("c.d e.f")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d e.f.g").unwrap().as_slice()),
                   Some(0x220));
        assert_eq!(ScopeStack::from_str("a.b c e.f")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d e.f.g").unwrap().as_slice()),
                   Some(0x212));
        assert_eq!(ScopeStack::from_str("a c.d")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d e.f.g").unwrap().as_slice()),
                   Some(0x021));
        assert_eq!(ScopeStack::from_str("a c.d.e")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d e.f.g").unwrap().as_slice()),
                   None);
    }
}
