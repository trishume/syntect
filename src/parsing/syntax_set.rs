use super::syntax_definition::*;
use super::scope::*;

#[cfg(feature = "metadata")]
use super::metadata::{LoadMetadata, Metadata, RawMetadataEntry};

#[cfg(feature = "yaml-load")]
use super::super::LoadingError;

use std::collections::{HashMap, HashSet, BTreeSet};
use std::path::Path;
#[cfg(feature = "yaml-load")]
use walkdir::WalkDir;
#[cfg(feature = "yaml-load")]
use std::io::Read;
use std::io::{self, BufRead, BufReader};
use std::fs::File;
use std::mem;

use lazycell::AtomicLazyCell;
use super::regex::Regex;
use crate::parsing::syntax_definition::ContextId;

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

    #[serde(skip_serializing, skip_deserializing, default = "AtomicLazyCell::new")]
    first_line_cache: AtomicLazyCell<FirstLineCache>,
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
    #[serde(skip)]
    pub(crate) lazy_loaded: lazycell::AtomicLazyCell<LazyLoaded>,
    pub(crate) serialized_lazy_loaded: Vec<u8>,
}

/// The lazy-loaded parts of a [`SyntaxReference`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct LazyLoaded {
    /// TODO: Moving this here breaks API, we but we already break it by changing ContextId
    /// We could leave it outside if we wished. We don't actually need to keep track
    /// of Sublime Syntax varibles inside SyntaxReferences at all, but that is a separate discussion.
    #[serde(serialize_with = "ordered_map")]
    pub variables: HashMap<String, String>,
    #[serde(serialize_with = "ordered_map")]
    pub(crate) contexts_map: HashMap<String, ContextId>,
    pub(crate) contexts: Vec<Context>,
}

/// Used in [`SyntaxSetBuilder::build`]
#[derive(Clone, Debug)]
struct CompleteSyntaxReference {
    name: String,
    file_extensions: Vec<String>,
    scope: Scope,
    first_line_match: Option<String>,
    hidden: bool,
    variables: HashMap<String, String>,
    contexts_map: HashMap<String, ContextId>,
    contexts: Vec<Context>,
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
    #[cfg(feature = "metadata")]
    raw_metadata: LoadMetadata,

    /// If this `SyntaxSetBuilder` is created with `SyntaxSet::into_builder`
    /// from a `SyntaxSet` that already had metadata, we keep that metadata,
    /// merging it with newly loaded metadata.
    #[cfg(feature = "metadata")]
    existing_metadata: Option<Metadata>,
}

#[cfg(feature = "yaml-load")]
fn load_syntax_file(p: &Path,
                    lines_include_newline: bool)
                    -> Result<SyntaxDefinition, LoadingError> {
    let mut f = File::open(p)?;
    let mut s = String::new();
    f.read_to_string(&mut s)?;

    SyntaxDefinition::load_from_str(
        &s,
        lines_include_newline,
        p.file_stem().and_then(|x| x.to_str()),
    )
    .map_err(|e| LoadingError::ParseSyntax(e, Some(format!("{}", p.display()))))
}

impl Clone for SyntaxSet {
    fn clone(&self) -> SyntaxSet {
        SyntaxSet {
            syntaxes: self.syntaxes.clone(),
            path_syntaxes: self.path_syntaxes.clone(),
            // Will need to be re-initialized
            first_line_cache: AtomicLazyCell::new(),
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
            first_line_cache: AtomicLazyCell::new(),
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
        self.syntaxes.iter().rev().find(|&s| s.file_extensions.iter().any(|e| e.eq_ignore_ascii_case(extension)))
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
        self.syntaxes.iter().rev().find(|&syntax| syntax.name.eq_ignore_ascii_case(s))
    }

    /// Try to find the syntax for a file based on its first line
    ///
    /// This uses regexes that come with some sublime syntax grammars for matching things like
    /// shebangs and mode lines like `-*- Mode: C -*-`
    pub fn find_syntax_by_first_line<'a>(&'a self, s: &str) -> Option<&'a SyntaxReference> {
        let cache = self.first_line_cache();
        for &(ref reg, i) in cache.regexes.iter().rev() {
            if reg.search(s, 0, s.len(), None) {
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
        self.path_syntaxes.iter().rev().find(|t| t.0.ends_with(&slash_path) || t.0 == path).map(|&(_,i)| &self.syntaxes[i])
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
    pub fn find_syntax_for_file<P: AsRef<Path>>(&self,
                                                path_obj: P)
                                                -> io::Result<Option<&SyntaxReference>> {
        let path: &Path = path_obj.as_ref();
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let extension = path.extension().and_then(|x| x.to_str()).unwrap_or("");
        let ext_syntax = self.find_syntax_by_extension(file_name).or_else(
                            || self.find_syntax_by_extension(extension));
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
        let SyntaxSet { ref syntaxes, ref path_syntaxes, ref metadata, .. } = self;
        #[cfg(not(feature = "metadata"))]
        let SyntaxSet { ref syntaxes, ref path_syntaxes, .. } = self;

        let mut builder_syntaxes = Vec::with_capacity(syntaxes.len());

        for syntax in syntaxes {
            let SyntaxReference {
                name,
                file_extensions,
                scope,
                first_line_match,
                hidden,
                ..
            } = syntax;

            let mut builder_contexts = HashMap::with_capacity(syntax.contexts_map().len());
            for (name, context_id) in syntax.contexts_map() {
                builder_contexts.insert(name.clone(), self.get_context(&context_id).clone());
            }

            let syntax_definition = SyntaxDefinition {
                name: name.clone(),
                file_extensions: file_extensions.clone(),
                scope: *scope,
                first_line_match: first_line_match.clone(),
                hidden: *hidden,
                variables: syntax.variables().clone(),
                contexts: builder_contexts,
            };
            builder_syntaxes.push(syntax_definition);
        }

        SyntaxSetBuilder {
            syntaxes: builder_syntaxes,
            path_syntaxes: path_syntaxes.clone(),
            #[cfg(feature = "metadata")]
            existing_metadata: Some(metadata.clone()),
            #[cfg(feature = "metadata")]
            raw_metadata: LoadMetadata::default(),
        }
    }

    #[inline(always)]
    pub(crate) fn get_context(&self, context_id: &ContextId) -> &Context {
        let syntax = &self.syntaxes[context_id.syntax_index()];
        &syntax.contexts()[context_id.context_index()]
    }

    fn first_line_cache(&self) -> &FirstLineCache {
        if let Some(cache) = self.first_line_cache.borrow() {
            cache
        } else {
            let cache = FirstLineCache::new(self.syntaxes());
            self.first_line_cache.fill(cache).ok();
            self.first_line_cache.borrow().unwrap()
        }
    }

    pub fn find_unlinked_contexts(&self) -> BTreeSet<String> {
        let SyntaxSet { syntaxes, .. } = self;

        let mut unlinked_contexts = BTreeSet::new();

        for syntax in syntaxes {
            let SyntaxReference {
                name,
                scope,
                ..
            } = syntax;

            // Ignore white space changes when diffing this section
            for context_id in syntax.contexts_map().values() {
                let context = self.get_context(&context_id);
                for pattern in context.patterns.iter() {
                    let maybe_refs_to_check = match pattern {
                        Pattern::Match(match_pat) => {
                            match &match_pat.operation {
                                MatchOperation::Push(context_refs) => {
                                    Some(context_refs)
                                },
                                MatchOperation::Set(context_refs) => {
                                    Some(context_refs)
                                },
                                _ => None,
                            }
                        },
                        _ => None,
                    };

                    for context_ref in maybe_refs_to_check.into_iter().flatten() {
                        match context_ref {
                            ContextReference::Direct(_) => {},
                            _ => {
                                unlinked_contexts.insert(
                                    format!(
                                        "Syntax '{}' with scope '{}' has unresolved context reference {:?}",
                                        name, scope, &context_ref
                                    )
                                );
                            },
                        }
                    }
                }
            }
        }
        unlinked_contexts
    }
}

impl SyntaxReference {
    pub(crate) fn lazy_loaded(&self) -> &LazyLoaded {
        if let Some(lazy_loaded) = self.lazy_loaded.borrow() {
            lazy_loaded
        } else {
            self.lazy_loaded
                .fill(
                    crate::dumps::from_reader(&self.serialized_lazy_loaded[..])
                        .expect("data is not corrupt or out of sync with the code"),
                )
                .ok();
            //eprintln!("Syntax {} lazy-loaded", self.name);
            self.lazy_loaded.borrow().unwrap()
        }
    }
 
    pub(crate) fn contexts_map(&self) -> &HashMap<String, ContextId> {
        &self.lazy_loaded().contexts_map
    }
 
    fn variables(&self) -> &HashMap<String, String> {
        &self.lazy_loaded().variables
    }
 
    fn contexts(&self) -> &[Context] {
        &self.lazy_loaded().contexts
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
        lines_include_newline: bool
    ) -> Result<(), LoadingError> {
        for entry in WalkDir::new(folder).sort_by(|a, b| a.file_name().cmp(b.file_name())) {
            let entry = entry.map_err(LoadingError::WalkDir)?;
            if entry.path().extension().map_or(false, |e| e == "sublime-syntax") {
                let syntax = load_syntax_file(entry.path(), lines_include_newline)?;
                if let Some(path_str) = entry.path().to_str() {
                    // Split the path up and rejoin with slashes so that syntaxes loaded on Windows
                    // can still be loaded the same way.
                    let path = Path::new(path_str);
                    let path_parts: Vec<_> = path.iter().map(|c| c.to_str().unwrap()).collect();
                    self.path_syntaxes.push((path_parts.join("/").to_string(), self.syntaxes.len()));
                }
                self.syntaxes.push(syntax);
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
        let SyntaxSetBuilder { syntaxes: syntax_definitions, path_syntaxes } = self;
        #[cfg(feature = "metadata")]
        let SyntaxSetBuilder {
            syntaxes: syntax_definitions,
            path_syntaxes,
            raw_metadata,
            existing_metadata,
        } = self;

        let mut syntaxes = Vec::with_capacity(syntax_definitions.len());

        for (syntax_index, syntax_definition) in syntax_definitions.into_iter().enumerate() {
            let SyntaxDefinition {
                name,
                file_extensions,
                scope,
                first_line_match,
                hidden,
                variables,
                contexts,
            } = syntax_definition;

            let mut map = HashMap::new();
            let mut syntax_contexts = Vec::new();

            let mut contexts: Vec<(String, Context)> = contexts.into_iter().collect();
            // Sort the values of the HashMap so that the contexts in the
            // resulting SyntaxSet have a deterministic order for serializing.
            // Because we're sorting by the keys which are unique, we can use
            // an unstable sort.
            contexts.sort_unstable_by(|(name_a, _), (name_b, _)| name_a.cmp(name_b));
            for (name, context) in contexts {
                let context_index = syntax_contexts.len();
                map.insert(name, ContextId::new(syntax_index, context_index));
                syntax_contexts.push(context);
            }

            let syntax = CompleteSyntaxReference {
                name,
                file_extensions,
                scope,
                first_line_match,
                hidden,
                variables,
                contexts_map: map,
                contexts: syntax_contexts,
            };
            syntaxes.push(syntax);
        }

        // TODO: Sort out borrow checker complaints instead of cloning
        let syntaxes_clone = syntaxes.clone();
        let mut found_more_backref_includes = true;
        for syntax in &mut syntaxes {
            let mut no_prototype = HashSet::new();
            let prototype = syntax.contexts_map.get("prototype");
            if let Some(prototype_id) = prototype {
                // TODO: We could do this after parsing YAML, instead of here?
                Self::recursively_mark_no_prototype(syntax, prototype_id.context_index(), &syntax.contexts, &mut no_prototype);
            }

            // TODO: Sort out borrow checker complaints instead of cloning
            let syntax_clone = syntax.clone();
            for context_id in syntax.contexts_map.values() {
                let index = context_id.context_index();
                let mut context = &mut syntax.contexts[index];
                if let Some(prototype_id) = prototype {
                    if context.meta_include_prototype && !no_prototype.contains(&index) {
                        context.prototype = Some(*prototype_id);
                    }
                }
                Self::link_context(&mut context, &syntax_clone, &syntaxes_clone);
                
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

            for syntax_index in 0..syntaxes.len() {
                let mut use_backrefs_indices = vec![];
                {
                    let syntax = &syntaxes[syntax_index];
                    // find any contexts which include a context which uses backrefs
                    // and mark those as using backrefs - to support nested includes
                    for context_index in 0..syntax.contexts.len() {
                        let context = &syntax.contexts[context_index];
                        if !context.uses_backrefs && context.patterns.clone().iter().any(|pattern| {
                            matches!(pattern, Pattern::Include(ContextReference::Direct(id)) if syntaxes[id.syntax_index()].contexts[id.context_index()].uses_backrefs)
                        }) {
                            use_backrefs_indices.push(context_index);
                        }
                    }
                }

                let syntax = &mut syntaxes[syntax_index];
                for context_index in use_backrefs_indices {
                    let mut context = &mut syntax.contexts[context_index];
                    context.uses_backrefs = true;
                    // look for contexts including this context
                    found_more_backref_includes = true;
                }
            }
        }

        #[cfg(feature = "metadata")]
        let metadata = match existing_metadata {
            Some(existing) => existing.merged_with_raw(raw_metadata),
            None => raw_metadata.into(),
        };

        // Final step: Separate the lazy-loaded part from the not lazy-loaded part
        let lazy_loaded_syntaxes = syntaxes.into_iter().map(|s|{
            let CompleteSyntaxReference {
                name,
                file_extensions,
                scope,
                first_line_match,
                hidden,
                variables,
                contexts_map,
                contexts,
            } = s;

            let lazy_loaded = LazyLoaded {
                variables,
                contexts_map,
                contexts,
            };

            SyntaxReference {
                name,
                file_extensions,
                scope,
                first_line_match,
                hidden,
                lazy_loaded: AtomicLazyCell::new(),
                serialized_lazy_loaded: crate::dumps::dump_binary(&lazy_loaded),
            }
        });

        SyntaxSet {
            syntaxes: lazy_loaded_syntaxes.collect(),
            path_syntaxes,
            first_line_cache: AtomicLazyCell::new(),
            #[cfg(feature = "metadata")]
            metadata,
        }
    }

    /// Anything recursively included by the prototype shouldn't include the prototype.
    /// This marks them as such.
    fn recursively_mark_no_prototype(
        syntax: &CompleteSyntaxReference,
        context_id: usize,
        contexts: &[Context],
        no_prototype: &mut HashSet<usize>,
    ) {
        let first_time = no_prototype.insert(context_id);
        if !first_time {
            return;
        }

        for pattern in &contexts[context_id].patterns {
            match *pattern {
                // Apparently inline blocks also don't include the prototype when within the prototype.
                // This is really weird, but necessary to run the YAML syntax.
                Pattern::Match(ref match_pat) => {
                    let maybe_context_refs = match match_pat.operation {
                        MatchOperation::Push(ref context_refs) |
                        MatchOperation::Set(ref context_refs) => Some(context_refs),
                        MatchOperation::Pop | MatchOperation::None => None,
                    };
                    if let Some(context_refs) = maybe_context_refs {
                        for context_ref in context_refs.iter() {
                            match context_ref {
                                ContextReference::Inline(ref s) | ContextReference::Named(ref s) => {
                                    if let Some(i) = syntax.contexts_map.get(s) {
                                        Self::recursively_mark_no_prototype(syntax, i.context_index(), contexts, no_prototype);
                                    }
                                },
                                ContextReference::Direct(ref id) => {
                                    Self::recursively_mark_no_prototype(syntax, id.context_index(), contexts, no_prototype);
                                },
                                _ => (),
                            }
                        }
                    }
                }
                Pattern::Include(ref reference) => {
                    match reference {
                        ContextReference::Named(ref s) => {
                            if let Some(id) = syntax.contexts_map.get(s) {
                                Self::recursively_mark_no_prototype(syntax, id.context_index(), contexts, no_prototype);
                            }
                        },
                        ContextReference::Direct(ref id) => {
                            Self::recursively_mark_no_prototype(syntax, id.context_index(), contexts, no_prototype);
                        },
                        _ => (),
                    }
                }
            }
        }
    }

    fn link_context(context: &mut Context, syntax: &CompleteSyntaxReference, syntaxes: &[CompleteSyntaxReference]) {
        for pattern in &mut context.patterns {
            match *pattern {
                Pattern::Match(ref mut match_pat) => Self::link_match_pat(match_pat, syntax, syntaxes),
                Pattern::Include(ref mut context_ref) => Self::link_ref(context_ref, syntax, syntaxes),
            }
        }
    }

    fn link_ref(context_ref: &mut ContextReference, syntax: &CompleteSyntaxReference, syntaxes: &[CompleteSyntaxReference]) {
        // println!("{:?}", context_ref);
        use super::syntax_definition::ContextReference::*;
        let linked_context_id = match *context_ref {
            Named(ref s) | Inline(ref s) => {
                // This isn't actually correct, but it is better than nothing/crashing.
                // This is being phased out anyhow, see https://github.com/sublimehq/Packages/issues/73
                // Fixes issue #30
                if s == "$top_level_main" {
                    syntax.contexts_map.get("main")
                } else {
                    syntax.contexts_map.get(s)
                }
            }
            ByScope { scope, ref sub_context } => {
                let context_name = sub_context.as_ref().map_or("main", |x| &**x);
                syntaxes
                    .iter()
                    .rev()
                    .find(|s| s.scope == scope)
                    .and_then(|s| s.contexts_map.get(context_name))
            }
            File { ref name, ref sub_context } => {
                let context_name = sub_context.as_ref().map_or("main", |x| &**x);
                syntaxes
                    .iter()
                    .rev()
                    .find(|s| &s.name == name)
                    .and_then(|s| s.contexts_map.get(context_name))
            }
            Direct(_) => None,
        };
        if let Some(context_id) = linked_context_id {
            let mut new_ref = Direct(*context_id);
            mem::swap(context_ref, &mut new_ref);
        }
    }

    fn link_match_pat(match_pat: &mut MatchPattern, syntax: &CompleteSyntaxReference, syntaxes: &[CompleteSyntaxReference]) {
        let maybe_context_refs = match match_pat.operation {
            MatchOperation::Push(ref mut context_refs) |
            MatchOperation::Set(ref mut context_refs) => Some(context_refs),
            MatchOperation::Pop | MatchOperation::None => None,
        };
        if let Some(context_refs) = maybe_context_refs {
            for context_ref in context_refs.iter_mut() {
                Self::link_ref(context_ref, syntax, syntaxes);
            }
        }
        if let Some(ref mut context_ref) = match_pat.with_prototype {
            Self::link_ref(context_ref, syntax, syntaxes);
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
        FirstLineCache {
            regexes,
        }
    }
}


#[cfg(feature = "yaml-load")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsing::{ParseState, Scope, syntax_definition};
    use std::collections::HashMap;

    #[test]
    fn can_load() {
        let mut builder = SyntaxSetBuilder::new();
        builder.add_from_folder("testdata/Packages", false).unwrap();

        let cmake_dummy_syntax = SyntaxDefinition {
            name: "CMake".to_string(),
            file_extensions: vec!["CMakeLists.txt".to_string(), "cmake".to_string()],
            scope: Scope::new("source.cmake").unwrap(),
            first_line_match: None,
            hidden: false,
            variables: HashMap::new(),
            contexts: HashMap::new(),
        };

        builder.add(cmake_dummy_syntax);
        builder.add_plain_text_syntax();

        let ps = builder.build();

        assert_eq!(&ps.find_syntax_by_first_line("#!/usr/bin/env node").unwrap().name,
                   "JavaScript");
        let rails_scope = Scope::new("source.ruby.rails").unwrap();
        let syntax = ps.find_syntax_by_name("Ruby on Rails").unwrap();
        ps.find_syntax_plain_text();
        assert_eq!(&ps.find_syntax_by_extension("rake").unwrap().name, "Ruby");
        assert_eq!(&ps.find_syntax_by_extension("RAKE").unwrap().name, "Ruby");
        assert_eq!(&ps.find_syntax_by_token("ruby").unwrap().name, "Ruby");
        assert_eq!(&ps.find_syntax_by_first_line("lol -*- Mode: C -*- such line").unwrap().name,
                   "C");
        assert_eq!(&ps.find_syntax_for_file("testdata/parser.rs").unwrap().unwrap().name,
                   "Rust");
        assert_eq!(&ps.find_syntax_for_file("testdata/test_first_line.test")
                       .expect("Error finding syntax for file")
                       .expect("No syntax found for file")
                       .name,
                   "Ruby");
        assert_eq!(&ps.find_syntax_for_file(".bashrc").unwrap().unwrap().name,
                   "Bourne Again Shell (bash)");
        assert_eq!(&ps.find_syntax_for_file("CMakeLists.txt").unwrap().unwrap().name,
                   "CMake");
        assert_eq!(&ps.find_syntax_for_file("test.cmake").unwrap().unwrap().name,
                   "CMake");
        assert_eq!(&ps.find_syntax_for_file("Rakefile").unwrap().unwrap().name, "Ruby");
        assert!(&ps.find_syntax_by_first_line("derp derp hi lol").is_none());
        assert_eq!(&ps.find_syntax_by_path("Packages/Rust/Rust.sublime-syntax").unwrap().name,
                   "Rust");
        // println!("{:#?}", syntax);
        assert_eq!(syntax.scope, rails_scope);
        // unreachable!();
        let main_context = ps.get_context(&syntax.contexts_map()["main"]);
        let count = syntax_definition::context_iter(&ps, main_context).count();
        assert_eq!(count, 109);
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
        let ops = parse_state.parse_line("a go_b b", &cloned_syntax_set);
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

        let syntax_c = SyntaxDefinition::load_from_str(r#"
        name: C
        scope: source.c
        file_extensions: [c]
        contexts:
          main:
            - match: 'c'
              scope: c
            - match: 'go_a'
              push: scope:source.a#main
        "#, true, None).unwrap();

        builder.add(syntax_c);

        let syntax_set = builder.build();

        let syntax = syntax_set.find_syntax_by_extension("c").unwrap();
        let mut parse_state = ParseState::new(syntax);
        let ops = parse_state.parse_line("c go_a a go_b b", &syntax_set);
        let expected = (14, ScopeStackOp::Push(Scope::new("b").unwrap()));
        assert_ops_contain(&ops, &expected);
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

        let unlinked_contexts : Vec<String> = syntax_set.find_unlinked_contexts().into_iter().collect();
        assert_eq!(unlinked_contexts.len(), 1);
        assert_eq!(unlinked_contexts[0], "Syntax 'A' with scope 'source.a' has unresolved context reference ByScope { scope: <source.b>, sub_context: Some(\"main\") }");
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

        let lines = vec![
            "a a a",
            "a go_b b",
            "go_b b",
            "go_b b  b",
        ];

        let results: Vec<Vec<(usize, ScopeStackOp)>> = lines
            .par_iter()
            .map(|line| {
                let syntax = syntax_set.find_syntax_by_extension("a").unwrap();
                let mut parse_state = ParseState::new(syntax);
                parse_state.parse_line(line, &syntax_set)
            })
            .collect();

        assert_ops_contain(&results[0], &(4, ScopeStackOp::Push(Scope::new("a").unwrap())));
        assert_ops_contain(&results[1], &(7, ScopeStackOp::Push(Scope::new("b").unwrap())));
        assert_ops_contain(&results[2], &(5, ScopeStackOp::Push(Scope::new("b").unwrap())));
        assert_ops_contain(&results[3], &(8, ScopeStackOp::Push(Scope::new("b").unwrap())));
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

            let syntax_a2 = SyntaxDefinition::load_from_str(r#"
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
                "#, true, None).unwrap();

            builder.add(syntax_a2);

            let syntax_c = SyntaxDefinition::load_from_str(r#"
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
                "#, true, None).unwrap();

            builder.add(syntax_c);

            builder.build()
        };

        let mut syntax = syntax_set.find_syntax_by_extension("a").unwrap();
        assert_eq!(syntax.name, "A improved");
        syntax = syntax_set.find_syntax_by_scope(Scope::new("source.a").unwrap()).unwrap();
        assert_eq!(syntax.name, "A improved");
        syntax = syntax_set.find_syntax_by_first_line("syntax a").unwrap();
        assert_eq!(syntax.name, "C");

        let mut parse_state = ParseState::new(syntax);
        let ops = parse_state.parse_line("c go_a a", &syntax_set);
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
        let ops = parse_state.parse_line("# test\n", &syntax_set);
        let expected = (0, ScopeStackOp::Push(Scope::new("comment.line.number-sign.yaml").unwrap()));
        assert_ops_contain(&ops, &expected);
    }

    #[test]
    fn no_prototype_for_contexts_included_from_prototype() {
        let mut builder = SyntaxSetBuilder::new();
        let syntax = SyntaxDefinition::load_from_str(r#"
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
                "#, true, None).unwrap();
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
    fn no_prototype_for_contexts_inline_in_prototype() {
        let mut builder = SyntaxSetBuilder::new();
        let syntax = SyntaxDefinition::load_from_str(r#"
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
                "#, true, None).unwrap();
        builder.add(syntax);
        let ss = builder.build();

        assert_prototype_only_on(&["main"], &ss, &ss.syntaxes()[0]);

        let rebuilt = ss.into_builder().build();
        assert_prototype_only_on(&["main"], &rebuilt, &rebuilt.syntaxes()[0]);
    }

    fn assert_ops_contain(
        ops: &[(usize, ScopeStackOp)],
        expected: &(usize, ScopeStackOp)
    ) {
        assert!(ops.contains(expected),
                "expected operations to contain {:?}: {:?}", expected, ops);
    }

    fn assert_prototype_only_on(expected: &[&str], syntax_set: &SyntaxSet, syntax: &SyntaxReference) {
        for (name, id) in syntax.contexts_map() {
            if name == "__main" || name == "__start" {
                // Skip special contexts
                continue;
            }
            let context = syntax_set.get_context(id);
            if expected.contains(&name.as_str()) {
                assert!(context.prototype.is_some(), "Expected context {} to have prototype", name);
            } else {
                assert!(context.prototype.is_none(), "Expected context {} to not have prototype", name);
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
        ).unwrap()
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
        ).unwrap()
    }
}
