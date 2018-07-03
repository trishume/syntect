use super::scope::*;
use super::syntax_definition::*;
use yaml_rust::{YamlLoader, Yaml, ScanError};
use yaml_rust::yaml::Hash;
use std::collections::HashMap;
use onig::{self, Regex, Captures, RegexOptions, Syntax};
use std::error::Error;
use std::fmt;
use std::path::Path;
use std::ops::DerefMut;

#[derive(Debug)]
pub enum ParseSyntaxError {
    /// Invalid YAML file syntax, or at least something yaml_rust can't handle
    InvalidYaml(ScanError),
    /// The file must contain at least one YAML document
    EmptyFile,
    /// Some keys are required for something to be a valid `.sublime-syntax`
    MissingMandatoryKey(&'static str),
    /// Invalid regex
    RegexCompileError(String, onig::Error),
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

impl fmt::Display for ParseSyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use ParseSyntaxError::*;

        match *self {
            RegexCompileError(ref regex, ref error) =>
                write!(f, "Error while compiling regex '{}': {}",
                       regex, error.description()),
            _ => write!(f, "{}", self.description())
        }
    }
}

impl Error for ParseSyntaxError {
    fn description(&self) -> &str {
        use ParseSyntaxError::*;

        match *self {
            InvalidYaml(_) => "Invalid YAML file syntax",
            EmptyFile => "Empty file",
            MissingMandatoryKey(_) => "Missing mandatory key in YAML file",
            RegexCompileError(_, ref error) => error.description(),
            InvalidScope(_) => "Invalid scope",
            BadFileRef => "Invalid file reference",
            MainMissing => "Context 'main' is missing",
            TypeMismatch => "Type mismatch",
        }
    }

    fn cause(&self) -> Option<&Error> {
        use ParseSyntaxError::*;

        match *self {
            InvalidYaml(ref error) => Some(error),
            RegexCompileError(_, ref error) => Some(error),
            _ => None,
        }
    }
}

fn get_key<'a, R, F: FnOnce(&'a Yaml) -> Option<R>>(map: &'a Hash,
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
    lines_include_newline: bool,
}

// `__start` must not include prototypes from the actual syntax definition,
// otherwise it's possible that a prototype makes us pop out of `__start`.
static START_CONTEXT: &'static str = "
__start:
    - meta_include_prototype: false
    - match: ''
      push: __main
__main:
    - include: main
";

impl SyntaxDefinition {
    /// In case you want to create your own SyntaxDefinition's in memory from strings.
    /// Generally you should use a `SyntaxSet`
    ///
    /// `fallback_name` is an optional name to use when the YAML doesn't provide a `name` key.
    pub fn load_from_str(s: &str,
                         lines_include_newline: bool,
                         fallback_name: Option<&str>)
                         -> Result<SyntaxDefinition, ParseSyntaxError> {
        let docs = match YamlLoader::load_from_str(s) {
            Ok(x) => x,
            Err(e) => return Err(ParseSyntaxError::InvalidYaml(e)),
        };
        if docs.is_empty() {
            return Err(ParseSyntaxError::EmptyFile);
        }
        let doc = &docs[0];
        let mut scope_repo = SCOPE_REPO.lock().unwrap();
        SyntaxDefinition::parse_top_level(doc, scope_repo.deref_mut(), lines_include_newline, fallback_name)
    }

    // TODO: visibility. also, is this the right file to have these?
    pub fn find_context_by_name(&self, name: &str) -> Option<&Context> {
        self.find_context_index_by_name(name).map(|i| &self.contexts[i])
//        if let Ok(i) = self.contexts.binary_search_by_key(&name, |c| &c.name) {
//            Some(&self.contexts[i])
//        } else {
//            None
//        }
    }

    // TODO: visibility
    pub fn find_context_index_by_name(&self, name: &str) -> Option<usize> {
        self.contexts.binary_search_by_key(&name, |c| &c.name).ok()
    }

    fn parse_top_level(doc: &Yaml,
                       scope_repo: &mut ScopeRepository,
                       lines_include_newline: bool,
                       fallback_name: Option<&str>)
                       -> Result<SyntaxDefinition, ParseSyntaxError> {
        let h = doc.as_hash().ok_or(ParseSyntaxError::TypeMismatch)?;

        let mut variables = HashMap::new();
        if let Ok(map) = get_key(h, "variables", |x| x.as_hash()) {
            for (key, value) in map.iter() {
                if let (Some(key_str), Some(val_str)) = (key.as_str(), value.as_str()) {
                    variables.insert(key_str.to_owned(), val_str.to_owned());
                }
            }
        }
        let contexts_hash = get_key(h, "contexts", |x| x.as_hash())?;
        let top_level_scope = scope_repo.build(get_key(h, "scope", |x| x.as_str())?)
            .map_err(ParseSyntaxError::InvalidScope)?;
        let mut state = ParserState {
            scope_repo: scope_repo,
            variables: variables,
            variable_regex: Regex::new(r"\{\{([A-Za-z0-9_]+)\}\}").unwrap(),
            backref_regex: Regex::new(r"\\\d").unwrap(),
            lines_include_newline: lines_include_newline,
        };

        let mut context_map = SyntaxDefinition::parse_contexts(contexts_hash, &mut state)?;
        if !context_map.contains_key("main") {
            return Err(ParseSyntaxError::MainMissing);
        }

        SyntaxDefinition::add_initial_contexts(
            &mut context_map,
            &mut state,
            top_level_scope,
        );

        let mut contexts: Vec<Context> = context_map.into_iter().map(|(_k, v)| v).collect();
        // Sorting is very important as the way we look up contexts by name
        // during linking is by doing a binary search by name.
        contexts.sort_by(|a, b| a.name.cmp(&b.name));

        let defn = SyntaxDefinition {
            name: get_key(h, "name", |x| x.as_str()).unwrap_or(fallback_name.unwrap_or("Unnamed")).to_owned(),
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
            contexts,
        };
        Ok(defn)
    }

    fn parse_contexts(map: &Hash,
                      state: &mut ParserState)
                      -> Result<HashMap<String, Context>, ParseSyntaxError> {
        let mut contexts = Vec::new();
        for (key, value) in map.iter() {
            if let (Some(name), Some(val_vec)) = (key.as_str(), value.as_vec()) {
                let is_prototype = name == "prototype";
                SyntaxDefinition::parse_context(name, val_vec, state, &mut contexts, is_prototype)?;
            }
        }

        let mut context_map = HashMap::new();
        for context in contexts {
            // TODO: the caller doesn't really need a map anymore, just needs
            // to know which one is the main context.
            context_map.insert(context.name.clone(), context);
        }
        Ok(context_map)
    }

    fn parse_context(name: &str,
                     vec: &[Yaml],
                     // TODO: Maybe just pass the scope repo if that's all that's needed?
                     state: &mut ParserState,
                     contexts: &mut Vec<Context>,
                     is_prototype: bool)
                     -> Result<(), ParseSyntaxError> {
        let mut context = Context::new(name, !is_prototype);

        for (i, y) in vec.iter().enumerate() {
            let map = y.as_hash().ok_or(ParseSyntaxError::TypeMismatch)?;

            let mut is_special = false;
            if let Some(x) = get_key(map, "meta_scope", |x| x.as_str()).ok() {
                context.meta_scope = str_to_scopes(x, state.scope_repo)?;
                is_special = true;
            }
            if let Some(x) = get_key(map, "meta_content_scope", |x| x.as_str()).ok() {
                context.meta_content_scope = str_to_scopes(x, state.scope_repo)?;
                is_special = true;
            }
            if let Some(x) = get_key(map, "meta_include_prototype", |x| x.as_bool()).ok() {
                context.meta_include_prototype = x;
                is_special = true;
            }
            if let Some(true) = get_key(map, "clear_scopes", |x| x.as_bool()).ok() {
                context.clear_scopes = Some(ClearAmount::All);
                is_special = true;
            }
            if let Some(x) = get_key(map, "clear_scopes", |x| x.as_i64()).ok() {
                context.clear_scopes = Some(ClearAmount::TopN(x as usize));
                is_special = true;
            }
            if !is_special {
                let subname = format!("{}.{}", name, i);
                if let Some(x) = get_key(map, "include", Some).ok() {
                    let reference = SyntaxDefinition::parse_reference(
                        &subname, x, state, contexts)?;
                    context.patterns.push(Pattern::Include(reference));
                } else {
                    let pattern = SyntaxDefinition::parse_match_pattern(
                        &subname, map, state, contexts)?;
                    if pattern.has_captures {
                        context.uses_backrefs = true;
                    }
                    context.patterns.push(Pattern::Match(pattern));
                }
            }

        }

        contexts.push(context);
        Ok(())
    }

    fn parse_reference(name: &str,
                       y: &Yaml,
                       state: &mut ParserState,
                       contexts: &mut Vec<Context>)
                       -> Result<ContextReference, ParseSyntaxError> {
        if let Some(s) = y.as_str() {
            let parts: Vec<&str> = s.split('#').collect();
            let sub_context = if parts.len() > 1 {
                Some(parts[1].to_owned())
            } else {
                None
            };
            if parts[0].starts_with("scope:") {
                Ok(ContextReference::ByScope {
                    scope: state.scope_repo
                        .build(&parts[0][6..])
                        .map_err(ParseSyntaxError::InvalidScope)?,
                    sub_context: sub_context,
                })
            } else if parts[0].ends_with(".sublime-syntax") {
                let stem = Path::new(parts[0])
                    .file_stem()
                    .and_then(|x| x.to_str())
                    .ok_or(ParseSyntaxError::BadFileRef)?;
                Ok(ContextReference::File {
                    name: stem.to_owned(),
                    sub_context: sub_context,
                })
            } else {
                Ok(ContextReference::Named(parts[0].to_owned()))
            }
        } else if let Some(v) = y.as_vec() {
            SyntaxDefinition::parse_context(&name, v, state, contexts, false)?;
            Ok(ContextReference::Inline(name.to_string()))
        } else {
            Err(ParseSyntaxError::TypeMismatch)
        }
    }

    fn resolve_variables(raw_regex: &str, state: &ParserState) -> String {
        state.variable_regex.replace_all(raw_regex, |caps: &Captures| {
            let var_regex_raw =
                state.variables.get(caps.at(1).unwrap_or("")).map_or("", |x| &**x);
            Self::resolve_variables(var_regex_raw, state)
        })
    }

    fn try_compile_regex(regex_str: &str) -> Result<(), ParseSyntaxError> {
        // Replace backreferences with a placeholder value that will also appear in errors
        let regex_str = substitute_backrefs_in_regex(regex_str, |i| Some(format!("<placeholder_{}>", i)));

        let result = Regex::with_options(&regex_str,
                                         RegexOptions::REGEX_OPTION_CAPTURE_GROUP,
                                         Syntax::default());
        match result {
            Err(onig_error) => {
                Err(ParseSyntaxError::RegexCompileError(regex_str, onig_error))
            },
            _ => Ok(())
        }
    }

    fn parse_match_pattern(name: &str,
                           map: &Hash,
                           state: &mut ParserState,
                           contexts: &mut Vec<Context>)
                           -> Result<MatchPattern, ParseSyntaxError> {
        let raw_regex = get_key(map, "match", |x| x.as_str())?;
        let regex_str_1 = Self::resolve_variables(raw_regex, state);
        // if the passed in strings don't include newlines (unlike Sublime) we can't match on them
        let regex_str = if state.lines_include_newline {
            regex_str_1
        } else {
            rewrite_regex(regex_str_1)
        };
        // println!("{:?}", regex_str);

        Self::try_compile_regex(&regex_str)?;

        let scope = get_key(map, "scope", |x| x.as_str())
            .ok()
            .map(|s| str_to_scopes(s, state.scope_repo))
            .unwrap_or_else(|| Ok(vec![]))?;


        let captures = if let Ok(map) = get_key(map, "captures", |x| x.as_hash()) {
            let mut res_map = Vec::new();
            for (key, value) in map.iter() {
                if let (Some(key_int), Some(val_str)) = (key.as_i64(), value.as_str()) {
                    res_map.push((key_int as usize,
                                  str_to_scopes(val_str, state.scope_repo)?));
                }
            }
            Some(res_map)
        } else {
            None
        };

        let mut has_captures = false;
        let operation = if let Ok(_) = get_key(map, "pop", Some) {
            // Thanks @wbond for letting me know this is the correct way to check for captures
            has_captures = state.backref_regex.find(&regex_str).is_some();
            MatchOperation::Pop
        } else if let Ok(y) = get_key(map, "push", Some) {
            let subname = format!("{}.push", name);
            MatchOperation::Push(SyntaxDefinition::parse_pushargs(&subname, y, state, contexts)?)
        } else if let Ok(y) = get_key(map, "set", Some) {
            let subname = format!("{}.set", name);
            MatchOperation::Set(SyntaxDefinition::parse_pushargs(&subname, y, state, contexts)?)
        } else if let Ok(y) = get_key(map, "embed", Some) {
            // Same as push so we translate it to what it would be
            let mut embed_escape_context_yaml = vec!();
            let mut commands = Hash::new();
            commands.insert(Yaml::String("meta_include_prototype".to_string()), Yaml::Boolean(false));
            embed_escape_context_yaml.push(Yaml::Hash(commands));
            if let Ok(s) = get_key(map, "embed_scope", Some) {
                commands = Hash::new();
                commands.insert(Yaml::String("meta_content_scope".to_string()), s.clone());
                embed_escape_context_yaml.push(Yaml::Hash(commands));
            }
            if let Ok(v) = get_key(map, "escape", Some) {
                let mut match_map = Hash::new();
                match_map.insert(Yaml::String("match".to_string()), v.clone());
                match_map.insert(Yaml::String("pop".to_string()), Yaml::Boolean(true));
                if let Ok(y) = get_key(map, "escape_captures", Some) {
                    match_map.insert(Yaml::String("captures".to_string()), y.clone());
                }
                embed_escape_context_yaml.push(Yaml::Hash(match_map));
                let subname = format!("{}.embed", name);
                SyntaxDefinition::parse_context(
                    &subname,
                    &embed_escape_context_yaml,
                    state,
                    contexts,
                    false
                )?;
                let subname2 = format!("{}.embed_TODO", name);
                MatchOperation::Push(vec![ContextReference::Inline(subname),
                                          SyntaxDefinition::parse_reference(&subname2, y, state, contexts)?])
            } else {
                return Err(ParseSyntaxError::MissingMandatoryKey("escape"));
            }

        } else {
            MatchOperation::None
        };

        let with_prototype = if let Ok(v) = get_key(map, "with_prototype", |x| x.as_vec()) {
            let subname = format!("{}.with_prototype", name);
            // should a with_prototype include the prototype? I don't think so.
            Self::parse_context(&subname, v, state, contexts, true)?;
            Some(ContextReference::Named(subname))
        } else if let Ok(v) = get_key(map, "escape", Some) {
            let subname = format!("{}.escape", name);

            let mut context = Context::new(&subname, false);
            let mut match_map = Hash::new();
            match_map.insert(Yaml::String("match".to_string()), Yaml::String(format!("(?={})", v.as_str().unwrap())));
            match_map.insert(Yaml::String("pop".to_string()), Yaml::Boolean(true));
            let pattern = SyntaxDefinition::parse_match_pattern(&subname, &match_map, state, contexts)?;
            if pattern.has_captures {
                context.uses_backrefs = true;
            }
            context.patterns.push(Pattern::Match(pattern));

            contexts.push(context);
            Some(ContextReference::Named(subname))
        } else {
            None
        };

        let pattern = MatchPattern::new(
            has_captures,
            regex_str,
            scope,
            captures,
            operation,
            with_prototype,
        );

        Ok(pattern)
    }

    fn parse_pushargs(name: &str,
                      y: &Yaml,
                      state: &mut ParserState,
                      contexts: &mut Vec<Context>)
                      -> Result<Vec<ContextReference>, ParseSyntaxError> {
        // check for a push of multiple items
        if y.as_vec().map_or(false, |v| !v.is_empty() && (v[0].as_str().is_some() || (v[0].as_vec().is_some() && v[0].as_vec().unwrap()[0].as_hash().is_some()))) {
            // this works because Result implements FromIterator to handle the errors
            y.as_vec()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(i, x)| SyntaxDefinition::parse_reference(
                    &format!("{}.{}", name, i), x, state, contexts))
                .collect()
        } else {
            let reference = SyntaxDefinition::parse_reference(name, y, state, contexts)?;
            Ok(vec![reference])
        }
    }

    /// Sublime treats the top level context slightly differently from
    /// including the main context from other syntaxes. When main is popped
    /// it is immediately re-added and when it is `set` over the file level
    /// scope remains. This behaviour is emulated through some added contexts
    /// that are the actual top level contexts used in parsing.
    /// See https://github.com/trishume/syntect/issues/58 for more.
    fn add_initial_contexts(
        contexts: &mut HashMap<String, Context>,
        state: &mut ParserState,
        top_level_scope: Scope,
    ) {
        let yaml_docs = YamlLoader::load_from_str(START_CONTEXT).unwrap();
        let yaml = &yaml_docs[0];

        let main_yaml : &[Yaml] = yaml["__main"].as_vec().unwrap();
        // TODO: Fix this. Maybe directly construct the Context instead of from yaml?
        let mut main = {
            let mut new_context = Vec::new();
            SyntaxDefinition::parse_context("__main", main_yaml, state, &mut new_context, false).unwrap();
            new_context.pop().unwrap()
        };

        if let Some(real_main) = contexts.get_mut("main") {
            main.meta_include_prototype = real_main.meta_include_prototype;
            main.meta_scope = real_main.meta_scope.clone();
            main.meta_content_scope = real_main.meta_content_scope.clone();

            // add the top_level_scope as a meta_content_scope to main so
            // pushes from other syntaxes add the file scope
            // TODO: this order is not quite correct if main also has a meta_scope
            real_main.meta_content_scope.insert(0, top_level_scope);
        }

        contexts.insert("__main".to_owned(), main);

        let start_yaml : &[Yaml] = yaml["__start"].as_vec().unwrap();
        // TODO: fix this, see above
        let mut start = {
            let mut new_context = Vec::new();
            SyntaxDefinition::parse_context("__start", start_yaml, state, &mut new_context, false).unwrap();
            new_context.pop().unwrap()
        };
        start.meta_content_scope = vec![top_level_scope];

        contexts.insert("__start".to_owned(), start);
    }
}

/// Rewrite a regex that matches `\n` to one that matches `$` (end of line) instead.
/// That allows the regex to be used to match lines that don't include a trailing newline character.
///
/// The reason we're doing this is because the regexes in the syntax definitions assume that the
/// lines that are being matched on include a trailing newline.
///
/// Note that the rewrite is just an approximation and there's a couple of cases it can not handle,
/// due to `$` being an anchor whereas `\n` matches a character.
fn rewrite_regex(regex: String) -> String {
    if !regex.contains(r"\n") {
        return regex;
    }

    // A special fix to rewrite a pattern from the `Rd` syntax that the RegexRewriter can not
    // handle properly.
    let regex = regex.replace("(?:\\n)?", "(?:$|)");

    let rewriter = RegexRewriter {
        bytes: regex.as_bytes(),
        index: 0,
    };
    rewriter.rewrite()
}

struct RegexRewriter<'a> {
    bytes: &'a [u8],
    index: usize,
}

impl<'a> RegexRewriter<'a> {
    fn rewrite(mut self) -> String {
        let mut result = Vec::new();
        while let Some(c) = self.peek() {
            match c {
                b'\\' => {
                    self.next();
                    if let Some(c2) = self.peek() {
                        self.next();
                        // Replacing `\n?` with `$?` would make parsing later fail with
                        // "target of repeat operator is invalid"
                        if c2 == b'n' && self.peek() != Some(b'?') {
                            result.extend_from_slice(b"$");
                        } else {
                            result.push(c);
                            result.push(c2);
                        }
                    } else {
                        result.push(c);
                    }
                }
                b'[' => {
                    let (mut content, matches_newline) = self.parse_character_class();
                    if matches_newline && self.peek() != Some(b'?') {
                        result.extend_from_slice(b"(?:");
                        result.append(&mut content);
                        result.extend_from_slice(br"|$)");
                    } else {
                        result.append(&mut content);
                    }
                }
                _ => {
                    self.next();
                    result.push(c);
                }
            }
        }
        String::from_utf8(result).unwrap()
    }

    fn parse_character_class(&mut self) -> (Vec<u8>, bool) {
        let mut content = Vec::new();
        let mut negated = false;
        let mut nesting = 0;
        let mut matches_newline = false;

        self.next();
        content.push(b'[');
        if let Some(b'^') = self.peek() {
            self.next();
            content.push(b'^');
            negated = true;
        }

        // An unescaped `]` is allowed after `[` or `[^` and doesn't mean the end of the class.
        if let Some(b']') = self.peek() {
            self.next();
            content.push(b']');
        }

        while let Some(c) = self.peek() {
            match c {
                b'\\' => {
                    self.next();
                    if let Some(c2) = self.peek() {
                        self.next();
                        if c2 == b'n' && !negated && nesting == 0 {
                            matches_newline = true;
                        }
                        content.push(c);
                        content.push(c2);
                    } else {
                        content.push(c);
                    }
                }
                b'[' => {
                    self.next();
                    content.push(b'[');
                    nesting += 1;
                }
                b']' => {
                    self.next();
                    content.push(b']');
                    if nesting == 0 {
                        break;
                    }
                    nesting -= 1;
                }
                _ => {
                    self.next();
                    content.push(c);
                }
            }
        }

        (content, matches_newline)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).map(|&b| b)
    }

    fn next(&mut self) {
        self.index += 1;
    }
}


#[cfg(test)]
mod tests {
    use parsing::syntax_definition::*;
    use parsing::Scope;
    use super::*;

    #[test]
    fn can_parse() {
        let defn: SyntaxDefinition =
            SyntaxDefinition::load_from_str("name: C\nscope: source.c\ncontexts: {main: []}",
                                            false, None)
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
                                            false, None)
                .unwrap();
        assert_eq!(defn2.name, "C");
        let top_level_scope = Scope::new("source.c").unwrap();
        assert_eq!(defn2.scope, top_level_scope);
        let exts: Vec<String> = vec![String::from("c"), String::from("h")];
        assert_eq!(defn2.file_extensions, exts);
        assert_eq!(defn2.hidden, true);
        assert_eq!(defn2.variables.get("ident").unwrap(), "[QY]+");

        let n: Vec<Scope> = Vec::new();
        println!("{:?}", defn2);
        // assert!(false);
        let main = defn2.find_context_by_name("main").unwrap();
        assert_eq!(main.meta_content_scope, vec![top_level_scope]);
        assert_eq!(main.meta_scope, n);
        assert_eq!(main.meta_include_prototype, true);

        assert_eq!(defn2.find_context_by_name("__main").unwrap().meta_content_scope, n);
        assert_eq!(defn2.find_context_by_name("__start").unwrap().meta_content_scope, vec![top_level_scope]);

        assert_eq!(defn2.find_context_by_name("string").unwrap().meta_scope,
                   vec![Scope::new("string.quoted.double.c").unwrap()]);
        let first_pattern: &Pattern = &main.patterns[0];
        match first_pattern {
            &Pattern::Match(ref match_pat) => {
                let m: &CaptureMapping = match_pat.captures.as_ref().expect("test failed");
                assert_eq!(&m[0], &(1,vec![Scope::new("meta.preprocessor.c++").unwrap()]));
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

    #[test]
    fn can_parse_embed_as_with_prototypes() {
        let old_def = SyntaxDefinition::load_from_str(r#"
        name: C
        scope: source.c
        file_extensions: [c, h]
        variables:
          ident: '[QY]+'
        contexts:
          main:
            - match: '(>)\s*'
              captures:
                1: meta.tag.style.begin.html punctuation.definition.tag.end.html
              push:
                - [{ meta_include_prototype: false }, { meta_content_scope: 'source.css.embedded.html' }, { match: '(?i)(?=</style)', pop: true }]
                - scope:source.css
              with_prototype:
                - match: (?=(?i)(?=</style))
                  pop: true
        "#,false, None).unwrap();

        let def_with_embed = SyntaxDefinition::load_from_str(r#"
        name: C
        scope: source.c
        file_extensions: [c, h]
        variables:
          ident: '[QY]+'
        contexts:
          main:
            - match: '(>)\s*'
              captures:
                1: meta.tag.style.begin.html punctuation.definition.tag.end.html
              embed: scope:source.css
              embed_scope: source.css.embedded.html
              escape: (?i)(?=</style)
        "#,false, None).unwrap();

        assert_eq!(old_def.find_context_by_name("main").unwrap(), def_with_embed.find_context_by_name("main").unwrap());
    }

    #[test]
    fn errors_on_embed_without_escape() {
        let def = SyntaxDefinition::load_from_str(r#"
        name: C
        scope: source.c
        file_extensions: [c, h]
        variables:
          ident: '[QY]+'
        contexts:
          main:
            - match: '(>)\s*'
              captures:
                1: meta.tag.style.begin.html punctuation.definition.tag.end.html
              embed: scope:source.css
              embed_scope: source.css.embedded.html
        "#,false, None);
        assert!(def.is_err());
        match def.unwrap_err() {
            ParseSyntaxError::MissingMandatoryKey(key) => assert_eq!(key, "escape"),
            _ => assert!(false, "Got unexpected ParseSyntaxError"),
        }
    }

    #[test]
    fn errors_on_regex_compile_error() {
        let def = SyntaxDefinition::load_from_str(r#"
        name: C
        scope: source.c
        file_extensions: [test]
        contexts:
          main:
            - match: '[a'
              scope: keyword.name
        "#,false, None);
        assert!(def.is_err());
        match def.unwrap_err() {
            ParseSyntaxError::RegexCompileError(ref regex, _) => assert_eq!("[a", regex),
            _ => assert!(false, "Got unexpected ParseSyntaxError"),
        }
    }

    #[test]
    fn can_parse_ugly_yaml() {
        let defn: SyntaxDefinition =
            SyntaxDefinition::load_from_str("
        name: LaTeX
        scope: text.tex.latex
        contexts:
          main:
            - match: '((\\\\)(?:framebox|makebox))\\b'
              captures:
                1: support.function.box.latex
                2: punctuation.definition.backslash.latex
              push:
                - [{meta_scope: meta.function.box.latex}, {match: '', pop: true}]
                - argument
                - optional-arguments
          argument:
            - match: '\\{'
              scope: punctuation.definition.group.brace.begin.latex
            - match: '(?=\\S)'
              pop: true
          optional-arguments:
            - match: '(?=\\S)'
              pop: true
        ",
                                            false, None)
                .unwrap();
        assert_eq!(defn.name, "LaTeX");
        let top_level_scope = Scope::new("text.tex.latex").unwrap();
        assert_eq!(defn.scope, top_level_scope);

        let first_pattern: &Pattern = &defn.find_context_by_name("main").unwrap().patterns[0];
        match first_pattern {
            &Pattern::Match(ref match_pat) => {
                let m: &CaptureMapping = match_pat.captures.as_ref().expect("test failed");
                assert_eq!(&m[0], &(1,vec![Scope::new("support.function.box.latex").unwrap()]));

                //use parsing::syntax_definition::ContextReference::*;
                // TODO: check the first pushed reference is Inline(...) and has a meta_scope of meta.function.box.latex
                // TODO: check the second pushed reference is Named("argument".to_owned())
                // TODO: check the third pushed reference is Named("optional-arguments".to_owned())

                assert!(match_pat.with_prototype.is_none());
            }
            _ => assert!(false),
        }
    }

    #[test]
    fn can_use_fallback_name() {
        let def = SyntaxDefinition::load_from_str(r#"
        scope: source.c
        contexts:
          main:
            - match: ''
        "#,false, Some("C"));
        assert_eq!(def.unwrap().name, "C");
    }

    #[test]
    fn can_rewrite_regex() {
        fn rewrite(s: &str) -> String {
            rewrite_regex(s.to_string())
        }

        assert_eq!(&rewrite(r"a"), r"a");
        assert_eq!(&rewrite(r"\b"), r"\b");
        assert_eq!(&rewrite(r"(a)"), r"(a)");
        assert_eq!(&rewrite(r"[a]"), r"[a]");
        assert_eq!(&rewrite(r"[^a]"), r"[^a]");
        assert_eq!(&rewrite(r"[]a]"), r"[]a]");
        assert_eq!(&rewrite(r"[[a]]"), r"[[a]]");

        assert_eq!(&rewrite(r"\n"), r"$");
        assert_eq!(&rewrite(r"\[\n"), r"\[$");
        assert_eq!(&rewrite(r"a\n?"), r"a\n?");
        assert_eq!(&rewrite(r"[abc\n]"), r"(?:[abc\n]|$)");
        assert_eq!(&rewrite(r"[^\n]"), r"[^\n]");
        assert_eq!(&rewrite(r"[^]\n]"), r"[^]\n]");
        assert_eq!(&rewrite(r"[\n]?"), r"[\n]?");
        // Removing the `\n` might result in an empty character class, so we should leave it.
        assert_eq!(&rewrite(r"[\n]"), r"(?:[\n]|$)");
        assert_eq!(&rewrite(r"[]\n]"), r"(?:[]\n]|$)");
        // In order to properly understand nesting, we'd have to have a full parser, so ignore it.
        assert_eq!(&rewrite(r"[[a]&&[\n]]"), r"[[a]&&[\n]]");

        assert_eq!(&rewrite(r"ab(?:\n)?"), r"ab(?:$|)");
        assert_eq!(&rewrite(r"(?<!\n)ab"), r"(?<!$)ab");
        assert_eq!(&rewrite(r"(?<=\n)ab"), r"(?<=$)ab");
    }
}
