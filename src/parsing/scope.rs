// see DESIGN.md
use std::collections::HashMap;
use std::u16;
use std::sync::Mutex;
use std::fmt;
use std::str::FromStr;
use std::u64;
use rustc_serialize::{Encodable, Encoder, Decodable, Decoder};
use std::cmp::Ordering;

/// Multiplier on the power of 2 for MatchPower.
/// Only useful if you compute your own MatchPower scores.
pub const ATOM_LEN_BITS: u16 = 3;

lazy_static! {
    /// The global scope repo, exposed in case you want to minimize locking and unlocking.
    /// Shouldn't be necessary for you to use. See the `ScopeRepository` docs.
    pub static ref SCOPE_REPO: Mutex<ScopeRepository> = Mutex::new(ScopeRepository::new());
}

/// A hierarchy of atoms with semi-standardized names
/// used to accord semantic information to a specific piece of text.
/// Generally written with the atoms separated by dots.
/// By convention atoms are all lowercase alphanumeric.
///
/// Example scopes: `text.plain`, `punctuation.definition.string.begin.ruby`,
/// `meta.function.parameters.rust`
///
/// `syntect` uses an optimized format for storing these that allows super fast comparison
/// and determining if one scope is a prefix of another. It also always takes 16 bytes of space.
/// It accomplishes this by using a global repository to store string values and using bit-packed
/// 16 bit numbers to represent and compare atoms. Like "atoms" or "symbols" in other languages.
/// This means that while comparing and prefix are fast, extracting a string is relatively slower
/// but ideally should be very rare.
#[derive(Clone, PartialEq, Eq, Copy, Default)]
pub struct Scope {
    a: u64,
    b: u64,
}

/// Not all strings are valid scopes
#[derive(Debug)]
pub enum ParseScopeError {
    /// Due to a limitation of the current optimized internal representation
    /// scopes can be at most 8 atoms long
    TooLong,
    /// The internal representation uses 16 bits per atom, so if all scopes ever
    /// used by the program have more than 2^16-2 atoms, things break
    TooManyAtoms,
}

/// The structure used to keep of the mapping between scope atom numbers
/// and their string names. It is only exposed in case you want to lock
/// `SCOPE_REPO` and then allocate a whole bunch of scopes at once
/// without thrashing the lock. It is recommended you just use `Scope::new()`
///
/// Only `Scope`s created by the same repository have valid comparison results.
#[derive(Debug)]
pub struct ScopeRepository {
    atoms: Vec<String>,
    atom_index_map: HashMap<String, usize>,
}

/// A stack/sequence of scopes. This is used both to represent hierarchies for a given
/// token of text, as well as in `ScopeSelectors`. Press `ctrl+shift+p` in Sublime Text
/// to see the scope stack at a given point.
/// Also see [the TextMate docs](https://manual.macromates.com/en/scope_selectors).
///
/// Example for a JS string inside a script tag in a Rails `ERB` file:
/// `text.html.ruby text.html.basic source.js.embedded.html string.quoted.double.js`
#[derive(Debug, Clone, PartialEq, Eq, Default, RustcEncodable, RustcDecodable)]
pub struct ScopeStack {
    scopes: Vec<Scope>,
}

/// A change to a scope stack. Generally `Noop` is only used internally and you don't have
/// to worry about ever getting one back from a public function.
/// Use `ScopeStack#apply` to apply this change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeStackOp {
    Push(Scope),
    Pop(usize),
    Noop,
}

fn pack_as_u16s(atoms: &[usize]) -> Result<Scope, ParseScopeError> {
    let mut res = Scope { a: 0, b: 0 };

    for (i, &n) in atoms.iter().enumerate() {
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
            s.push_str(self.atom_str(atom_number));
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

        index
    }

    /// Return the string for an atom number returned by `Scope#atom_at`
    pub fn atom_str(&self, atom_number: u16) -> &str {
        &self.atoms[(atom_number - 1) as usize]
    }
}

impl Scope {
    /// Parses a `Scope` from a series of atoms separated by
    /// `.` characters. Example: `Scope::new("meta.rails.controller")`
    pub fn new(s: &str) -> Result<Scope, ParseScopeError> {
        let mut repo = SCOPE_REPO.lock().unwrap();
        repo.build(s.trim())
    }

    /// Gets the atom number at a given index.
    /// I can't think of any reason you'd find this useful.
    /// It is used internally for turning a scope back into a string.
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

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// returns a string representation of this scope, this requires locking a
    /// global repo and shouldn't be done frequently.
    pub fn build_string(self) -> String {
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
    /// use syntect::parsing::Scope;
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

/// Wrapper to get around the fact Rust f64 doesn't implement Ord
/// and there is no non-NaN float type
#[derive(Debug, Copy, Clone, PartialOrd, PartialEq)]
pub struct MatchPower(pub f64);
impl Eq for MatchPower {}
impl Ord for MatchPower {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).unwrap()
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

    /// Modifies this stack according to the operation given
    /// use this to create a stack from a `Vec` of changes
    /// given by the parser.
    pub fn apply(&mut self, op: &ScopeStackOp) {
        match *op {
            ScopeStackOp::Push(scope) => self.scopes.push(scope),
            ScopeStackOp::Pop(count) => {
                for _ in 0..count {
                    self.scopes.pop();
                }
            }
            ScopeStackOp::Noop => (),
        }
    }

    /// Prints out each scope in the stack separated by spaces
    /// and then a newline. Top of the stack at the end.
    pub fn debug_print(&self, repo: &ScopeRepository) {
        for s in &self.scopes {
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
    #[inline]
    pub fn as_slice(&self) -> &[Scope] {
        &self.scopes[..]
    }

    /// Return the height/length of this stack
    #[inline]
    pub fn len(&self) -> usize {
        self.scopes.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// checks if this stack as a selector matches the given stack
    /// if so it returns a match score, higher match scores are stronger
    /// matches. Scores are ordered according to the rules found
    /// at https://manual.macromates.com/en/scope_selectors
    ///
    /// It accomplishes this ordering through some floating point math
    /// ensuring deeper and longer matches matter.
    /// Unfortunately it is only guaranteed to return perfectly accurate results
    /// up to stack depths of 17, but it should be reasonably good even afterwards.
    /// Textmate has the exact same limitation, dunno about Sublime.
    /// # Examples
    /// ```
    /// use syntect::parsing::{ScopeStack, MatchPower};
    /// use std::str::FromStr;
    /// assert_eq!(ScopeStack::from_str("a.b c e.f").unwrap()
    ///     .does_match(ScopeStack::from_str("a.b c.d e.f.g").unwrap().as_slice()),
    ///     Some(MatchPower(0o212u64 as f64)));
    /// assert_eq!(ScopeStack::from_str("a c.d.e").unwrap()
    ///     .does_match(ScopeStack::from_str("a.b c.d e.f.g").unwrap().as_slice()),
    ///     None);
    /// ```
    pub fn does_match(&self, stack: &[Scope]) -> Option<MatchPower> {
        let mut sel_index: usize = 0;
        let mut score: f64 = 0.0;
        for (i, scope) in stack.iter().enumerate() {
            let sel_scope = self.scopes[sel_index];
            if sel_scope.is_prefix_of(*scope) {
                let len = sel_scope.len();
                // equivalent to score |= len << (ATOM_LEN_BITS*i) on a large unsigned
                score += (len as f64) * ((ATOM_LEN_BITS * (i as u16)) as f64).exp2();
                sel_index += 1;
                if sel_index >= self.scopes.len() {
                    return Some(MatchPower(score));
                }
            }
        }
        None
    }
}

impl FromStr for ScopeStack {
    type Err = ParseScopeError;

    /// Parses a scope stack from a whitespace separated list of scopes.
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
    use super::*;
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
        use std::str::FromStr;
        assert_eq!(Scope::new("source.php").unwrap(),
                   Scope::new("source.php").unwrap());
        assert!(Scope::from_str("1.2.3.4.5.6.7.8").is_ok());
        assert!(Scope::from_str("1.2.3.4.5.6.7.8.9").is_err());
    }
    #[test]
    fn prefixes_work() {
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
        use std::str::FromStr;
        assert_eq!(ScopeStack::from_str("string")
                       .unwrap()
                       .does_match(ScopeStack::from_str("string.quoted").unwrap().as_slice()),
                   Some(MatchPower(0o1u64 as f64)));
        assert_eq!(ScopeStack::from_str("source")
                       .unwrap()
                       .does_match(ScopeStack::from_str("string.quoted").unwrap().as_slice()),
                   None);
        assert_eq!(ScopeStack::from_str("a.b e.f")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d e.f.g").unwrap().as_slice()),
                   Some(MatchPower(0o202u64 as f64)));
        assert_eq!(ScopeStack::from_str("c e.f")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d e.f.g").unwrap().as_slice()),
                   Some(MatchPower(0o210u64 as f64)));
        assert_eq!(ScopeStack::from_str("c.d e.f")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d e.f.g").unwrap().as_slice()),
                   Some(MatchPower(0o220u64 as f64)));
        assert_eq!(ScopeStack::from_str("a.b c e.f")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d e.f.g").unwrap().as_slice()),
                   Some(MatchPower(0o212u64 as f64)));
        assert_eq!(ScopeStack::from_str("a c.d")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d e.f.g").unwrap().as_slice()),
                   Some(MatchPower(0o021u64 as f64)));
        assert_eq!(ScopeStack::from_str("a c.d.e")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d e.f.g").unwrap().as_slice()),
                   None);
    }
}
