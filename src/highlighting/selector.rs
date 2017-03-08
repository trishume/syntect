/// Code based on https://github.com/defuz/sublimate/blob/master/src/core/syntax/scope.rs
/// released under the MIT license by @defuz

use parsing::{Scope, ScopeStack, MatchPower, ParseScopeError};
use std::str::FromStr;

/// A single selector consisting of a stack to match and a possible stack to exclude from being matched.
/// You probably want `ScopeSelectors` which is this but with union support.
#[derive(Debug, Clone, PartialEq, Eq, Default, RustcEncodable, RustcDecodable)]
pub struct ScopeSelector {
    path: ScopeStack,
    exclude: Option<ScopeStack>,
}

/// A selector set that matches anything matched by any of its component selectors.
/// See [The TextMate Docs](https://manual.macromates.com/en/scope_selectors) for how these
/// work.
#[derive(Debug, Clone, PartialEq, Eq, Default, RustcEncodable, RustcDecodable)]
pub struct ScopeSelectors {
    /// the selectors, if any of them match, this matches
    pub selectors: Vec<ScopeSelector>,
}

impl ScopeSelector {
    /// Checks if this selector matches a given scope stack.
    /// See `ScopeSelectors#does_match` for more info.
    pub fn does_match(&self, stack: &[Scope]) -> Option<MatchPower> {
        // if there is an exclusion, and it matches, then this selector doesn't match
        if self.exclude.is_some() {
            let exclusion = self.exclude.as_ref().unwrap();
            if exclusion.is_empty() || exclusion.does_match(stack).is_some() {
                return None;
            }
        }
        if self.path.is_empty() {
            // an empty scope selector always matches with a score of 1
            Some(MatchPower(0o1u64 as f64))
        } else {
            self.path.does_match(stack)
        }
    }

    /// If this selector is really just a single scope, return it
    pub fn extract_single_scope(&self) -> Option<Scope> {
        if self.path.len() > 1 || self.exclude.is_some() || self.path.is_empty() {
            return None;
        }
        Some(self.path.as_slice()[0])
    }
}

impl FromStr for ScopeSelector {
    type Err = ParseScopeError;

    /// Parses a scope stack followed optionally by a " -" and then a scope stack to exclude
    fn from_str(s: &str) -> Result<ScopeSelector, ParseScopeError> {
        match s.find(" -") {
            Some(index) => {
                let (path_str, exclude_with_dash) = s.split_at(index);
                let exclude_str = &exclude_with_dash[2..];
                Ok(ScopeSelector {
                    path: try!(ScopeStack::from_str(path_str)),
                    exclude: Some(try!(ScopeStack::from_str(exclude_str))),
                })
            }
            None => {
                Ok(ScopeSelector {
                    path: try!(ScopeStack::from_str(s)),
                    exclude: None,
                })
            }
        }
    }
}

impl ScopeSelectors {
    /// checks if any of these selectors match the given scope stack
    /// if so it returns a match score, higher match scores are stronger
    /// matches. Scores are ordered according to the rules found
    /// at https://manual.macromates.com/en/scope_selectors
    ///
    /// # Examples
    ///
    /// ```
    /// use syntect::parsing::{ScopeStack, MatchPower};
    /// use syntect::highlighting::ScopeSelectors;
    /// use std::str::FromStr;
    /// assert_eq!(ScopeSelectors::from_str("a.b, a e.f - c k, e.f - a.b").unwrap()
    ///     .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
    ///     Some(MatchPower(0o2001u64 as f64)));
    /// ```
    pub fn does_match(&self, stack: &[Scope]) -> Option<MatchPower> {
        self.selectors.iter().filter_map(|sel| sel.does_match(stack)).max()
    }
}

impl FromStr for ScopeSelectors {
    type Err = ParseScopeError;

    /// Parses a series of selectors separated by commas or pipes
    fn from_str(s: &str) -> Result<ScopeSelectors, ParseScopeError> {
        let mut selectors = Vec::new();
        for selector in s.split(&[',', '|'][..]) {
            selectors.push(try!(ScopeSelector::from_str(selector)))
        }
        Ok(ScopeSelectors { selectors: selectors })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selectors_work() {
        use std::str::FromStr;
        let sels = ScopeSelectors::from_str("source.php meta.preprocessor - string.quoted, \
                                             source string")
            .unwrap();
        assert_eq!(sels.selectors.len(), 2);
        let first_sel = &sels.selectors[0];
        assert_eq!(format!("{:?}", first_sel),
                   "ScopeSelector { path: ScopeStack { clear_stack: [], scopes: [<source.php>, <meta.preprocessor>] }, exclude: Some(ScopeStack { clear_stack: [], scopes: [<string.quoted>] }) }");
        
        let sels = ScopeSelectors::from_str("source.php meta.preprocessor -string.quoted|\
                                             source string")
            .unwrap();
        assert_eq!(sels.selectors.len(), 2);
        let first_sel = &sels.selectors[0];
        assert_eq!(format!("{:?}", first_sel),
                   "ScopeSelector { path: ScopeStack { clear_stack: [], scopes: [<source.php>, <meta.preprocessor>] }, exclude: Some(ScopeStack { clear_stack: [], scopes: [<string.quoted>] }) }");
        
        let sels = ScopeSelectors::from_str("text.xml meta.tag.preprocessor.xml punctuation.separator.key-value.xml")
            .unwrap();
        assert_eq!(sels.selectors.len(), 1);
        let first_sel = &sels.selectors[0];
        assert_eq!(format!("{:?}", first_sel),
                   "ScopeSelector { path: ScopeStack { clear_stack: [], scopes: [<text.xml>, <meta.tag.preprocessor.xml>, <punctuation.separator.key-value.xml>] }, exclude: None }");
    }
    #[test]
    fn matching_works() {
        use parsing::{ScopeStack, MatchPower};
        use std::str::FromStr;
        assert_eq!(ScopeSelectors::from_str("a.b, a e, e.f")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b e.f").unwrap().as_slice()),
                   Some(MatchPower(0o20u64 as f64)));
        assert_eq!(ScopeSelectors::from_str("a.b, a e.f, e.f")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b e.f").unwrap().as_slice()),
                   Some(MatchPower(0o21u64 as f64)));
        assert_eq!(ScopeSelectors::from_str("a.b, a e.f - c j, e.f")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
                   Some(MatchPower(0o2000u64 as f64)));
        assert_eq!(ScopeSelectors::from_str("a.b, a e.f - c j, e.f - a.b")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
                   Some(MatchPower(0o2u64 as f64)));
        assert_eq!(ScopeSelectors::from_str("a.b, a e.f - c k, e.f - a.b")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
                   Some(MatchPower(0o2001u64 as f64)));
        assert_eq!(ScopeSelectors::from_str("a.b|a e.f -d, e.f -a.b")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d e.f").unwrap().as_slice()),
                   Some(MatchPower(0o201u64 as f64)));
    }
    
    #[test]
    fn empty_stack_matching_works() {
        use parsing::{ScopeStack, MatchPower};
        use std::str::FromStr;
        assert_eq!(ScopeSelector::from_str(" - a.b")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
                   None);
        assert_eq!(ScopeSelector::from_str("")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
                   Some(MatchPower(0o1u64 as f64)));
        assert_eq!(ScopeSelector::from_str("")
                       .unwrap()
                       .does_match(ScopeStack::from_str("").unwrap().as_slice()),
                   Some(MatchPower(0o1u64 as f64)));
        assert_eq!(ScopeSelector::from_str(" - a.b")
                       .unwrap()
                       .does_match(ScopeStack::from_str("").unwrap().as_slice()),
                   Some(MatchPower(0o1u64 as f64)));
        assert_eq!(ScopeSelector::from_str("a.b - ")
                       .unwrap()
                       .does_match(ScopeStack::from_str("").unwrap().as_slice()),
                   None);
        assert_eq!(ScopeSelector::from_str("a.b - ")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
                   None);
        assert_eq!(ScopeSelector::from_str(" - ")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
                   None);
        assert_eq!(ScopeSelector::from_str(" - a.b")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
                   None);
        assert_eq!(ScopeSelector::from_str(" - g.h")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
                   Some(MatchPower(0o1u64 as f64)));
        
        assert_eq!(ScopeSelector::from_str(" -a.b")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
                   None);
        assert_eq!(ScopeSelector::from_str("")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
                   Some(MatchPower(0o1u64 as f64)));
        assert_eq!(ScopeSelector::from_str(" -a.b")
                       .unwrap()
                       .does_match(ScopeStack::from_str("").unwrap().as_slice()),
                   Some(MatchPower(0o1u64 as f64)));
        assert_eq!(ScopeSelector::from_str("a.b -")
                       .unwrap()
                       .does_match(ScopeStack::from_str("").unwrap().as_slice()),
                   None);
        assert_eq!(ScopeSelector::from_str("a.b -")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
                   None);
        assert_eq!(ScopeSelector::from_str(" -")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
                   None);
        assert_eq!(ScopeSelector::from_str(" -a.b")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
                   None);
        assert_eq!(ScopeSelector::from_str(" -g.h")
                       .unwrap()
                       .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
                   Some(MatchPower(0o1u64 as f64)));
    }
}
