// see DESIGN.md

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    atoms: Vec<ScopeAtom>
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScopeAtom {
    name: String
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeStack {
    scopes: Vec<Scope>
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeStackOp {
    Push(Scope),
    Pop,
}

impl ScopeStack {
  pub fn new() -> ScopeStack {
    ScopeStack {scopes: Vec::new()}
  }
}
