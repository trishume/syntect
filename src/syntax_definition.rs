use yaml_rust::{YamlLoader, Yaml, ScanError};
use std::collections::{HashMap, BTreeMap};
use onig::{Regex, Captures, Syntax, self};
use std::rc::Rc;
use std::cell::{RefCell};

pub type ScopeElement = String;
pub type CaptureMapping = HashMap<usize, ScopeElement>;
pub type ContextPtr = Rc<RefCell<Context>>;

#[derive(Debug)]
pub struct SyntaxDefinition {
    pub name: String,
    pub file_extensions: Vec<String>,
    pub scope: ScopeElement,
    pub first_line_match: Option<Regex>,
    pub hidden: bool,

    pub variables: HashMap<String, String>,
    pub contexts: HashMap<String, ContextPtr>,
}

#[derive(Debug)]
pub struct Context {
    pub meta_scope: Option<ScopeElement>,
    pub meta_content_scope: Option<ScopeElement>,
    pub meta_include_prototype: bool,

    pub patterns: Vec<Pattern>,
}

#[derive(Debug)]
pub enum Pattern {
    Match(MatchPattern),
    Include(ContextReference),
}

#[derive(Debug)]
pub struct MatchPattern {
    pub regex_str: String,
    // present unless contains backrefs and has to be dynamically compiled
    pub regex: Option<Regex>,
    pub scope: Vec<ScopeElement>,
    pub captures: Option<CaptureMapping>,
    pub operation: MatchOperation,
}

#[derive(Debug)]
pub enum ContextReference {
    Named(String),
    ByScope {
        name: String,
        sub_context: Option<String>,
    },
    File {
        name: String,
        sub_context: Option<String>,
    },
    Direct(ContextPtr),
}

#[derive(Debug)]
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
    RegexCompileError(onig::Error),
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

struct ParserState {
    variables: HashMap<String, String>,
    variable_regex: Regex,
    backref_regex: Regex,
    short_multibyte_regex: Regex,
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
        let state = ParserState {
            variables: variables,
            variable_regex: Regex::new(r"\{\{([A-Za-z0-9_]+)\}\}").unwrap(),
            backref_regex: Regex::new(r"\\\d").unwrap(),
            short_multibyte_regex: Regex::new(r"\\x([a-fA-F][a-fA-F0-9])").unwrap(),
        };

        let contexts_hash = try!(get_key(h, "contexts", |x| x.as_hash()));
        let contexts = try!(SyntaxDefinition::parse_contexts(contexts_hash, &state));

        let defn = SyntaxDefinition {
            name: try!(get_key(h, "name", |x| x.as_str())).to_owned(),
            scope: try!(get_key(h, "scope", |x| x.as_str())).to_owned(),
            file_extensions: {
                get_key(h, "file_extensions", |x| x.as_vec())
                    .map(|v| v.iter().filter_map(|y| y.as_str()).map(|x| x.to_owned()).collect())
                    .unwrap_or_else(|_| Vec::new())
            },
            first_line_match: if let Ok(s) = get_key(h, "first_line_match", |x| x.as_str()) {
                Some(try!(Regex::new(s).map_err(|e| ParseError::RegexCompileError(e))))
            } else {
                None
            },
            hidden: get_key(h, "hidden", |x| x.as_bool()).unwrap_or(false),

            variables: state.variables.clone(),
            contexts: contexts,
        };
        Ok(defn)
    }

    fn parse_contexts(map: &BTreeMap<Yaml, Yaml>,
                      state: &ParserState)
                      -> Result<HashMap<String, ContextPtr>, ParseError> {
        let mut contexts = HashMap::new();
        for (key, value) in map.iter() {
            if let (Some(name), Some(val_vec)) = (key.as_str(), value.as_vec()) {
                let context = try!(SyntaxDefinition::parse_context(val_vec, state));
                contexts.insert(name.to_owned(), context);
            }
        }
        return Ok(contexts);
    }

    fn parse_context(vec: &Vec<Yaml>,
                     state: &ParserState)
                     -> Result<ContextPtr, ParseError> {
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
                let reference = try!(SyntaxDefinition::parse_reference(x, state));
                context.patterns.push(Pattern::Include(reference));
            } else {
                let pattern = try!(SyntaxDefinition::parse_match_pattern(map, state));
                context.patterns.push(Pattern::Match(pattern));
            }

        }
        return Ok(Rc::new(RefCell::new(context)));
    }

    fn parse_reference(y: &Yaml,
                       state: &ParserState)
                       -> Result<ContextReference, ParseError> {
        if let Some(s) = y.as_str() {
            let parts: Vec<&str> = s.split("#").collect();
            let sub_context = if parts.len() > 1 {
                Some(parts[1].to_owned())
            } else {
                None
            };
            if parts[0].starts_with("scope:") {
                Ok(ContextReference::ByScope {
                    name: parts[0][6..].to_owned(),
                    sub_context: sub_context,
                })
            } else if parts[0].ends_with(".sublime-syntax") {
                Ok(ContextReference::File {
                    name: s.to_owned(),
                    sub_context: sub_context,
                })
            } else {
                Ok(ContextReference::Named(parts[0].to_owned()))
            }
        } else if let Some(v) = y.as_vec() {
            let context = try!(SyntaxDefinition::parse_context(v, state));
            Ok(ContextReference::Direct(context))
        } else {
            Err(ParseError::TypeMismatch)
        }
    }

    fn parse_match_pattern(map: &BTreeMap<Yaml, Yaml>,
                           state: &ParserState)
                           -> Result<MatchPattern, ParseError> {
        let raw_regex = try!(get_key(map, "match", |x| x.as_str()));
        let regex_str_1 = state.variable_regex.replace_all(raw_regex, |caps: &Captures| {
            state.variables.get(caps.at(1).unwrap_or("")).map(|x| &**x).unwrap_or("").to_owned()
        });
        // bug triggered by CSS.sublime-syntax, dunno why this is necessary
        let regex_str = state.short_multibyte_regex.replace_all(&regex_str_1, |caps: &Captures| {
            format!("\\x{{000000{}}}", caps.at(1).unwrap_or(""))
        });
        // println!("{:?}", regex_str);

        // if it contains back references we can't resolve it until runtime
        let regex = if state.backref_regex.find(&regex_str).is_some() {
            None
        } else {
            Some(try!(Regex::with_options(&regex_str, onig::REGEX_OPTION_CAPTURE_GROUP, Syntax::default()).map_err(|e| ParseError::RegexCompileError(e))))
        };

        let scope = get_key(map, "scope", |x| x.as_str()).ok().map(|s| s.split_whitespace().map(|scope| scope.to_owned()).collect()).unwrap_or_else(|| vec![]);

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

        let operation = if let Ok(_) = get_key(map, "pop", |x| Some(x)) {
            MatchOperation::Pop
        } else if let Ok(y) = get_key(map, "push", |x| Some(x)) {
            MatchOperation::Push(try!(SyntaxDefinition::parse_pushargs(y, state)))
        } else if let Ok(y) = get_key(map, "set", |x| Some(x)) {
            MatchOperation::Set(try!(SyntaxDefinition::parse_pushargs(y, state)))
        } else {
            MatchOperation::None
        };

        let pattern = MatchPattern {
            regex_str: regex_str,
            regex: regex,
            scope: scope,
            captures: captures,
            operation: operation,
        };
        return Ok(pattern);
    }

    fn parse_pushargs(y: &Yaml,
                      state: &ParserState)
                      -> Result<Vec<ContextReference>, ParseError> {
        // check for a push of multiple items
        if y.as_vec().map(|v| !v.is_empty() && v[0].as_str().is_some()).unwrap_or(false) {
            // this works because Result implements FromIterator to handle the errors
            y.as_vec().unwrap().iter().map(|x| SyntaxDefinition::parse_reference(x, state)).collect()
        } else {
            Ok(vec![try!(SyntaxDefinition::parse_reference(y, state))])
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn can_parse() {
        use syntax_definition::{SyntaxDefinition, Pattern, CaptureMapping};
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
          ident: '[QY]+'
        contexts:
          main:
            - match: \\b(if|else|for|while|{{ident}})\\b
              scope: keyword.control.c keyword.looping.c
              captures:
                  1: meta.preprocessor.c++
                  2: keyword.control.include.c++
              push: [string, 'scope:source.c#main', 'CSS.sublime-syntax#rule-list-body']
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
        assert_eq!(defn2.variables.get("ident").unwrap(), "[QY]+");

        let n: Option<String> = None;
        println!("{:?}", defn2);
        // assert!(false);
        assert_eq!(defn2.contexts["main"].borrow().meta_scope, n);
        assert_eq!(defn2.contexts["main"].borrow().meta_include_prototype, true);
        assert_eq!(defn2.contexts["string"].borrow().meta_scope,
                   Some(String::from("string.quoted.double.c")));
        let first_pattern: &Pattern = &defn2.contexts["main"].borrow().patterns[0];
        match first_pattern {
            &Pattern::Match(ref match_pat) => {
                let m : &CaptureMapping = match_pat.captures.as_ref().expect("test failed");
                let x : &String = &m[&1];
                assert_eq!(x, "meta.preprocessor.c++");
                assert_eq!(format!("{:?}",match_pat.operation),
                    "Push([Named(\"string\"), ByScope { name: \"source.c\", sub_context: Some(\"main\") }, \
                    File { name: \"CSS.sublime-syntax#rule-list-body\", sub_context: Some(\"rule-list-body\") }])");

                assert_eq!(match_pat.scope, vec![String::from("keyword.control.c"), String::from("keyword.looping.c")]);

                let r = match_pat.regex.as_ref().unwrap();
                assert!(r.is_match("else"));
                assert!(!r.is_match("elses"));
                assert!(!r.is_match("elose"));
                assert!(r.is_match("QYYQQQ"));
                assert!(!r.is_match("QYYQZQQ"));
            },
            _ => assert!(false)
        }
    }
}
