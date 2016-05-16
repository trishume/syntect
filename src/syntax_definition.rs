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
    first_line_match: Option<Regex>,
    pub hidden: bool,

    variables: HashMap<String, String>,
    contexts: HashMap<String, Context>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Context {
    meta_scope: Option<ScopeElement>,
    meta_content_scope: Option<ScopeElement>,
    meta_include_prototype: bool,

    patterns: Vec<Pattern>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Pattern {
    Match(MatchPattern),
    Include(ContextReference),
}

#[derive(Debug, PartialEq, Eq)]
pub struct MatchPattern {
    regex: Regex,
    scope: Option<ScopeElement>,
    captures: Option<CaptureMapping>,
    operation: MatchOperation,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ContextReference {
    Named(String),
    ByScope {
        name: String,
        sub_context: Option<String>,
    },
    File(String),
    Inline(Box<Context>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum MatchOperation {
    Push(Vec<ContextReference>),
    Set(Vec<ContextReference>),
    Pop,
    None,
}

#[derive(Debug)]
pub enum ParseError {
    InvalidYaml(ScanError),
    EmptyFile,
    MissingMandatoryKey(&'static str),
    TypeMismatch,
}

fn get_key<'a, R, F: FnOnce(&'a Yaml) -> Option<R>>(map: &'a BTreeMap<Yaml, Yaml>,
                                                    key: &'static str,
                                                    f: F)
                                                    -> Result<R, ParseError> {
    map.get(&Yaml::String(key.to_owned()))
        .ok_or(ParseError::MissingMandatoryKey(key))
        .and_then(|x| f(x).ok_or(ParseError::TypeMismatch))
}

impl SyntaxDefinition {
    pub fn load_from_str(s: &str) -> Result<SyntaxDefinition, ParseError> {
        let docs = match YamlLoader::load_from_str(s) {
            Ok(x) => x,
            Err(e) => return Err(ParseError::InvalidYaml(e)),
        };
        if docs.len() == 0 {
            return Err(ParseError::EmptyFile);
        }
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

        let contexts_hash = try!(get_key(h, "contexts", |x| x.as_hash()));
        let contexts = try!(SyntaxDefinition::parse_contexts(contexts_hash, &variables));

        let defn = SyntaxDefinition {
            name: try!(get_key(h, "name", |x| x.as_str())).to_owned(),
            scope: try!(get_key(h, "scope", |x| x.as_str())).to_owned(),
            file_extensions: {
                get_key(h, "file_extensions", |x| x.as_vec())
                    .map(|v| v.iter().filter_map(|y| y.as_str()).map(|x| x.to_owned()).collect())
                    .unwrap_or_else(|_| Vec::new())
            },
            first_line_match: get_key(h, "first_line_match", |x| x.as_str())
                .ok()
                .map(|x| x.to_owned()),
            hidden: get_key(h, "hidden", |x| x.as_bool()).unwrap_or(false),

            variables: variables,
            contexts: contexts,
        };
        Ok(defn)
    }

    fn parse_contexts(map: &BTreeMap<Yaml, Yaml>,
                      variables: &HashMap<String, String>)
                      -> Result<HashMap<String, Context>, ParseError> {
        let mut contexts = HashMap::new();
        for (key, value) in map.iter() {
            if let (Some(name), Some(val_vec)) = (key.as_str(), value.as_vec()) {
                let context = try!(SyntaxDefinition::parse_context(val_vec, variables));
                contexts.insert(name.to_owned(), context);
            }
        }
        return Ok(contexts);
    }

    fn parse_context(vec: &Vec<Yaml>,
                     variables: &HashMap<String, String>)
                     -> Result<Context, ParseError> {
        let mut context = Context {
            meta_scope: None,
            meta_content_scope: None,
            meta_include_prototype: true,
            patterns: Vec::new(),
        };
        for y in vec.iter() {
            let map = try!(y.as_hash().ok_or(ParseError::TypeMismatch));

            if let Some(x) = get_key(map, "meta_scope", |x| x.as_str()).ok() {
                context.meta_scope = Some(x.to_owned());
            } else if let Some(x) = get_key(map, "meta_content_scope", |x| x.as_str()).ok() {
                context.meta_scope = Some(x.to_owned());
            } else if let Some(x) = get_key(map, "meta_include_prototype", |x| x.as_bool()).ok() {
                context.meta_include_prototype = x;
            } else if let Some(x) = get_key(map, "include", |x| Some(x)).ok() {
                let reference = try!(SyntaxDefinition::parse_reference(x, variables));
                context.patterns.push(Pattern::Include(reference));
            } else {
                let pattern = try!(SyntaxDefinition::parse_match_pattern(map, variables));
                context.patterns.push(Pattern::Match(pattern));
            }

        }
        return Ok(context);
    }

    fn parse_reference(y: &Yaml,
                       variables: &HashMap<String, String>)
                       -> Result<ContextReference, ParseError> {
        if let Some(s) = y.as_str() {
            if s.starts_with("scope:") {
                let scope_ref = &s[6..];
                let parts: Vec<&str> = scope_ref.split("#").collect();
                Ok(ContextReference::ByScope {
                    name: parts[0].to_owned(),
                    sub_context: if parts.len() > 0 {
                        Some(parts[1].to_owned())
                    } else {
                        None
                    },
                })
            } else if s.ends_with(".sublime-syntax") {
                Ok(ContextReference::File(s.to_owned()))
            } else {
                Ok(ContextReference::Named(s.to_owned()))
            }
        } else if let Some(v) = y.as_vec() {
            let context = try!(SyntaxDefinition::parse_context(v, variables));
            Ok(ContextReference::Inline(Box::new(context)))
        } else {
            Err(ParseError::TypeMismatch)
        }
    }

    fn parse_match_pattern(map: &BTreeMap<Yaml, Yaml>,
                           variables: &HashMap<String, String>)
                           -> Result<MatchPattern, ParseError> {
        let raw_regex = try!(get_key(map, "match", |x| x.as_str()));
        let regex = raw_regex.to_owned(); // TODO substitute variables, compile with Onigurama
        let scope = get_key(map, "scope", |x| x.as_str()).ok().map(|s| s.to_owned());

        let captures = if let Ok(map) = get_key(map, "captures", |x| x.as_hash()) {
            let mut res_map = HashMap::new();
            for (key, value) in map.iter() {
                if let (Some(key_int), Some(val_str)) = (key.as_i64(), value.as_str()) {
                    res_map.insert(key_int as usize, val_str.to_owned());
                }
            }
            Some(res_map)
        } else {
            None
        };

        let operation = if let Ok(map) = get_key(map, "pop", |x| x.as_bool()) {
            MatchOperation::Pop
        } else if let Ok(y) = get_key(map, "push", |x| Some(x)) {
            MatchOperation::Push(vec![try!(SyntaxDefinition::parse_reference(y, variables))])
        } else if let Ok(y) = get_key(map, "set", |x| Some(x)) {
            MatchOperation::Set(vec![try!(SyntaxDefinition::parse_reference(y, variables))])
            // TODO multi-push and multi-pop
        } else {
            MatchOperation::None
        };

        let pattern = MatchPattern {
            regex: regex,
            scope: scope,
            captures: captures,
            operation: operation, // TODO
        };
        return Ok(pattern);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn can_parse() {
        use syntax_definition::{SyntaxDefinition, Pattern, CaptureMapping, MatchPattern, MatchOperation, ContextReference};
        let defn: SyntaxDefinition =
            SyntaxDefinition::load_from_str("name: C\nscope: source.c\ncontexts: {}").unwrap();
        assert_eq!(defn.name, "C");
        assert_eq!(defn.scope, "source.c");
        let exts_empty: Vec<String> = Vec::new();
        assert_eq!(defn.file_extensions, exts_empty);
        assert_eq!(defn.hidden, false);
        assert!(defn.variables.is_empty());
        let defn2: SyntaxDefinition =
            SyntaxDefinition::load_from_str("
        name: C
        scope: source.c
        file_extensions: [c, h]
        hidden: true
        variables:
          ident: '[A-Za-z_][A-Za-z_0-9]*'
        contexts:
          main:
            - match: \\b(if|else|for|while)\\b
              scope: keyword.control.c
              captures:
                  1: meta.preprocessor.c++
                  2: keyword.control.include.c++
              push: scope:source.c#main
            - match: '\"'
              push: string
          string:
            - meta_scope: string.quoted.double.c
            - match: \\\\.
              scope: constant.character.escape.c
            - match: '\"'
              pop: true
        ")
                .unwrap();
        assert_eq!(defn2.name, "C");
        assert_eq!(defn2.scope, "source.c");
        let exts: Vec<String> = vec![String::from("c"), String::from("h")];
        assert_eq!(defn2.file_extensions, exts);
        assert_eq!(defn2.hidden, true);
        assert_eq!(defn2.variables.get("ident").unwrap(),
                   "[A-Za-z_][A-Za-z_0-9]*");

        let n: Option<String> = None;
        println!("{:?}", defn2);
        // assert!(false);
        assert_eq!(defn2.contexts["main"].meta_scope, n);
        assert_eq!(defn2.contexts["main"].meta_include_prototype, true);
        assert_eq!(defn2.contexts["string"].meta_scope,
                   Some(String::from("string.quoted.double.c")));
        let first_pattern: &Pattern = &defn2.contexts["main"].patterns[0];
        match first_pattern {
            &Pattern::Match(ref match_pat) => {
                let m : &CaptureMapping = match_pat.captures.as_ref().expect("test failed");
                let x : &String = &m[&1];
                assert_eq!(x, "meta.preprocessor.c++");
                assert_eq!(match_pat.operation, MatchOperation::Push(vec![
                    ContextReference::ByScope {
                        name: String::from("source.c"),
                        sub_context: Some(String::from("main"))
                    }]));
            },
            _ => assert!(false)
        }
    }
}
