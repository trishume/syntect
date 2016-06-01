/// Code based on https://github.com/defuz/sublimate/blob/master/src/core/syntax/scope.rs
/// released under the MIT license by @defuz

use scope::*;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScopeSelector {
    path: ScopeStack,
    exclude: Option<ScopeStack>
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScopeSelectors {
    pub selectors: Vec<ScopeSelector>
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
            },
            None => {
                Ok(ScopeSelector {
                    path: try!(ScopeStack::from_str(s)),
                    exclude: None,
                })
            }
        }
    }
}

impl FromStr for ScopeSelectors {
    type Err = ParseScopeError;

    fn from_str(s: &str) -> Result<ScopeSelectors, ParseScopeError> {
        let mut selectors = Vec::new();
        for selector in s.split(',') {
            selectors.push(try!(ScopeSelector::from_str(selector)))
        };
        Ok(ScopeSelectors {
            selectors: selectors
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn selectors_work() {
        use scope::*;
        use theme::selector::*;
        use std::str::FromStr;
        let sels = ScopeSelectors::from_str("source.php meta.preprocessor - string.quoted, source string").unwrap();
        assert_eq!(sels.selectors.len(), 2);
        let first_sel = &sels.selectors[0];
        assert_eq!(format!("{:?}", first_sel),
            "ScopeSelector { path: ScopeStack { scopes: [<source.php>, <meta.preprocessor>] }, exclude: Some(ScopeStack { scopes: [<string.quoted>] }) }");
    }
}
