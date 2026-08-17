use serde::de::{Deserialize, Deserializer};
use serde::ser::{Serialize, Serializer};
use std::error::Error;
use std::sync::OnceLock;

/// An abstraction for regex patterns.
///
/// * Allows swapping out the regex implementation because it's only in this module.
/// * Makes regexes serializable and deserializable using just the pattern string.
/// * Lazily compiles regexes on first use to improve initialization time.
///
/// ### The literal prefilter
///
/// Colouring a line costs one regex search per pattern in the context stack,
/// and a context has tens of them. Nearly all of those searches fail, and a
/// backtracking engine is an expensive way to establish that a line does not
/// contain `struct`.
///
/// So each pattern carries a `Prefilter`: the literal text every match of it
/// must begin with, taken off the pattern by `regex-syntax`. A memchr or
/// Aho-Corasick pass over the rest of the line says whether any of those
/// literals is there at all, and a pattern whose literal is absent is not
/// searched. It is a *necessary* condition only — a hit still runs the real
/// engine — so nothing it says can add a match, and a debug build checks every
/// rejection against the engine it skipped (see [`Regex::search`]).
#[derive(Debug)]
pub struct Regex {
    regex_str: String,
    regex: OnceLock<regex_impl::Regex>,
    prefilter: OnceLock<Option<Prefilter>>,
    /// Lazily-compiled variant that won't match zero-length strings.
    /// Used for `MatchOperation::None` patterns, where a zero-width match
    /// would stall the parser at the same position.
    ///
    /// `None` means the pattern can only ever match zero-width, so compiling with
    /// the engine's `FIND_NOT_EMPTY` option fails (fancy-regex reports
    /// `PatternCanNeverMatch`). In that case searches return no match, which is
    /// what the parser wants — a scope-only pattern that can only match
    /// zero-width would stall.
    regex_not_empty: OnceLock<Option<regex_impl::Regex>>,
}

/// What every match of a pattern has to begin with.
///
/// Built once per pattern, on first search, from the pattern string rather
/// than from the compiled regex — the engine is Oniguruma and does not offer
/// this, and `regex-syntax` is already here to pre-test patterns.
#[derive(Debug)]
enum Prefilter {
    /// One required prefix: the common case, and the cheapest.
    One(memchr::memmem::Finder<'static>),
    /// An alternation of them — `(?:pub|fn|let)` — searched together.
    Any(Box<aho_corasick::AhoCorasick>),
}

impl Prefilter {
    /// Could a match begin anywhere in this stretch of text?
    ///
    /// Conservative in one direction only: `true` means "run the engine", and
    /// is always allowed to be wrong. `false` means no match is possible, and
    /// has to be right.
    fn maybe(&self, haystack: &[u8]) -> bool {
        match self {
            Prefilter::One(finder) => finder.find(haystack).is_some(),
            Prefilter::Any(set) => set.is_match(haystack),
        }
    }

    /// The literals every match of `pattern` must start with, or `None` when
    /// there is no useful answer.
    ///
    /// `None` is the safe result and the common one: a pattern `regex-syntax`
    /// will not parse — Oniguruma has lookaround, possessive quantifiers and
    /// `\h`, and Sublime's definitions use all three — is simply not filtered.
    /// So is one whose prefixes include the empty string, which is every
    /// pattern that can match nothing and every pattern starting with `^`.
    fn extract(pattern: &str) -> Option<Prefilter> {
        use regex_syntax::hir::literal::{ExtractKind, Extractor};

        let hir = match regex_syntax::Parser::new().parse(pattern) {
            Ok(hir) => hir,
            // A pattern `regex-syntax` will not read may
            // still *start* with one it will. `(?=X)…` says that a match here
            // requires `X` to match here too, so whatever every match of `X`
            // has to begin with, every match of the whole pattern has to begin
            // with. Around half of Sublime's patterns get no prefilter because
            // of lookaround, and a good few of those are lookaround at the
            // front — this is the half of that half which is nearly free.
            //
            // Still a necessary condition and nothing more: the literals are
            // required *at the match position*, so requiring them anywhere in
            // the rest of the line is weaker than the truth, which is the safe
            // direction. And it is checked either way — every rejection goes
            // to the engine it skipped under the `debug_assert` in `search`.
            Err(_) => regex_syntax::Parser::new()
                .parse(lookahead_head(pattern)?)
                .ok()?,
        };
        let seq = Extractor::new().kind(ExtractKind::Prefix).extract(&hir);
        let literals = seq.literals()?;
        // An empty literal is a prefix of everything and rules nothing out;
        // one that is long enough to be worth searching for is the point.
        if literals.is_empty() || seq.min_literal_len().unwrap_or(0) == 0 {
            return None;
        }
        // A very wide alternation costs more to run than the searches it
        // saves. `(?i:...)` is what produces these: case folding turns four
        // letters into sixteen literals.
        if literals.len() > 64 {
            return None;
        }
        if let [only] = literals {
            let owned: Vec<u8> = only.as_bytes().to_vec();
            return Some(Prefilter::One(memchr::memmem::Finder::new(&owned).into_owned()));
        }
        let set = aho_corasick::AhoCorasick::new(literals.iter().map(|l| l.as_bytes())).ok()?;
        Some(Prefilter::Any(Box::new(set)))
    }
}

/// What is inside a leading `(?=…)`, if the pattern opens
/// with one.
///
/// Text rather than a parse, because the point is to hand a *smaller string*
/// to the parser that could not read the whole one. Only a positive lookahead
/// at the very front: `(?!…)` says what is not there and rules nothing in, and
/// one anywhere else in the pattern is a condition on a position this cannot
/// know.
///
/// The scan has to know about escapes and character classes, or `(?=[)])`
/// ends at the wrong place — and a lookahead cut short is a *different*
/// expression, which is the one way this could produce literals that are not
/// required.
///
/// **And the lookahead has to govern the whole pattern.** `(?=X)|Y` is an
/// alternation whose second branch never sees `X` at all, so `X`'s literals
/// are required by one reading and by nothing else — this is the shape that
/// caught it, out of OCaml:
///
/// ```text
/// (?=\b(type|val|external|class|module|end)\b)|^\s*(?m:$)
/// ```
///
/// A blank line matches the right-hand branch and holds none of those words,
/// so filtering on them skips a search that would have matched. So a `|` left
/// at depth zero after the group means no prefilter.
fn lookahead_head(pattern: &str) -> Option<&str> {
    let rest = pattern.strip_prefix("(?=")?;
    // Where the lookahead group ends, and then whether what follows it is
    // another way for the whole pattern to match. Two passes, because they are
    // two questions: the first `)` at depth zero closes the group, and every
    // later paren belongs to something else — reading them as if they could
    // still close it is how this returned `\bfn\b)(?<!\.` for
    // `(?=\bfn\b)(?<!\.)`, which is not an expression at all.
    let scan = |from: usize, stop_at_depth_zero: bool| -> Option<usize> {
        let (mut depth, mut escaped, mut in_class) = (1usize, false, false);
        for (at, c) in rest[from..].char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match c {
                '\\' => escaped = true,
                '[' if !in_class => in_class = true,
                ']' if in_class => in_class = false,
                '(' if !in_class => depth += 1,
                ')' if !in_class => {
                    depth -= 1;
                    if depth == 0 && stop_at_depth_zero {
                        return Some(from + at);
                    }
                }
                // Only asked for on the second pass, where `depth` starts at
                // one for the pattern itself: a `|` here is a branch of the
                // whole pattern, and it never sees the lookahead.
                '|' if !in_class && depth == 1 && !stop_at_depth_zero => return Some(from + at),
                _ => {}
            }
        }
        None
    };
    let end = scan(0, true)?;
    if end == 0 || scan(end + 1, false).is_some() {
        return None;
    }
    Some(&rest[..end])
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
            regex_not_empty: OnceLock::new(),
            prefilter: OnceLock::new(),
        }
    }

    /// The same, for a pattern that will be searched once
    /// and thrown away.
    ///
    /// Backreference patterns are rebuilt at every token they are tried at —
    /// `substitute_backrefs_in_regex` makes a new pattern out of what the last
    /// match captured — so working out a prefilter for one costs a
    /// `regex-syntax` parse per token to save at most a single search. That is
    /// the wrong trade, and this is how the parser says so.
    pub fn new_unfiltered(regex_str: String) -> Self {
        let prefilter = OnceLock::new();
        let _ = prefilter.set(None);
        Self {
            regex_str,
            regex: OnceLock::new(),
            regex_not_empty: OnceLock::new(),
            prefilter,
        }
    }

    /// Check whether the pattern compiles as a valid regex or not.
    pub fn try_compile(regex_str: &str) -> Option<Box<dyn Error + Send + Sync + 'static>> {
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
    ///
    /// If a region is passed, it is used for storing match group positions. The argument allows
    /// the [`Region`] to be reused between searches, which makes a significant performance
    /// difference.
    ///
    /// When `allow_empty` is `false`, zero-length matches are rejected by the engine itself.
    /// Pass `false` for match patterns whose operation does not push, set, pop, or embed a
    /// context, to prevent the parser from stalling at the same position. Pass `true` in every
    /// other situation (lookaheads used with branch/fail, empty patterns used with pop/set,
    /// escape patterns, variable-substitution helpers, etc.).
    ///
    /// [`Region`]: struct.Region.html
    pub fn search(
        &self,
        text: &str,
        begin: usize,
        end: usize,
        region: Option<&mut Region>,
        allow_empty: bool,
    ) -> bool {
        // The prefilter, in front of whichever engine is
        // about to be asked. It is a necessary condition on the *pattern*, so
        // it holds for the not-empty variant too — that one is this pattern
        // with zero-width matches ruled out, which can only ever match less.
        if let Some(prefilter) = self.prefilter() {
            let haystack = &text.as_bytes()[begin.min(end)..end.min(text.len())];
            if !prefilter.maybe(haystack) {
                // The whole safety argument for the prefilter, checked rather
                // than argued, on every debug build and every test run: what
                // it turned away must be what the engine would have turned
                // away. `regex-syntax` and Oniguruma are two readings of one
                // pattern string, and this is where they would disagree.
                debug_assert!(
                    !self.regex().search(text, begin, end, None),
                    "prefilter rejected a line the engine matches: {:?}",
                    self.regex_str
                );
                return false;
            }
        }
        if allow_empty {
            return self
                .regex()
                .search(text, begin, end, region.map(|r| &mut r.region));
        }
        match self.regex_not_empty() {
            Some(regex) => regex.search(text, begin, end, region.map(|r| &mut r.region)),
            None => false,
        }
    }

    /// Whether this pattern got a prefilter at all.
    ///
    /// The prefilter is invisible when it stops working. If `extract` began
    /// answering `None` for everything — a `regex-syntax` upgrade that parses
    /// one construct differently, a guard clause that grew too strict — every
    /// test would still pass, every file would still be coloured correctly,
    /// and colouring a line would quietly cost twice what it costs now. So
    /// there is a test that a pattern which *can* be filtered is, and this is
    /// the only thing it needs to see. Nothing in the parser calls it.
    pub fn has_prefilter(&self) -> bool {
        self.prefilter().is_some()
    }

    fn prefilter(&self) -> Option<&Prefilter> {
        self.prefilter
            .get_or_init(|| Prefilter::extract(&self.regex_str))
            .as_ref()
    }

    fn regex(&self) -> &regex_impl::Regex {
        self.regex.get_or_init(|| {
            regex_impl::Regex::new(&self.regex_str).expect("regex string should be pre-tested")
        })
    }

    fn regex_not_empty(&self) -> Option<&regex_impl::Regex> {
        self.regex_not_empty
            .get_or_init(|| regex_impl::Regex::new_find_not_empty(&self.regex_str).ok())
            .as_ref()
    }
}

impl Clone for Regex {
    fn clone(&self) -> Self {
        Regex {
            regex_str: self.regex_str.clone(),
            regex: OnceLock::new(),
            regex_not_empty: OnceLock::new(),
            prefilter: OnceLock::new(),
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

        pub fn new_find_not_empty(
            regex_str: &str,
        ) -> Result<Regex, Box<dyn Error + Send + Sync + 'static>> {
            let result = onig::Regex::with_options(
                regex_str,
                RegexOptions::REGEX_OPTION_CAPTURE_GROUP
                    | RegexOptions::REGEX_OPTION_FIND_NOT_EMPTY,
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

        pub fn new_find_not_empty(
            regex_str: &str,
        ) -> Result<Regex, Box<dyn Error + Send + Sync + 'static>> {
            let result = fancy_regex::RegexBuilder::new(regex_str)
                .oniguruma_mode(true)
                .find_not_empty(true)
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
}
