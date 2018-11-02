use std::collections::BTreeMap;
use std::path::Path;
use std::fs::File;
use std::io::BufReader;
use std::str::FromStr;

use lazycell::AtomicLazyCell;
use onig::{Regex, SearchOptions};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json;

use super::scope::{MatchPower, Scope};
use super::super::LoadingError;
use super::super::highlighting::settings::*;
use super::super::highlighting::ScopeSelectors;

type Dict = serde_json::Map<String, Settings>;

/// A String representation of a `ScopeSelectors` instance.
type SelectorString = String;

/// A simple regex pattern, used for checking indentation state.
#[derive(Debug)]
pub struct Pattern {
    regex_str: String,
    regex: AtomicLazyCell<Regex>,
}

/// A collection of all loaded metadata.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Metadata {
    pub scoped_metadata: Vec<MetadataSet>,
}

/// Metadata for a particular `ScopeSelector`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataSet {
    /// The raw string representation of this selector. We keep this around
    /// for serialization; it's easier than trying to rebuild it from the
    /// parsed `ScopeSelectors`.
    selector_string: SelectorString,
    /// The scope selector to which this metadata applies
    selector: ScopeSelectors,
    /// The actual metadata.
    pub items: MetadataItems,
}

/// Items loaded from `.tmPreferences` metadata files, for a particular scope.
/// For more information, see [Metadata Files](http://docs.sublimetext.info/en/latest/reference/metadata.html)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MetadataItems {
    pub increase_indent_pattern: Option<Pattern>,
    pub decrease_indent_pattern: Option<Pattern>,
    pub bracket_indent_next_line_pattern: Option<Pattern>,
    pub disable_indent_next_line_pattern: Option<Pattern>,
    pub unindented_line_pattern: Option<Pattern>,
    pub indent_parens: Option<bool>,
}

/// A type that can be deserialized from a `.tmPreferences` file.
/// Since multiple files can refer to the same scope, we merge them while loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RawMetadataEntry {
    scope: SelectorString,
    settings: BTreeMap<String, Settings>,
}

/// Convenience type for loading heterogeneous metadata.
#[derive(Debug, Default, Clone)]
pub(crate) struct LoadMetadata {
    loaded: BTreeMap<String, Dict>,
}

// all of these are optional, but we don't want to deserialize if
// we don't have at least _one_ of them present
const KEYS_WE_USE: &[&str] = &[
    "increaseIndentPattern",
    "decreaseIndentPattern",
    "bracketIndentNextLinePattern",
    "disableIndentNextLinePattern",
    "unIndentedLinePattern",
    "indentParens",
];

impl LoadMetadata {
    /// Adds the provided `RawMetadataEntry`, merging it with any other
    /// loaded metadata with which it shares a scope. If duplicate keys exist,
    /// last writer wins.
    //TODO: verify that this is A) reasonable and B) deterministic
    pub fn add_raw(&mut self, raw: RawMetadataEntry) {
        let RawMetadataEntry { scope, settings } = raw;
        let scoped_settings = self.loaded.entry(scope.clone())
            .or_insert_with(|| Dict::new());

        for (key, value) in settings {
            if !KEYS_WE_USE.contains(&key.as_str()) {
                continue;
            }
            //NOTE: because we can't guarantee the order that files get loaded,
            // we do a bit of a dance to try and ensure this process is deterministic.
            // TLDR: higher numerical values are prefered to lower, and strings are chosen
            // by length, then by lexigraphic order.
            let should_insert = match scoped_settings.get(&key) {
                Some(existing) if existing == &value => false,
                Some(existing) => {
                    let result = match (existing, &value) {
                        (Settings::Number(old), Settings::Number(new)) => {
                            if let (Some(old), Some(new)) = (old.as_u64(), new.as_u64()) {
                                new > old
                            } else if let (Some(old), Some(new)) = (old.as_f64(), new.as_f64()) {
                                new > old
                            } else {
                                //eprintln!("unexpected duplicate values for metadata key: \
                                    //{} in scope {}: {:?}, {:?}", &key, scope, old, new);
                                false
                            }

                        }
                        (Settings::String(old), Settings::String(new)) => {
                            if new.len() != old.len() {
                                new.len() > old.len()
                            } else {
                                new > old
                            }
                        }
                        (Settings::Array(old), Settings::Array(new)) => new.len() > old.len(),
                        (_old, _new) => {
                            //eprintln!("unexpected duplicate values for metadata key: \
                        //{} in scope {}: {:?}, {:?}", &key, scope, old, new);
                            false
                        }
                    };
                    //let madlib = if result { "" } else { "not " };
                    //eprintln!("will {}replace duplicate metadata for {} in {}\n\
                              //old {:?}\nnew {:?}", madlib, key, scope, &existing, &value);
                    result
                }
                None => true,
            };
            if should_insert {
                scoped_settings.insert(key, value);
            }
        }
    }

    fn from_raw(tuple: (SelectorString, Dict)) -> Result<MetadataSet, String> {
        let (selector_string, settings) = tuple;

       if !KEYS_WE_USE.iter().any(|key| settings.contains_key(*key)) {
           return Err(format!("no interesting metadata for {}", &selector_string));
       }

        let items: MetadataItems = serde_json::from_value(settings.into())
            .map_err(|e| e.to_string())?;
        let selector = ScopeSelectors::from_str(&selector_string)
            .map_err(|e| format!("{:?}", e))?;
        Ok(MetadataSet { selector_string, selector, items })
    }

    /// Generates a `MetadataSet` from a single file
    #[cfg(test)]
    pub fn quick_load(path: &str) -> Result<MetadataSet, LoadingError> {
        let mut loaded = Self::default();
        let raw = RawMetadataEntry::load(path)?;
        loaded.add_raw(raw);
        let mut metadata: Metadata = loaded.into();
        Ok(metadata.scoped_metadata.pop().unwrap())
    }
}

impl From<LoadMetadata> for Metadata {
    fn from(src: LoadMetadata) -> Metadata {
        let scoped_metadata = src.loaded.into_iter()
            .flat_map(LoadMetadata::from_raw)
            .collect();
        Metadata { scoped_metadata }
    }
}

impl Metadata {
    fn metadata_matching_scope(&self, scope_path: &[Scope]) -> Vec<(MatchPower, &MetadataSet)> {
        let mut metadata_matches = self.scoped_metadata
            .iter()
            .filter_map(|meta_set| {
                meta_set.selector.does_match(scope_path)
                    .map(|score| (score, meta_set))
            }).collect::<Vec<_>>();

        metadata_matches.sort_by(|one, two| two.0.cmp(&one.0));
        metadata_matches
    }

    pub fn metadata_for_scope(&self, scope: &[Scope]) -> ScopedMetadata {
        ScopedMetadata(self.metadata_matching_scope(scope))
    }
}

/// A cleaner interface for the pattern of finding the first match in a stack.
#[derive(Debug, Clone)]
pub struct ScopedMetadata<'a>(Vec<(MatchPower, &'a MetadataSet)>);

impl<'a> ScopedMetadata<'a> {

    pub fn unindented_line(&self, line: &str) -> bool {
        self.best_match(|ind| ind.unindented_line_pattern.as_ref().map(|p| p.is_match(line)))
            .unwrap_or(false)
    }

    pub fn decrease_indent(&self, line: &str) -> bool {
        self.best_match(|ind| ind.decrease_indent_pattern.as_ref().map(|p| p.is_match(line)))
            .unwrap_or(false)
    }

    pub fn increase_indent(&self, line: &str) -> bool {
        self.best_match(|ind| ind.increase_indent_pattern.as_ref().map(|p| p.is_match(line)))
            .unwrap_or(false)
    }

    pub fn bracket_increase(&self, line: &str) -> bool {
        self.best_match(|ind| ind.bracket_indent_next_line_pattern.as_ref().map(|p| p.is_match(line)))
            .unwrap_or(false)
    }

    pub fn disable_indent_next_line(&self, line: &str) -> bool {
        self.best_match(|ind| ind.disable_indent_next_line_pattern.as_ref().map(|p| p.is_match(line)))
            .unwrap_or(false)
    }

    fn best_match<T, F>(&self, f: F) -> Option<T>
        where F: FnMut(&MetadataItems) -> Option<T>
    {
        self.0.iter()
            .map(|(_, meta_set)| &meta_set.items)
            .flat_map(f)
            .next()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}


impl Pattern {
    pub fn is_match<S: AsRef<str>>(&self, string: S) -> bool {
        self.regex()
            .match_with_options(
                string.as_ref(),
                0,
                SearchOptions::SEARCH_OPTION_NONE,
                None)
            .is_some()
    }

    pub fn regex(&self) -> &Regex {
        if let Some(regex) = self.regex.borrow() {
            regex
        } else {
            let regex = Regex::new(&self.regex_str)
                .expect("regex string should be pre-tested");
            self.regex.fill(regex).ok();
            self.regex.borrow().unwrap()
        }
    }
}

impl Clone for Pattern {
    fn clone(&self) -> Self {
        Pattern { regex_str: self.regex_str.clone(), regex: AtomicLazyCell::new() }
    }
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Pattern) -> bool {
        self.regex_str == other.regex_str
    }
}

impl Eq for Pattern {}

impl Serialize for Pattern {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: Serializer {
        serializer.serialize_str(&self.regex_str)
    }
}

impl<'de> Deserialize<'de> for Pattern {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error> where D: Deserializer<'de> {
        let regex_str = String::deserialize(deserializer)?;
        Ok(Pattern { regex_str, regex: AtomicLazyCell::new() })
    }
}


impl RawMetadataEntry {
    pub fn load<P: AsRef<Path>>(file: P) -> Result<Self, LoadingError> {
        let file = File::open(file)?;
        let file = BufReader::new(file);
        let contents = read_plist(file)?;
        Ok(serde_json::from_value(contents)?)
    }
}

#[derive(Serialize, Deserialize)]
struct MetaSetSerializable {
    selector_string: String,
    items: Option<MetadataItems>,
}

impl Serialize for MetadataSet {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: Serializer {

        let MetadataSet { selector_string, items, .. } = self.clone();
        let inner = MetaSetSerializable { selector_string, items: Some(items) };
        inner.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for MetadataSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error> where D: Deserializer<'de> {
        use serde::de::Error;
        let inner = MetaSetSerializable::deserialize(deserializer)?;
        let MetaSetSerializable { selector_string, items } = inner;
        let selector = ScopeSelectors::from_str(&selector_string)
            .map_err(|e| Error::custom(format!("{:?}", e)))?;
        let items = items.ok_or_else(|| Error::custom(format!("no metadata items")))?;
        Ok(MetadataSet { selector_string, selector, items })
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use parsing::SyntaxSet;
    #[test]
    fn load_raw() {
        let comments_file: &str = "testdata/Packages/Go/Comments.tmPreferences";
        assert!(Path::new(comments_file).exists());

        let r = RawMetadataEntry::load(comments_file);
        assert!(r.is_ok());

        let indent_file: &str = "testdata/Packages/Go/Indentation Rules.tmPreferences";
        assert!(Path::new(indent_file).exists());

        let r = RawMetadataEntry::load(indent_file).unwrap();
        assert_eq!(r.scope, "source.go");

        let indent_file: &str = "testdata/Packages/Rust/RustIndent.tmPreferences";
        assert!(Path::new(indent_file).exists());

        let r = RawMetadataEntry::load(indent_file).unwrap();
        assert_eq!(r.scope, "source.rust");
    }

    #[test]
    fn load_groups() {
        let mut loaded = LoadMetadata::default();
        let indent_file: &str = "testdata/Packages/Rust/RustIndent.tmPreferences";
        let raw = RawMetadataEntry::load(indent_file).expect("failed to load indent metadata");
        loaded.add_raw(raw);
        let comment_file: &str = "testdata/Packages/Rust/RustComment.tmPreferences";
        let raw = RawMetadataEntry::load(comment_file).expect("failed to load comment metadata");
        loaded.add_raw(raw);

        // both these have the same scope, and so should end up in one file
        assert_eq!(loaded.loaded.len(), 1);

        let rust_meta_raw = loaded.loaded.get("source.rust").unwrap().clone();
        //eprintln!("{}", rust_meta_raw.to_string_pretty());
        let rust_meta = LoadMetadata::from_raw(("source.rust".into(), rust_meta_raw))
            .unwrap();
        //assert!(rust_meta.indentation.is_some());
        assert!(rust_meta.items.increase_indent_pattern.is_some());

        let metadata: Metadata = loaded.into();
        assert_eq!(metadata.scoped_metadata.len(), 1);
        let rust_meta = metadata.scoped_metadata.first().unwrap();
        assert!(rust_meta.items.increase_indent_pattern.is_some());
    }

    #[test]
    fn parse_yaml_meta() {
        let path = "testdata/Packages/YAML/Indentation Rules.tmPreferences";
        let metaset = LoadMetadata::quick_load(path).unwrap();
        assert!(metaset.items.increase_indent_pattern.is_some());
        assert!(metaset.items.decrease_indent_pattern.is_some());
        assert!(metaset.items.bracket_indent_next_line_pattern.is_none());
    }

    #[test]
    fn serde_pattern() {
        let pattern: Pattern = serde_json::from_str("\"just a string\"").unwrap();
        assert_eq!(pattern.regex_str, "just a string");
        let back_to_str = serde_json::to_string(&pattern).unwrap();
        assert_eq!(back_to_str, "\"just a string\"");
    }

    #[test]
    fn indent_rust() {
        let ps = SyntaxSet::load_from_folder("testdata/Packages/Rust").unwrap();

        let rust_scopes = [Scope::new("source.rust").unwrap()];
        let indent_ctx = ScopedMetadata(
            ps.metadata.metadata_matching_scope(&rust_scopes));

        assert_eq!(indent_ctx.0.len(), 1, "failed to load rust metadata");
        assert_eq!(indent_ctx.increase_indent("struct This {"), true);
        assert_eq!(indent_ctx.increase_indent("struct This }"), false);
        assert_eq!(indent_ctx.decrease_indent("     }"), true);
        assert_eq!(indent_ctx.decrease_indent("struct This {"), false);
        assert_eq!(indent_ctx.decrease_indent("struct This {}"), false);
        assert_eq!(indent_ctx.increase_indent("struct This {}"), false);

    }
}
