/// Code based on https://github.com/defuz/sublimate/blob/master/src/core/syntax/scope.rs
/// released under the MIT license by @defuz

use scope::*;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Default, RustcEncodable, RustcDecodable)]
pub struct ScopeSelector {
    path: ScopeStack,
    exclude: Option<ScopeStack>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, RustcEncodable, RustcDecodable)]
pub struct ScopeSelectors {
    pub selectors: Vec<ScopeSelector>,
}

impl ScopeSelector {
    pub fn does_match(&self, stack: &[Scope]) -> Option<MatchPower> {
        if self.exclude.as_ref().and_then(|sel| sel.does_match(stack)).is_some() {
            return None;
        }
        self.path.does_match(stack)
    }
}

impl FromStr for ScopeSelector {
    type Err = ParseScopeError;

    fn from_str(s: &str) -> Result<ScopeSelector, ParseScopeError> {
        match s.find(" - ") {
            Some(index) => {
                let (path_str, exclude_with_dash) = s.split_at(index);
                let exclude_str = &exclude_with_dash[3..];
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
    pub fn does_match(&self, stack: &[Scope]) -> Option<MatchPower> {
        self.selectors.iter().filter_map(|sel| sel.does_match(stack)).max()
    }
}

impl FromStr for ScopeSelectors {
    type Err = ParseScopeError;

    /// checks if any of these selectors match the given scope stack
    /// if so it returns a match score, higher match scores are stronger
    /// matches. Scores are ordered according to the rules found
    /// at https://manual.macromates.com/en/scope_selectors
    /// # Examples
    /// ```
    /// use syntect::scope::{ScopeStack, MatchPower};
    /// use syntect::theme::selector::ScopeSelectors;
    /// use std::str::FromStr;
    /// assert_eq!(ScopeSelectors::from_str("a.b, a e.f - c k, e.f - a.b").unwrap()
    ///     .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
    ///     Some(MatchPower(0o2001u64 as f64)));
    /// ```
    fn from_str(s: &str) -> Result<ScopeSelectors, ParseScopeError> {
        let mut selectors = Vec::new();
        for selector in s.split(',') {
            selectors.push(try!(ScopeSelector::from_str(selector)))
        }
        Ok(ScopeSelectors { selectors: selectors })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn selectors_work() {
        use theme::selector::*;
        use std::str::FromStr;
        let sels = ScopeSelectors::from_str("source.php meta.preprocessor - string.quoted, \
                                             source string")
            .unwrap();
        assert_eq!(sels.selectors.len(), 2);
        let first_sel = &sels.selectors[0];
        assert_eq!(format!("{:?}", first_sel),
                   "ScopeSelector { path: ScopeStack { scopes: [<source.php>, \
                    <meta.preprocessor>] }, exclude: Some(ScopeStack { scopes: [<string.quoted>] \
                    }) }");
    }
    #[test]
    fn matching_works() {
        use scope::{ScopeStack,MatchPower};
        use theme::selector::*;
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
    }
}
