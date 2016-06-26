use super::scope::*;
use super::syntax_definition::*;
use yaml_rust::{YamlLoader, Yaml, ScanError};
use std::collections::{HashMap, BTreeMap};
use onig::{self, Regex, Captures};
use std::rc::Rc;
use std::cell::RefCell;
use std::path::Path;
use std::ops::DerefMut;

#[derive(Debug)]
pub enum ParseSyntaxError {
    /// Invalid YAML file syntax, or at least something yaml_rust can't handle
    InvalidYaml(ScanError),
    /// The file must contain at least on YAML document
    EmptyFile,
    /// Some keys are required for something to be a valid `.sublime-syntax`
    MissingMandatoryKey(&'static str),
    /// Invalid regex
    RegexCompileError(onig::Error),
    /// A scope that syntect's scope implementation can't handle
    InvalidScope(ParseScopeError),
    /// A reference to another file that is invalid
    BadFileRef,
    /// Syntaxes must have a context named "main"
    MainMissing,
    /// Some part of the YAML file is the wrong type (e.g a string but should be a list)
    /// Sorry this doesn't give you any way to narrow down where this is.
    /// Maybe use Sublime Text to figure it out.
    TypeMismatch,
}

fn get_key<'a, R, F: FnOnce(&'a Yaml) -> Option<R>>(map: &'a BTreeMap<Yaml, Yaml>,
                                                    key: &'static str,
                                                    f: F)
                                                    -> Result<R, ParseSyntaxError> {
    map.get(&Yaml::String(key.to_owned()))
        .ok_or(ParseSyntaxError::MissingMandatoryKey(key))
        .and_then(|x| f(x).ok_or(ParseSyntaxError::TypeMismatch))
}

fn str_to_scopes(s: &str, repo: &mut ScopeRepository) -> Result<Vec<Scope>, ParseSyntaxError> {
    s.split_whitespace()
        .map(|scope| repo.build(scope).map_err(ParseSyntaxError::InvalidScope))
        .collect()
}

struct ParserState<'a> {
    scope_repo: &'a mut ScopeRepository,
    variables: HashMap<String, String>,
    variable_regex: Regex,
    backref_regex: Regex,
    short_multibyte_regex: Regex,
    top_level_scope: Scope,
    lines_include_newline: bool,
}

impl SyntaxDefinition {
    /// In case you want to create your own SyntaxDefinition's in memory from strings.
    /// Generally you should use a `SyntaxSet`
    pub fn load_from_str(s: &str,
                         lines_include_newline: bool)
                         -> Result<SyntaxDefinition, ParseSyntaxError> {
        let docs = match YamlLoader::load_from_str(s) {
            Ok(x) => x,
            Err(e) => return Err(ParseSyntaxError::InvalidYaml(e)),
        };
        if docs.len() == 0 {
            return Err(ParseSyntaxError::EmptyFile);
        }
        let doc = &docs[0];
        let mut scope_repo = SCOPE_REPO.lock().unwrap();
        SyntaxDefinition::parse_top_level(doc, scope_repo.deref_mut(), lines_include_newline)
    }

    fn parse_top_level(doc: &Yaml,
                       scope_repo: &mut ScopeRepository,
                       lines_include_newline: bool)
                       -> Result<SyntaxDefinition, ParseSyntaxError> {
        let h = try!(doc.as_hash().ok_or(ParseSyntaxError::TypeMismatch));

        let mut variables = HashMap::new();
        if let Ok(map) = get_key(h, "variables", |x| x.as_hash()) {
            for (key, value) in map.iter() {
                if let (Some(key_str), Some(val_str)) = (key.as_str(), value.as_str()) {
                    variables.insert(key_str.to_owned(), val_str.to_owned());
                }
            }
        }
        let contexts_hash = try!(get_key(h, "contexts", |x| x.as_hash()));
        let top_level_scope = try!(scope_repo.build(try!(get_key(h, "scope", |x| x.as_str())))
            .map_err(ParseSyntaxError::InvalidScope));
        let mut state = ParserState {
            scope_repo: scope_repo,
            variables: variables,
            variable_regex: Regex::new(r"\{\{([A-Za-z0-9_]+)\}\}").unwrap(),
            backref_regex: Regex::new(r"\\\d").unwrap(),
            short_multibyte_regex: Regex::new(r"\\x([a-fA-F][a-fA-F0-9])").unwrap(),
            top_level_scope: top_level_scope,
            lines_include_newline: lines_include_newline,
        };

        let contexts = try!(SyntaxDefinition::parse_contexts(contexts_hash, &mut state));
        if !contexts.contains_key("main") {
            return Err(ParseSyntaxError::MainMissing);
        }

        let defn = SyntaxDefinition {
            name: try!(get_key(h, "name", |x| x.as_str())).to_owned(),
            scope: top_level_scope,
            file_extensions: {
                get_key(h, "file_extensions", |x| x.as_vec())
                    .map(|v| v.iter().filter_map(|y| y.as_str()).map(|x| x.to_owned()).collect())
                    .unwrap_or_else(|_| Vec::new())
            },
            // TODO maybe cache a compiled version of this Regex
            first_line_match: get_key(h, "first_line_match", |x| x.as_str())
                .ok()
                .map(|s| s.to_owned()),
            hidden: get_key(h, "hidden", |x| x.as_bool()).unwrap_or(false),

            variables: state.variables.clone(),
            contexts: contexts,
            prototype: None,
        };
        Ok(defn)
    }

    fn parse_contexts(map: &BTreeMap<Yaml, Yaml>,
                      state: &mut ParserState)
                      -> Result<HashMap<String, ContextPtr>, ParseSyntaxError> {
        let mut contexts = HashMap::new();
        for (key, value) in map.iter() {
            if let (Some(name), Some(val_vec)) = (key.as_str(), value.as_vec()) {
                let is_prototype = name == "prototype";
                let context_ptr =
                    try!(SyntaxDefinition::parse_context(val_vec, state, is_prototype));
                if name == "main" {
                    let mut context = context_ptr.borrow_mut();
                    if context.meta_content_scope.is_empty() {
                        context.meta_content_scope.push(state.top_level_scope)
                    }
                }
                contexts.insert(name.to_owned(), context_ptr);
            }
        }
        return Ok(contexts);
    }

    fn parse_context(vec: &Vec<Yaml>,
                     state: &mut ParserState,
                     is_prototype: bool)
                     -> Result<ContextPtr, ParseSyntaxError> {
        let mut context = Context {
            meta_scope: Vec::new(),
            meta_content_scope: Vec::new(),
            meta_include_prototype: !is_prototype,
            uses_backrefs: false,
            patterns: Vec::new(),
            prototype: None,
        };
        for y in vec.iter() {
            let map = try!(y.as_hash().ok_or(ParseSyntaxError::TypeMismatch));

            let mut is_special = false;
            if let Some(x) = get_key(map, "meta_scope", |x| x.as_str()).ok() {
                context.meta_scope = try!(str_to_scopes(x, state.scope_repo));
                is_special = true;
            }
            if let Some(x) = get_key(map, "meta_content_scope", |x| x.as_str()).ok() {
                context.meta_content_scope = try!(str_to_scopes(x, state.scope_repo));
                is_special = true;
            }
            if let Some(x) = get_key(map, "meta_include_prototype", |x| x.as_bool()).ok() {
                context.meta_include_prototype = x;
                is_special = true;
            }
            if !is_special {
                if let Some(x) = get_key(map, "include", |x| Some(x)).ok() {
                    let reference = try!(SyntaxDefinition::parse_reference(x, state));
                    context.patterns.push(Pattern::Include(reference));
                } else {
                    let pattern = try!(SyntaxDefinition::parse_match_pattern(map, state));
                    if pattern.has_captures {
                        context.uses_backrefs = true;
                    }
                    context.patterns.push(Pattern::Match(pattern));
                }
            }

        }
        return Ok(Rc::new(RefCell::new(context)));
    }

    fn parse_reference(y: &Yaml,
                       state: &mut ParserState)
                       -> Result<ContextReference, ParseSyntaxError> {
        if let Some(s) = y.as_str() {
            let parts: Vec<&str> = s.split("#").collect();
            let sub_context = if parts.len() > 1 {
                Some(parts[1].to_owned())
            } else {
                None
            };
            if parts[0].starts_with("scope:") {
                Ok(ContextReference::ByScope {
                    scope: try!(state.scope_repo
                        .build(&parts[0][6..])
                        .map_err(ParseSyntaxError::InvalidScope)),
                    sub_context: sub_context,
                })
            } else if parts[0].ends_with(".sublime-syntax") {
                let stem = try!(Path::new(parts[0])
                    .file_stem()
                    .and_then(|x| x.to_str())
                    .ok_or(ParseSyntaxError::BadFileRef));
                Ok(ContextReference::File {
                    name: stem.to_owned(),
                    sub_context: sub_context,
                })
            } else {
                Ok(ContextReference::Named(parts[0].to_owned()))
            }
        } else if let Some(v) = y.as_vec() {
            let context = try!(SyntaxDefinition::parse_context(v, state, false));
            Ok(ContextReference::Inline(context))
        } else {
            Err(ParseSyntaxError::TypeMismatch)
        }
    }

    fn resolve_variables(raw_regex: &str, state: &ParserState) -> String {
        state.variable_regex.replace_all(raw_regex, |caps: &Captures| {
            let var_regex_raw =
                state.variables.get(caps.at(1).unwrap_or("")).map(|x| &**x).unwrap_or("");
            Self::resolve_variables(var_regex_raw, state)
        })
    }

    fn parse_match_pattern(map: &BTreeMap<Yaml, Yaml>,
                           state: &mut ParserState)
                           -> Result<MatchPattern, ParseSyntaxError> {
        let raw_regex = try!(get_key(map, "match", |x| x.as_str()));
        let regex_str_1 = Self::resolve_variables(raw_regex, state);
        // bug triggered by CSS.sublime-syntax, dunno why this is necessary
        let regex_str_2 =
            state.short_multibyte_regex.replace_all(&regex_str_1, |caps: &Captures| {
                format!("\\x{{000000{}}}", caps.at(1).unwrap_or(""))
            });
        // if the passed in strings don't include newlines (unlike Sublime) we can't match on them
        let regex_str = if state.lines_include_newline {
            regex_str_2
        } else {
            regex_str_2
                .replace("\\n?","") // fails with invalid operand of repeat expression
                .replace("(?:\\n)?","") // fails with invalid operand of repeat expression
                .replace("(?<!\\n)","") // fails with invalid pattern in look-behind
                .replace("(?<=\\n)","") // fails with invalid pattern in look-behind
                .replace("  :\\s","  :(\\s|\\z)") // hack specific to YAML.sublime-syntax
                .replace("\\n","\\z")
        };
        // println!("{:?}", regex_str);

        let scope = try!(get_key(map, "scope", |x| x.as_str())
            .ok()
            .map(|s| str_to_scopes(s, state.scope_repo))
            .unwrap_or_else(|| Ok(vec![])));

        let captures = if let Ok(map) = get_key(map, "captures", |x| x.as_hash()) {
            let mut res_map = HashMap::new();
            for (key, value) in map.iter() {
                if let (Some(key_int), Some(val_str)) = (key.as_i64(), value.as_str()) {
                    res_map.insert(key_int as usize,
                                   try!(str_to_scopes(val_str, state.scope_repo)));
                }
            }
            Some(res_map)
        } else {
            None
        };

        let mut has_captures = false;
        let operation = if let Ok(_) = get_key(map, "pop", |x| Some(x)) {
            // Thanks @wbond for letting me know this is the correct way to check for captures
            has_captures = state.backref_regex.find(&regex_str).is_some();
            MatchOperation::Pop
        } else if let Ok(y) = get_key(map, "push", |x| Some(x)) {
            MatchOperation::Push(try!(SyntaxDefinition::parse_pushargs(y, state)))
        } else if let Ok(y) = get_key(map, "set", |x| Some(x)) {
            MatchOperation::Set(try!(SyntaxDefinition::parse_pushargs(y, state)))
        } else {
            MatchOperation::None
        };

        let with_prototype = if let Ok(v) = get_key(map, "with_prototype", |x| x.as_vec()) {
            // should a with_prototype include the prototype? I don't think so.
            Some(try!(SyntaxDefinition::parse_context(v, state, true)))
        } else {
            None
        };

        let pattern = MatchPattern {
            has_captures: has_captures,
            regex_str: regex_str,
            regex: None,
            scope: scope,
            captures: captures,
            operation: operation,
            with_prototype: with_prototype,
        };
        return Ok(pattern);
    }

    fn parse_pushargs(y: &Yaml,
                      state: &mut ParserState)
                      -> Result<Vec<ContextReference>, ParseSyntaxError> {
        // check for a push of multiple items
        if y.as_vec().map(|v| !v.is_empty() && v[0].as_str().is_some()).unwrap_or(false) {
            // this works because Result implements FromIterator to handle the errors
            y.as_vec()
                .unwrap()
                .iter()
                .map(|x| SyntaxDefinition::parse_reference(x, state))
                .collect()
        } else {
            Ok(vec![try!(SyntaxDefinition::parse_reference(y, state))])
        }
    }
}

#[cfg(test)]
mod tests {
    use parsing::syntax_definition::*;
    use parsing::Scope;
    #[test]
    fn can_parse() {
        let defn: SyntaxDefinition =
            SyntaxDefinition::load_from_str("name: C\nscope: source.c\ncontexts: {main: []}",
                                            false)
                .unwrap();
        assert_eq!(defn.name, "C");
        assert_eq!(defn.scope, Scope::new("source.c").unwrap());
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
          prototype:
            - match: lol
              scope: source.php
          main:
            - match: \\b(if|else|for|while|{{ident}})\\b
              scope: keyword.control.c keyword.looping.c
              captures:
                  1: meta.preprocessor.c++
                  2: keyword.control.include.c++
              push: [string, 'scope:source.c#main', 'CSS.sublime-syntax#rule-list-body']
              with_prototype:
                - match: wow
                  pop: true
            - match: '\"'
              push: string
          string:
            - meta_scope: string.quoted.double.c
            - meta_include_prototype: false
            - match: \\\\.
              scope: constant.character.escape.c
            - match: '\"'
              pop: true
        ",
                                            false)
                .unwrap();
        assert_eq!(defn2.name, "C");
        assert_eq!(defn2.scope, Scope::new("source.c").unwrap());
        let exts: Vec<String> = vec![String::from("c"), String::from("h")];
        assert_eq!(defn2.file_extensions, exts);
        assert_eq!(defn2.hidden, true);
        assert_eq!(defn2.variables.get("ident").unwrap(), "[QY]+");

        let n: Vec<Scope> = Vec::new();
        println!("{:?}", defn2);
        // assert!(false);
        assert_eq!(defn2.contexts["main"].borrow().meta_scope, n);
        assert_eq!(defn2.contexts["main"].borrow().meta_include_prototype, true);
        assert_eq!(defn2.contexts["string"].borrow().meta_scope,
                   vec![Scope::new("string.quoted.double.c").unwrap()]);
        let first_pattern: &Pattern = &defn2.contexts["main"].borrow().patterns[0];
        match first_pattern {
            &Pattern::Match(ref match_pat) => {
                let m: &CaptureMapping = match_pat.captures.as_ref().expect("test failed");
                assert_eq!(&m[&1], &vec![Scope::new("meta.preprocessor.c++").unwrap()]);
                use parsing::syntax_definition::ContextReference::*;

                // this is sadly necessary because Context is not Eq because of the Regex
                let expected = MatchOperation::Push(vec![
                    Named("string".to_owned()),
                    ByScope { scope: Scope::new("source.c").unwrap(), sub_context: Some("main".to_owned()) },
                    File {
                        name: "CSS".to_owned(),
                        sub_context: Some("rule-list-body".to_owned())
                    },
                ]);
                assert_eq!(format!("{:?}", match_pat.operation),
                           format!("{:?}", expected));

                assert_eq!(match_pat.scope,
                           vec![Scope::new("keyword.control.c").unwrap(),
                                Scope::new("keyword.looping.c").unwrap()]);

                assert!(match_pat.with_prototype.is_some());
            }
            _ => assert!(false),
        }
    }
}
