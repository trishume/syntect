use super::regex::{Regex, Region};
use super::scope::*;
use super::syntax_definition::*;
use std::collections::HashMap;
use std::error::Error;
use std::ops::DerefMut;
use std::path::Path;
use yaml_rust2::yaml::Hash;
use yaml_rust2::{Yaml, YamlLoader};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ParseSyntaxError {
    /// Invalid YAML file syntax, or at least something yaml_rust2 can't handle
    #[error("Invalid YAML file syntax: {0}")]
    InvalidYaml(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The file must contain at least one YAML document
    #[error("The file must contain at least one YAML document")]
    EmptyFile,
    /// Some keys are required for something to be a valid `.sublime-syntax`
    #[error("Missing mandatory key in YAML file: {0}")]
    MissingMandatoryKey(&'static str),
    /// Invalid regex
    #[error("Error while compiling regex '{0}': {1}")]
    RegexCompileError(String, #[source] Box<dyn Error + Send + Sync + 'static>),
    /// A scope that syntect's scope implementation can't handle
    #[error("Invalid scope: {0}")]
    InvalidScope(ParseScopeError),
    /// A reference to another file that is invalid
    #[error("Invalid file reference")]
    BadFileRef,
    /// Syntaxes must have a context named "main"
    #[error("Context 'main' is missing")]
    MainMissing,
    /// Some part of the YAML file is the wrong type (e.g a string but should be a list)
    /// Sorry this doesn't give you any way to narrow down where this is.
    /// Maybe use Sublime Text to figure it out.
    #[error("Type mismatch")]
    TypeMismatch,
}

fn get_key<'a, R, F: FnOnce(&'a Yaml) -> Option<R>>(
    map: &'a Hash,
    key: &'static str,
    f: F,
) -> Result<R, ParseSyntaxError> {
    map.get(&Yaml::String(key.to_owned()))
        .ok_or(ParseSyntaxError::MissingMandatoryKey(key))
        .and_then(|x| f(x).ok_or(ParseSyntaxError::TypeMismatch))
}

fn str_to_scopes(s: &str, repo: &mut ScopeRepository) -> Result<Vec<Scope>, ParseSyntaxError> {
    s.split_whitespace()
        .map(|scope| repo.build(scope).map_err(ParseSyntaxError::InvalidScope))
        .collect()
}

pub(crate) struct ParserState<'a> {
    pub(crate) scope_repo: &'a mut ScopeRepository,
    pub(crate) variables: HashMap<String, String>,
    pub(crate) variable_regex: Regex,
    pub(crate) backref_regex: Regex,
    pub(crate) lines_include_newline: bool,
    pub(crate) version: u32,
    /// When true, `parse_regex` skips the `try_compile_regex` validation
    /// step. This is set for syntaxes that use `extends:`, because their
    /// regexes may reference variables from the parent that aren't available
    /// yet at load time. The regexes will be re-validated by
    /// `re_resolve_all_regexes` after `resolve_extends` merges variables.
    pub(crate) defer_regex_validation: bool,
}

// `__start` must not include prototypes from the actual syntax definition,
// otherwise it's possible that a prototype makes us pop out of `__start`.
static START_CONTEXT: &str = "
__start:
    - meta_include_prototype: false
    - match: ''
      push: __main
__main:
    - include: main
";

impl SyntaxDefinition {
    /// In case you want to create your own SyntaxDefinition's in memory from strings.
    ///
    /// Generally you should use a [`SyntaxSet`].
    ///
    /// `fallback_name` is an optional name to use when the YAML doesn't provide a `name` key.
    ///
    /// [`SyntaxSet`]: ../struct.SyntaxSet.html
    pub fn load_from_str(
        s: &str,
        lines_include_newline: bool,
        fallback_name: Option<&str>,
    ) -> Result<SyntaxDefinition, ParseSyntaxError> {
        let docs = match YamlLoader::load_from_str(s) {
            Ok(x) => x,
            Err(e) => return Err(ParseSyntaxError::InvalidYaml(Box::new(e))),
        };
        if docs.is_empty() {
            return Err(ParseSyntaxError::EmptyFile);
        }
        let doc = &docs[0];
        let mut scope_repo = lock_global_scope_repo();
        SyntaxDefinition::parse_top_level(
            doc,
            scope_repo.deref_mut(),
            lines_include_newline,
            fallback_name,
        )
    }

    fn parse_top_level(
        doc: &Yaml,
        scope_repo: &mut ScopeRepository,
        lines_include_newline: bool,
        fallback_name: Option<&str>,
    ) -> Result<SyntaxDefinition, ParseSyntaxError> {
        let h = doc.as_hash().ok_or(ParseSyntaxError::TypeMismatch)?;

        let mut variables = HashMap::new();
        if let Ok(map) = get_key(h, "variables", |x| x.as_hash()) {
            for (key, value) in map.iter() {
                if let (Some(key_str), Some(val_str)) = (key.as_str(), value.as_str()) {
                    variables.insert(key_str.to_owned(), val_str.to_owned());
                }
            }
        }
        let has_extends = get_key(h, "extends", Some).is_ok();
        let empty_contexts = Hash::new();
        let contexts_hash = match get_key(h, "contexts", |x| x.as_hash()) {
            Ok(hash) => hash,
            // extends-only syntaxes (e.g. "Batch File (Compound)") inherit
            // all contexts from their parent and have no `contexts:` key.
            Err(_) if has_extends => &empty_contexts,
            Err(e) => return Err(e),
        };
        let top_level_scope = scope_repo
            .build(get_key(h, "scope", |x| x.as_str())?)
            .map_err(ParseSyntaxError::InvalidScope)?;
        let version = get_key(h, "version", |x| x.as_i64()).unwrap_or(1) as u32;

        let mut state = ParserState {
            scope_repo,
            variables,
            variable_regex: Regex::new(r"\{\{([A-Za-z0-9_]+)\}\}".into()),
            backref_regex: Regex::new(r"\\\d".into()),
            lines_include_newline,
            version,
            defer_regex_validation: has_extends,
        };

        let mut contexts = SyntaxDefinition::parse_contexts(contexts_hash, &mut state)?;
        if !contexts.contains_key("main") && !has_extends {
            return Err(ParseSyntaxError::MainMissing);
        }

        if contexts.contains_key("main") {
            SyntaxDefinition::add_initial_contexts(&mut contexts, &mut state, top_level_scope);
        }

        let mut file_extensions = Vec::new();
        for extension_key in &["file_extensions", "hidden_file_extensions"] {
            if let Ok(v) = get_key(h, extension_key, |x| x.as_vec()) {
                file_extensions.extend(v.iter().filter_map(|y| y.as_str().map(|s| s.to_owned())))
            }
        }

        let extends = get_key(h, "extends", Some)
            .ok()
            .map(|y| {
                if let Some(s) = y.as_str() {
                    vec![s.to_owned()]
                } else if let Some(seq) = y.as_vec() {
                    seq.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_owned()))
                        .collect()
                } else {
                    vec![]
                }
            })
            .unwrap_or_default();

        let defn = SyntaxDefinition {
            name: get_key(h, "name", |x| x.as_str())
                .unwrap_or_else(|_| fallback_name.unwrap_or("Unnamed"))
                .to_owned(),
            scope: top_level_scope,
            file_extensions,
            // TODO maybe cache a compiled version of this Regex
            first_line_match: get_key(h, "first_line_match", |x| x.as_str())
                .ok()
                .map(|s| Self::resolve_variables(s, &state)),
            hidden: get_key(h, "hidden", |x| x.as_bool()).unwrap_or(false),

            variables: state.variables,
            contexts,
            extends,
            version,
        };
        Ok(defn)
    }

    fn parse_contexts(
        map: &Hash,
        state: &mut ParserState<'_>,
    ) -> Result<HashMap<String, Context>, ParseSyntaxError> {
        let mut contexts = HashMap::new();
        for (key, value) in map.iter() {
            if let (Some(name), Some(val_vec)) = (key.as_str(), value.as_vec()) {
                let mut namer = ContextNamer::new(name);
                SyntaxDefinition::parse_context(val_vec, state, &mut contexts, &mut namer)?;
            }
        }

        Ok(contexts)
    }

    fn parse_context(
        vec: &[Yaml],
        // TODO: Maybe just pass the scope repo if that's all that's needed?
        state: &mut ParserState<'_>,
        contexts: &mut HashMap<String, Context>,
        namer: &mut ContextNamer,
    ) -> Result<String, ParseSyntaxError> {
        // Every parsed context starts with `meta_include_prototype = None`
        // (unset). YAML-explicit `meta_include_prototype: <bool>` later
        // upgrades it to `Some(<bool>)`. The prototype context's own
        // self-attachment is suppressed by the `no_prototype` set in
        // `SyntaxSetBuilder::link_syntaxes`, so we don't need a distinct
        // initial value for prototype vs. non-prototype contexts.
        let mut context = Context::new(None);
        let name = namer.next();

        for y in vec.iter() {
            let map = y.as_hash().ok_or(ParseSyntaxError::TypeMismatch)?;

            let mut is_special = false;
            if let Ok(x) = get_key(map, "meta_scope", |x| x.as_str()) {
                context.meta_scope = str_to_scopes(x, state.scope_repo)?;
                is_special = true;
            }
            if let Ok(x) = get_key(map, "meta_content_scope", |x| x.as_str()) {
                context.meta_content_scope = str_to_scopes(x, state.scope_repo)?;
                is_special = true;
            }
            if let Ok(x) = get_key(map, "meta_include_prototype", |x| x.as_bool()) {
                context.meta_include_prototype = Some(x);
                is_special = true;
            }
            if let Ok(true) = get_key(map, "meta_prepend", |x| x.as_bool()) {
                context.merge_mode = ContextMergeMode::Prepend;
                is_special = true;
            }
            if let Ok(true) = get_key(map, "meta_append", |x| x.as_bool()) {
                context.merge_mode = ContextMergeMode::Append;
                is_special = true;
            }
            if let Ok(true) = get_key(map, "clear_scopes", |x| x.as_bool()) {
                context.clear_scopes = Some(ClearAmount::All);
                is_special = true;
            }
            if let Ok(x) = get_key(map, "clear_scopes", |x| x.as_i64()) {
                context.clear_scopes = Some(ClearAmount::TopN(x as usize));
                is_special = true;
            }
            if !is_special {
                if let Ok(x) = get_key(map, "include", Some) {
                    let reference =
                        SyntaxDefinition::parse_reference(x, state, contexts, namer, false)?;
                    let apply_prototype =
                        get_key(map, "apply_prototype", |x| x.as_bool()).unwrap_or(false);
                    if apply_prototype {
                        context
                            .patterns
                            .push(Pattern::IncludeWithPrototype(reference));
                    } else {
                        context.patterns.push(Pattern::Include(reference));
                    }
                } else {
                    let pattern =
                        SyntaxDefinition::parse_match_pattern(map, state, contexts, namer)?;
                    if pattern.has_captures {
                        context.uses_backrefs = true;
                    }
                    context.patterns.push(Pattern::Match(pattern));
                }
            }
        }

        contexts.insert(name.clone(), context);
        Ok(name)
    }

    fn parse_reference(
        y: &Yaml,
        state: &mut ParserState<'_>,
        contexts: &mut HashMap<String, Context>,
        namer: &mut ContextNamer,
        with_escape: bool,
    ) -> Result<ContextReference, ParseSyntaxError> {
        if let Some(s) = y.as_str() {
            let parts: Vec<&str> = s.split('#').collect();
            let sub_context = if parts.len() > 1 {
                Some(parts[1].to_owned())
            } else {
                None
            };
            if parts[0].starts_with("scope:") {
                Ok(ContextReference::ByScope {
                    scope: state
                        .scope_repo
                        .build(&parts[0][6..])
                        .map_err(ParseSyntaxError::InvalidScope)?,
                    sub_context,
                    with_escape,
                })
            } else if parts[0].ends_with(".sublime-syntax") {
                let stem = Path::new(parts[0])
                    .file_stem()
                    .and_then(|x| x.to_str())
                    .ok_or(ParseSyntaxError::BadFileRef)?;
                Ok(ContextReference::File {
                    name: stem.to_owned(),
                    sub_context,
                    with_escape,
                })
            } else {
                Ok(ContextReference::Named(parts[0].to_owned()))
            }
        } else if let Some(v) = y.as_vec() {
            let subname = SyntaxDefinition::parse_context(v, state, contexts, namer)?;
            Ok(ContextReference::Inline(subname))
        } else {
            Err(ParseSyntaxError::TypeMismatch)
        }
    }

    fn parse_match_pattern(
        map: &Hash,
        state: &mut ParserState<'_>,
        contexts: &mut HashMap<String, Context>,
        namer: &mut ContextNamer,
    ) -> Result<MatchPattern, ParseSyntaxError> {
        let raw_regex = get_key(map, "match", |x| x.as_str())?;
        let raw_regex_owned = raw_regex.to_owned();
        let regex_str = Self::parse_regex(raw_regex, state)?;

        let scope = get_key(map, "scope", |x| x.as_str())
            .ok()
            .map(|s| str_to_scopes(s, state.scope_repo))
            .unwrap_or_else(|| Ok(vec![]))?;

        let captures = if let Ok(map) = get_key(map, "captures", |x| x.as_hash()) {
            Some(Self::parse_captures(map, &regex_str, state)?)
        } else {
            None
        };

        let mut has_captures = false;
        let operation = if let Ok(y) = get_key(map, "pop", |y| {
            y.as_i64().or(match y.as_bool() {
                Some(true) => Some(1),
                _ => None,
            })
        }) {
            // Thanks @wbond for letting me know this is the correct way to check for captures
            has_captures = state
                .backref_regex
                .search(&regex_str, 0, regex_str.len(), None, true);
            // In Sublime Text, `pop: N` + `embed:` pops N contexts then pushes
            // the embedded syntax with escape priority.
            if get_key(map, "embed", Some).is_ok() {
                Self::parse_embed_op(map, state, contexts, namer, y as usize)?
            } else if let Ok(b) = get_key(map, "branch", |x| x.as_vec()) {
                let branch_point = get_key(map, "branch_point", |x| x.as_str())?;
                let alternatives: Vec<ContextReference> = b
                    .iter()
                    .map(|item| {
                        SyntaxDefinition::parse_reference(item, state, contexts, namer, false)
                    })
                    .collect::<Result<_, _>>()?;
                MatchOperation::Branch {
                    name: branch_point.to_owned(),
                    alternatives,
                    pop_count: y as usize,
                }
            } else if let Ok(s) = get_key(map, "set", Some) {
                // `pop: N + set: X` pops N contexts then pushes X.
                MatchOperation::Set {
                    ctx_refs: SyntaxDefinition::parse_pushargs(s, state, contexts, namer)?,
                    pop_count: y as usize,
                }
            } else if let Ok(p) = get_key(map, "push", Some) {
                // `pop: N + push: X` pops N contexts then pushes X. ST docs:
                // when `pop` is combined with another action, the pop happens
                // first and the matched text is treated as a lookahead — so
                // the trigger token does NOT inherit the popped frames'
                // meta_scope atoms. This is distinct from `pop: N + set:`,
                // which is the documented exception (stacking).
                //
                // Observed on Java's `pop: 2 + push: annotation-parameters-body`
                // (annotation arg lists) and `pop: 1 + push: case-label-expression`,
                // and on Python's `pop: 2 + push: function-parameter-list-body`
                // / `type-parameter-list-body`.
                MatchOperation::Push {
                    ctx_refs: SyntaxDefinition::parse_pushargs(p, state, contexts, namer)?,
                    pop_count: y as usize,
                }
            } else {
                MatchOperation::Pop(y as usize)
            }
        } else if let Ok(y) = get_key(map, "push", Some) {
            MatchOperation::Push {
                ctx_refs: SyntaxDefinition::parse_pushargs(y, state, contexts, namer)?,
                pop_count: 0,
            }
        } else if let Ok(y) = get_key(map, "set", Some) {
            MatchOperation::Set {
                ctx_refs: SyntaxDefinition::parse_pushargs(y, state, contexts, namer)?,
                pop_count: 1,
            }
        } else if let Ok(y) = get_key(map, "branch", |x| x.as_vec()) {
            let branch_point = get_key(map, "branch_point", |x| x.as_str())?;
            let alternatives: Vec<ContextReference> = y
                .iter()
                .map(|item| SyntaxDefinition::parse_reference(item, state, contexts, namer, false))
                .collect::<Result<_, _>>()?;
            MatchOperation::Branch {
                name: branch_point.to_owned(),
                alternatives,
                pop_count: 0,
            }
        } else if let Ok(y) = get_key(map, "fail", |x| x.as_str()) {
            MatchOperation::Fail(y.to_owned())
        } else if get_key(map, "embed", Some).is_ok() {
            Self::parse_embed_op(map, state, contexts, namer, 0)?
        } else {
            MatchOperation::None
        };

        let with_prototype = if let Ok(v) = get_key(map, "with_prototype", |x| x.as_vec()) {
            // should a with_prototype include the prototype? I don't think so.
            let subname = Self::parse_context(v, state, contexts, namer)?;
            Some(ContextReference::Inline(subname))
        } else {
            None
        };

        let pattern = MatchPattern::new_with_raw(
            has_captures,
            regex_str,
            raw_regex_owned,
            scope,
            captures,
            operation,
            with_prototype,
        );

        Ok(pattern)
    }

    fn parse_embed_op(
        map: &Hash,
        state: &mut ParserState<'_>,
        contexts: &mut HashMap<String, Context>,
        namer: &mut ContextNamer,
        pop_count: usize,
    ) -> Result<MatchOperation, ParseSyntaxError> {
        let y = get_key(map, "embed", Some)?;
        let v = get_key(map, "escape", Some)
            .map_err(|_| ParseSyntaxError::MissingMandatoryKey("escape"))?;

        let escape_raw = v.as_str().ok_or(ParseSyntaxError::TypeMismatch)?;
        let escape_regex_str = Self::parse_regex(escape_raw, state)?;
        let escape_has_captures =
            state
                .backref_regex
                .search(&escape_regex_str, 0, escape_regex_str.len(), None, true);

        let escape_captures = if let Ok(cap_map) = get_key(map, "escape_captures", |x| x.as_hash())
        {
            Some(Self::parse_captures(cap_map, &escape_regex_str, state)?)
        } else {
            None
        };

        let escape_info = EscapeInfo {
            escape_regex: Regex::new(escape_regex_str),
            has_captures: escape_has_captures,
            escape_captures,
            raw_escape_regex_str: Some(escape_raw.to_owned()),
        };

        // Build the wrapper context for embed_scope (meta_content_scope)
        // and the embedded context reference
        let mut embed_contexts = Vec::new();

        // Parse the target reference up front so the wrapper construction
        // below can tell whether the embed enters via the embedded syntax's
        // `main` (no fragment) or via a `#fragment`. Only the no-fragment
        // case needs `embed_scope_replaces=true`: that's the case where the
        // top-level scope auto-inserted into `main`'s mcs by `add_initial_contexts`
        // duplicates the wrapper's last embed_scope atom and the per-target
        // loop must suppress it. Fragment embeds enter a named context whose
        // mcs is independent of the syntax's top-level scope; suppressing it
        // strips real grammar atoms (e.g., TOML's `meta.mapping.toml`) and
        // downstream `clear_scopes:` then bites the wrapper instead of the
        // intended grammar atom. Observed on Python's PEP 723 inline TOML
        // (`embed: scope:source.toml.embedded.python#toml`).
        let target_ref = SyntaxDefinition::parse_reference(y, state, contexts, namer, true)?;
        let target_has_fragment = matches!(
            &target_ref,
            ContextReference::ByScope {
                sub_context: Some(_),
                ..
            } | ContextReference::File {
                sub_context: Some(_),
                ..
            }
        );

        // Create wrapper context with embed_scope if present
        let has_embed_scope = get_key(map, "embed_scope", Some).is_ok();
        if has_embed_scope {
            let mut embed_scope_context_yaml = vec![];
            let mut commands = Hash::new();
            commands.insert(
                Yaml::String("meta_include_prototype".to_string()),
                Yaml::Boolean(false),
            );
            embed_scope_context_yaml.push(Yaml::Hash(commands));
            if let Ok(s) = get_key(map, "embed_scope", Some) {
                let mut commands2 = Hash::new();
                commands2.insert(Yaml::String("meta_content_scope".to_string()), s.clone());
                embed_scope_context_yaml.push(Yaml::Hash(commands2));
            }
            // Add a match-all to pass through to next context
            let mut match_map = Hash::new();
            match_map.insert(
                Yaml::String("match".to_string()),
                Yaml::String(String::new()),
            );
            match_map.insert(Yaml::String("pop".to_string()), Yaml::Boolean(true));
            embed_scope_context_yaml.push(Yaml::Hash(match_map));
            let scope_ctx_name =
                SyntaxDefinition::parse_context(&embed_scope_context_yaml, state, contexts, namer)?;
            // In v2, embed_scope replaces the embedded syntax's scope — but
            // only when the embed enters via `main`. See the comment above
            // `target_has_fragment` for the rationale.
            if state.version >= 2 && !target_has_fragment {
                if let Some(ctx) = contexts.get_mut(&scope_ctx_name) {
                    ctx.embed_scope_replaces = true;
                }
            }
            embed_contexts.push(ContextReference::Inline(scope_ctx_name));
        }

        embed_contexts.push(target_ref);

        Ok(MatchOperation::Embed {
            contexts: embed_contexts,
            escape: escape_info,
            pop_count,
        })
    }

    fn parse_pushargs(
        y: &Yaml,
        state: &mut ParserState<'_>,
        contexts: &mut HashMap<String, Context>,
        namer: &mut ContextNamer,
    ) -> Result<Vec<ContextReference>, ParseSyntaxError> {
        // check for a push of multiple items
        if y.as_vec().is_some_and(|v| {
            !v.is_empty()
                && (v[0].as_str().is_some()
                    || (v[0].as_vec().is_some() && v[0].as_vec().unwrap()[0].as_hash().is_some()))
        }) {
            // this works because Result implements FromIterator to handle the errors
            y.as_vec()
                .unwrap()
                .iter()
                .map(|x| SyntaxDefinition::parse_reference(x, state, contexts, namer, false))
                .collect()
        } else {
            let reference = SyntaxDefinition::parse_reference(y, state, contexts, namer, false)?;
            Ok(vec![reference])
        }
    }

    fn parse_regex(raw_regex: &str, state: &ParserState<'_>) -> Result<String, ParseSyntaxError> {
        let regex = Self::resolve_variables(raw_regex, state);
        let regex = replace_posix_char_classes(regex);
        let regex = if state.lines_include_newline {
            regex_for_newlines(regex)
        } else {
            // If the passed in strings don't include newlines (unlike Sublime) we can't match on
            // them using the original regex. So this tries to rewrite the regex in a way that
            // allows matching against lines without newlines (essentially replacing `\n` with `$`).
            regex_for_no_newlines(regex)
        };
        if !state.defer_regex_validation {
            Self::try_compile_regex(&regex)?;
        }
        Ok(regex)
    }

    fn resolve_variables(raw_regex: &str, state: &ParserState<'_>) -> String {
        let mut result = String::new();
        let mut index = 0;
        let mut region = Region::new();
        while state.variable_regex.search(
            raw_regex,
            index,
            raw_regex.len(),
            Some(&mut region),
            true,
        ) {
            let (begin, end) = region.pos(0).unwrap();

            result.push_str(&raw_regex[index..begin]);

            let var_pos = region.pos(1).unwrap();
            let var_name = &raw_regex[var_pos.0..var_pos.1];
            let var_raw = state
                .variables
                .get(var_name)
                .map(String::as_ref)
                .unwrap_or("");
            let var_resolved = Self::resolve_variables(var_raw, state);
            result.push_str(&var_resolved);

            index = end;
        }
        if index < raw_regex.len() {
            result.push_str(&raw_regex[index..]);
        }
        result
    }

    fn try_compile_regex(regex_str: &str) -> Result<(), ParseSyntaxError> {
        // Replace backreferences with a placeholder value that will also appear in errors
        let regex_str =
            substitute_backrefs_in_regex(regex_str, |i| Some(format!("<placeholder_{}>", i)));

        if let Some(error) = Regex::try_compile(&regex_str) {
            Err(ParseSyntaxError::RegexCompileError(regex_str, error))
        } else {
            Ok(())
        }
    }

    fn parse_captures(
        map: &Hash,
        _regex_str: &str,
        state: &mut ParserState<'_>,
    ) -> Result<CaptureMapping, ParseSyntaxError> {
        // Accept every numeric capture entry. Groups inside lookarounds are
        // kept — `build_capture_ops` clips each capture's span to the rule's
        // consumed match range at parse time, mirroring Sublime Text. An
        // earlier version filtered lookaround-internal indices here on the
        // assumption "those scopes are not applied," which produced silent
        // drops for valid rules like C#'s generic function-call pattern.
        let mut captures = Vec::new();
        for (key, value) in map.iter() {
            if let (Some(key_int), Some(val_str)) = (key.as_i64(), value.as_str()) {
                if key_int < 0 {
                    continue;
                }
                captures.push((key_int as usize, str_to_scopes(val_str, state.scope_repo)?));
            }
        }
        Ok(captures)
    }

    /// Sublime treats the top level context slightly differently from
    /// including the main context from other syntaxes. When main is popped
    /// it is immediately re-added and when it is `set` over the file level
    /// scope remains. This behaviour is emulated through some added contexts
    /// that are the actual top level contexts used in parsing.
    /// See <https://github.com/trishume/syntect/issues/58> for more.
    pub(crate) fn add_initial_contexts(
        contexts: &mut HashMap<String, Context>,
        state: &mut ParserState<'_>,
        top_level_scope: Scope,
    ) {
        let yaml_docs = YamlLoader::load_from_str(START_CONTEXT).unwrap();
        let yaml = &yaml_docs[0];

        let start_yaml: &[Yaml] = yaml["__start"].as_vec().unwrap();
        SyntaxDefinition::parse_context(
            start_yaml,
            state,
            contexts,
            &mut ContextNamer::new("__start"),
        )
        .unwrap();
        if let Some(start) = contexts.get_mut("__start") {
            start.meta_content_scope = vec![top_level_scope];
        }

        let main_yaml: &[Yaml] = yaml["__main"].as_vec().unwrap();
        SyntaxDefinition::parse_context(
            main_yaml,
            state,
            contexts,
            &mut ContextNamer::new("__main"),
        )
        .unwrap();

        let meta_include_prototype = contexts["main"].meta_include_prototype;
        let meta_scope = contexts["main"].meta_scope.clone();
        // Copy `main`'s meta_content_scope to `__main`, but strip the
        // auto-inserted `top_level_scope` at position 0 if present.
        // On a fresh load `main.meta_content_scope` is still pre-insert
        // here, so this is a no-op. On a re-run (from `resolve_extends`
        // after a child inherits its parent's contexts), `main` already
        // carries `top_level_scope` from the first call; copying it to
        // `__main` would make both push the file scope at runtime,
        // producing duplicates like `[source.diff.git, source.diff.git]`
        // for Git Diff.
        let mut meta_content_scope = contexts["main"].meta_content_scope.clone();
        if meta_content_scope.first() == Some(&top_level_scope) {
            meta_content_scope.remove(0);
        }

        if let Some(outer_main) = contexts.get_mut("__main") {
            outer_main.meta_include_prototype = meta_include_prototype;
            outer_main.meta_scope = meta_scope;
            outer_main.meta_content_scope = meta_content_scope;
        }

        // add the top_level_scope as a meta_content_scope to main so
        // pushes from other syntaxes add the file scope.
        // Idempotent so a re-run (from `resolve_extends`) doesn't
        // double-insert — see the comment on the copy above.
        // TODO: this order is not quite correct if main also has a meta_scope
        if let Some(main) = contexts.get_mut("main") {
            if main.meta_content_scope.first() != Some(&top_level_scope) {
                main.meta_content_scope.insert(0, top_level_scope);
            }
        }
    }
}

/// Re-resolve a raw regex string with the given variables and newline mode.
/// Applies the full pipeline: resolve_variables → replace_posix → newlines → try_compile.
pub(crate) fn re_resolve_regex(
    raw: &str,
    variables: &HashMap<String, String>,
    lines_include_newline: bool,
) -> Result<String, ParseSyntaxError> {
    let variable_regex = Regex::new(r"\{\{([A-Za-z0-9_]+)\}\}".into());
    let state = ReResolveState {
        variables,
        variable_regex: &variable_regex,
    };
    let regex = re_resolve_variables(raw, &state);
    let regex = replace_posix_char_classes(regex);
    let regex = if lines_include_newline {
        regex_for_newlines(regex)
    } else {
        regex_for_no_newlines(regex)
    };
    SyntaxDefinition::try_compile_regex(&regex)?;
    Ok(regex)
}

struct ReResolveState<'a> {
    variables: &'a HashMap<String, String>,
    variable_regex: &'a Regex,
}

fn re_resolve_variables(raw_regex: &str, state: &ReResolveState<'_>) -> String {
    let mut result = String::new();
    let mut index = 0;
    let mut region = Region::new();
    while state
        .variable_regex
        .search(raw_regex, index, raw_regex.len(), Some(&mut region), true)
    {
        let (begin, end) = region.pos(0).unwrap();
        result.push_str(&raw_regex[index..begin]);

        let var_pos = region.pos(1).unwrap();
        let var_name = &raw_regex[var_pos.0..var_pos.1];
        let var_raw = state
            .variables
            .get(var_name)
            .map(String::as_ref)
            .unwrap_or("");
        let var_resolved = re_resolve_variables(var_raw, state);
        result.push_str(&var_resolved);

        index = end;
    }
    if index < raw_regex.len() {
        result.push_str(&raw_regex[index..]);
    }
    result
}

/// Re-resolve all regexes in a SyntaxDefinition that have a stored raw_regex_str.
/// This is used after merging variables during extends resolution.
pub(crate) fn re_resolve_all_regexes(
    syntax: &mut SyntaxDefinition,
    lines_include_newline: bool,
) -> Result<(), ParseSyntaxError> {
    for context in syntax.contexts.values_mut() {
        for pattern in &mut context.patterns {
            if let Pattern::Match(ref mut match_pat) = pattern {
                if let Some(ref raw) = match_pat.raw_regex_str {
                    let new_regex_str =
                        re_resolve_regex(raw, &syntax.variables, lines_include_newline)?;
                    match_pat.regex = Regex::new(new_regex_str);
                }
                // Also re-resolve the escape regex for embed operations
                if let MatchOperation::Embed { ref mut escape, .. } = match_pat.operation {
                    if let Some(ref raw) = escape.raw_escape_regex_str {
                        let new_regex_str =
                            re_resolve_regex(raw, &syntax.variables, lines_include_newline)?;
                        escape.escape_regex = Regex::new(new_regex_str);
                    }
                }
            }
        }
    }
    Ok(())
}

struct ContextNamer {
    name: String,
    anonymous_index: Option<usize>,
}

impl ContextNamer {
    fn new(name: &str) -> ContextNamer {
        ContextNamer {
            name: name.to_string(),
            anonymous_index: None,
        }
    }

    fn next(&mut self) -> String {
        let name = if let Some(index) = self.anonymous_index {
            format!("#anon_{}_{}", self.name, index)
        } else {
            self.name.clone()
        };

        self.anonymous_index = Some(self.anonymous_index.map(|i| i + 1).unwrap_or(0));
        name
    }
}

/// In fancy-regex, POSIX character classes only match ASCII characters.
///
/// Sublime's syntaxes expect them to match Unicode characters as well, so transform them to
/// corresponding Unicode character classes.
fn replace_posix_char_classes(regex: String) -> String {
    regex
        .replace("[:alpha:]", r"\p{L}")
        .replace("[:alnum:]", r"\p{L}\p{N}")
        .replace("[:lower:]", r"\p{Ll}")
        .replace("[:upper:]", r"\p{Lu}")
        .replace("[:digit:]", r"\p{Nd}")
}

/// Some of the regexes include `$` and expect it to match end of line,
/// e.g. *before* the `\n` in `test\n`.
///
/// In fancy-regex, `$` means end of text by default, so that would
/// match *after* `\n`. Using `(?m:$)` instead means it matches end of line.
///
/// Note that we don't want to add a `(?m)` in the beginning to change the
/// whole regex because that would also change the meaning of `^`. In
/// fancy-regex, that also matches at the end of e.g. `test\n` which is
/// different from onig. It would also change `.` to match more.
fn regex_for_newlines(regex: String) -> String {
    if !regex.contains('$') {
        return regex;
    }

    let rewriter = RegexRewriterForNewlines {
        parser: Parser::new(regex.as_bytes()),
    };
    rewriter.rewrite()
}

struct RegexRewriterForNewlines<'a> {
    parser: Parser<'a>,
}

impl RegexRewriterForNewlines<'_> {
    fn rewrite(mut self) -> String {
        let mut result = Vec::new();

        while let Some(c) = self.parser.peek() {
            match c {
                b'$' => {
                    self.parser.next();
                    result.extend_from_slice(br"(?m:$)");
                }
                b'\\' => {
                    self.parser.next();
                    result.push(c);
                    if let Some(c2) = self.parser.peek() {
                        self.parser.next();
                        result.push(c2);
                    }
                }
                b'[' => {
                    let (mut content, _) = self.parser.parse_character_class();
                    result.append(&mut content);
                }
                _ => {
                    self.parser.next();
                    result.push(c);
                }
            }
        }
        String::from_utf8(result).unwrap()
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
fn regex_for_no_newlines(regex: String) -> String {
    if !regex.contains(r"\n") {
        return regex;
    }

    // A special fix to rewrite a pattern from the `Rd` syntax that the RegexRewriter can not
    // handle properly.
    let regex = regex.replace("(?:\\n)?", "(?:$|)");

    let rewriter = RegexRewriterForNoNewlines {
        parser: Parser::new(regex.as_bytes()),
    };
    rewriter.rewrite()
}

struct RegexRewriterForNoNewlines<'a> {
    parser: Parser<'a>,
}

impl RegexRewriterForNoNewlines<'_> {
    fn rewrite(mut self) -> String {
        let mut result = Vec::new();
        while let Some(c) = self.parser.peek() {
            match c {
                b'\\' => {
                    self.parser.next();
                    if let Some(c2) = self.parser.peek() {
                        self.parser.next();
                        // Replacing `\n` with `$` in `\n?` or `\n+` would make parsing later fail
                        // with "target of repeat operator is invalid"
                        let c3 = self.parser.peek();
                        if c2 == b'n' && c3 != Some(b'?') && c3 != Some(b'+') && c3 != Some(b'*') {
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
                    let (mut content, matches_newline) = self.parser.parse_character_class();
                    if matches_newline && self.parser.peek() != Some(b'?') {
                        result.extend_from_slice(b"(?:");
                        result.append(&mut content);
                        result.extend_from_slice(br"|$)");
                    } else {
                        result.append(&mut content);
                    }
                }
                _ => {
                    self.parser.next();
                    result.push(c);
                }
            }
        }
        String::from_utf8(result).unwrap()
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    index: usize,
}

impl Parser<'_> {
    fn new(bytes: &[u8]) -> Parser<'_> {
        Parser { bytes, index: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }

    fn next(&mut self) {
        self.index += 1;
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
                    content.push(c);
                    if let Some(c2) = self.peek() {
                        self.next();
                        if c2 == b'n' && !negated && nesting == 0 {
                            matches_newline = true;
                        }
                        content.push(c2);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsing::Scope;

    #[test]
    fn can_parse() {
        let defn: SyntaxDefinition = SyntaxDefinition::load_from_str(
            "name: C\nscope: source.c\ncontexts: {main: []}",
            false,
            None,
        )
        .unwrap();
        assert_eq!(defn.name, "C");
        assert_eq!(defn.scope, Scope::new("source.c").unwrap());
        let exts_empty: Vec<String> = Vec::new();
        assert_eq!(defn.file_extensions, exts_empty);
        assert!(!defn.hidden);
        assert!(defn.variables.is_empty());
        let defn2: SyntaxDefinition = SyntaxDefinition::load_from_str(
            "
        name: C
        scope: source.c
        file_extensions: [c, h]
        hidden_file_extensions: [k, l]
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
            false,
            None,
        )
        .unwrap();
        assert_eq!(defn2.name, "C");
        let top_level_scope = Scope::new("source.c").unwrap();
        assert_eq!(defn2.scope, top_level_scope);
        let exts: Vec<String> = vec!["c", "h", "k", "l"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(defn2.file_extensions, exts);
        assert!(defn2.hidden);
        assert_eq!(defn2.variables.get("ident").unwrap(), "[QY]+");

        let n: Vec<Scope> = Vec::new();
        println!("{:?}", defn2);
        // unreachable!();
        let main = &defn2.contexts["main"];
        assert_eq!(main.meta_content_scope, vec![top_level_scope]);
        assert_eq!(main.meta_scope, n);
        assert!(main.meta_include_prototype.unwrap_or(true));

        assert_eq!(defn2.contexts["__main"].meta_content_scope, n);
        assert_eq!(
            defn2.contexts["__start"].meta_content_scope,
            vec![top_level_scope]
        );

        assert_eq!(
            defn2.contexts["string"].meta_scope,
            vec![Scope::new("string.quoted.double.c").unwrap()]
        );
        let first_pattern: &Pattern = &main.patterns[0];
        match *first_pattern {
            Pattern::Match(ref match_pat) => {
                let m: &CaptureMapping = match_pat.captures.as_ref().expect("test failed");
                assert_eq!(
                    &m[0],
                    &(1, vec![Scope::new("meta.preprocessor.c++").unwrap()])
                );
                use crate::parsing::syntax_definition::ContextReference::*;

                // this is sadly necessary because Context is not Eq because of the Regex
                let expected = MatchOperation::Push {
                    ctx_refs: vec![
                        Named("string".to_owned()),
                        ByScope {
                            scope: Scope::new("source.c").unwrap(),
                            sub_context: Some("main".to_owned()),
                            with_escape: false,
                        },
                        File {
                            name: "CSS".to_owned(),
                            sub_context: Some("rule-list-body".to_owned()),
                            with_escape: false,
                        },
                    ],
                    pop_count: 0,
                };
                assert_eq!(
                    format!("{:?}", match_pat.operation),
                    format!("{:?}", expected)
                );

                assert_eq!(
                    match_pat.scope,
                    vec![
                        Scope::new("keyword.control.c").unwrap(),
                        Scope::new("keyword.looping.c").unwrap()
                    ]
                );

                assert!(match_pat.with_prototype.is_some());
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn can_parse_embed_produces_embed_op() {
        let def = SyntaxDefinition::load_from_str(
            r#"
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
        "#,
            false,
            None,
        )
        .unwrap();

        // Verify the operation is Embed (not Push)
        let main_ctx = &def.contexts["main"];
        if let Pattern::Match(ref match_pattern) = main_ctx.patterns[0] {
            match match_pattern.operation {
                MatchOperation::Embed {
                    ref contexts,
                    ref escape,
                    pop_count,
                } => {
                    assert_eq!(pop_count, 0);
                    // Should have 2 contexts: wrapper for embed_scope + the embedded syntax
                    assert_eq!(contexts.len(), 2);
                    // First is the inline wrapper context
                    assert!(matches!(contexts[0], ContextReference::Inline(_)));
                    // Second is the scope reference
                    assert!(matches!(
                        contexts[1],
                        ContextReference::ByScope {
                            with_escape: true,
                            ..
                        }
                    ));
                    // Escape regex should be present
                    assert_eq!(escape.escape_regex.regex_str(), "(?i)(?=</style)");
                    assert!(!escape.has_captures);
                    assert!(escape.escape_captures.is_none());
                }
                _ => panic!(
                    "Expected Embed operation, got {:?}",
                    match_pattern.operation
                ),
            }
            // No with_prototype for embed (escape is native)
            assert!(match_pattern.with_prototype.is_none());
        } else {
            panic!("Expected Match pattern");
        }
    }

    #[test]
    fn errors_on_embed_without_escape() {
        let def = SyntaxDefinition::load_from_str(
            r#"
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
        "#,
            false,
            None,
        );
        assert!(def.is_err());
        match def.unwrap_err() {
            ParseSyntaxError::MissingMandatoryKey(key) => assert_eq!(key, "escape"),
            _ => unreachable!("Got unexpected ParseSyntaxError"),
        }
    }

    #[test]
    fn can_parse_pop_plus_embed() {
        let def = SyntaxDefinition::load_from_str(
            r#"
        name: Test
        scope: text.test
        file_extensions: [test]
        contexts:
          main:
            - match: '<script>'
              push: script-content
          script-content:
            - match: '>'
              pop: 1
              embed: scope:source.js
              embed_scope: source.js.embedded.html
              escape: '(?=</script>)'
        "#,
            false,
            None,
        )
        .unwrap();

        let ctx = &def.contexts["script-content"];
        if let Pattern::Match(ref match_pattern) = ctx.patterns[0] {
            match match_pattern.operation {
                MatchOperation::Embed {
                    ref contexts,
                    ref escape,
                    pop_count,
                } => {
                    assert_eq!(pop_count, 1);
                    // 2 contexts: embed_scope wrapper + scope reference
                    assert_eq!(contexts.len(), 2);
                    assert!(matches!(contexts[0], ContextReference::Inline(_)));
                    assert!(matches!(
                        contexts[1],
                        ContextReference::ByScope {
                            with_escape: true,
                            ..
                        }
                    ));
                    assert_eq!(escape.escape_regex.regex_str(), "(?=</script>)");
                }
                _ => panic!(
                    "Expected Embed operation, got {:?}",
                    match_pattern.operation
                ),
            }
        } else {
            panic!("Expected Match pattern");
        }
    }

    #[test]
    fn pop_plus_push_becomes_push_with_pop_count() {
        // `pop: N + push: X` is **lookahead** per ST docs ("when pop is
        // combined with another action, the pop happens first and the match
        // is treated as a lookahead"), distinct from `pop: N + set:` which
        // ST documents as the stacking exception. yaml_load preserves that
        // distinction by routing `pop:N + push:` to `Push { pop_count: N }`
        // and `pop:N + set:` to `Set { pop_count: N }`. See
        // `pop_plus_set_keeps_set_with_pop_count` for the sibling case.
        //
        // Real-world callers: Java's `pop: 2 + push: annotation-parameters-body`
        // (annotation arg lists), `pop: 1 + push: case-label-expression`;
        // Python's `pop: 2 + push: function-parameter-list-body` and
        // `type-parameter-list-body`.
        let def = SyntaxDefinition::load_from_str(
            r#"
        name: Test
        scope: text.test
        file_extensions: [test]
        contexts:
          main:
            - match: \(
              pop: 2
              push: target
          target:
            - meta_scope: meta.target
            - include: main
        "#,
            false,
            None,
        )
        .unwrap();

        let ctx = &def.contexts["main"];
        if let Pattern::Match(ref match_pattern) = ctx.patterns[0] {
            match match_pattern.operation {
                MatchOperation::Push {
                    ref ctx_refs,
                    pop_count,
                } => {
                    assert_eq!(pop_count, 2);
                    assert_eq!(ctx_refs.len(), 1);
                    assert!(matches!(
                        ctx_refs[0],
                        ContextReference::Named(ref n) if n == "target"
                    ));
                }
                _ => panic!(
                    "Expected Push operation with pop_count=2, got {:?}",
                    match_pattern.operation
                ),
            }
        } else {
            panic!("Expected Match pattern");
        }
    }

    #[test]
    fn errors_on_regex_compile_error() {
        let def = SyntaxDefinition::load_from_str(
            r#"
        name: C
        scope: source.c
        file_extensions: [test]
        contexts:
          main:
            - match: '[a'
              scope: keyword.name
        "#,
            false,
            None,
        );
        assert!(def.is_err());
        match def.unwrap_err() {
            ParseSyntaxError::RegexCompileError(ref regex, _) => assert_eq!("[a", regex),
            _ => unreachable!("Got unexpected ParseSyntaxError"),
        }
    }

    #[test]
    fn can_parse_ugly_yaml() {
        let defn: SyntaxDefinition = SyntaxDefinition::load_from_str(
            "
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
            false,
            None,
        )
        .unwrap();
        assert_eq!(defn.name, "LaTeX");
        let top_level_scope = Scope::new("text.tex.latex").unwrap();
        assert_eq!(defn.scope, top_level_scope);

        let first_pattern: &Pattern = &defn.contexts["main"].patterns[0];
        match *first_pattern {
            Pattern::Match(ref match_pat) => {
                let m: &CaptureMapping = match_pat.captures.as_ref().expect("test failed");
                assert_eq!(
                    &m[0],
                    &(1, vec![Scope::new("support.function.box.latex").unwrap()])
                );

                //use parsing::syntax_definition::ContextReference::*;
                // TODO: check the first pushed reference is Inline(...) and has a meta_scope of meta.function.box.latex
                // TODO: check the second pushed reference is Named("argument".to_owned())
                // TODO: check the third pushed reference is Named("optional-arguments".to_owned())

                assert!(match_pat.with_prototype.is_none());
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn names_anonymous_contexts() {
        let def = SyntaxDefinition::load_from_str(
            r#"
            scope: source.c
            contexts:
              main:
                - match: a
                  push: a
              a:
                - meta_scope: a
                - match: x
                  push:
                    - meta_scope: anonymous_x
                    - match: anything
                      push:
                        - meta_scope: anonymous_x_2
                - match: y
                  push:
                    - meta_scope: anonymous_y
                - match: z
                  escape: 'test'
            "#,
            false,
            None,
        )
        .unwrap();

        assert_eq!(def.contexts["a"].meta_scope, vec![Scope::new("a").unwrap()]);
        assert_eq!(
            def.contexts["#anon_a_0"].meta_scope,
            vec![Scope::new("anonymous_x").unwrap()]
        );
        assert_eq!(
            def.contexts["#anon_a_1"].meta_scope,
            vec![Scope::new("anonymous_x_2").unwrap()]
        );
        assert_eq!(
            def.contexts["#anon_a_2"].meta_scope,
            vec![Scope::new("anonymous_y").unwrap()]
        );
        // With native embed/escape, no synthetic escape context is created,
        // so #anon_a_3 should not exist.
        assert!(!def.contexts.contains_key("#anon_a_3"));
    }

    #[test]
    fn can_use_fallback_name() {
        let def = SyntaxDefinition::load_from_str(
            r#"
        scope: source.c
        contexts:
          main:
            - match: ''
        "#,
            false,
            Some("C"),
        );
        assert_eq!(def.unwrap().name, "C");
    }

    #[test]
    fn can_rewrite_regex_for_newlines() {
        fn rewrite(s: &str) -> String {
            regex_for_newlines(s.to_string())
        }

        assert_eq!(&rewrite(r"a"), r"a");
        assert_eq!(&rewrite(r"\b"), r"\b");
        assert_eq!(&rewrite(r"(a)"), r"(a)");
        assert_eq!(&rewrite(r"[a]"), r"[a]");
        assert_eq!(&rewrite(r"[^a]"), r"[^a]");
        assert_eq!(&rewrite(r"[]a]"), r"[]a]");
        assert_eq!(&rewrite(r"[[a]]"), r"[[a]]");

        assert_eq!(&rewrite(r"^"), r"^");
        assert_eq!(&rewrite(r"$"), r"(?m:$)");
        assert_eq!(&rewrite(r"^ab$"), r"^ab(?m:$)");
        assert_eq!(&rewrite(r"\^ab\$"), r"\^ab\$");
        assert_eq!(&rewrite(r"(//).*$"), r"(//).*(?m:$)");

        // Do not rewrite this `$` because it's in a char class and doesn't mean end of line
        assert_eq!(&rewrite(r"[a$]"), r"[a$]");
    }

    #[test]
    fn can_rewrite_regex_for_no_newlines() {
        fn rewrite(s: &str) -> String {
            regex_for_no_newlines(s.to_string())
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
        assert_eq!(&rewrite(r"a\n+"), r"a\n+");
        assert_eq!(&rewrite(r"a\n*"), r"a\n*");
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

    #[test]
    fn error_loading_syntax_with_unescaped_backslash() {
        let load_err = SyntaxDefinition::load_from_str(
            r#"
            name: Unescaped Backslash
            scope: source.c
            file_extensions: [test]
            contexts:
              main:
                - match: '\'
            "#,
            false,
            None,
        )
        .unwrap_err();
        match load_err {
            ParseSyntaxError::RegexCompileError(bad_regex, _) => assert_eq!(bad_regex, r"\"),
            _ => panic!("Unexpected error: {load_err}"),
        }
    }

    #[test]
    fn can_parse_extends_field() {
        let defn = SyntaxDefinition::load_from_str(
            r#"
            name: C++
            scope: source.c++
            extends: Packages/C/C.sublime-syntax
            contexts:
              main:
                - match: 'class'
                  scope: keyword.c++
            "#,
            false,
            None,
        )
        .unwrap();
        assert_eq!(defn.extends, vec!["Packages/C/C.sublime-syntax".to_owned()]);
    }

    #[test]
    fn can_parse_extends_without_main() {
        // A child with extends can omit the main context
        let defn = SyntaxDefinition::load_from_str(
            r#"
            name: C++ Extra
            scope: source.c++
            extends: Packages/C/C.sublime-syntax
            contexts:
              extra:
                - match: 'extra'
                  scope: keyword.extra
            "#,
            false,
            None,
        )
        .unwrap();
        assert_eq!(defn.extends, vec!["Packages/C/C.sublime-syntax".to_owned()]);
        assert!(!defn.contexts.contains_key("main"));
    }

    #[test]
    fn can_parse_version_field() {
        let defn = SyntaxDefinition::load_from_str(
            r#"
            name: V2 Test
            scope: source.v2
            version: 2
            contexts:
              main:
                - match: 'test'
                  scope: keyword.test
            "#,
            false,
            None,
        )
        .unwrap();
        assert_eq!(defn.version, 2);
    }

    #[test]
    fn version_defaults_to_1() {
        let defn = SyntaxDefinition::load_from_str(
            "name: V1\nscope: source.v1\ncontexts: {main: []}",
            false,
            None,
        )
        .unwrap();
        assert_eq!(defn.version, 1);
    }

    #[test]
    fn can_parse_meta_prepend() {
        let defn = SyntaxDefinition::load_from_str(
            r#"
            name: Test
            scope: source.test
            extends: Packages/Base/Base.sublime-syntax
            contexts:
              main:
                - meta_prepend: true
                - match: 'prepended'
                  scope: keyword.prepended
            "#,
            false,
            None,
        )
        .unwrap();
        let main = &defn.contexts["main"];
        assert_eq!(main.merge_mode, ContextMergeMode::Prepend);
    }

    #[test]
    fn can_parse_meta_append() {
        let defn = SyntaxDefinition::load_from_str(
            r#"
            name: Test
            scope: source.test
            extends: Packages/Base/Base.sublime-syntax
            contexts:
              main:
                - meta_append: true
                - match: 'appended'
                  scope: keyword.appended
            "#,
            false,
            None,
        )
        .unwrap();
        let main = &defn.contexts["main"];
        assert_eq!(main.merge_mode, ContextMergeMode::Append);
    }

    #[test]
    fn can_parse_apply_prototype() {
        let defn = SyntaxDefinition::load_from_str(
            r#"
            name: Test
            scope: source.test
            contexts:
              main:
                - include: scope:source.other
                  apply_prototype: true
            "#,
            false,
            None,
        )
        .unwrap();
        let main = &defn.contexts["main"];
        assert_eq!(main.patterns.len(), 1);
        match &main.patterns[0] {
            Pattern::IncludeWithPrototype(_) => {}
            other => panic!("Expected IncludeWithPrototype, got {:?}", other),
        }
    }

    #[test]
    fn stores_raw_regex_str() {
        let defn = SyntaxDefinition::load_from_str(
            r#"
            name: Test
            scope: source.test
            variables:
              ident: '[a-z]+'
            contexts:
              main:
                - match: '{{ident}}'
                  scope: variable.test
            "#,
            false,
            None,
        )
        .unwrap();
        let main = &defn.contexts["main"];
        match &main.patterns[0] {
            Pattern::Match(mp) => {
                assert_eq!(mp.raw_regex_str.as_deref(), Some("{{ident}}"));
            }
            _ => panic!("Expected Match pattern"),
        }
    }
}
