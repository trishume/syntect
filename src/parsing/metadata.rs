
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::fs::File;
use std::io::BufReader;

use onig::{Regex, SearchOptions};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use serde_json;

use super::scope::Scope;
use super::super::LoadingError;
use super::super::highlighting::settings::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Indentation {
    pub increase_indent_pattern: Pattern,
    #[serde(default)]
    pub decrease_indent_pattern: Option<Pattern>,
    #[serde(default)]
    pub bracket_indent_next_line_pattern: Option<Pattern>,
    #[serde(default)]
    pub disable_indent_next_line_pattern: Option<Pattern>,
    #[serde(default, rename = "unIndentedLinePattern")]
    pub unindented_line_pattern: Option<Pattern>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Pattern {
    regex_str: String,
    regex: RefCell<Option<Regex>>,
}

/// Describes the indentaion state for a line.
#[derive(Default, Debug, Clone, Copy)]
pub struct IndentationState {
    /// The indent level for subsequent lines.
    pub next_indent_level: usize,
    /// If `true`, the next line should be indented one extra level.
    /// This is used for things like indenting chained functions, e.g:
    ///
    /// ```
    /// "hello".chars()
    ///     .collect::<String>();
    /// ```
    pub extra_indent_next_line: bool,
    /// If `true`, the _current_ line (that is, the line that was passed
    /// in to generate this `IndentState`) should have its indent level
    /// increased.
    ///
    /// `next_indent_level` will still be correct in this case.
    pub decrease_current_indent: bool,
}

//#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
//pub struct Comments {
    //shell_variables: serde_json::Map,
//}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Metadata {
    pub indentation: Option<Indentation>,
    //comments: Option<Comments>,
}

//FIXME: using a newtype so I can strip trailing "- something" annotations
//from metadata scope selectors
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Default, Hash)]
struct BareScope(pub Scope);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawMetadataEntry {
    scope: BareScope,
    settings: BTreeMap<String, Settings>,
}

/// Convenience type for loading heterogeneous metadata.
#[derive(Debug, Default)]
pub struct LoadMetadata {
    loaded: BTreeMap<Scope, Settings>,
}

impl LoadMetadata {
    pub fn add_raw(&mut self, raw: RawMetadataEntry) {
        let RawMetadataEntry { scope, settings } = raw;
        let scoped_settings = self.loaded.entry(scope.0)
            .or_insert_with(|| {
                let map: serde_json::Map<String, Settings> = serde_json::Map::new();
                map.into()
            })
            .as_object_mut()
            .unwrap();

            scoped_settings.extend(settings)
    }

    pub fn metadata_for_scope(&mut self, scope: Scope) -> Option<Metadata> {
        let indentation: Option<Indentation> = self.loaded.remove(&scope)
            .and_then(|raw| serde_json::from_value(raw)
                 .map_err(|e| eprintln!("metadata error in {}: {:?}", scope, e))
                 .ok()
             );
        Some(Metadata { indentation })
    }
}

impl Indentation {
    /// Given the state of the previous line, computes the state for the current line.
    pub fn state_for_line<S: AsRef<str>>(
        &self,
        line: S,
        prev_state: IndentationState) -> IndentationState {
        let line = line.as_ref();
        if self.unindented_line_pattern.as_ref()
            .map(|p| p.is_match(line))
            .unwrap_or(false)
        {
            prev_state
        } else if self.decrease_indent_pattern.as_ref()
            .map(|p| p.is_match(line))
            .unwrap_or(false)
        {
            prev_state.new_by_decreasing_level()
        } else if self.increase_indent_pattern.is_match(line) {
            let mut next = prev_state.next_base_state();
            next.next_indent_level += 1;
            next
        } else {
            let indent_next_line = self.bracket_indent_next_line_pattern
            .as_ref()
            .map(|p| p.is_match(line))
            .unwrap_or(false)
            && !self.disable_indent_next_line_pattern.as_ref()
                .map(|p| p.is_match(line))
                .unwrap_or(false);

            let mut next = prev_state.next_base_state();
            next.extra_indent_next_line = indent_next_line;
            next
        }
    }
}

impl IndentationState {
    fn next_base_state(self) -> Self {
        let IndentationState { next_indent_level, .. } = self;
        IndentationState {
            next_indent_level,
            extra_indent_next_line: false,
            decrease_current_indent: false,
        }
    }

    fn new_by_decreasing_level(self) -> IndentationState {
        let mut next = self.next_base_state();
        next.next_indent_level = next.next_indent_level.saturating_sub(1);
        next.decrease_current_indent = true;
        next
    }
}

    ///// **Naively** guesses the current whitespace setting.
    //fn guess_state_from_line<S: AsRef<str>>(line: S) -> Self {
        //let line = line.as_ref();
        //let is_spaces = line.starts_with(' ');
        //let indent_level = if is_spaces {
            //match line.chars()
                //.take_while(|c| *c == ' ')
                //.count()
            //{
                //0 => 0,
                //2 => 2,
                //num  if num % 4 == 0 => num / 4,
                //_ => 0,
            //}
        //} else {
            //line.chars().take_while(|c| *c == '\t').count()
        //};

        //let mut preceding_state = IndentationState::default();
        //preceding_state.next_indent_level = indent_level;
        //preceding_state
    //}
//}

impl Pattern {
    pub fn is_match<S: AsRef<str>>(&self, string: S) -> bool {
        self.compile_if_needed();
        self.regex.borrow_mut()
            .as_ref()
            .unwrap()
            .match_with_options(
                string.as_ref(),
                0,
                SearchOptions::SEARCH_OPTION_NONE,
                None)
            .is_some()
    }

    fn compile_if_needed(&self) {
        if self.regex.borrow().is_some() { return; }
        *self.regex.borrow_mut() = Some(Regex::new(&self.regex_str)
            .expect("regex strings should be pre tested"))
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

impl Serialize for Pattern {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: Serializer {
        serializer.serialize_str(&self.regex_str)
    }
}

impl<'de> Deserialize<'de> for Pattern {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error> where D: Deserializer<'de> {
        let regex_str = String::deserialize(deserializer)?;
        Ok(Pattern { regex_str, regex: RefCell::default() })
    }
}

impl<'de> Deserialize<'de> for BareScope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where D: Deserializer<'de>
    {
        let s = String::deserialize(deserializer)?;
        let scope = Scope::new(&s.split(" -").next().unwrap())
            .map_err(|e| de::Error::custom(format!("Invalid scope: {:?}", e)))?;
        Ok(BareScope(scope))
    }
}

impl Serialize for BareScope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: Serializer {
        self.0.serialize(serializer)
    }
}


impl Clone for Pattern {
    fn clone(&self) -> Self {
        //FIXME: probably we should keep our compiled regex in a shared pointer?
        //I'm not sure how often patterns are going to be passed around.
        Pattern { regex_str: self.regex_str.clone(), regex: RefCell::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsing::SyntaxSet;
    #[test]
    fn try_load() {
        let comments_file: &str = "testdata/Packages/Go/Comments.tmPreferences";
        assert!(Path::new(comments_file).exists());

        let r = RawMetadataEntry::load(comments_file);
        assert!(r.is_ok());

        let indent_file: &str = "testdata/Packages/Go/Indentation Rules.tmPreferences";
        assert!(Path::new(indent_file).exists());

        let r = RawMetadataEntry::load(indent_file).unwrap();
        assert_eq!(r.scope.0, Scope::new("source.go").unwrap());

        let indent_file: &str = "testdata/Packages/Rust/RustIndent.tmPreferences";
        assert!(Path::new(indent_file).exists());

        let r = RawMetadataEntry::load(indent_file).unwrap();
        assert_eq!(r.scope.0, Scope::new("source.rust").unwrap())
    }

    #[test]
    fn load_groups() {
        let mut loaded = LoadMetadata::default();
        let indent_file: &str = "testdata/Packages/Rust/RustIndent.tmPreferences";
        let raw = RawMetadataEntry::load(indent_file).unwrap();
        loaded.add_raw(raw);
        let rust_meta = loaded.metadata_for_scope(Scope::new("source.rust").unwrap()).unwrap();
        let _ = rust_meta.indentation.unwrap();
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
        let syntax = ps.find_syntax_by_extension("rs").unwrap();
        //eprintln!("{:?}", syntax.metadata);
        let indentation = syntax.metadata.as_ref().and_then(|m| m.indentation.as_ref())
            .unwrap();
        let base_state = IndentationState::default();

        let state = indentation.state_for_line("struct This {", base_state);
        assert_eq!(state.next_indent_level, 1);

        let state = indentation.state_for_line("}", state);
        assert_eq!(state.next_indent_level, 0);
        assert!(state.decrease_current_indent);

        assert!(indentation.increase_indent_pattern.is_match("struct This {"));
        assert!(indentation.increase_indent_pattern.is_match("struct That ("));
        assert!(indentation.increase_indent_pattern.is_match("fn my_fun("));

        let state = indentation.state_for_line("fn my_fn(", state);
        assert_eq!(state.next_indent_level, 1);
        let state = indentation.state_for_line("    arg1,", state);
        assert_eq!(state.next_indent_level, 1);
        let state = indentation.state_for_line("    arg2(", state);
        assert_eq!(state.next_indent_level, 2);
        let state = indentation.state_for_line("    arg2_arg", state);
        assert_eq!(state.next_indent_level, 2);
        let state = indentation.state_for_line("        arg2_arg2)", state);
        assert_eq!(state.next_indent_level, 1);
        let state = indentation.state_for_line("    )", state);
        assert_eq!(state.next_indent_level, 0);
        let state = indentation.state_for_line("    )", state);
        assert_eq!(state.next_indent_level, 0);
        let state = indentation.state_for_line("    )", state);
        assert_eq!(state.next_indent_level, 0);
    }

    #[test]
    fn indent_python() {
        let ps = SyntaxSet::load_from_folder("testdata/Packages/Python").unwrap();
        let syntax = ps.find_syntax_by_extension("py").unwrap();
        let indentation = syntax.metadata.as_ref().and_then(|m| m.indentation.as_ref())
            .unwrap();

        assert!(indentation.increase_indent_pattern.is_match("class T:"));
        assert!(indentation.increase_indent_pattern.is_match("def F:"));
        assert!(indentation.increase_indent_pattern.is_match("for x in y:"));
        assert!(!indentation.increase_indent_pattern.is_match("snore x in y:"));
        assert!(!indentation.increase_indent_pattern.is_match("grass T:"));
        assert!(!indentation.increase_indent_pattern.is_match("fed F:"));

        let state = IndentationState::default();
        let state = indentation.state_for_line("if __name__ == '__main__':", state);
        assert_eq!(state.next_indent_level, 1);
        let state = indentation.state_for_line("stream = StreamHandler()", state);
        assert_eq!(state.next_indent_level, 1);
        let state = indentation.state_for_line("try:", state);
        assert_eq!(state.next_indent_level, 2);
        let state = indentation.state_for_line("for t in stream:", state);
        assert_eq!(state.next_indent_level, 3);
        let state = indentation.state_for_line("if not t: ", state);
        assert_eq!(state.next_indent_level, 4);
        let state = indentation.state_for_line("continue", state);
        assert_eq!(state.next_indent_level, 4);
        let state = indentation.state_for_line("else:", state);
        assert_eq!(state.next_indent_level, 3);
        let state = indentation.state_for_line("break", state);
        assert_eq!(state.next_indent_level, 3);
        let state = indentation.state_for_line("except Exception as e:", state);
        assert_eq!(state.next_indent_level, 2);
    }
}
