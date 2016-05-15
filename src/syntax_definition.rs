use yaml_rust::{YamlLoader, Yaml, ScanError};
use std::collections::{HashMap, BTreeMap};

pub type Regex = String;
pub type ScopeElement = String;
pub type CaptureMapping = HashMap<usize, ScopeElement>;

#[derive(Debug)]
pub struct SyntaxDefinition {
    pub name: String,
    pub file_extensions: Vec<String>,
    pub scope: ScopeElement,
    pub first_line_match: Option<Regex>,
    pub hidden: bool,

    pub variables: HashMap<String, String>,
    pub contexts: HashMap<String, Context>
}

#[derive(Debug)]
pub struct Context {
    meta_scope: ScopeElement,
    meta_content_scope: ScopeElement,
    meta_include_prototype: bool,

    patterns: Vec<Pattern>
}

#[derive(Debug)]
pub enum Pattern {
  Match(MatchPattern),
  Include(ContextReference)
}

#[derive(Debug)]
pub struct MatchPattern {
    regex: Regex,
    scope: Option<ScopeElement>,
    captures: Option<CaptureMapping>,
    operation: MatchOperation
}

#[derive(Debug)]
pub enum ContextReference {
  Named(String),
  ByScope {name: String, sub_context: Option<String>},
  File(String),
  Inline(Box<Context>)
}

#[derive(Debug)]
pub enum MatchOperation {
    Push(Vec<ContextReference>),
    Set(Vec<ContextReference>),
    Pop,
    None
}

#[derive(Debug)]
pub enum ParseError {
  InvalidYaml(ScanError),
  EmptyFile,
  MissingMandatoryKey(&'static str),
  TypeMismatch
}

fn get_key<'a, R, F: FnOnce(&'a Yaml) -> Option<R>>
  (map: &'a BTreeMap<Yaml, Yaml>, key: &'static str, f: F) -> Result<R, ParseError> {
  map.get(&Yaml::String(key.to_owned()))
     .ok_or(ParseError::MissingMandatoryKey(key))
     .and_then(|x| f(x).ok_or(ParseError::TypeMismatch))
}

impl SyntaxDefinition {
  pub fn load_from_str(s: &str) -> Result<SyntaxDefinition, ParseError> {
    let docs = match YamlLoader::load_from_str(s) {
      Ok(x) => x,
      Err(e) => return Err(ParseError::InvalidYaml(e))
    };
    if docs.len() == 0 { return Err(ParseError::EmptyFile) }
    let doc = &docs[0];
    SyntaxDefinition::parse_top_level(doc)
  }

  fn parse_top_level(doc: &Yaml) -> Result<SyntaxDefinition, ParseError> {
    let h = try!(doc.as_hash().ok_or(ParseError::TypeMismatch));

    let mut variables = HashMap::new();
    if let Ok(map) = get_key(h, "variables", |x| x.as_hash()) {
      for (key, value) in map.iter() {
        if let (Some(key_str), Some(val_str)) = (key.as_str(), value.as_str()) {
          variables.insert(key_str.to_owned(), val_str.to_owned());
        }
      }
    }

    let defn = SyntaxDefinition {
      name: try!(get_key(h, "name", |x| x.as_str())).to_owned(),
      scope: try!(get_key(h, "scope", |x| x.as_str())).to_owned(),
      file_extensions: {
        get_key(h, "file_extensions", |x| x.as_vec()).map(|v|
          v.iter().filter_map(|y| y.as_str()).map(|x| x.to_owned()).collect()
          ).unwrap_or_else(|_| Vec::new())
      },
      first_line_match: get_key(h, "first_line_match", |x| x.as_str()).ok().map(|x| x.to_owned()),
      hidden: get_key(h, "hidden", |x| x.as_bool()).unwrap_or(false),

      variables: variables,
      contexts: HashMap::new(), // TODO
    };
    Ok(defn)
  }
}
