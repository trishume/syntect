use serde::de::{Deserialize, Deserializer};
use serde::ser::{Serialize, Serializer};
use std::error::Error;
use std::sync::OnceLock;

/// An abstraction for regex patterns.
///
/// * Allows swapping out the regex implementation because it's only in this module.
/// * Makes regexes serializable and deserializable using just the pattern string.
/// * Lazily compiles regexes on first use to improve initialization time.
#[derive(Debug)]
pub struct Regex {
    regex_str: String,
    regex: OnceLock<regex_impl::Regex>,
}

/// A region contains text positions for capture groups in a match result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Region {
    region: regex_impl::Region,
}

impl Regex {
    /// Create a new regex from the pattern string.
    ///
    /// Note that the regex compilation happens on first use, which is why this method does not
    /// return a result.
    pub fn new(regex_str: String) -> Self {
        Self {
            regex_str,
            regex: OnceLock::new(),
        }
    }

    /// Check whether the pattern compiles as a valid regex or not.
    pub fn try_compile(regex_str: &str) -> Option<Box<dyn Error + Send + Sync + 'static>> {
        regex_impl::Regex::new(&strip_redundant_empty_alternatives(regex_str)).err()
    }

    /// Return the regex pattern.
    pub fn regex_str(&self) -> &str {
        &self.regex_str
    }

    /// Check if the regex matches the given text.
    pub fn is_match(&self, text: &str) -> bool {
        self.regex().is_match(text)
    }

    /// Search for the pattern in the given text from begin/end positions.
    ///
    /// If a region is passed, it is used for storing match group positions. The argument allows
    /// the [`Region`] to be reused between searches, which makes a significant performance
    /// difference.
    ///
    /// [`Region`]: struct.Region.html
    pub fn search(
        &self,
        text: &str,
        begin: usize,
        end: usize,
        region: Option<&mut Region>,
    ) -> bool {
        self.regex()
            .search(text, begin, end, region.map(|r| &mut r.region))
    }

    fn regex(&self) -> &regex_impl::Regex {
        self.regex.get_or_init(|| {
            regex_impl::Regex::new(&strip_redundant_empty_alternatives(&self.regex_str))
                .expect("regex string should be pre-tested")
        })
    }
}

/// Collapse runs of two or more consecutive unescaped `|` into a single `|`,
/// but only at the top level of the pattern (outside any `(...)` group and
/// outside any `[...]` character class).
///
/// A top-level empty alternative (e.g. `x||y`) matches the empty string at
/// any position. Under leftmost-first semantics, an empty alternative in the
/// middle of a top-level alternation always matches before later alternatives
/// get a chance, making them dead code — and in a search context it makes
/// the whole regex "match" anywhere with zero width, which masks real matches.
/// This pattern is almost always a typo — e.g. Cabal's `\|\||&&||!`,
/// where the author meant `\|\||&&|!`.
///
/// Inside a group, empty alternatives are a legitimate idiom for an optional
/// alternation (e.g. `(a|b|)c` matches `c` as well as `ac`/`bc`), so we leave
/// those untouched — matching Sublime's behavior.
///
/// UTF-8 is safe to scan by bytes because `|`, `\\`, `[`, `]`, `(`, and `)`
/// are single-byte ASCII and never appear in multi-byte sequences.
fn strip_redundant_empty_alternatives(pattern: &str) -> String {
    let bytes = pattern.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_class = false;
    let mut group_depth: usize = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\\' => {
                out.push(b as char);
                if i + 1 < bytes.len() {
                    let next = bytes[i + 1];
                    if next < 0x80 {
                        out.push(next as char);
                        i += 2;
                    } else {
                        // Multi-byte codepoint after the backslash: copy the
                        // whole UTF-8 sequence so we don't split it.
                        let ch_end = i + 1 + utf8_char_len(next);
                        out.push_str(&pattern[i + 1..ch_end]);
                        i = ch_end;
                    }
                } else {
                    i += 1;
                }
            }
            b'[' if !in_class => {
                out.push(b as char);
                in_class = true;
                i += 1;
            }
            b']' if in_class => {
                out.push(b as char);
                in_class = false;
                i += 1;
            }
            b'(' if !in_class => {
                out.push(b as char);
                group_depth += 1;
                i += 1;
            }
            b')' if !in_class => {
                out.push(b as char);
                group_depth = group_depth.saturating_sub(1);
                i += 1;
            }
            b'|' if !in_class && group_depth == 0 => {
                out.push(b as char);
                i += 1;
                while i < bytes.len() && bytes[i] == b'|' {
                    i += 1;
                }
            }
            _ if b < 0x80 => {
                out.push(b as char);
                i += 1;
            }
            _ => {
                let ch_end = i + utf8_char_len(b);
                out.push_str(&pattern[i..ch_end]);
                i = ch_end;
            }
        }
    }
    out
}

fn utf8_char_len(first_byte: u8) -> usize {
    if first_byte < 0x80 {
        1
    } else if first_byte < 0xC0 {
        1 // continuation byte — shouldn't be a leading byte; copy 1 to make progress
    } else if first_byte < 0xE0 {
        2
    } else if first_byte < 0xF0 {
        3
    } else {
        4
    }
}

impl Clone for Regex {
    fn clone(&self) -> Self {
        Regex {
            regex_str: self.regex_str.clone(),
            regex: OnceLock::new(),
        }
    }
}

impl PartialEq for Regex {
    fn eq(&self, other: &Regex) -> bool {
        self.regex_str == other.regex_str
    }
}

impl Eq for Regex {}

impl Serialize for Regex {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.regex_str)
    }
}

impl<'de> Deserialize<'de> for Regex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let regex_str = String::deserialize(deserializer)?;
        Ok(Regex::new(regex_str))
    }
}

impl Region {
    pub fn new() -> Self {
        Self {
            region: regex_impl::new_region(),
        }
    }

    /// Get the start/end positions of the capture group with given index.
    ///
    /// If there is no match for that group or the index does not correspond to a group, `None` is
    /// returned. The index 0 returns the whole match.
    pub fn pos(&self, index: usize) -> Option<(usize, usize)> {
        self.region.pos(index)
    }
}

impl Default for Region {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "regex-onig")]
mod regex_impl {
    pub use onig::Region;
    use onig::{MatchParam, RegexOptions, SearchOptions, Syntax};
    use std::error::Error;

    #[derive(Debug)]
    pub struct Regex {
        regex: onig::Regex,
    }

    pub fn new_region() -> Region {
        Region::with_capacity(8)
    }

    impl Regex {
        pub fn new(regex_str: &str) -> Result<Regex, Box<dyn Error + Send + Sync + 'static>> {
            let result = onig::Regex::with_options(
                regex_str,
                RegexOptions::REGEX_OPTION_CAPTURE_GROUP,
                Syntax::default(),
            );
            match result {
                Ok(regex) => Ok(Regex { regex }),
                Err(error) => Err(Box::new(error)),
            }
        }

        pub fn is_match(&self, text: &str) -> bool {
            self.regex
                .match_with_options(text, 0, SearchOptions::SEARCH_OPTION_NONE, None)
                .is_some()
        }

        pub fn search(
            &self,
            text: &str,
            begin: usize,
            end: usize,
            region: Option<&mut Region>,
        ) -> bool {
            let matched = self.regex.search_with_param(
                text,
                begin,
                end,
                SearchOptions::SEARCH_OPTION_NONE,
                region,
                MatchParam::default(),
            );

            // If there's an error during search, treat it as non-matching.
            // For example, in case of catastrophic backtracking, onig should
            // fail with a "retry-limit-in-match over" error eventually.
            matches!(matched, Ok(Some(_)))
        }
    }
}

// If both regex-fancy and regex-onig are requested, this condition makes regex-onig win.
#[cfg(all(feature = "regex-fancy", not(feature = "regex-onig")))]
mod regex_impl {
    use std::error::Error;

    #[derive(Debug)]
    pub struct Regex {
        regex: fancy_regex::Regex,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct Region {
        positions: Vec<Option<(usize, usize)>>,
    }

    pub fn new_region() -> Region {
        Region {
            positions: Vec::with_capacity(8),
        }
    }

    impl Regex {
        pub fn new(regex_str: &str) -> Result<Regex, Box<dyn Error + Send + Sync + 'static>> {
            let result = fancy_regex::RegexBuilder::new(regex_str)
                .oniguruma_mode(true)
                .build();
            match result {
                Ok(regex) => Ok(Regex { regex }),
                Err(error) => Err(Box::new(error)),
            }
        }

        pub fn is_match(&self, text: &str) -> bool {
            // Errors are treated as non-matches
            self.regex.is_match(text).unwrap_or(false)
        }

        pub fn search(
            &self,
            text: &str,
            begin: usize,
            end: usize,
            region: Option<&mut Region>,
        ) -> bool {
            // If there's an error during search, treat it as non-matching.
            // For example, in case of catastrophic backtracking, fancy-regex should
            // fail with an error eventually.
            if let Ok(Some(captures)) = self.regex.captures_from_pos(&text[..end], begin) {
                if let Some(region) = region {
                    region.init_from_captures(&captures);
                }
                true
            } else {
                false
            }
        }
    }

    impl Region {
        fn init_from_captures(&mut self, captures: &fancy_regex::Captures) {
            self.positions.clear();
            for i in 0..captures.len() {
                let pos = captures.get(i).map(|m| (m.start(), m.end()));
                self.positions.push(pos);
            }
        }

        pub fn pos(&self, i: usize) -> Option<(usize, usize)> {
            if i < self.positions.len() {
                self.positions[i]
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_compiled_regex() {
        let regex = Regex::new(String::from(r"\w+"));

        assert!(regex.regex.get().is_none());
        assert!(regex.is_match("test"));
        assert!(regex.regex.get().is_some());
    }

    #[test]
    fn serde_as_string() {
        let pattern: Regex = serde_json::from_str("\"just a string\"").unwrap();
        assert_eq!(pattern.regex_str(), "just a string");
        let back_to_str = serde_json::to_string(&pattern).unwrap();
        assert_eq!(back_to_str, "\"just a string\"");
    }

    #[test]
    fn strips_top_level_empty_alternatives() {
        // Cabal's typo: `\|\||&&||!` — empty alt between `&&` and `!` makes
        // the whole regex match zero-width. After stripping, `!` matches.
        assert_eq!(
            strip_redundant_empty_alternatives(r"\|\||&&||!"),
            r"\|\||&&|!"
        );
        assert_eq!(strip_redundant_empty_alternatives("a|||b"), "a|b");
        assert_eq!(strip_redundant_empty_alternatives("a||"), "a|");
        assert_eq!(strip_redundant_empty_alternatives("||b"), "|b");
    }

    #[test]
    fn preserves_empty_alternatives_inside_groups() {
        // D's assignment operators: `(...|>>>||\*|...)=` — empty alt inside
        // the group makes the group optional, which is intentional.
        assert_eq!(
            strip_redundant_empty_alternatives(r"(a|b||c)="),
            r"(a|b||c)="
        );
        assert_eq!(strip_redundant_empty_alternatives("(a||)"), "(a||)");
    }

    #[test]
    fn preserves_pipes_in_character_classes_and_escapes() {
        assert_eq!(strip_redundant_empty_alternatives(r"[a||b]"), r"[a||b]");
        assert_eq!(strip_redundant_empty_alternatives(r"\|\|\|"), r"\|\|\|");
    }

    #[test]
    fn empty_alt_regex_matches_bang() {
        // End-to-end: the Cabal pattern should now successfully match `!`.
        let regex = Regex::new(String::from(r"\|\||&&||!"));
        let mut region = Region::new();
        assert!(regex.search("!impl", 0, 5, Some(&mut region)));
        assert_eq!(region.pos(0), Some((0, 1)));
    }
}
