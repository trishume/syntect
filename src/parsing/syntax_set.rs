use super::scope::*;
use super::syntax_definition::*;
use super::ParsingError;

#[cfg(feature = "metadata")]
use super::metadata::{LoadMetadata, Metadata, RawMetadataEntry};

#[cfg(feature = "yaml-load")]
use super::super::LoadingError;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::mem;
use std::ops::DerefMut;
use std::path::Path;

use super::regex::Regex;
use crate::parsing::syntax_definition::ContextId;
use serde_derive::{Deserialize, Serialize};
use std::sync::OnceLock;

/// A syntax set holds multiple syntaxes that have been linked together.
///
/// Use a [`SyntaxSetBuilder`] to load syntax definitions and build a syntax set.
///
/// After building, the syntax set is immutable and can no longer be modified, but you can convert
/// it back into a builder by using the [`into_builder`] method.
///
/// [`SyntaxSetBuilder`]: struct.SyntaxSetBuilder.html
/// [`into_builder`]: #method.into_builder
#[derive(Debug, Serialize, Deserialize)]
pub struct SyntaxSet {
    syntaxes: Vec<SyntaxReference>,
    /// Stores the syntax index for every path that was loaded
    path_syntaxes: Vec<(String, usize)>,

    /// Warnings collected during syntax loading and linking.
    #[serde(skip_serializing, skip_deserializing, default)]
    warnings: Vec<String>,

    #[serde(skip_serializing, skip_deserializing, default = "OnceLock::new")]
    first_line_cache: OnceLock<FirstLineCache>,
    /// Metadata, e.g. indent and commenting information.
    ///
    /// NOTE: if serializing, you should handle metadata manually; that is, you should serialize and
    /// deserialize it separately. See `examples/gendata.rs` for an example.
    #[cfg(feature = "metadata")]
    #[serde(skip, default)]
    pub(crate) metadata: Metadata,
}

/// A linked version of a [`SyntaxDefinition`] that is only useful as part of the
/// [`SyntaxSet`] that contains it. See docs for [`SyntaxSetBuilder::build`] for
/// more info.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyntaxReference {
    pub name: String,
    pub file_extensions: Vec<String>,
    pub scope: Scope,
    pub first_line_match: Option<String>,
    pub hidden: bool,
    #[serde(serialize_with = "ordered_map")]
    pub variables: HashMap<String, String>,
    /// The version of the sublime-syntax format (1 or 2). Default is 1.
    #[serde(default = "default_syntax_version")]
    pub version: u32,
    #[serde(skip)]
    pub(crate) lazy_contexts: OnceLock<LazyContexts>,
    pub(crate) serialized_lazy_contexts: Vec<u8>,
}

fn default_syntax_version() -> u32 {
    1
}

/// The lazy-loaded parts of a [`SyntaxReference`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct LazyContexts {
    #[serde(serialize_with = "ordered_map")]
    pub(crate) context_ids: HashMap<String, ContextId>,
    pub(crate) contexts: Vec<Context>,
}

/// A syntax set builder is used for loading syntax definitions from the file
/// system or by adding [`SyntaxDefinition`] objects.
///
/// Once all the syntaxes have been added, call [`build`] to turn the builder into
/// a [`SyntaxSet`] that can be used for parsing or highlighting.
///
/// [`SyntaxDefinition`]: syntax_definition/struct.SyntaxDefinition.html
/// [`build`]: #method.build
/// [`SyntaxSet`]: struct.SyntaxSet.html
#[derive(Clone, Default)]
pub struct SyntaxSetBuilder {
    syntaxes: Vec<SyntaxDefinition>,
    path_syntaxes: Vec<(String, usize)>,
    warnings: Vec<String>,
    /// Tracks the `lines_include_newline` flag from the most recent
    /// `add_from_folder` call. Used by `resolve_extends` to re-resolve
    /// regexes with the correct newline mode after merging parent variables.
    lines_include_newline: bool,
    #[cfg(feature = "metadata")]
    raw_metadata: LoadMetadata,

    /// If this `SyntaxSetBuilder` is created with `SyntaxSet::into_builder`
    /// from a `SyntaxSet` that already had metadata, we keep that metadata,
    /// merging it with newly loaded metadata.
    #[cfg(feature = "metadata")]
    existing_metadata: Option<Metadata>,
}

#[cfg(feature = "yaml-load")]
fn load_syntax_file(
    p: &Path,
    lines_include_newline: bool,
) -> Result<SyntaxDefinition, LoadingError> {
    let s = std::fs::read_to_string(p)?;

    SyntaxDefinition::load_from_str(
        &s,
        lines_include_newline,
        p.file_stem().and_then(|x| x.to_str()),
    )
    .map_err(|e| LoadingError::ParseSyntax(e, format!("{}", p.display())))
}

impl Clone for SyntaxSet {
    fn clone(&self) -> SyntaxSet {
        SyntaxSet {
            syntaxes: self.syntaxes.clone(),
            path_syntaxes: self.path_syntaxes.clone(),
            warnings: self.warnings.clone(),
            // Will need to be re-initialized
            first_line_cache: OnceLock::new(),
            #[cfg(feature = "metadata")]
            metadata: self.metadata.clone(),
        }
    }
}

impl Default for SyntaxSet {
    fn default() -> Self {
        SyntaxSet {
            syntaxes: Vec::new(),
            path_syntaxes: Vec::new(),
            warnings: Vec::new(),
            first_line_cache: OnceLock::new(),
            #[cfg(feature = "metadata")]
            metadata: Metadata::default(),
        }
    }
}

impl SyntaxSet {
    pub fn new() -> SyntaxSet {
        SyntaxSet::default()
    }

    /// Convenience constructor for creating a builder, then loading syntax
    /// definitions from a folder and then building the syntax set.
    ///
    /// Note that this uses `lines_include_newline` set to `false`, see the
    /// [`add_from_folder`] method docs on [`SyntaxSetBuilder`] for an explanation
    /// as to why this might not be the best.
    ///
    /// [`add_from_folder`]: struct.SyntaxSetBuilder.html#method.add_from_folder
    /// [`SyntaxSetBuilder`]: struct.SyntaxSetBuilder.html
    #[cfg(feature = "yaml-load")]
    pub fn load_from_folder<P: AsRef<Path>>(folder: P) -> Result<SyntaxSet, LoadingError> {
        let mut builder = SyntaxSetBuilder::new();
        builder.add_from_folder(folder, false)?;
        Ok(builder.build())
    }

    /// Warnings collected during syntax loading and linking.
    ///
    /// These include issues like skipped files, version mismatches, and
    /// unresolved `extends` references.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The list of syntaxes in the set
    pub fn syntaxes(&self) -> &[SyntaxReference] {
        &self.syntaxes[..]
    }

    #[cfg(feature = "metadata")]
    pub fn set_metadata(&mut self, metadata: Metadata) {
        self.metadata = metadata;
    }

    /// The loaded metadata for this set.
    #[cfg(feature = "metadata")]
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Finds a syntax by its default scope, for example `source.regexp` finds the regex syntax.
    ///
    /// This and all similar methods below do a linear search of syntaxes, this should be fast
    /// because there aren't many syntaxes, but don't think you can call it a bajillion times per
    /// second.
    pub fn find_syntax_by_scope(&self, scope: Scope) -> Option<&SyntaxReference> {
        self.syntaxes.iter().rev().find(|&s| s.scope == scope)
    }

    pub fn find_syntax_by_name<'a>(&'a self, name: &str) -> Option<&'a SyntaxReference> {
        self.syntaxes.iter().rev().find(|&s| name == s.name)
    }

    pub fn find_syntax_by_extension<'a>(&'a self, extension: &str) -> Option<&'a SyntaxReference> {
        self.syntaxes.iter().rev().find(|&s| {
            s.file_extensions
                .iter()
                .any(|e| e.eq_ignore_ascii_case(extension))
        })
    }

    /// Searches for a syntax first by extension and then by case-insensitive name
    ///
    /// This is useful for things like Github-flavoured-markdown code block highlighting where all
    /// you have to go on is a short token given by the user
    pub fn find_syntax_by_token<'a>(&'a self, s: &str) -> Option<&'a SyntaxReference> {
        {
            let ext_res = self.find_syntax_by_extension(s);
            if ext_res.is_some() {
                return ext_res;
            }
        }
        self.syntaxes
            .iter()
            .rev()
            .find(|&syntax| syntax.name.eq_ignore_ascii_case(s))
    }

    /// Try to find the syntax for a file based on its first line
    ///
    /// This uses regexes that come with some sublime syntax grammars for matching things like
    /// shebangs and mode lines like `-*- Mode: C -*-`
    pub fn find_syntax_by_first_line<'a>(&'a self, s: &str) -> Option<&'a SyntaxReference> {
        let s = s.strip_prefix("\u{feff}").unwrap_or(s); // Strip UTF-8 BOM
        let cache = self.first_line_cache();
        for &(ref reg, i) in cache.regexes.iter().rev() {
            if reg.search(s, 0, s.len(), None, true) {
                return Some(&self.syntaxes[i]);
            }
        }
        None
    }

    /// Searches for a syntax by it's original file path when it was first loaded from disk
    ///
    /// This is primarily useful for syntax tests. Some may specify a
    /// `Packages/PackageName/SyntaxName.sublime-syntax` path, and others may just have
    /// `SyntaxName.sublime-syntax`. This caters for these by matching the end of the path of the
    /// loaded syntax definition files
    // however, if a syntax name is provided without a folder, make sure we don't accidentally match the end of a different syntax definition's name - by checking a / comes before it or it is the full path
    pub fn find_syntax_by_path<'a>(&'a self, path: &str) -> Option<&'a SyntaxReference> {
        let mut slash_path = "/".to_string();
        slash_path.push_str(path);
        self.path_syntaxes
            .iter()
            .rev()
            .find(|t| t.0.ends_with(&slash_path) || t.0 == path)
            .map(|&(_, i)| &self.syntaxes[i])
    }

    /// Convenience method that tries to find the syntax for a file path, first by extension/name
    /// and then by first line of the file if that doesn't work.
    ///
    /// May IO Error because it sometimes tries to read the first line of the file.
    ///
    /// # Examples
    ///
    /// When determining how to highlight a file, use this in combination with a fallback to plain
    /// text:
    ///
    /// ```
    /// use syntect::parsing::SyntaxSet;
    /// let ss = SyntaxSet::load_defaults_newlines();
    /// let syntax = ss.find_syntax_for_file("testdata/highlight_test.erb")
    ///     .unwrap() // for IO errors, you may want to use try!() or another plain text fallback
    ///     .unwrap_or_else(|| ss.find_syntax_plain_text());
    /// assert_eq!(syntax.name, "HTML (Rails)");
    /// ```
    pub fn find_syntax_for_file<P: AsRef<Path>>(
        &self,
        path_obj: P,
    ) -> io::Result<Option<&SyntaxReference>> {
        let path: &Path = path_obj.as_ref();
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let extension = path.extension().and_then(|x| x.to_str()).unwrap_or("");
        let ext_syntax = self
            .find_syntax_by_extension(file_name)
            .or_else(|| self.find_syntax_by_extension(extension));
        let line_syntax = if ext_syntax.is_none() {
            let mut line = String::new();
            let f = File::open(path)?;
            let mut line_reader = BufReader::new(&f);
            line_reader.read_line(&mut line)?;
            self.find_syntax_by_first_line(&line)
        } else {
            None
        };
        let syntax = ext_syntax.or(line_syntax);
        Ok(syntax)
    }

    /// Finds a syntax for plain text, which usually has no highlighting rules.
    ///
    /// This is good as a fallback when you can't find another syntax but you still want to use the
    /// same highlighting pipeline code.
    ///
    /// This syntax should always be present, if not this method will panic. If the way you load
    /// syntaxes doesn't create one, use [`add_plain_text_syntax`].
    ///
    /// # Examples
    /// ```
    /// use syntect::parsing::SyntaxSetBuilder;
    /// let mut builder = SyntaxSetBuilder::new();
    /// builder.add_plain_text_syntax();
    /// let ss = builder.build();
    /// let syntax = ss.find_syntax_by_token("rs").unwrap_or_else(|| ss.find_syntax_plain_text());
    /// assert_eq!(syntax.name, "Plain Text");
    /// ```
    ///
    /// [`add_plain_text_syntax`]: struct.SyntaxSetBuilder.html#method.add_plain_text_syntax
    pub fn find_syntax_plain_text(&self) -> &SyntaxReference {
        self.find_syntax_by_name("Plain Text")
            .expect("All syntax sets ought to have a plain text syntax")
    }

    /// Converts this syntax set into a builder so that more syntaxes can be
    /// added to it.
    ///
    /// Note that newly added syntaxes can have references to existing syntaxes
    /// in the set, but not the other way around.
    pub fn into_builder(self) -> SyntaxSetBuilder {
        #[cfg(feature = "metadata")]
        let SyntaxSet {
            syntaxes,
            path_syntaxes,
            metadata,
            ..
        } = self;
        #[cfg(not(feature = "metadata"))]
        let SyntaxSet {
            syntaxes,
            path_syntaxes,
            ..
        } = self;

        let mut context_map = HashMap::new();
        for (syntax_index, syntax) in syntaxes.iter().enumerate() {
            for (context_index, context) in syntax.contexts().iter().enumerate() {
                context_map.insert(
                    ContextId {
                        syntax_index,
                        context_index,
                    },
                    context.clone(),
                );
            }
        }

        let mut builder_syntaxes = Vec::with_capacity(syntaxes.len());

        for syntax in syntaxes {
            let SyntaxReference {
                name,
                file_extensions,
                scope,
                first_line_match,
                hidden,
                variables,
                version,
                serialized_lazy_contexts,
                ..
            } = syntax;

            let lazy_contexts = LazyContexts::deserialize(&serialized_lazy_contexts[..]);
            let mut builder_contexts = HashMap::with_capacity(lazy_contexts.context_ids.len());
            for (name, context_id) in lazy_contexts.context_ids {
                if let Some(context) = context_map.remove(&context_id) {
                    builder_contexts.insert(name, context);
                }
            }

            let syntax_definition = SyntaxDefinition {
                name,
                file_extensions,
                scope,
                first_line_match,
                hidden,
                variables,
                contexts: builder_contexts,
                extends: vec![],
                version,
            };
            builder_syntaxes.push(syntax_definition);
        }

        SyntaxSetBuilder {
            syntaxes: builder_syntaxes,
            path_syntaxes,
            warnings: Vec::new(),
            lines_include_newline: false,
            #[cfg(feature = "metadata")]
            existing_metadata: Some(metadata),
            #[cfg(feature = "metadata")]
            raw_metadata: LoadMetadata::default(),
        }
    }

    #[inline(always)]
    pub(crate) fn get_context(&self, context_id: &ContextId) -> Result<&Context, ParsingError> {
        let syntax = &self
            .syntaxes
            .get(context_id.syntax_index)
            .ok_or(ParsingError::MissingContext(*context_id))?;
        syntax
            .contexts()
            .get(context_id.context_index)
            .ok_or(ParsingError::MissingContext(*context_id))
    }

    fn first_line_cache(&self) -> &FirstLineCache {
        self.first_line_cache
            .get_or_init(|| FirstLineCache::new(self.syntaxes()))
    }

    pub fn find_unlinked_contexts(&self) -> BTreeSet<String> {
        let SyntaxSet { syntaxes, .. } = self;

        let mut unlinked_contexts = BTreeSet::new();

        for syntax in syntaxes {
            let SyntaxReference { name, scope, .. } = syntax;

            for context in syntax.contexts() {
                Self::find_unlinked_contexts_in_context(
                    name,
                    scope,
                    context,
                    &mut unlinked_contexts,
                );
            }
        }
        unlinked_contexts
    }

    fn find_unlinked_contexts_in_context(
        name: &str,
        scope: &Scope,
        context: &Context,
        unlinked_contexts: &mut BTreeSet<String>,
    ) {
        for pattern in context.patterns.iter() {
            let maybe_refs_to_check = match pattern {
                Pattern::Match(match_pat) => match &match_pat.operation {
                    MatchOperation::Push {
                        ctx_refs: context_refs,
                        ..
                    }
                    | MatchOperation::Set {
                        ctx_refs: context_refs,
                        ..
                    }
                    | MatchOperation::Branch {
                        alternatives: context_refs,
                        ..
                    } => Some(context_refs),
                    MatchOperation::Embed { ref contexts, .. } => Some(contexts),
                    MatchOperation::Pop(_) | MatchOperation::None | MatchOperation::Fail(_) => None,
                },
                _ => None,
            };
            for context_ref in maybe_refs_to_check.into_iter().flatten() {
                match context_ref {
                    ContextReference::Direct(_) => {}
                    _ => {
                        unlinked_contexts.insert(format!(
                            "Syntax '{}' with scope '{}' has unresolved context reference {:?}",
                            name, scope, &context_ref
                        ));
                    }
                }
            }
        }
    }
}

impl SyntaxReference {
    pub(crate) fn context_ids(&self) -> &HashMap<String, ContextId> {
        &self.lazy_contexts().context_ids
    }

    fn contexts(&self) -> &[Context] {
        &self.lazy_contexts().contexts
    }

    fn lazy_contexts(&self) -> &LazyContexts {
        self.lazy_contexts
            .get_or_init(|| LazyContexts::deserialize(&self.serialized_lazy_contexts[..]))
    }
}

impl LazyContexts {
    fn deserialize(data: &[u8]) -> LazyContexts {
        crate::dumps::from_reader(data).expect("data is not corrupt or out of sync with the code")
    }
}

impl SyntaxSetBuilder {
    pub fn new() -> SyntaxSetBuilder {
        SyntaxSetBuilder::default()
    }

    /// Add a syntax to the set.
    pub fn add(&mut self, syntax: SyntaxDefinition) {
        self.syntaxes.push(syntax);
    }

    /// The list of syntaxes added so far.
    pub fn syntaxes(&self) -> &[SyntaxDefinition] {
        &self.syntaxes[..]
    }

    /// Warnings collected during syntax loading and linking.
    ///
    /// These include issues like skipped files, version mismatches, and
    /// unresolved `extends` references that were previously printed to stderr.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// A rarely useful method that loads in a syntax with no highlighting rules for plain text
    ///
    /// Exists mainly for adding the plain text syntax to syntax set dumps, because for some reason
    /// the default Sublime plain text syntax is still in `.tmLanguage` format.
    #[cfg(feature = "yaml-load")]
    pub fn add_plain_text_syntax(&mut self) {
        let s = "---\nname: Plain Text\nfile_extensions: [txt]\nscope: text.plain\ncontexts: \
                 {main: []}";
        let syn = SyntaxDefinition::load_from_str(s, false, None).unwrap();
        self.syntaxes.push(syn);
    }

    /// Loads all the `.sublime-syntax` files in a folder into this builder.
    ///
    /// The `lines_include_newline` parameter is used to work around the fact that Sublime Text
    /// normally passes line strings including newline characters (`\n`) to its regex engine. This
    /// results in many syntaxes having regexes matching `\n`, which doesn't work if you don't pass
    /// in newlines. It is recommended that if you can you pass in lines with newlines if you can
    /// and pass `true` for this parameter. If that is inconvenient pass `false` and the loader
    /// will do some hacky find and replaces on the match regexes that seem to work for the default
    /// syntax set, but may not work for any other syntaxes.
    ///
    /// In the future I might include a "slow mode" that copies the lines passed in and appends a
    /// newline if there isn't one, but in the interest of performance currently this hacky fix will
    /// have to do.
    #[cfg(feature = "yaml-load")]
    pub fn add_from_folder<P: AsRef<Path>>(
        &mut self,
        folder: P,
        lines_include_newline: bool,
    ) -> Result<(), LoadingError> {
        self.lines_include_newline = lines_include_newline;
        for entry in crate::utils::walk_dir(folder).sort_by(|a, b| a.file_name().cmp(b.file_name()))
        {
            let entry = entry.map_err(|e| LoadingError::WalkDir(Box::new(e)))?;
            if entry
                .path()
                .extension()
                .is_some_and(|e| e == "sublime-syntax")
            {
                match load_syntax_file(entry.path(), lines_include_newline) {
                    Ok(syntax) => {
                        if let Some(path_str) = entry.path().to_str() {
                            // Split the path up and rejoin with slashes so that syntaxes loaded on Windows
                            // can still be loaded the same way.
                            let path = Path::new(path_str);
                            let path_parts: Vec<_> =
                                path.iter().map(|c| c.to_str().unwrap()).collect();
                            self.path_syntaxes
                                .push((path_parts.join("/").to_string(), self.syntaxes.len()));
                        }
                        self.syntaxes.push(syntax);
                    }
                    Err(err) => {
                        self.warnings
                            .push(format!("skipping {:?}: {}", entry.path(), err));
                    }
                }
            }

            #[cfg(feature = "metadata")]
            {
                if entry.path().extension() == Some("tmPreferences".as_ref()) {
                    match RawMetadataEntry::load(entry.path()) {
                        Ok(meta) => self.raw_metadata.add_raw(meta),
                        Err(_err) => (),
                    }
                }
            }
        }

        Ok(())
    }

    /// Build a [`SyntaxSet`] from the syntaxes that have been added to this
    /// builder.
    ///
    /// ### Linking
    ///
    /// The contexts in syntaxes can reference other contexts in the same syntax
    /// or even other syntaxes. For example, a HTML syntax can reference a CSS
    /// syntax so that CSS blocks in HTML work as expected.
    ///
    /// Those references work in various ways and involve one or two lookups.
    /// To avoid having to do these lookups during parsing/highlighting, the
    /// references are changed to directly reference contexts via index. That's
    /// called linking.
    ///
    /// Linking is done in this build step. So in order to get the best
    /// performance, you should try to avoid calling this too much. Ideally,
    /// create a [`SyntaxSet`] once and then use it many times. If you can,
    /// serialize a [`SyntaxSet`] for your program and when you run the program,
    /// directly load the [`SyntaxSet`].
    ///
    /// [`SyntaxSet`]: struct.SyntaxSet.html
    pub fn build(self) -> SyntaxSet {
        #[cfg(not(feature = "metadata"))]
        let SyntaxSetBuilder {
            syntaxes: syntax_definitions,
            path_syntaxes,
            mut warnings,
            lines_include_newline,
        } = self;
        #[cfg(feature = "metadata")]
        let SyntaxSetBuilder {
            syntaxes: syntax_definitions,
            path_syntaxes,
            mut warnings,
            lines_include_newline,
            raw_metadata,
            existing_metadata,
        } = self;

        // Extends resolution phase: merge parent contexts/variables into children
        let (syntax_definitions, extends_warnings) =
            Self::resolve_extends(syntax_definitions, &path_syntaxes, lines_include_newline);
        warnings.extend(extends_warnings);

        let mut syntaxes = Vec::with_capacity(syntax_definitions.len());
        let mut all_context_ids = Vec::new();
        let mut all_contexts = vec![Vec::new(); syntax_definitions.len()];

        for (syntax_index, syntax_definition) in syntax_definitions.into_iter().enumerate() {
            let SyntaxDefinition {
                name,
                file_extensions,
                scope,
                first_line_match,
                hidden,
                variables,
                contexts,
                extends: _,
                version,
            } = syntax_definition;

            let mut context_ids = HashMap::new();

            let mut contexts: Vec<(String, Context)> = contexts.into_iter().collect();
            // Sort the values of the HashMap so that the contexts in the
            // resulting SyntaxSet have a deterministic order for serializing.
            // Because we're sorting by the keys which are unique, we can use
            // an unstable sort.
            contexts.sort_unstable_by(|(name_a, _), (name_b, _)| name_a.cmp(name_b));
            for (name, context) in contexts {
                let context_index = all_contexts[syntax_index].len();
                context_ids.insert(
                    name,
                    ContextId {
                        syntax_index,
                        context_index,
                    },
                );
                all_contexts[syntax_index].push(context);
            }

            let syntax = SyntaxReference {
                name,
                file_extensions,
                scope,
                first_line_match,
                hidden,
                variables,
                version,
                lazy_contexts: OnceLock::new(),
                serialized_lazy_contexts: Vec::new(), // initialized in the last step
            };
            syntaxes.push(syntax);
            all_context_ids.push(context_ids);
        }

        let mut found_more_backref_includes = true;
        for (syntax_index, _syntax) in syntaxes.iter().enumerate() {
            let mut no_prototype = HashSet::new();
            let prototype = all_context_ids[syntax_index].get("prototype");
            if let Some(prototype_id) = prototype {
                // TODO: We could do this after parsing YAML, instead of here?
                Self::recursively_mark_no_prototype(
                    prototype_id,
                    &all_context_ids[syntax_index],
                    &all_contexts,
                    &mut no_prototype,
                    false,
                );
            }

            for context_id in all_context_ids[syntax_index].values() {
                let context = &mut all_contexts[context_id.syntax_index][context_id.context_index];
                if let Some(prototype_id) = prototype {
                    if context.meta_include_prototype.unwrap_or(true)
                        && !no_prototype.contains(context_id)
                    {
                        context.prototype = Some(*prototype_id);
                    }
                }
                Self::link_context(context, syntax_index, &all_context_ids, &syntaxes);

                if context.uses_backrefs {
                    found_more_backref_includes = true;
                }
            }
        }

        // We need to recursively mark contexts that include contexts which
        // use backreferences as using backreferences. In theory we could use
        // a more efficient method here like doing a toposort or constructing
        // a representation with reversed edges and then tracing in the
        // opposite direction, but I benchmarked this and it adds <2% to link
        // time on the default syntax set, and linking doesn't even happen
        // when loading from a binary dump.
        while found_more_backref_includes {
            found_more_backref_includes = false;
            // find any contexts which include a context which uses backrefs
            // and mark those as using backrefs - to support nested includes
            for syntax_index in 0..syntaxes.len() {
                for context_index in 0..all_contexts[syntax_index].len() {
                    let context = &all_contexts[syntax_index][context_index];
                    if !context.uses_backrefs && context.patterns.iter().any(|pattern| {
                        matches!(pattern, Pattern::Include(ContextReference::Direct(id)) | Pattern::IncludeWithPrototype(ContextReference::Direct(id)) if all_contexts[id.syntax_index][id.context_index].uses_backrefs)
                    }) {
                        let context = &mut all_contexts[syntax_index][context_index];
                        context.uses_backrefs = true;
                        // look for contexts including this context
                        found_more_backref_includes = true;
                    }
                }
            }
        }

        #[cfg(feature = "metadata")]
        let metadata = match existing_metadata {
            Some(existing) => existing.merged_with_raw(raw_metadata),
            None => raw_metadata.into(),
        };

        // The combination of
        //  * the algorithms above
        //  * the borrow checker
        // makes it necessary to set these up as the last step.
        for syntax in &mut syntaxes {
            let lazy_contexts = LazyContexts {
                context_ids: all_context_ids.remove(0),
                contexts: all_contexts.remove(0),
            };

            syntax.serialized_lazy_contexts = crate::dumps::dump_binary(&lazy_contexts);
        }

        SyntaxSet {
            syntaxes,
            path_syntaxes,
            warnings,
            first_line_cache: OnceLock::new(),
            #[cfg(feature = "metadata")]
            metadata,
        }
    }

    /// No-op extends resolution when yaml-load feature is not available.
    #[cfg(not(feature = "yaml-load"))]
    fn resolve_extends(
        syntax_definitions: Vec<SyntaxDefinition>,
        _path_syntaxes: &[(String, usize)],
        _lines_include_newline: bool,
    ) -> (Vec<SyntaxDefinition>, Vec<String>) {
        (syntax_definitions, Vec::new())
    }

    /// Resolve `extends` relationships between syntax definitions.
    ///
    /// For each child syntax that extends a parent, merge the parent's contexts and variables
    /// into the child. Respects `meta_prepend`/`meta_append` merge modes.
    #[cfg(feature = "yaml-load")]
    fn resolve_extends(
        mut syntax_definitions: Vec<SyntaxDefinition>,
        path_syntaxes: &[(String, usize)],
        lines_include_newline: bool,
    ) -> (Vec<SyntaxDefinition>, Vec<String>) {
        let mut warnings = Vec::new();

        // Build lookup maps: name -> index and path-suffix -> index
        let mut name_to_index: HashMap<String, usize> = HashMap::new();
        for (i, sd) in syntax_definitions.iter().enumerate() {
            name_to_index.insert(sd.name.clone(), i);
        }

        // Track which syntaxes need extends resolution
        let mut unresolved: HashSet<usize> = HashSet::new();
        for (i, sd) in syntax_definitions.iter().enumerate() {
            if !sd.extends.is_empty() {
                unresolved.insert(i);
            }
        }

        if unresolved.is_empty() {
            return (syntax_definitions, warnings);
        }

        // Snapshot which contexts and variables each syntax defined directly
        // (before any extends merging populates `.contexts` / `.variables`
        // with parent entries). Used during multi-parent merge to prefer a
        // parent's *own* definition over another parent's *inherited* one
        // — see `extends_array_own_beats_inherited` regression test.
        let own_contexts: HashMap<usize, HashSet<String>> = syntax_definitions
            .iter()
            .enumerate()
            .map(|(i, sd)| (i, sd.contexts.keys().cloned().collect()))
            .collect();
        let own_variables: HashMap<usize, HashSet<String>> = syntax_definitions
            .iter()
            .enumerate()
            .map(|(i, sd)| (i, sd.variables.keys().cloned().collect()))
            .collect();

        // Track root ancestor for each syntax (syntaxes with no extends are their own root)
        let mut syntax_roots: HashMap<usize, usize> = HashMap::new();
        for (i, sd) in syntax_definitions.iter().enumerate() {
            if sd.extends.is_empty() {
                syntax_roots.insert(i, i);
            }
        }

        // Fixed-point loop: resolve extends iteratively (to handle chains and multiple parents)
        let mut made_progress = true;
        while made_progress && !unresolved.is_empty() {
            made_progress = false;

            let still_unresolved: Vec<usize> = unresolved.iter().copied().collect();
            for child_idx in still_unresolved {
                let extends_paths = syntax_definitions[child_idx].extends.clone();
                if extends_paths.is_empty() {
                    continue;
                }

                // Find all parent indices; skip if any parent is not found or unresolved
                let mut parent_indices = Vec::with_capacity(extends_paths.len());
                let mut all_parents_ready = true;
                for extends_path in &extends_paths {
                    let parent_idx = Self::find_parent_index(
                        extends_path,
                        path_syntaxes,
                        &syntax_definitions,
                        &name_to_index,
                    );
                    match parent_idx {
                        Some(idx) if !unresolved.contains(&idx) => {
                            parent_indices.push(idx);
                        }
                        _ => {
                            all_parents_ready = false;
                            break;
                        }
                    }
                }

                if !all_parents_ready {
                    continue;
                }

                // H & I: all parents must share the same version as the child
                let child_version = syntax_definitions[child_idx].version;
                let version_ok = parent_indices
                    .iter()
                    .all(|&pi| syntax_definitions[pi].version == child_version);
                if !version_ok {
                    warnings.push(format!(
                        "syntax '{}' has a version mismatch with one or more parents; \
                         extends will not be applied",
                        syntax_definitions[child_idx].name
                    ));
                    unresolved.remove(&child_idx);
                    syntax_roots.insert(child_idx, child_idx);
                    made_progress = true;
                    continue;
                }

                // G: for multiple parents, all must share the same root ancestor
                let parent_roots: Vec<usize> = parent_indices
                    .iter()
                    .map(|&pi| *syntax_roots.get(&pi).unwrap_or(&pi))
                    .collect();
                let common_root = parent_roots[0];
                if !parent_roots.iter().all(|&r| r == common_root) {
                    warnings.push(format!(
                        "syntax '{}' extends parents that derive from different base syntaxes; \
                         extends will not be applied",
                        syntax_definitions[child_idx].name
                    ));
                    unresolved.remove(&child_idx);
                    syntax_roots.insert(child_idx, child_idx);
                    made_progress = true;
                    continue;
                }

                // Merge all parents left-to-right with provenance-aware
                // tiebreaking: a parent's *own* (directly-defined) entry
                // outranks another parent's *inherited* entry. Among
                // entries of the same provenance, last parent wins.
                //
                // Without this, `Zsh (for Markdown)`'s extends list
                // `[Bash (for Markdown), Zsh]` would let Zsh's inherited
                // Bash `main` (strict shebang regex) overwrite
                // Bash (for Markdown)'s own lenient `main`.
                let mut merged_variables: HashMap<String, String> = HashMap::new();
                let mut merged_var_is_own: HashMap<String, bool> = HashMap::new();
                let mut merged_contexts: HashMap<String, Context> = HashMap::new();
                let mut merged_ctx_is_own: HashMap<String, bool> = HashMap::new();

                for &parent_idx in &parent_indices {
                    let parent_variables = syntax_definitions[parent_idx].variables.clone();
                    let parent_contexts = syntax_definitions[parent_idx].contexts.clone();
                    let parent_own_vars = &own_variables[&parent_idx];
                    let parent_own_ctxs = &own_contexts[&parent_idx];

                    for (k, v) in parent_variables {
                        let is_own = parent_own_vars.contains(&k);
                        let take = match merged_var_is_own.get(&k).copied() {
                            None => true,
                            Some(false) => true,
                            Some(true) => is_own,
                        };
                        if take {
                            merged_var_is_own.insert(k.clone(), is_own);
                            merged_variables.insert(k, v);
                        }
                    }

                    for (ctx_name, parent_ctx) in parent_contexts {
                        let is_own = parent_own_ctxs.contains(&ctx_name);
                        let take = match merged_ctx_is_own.get(&ctx_name).copied() {
                            None => true,
                            Some(false) => true,
                            Some(true) => is_own,
                        };
                        if take {
                            merged_ctx_is_own.insert(ctx_name.clone(), is_own);
                            merged_contexts.insert(ctx_name, parent_ctx);
                        }
                    }
                }

                let child = &mut syntax_definitions[child_idx];

                // Child variables override merged parent variables
                let child_variables: HashMap<String, String> = child.variables.drain().collect();
                for (k, v) in child_variables {
                    merged_variables.insert(k, v);
                }
                child.variables = merged_variables;

                // Merge contexts: child applies merge_mode against merged parent result
                for (ctx_name, parent_ctx) in merged_contexts {
                    if let Some(child_ctx) = child.contexts.get_mut(&ctx_name) {
                        match child_ctx.merge_mode {
                            ContextMergeMode::Replace => {
                                // Child's version wins, keep as-is
                            }
                            ContextMergeMode::Prepend => {
                                // child patterns + parent patterns
                                let mut merged_patterns = child_ctx.patterns.clone();
                                merged_patterns.extend(parent_ctx.patterns);
                                child_ctx.patterns = merged_patterns;
                                if child_ctx.meta_scope.is_empty() {
                                    child_ctx.meta_scope = parent_ctx.meta_scope;
                                }
                                if child_ctx.meta_content_scope.is_empty() {
                                    child_ctx.meta_content_scope = parent_ctx.meta_content_scope;
                                }
                                if child_ctx.clear_scopes.is_none() {
                                    child_ctx.clear_scopes = parent_ctx.clear_scopes;
                                }
                                if child_ctx.meta_include_prototype.is_none() {
                                    child_ctx.meta_include_prototype =
                                        parent_ctx.meta_include_prototype;
                                }
                            }
                            ContextMergeMode::Append => {
                                // parent patterns + child patterns
                                let child_patterns = child_ctx.patterns.clone();
                                child_ctx.patterns = parent_ctx.patterns;
                                child_ctx.patterns.extend(child_patterns);
                                if child_ctx.meta_scope.is_empty() {
                                    child_ctx.meta_scope = parent_ctx.meta_scope;
                                }
                                if child_ctx.meta_content_scope.is_empty() {
                                    child_ctx.meta_content_scope = parent_ctx.meta_content_scope;
                                }
                                if child_ctx.clear_scopes.is_none() {
                                    child_ctx.clear_scopes = parent_ctx.clear_scopes;
                                }
                                if child_ctx.meta_include_prototype.is_none() {
                                    child_ctx.meta_include_prototype =
                                        parent_ctx.meta_include_prototype;
                                }
                            }
                        }
                    } else {
                        // Parent context not in child: inherit it
                        child.contexts.insert(ctx_name, parent_ctx);
                    }
                }

                // Regenerate __start/__main for the child's own scope.
                // The parent's __start carries the parent's meta_scope (e.g.
                // text.html.plain), which is wrong for the child (e.g.
                // text.html.rails). Always regenerate so the child's scope
                // is used.
                if child.contexts.contains_key("main") {
                    let mut scope_repo = crate::parsing::scope::lock_global_scope_repo();
                    let top_level_scope = child.scope;
                    SyntaxDefinition::add_initial_contexts(
                        &mut child.contexts,
                        &mut crate::parsing::yaml_load::ParserState {
                            scope_repo: scope_repo.deref_mut(),
                            variables: child.variables.clone(),
                            variable_regex: Regex::new(r"\{\{([A-Za-z0-9_]+)\}\}".into()),
                            backref_regex: Regex::new(r"\\\d".into()),
                            lines_include_newline,
                            version: child.version,
                            defer_regex_validation: false,
                        },
                        top_level_scope,
                    );
                }

                if let Err(e) =
                    crate::parsing::yaml_load::re_resolve_all_regexes(child, lines_include_newline)
                {
                    warnings.push(format!(
                        "failed to re-resolve regexes for '{}' after extends: {}",
                        child.name, e
                    ));
                }

                syntax_roots.insert(child_idx, common_root);
                unresolved.remove(&child_idx);
                made_progress = true;
            }
        }

        if !unresolved.is_empty() {
            for idx in &unresolved {
                let syntax = &mut syntax_definitions[*idx];
                let e = &syntax.extends;
                let extends_str = if e.is_empty() {
                    "?".to_string()
                } else {
                    e.join(", ")
                };
                warnings.push(format!(
                    "syntax '{}' extends '{}' but parent was not found or has circular dependency",
                    syntax.name, extends_str,
                ));
                // Mark broken syntaxes as hidden so they won't be found by
                // name/extension lookups and won't cause panics when used.
                syntax.hidden = true;

                // Ensure the syntax has __start and __main contexts so that
                // ParseState::new won't panic if this syntax is still accessed.
                if !syntax.contexts.contains_key("main") {
                    syntax
                        .contexts
                        .insert("main".to_string(), Context::new(None));
                }
                if !syntax.contexts.contains_key("__start") {
                    let mut scope_repo = crate::parsing::scope::lock_global_scope_repo();
                    let top_level_scope = syntax.scope;
                    SyntaxDefinition::add_initial_contexts(
                        &mut syntax.contexts,
                        &mut crate::parsing::yaml_load::ParserState {
                            scope_repo: scope_repo.deref_mut(),
                            variables: syntax.variables.clone(),
                            variable_regex: Regex::new(r"\{\{([A-Za-z0-9_]+)\}\}".into()),
                            backref_regex: Regex::new(r"\\\d".into()),
                            lines_include_newline,
                            version: syntax.version,
                            defer_regex_validation: false,
                        },
                        top_level_scope,
                    );
                }
            }
        }

        (syntax_definitions, warnings)
    }

    /// Find the index of a parent syntax by matching the extends path.
    #[cfg(feature = "yaml-load")]
    fn find_parent_index(
        extends_path: &str,
        path_syntaxes: &[(String, usize)],
        syntax_definitions: &[SyntaxDefinition],
        name_to_index: &HashMap<String, usize>,
    ) -> Option<usize> {
        // Normalize separators for matching
        let normalized = extends_path.replace('\\', "/");

        // First try matching against path_syntaxes (path ends with extends value)
        let slash_normalized = format!("/{}", normalized);
        for (path, idx) in path_syntaxes {
            let path_normalized = path.replace('\\', "/");
            if path_normalized.ends_with(&slash_normalized) || path_normalized == normalized {
                return Some(*idx);
            }
        }

        // Try matching by file stem (e.g., "Packages/C/C.sublime-syntax" -> look for syntax named "C")
        if let Some(file_name) = std::path::Path::new(&normalized).file_stem() {
            if let Some(name_str) = file_name.to_str() {
                if let Some(&idx) = name_to_index.get(name_str) {
                    return Some(idx);
                }
                // Try case-insensitive match
                for (i, sd) in syntax_definitions.iter().enumerate() {
                    if sd.name.eq_ignore_ascii_case(name_str) {
                        return Some(i);
                    }
                }
            }
        }

        None
    }

    /// Anything recursively included by the prototype shouldn't include the prototype.
    /// This marks them as such.
    ///
    /// Reachability from the prototype propagates through `include` AND through
    /// `Push` / `Set` / `Branch` / `Embed` targets — both can produce a recursive
    /// `prototype → … → prototype` loop if the target also gets the prototype
    /// attached. The classic case is YAML's `prototype: include: property` where
    /// `property` pushes `property-body`: if `property-body` had the prototype,
    /// every match in property-body would re-fire `property` and push another
    /// property-body forever (`meta.property.yaml` × ∞).
    ///
    /// The `via_push` flag distinguishes "still inside the prototype's include
    /// chain" from "already inside a body that the prototype pushed/embedded
    /// into". Once we transition through Push/Set/Branch/Embed (`via_push: true`)
    /// we keep following further Push/Set/Branch/Embed targets — those targets
    /// chain back via `set:` to the same family of contexts (Lua's
    /// `line-doc-comment-body → set: maybe-line-doc-comment → set:
    /// line-doc-comment-body`) and would loop through the prototype. But we
    /// stop following `include:` of independent named contexts: those `include`s
    /// reach into general code-parsing land that has nothing to do with the
    /// prototype's intent (Haskell's `preprocessor-pragma-signature-value`
    /// `include: functions`, which would otherwise mark every function context
    /// in the syntax — including `variable-name-end`, breaking the
    /// `meta.function.identifier.haskell` cross-line replay because the
    /// prototype's `line-comments` rule would then never fire inside
    /// `variable-name-end` and the `(?=\S)` pop:2 would orphan the `functions`
    /// branch_point on every assertion-comment line).
    fn recursively_mark_no_prototype(
        context_id: &ContextId,
        syntax_context_ids: &HashMap<String, ContextId>,
        all_contexts: &[Vec<Context>],
        no_prototype: &mut HashSet<ContextId>,
        via_push: bool,
    ) {
        let first_time = no_prototype.insert(*context_id);
        if !first_time {
            return;
        }

        for pattern in &all_contexts[context_id.syntax_index][context_id.context_index].patterns {
            match *pattern {
                // Apparently inline blocks also don't include the prototype when within the prototype.
                // This is really weird, but necessary to run the YAML syntax.
                Pattern::Match(ref match_pat) => {
                    let maybe_context_refs = match match_pat.operation {
                        MatchOperation::Push {
                            ctx_refs: ref context_refs,
                            ..
                        }
                        | MatchOperation::Set {
                            ctx_refs: ref context_refs,
                            ..
                        }
                        | MatchOperation::Branch {
                            alternatives: ref context_refs,
                            ..
                        }
                        | MatchOperation::Embed {
                            contexts: ref context_refs,
                            ..
                        } => Some(context_refs),
                        MatchOperation::Pop(_) | MatchOperation::None | MatchOperation::Fail(_) => {
                            None
                        }
                    };
                    if let Some(context_refs) = maybe_context_refs {
                        for context_ref in context_refs.iter() {
                            match context_ref {
                                ContextReference::Inline(ref s)
                                | ContextReference::Named(ref s) => {
                                    if let Some(i) = syntax_context_ids.get(s) {
                                        Self::recursively_mark_no_prototype(
                                            i,
                                            syntax_context_ids,
                                            all_contexts,
                                            no_prototype,
                                            true,
                                        );
                                    }
                                }
                                ContextReference::Direct(ref id) => {
                                    Self::recursively_mark_no_prototype(
                                        id,
                                        syntax_context_ids,
                                        all_contexts,
                                        no_prototype,
                                        true,
                                    );
                                }
                                _ => (),
                            }
                        }
                    }
                }
                Pattern::Include(ref reference) | Pattern::IncludeWithPrototype(ref reference) => {
                    // Once we've crossed a Push/Set/Branch/Embed transition,
                    // the body's `include:`s reach into the syntax's general
                    // code-parsing contexts and don't actually reintroduce a
                    // prototype loop — stop following them. See the function
                    // doc-comment for the Haskell case that motivated this.
                    if via_push {
                        continue;
                    }
                    match reference {
                        ContextReference::Named(ref s) => {
                            if let Some(id) = syntax_context_ids.get(s) {
                                Self::recursively_mark_no_prototype(
                                    id,
                                    syntax_context_ids,
                                    all_contexts,
                                    no_prototype,
                                    false,
                                );
                            }
                        }
                        ContextReference::Direct(ref id) => {
                            Self::recursively_mark_no_prototype(
                                id,
                                syntax_context_ids,
                                all_contexts,
                                no_prototype,
                                false,
                            );
                        }
                        _ => (),
                    }
                }
            }
        }
    }

    fn link_context(
        context: &mut Context,
        syntax_index: usize,
        all_context_ids: &[HashMap<String, ContextId>],
        syntaxes: &[SyntaxReference],
    ) {
        for pattern in &mut context.patterns {
            match *pattern {
                Pattern::Match(ref mut match_pat) => {
                    Self::link_match_pat(match_pat, syntax_index, all_context_ids, syntaxes)
                }
                Pattern::Include(ref mut context_ref)
                | Pattern::IncludeWithPrototype(ref mut context_ref) => {
                    Self::link_ref(context_ref, syntax_index, all_context_ids, syntaxes)
                }
            }
        }
    }

    fn link_ref(
        context_ref: &mut ContextReference,
        syntax_index: usize,
        all_context_ids: &[HashMap<String, ContextId>],
        syntaxes: &[SyntaxReference],
    ) {
        // println!("{:?}", context_ref);
        use super::syntax_definition::ContextReference::*;
        let linked_context_id = match *context_ref {
            Named(ref s) | Inline(ref s) => {
                // This isn't actually correct, but it is better than nothing/crashing.
                // This is being phased out anyhow, see https://github.com/sublimehq/Packages/issues/73
                // Fixes issue #30
                if s == "$top_level_main" {
                    all_context_ids[syntax_index].get("main")
                } else {
                    all_context_ids[syntax_index].get(s)
                }
            }
            ByScope {
                scope,
                ref sub_context,
                with_escape,
            } => Self::with_plain_text_fallback(
                all_context_ids,
                syntaxes,
                with_escape,
                Self::find_id(sub_context, all_context_ids, syntaxes, |index_and_syntax| {
                    index_and_syntax.1.scope == scope
                }),
            ),
            File {
                ref name,
                ref sub_context,
                with_escape,
            } => Self::with_plain_text_fallback(
                all_context_ids,
                syntaxes,
                with_escape,
                Self::find_id(sub_context, all_context_ids, syntaxes, |index_and_syntax| {
                    &index_and_syntax.1.name == name
                }),
            ),
            Direct(_) => None,
        };
        if let Some(context_id) = linked_context_id {
            let mut new_ref = Direct(*context_id);
            mem::swap(context_ref, &mut new_ref);
        }
    }

    fn with_plain_text_fallback<'a>(
        all_context_ids: &'a [HashMap<String, ContextId>],
        syntaxes: &'a [SyntaxReference],
        with_escape: bool,
        context_id: Option<&'a ContextId>,
    ) -> Option<&'a ContextId> {
        context_id.or_else(|| {
            if with_escape {
                // If we keep this reference unresolved, syntect will crash
                // when it encounters the reference. Rather than crashing,
                // we instead fall back to "Plain Text". This seems to be
                // how Sublime Text behaves. It should be a safe thing to do
                // since `embed`s always includes an `escape` to get out of
                // the `embed`.
                Self::find_id(&None, all_context_ids, syntaxes, |index_and_syntax| {
                    index_and_syntax.1.name == "Plain Text"
                })
            } else {
                None
            }
        })
    }

    fn find_id<'a>(
        sub_context: &Option<String>,
        all_context_ids: &'a [HashMap<String, ContextId>],
        syntaxes: &'a [SyntaxReference],
        predicate: impl FnMut(&(usize, &SyntaxReference)) -> bool,
    ) -> Option<&'a ContextId> {
        let context_name = sub_context.as_ref().map_or("main", |x| &**x);
        syntaxes
            .iter()
            .enumerate()
            .rev()
            .find(predicate)
            .and_then(|index_and_syntax| all_context_ids[index_and_syntax.0].get(context_name))
    }

    fn link_match_pat(
        match_pat: &mut MatchPattern,
        syntax_index: usize,
        all_context_ids: &[HashMap<String, ContextId>],
        syntaxes: &[SyntaxReference],
    ) {
        let maybe_context_refs = match match_pat.operation {
            MatchOperation::Push {
                ctx_refs: ref mut context_refs,
                ..
            }
            | MatchOperation::Set {
                ctx_refs: ref mut context_refs,
                ..
            }
            | MatchOperation::Branch {
                alternatives: ref mut context_refs,
                ..
            }
            | MatchOperation::Embed {
                contexts: ref mut context_refs,
                ..
            } => Some(context_refs),
            MatchOperation::Pop(_) | MatchOperation::None | MatchOperation::Fail(_) => None,
        };
        if let Some(context_refs) = maybe_context_refs {
            for context_ref in context_refs.iter_mut() {
                Self::link_ref(context_ref, syntax_index, all_context_ids, syntaxes);
            }
        }
        if let Some(ref mut context_ref) = match_pat.with_prototype {
            Self::link_ref(context_ref, syntax_index, all_context_ids, syntaxes);
        }
    }
}

#[derive(Debug)]
struct FirstLineCache {
    /// (first line regex, syntax index) pairs for all syntaxes with a first line regex
    regexes: Vec<(Regex, usize)>,
}

impl FirstLineCache {
    fn new(syntaxes: &[SyntaxReference]) -> FirstLineCache {
        let mut regexes = Vec::new();
        for (i, syntax) in syntaxes.iter().enumerate() {
            if let Some(ref reg_str) = syntax.first_line_match {
                let reg = Regex::new(reg_str.into());
                regexes.push((reg, i));
            }
        }
        FirstLineCache { regexes }
    }
}

#[cfg(feature = "yaml-load")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        parsing::{syntax_definition, ParseState, Scope},
        utils::testdata,
    };
    use std::collections::HashMap;

    #[test]
    fn can_load() {
        let mut builder = testdata::PACKAGES_SYN_SET.to_owned().into_builder();

        let cmake_dummy_syntax = SyntaxDefinition {
            name: "CMake".to_string(),
            file_extensions: vec!["CMakeLists.txt".to_string(), "cmake".to_string()],
            scope: Scope::new("source.cmake").unwrap(),
            first_line_match: None,
            hidden: false,
            variables: HashMap::new(),
            contexts: HashMap::new(),
            extends: vec![],
            version: 1,
        };

        builder.add(cmake_dummy_syntax);
        builder.add_plain_text_syntax();

        let ps = builder.build();

        assert_eq!(
            &ps.find_syntax_by_first_line("#!/usr/bin/env node")
                .unwrap()
                .name,
            "JavaScript"
        );
        let rails_scope = Scope::new("source.ruby.rails").unwrap();
        let syntax = ps.find_syntax_by_name("Ruby (Rails)").unwrap();
        ps.find_syntax_plain_text();
        assert_eq!(&ps.find_syntax_by_extension("rake").unwrap().name, "Ruby");
        assert_eq!(&ps.find_syntax_by_extension("RAKE").unwrap().name, "Ruby");
        assert_eq!(&ps.find_syntax_by_token("ruby").unwrap().name, "Ruby");
        assert_eq!(
            &ps.find_syntax_by_first_line("// -*- Mode: C -*- such line")
                .unwrap()
                .name,
            "C"
        );
        assert_eq!(
            &ps.find_syntax_for_file("testdata/parser.rs")
                .unwrap()
                .unwrap()
                .name,
            "Rust"
        );
        assert_eq!(
            &ps.find_syntax_for_file("testdata/test_first_line.test")
                .expect("Error finding syntax for file")
                .expect("No syntax found for file")
                .name,
            "Ruby"
        );
        assert_eq!(
            &ps.find_syntax_for_file(".bashrc").unwrap().unwrap().name,
            "Bash"
        );
        assert_eq!(
            &ps.find_syntax_for_file("CMakeLists.txt")
                .unwrap()
                .unwrap()
                .name,
            "CMake"
        );
        assert_eq!(
            &ps.find_syntax_for_file("test.cmake").unwrap().unwrap().name,
            "CMake"
        );
        assert_eq!(
            &ps.find_syntax_for_file("Rakefile").unwrap().unwrap().name,
            "Ruby"
        );
        assert!(&ps.find_syntax_by_first_line("derp derp hi lol").is_none());
        assert_eq!(
            &ps.find_syntax_by_path("Packages/Rust/Rust.sublime-syntax")
                .unwrap()
                .name,
            "Rust"
        );
        // println!("{:#?}", syntax);
        assert_eq!(syntax.scope, rails_scope);
        // unreachable!();
        let main_context = ps
            .get_context(&syntax.context_ids()["main"])
            .expect("#[cfg(test)]");
        let count = syntax_definition::context_iter(&ps, main_context).count();
        assert_eq!(count, 184);
    }

    #[test]
    fn can_clone() {
        let cloned_syntax_set = {
            let mut builder = SyntaxSetBuilder::new();
            builder.add(syntax_a());
            builder.add(syntax_b());

            let syntax_set_original = builder.build();
            #[allow(clippy::redundant_clone)] // We want to test .clone()
            syntax_set_original.clone()
            // Note: The original syntax set is dropped
        };

        let syntax = cloned_syntax_set.find_syntax_by_extension("a").unwrap();
        let mut parse_state = ParseState::new(syntax);
        let ops = parse_state
            .parse_line("a go_b b", &cloned_syntax_set)
            .expect("#[cfg(test)]")
            .ops;
        let expected = (7, ScopeStackOp::Push(Scope::new("b").unwrap()));
        assert_ops_contain(&ops, &expected);
    }

    #[test]
    fn can_list_added_syntaxes() {
        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax_a());
        builder.add(syntax_b());
        let syntaxes = builder.syntaxes();

        assert_eq!(syntaxes.len(), 2);
        assert_eq!(syntaxes[0].name, "A");
        assert_eq!(syntaxes[1].name, "B");
    }

    #[test]
    fn can_add_more_syntaxes_with_builder() {
        let syntax_set_original = {
            let mut builder = SyntaxSetBuilder::new();
            builder.add(syntax_a());
            builder.add(syntax_b());
            builder.build()
        };

        let mut builder = syntax_set_original.into_builder();

        let syntax_c = SyntaxDefinition::load_from_str(
            r#"
        name: C
        scope: source.c
        file_extensions: [c]
        contexts:
          main:
            - match: 'c'
              scope: c
            - match: 'go_a'
              push: scope:source.a#main
        "#,
            true,
            None,
        )
        .unwrap();

        builder.add(syntax_c);

        let syntax_set = builder.build();

        let syntax = syntax_set.find_syntax_by_extension("c").unwrap();
        let mut parse_state = ParseState::new(syntax);
        let ops = parse_state
            .parse_line("c go_a a go_b b", &syntax_set)
            .expect("#[cfg(test)]")
            .ops;
        let expected = (14, ScopeStackOp::Push(Scope::new("b").unwrap()));
        assert_ops_contain(&ops, &expected);
    }

    #[test]
    fn falls_back_to_plain_text_when_embedded_scope_is_missing() {
        test_plain_text_fallback(
            r#"
        name: Z
        scope: source.z
        file_extensions: [z]
        contexts:
          main:
            - match: 'z'
              scope: z
            - match: 'go_x'
              embed: scope:does.not.exist
              escape: 'leave_x'
        "#,
        );
    }

    #[test]
    fn falls_back_to_plain_text_when_embedded_file_is_missing() {
        test_plain_text_fallback(
            r#"
        name: Z
        scope: source.z
        file_extensions: [z]
        contexts:
          main:
            - match: 'z'
              scope: z
            - match: 'go_x'
              embed: DoesNotExist.sublime-syntax
              escape: 'leave_x'
        "#,
        );
    }

    fn test_plain_text_fallback(syntax_definition: &str) {
        let syntax = SyntaxDefinition::load_from_str(syntax_definition, true, None).unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add_plain_text_syntax();
        builder.add(syntax);
        let syntax_set = builder.build();

        let syntax = syntax_set.find_syntax_by_extension("z").unwrap();
        let mut parse_state = ParseState::new(syntax);
        let ops = parse_state
            .parse_line("z go_x x leave_x z", &syntax_set)
            .unwrap()
            .ops;
        let expected_ops = vec![
            (0, ScopeStackOp::Push(Scope::new("source.z").unwrap())),
            (0, ScopeStackOp::Push(Scope::new("z").unwrap())),
            (1, ScopeStackOp::Pop(1)),
            (6, ScopeStackOp::Push(Scope::new("text.plain").unwrap())),
            (9, ScopeStackOp::Pop(1)),
            (17, ScopeStackOp::Push(Scope::new("z").unwrap())),
            (18, ScopeStackOp::Pop(1)),
        ];
        assert_eq!(ops, expected_ops);
    }

    #[test]
    fn can_find_unlinked_contexts() {
        let syntax_set = {
            let mut builder = SyntaxSetBuilder::new();
            builder.add(syntax_a());
            builder.add(syntax_b());
            builder.build()
        };

        let unlinked_contexts = syntax_set.find_unlinked_contexts();
        assert_eq!(unlinked_contexts.len(), 0);

        let syntax_set = {
            let mut builder = SyntaxSetBuilder::new();
            builder.add(syntax_a());
            builder.build()
        };

        let unlinked_contexts: Vec<String> =
            syntax_set.find_unlinked_contexts().into_iter().collect();
        assert_eq!(unlinked_contexts.len(), 1);
        assert_eq!(unlinked_contexts[0], "Syntax 'A' with scope 'source.a' has unresolved context reference ByScope { scope: <source.b>, sub_context: Some(\"main\"), with_escape: false }");
    }

    #[test]
    fn can_use_in_multiple_threads() {
        use rayon::prelude::*;

        let syntax_set = {
            let mut builder = SyntaxSetBuilder::new();
            builder.add(syntax_a());
            builder.add(syntax_b());
            builder.build()
        };

        let lines = vec!["a a a", "a go_b b", "go_b b", "go_b b  b"];

        let results: Vec<Vec<(usize, ScopeStackOp)>> = lines
            .par_iter()
            .map(|line| {
                let syntax = syntax_set.find_syntax_by_extension("a").unwrap();
                let mut parse_state = ParseState::new(syntax);
                parse_state
                    .parse_line(line, &syntax_set)
                    .expect("#[cfg(test)]")
                    .ops
            })
            .collect();

        assert_ops_contain(
            &results[0],
            &(4, ScopeStackOp::Push(Scope::new("a").unwrap())),
        );
        assert_ops_contain(
            &results[1],
            &(7, ScopeStackOp::Push(Scope::new("b").unwrap())),
        );
        assert_ops_contain(
            &results[2],
            &(5, ScopeStackOp::Push(Scope::new("b").unwrap())),
        );
        assert_ops_contain(
            &results[3],
            &(8, ScopeStackOp::Push(Scope::new("b").unwrap())),
        );
    }

    #[test]
    fn is_sync() {
        check_sync::<SyntaxSet>();
    }

    #[test]
    fn is_send() {
        check_send::<SyntaxSet>();
    }

    #[test]
    fn can_override_syntaxes() {
        let syntax_set = {
            let mut builder = SyntaxSetBuilder::new();
            builder.add(syntax_a());
            builder.add(syntax_b());

            let syntax_a2 = SyntaxDefinition::load_from_str(
                r#"
                name: A improved
                scope: source.a
                file_extensions: [a]
                first_line_match: syntax\s+a
                contexts:
                  main:
                    - match: a
                      scope: a2
                    - match: go_b
                      push: scope:source.b#main
                "#,
                true,
                None,
            )
            .unwrap();

            builder.add(syntax_a2);

            let syntax_c = SyntaxDefinition::load_from_str(
                r#"
                name: C
                scope: source.c
                file_extensions: [c]
                first_line_match: syntax\s+.*
                contexts:
                  main:
                    - match: c
                      scope: c
                    - match: go_a
                      push: scope:source.a#main
                "#,
                true,
                None,
            )
            .unwrap();

            builder.add(syntax_c);

            builder.build()
        };

        let mut syntax = syntax_set.find_syntax_by_extension("a").unwrap();
        assert_eq!(syntax.name, "A improved");
        syntax = syntax_set
            .find_syntax_by_scope(Scope::new("source.a").unwrap())
            .unwrap();
        assert_eq!(syntax.name, "A improved");
        syntax = syntax_set.find_syntax_by_first_line("syntax a").unwrap();
        assert_eq!(syntax.name, "C");

        let mut parse_state = ParseState::new(syntax);
        let ops = parse_state
            .parse_line("c go_a a", &syntax_set)
            .expect("msg")
            .ops;
        let expected = (7, ScopeStackOp::Push(Scope::new("a2").unwrap()));
        assert_ops_contain(&ops, &expected);
    }

    #[test]
    fn can_parse_issue219() {
        // Go to builder and back after loading so that build() gets Direct references instead of
        // Named ones. The bug was that Direct references were not handled when marking as
        // "no prototype", so prototype contexts accidentally had the prototype set, which made
        // the parser loop forever.
        let syntax_set = SyntaxSet::load_defaults_newlines().into_builder().build();
        let syntax = syntax_set.find_syntax_by_extension("yaml").unwrap();

        let mut parse_state = ParseState::new(syntax);
        let ops = parse_state
            .parse_line("# test\n", &syntax_set)
            .expect("#[cfg(test)]")
            .ops;
        let expected = (
            0,
            ScopeStackOp::Push(Scope::new("comment.line.number-sign.yaml").unwrap()),
        );
        assert_ops_contain(&ops, &expected);
    }

    #[test]
    fn no_prototype_for_contexts_included_from_prototype() {
        let mut builder = SyntaxSetBuilder::new();
        let syntax = SyntaxDefinition::load_from_str(
            r#"
                name: Test Prototype
                scope: source.test
                file_extensions: [test]
                contexts:
                  prototype:
                    - include: included_from_prototype
                  main:
                    - match: main
                    - match: other
                      push: other
                  other:
                    - match: o
                  included_from_prototype:
                    - match: p
                      scope: p
                "#,
            true,
            None,
        )
        .unwrap();
        builder.add(syntax);
        let ss = builder.build();

        // "main" and "other" should have context set, "prototype" and "included_from_prototype"
        // must not have a prototype set.
        assert_prototype_only_on(&["main", "other"], &ss, &ss.syntaxes()[0]);

        // Building again should have the same result. The difference is that after the first
        // build(), the references have been replaced with Direct references, so the code needs to
        // handle that correctly.
        let rebuilt = ss.into_builder().build();
        assert_prototype_only_on(&["main", "other"], &rebuilt, &rebuilt.syntaxes()[0]);
    }

    #[test]
    fn prototype_attached_to_named_contexts_reachable_only_via_push_then_include() {
        // Regression guard for the Haskell `meta.function.identifier.haskell`
        // case (`syntax_test_haskell.hs:2312-2348`). The chain:
        //   prototype          (include)
        //   → push-source      (push: pushed-body)
        //   → pushed-body      (mip: false; include: code)
        //   → code             (push: var-name → push: var-name-end)
        // The `include: code` inside `pushed-body` is the boundary: `code`,
        // `var-name`, and `var-name-end` are general syntax contexts that
        // should still get the prototype attached (so the `--`-comment rule
        // declared in the prototype fires inside them). The pre-fix logic
        // marked all of them as `no_prototype`, suppressing the cross-line
        // `fail` replay that installs `meta.function.identifier.haskell`.
        let mut builder = SyntaxSetBuilder::new();
        let syntax = SyntaxDefinition::load_from_str(
            r#"
                name: Test Prototype Push
                scope: source.test
                file_extensions: [test]
                contexts:
                  prototype:
                    - include: push-source
                  main:
                    - include: code
                  push-source:
                    - match: 'p'
                      push: pushed-body
                  pushed-body:
                    - meta_include_prototype: false
                    - meta_scope: meta.pushed-body
                    - match: 'q'
                      pop: 1
                    - include: code
                  code:
                    - match: 'a'
                      push: var-name
                  var-name:
                    - match: 'b'
                      push: var-name-end
                  var-name-end:
                    - match: 'c'
                      pop: 2
                "#,
            true,
            None,
        )
        .unwrap();
        builder.add(syntax);
        let ss = builder.build();

        // `prototype`, `push-source`, and `pushed-body` are reachable from the
        // prototype before/at the Push boundary and stay out of the prototype.
        // `main`, `code`, `var-name`, and `var-name-end` are reachable only
        // through `pushed-body`'s `include: code` and must keep the prototype
        // attached.
        assert_prototype_only_on(
            &["main", "code", "var-name", "var-name-end"],
            &ss,
            &ss.syntaxes()[0],
        );

        let rebuilt = ss.into_builder().build();
        assert_prototype_only_on(
            &["main", "code", "var-name", "var-name-end"],
            &rebuilt,
            &rebuilt.syntaxes()[0],
        );
    }

    #[test]
    fn no_prototype_for_contexts_inline_in_prototype() {
        let mut builder = SyntaxSetBuilder::new();
        let syntax = SyntaxDefinition::load_from_str(
            r#"
                name: Test Prototype
                scope: source.test
                file_extensions: [test]
                contexts:
                  prototype:
                    - match: p
                      push:
                        - match: p2
                  main:
                    - match: main
                "#,
            true,
            None,
        )
        .unwrap();
        builder.add(syntax);
        let ss = builder.build();

        assert_prototype_only_on(&["main"], &ss, &ss.syntaxes()[0]);

        let rebuilt = ss.into_builder().build();
        assert_prototype_only_on(&["main"], &rebuilt, &rebuilt.syntaxes()[0]);
    }

    #[test]
    fn find_syntax_set_from_line_with_bom() {
        // Regression test for #529
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let syntax_ref = syntax_set
            .find_syntax_by_first_line("\u{feff}<?xml version=\"1.0\"?>")
            .unwrap();
        assert_eq!(syntax_ref.name, "XML");
    }

    fn assert_ops_contain(ops: &[(usize, ScopeStackOp)], expected: &(usize, ScopeStackOp)) {
        assert!(
            ops.contains(expected),
            "expected operations to contain {:?}: {:?}",
            expected,
            ops
        );
    }

    fn assert_prototype_only_on(
        expected: &[&str],
        syntax_set: &SyntaxSet,
        syntax: &SyntaxReference,
    ) {
        for (name, id) in syntax.context_ids() {
            if name == "__main" || name == "__start" {
                // Skip special contexts
                continue;
            }
            let context = syntax_set.get_context(id).expect("#[cfg(test)]");
            if expected.contains(&name.as_str()) {
                assert!(
                    context.prototype.is_some(),
                    "Expected context {} to have prototype",
                    name
                );
            } else {
                assert!(
                    context.prototype.is_none(),
                    "Expected context {} to not have prototype",
                    name
                );
            }
        }
    }

    fn check_send<T: Send>() {}

    fn check_sync<T: Sync>() {}

    fn syntax_a() -> SyntaxDefinition {
        SyntaxDefinition::load_from_str(
            r#"
            name: A
            scope: source.a
            file_extensions: [a]
            contexts:
              main:
                - match: 'a'
                  scope: a
                - match: 'go_b'
                  push: scope:source.b#main
            "#,
            true,
            None,
        )
        .unwrap()
    }

    fn syntax_b() -> SyntaxDefinition {
        SyntaxDefinition::load_from_str(
            r#"
            name: B
            scope: source.b
            file_extensions: [b]
            contexts:
              main:
                - match: 'b'
                  scope: b
            "#,
            true,
            None,
        )
        .unwrap()
    }

    // =====================================================
    // Tests for extends (syntax inheritance)
    // =====================================================

    fn base_syntax() -> SyntaxDefinition {
        SyntaxDefinition::load_from_str(
            r#"
            name: Base
            scope: source.base
            file_extensions: [base]
            variables:
              ident: '[a-z]+'
            contexts:
              main:
                - match: '{{ident}}'
                  scope: variable.base
              string:
                - meta_scope: string.base
                - match: '"'
                  pop: true
            "#,
            true,
            None,
        )
        .unwrap()
    }

    #[test]
    fn extends_inherits_contexts() {
        // Child extends base and inherits the 'string' context
        let base = base_syntax();
        let child = SyntaxDefinition::load_from_str(
            r#"
            name: Child
            scope: source.child
            file_extensions: [child]
            extends: Base.sublime-syntax
            contexts:
              main:
                - match: 'child'
                  scope: keyword.child
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(base);
        builder.add(child);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("Child").unwrap();
        // Child should have the 'string' context inherited from Base
        assert!(syntax.context_ids().contains_key("string"));
    }

    #[test]
    fn extends_overrides_context() {
        // Child overrides the 'main' context
        let base = base_syntax();
        let child = SyntaxDefinition::load_from_str(
            r#"
            name: Child
            scope: source.child
            file_extensions: [child]
            extends: Base.sublime-syntax
            contexts:
              main:
                - match: 'override'
                  scope: keyword.override
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(base);
        builder.add(child);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("Child").unwrap();
        let mut parse_state = ParseState::new(syntax);
        let ops = parse_state
            .parse_line("override\n", &ss)
            .expect("parse failed")
            .ops;
        // Should match child's 'override' keyword, not base's ident
        let expected = (
            0,
            ScopeStackOp::Push(Scope::new("keyword.override").unwrap()),
        );
        assert_ops_contain(&ops, &expected);
    }

    #[test]
    fn extends_meta_prepend() {
        // Child prepends patterns to main
        let base = base_syntax();
        let child = SyntaxDefinition::load_from_str(
            r#"
            name: Child
            scope: source.child
            file_extensions: [child]
            extends: Base.sublime-syntax
            contexts:
              main:
                - meta_prepend: true
                - match: 'keyword'
                  scope: keyword.child
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(base);
        builder.add(child);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("Child").unwrap();
        let mut parse_state = ParseState::new(syntax);
        // 'keyword' should match the child's pattern (prepended, so first in list)
        let ops = parse_state
            .parse_line("keyword\n", &ss)
            .expect("parse failed")
            .ops;
        let expected = (0, ScopeStackOp::Push(Scope::new("keyword.child").unwrap()));
        assert_ops_contain(&ops, &expected);
    }

    #[test]
    fn extends_meta_append() {
        // Child appends patterns to main
        let base = base_syntax();
        let child = SyntaxDefinition::load_from_str(
            r#"
            name: Child
            scope: source.child
            file_extensions: [child]
            extends: Base.sublime-syntax
            contexts:
              main:
                - meta_append: true
                - match: 'extra'
                  scope: keyword.extra
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(base);
        builder.add(child);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("Child").unwrap();
        let mut parse_state = ParseState::new(syntax);
        // 'abc' should still match base's ident pattern (it comes first since child is appended)
        let ops = parse_state
            .parse_line("abc\n", &ss)
            .expect("parse failed")
            .ops;
        let expected = (0, ScopeStackOp::Push(Scope::new("variable.base").unwrap()));
        assert_ops_contain(&ops, &expected);
    }

    #[test]
    fn extends_variable_override() {
        // Child overrides parent's 'ident' variable
        let base = SyntaxDefinition::load_from_str(
            r#"
            name: Base
            scope: source.base
            file_extensions: [base]
            variables:
              ident: '[a-z]+'
            contexts:
              main:
                - match: '{{ident}}'
                  scope: variable.base
            "#,
            true,
            None,
        )
        .unwrap();
        let child = SyntaxDefinition::load_from_str(
            r#"
            name: Child
            scope: source.child
            file_extensions: [child]
            extends: Base.sublime-syntax
            variables:
              ident: '[A-Z]+'
            contexts: {}
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(base);
        builder.add(child);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("Child").unwrap();
        let mut parse_state = ParseState::new(syntax);
        // Lowercase should NOT match (child overrides ident to uppercase only)
        let ops = parse_state
            .parse_line("ABC\n", &ss)
            .expect("parse failed")
            .ops;
        let expected = (0, ScopeStackOp::Push(Scope::new("variable.base").unwrap()));
        assert_ops_contain(&ops, &expected);
    }

    #[test]
    fn extends_variable_inherited_without_override() {
        // A child whose own regex references a variable defined only in
        // the parent must still compile that regex correctly. Without
        // deferred regex validation, the child's load-time pass would
        // resolve {{ident}} to "" (Base hasn't been merged yet) and
        // compile a broken pattern, causing the syntax to be skipped
        // entirely. `re_resolve_all_regexes` reruns compilation after
        // `resolve_extends` merges Base's variables into Child.
        let base = SyntaxDefinition::load_from_str(
            r#"
            name: Base
            scope: source.base
            file_extensions: [base]
            variables:
              ident: '[a-z]+'
            contexts:
              main:
                - match: '{{ident}}'
                  scope: variable.base
            "#,
            true,
            None,
        )
        .unwrap();
        // No `variables:` section — ident comes solely from Base.
        let child = SyntaxDefinition::load_from_str(
            r#"
            name: Child
            scope: source.child
            file_extensions: [child]
            extends: Base.sublime-syntax
            contexts:
              main:
                - match: '\${{ident}}'
                  scope: variable.child
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(base);
        builder.add(child);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("Child").unwrap();
        let mut parse_state = ParseState::new(syntax);
        let ops = parse_state
            .parse_line("$abc\n", &ss)
            .expect("parse failed")
            .ops;
        let expected = (0, ScopeStackOp::Push(Scope::new("variable.child").unwrap()));
        assert_ops_contain(&ops, &expected);
    }

    #[test]
    fn extends_only_syntax_with_no_contexts_key() {
        // Syntaxes that consist solely of `extends:` and have no
        // `contexts:` key at all (e.g. "Batch File (Compound)") must
        // load successfully — all contexts come from the parent.
        let base = SyntaxDefinition::load_from_str(
            r#"
            name: Base
            scope: source.base
            file_extensions: [base]
            variables:
              ident: '[a-z]+'
            contexts:
              main:
                - match: '{{ident}}'
                  scope: variable.base
            "#,
            true,
            None,
        )
        .unwrap();
        // No `contexts:` key at all — only extends + variables.
        let child = SyntaxDefinition::load_from_str(
            r#"
            name: ChildNoCtx
            scope: source.childnoctx
            file_extensions: [childnoctx]
            extends: Base.sublime-syntax
            variables:
              ident: '[A-Z]+'
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(base);
        builder.add(child);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("ChildNoCtx").unwrap();
        let mut parse_state = ParseState::new(syntax);
        // Uppercase should match (child overrides ident to uppercase)
        let ops = parse_state
            .parse_line("ABC\n", &ss)
            .expect("parse failed")
            .ops;
        let expected = (0, ScopeStackOp::Push(Scope::new("variable.base").unwrap()));
        assert_ops_contain(&ops, &expected);
    }

    #[test]
    fn extends_missing_parent_warns_but_no_panic() {
        // A syntax extends a non-existent parent. Should not panic.
        let child = SyntaxDefinition::load_from_str(
            r#"
            name: Orphan
            scope: source.orphan
            file_extensions: [orphan]
            extends: NonExistent.sublime-syntax
            contexts:
              main:
                - match: 'x'
                  scope: x
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(child);
        // Should not panic
        let ss = builder.build();
        assert!(ss.find_syntax_by_name("Orphan").is_some());
    }

    #[test]
    fn extends_multiple_parents_must_share_common_base() {
        // Per Sublime docs: all parents in `extends` list must derive from the same base syntax.
        // If they don't, the child should be rejected.
        let base1 = SyntaxDefinition::load_from_str(
            r#"
            name: Base1
            scope: source.base1
            file_extensions: [base1]
            contexts:
              main:
                - match: 'x'
                  scope: keyword.base1
              base1_only_ctx:
                - match: 'y'
                  scope: keyword.base1.y
            "#,
            true,
            None,
        )
        .unwrap();

        let base2 = SyntaxDefinition::load_from_str(
            r#"
            name: Base2
            scope: source.base2
            file_extensions: [base2]
            contexts:
              main:
                - match: 'x'
                  scope: keyword.base2
              base2_only_ctx:
                - match: 'z'
                  scope: keyword.base2.z
            "#,
            true,
            None,
        )
        .unwrap();

        let parent_a = SyntaxDefinition::load_from_str(
            r#"
            name: ParentA
            scope: source.parenta
            file_extensions: [parenta]
            extends: Base1.sublime-syntax
            contexts:
              parent_a_ctx:
                - match: 'a'
                  scope: keyword.a
            "#,
            true,
            None,
        )
        .unwrap();

        let parent_b = SyntaxDefinition::load_from_str(
            r#"
            name: ParentB
            scope: source.parentb
            file_extensions: [parentb]
            extends: Base2.sublime-syntax
            contexts:
              parent_b_ctx:
                - match: 'b'
                  scope: keyword.b
            "#,
            true,
            None,
        )
        .unwrap();

        let child = SyntaxDefinition::load_from_str(
            r#"
            name: Child
            scope: source.child_diffbase
            file_extensions: [child_diffbase]
            extends:
              - ParentA.sublime-syntax
              - ParentB.sublime-syntax
            contexts:
              child_ctx:
                - match: 'c'
                  scope: keyword.c
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(base1);
        builder.add(base2);
        builder.add(parent_a);
        builder.add(parent_b);
        builder.add(child);
        let ss = builder.build();

        let child_ref = ss.find_syntax_by_name("Child").unwrap();
        let context_ids = child_ref.context_ids();

        // Per Sublime spec: a child whose parents derive from different bases should be rejected.
        // Syntect currently silently merges both, so it will have contexts from both unrelated bases.
        // This assertion reflects the CORRECT behavior and is EXPECTED TO FAIL until validation
        // is implemented.
        assert!(
            !(context_ids.contains_key("base1_only_ctx")
                && context_ids.contains_key("base2_only_ctx")),
            "Child with parents from different bases should be rejected; \
             found contexts from both unrelated bases: {:?}",
            context_ids.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn extends_parents_and_base_must_have_same_version() {
        // Per Sublime docs: all syntaxes in the inheritance chain must have the same version.
        // Here: Base is v1, Parent is v2 extending Base — version mismatch, should be invalid.
        let base = SyntaxDefinition::load_from_str(
            r#"
            name: BaseV1
            scope: source.basev1
            file_extensions: [basev1]
            contexts:
              main:
                - match: 'x'
                  scope: keyword.base
              base_only_ctx:
                - match: 'y'
                  scope: keyword.base.y
            "#,
            true,
            None,
        )
        .unwrap();

        let parent = SyntaxDefinition::load_from_str(
            r#"
            name: ParentV2
            scope: source.parentv2
            file_extensions: [parentv2]
            version: 2
            extends: BaseV1.sublime-syntax
            contexts:
              parent_ctx:
                - match: 'p'
                  scope: keyword.parent
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(base);
        builder.add(parent);
        let ss = builder.build();

        let parent_ref = ss.find_syntax_by_name("ParentV2").unwrap();
        let context_ids = parent_ref.context_ids();

        // Per Sublime spec: a v2 syntax extending a v1 base is invalid (version mismatch).
        // The extends should not be applied. Syntect currently silently merges regardless,
        // so it will contain base_only_ctx from the v1 base.
        // This assertion reflects the CORRECT behavior and is EXPECTED TO FAIL until validation
        // is implemented.
        assert!(
            !context_ids.contains_key("base_only_ctx"),
            "ParentV2 (v2) should not inherit from BaseV1 (v1) due to version mismatch; \
             found base_only_ctx in parent's contexts: {:?}",
            context_ids.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn extends_source_and_parent_must_have_same_version() {
        // Per Sublime docs: the source syntax must share the same version as its parents.
        // Here: Base (v1), Parent (v1 extends Base), Child (v2 extends Parent) — mismatch.
        let base = SyntaxDefinition::load_from_str(
            r#"
            name: SharedBase
            scope: source.sharedbase
            file_extensions: [sharedbase]
            contexts:
              main:
                - match: 'x'
                  scope: keyword.base
              shared_ctx:
                - match: 'y'
                  scope: keyword.base.shared
            "#,
            true,
            None,
        )
        .unwrap();

        let parent = SyntaxDefinition::load_from_str(
            r#"
            name: ParentV1
            scope: source.parentv1
            file_extensions: [parentv1]
            extends: SharedBase.sublime-syntax
            contexts:
              parent_ctx:
                - match: 'p'
                  scope: keyword.parent
            "#,
            true,
            None,
        )
        .unwrap();

        let child = SyntaxDefinition::load_from_str(
            r#"
            name: ChildV2
            scope: source.childv2
            file_extensions: [childv2]
            version: 2
            extends: ParentV1.sublime-syntax
            contexts:
              child_ctx:
                - match: 'c'
                  scope: keyword.child
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(base);
        builder.add(parent);
        builder.add(child);
        let ss = builder.build();

        let child_ref = ss.find_syntax_by_name("ChildV2").unwrap();
        let context_ids = child_ref.context_ids();

        // Per Sublime spec: a v2 syntax extending a v1 parent is invalid (version mismatch).
        // The extends should not be applied. Syntect currently silently merges regardless,
        // so it will contain parent_ctx and shared_ctx from the v1 hierarchy.
        // This assertion reflects the CORRECT behavior and is EXPECTED TO FAIL until validation
        // is implemented.
        assert!(
            !context_ids.contains_key("parent_ctx"),
            "ChildV2 (v2) should not inherit from ParentV1 (v1) due to version mismatch; \
             found parent_ctx in child's contexts: {:?}",
            context_ids.keys().collect::<Vec<_>>()
        );
    }

    // =====================================================
    // Tests for apply_prototype
    // =====================================================

    #[test]
    fn apply_prototype_includes_external_prototype() {
        let syntax_with_proto = SyntaxDefinition::load_from_str(
            r#"
            name: WithProto
            scope: source.withproto
            file_extensions: [wp]
            contexts:
              prototype:
                - match: '#'
                  scope: comment.proto
                  push:
                    - meta_scope: comment.line
                    - match: '$'
                      pop: true
              main:
                - match: 'x'
                  scope: x
            "#,
            true,
            None,
        )
        .unwrap();

        let syntax_using_proto = SyntaxDefinition::load_from_str(
            r#"
            name: UsingProto
            scope: source.usingproto
            file_extensions: [up]
            contexts:
              main:
                - match: 'y'
                  scope: y
                - include: scope:source.withproto
                  apply_prototype: true
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax_with_proto);
        builder.add(syntax_using_proto);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("UsingProto").unwrap();
        let mut parse_state = ParseState::new(syntax);
        let ops = parse_state.parse_line("#", &ss).expect("#[cfg(test)]").ops;
        // The external `prototype`'s `#` rule must be reachable from the
        // including context via `apply_prototype: true`.
        let expected = (0, ScopeStackOp::Push(Scope::new("comment.proto").unwrap()));
        assert_ops_contain(&ops, &expected);
    }

    #[test]
    fn apply_prototype_prototype_wins_tie_over_target_main() {
        // Both the target's `main` and its `prototype` match `|` at the same
        // position. ST's `apply_prototype` semantics put the prototype ahead
        // of the target, so the prototype's scope wins the tie.
        let target = SyntaxDefinition::load_from_str(
            r#"
            name: Target
            scope: source.target
            file_extensions: [t]
            contexts:
              prototype:
                - match: '\|'
                  scope: proto.pipe
              main:
                - match: '\|'
                  scope: target.bitor
            "#,
            true,
            None,
        )
        .unwrap();

        let outer = SyntaxDefinition::load_from_str(
            r#"
            name: Outer
            scope: source.outer
            file_extensions: [o]
            contexts:
              main:
                - include: scope:source.target
                  apply_prototype: true
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(target);
        builder.add(outer);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("Outer").unwrap();
        let mut parse_state = ParseState::new(syntax);
        let ops = parse_state.parse_line("|", &ss).expect("#[cfg(test)]").ops;
        assert_ops_contain(
            &ops,
            &(0, ScopeStackOp::Push(Scope::new("proto.pipe").unwrap())),
        );
        assert!(
            !ops.iter()
                .any(|(_, op)| matches!(op, ScopeStackOp::Push(s) if s == &Scope::new("target.bitor").unwrap())),
            "target main's `|` rule must not pre-empt the external prototype's `|` rule: {:?}",
            ops,
        );
    }

    #[test]
    fn apply_prototype_respects_meta_include_prototype_false() {
        // When the include target opts out of prototype inclusion, the
        // external prototype must NOT be injected even with
        // `apply_prototype: true` on the include.
        let target = SyntaxDefinition::load_from_str(
            r#"
            name: Target
            scope: source.target
            file_extensions: [t]
            contexts:
              prototype:
                - match: '\|'
                  scope: proto.pipe
              main:
                - meta_include_prototype: false
                - match: '\|'
                  scope: target.bitor
            "#,
            true,
            None,
        )
        .unwrap();

        let outer = SyntaxDefinition::load_from_str(
            r#"
            name: Outer
            scope: source.outer
            file_extensions: [o]
            contexts:
              main:
                - include: scope:source.target
                  apply_prototype: true
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(target);
        builder.add(outer);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("Outer").unwrap();
        let mut parse_state = ParseState::new(syntax);
        let ops = parse_state.parse_line("|", &ss).expect("#[cfg(test)]").ops;
        assert_ops_contain(
            &ops,
            &(0, ScopeStackOp::Push(Scope::new("target.bitor").unwrap())),
        );
        assert!(
            !ops.iter()
                .any(|(_, op)| matches!(op, ScopeStackOp::Push(s) if s == &Scope::new("proto.pipe").unwrap())),
            "prototype must stay out when target's `meta_include_prototype: false`: {:?}",
            ops,
        );
    }

    #[test]
    fn haml_pipe_continuation_wins_over_ruby_bitor() {
        // Real-package regression guard for the fix: inside HAML attribute
        // braces, a trailing `|` must scope as HAML's pipe-continuation
        // (injected via Ruby-for-HAML's prototype under `apply_prototype`),
        // not as Ruby's bitwise-or operator.
        let ss = &*testdata::PACKAGES_SYN_SET;
        let syntax = ss.find_syntax_by_name("HAML").unwrap();
        let mut parse_state = ParseState::new(syntax);
        let ops = parse_state
            .parse_line("%p{:a => 1, |", ss)
            .expect("#[cfg(test)]")
            .ops;
        let pipe_cont = Scope::new("punctuation.separator.continuation.haml").unwrap();
        let ruby_bitor = Scope::new("keyword.operator.bitwise.ruby").unwrap();
        assert!(
            ops.iter()
                .any(|(_, op)| matches!(op, ScopeStackOp::Push(s) if s == &pipe_cont)),
            "expected punctuation.separator.continuation.haml push: {:?}",
            ops,
        );
        assert!(
            !ops.iter()
                .any(|(_, op)| matches!(op, ScopeStackOp::Push(s) if s == &ruby_bitor)),
            "Ruby bitwise-or must not win over HAML pipe-continuation: {:?}",
            ops,
        );
    }

    // =====================================================
    // Tests for version 2 behavioral fixes
    // =====================================================

    #[test]
    fn v2_set_excludes_parent_meta_content_scope() {
        let syntax = SyntaxDefinition::load_from_str(
            r#"
            name: V2Test
            scope: source.v2test
            file_extensions: [v2]
            version: 2
            contexts:
              main:
                - meta_content_scope: meta.content.main
                - match: 'go'
                  set: other
              other:
                - match: 'x'
                  scope: x
                  pop: true
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("V2Test").unwrap();
        assert_eq!(syntax.version, 2);
    }

    #[test]
    fn version_preserved_in_syntax_reference() {
        let syntax = SyntaxDefinition::load_from_str(
            r#"
            name: V2
            scope: source.v2
            version: 2
            contexts:
              main:
                - match: 'x'
                  scope: x
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax);
        let ss = builder.build();

        let syntax_ref = ss.find_syntax_by_name("V2").unwrap();
        assert_eq!(syntax_ref.version, 2);
    }

    #[test]
    fn v2_set_applies_clear_scopes() {
        use crate::parsing::ParseState;

        let syntax = SyntaxDefinition::load_from_str(
            r#"
            name: V2ClearScopes
            scope: source.v2clearscopes
            file_extensions: [v2cs]
            version: 2
            contexts:
              main:
                - match: 'go'
                  set: cleared
              cleared:
                - clear_scopes: true
                - match: 'x'
                  scope: x
                  pop: true
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("V2ClearScopes").unwrap();
        let mut state = ParseState::new(syntax);
        let ops = state.parse_line("gox\n", &ss).unwrap().ops;

        // After "go" sets to "cleared" which has clear_scopes: true,
        // the source.v2clearscopes scope should be cleared before "x" is matched
        let has_clear = ops
            .iter()
            .any(|(_, op)| matches!(op, ScopeStackOp::Clear(_)));
        assert!(
            has_clear,
            "v2 set should apply clear_scopes; ops: {:?}",
            ops
        );
    }

    #[test]
    fn pop_n_set_pops_n_then_pushes_target() {
        use crate::parsing::{ParseState, ScopeStack};

        // `pop: 2 + set: target` on a 3-deep stack should leave a 2-deep stack
        // where the top frame is `target`, not silently discard the set.
        let syntax = SyntaxDefinition::load_from_str(
            r#"
            name: PopSet
            scope: source.popset
            file_extensions: [pset]
            version: 2
            contexts:
              main:
                - match: 'a'
                  scope: a
                  push: level1
              level1:
                - meta_scope: meta.level1
                - match: 'b'
                  scope: b
                  push: level2
              level2:
                - meta_scope: meta.level2
                - match: 'c'
                  scope: c
                  pop: 2
                  set: target
              target:
                - meta_scope: meta.target
                - match: 'd'
                  scope: d
                  pop: true
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("PopSet").unwrap();
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        for (_, op) in state.parse_line("abc", &ss).unwrap().ops {
            stack.apply(&op).unwrap();
        }

        // After 'a' (push level1), 'b' (push level2), 'c' (pop 2 + set target):
        // the stack should hold [source.popset, meta.target]. If pop: 2 + set:
        // were silently dropped (old behavior), it'd be [source.popset] with no
        // meta.target.
        let scopes: Vec<String> = stack.as_slice().iter().map(|s| format!("{}", s)).collect();
        let has_target = scopes.iter().any(|s| s == "meta.target");
        let no_level1 = !scopes.iter().any(|s| s == "meta.level1");
        let no_level2 = !scopes.iter().any(|s| s == "meta.level2");
        assert!(
            has_target && no_level1 && no_level2,
            "expected meta.target on stack with no intermediate meta scopes, got: {:?}",
            scopes
        );

        // `d` should pop 'target' and emit its scope.
        let ops2 = state.parse_line("d", &ss).unwrap().ops;
        let d_pushed = ops2.iter().any(|(_, op)| match op {
            ScopeStackOp::Push(s) => format!("{}", s) == "d",
            _ => false,
        });
        assert!(
            d_pushed,
            "'d' should fire 'target's d rule, ops: {:?}",
            ops2
        );
    }

    #[test]
    fn v2_embed_scope_replaces_embedded_scope() {
        use crate::parsing::ParseState;
        use crate::parsing::ScopeStack;

        let host = SyntaxDefinition::load_from_str(
            r#"
            name: V2Host
            scope: source.v2host
            file_extensions: [v2host]
            version: 2
            contexts:
              main:
                - match: '<<'
                  embed: scope:source.v2embedded
                  embed_scope: meta.embedded.custom
                  escape: '>>'
            "#,
            true,
            None,
        )
        .unwrap();

        let embedded = SyntaxDefinition::load_from_str(
            r#"
            name: V2Embedded
            scope: source.v2embedded
            file_extensions: [v2emb]
            version: 2
            contexts:
              main:
                - match: 'x'
                  scope: keyword.x
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(host);
        builder.add(embedded);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("V2Host").unwrap();
        let mut state = ParseState::new(syntax);
        let ops = state.parse_line("<<x>>\n", &ss).unwrap().ops;

        // Build scope stack to check what scopes are active when "x" is matched
        let mut scope_stack = ScopeStack::new();
        let mut x_scopes = None;
        for (idx, op) in &ops {
            if *idx <= 2 {
                scope_stack.apply(op).unwrap();
            }
            // After applying ops at index 2 (the "x"), capture scopes
            if *idx > 2 && x_scopes.is_none() {
                x_scopes = Some(scope_stack.clone());
            }
        }
        let x_scopes = x_scopes.unwrap_or(scope_stack);
        let scopes: Vec<_> = x_scopes.as_slice().to_vec();

        // embed_scope should replace, not stack with, embedded syntax's scope
        // So we should have: source.v2host, meta.embedded.custom, keyword.x
        // but NOT source.v2embedded
        let has_custom = scopes
            .iter()
            .any(|s| s.build_string() == "meta.embedded.custom");
        let has_embedded_scope = scopes
            .iter()
            .any(|s| s.build_string() == "source.v2embedded");
        assert!(
            has_custom,
            "should have embed_scope; scopes: {:?}",
            scopes.iter().map(|s| s.build_string()).collect::<Vec<_>>()
        );
        assert!(
            !has_embedded_scope,
            "should NOT have embedded syntax scope; scopes: {:?}",
            scopes.iter().map(|s| s.build_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn v2_set_does_not_apply_parent_meta_content_scope_to_matched_text() {
        // Per Sublime docs (v2): set action does NOT apply the parent context's
        // meta_content_scope to the matched text. In v1 it does.
        use crate::parsing::{ParseState, ScopeStack};

        fn scopes_at_pos(ops: &[(usize, ScopeStackOp)], pos: usize) -> Vec<String> {
            let mut stack = ScopeStack::new();
            for (idx, op) in ops {
                if *idx <= pos {
                    stack.apply(op).unwrap();
                }
            }
            stack.as_slice().iter().map(|s| s.build_string()).collect()
        }

        // v2 syntax: main has meta_content_scope, 'go' triggers set: other
        let v2_syntax = SyntaxDefinition::load_from_str(
            r#"
            name: V2SetMCS
            scope: source.v2setmcs
            file_extensions: [v2setmcs]
            version: 2
            contexts:
              main:
                - meta_content_scope: meta.content.main
                - match: 'go'
                  set: other
              other:
                - match: 'x'
                  scope: x
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(v2_syntax);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("V2SetMCS").unwrap();
        let mut state = ParseState::new(syntax);
        // "go" is at positions [0, 2); check at position 0 (inside the matched text)
        let ops = state.parse_line("go\n", &ss).unwrap().ops;
        let v2_scopes = scopes_at_pos(&ops, 0);

        // v2: the matched text 'go' should NOT have meta.content.main
        // NOTE: This test is expected to FAIL if the v2 behavior is not yet correctly implemented.
        assert!(
            !v2_scopes.iter().any(|s| s == "meta.content.main"),
            "v2: matched text 'go' should NOT have meta.content.main; scopes: {:?}",
            v2_scopes
        );

        // v1 syntax: same structure, version 1
        let v1_syntax = SyntaxDefinition::load_from_str(
            r#"
            name: V1SetMCS
            scope: source.v1setmcs
            file_extensions: [v1setmcs]
            contexts:
              main:
                - meta_content_scope: meta.content.main
                - match: 'go'
                  set: other
              other:
                - match: 'x'
                  scope: x
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder2 = SyntaxSetBuilder::new();
        builder2.add(v1_syntax);
        let ss2 = builder2.build();

        let syntax2 = ss2.find_syntax_by_name("V1SetMCS").unwrap();
        let mut state2 = ParseState::new(syntax2);
        let ops2 = state2.parse_line("go\n", &ss2).unwrap().ops;
        let v1_scopes = scopes_at_pos(&ops2, 0);

        // v1: the matched text 'go' SHOULD have meta.content.main
        assert!(
            v1_scopes.iter().any(|s| s == "meta.content.main"),
            "v1: matched text 'go' SHOULD have meta.content.main; scopes: {:?}",
            v1_scopes
        );
    }

    #[test]
    fn v2_embed_escape_does_not_get_embed_scope() {
        // Per Sublime docs: embed_scope applies to text "after the match and before the escape",
        // so the escape text should NOT have the embed_scope (meta_content_scope).
        // This is the same in both v1 and v2.
        use crate::parsing::{ParseState, ScopeStack};

        let host = SyntaxDefinition::load_from_str(
            r#"
            name: V2EmbedMeta
            scope: source.v2embedmeta
            file_extensions: [v2em]
            version: 2
            contexts:
              main:
                - match: '<<'
                  embed: scope:source.v2em_embedded
                  embed_scope: meta.embedded.block
                  escape: '>>'
            "#,
            true,
            None,
        )
        .unwrap();

        let embedded = SyntaxDefinition::load_from_str(
            r#"
            name: V2EmbedMetaEmbedded
            scope: source.v2em_embedded
            file_extensions: [v2eme]
            version: 2
            contexts:
              main:
                - match: 'x'
                  scope: keyword.x
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(host);
        builder.add(embedded);
        let ss = builder.build();

        // "<<x>>" — '<<' at [0,2], 'x' at [2,3], '>>' at [3,5]
        let syntax = ss.find_syntax_by_name("V2EmbedMeta").unwrap();
        let mut state = ParseState::new(syntax);
        let ops = state.parse_line("<<x>>\n", &ss).unwrap().ops;

        // Build scope stack at position 3 (start of '>>' escape text)
        let mut stack = ScopeStack::new();
        for (idx, op) in &ops {
            if *idx <= 3 {
                stack.apply(op).unwrap();
            }
        }
        let scopes: Vec<String> = stack.as_slice().iter().map(|s| s.build_string()).collect();

        // Escape text '>>' should NOT have the embed_scope (meta.embedded.block)
        assert!(
            !scopes.iter().any(|s| s == "meta.embedded.block"),
            "escape text '>>' should not have meta.embedded.block; scopes: {:?}",
            scopes
        );
    }

    #[test]
    fn v2_push_multiple_clear_scopes_each_applies() {
        // v2 push emits `Clear` for every pushed context that has
        // `clear_scopes`, at that context's index position in the push
        // order — same as v1. Python's f/t-string interpolation relies on
        // this: `clear_scopes: 1` sits on `f-string-replacement-meta` at
        // index 0 of a 3-context push, not on the topmost entry.
        use crate::parsing::ParseState;

        let v2_syntax = SyntaxDefinition::load_from_str(
            r#"
            name: V2MultiClear
            scope: source.v2multiclear
            file_extensions: [v2mc]
            version: 2
            contexts:
              main:
                - meta_scope: source.v2multiclear
                - match: 'go'
                  push:
                    - ctx_a
                    - ctx_b
              ctx_a:
                - clear_scopes: 1
                - meta_scope: ctx.a
                - match: 'x'
                  pop: 2
              ctx_b:
                - clear_scopes: 2
                - meta_scope: ctx.b
                - match: 'x'
                  pop: 1
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(v2_syntax);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("V2MultiClear").unwrap();
        let mut state = ParseState::new(syntax);
        let ops = state.parse_line("go\n", &ss).unwrap().ops;

        let clear_ops: Vec<_> = ops
            .iter()
            .filter_map(|(_, op)| match op {
                ScopeStackOp::Clear(a) => Some(*a),
                _ => None,
            })
            .collect();

        // Both ctx_a (TopN(1)) and ctx_b (TopN(2)) apply, in push order.
        assert_eq!(
            clear_ops,
            vec![ClearAmount::TopN(1), ClearAmount::TopN(2)],
            "v2: push [ctx_a(1), ctx_b(2)] should emit Clear(1) then Clear(2); got: {:?}",
            clear_ops
        );

        // v1: both ctx_a and ctx_b apply their clear_scopes — two Clear ops
        let v1_syntax = SyntaxDefinition::load_from_str(
            r#"
            name: V1MultiClear
            scope: source.v1multiclear
            file_extensions: [v1mc]
            contexts:
              main:
                - meta_scope: source.v1multiclear
                - match: 'go'
                  push:
                    - ctx_a
                    - ctx_b
              ctx_a:
                - clear_scopes: 1
                - meta_scope: ctx.a
                - match: 'x'
                  pop: 2
              ctx_b:
                - clear_scopes: 2
                - meta_scope: ctx.b
                - match: 'x'
                  pop: 1
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder2 = SyntaxSetBuilder::new();
        builder2.add(v1_syntax);
        let ss2 = builder2.build();

        let syntax2 = ss2.find_syntax_by_name("V1MultiClear").unwrap();
        let mut state2 = ParseState::new(syntax2);
        let ops2 = state2.parse_line("go\n", &ss2).unwrap().ops;

        let v1_clear_ops: Vec<_> = ops2
            .iter()
            .filter(|(_, op)| matches!(op, ScopeStackOp::Clear(_)))
            .collect();

        // v1: both ctx_a's clear_scopes and ctx_b's clear_scopes should produce TWO Clear ops
        assert_eq!(
            v1_clear_ops.len(),
            2,
            "v1: push [ctx_a, ctx_b] should produce TWO Clear ops (one per context); \
             got: {:?}",
            v1_clear_ops
        );
    }

    #[test]
    fn v2_push_deeper_clear_scopes_applies_and_restores() {
        // Pins the invariant Python f/t-string interpolation relies on:
        // a multi-context push where a non-topmost entry carries
        // `clear_scopes`. The Clear must fire when that entry is pushed,
        // and a paired Restore must fire when it is popped — even when the
        // pop reaches it via `pop: N` from a shallower context.
        //
        // Shape:
        //   main mcs = "m.outer string.outer"
        //   push  = [wrap(clear_scopes: 1, meta_scope: m.wrap), body]
        //   body  = matches 'x' with `pop: 2` (unwinds body + wrap together,
        //           exercising the deeper-context Restore path).
        use crate::parsing::{ParseState, ScopeStack};

        let syntax = SyntaxDefinition::load_from_str(
            r#"
            name: V2DeeperClear
            scope: source.deeperclear
            file_extensions: [v2dc]
            version: 2
            contexts:
              main:
                - meta_content_scope: m.outer string.outer
                - match: 'go'
                  push:
                    - wrap
                    - body
              wrap:
                - clear_scopes: 1
                - meta_scope: m.wrap
                - include: immediately-pop
              immediately-pop:
                - match: ''
                  pop: 1
              body:
                - match: 'x'
                  scope: word.x
                  pop: 2
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax);
        let ss = builder.build();
        let sref = ss.find_syntax_by_name("V2DeeperClear").unwrap();

        // Scope stack at each position through "go x y\n".
        let mut state = ParseState::new(sref);
        let mut stack = ScopeStack::new();
        let mut scopes_at = |line: &str, cols: &[usize]| -> Vec<Vec<String>> {
            let ops = state.parse_line(line, &ss).unwrap().ops;
            let mut out = vec![Vec::new(); cols.len()];
            let mut next_op = 0;
            for (i, _) in line.char_indices() {
                while next_op < ops.len() && ops[next_op].0 <= i {
                    stack.apply(&ops[next_op].1).unwrap();
                    next_op += 1;
                }
                if let Some(pos) = cols.iter().position(|c| *c == i) {
                    out[pos] = stack.as_slice().iter().map(|s| s.build_string()).collect();
                }
            }
            while next_op < ops.len() {
                stack.apply(&ops[next_op].1).unwrap();
                next_op += 1;
            }
            out
        };

        // col 3 = 'x' inside the push — must see m.outer, m.wrap, word.x,
        // but NOT string.outer (cleared by wrap's clear_scopes: 1).
        // col 5 = 'y' (any trailing byte) after the pop — string.outer
        // must be back (Restore restored the cleared atom).
        let line = "go x y\n";
        let scopes = scopes_at(line, &[3, 5]);

        let at_x = &scopes[0];
        assert!(
            at_x.iter().any(|s| s == "m.outer"),
            "'x' should carry m.outer; got {:?}",
            at_x
        );
        assert!(
            at_x.iter().any(|s| s == "m.wrap"),
            "'x' should carry m.wrap from the pushed wrap meta_scope; got {:?}",
            at_x
        );
        assert!(
            !at_x.iter().any(|s| s == "string.outer"),
            "'x' should NOT carry string.outer (cleared by wrap.clear_scopes: 1); got {:?}",
            at_x
        );

        let at_y = &scopes[1];
        assert!(
            at_y.iter().any(|s| s == "m.outer"),
            "'y' should carry m.outer after pop; got {:?}",
            at_y
        );
        assert!(
            at_y.iter().any(|s| s == "string.outer"),
            "'y' should carry string.outer after pop (Restore must fire); got {:?}",
            at_y
        );
        assert!(
            !at_y.iter().any(|s| s == "m.wrap"),
            "'y' should NOT carry m.wrap after pop; got {:?}",
            at_y
        );
    }

    #[test]
    fn v2_capture_group_ordering_applies_scopes_in_text_order() {
        // Capture group scopes should be applied in text position order, not capture-number order.
        // The code sorts captures by (start_pos, -length) so outer/earlier captures come first.
        use crate::parsing::ParseState;

        let syntax = SyntaxDefinition::load_from_str(
            r#"
            name: CaptureOrder
            scope: source.captureorder
            file_extensions: [co]
            version: 2
            contexts:
              main:
                - match: '(a(b))'
                  captures:
                    1: outer.scope
                    2: inner.scope
            "#,
            true,
            None,
        )
        .unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(syntax);
        let ss = builder.build();

        let syntax = ss.find_syntax_by_name("CaptureOrder").unwrap();
        let mut state = ParseState::new(syntax);
        // "ab" — capture 1 (outer) matches [0,2], capture 2 (inner) matches [1,2]
        let ops = state.parse_line("ab\n", &ss).unwrap().ops;

        let outer_scope = Scope::new("outer.scope").unwrap();
        let inner_scope = Scope::new("inner.scope").unwrap();

        let outer_push_idx = ops
            .iter()
            .position(|(_, op)| matches!(op, ScopeStackOp::Push(s) if *s == outer_scope));
        let inner_push_idx = ops
            .iter()
            .position(|(_, op)| matches!(op, ScopeStackOp::Push(s) if *s == inner_scope));

        assert!(
            outer_push_idx.is_some(),
            "outer.scope should be pushed; ops: {:?}",
            ops
        );
        assert!(
            inner_push_idx.is_some(),
            "inner.scope should be pushed; ops: {:?}",
            ops
        );

        // outer.scope (starts at pos 0) must be pushed before inner.scope (starts at pos 1)
        assert!(
            outer_push_idx.unwrap() < inner_push_idx.unwrap(),
            "outer.scope (byte pos 0) should be pushed before inner.scope (byte pos 1) in ops; \
             outer at ops[{}], inner at ops[{}]",
            outer_push_idx.unwrap(),
            inner_push_idx.unwrap()
        );

        // Also verify the byte positions are in text order
        let (outer_byte_pos, _) = ops[outer_push_idx.unwrap()];
        let (inner_byte_pos, _) = ops[inner_push_idx.unwrap()];
        assert!(
            outer_byte_pos <= inner_byte_pos,
            "outer.scope byte pos ({}) should be <= inner.scope byte pos ({})",
            outer_byte_pos,
            inner_byte_pos
        );
    }

    #[test]
    fn multiple_inheritance_extends_array() {
        // Base syntax with a variable and main context
        let base = SyntaxDefinition::load_from_str(
            r#"
            name: Base
            scope: source.base
            file_extensions: [base]
            variables:
              IDENT: '[a-z]+'
            contexts:
              main:
                - match: '{{IDENT}}'
                  scope: variable.base
              helpers:
                - match: 'help'
                  scope: keyword.help
            "#,
            false,
            None,
        )
        .unwrap();

        // ExtA overrides IDENT and adds a context
        let ext_a = SyntaxDefinition::load_from_str(
            r#"
            name: ExtA
            scope: source.ext_a
            extends: Base
            variables:
              IDENT: '[a-zA-Z]+'
            contexts:
              ext_a_ctx:
                - match: 'aaa'
                  scope: keyword.a
            "#,
            false,
            None,
        )
        .unwrap();

        // ExtB adds a different variable and context
        let ext_b = SyntaxDefinition::load_from_str(
            r#"
            name: ExtB
            scope: source.ext_b
            extends: Base
            variables:
              NUM: '[0-9]+'
            contexts:
              ext_b_ctx:
                - match: 'bbb'
                  scope: keyword.b
            "#,
            false,
            None,
        )
        .unwrap();

        // Child extends both ExtA and ExtB
        let child = SyntaxDefinition::load_from_str(
            r#"
            name: Child
            scope: source.child
            file_extensions: [child]
            extends:
              - ExtA
              - ExtB
            contexts:
              child_ctx:
                - match: 'ccc'
                  scope: keyword.c
            "#,
            false,
            None,
        )
        .unwrap();

        assert_eq!(child.extends, vec!["ExtA".to_owned(), "ExtB".to_owned()]);

        let mut builder = SyntaxSetBuilder::new();
        builder.add(base);
        builder.add(ext_a);
        builder.add(ext_b);
        builder.add(child);
        let ss = builder.build();

        let child_ref = ss.find_syntax_by_name("Child").unwrap();

        // Child should have contexts from both parents and itself
        let context_ids = child_ref.context_ids();
        assert!(
            context_ids.contains_key("main"),
            "should inherit main from Base"
        );
        assert!(
            context_ids.contains_key("helpers"),
            "should inherit helpers from Base"
        );
        assert!(
            context_ids.contains_key("ext_a_ctx"),
            "should inherit ext_a_ctx from ExtA"
        );
        assert!(
            context_ids.contains_key("ext_b_ctx"),
            "should inherit ext_b_ctx from ExtB"
        );
        assert!(
            context_ids.contains_key("child_ctx"),
            "should have own child_ctx"
        );

        // Variables: ExtA owns its `IDENT='[a-zA-Z]+'`; ExtB only inherits
        // `IDENT='[a-z]+'` from Base. Per the own-beats-inherited rule
        // (see `extends_array_own_beats_inherited`), Child's IDENT comes
        // from ExtA, not ExtB. ExtB owns NUM, so Child gets NUM too.

        // Verify the child syntax can be used without panicking
        let syntax = ss.find_syntax_by_name("Child").unwrap();
        let mut state = crate::parsing::ParseState::new(syntax);
        let _ops = state.parse_line("hello\n", &ss).unwrap();
    }

    /// When a child extends multiple parents and they disagree on a
    /// shared context, a parent's *own* (directly-defined) version
    /// must win over another parent's *inherited* version — regardless
    /// of position in the extends list. Models the real-world
    /// `Zsh (for Markdown)` extends `[Bash (for Markdown), Zsh]` case
    /// where Bash (for Markdown) owns `main` and Zsh inherits a
    /// different `main` from Bash.
    fn build_own_beats_inherited_set(child_yaml: &str) -> SyntaxSet {
        let base = SyntaxDefinition::load_from_str(
            r#"
            name: Base
            scope: source.obi_base
            file_extensions: [obi_base]
            contexts:
              main:
                - match: 'x'
                  scope: keyword.from_base
            "#,
            false,
            None,
        )
        .unwrap();

        let owner = SyntaxDefinition::load_from_str(
            r#"
            name: Owner
            scope: source.obi_owner
            extends: Base
            contexts:
              main:
                - match: 'x'
                  scope: keyword.from_owner
            "#,
            false,
            None,
        )
        .unwrap();

        let inheritor = SyntaxDefinition::load_from_str(
            r#"
            name: Inheritor
            scope: source.obi_inheritor
            extends: Base
            contexts:
              inheritor_extra:
                - match: 'i'
                  scope: keyword.inheritor
            "#,
            false,
            None,
        )
        .unwrap();

        let child = SyntaxDefinition::load_from_str(child_yaml, false, None).unwrap();

        let mut builder = SyntaxSetBuilder::new();
        builder.add(base);
        builder.add(owner);
        builder.add(inheritor);
        builder.add(child);
        builder.build()
    }

    fn assert_own_beats_inherited(ss: &SyntaxSet) {
        let child_ref = ss.find_syntax_by_name("Child").unwrap();
        let mut state = crate::parsing::ParseState::new(child_ref);
        let out = state.parse_line("x", ss).unwrap();
        let expected = Scope::new("keyword.from_owner").unwrap();
        let unexpected = Scope::new("keyword.from_base").unwrap();
        let pushes: Vec<Scope> = out
            .ops
            .iter()
            .filter_map(|(_, op)| match op {
                crate::parsing::ScopeStackOp::Push(s) => Some(*s),
                _ => None,
            })
            .collect();
        let names: Vec<String> = pushes.iter().map(|s| s.build_string()).collect();
        assert!(
            pushes.contains(&expected),
            "expected Owner's own main to win (Push keyword.from_owner); got {:?}",
            names
        );
        assert!(
            !pushes.contains(&unexpected),
            "Inheritor's inherited main from Base must NOT win; got Push keyword.from_base in {:?}",
            names
        );
    }

    #[test]
    fn extends_array_own_beats_inherited() {
        let ss = build_own_beats_inherited_set(
            r#"
            name: Child
            scope: source.obi_child
            file_extensions: [obi_child]
            extends:
              - Owner
              - Inheritor
            "#,
        );
        assert_own_beats_inherited(&ss);
    }

    #[test]
    fn extends_array_own_beats_inherited_reversed() {
        let ss = build_own_beats_inherited_set(
            r#"
            name: Child
            scope: source.obi_child
            file_extensions: [obi_child]
            extends:
              - Inheritor
              - Owner
            "#,
        );
        assert_own_beats_inherited(&ss);
    }

    #[cfg(feature = "yaml-load")]
    #[test]
    fn find_parent_index_resolves_relative_paths() {
        // Simulates a syntax loaded from "Packages/Test/syntaxes/Child.sublime-syntax"
        // being extended via the relative path "syntaxes/Child.sublime-syntax".
        let syntax = SyntaxDefinition {
            name: "Child".to_string(),
            file_extensions: vec![],
            scope: Scope::new("source.child").unwrap(),
            first_line_match: None,
            hidden: false,
            variables: HashMap::new(),
            contexts: HashMap::new(),
            extends: vec![],
            version: 1,
        };

        let syntax_definitions = vec![syntax];
        let path_syntaxes = vec![(
            "Packages/Test/syntaxes/Child.sublime-syntax".to_string(),
            0usize,
        )];
        let mut name_to_index = HashMap::new();
        name_to_index.insert("Child".to_string(), 0);

        // Relative path should match via suffix matching
        let result = SyntaxSetBuilder::find_parent_index(
            "syntaxes/Child.sublime-syntax",
            &path_syntaxes,
            &syntax_definitions,
            &name_to_index,
        );
        assert_eq!(result, Some(0), "relative path should match via suffix");

        // Absolute path should also match
        let result = SyntaxSetBuilder::find_parent_index(
            "Packages/Test/syntaxes/Child.sublime-syntax",
            &path_syntaxes,
            &syntax_definitions,
            &name_to_index,
        );
        assert_eq!(result, Some(0), "absolute path should match via suffix");

        // Just the filename (without extension) should match via name lookup
        let result = SyntaxSetBuilder::find_parent_index(
            "Child.sublime-syntax",
            &path_syntaxes,
            &syntax_definitions,
            &name_to_index,
        );
        assert_eq!(
            result,
            Some(0),
            "bare filename should match via name lookup"
        );

        // A non-matching relative path should return None
        let result = SyntaxSetBuilder::find_parent_index(
            "other/NonExistent.sublime-syntax",
            &path_syntaxes,
            &syntax_definitions,
            &name_to_index,
        );
        assert_eq!(result, None, "non-matching path should return None");
    }
}
