use lazycell::AtomicLazyCell;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::error::Error;

/// An abstraction for regex patterns.
///
/// * Allows swapping out the regex implementation because it's only in this module.
/// * Makes regexes serializable and deserializable using just the pattern string.
/// * Lazily compiles regexes on first use to improve initialization time.
#[derive(Debug)]
pub struct Regex {
    regex_str: String,
    regex: AtomicLazyCell<regex_impl::Regex>,
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
            regex: AtomicLazyCell::new(),
        }
    }

    /// Check whether the pattern compiles as a valid regex or not.
    pub fn try_compile(regex_str: &str) -> Option<Box<dyn Error>> {
        regex_impl::Regex::new(regex_str).err()
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
    pub fn search(&self, text: &str, begin: usize, end: usize) -> Option<Region> {
        self.regex()
            .search(text, begin, end)
            .map(|r| Region::new(r))
    }

    fn regex(&self) -> &regex_impl::Regex {
        if let Some(regex) = self.regex.borrow() {
            regex
        } else {
            let regex =
                regex_impl::Regex::new(&self.regex_str).expect("regex string should be pre-tested");
            self.regex.fill(regex).ok();
            self.regex.borrow().unwrap()
        }
    }
}

impl Clone for Regex {
    fn clone(&self) -> Self {
        Regex {
            regex_str: self.regex_str.clone(),
            regex: AtomicLazyCell::new(),
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
    fn new(region: regex_impl::Region) -> Self {
        Self { region }
    }

    /// Get the start/end positions of the capture group with given index.
    ///
    /// If there is no match for that group or the index does not correspond to a group, `None` is
    /// returned. The index 0 returns the whole match.
    pub fn pos(&self, index: usize) -> Option<(usize, usize)> {
        self.region.pos(index)
    }
}

#[cfg(feature = "onig")]
mod regex_impl {
    pub use onig::Region;
    use onig::{MatchParam, RegexOptions, SearchOptions, Syntax};
    use std::error::Error;

    #[derive(Debug)]
    pub struct Regex {
        regex: onig::Regex,
    }

    impl Regex {
        pub fn new(regex_str: &str) -> Result<Regex, Box<dyn Error>> {
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

        pub fn search(&self, text: &str, begin: usize, end: usize) -> Option<Region> {
            let mut region = Region::new();
            let matched = self.regex.search_with_param(
                text,
                begin,
                end,
                SearchOptions::SEARCH_OPTION_NONE,
                Some(&mut region),
                MatchParam::default(),
            );

            // If there's an error during search, treat it as non-matching.
            // For example, in case of catastrophic backtracking, onig should
            // fail with a "retry-limit-in-match over" error eventually.
            if let Ok(Some(_)) = matched {
                Some(region)
            } else {
                None
            }
        }
    }
}

#[cfg(feature = "fancy-regex")]
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

    impl Regex {
        pub fn new(regex_str: &str) -> Result<Regex, Box<dyn Error>> {
            let result = fancy_regex::Regex::new(regex_str);
            match result {
                Ok(regex) => Ok(Regex { regex }),
                Err(error) => Err(Box::new(error)),
            }
        }

        pub fn is_match(&self, text: &str) -> bool {
            // Errors are treated as non-matches
            self.regex.is_match(text).unwrap_or(false)
        }

        pub fn search(&self, text: &str, begin: usize, end: usize) -> Option<Region> {
            self.regex
                .captures_from_pos(&text[..end], begin)
                // Errors are treated as non-matches
                .unwrap_or(None)
                .map(|captures| Region::from_captures(&captures))
        }
    }

    impl Region {
        fn new() -> Region {
            Region {
                positions: Vec::new(),
            }
        }

        fn from_captures(captures: &fancy_regex::Captures) -> Region {
            let mut region = Region::new();
            for i in 0..captures.len() {
                let pos = captures.get(i).map(|m| (m.start(), m.end()));
                region.positions.push(pos);
            }
            region
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

        assert!(!regex.regex.filled());
        assert!(regex.is_match("test"));
        assert!(regex.regex.filled());
    }

    #[test]
    fn serde_as_string() {
        let pattern: Regex = serde_json::from_str("\"just a string\"").unwrap();
        assert_eq!(pattern.regex_str(), "just a string");
        let back_to_str = serde_json::to_string(&pattern).unwrap();
        assert_eq!(back_to_str, "\"just a string\"");
    }
}
