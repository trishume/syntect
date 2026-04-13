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

/// Collapse empty alternatives that are almost always typos:
///
/// - Runs of consecutive unescaped `|` at the top level of the pattern
///   (outside any group and any `[...]` class). A top-level empty alternative
///   (e.g. `x||y`) matches the empty string at any position; under
///   leftmost-first semantics it always wins zero-width before later
///   alternatives get a chance, making them dead code — e.g. Cabal's typo
///   `\|\||&&||!` where the author meant `\|\||&&|!`.
///
/// - A **leading** empty alternative inside a group — `(|x|y)`, `(?:|x|y)`,
///   `(?x:|x|y)`, `(?P<n>|x|y)`, `(?=|x|y)`, etc. The leading empty alt
///   makes the group match zero-width at any position, masking the real
///   alternatives. Rust's `prelude_types` variable has this shape
///   (`(?x:|Box|Option|...)`) because the author put `|` before each
///   alternative, including the first.
///
/// Middle and trailing empty alternatives **inside a group** are preserved —
/// they are a legitimate optional-alternation idiom (e.g. D's
/// `(...|>>>||\*|...)=` relies on the middle empty alt to match bare `=`).
///
/// UTF-8 is safe to scan by bytes because `|`, `\\`, `[`, `]`, `(`, `)`,
/// `?`, `:`, `=`, `!`, `<`, `>`, and `#` are single-byte ASCII and never
/// appear in multi-byte sequences.
fn strip_redundant_empty_alternatives(pattern: &str) -> String {
    let bytes = pattern.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_class = false;
    // Stack of open-group state. For each currently-open group we track the
    // parse phase, its effective extended-mode flag (inherited from parent +
    // modified by the group's own `?x` / `?-x` prefix), a `prefix_negating`
    // flag to handle `(?ix-m:...)`, and `body_started` — set once we've seen
    // a content character in the body, so we can drop *leading* empty alts
    // while preserving middle/trailing ones.
    let mut groups: Vec<Group> = Vec::new();
    while i < bytes.len() {
        let b = bytes[i];

        // `(?#...)` comment groups: pass everything through verbatim until `)`.
        if matches!(groups.last().map(|g| g.phase), Some(GroupPhase::Comment)) {
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(b as char);
                let next = bytes[i + 1];
                if next < 0x80 {
                    out.push(next as char);
                    i += 2;
                } else {
                    let ch_end = i + 1 + utf8_char_len(next);
                    out.push_str(&pattern[i + 1..ch_end]);
                    i = ch_end;
                }
                continue;
            }
            if b == b')' {
                groups.pop();
            }
            if b < 0x80 {
                out.push(b as char);
                i += 1;
            } else {
                let ch_end = i + utf8_char_len(b);
                out.push_str(&pattern[i..ch_end]);
                i = ch_end;
            }
            continue;
        }

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
                mark_body_started(&mut groups);
            }
            b'[' if !in_class => {
                out.push(b as char);
                in_class = true;
                i += 1;
                mark_body_started(&mut groups);
            }
            b']' if in_class => {
                out.push(b as char);
                in_class = false;
                i += 1;
            }
            b'(' if !in_class => {
                out.push(b as char);
                i += 1;
                let parent_extended = groups.last().map(|g| g.extended).unwrap_or(false);
                // Decide this group's initial phase based on what follows `(`.
                if i + 1 < bytes.len() && bytes[i] == b'?' && bytes[i + 1] == b'#' {
                    out.push('?');
                    out.push('#');
                    i += 2;
                    groups.push(Group {
                        phase: GroupPhase::Comment,
                        extended: parent_extended,
                        prefix_negating: false,
                        body_started: false,
                    });
                } else if i < bytes.len() && bytes[i] == b'?' {
                    groups.push(Group {
                        phase: GroupPhase::Prefix,
                        extended: parent_extended,
                        prefix_negating: false,
                        body_started: false,
                    });
                } else {
                    groups.push(Group {
                        phase: GroupPhase::Body,
                        extended: parent_extended,
                        prefix_negating: false,
                        body_started: false,
                    });
                }
            }
            b')' if !in_class => {
                out.push(b as char);
                groups.pop();
                i += 1;
                mark_body_started(&mut groups);
            }
            b'|' if !in_class => match groups.last_mut() {
                None => {
                    out.push(b as char);
                    i += 1;
                    while i < bytes.len() && bytes[i] == b'|' {
                        i += 1;
                    }
                }
                Some(group) => {
                    if group.body_started {
                        // Middle or trailing empty alt — preserve.
                        out.push(b as char);
                        i += 1;
                    } else {
                        // Leading empty alt — skip this and any consecutive
                        // `|`s. `body_started` stays false so further leading
                        // alts keep getting stripped.
                        i += 1;
                    }
                }
            },
            _ if matches!(groups.last().map(|g| g.phase), Some(GroupPhase::Prefix)) => {
                out.push(b as char);
                i += 1;
                let group = groups.last_mut().unwrap();
                match b {
                    b'x' => {
                        group.extended = !group.prefix_negating;
                    }
                    b'-' => {
                        group.prefix_negating = true;
                    }
                    b':' | b'=' | b'!' | b'>' => {
                        group.phase = GroupPhase::Body;
                    }
                    b'<' => {
                        if i < bytes.len() && (bytes[i] == b'=' || bytes[i] == b'!') {
                            // Lookbehind `(?<=` / `(?<!`.
                            out.push(bytes[i] as char);
                            i += 1;
                            group.phase = GroupPhase::Body;
                        } else {
                            group.phase = GroupPhase::PrefixName;
                        }
                    }
                    _ => {
                        // Continue reading modifier / flag characters.
                    }
                }
            }
            _ if matches!(groups.last().map(|g| g.phase), Some(GroupPhase::PrefixName)) => {
                out.push(b as char);
                i += 1;
                if b == b'>' {
                    groups.last_mut().unwrap().phase = GroupPhase::Body;
                }
            }
            _ if b < 0x80 => {
                let is_ws = matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c);
                let extended = groups.last().map(|g| g.extended).unwrap_or(false);
                // In extended mode, `#` starts a line comment running to the
                // next `\n`; whitespace is ignored. Neither counts as the
                // first body character.
                if extended && b == b'#' {
                    while i < bytes.len() && bytes[i] != b'\n' {
                        if bytes[i] < 0x80 {
                            out.push(bytes[i] as char);
                            i += 1;
                        } else {
                            let ch_end = i + utf8_char_len(bytes[i]);
                            out.push_str(&pattern[i..ch_end]);
                            i = ch_end;
                        }
                    }
                    continue;
                }
                out.push(b as char);
                i += 1;
                if !(extended && is_ws) {
                    mark_body_started(&mut groups);
                }
            }
            _ => {
                let ch_end = i + utf8_char_len(b);
                out.push_str(&pattern[i..ch_end]);
                i = ch_end;
                mark_body_started(&mut groups);
            }
        }
    }
    out
}

#[derive(Copy, Clone, Debug)]
struct Group {
    phase: GroupPhase,
    extended: bool,
    prefix_negating: bool,
    body_started: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum GroupPhase {
    Prefix,
    PrefixName,
    Comment,
    Body,
}

fn mark_body_started(groups: &mut [Group]) {
    if let Some(group) = groups.last_mut() {
        if group.phase == GroupPhase::Body {
            group.body_started = true;
        }
    }
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
    fn preserves_middle_and_trailing_empty_alternatives_inside_groups() {
        // D's assignment operators: `(...|>>>||\*|...)=` — a middle empty alt
        // inside the group makes the group optional, which is intentional.
        assert_eq!(
            strip_redundant_empty_alternatives(r"(a|b||c)="),
            r"(a|b||c)="
        );
        assert_eq!(strip_redundant_empty_alternatives("(a||)"), "(a||)");
    }

    #[test]
    fn strips_leading_empty_alternatives_inside_groups() {
        // Rust's prelude_types: `(?x:|Box|Option|...)` — leading empty alt
        // wins zero-width under leftmost-first, masking the real names.
        assert_eq!(
            strip_redundant_empty_alternatives("(?x:|Box|Vec)"),
            "(?x:Box|Vec)"
        );
        assert_eq!(strip_redundant_empty_alternatives("(|a|b)"), "(a|b)");
        assert_eq!(strip_redundant_empty_alternatives("(?:|a|b)"), "(?:a|b)");
        assert_eq!(
            strip_redundant_empty_alternatives("(?P<n>|a|b)"),
            "(?P<n>a|b)"
        );
        assert_eq!(
            strip_redundant_empty_alternatives("(?<n>|a|b)"),
            "(?<n>a|b)"
        );
        assert_eq!(strip_redundant_empty_alternatives("(?=|a|b)"), "(?=a|b)");
        assert_eq!(strip_redundant_empty_alternatives("(?!|a|b)"), "(?!a|b)");
        assert_eq!(strip_redundant_empty_alternatives("(?<=|a|b)"), "(?<=a|b)");
        assert_eq!(strip_redundant_empty_alternatives("(?<!|a|b)"), "(?<!a|b)");
        // Leading stripped, middle empty preserved.
        assert_eq!(
            strip_redundant_empty_alternatives("(?x:|a|b||c)"),
            "(?x:a|b||c)"
        );
        // Consecutive leading `|`s are all skipped (same as the top-level
        // `||`-collapsing path).
        assert_eq!(strip_redundant_empty_alternatives("(|||a)"), "(a)");
        // Nested groups: both leading empties get stripped independently.
        assert_eq!(
            strip_redundant_empty_alternatives("((?x:|a)|b)"),
            "((?x:a)|b)"
        );
        // Extended mode: whitespace and `#` comments between the prefix and
        // the leading `|` are ignored by the regex engine, so they should not
        // prevent the leading `|` from being recognized as empty alt.
        // This is the shape of Rust's `prelude_types` variable.
        assert_eq!(
            strip_redundant_empty_alternatives("(?x:\n  |Box\n  |Vec\n)"),
            "(?x:\n  Box\n  |Vec\n)"
        );
        assert_eq!(
            strip_redundant_empty_alternatives("(?x:\n  # std::boxed\n  |Box\n)"),
            "(?x:\n  # std::boxed\n  Box\n)"
        );
        // Extended flag among others: `(?ix:` and `(?xi:` both enable x.
        assert_eq!(strip_redundant_empty_alternatives("(?ix: |a)"), "(?ix: a)");
        // `-x` disables extended mode — whitespace in the group body is
        // significant and must not trigger the skip-whitespace-then-alt path.
        assert_eq!(strip_redundant_empty_alternatives("(?-x: |a)"), "(?-x: |a)");
    }

    #[test]
    fn preserves_regex_comment_groups_verbatim() {
        // `(?#...)` is a comment group — whatever is inside (including `|`)
        // is ignored by the regex engine and must be passed through unchanged.
        assert_eq!(
            strip_redundant_empty_alternatives("(?#|abc)foo"),
            "(?#|abc)foo"
        );
    }

    #[test]
    fn preserves_pipes_in_character_classes_and_escapes() {
        assert_eq!(strip_redundant_empty_alternatives(r"[a||b]"), r"[a||b]");
        assert_eq!(strip_redundant_empty_alternatives(r"\|\|\|"), r"\|\|\|");
        // Escaped `(` and `|` must not trigger group/alt handling.
        assert_eq!(strip_redundant_empty_alternatives(r"\(|x"), r"\(|x");
    }

    #[test]
    fn empty_alt_regex_matches_bang() {
        // End-to-end: the Cabal pattern should now successfully match `!`.
        let regex = Regex::new(String::from(r"\|\||&&||!"));
        let mut region = Region::new();
        assert!(regex.search("!impl", 0, 5, Some(&mut region)));
        assert_eq!(region.pos(0), Some((0, 1)));
    }

    #[test]
    fn leading_empty_alt_in_group_matches_name() {
        // End-to-end: the Rust prelude_types shape should match `Vec` as a
        // real alternative rather than winning empty at position 0.
        let regex = Regex::new(String::from(r"\b(?x:|Box|Vec)\b"));
        let mut region = Region::new();
        assert!(regex.search("Vec", 0, 3, Some(&mut region)));
        assert_eq!(region.pos(0), Some((0, 3)));
    }
}
