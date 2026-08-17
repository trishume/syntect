/// Code based on <https://github.com/defuz/sublimate/blob/master/src/core/syntax/scope.rs>
/// released under the MIT license by @defuz
use crate::parsing::{MatchPower, ParseScopeError, Scope, ScopeStack};
use serde_derive::{Deserialize, Serialize};
use std::str::FromStr;

/// A single selector consisting of a stack to match and a possible stack to
/// exclude from being matched.
///
/// You probably want [`ScopeSelectors`] which is this but with union support.
///
/// [`ScopeSelectors`]: struct.ScopeSelectors.html
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ScopeSelector {
    pub path: ScopeStack,
    pub excludes: Vec<ScopeStack>,
    /// The other stacks this selector requires, from `&`.
    ///
    /// A `&` is a *conjunction*: `(source.c | source.c++) & comment.block`
    /// matches a stack that has one of the two source scopes **and**
    /// `comment.block`, in any order and at any depth — which is a different
    /// question from `source.c++ comment.block`, a descendant path, where the
    /// order is the whole point.
    ///
    /// Splitting on `,` and `|` alone left a bare ` source.c++ ` as one of the
    /// selectors of that example, so it matched a file that was not in a
    /// comment at all. Sublime Text answers 0 there and 32 with the comment on
    /// the stack (`sublime.score_selector`, asked directly).
    #[serde(default)]
    pub conjuncts: Vec<ScopeStack>,
}

/// A selector set that matches anything matched by any of its component selectors.
///
/// See [The TextMate Docs](https://manual.macromates.com/en/scope_selectors) for how these work.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ScopeSelectors {
    /// The selectors, if any of them match, that this matches
    pub selectors: Vec<ScopeSelector>,
}

impl ScopeSelector {
    /// Checks if this selector matches a given scope stack.
    ///
    /// See [`ScopeSelectors::does_match`] for more info.
    ///
    /// [`ScopeSelectors::does_match`]: struct.ScopeSelectors.html#method.does_match
    pub fn does_match(&self, stack: &[Scope]) -> Option<MatchPower> {
        // if there are any exclusions, and any one of them matches, then this selector doesn't match
        if self
            .excludes
            .iter()
            .any(|sel| sel.is_empty() || sel.does_match(stack).is_some())
        {
            return None;
        }
        let power = if self.path.is_empty() {
            // an empty scope selector always matches with a score of 1
            MatchPower(0o1u64 as f64)
        } else {
            self.path.does_match(stack)?
        };
        // Every conjunct has to match as well, and the score
        // is the strongest of them rather than their sum.
        //
        // Sublime Text's own numbers say which: on `source.c++ comment.block`,
        // `source.c++` scores 4, `comment.block` scores 32, the descendant path
        // `source.c++ comment.block` scores 36 — the two added — and
        // `source.c++ & comment.block` scores 32, the larger of the two. Both
        // orders of the conjunction give 32, so it is the maximum and not the
        // last one written.
        let mut best = power;
        for conjunct in self.conjuncts.iter() {
            let power = match conjunct.is_empty() {
                true => MatchPower(0o1u64 as f64),
                false => conjunct.does_match(stack)?,
            };
            if power > best {
                best = power;
            }
        }
        Some(best)
    }

    /// If this selector is really just a single scope, return it
    pub fn extract_single_scope(&self) -> Option<Scope> {
        // A conjunction is not a single scope either.
        if self.path.len() > 1
            || !self.excludes.is_empty()
            || !self.conjuncts.is_empty()
            || self.path.is_empty()
        {
            return None;
        }
        Some(self.path.as_slice()[0])
    }

    /// Extract all selectors for generating CSS
    pub fn extract_scopes(&self) -> Vec<Scope> {
        // Including the ones a `&` requires — they are as
        // much a part of what this selector is about as the path is.
        let mut scopes = self.path.scopes.clone();
        for conjunct in self.conjuncts.iter() {
            scopes.extend_from_slice(&conjunct.scopes);
        }
        scopes
    }
}

impl FromStr for ScopeSelector {
    type Err = ParseScopeError;

    /// Parses a scope stack followed optionally by (one or more) " -" and then a scope stack to exclude
    ///
    /// And `&` and grouping, by way of the same parser
    /// [`ScopeSelectors`] uses. A string that comes out as more than one
    /// selector — `a | b`, or a `-` over a group — keeps only the first, since
    /// this type is one selector by definition; parse it as `ScopeSelectors`
    /// when it may be a union.
    fn from_str(s: &str) -> Result<ScopeSelector, ParseScopeError> {
        let mut selectors = parse_selector_expr(s)?.into_dnf();
        match selectors.is_empty() {
            true => Ok(ScopeSelector::default()),
            false => Ok(selectors.swap_remove(0)),
        }
    }
}

/// The selector grammar, which is more than a `split`.
///
/// Sublime documents four operators and they have a precedence:
///
/// - `,` and `|` — union, and the loosest
/// - `&` — conjunction, tighter than union
/// - ` -` — exclusion, tighter still, and written with a space in front so that
///   a scope name may contain a dash (`entity.name.function.post-blit`)
/// - `( … )` — grouping
///
/// The precedence was asked of Sublime Text rather than assumed:
/// `source.c | source.c++ & comment.block` matches a bare `source.c` (score 4),
/// which it could not if `|` bound tighter, and
/// `(source.c | source.c++) & comment.block` does not — so the parentheses
/// change the answer, which is what says there is a grammar here at all.
///
/// A parsed expression is flattened to a union of simple selectors — a path,
/// the conjuncts it requires and the exclusions that veto it — because that is
/// what [`ScopeSelector`] can hold and what `does_match` is written against.
/// Distributing is what makes `a - (b & c)` expressible: it is
/// `(a - b) | (a - c)`.
#[derive(Debug, Clone)]
enum SelectorExpr {
    Path(ScopeStack),
    Union(Vec<SelectorExpr>),
    Conjunction(Vec<SelectorExpr>),
    Exclusion(Box<SelectorExpr>, Vec<SelectorExpr>),
}

impl SelectorExpr {
    /// Every way this expression can match, as selectors that do not nest.
    fn into_dnf(self) -> Vec<ScopeSelector> {
        match self {
            SelectorExpr::Path(path) => vec![ScopeSelector {
                path,
                excludes: Vec::new(),
                conjuncts: Vec::new(),
            }],
            SelectorExpr::Union(parts) => {
                parts.into_iter().flat_map(|part| part.into_dnf()).collect()
            }
            SelectorExpr::Conjunction(parts) => {
                // The cross product: one selector per combination of the
                // alternatives its operands offer.
                let mut out: Vec<ScopeSelector> = vec![ScopeSelector::default()];
                for part in parts {
                    let alternatives = part.into_dnf();
                    let mut next = Vec::with_capacity(out.len() * alternatives.len());
                    for base in out.iter() {
                        for alternative in alternatives.iter() {
                            next.push(conjoin(base, alternative));
                        }
                    }
                    out = next;
                }
                out
            }
            SelectorExpr::Exclusion(of, minus) => {
                // `a - (b | c)` excludes both; `a - (b & c)` is two selectors,
                // one excluding each, because "not (b and c)" is "not b or not
                // c" and there is nowhere else to put it.
                let mut out = of.into_dnf();
                for term in minus {
                    let mut next = Vec::new();
                    for excluded in term.into_dnf() {
                        // A selector with conjuncts contributes one exclusion
                        // per stack it requires.
                        // An *empty* exclusion is kept, not dropped: `a.b - `
                        // excludes everything and matches nothing, which is
                        // what `does_match` reads it as and what
                        // `empty_stack_matching_works` pins.
                        let stacks: Vec<ScopeStack> = std::iter::once(excluded.path)
                            .chain(excluded.conjuncts)
                            .collect();
                        for stack in stacks {
                            for base in out.iter() {
                                let mut with = base.clone();
                                with.excludes.push(stack.clone());
                                next.push(with);
                            }
                        }
                    }
                    if !next.is_empty() {
                        out = next;
                    }
                }
                out
            }
        }
    }
}

/// One selector requiring everything two of them require.
fn conjoin(a: &ScopeSelector, b: &ScopeSelector) -> ScopeSelector {
    let mut out = a.clone();
    if out.path.is_empty() {
        out.path = b.path.clone();
    } else if !b.path.is_empty() {
        out.conjuncts.push(b.path.clone());
    }
    out.conjuncts.extend(b.conjuncts.iter().cloned());
    out.excludes.extend(b.excludes.iter().cloned());
    out
}

/// Union: the loosest, so it is read first and its operands are everything else.
fn parse_selector_expr(s: &str) -> Result<SelectorExpr, ParseScopeError> {
    let parts = split_top_level(s, &[',', '|']);
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        out.push(parse_conjunction(part)?);
    }
    Ok(match out.len() {
        1 => out.pop().unwrap(),
        _ => SelectorExpr::Union(out),
    })
}

fn parse_conjunction(s: &str) -> Result<SelectorExpr, ParseScopeError> {
    let parts = split_top_level(s, &['&']);
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        out.push(parse_exclusion(part)?);
    }
    Ok(match out.len() {
        1 => out.pop().unwrap(),
        _ => SelectorExpr::Conjunction(out),
    })
}

/// A `-` is an operator only with whitespace in front of it: scope names carry
/// dashes (`meta.function.local-symbol`) and always have.
fn parse_exclusion(s: &str) -> Result<SelectorExpr, ParseScopeError> {
    let mut terms = Vec::new();
    let mut depth = 0usize;
    let mut last = 0usize;
    let bytes = s.as_bytes();
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '-' if depth == 0
                && i > 0
                && bytes[i - 1].is_ascii_whitespace() =>
            {
                terms.push(&s[last..i]);
                last = i + 1;
            }
            _ => {}
        }
    }
    terms.push(&s[last..]);
    let first = parse_primary(terms[0])?;
    if terms.len() == 1 {
        return Ok(first);
    }
    let mut minus = Vec::with_capacity(terms.len() - 1);
    for term in &terms[1..] {
        minus.push(parse_primary(term)?);
    }
    Ok(SelectorExpr::Exclusion(Box::new(first), minus))
}

/// A group, or a plain descendant path.
fn parse_primary(s: &str) -> Result<SelectorExpr, ParseScopeError> {
    let trimmed = s.trim();
    if trimmed.starts_with('(') && ends_the_group(trimmed) {
        return parse_selector_expr(&trimmed[1..trimmed.len() - 1]);
    }
    Ok(SelectorExpr::Path(ScopeStack::from_str(trimmed)?))
}

/// Whether the `)` that closes this string's leading `(` is its last character
/// — `(a) & (b)` opens and closes twice and is not one group.
fn ends_the_group(s: &str) -> bool {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return i + c.len_utf8() == s.len();
                }
            }
            _ => {}
        }
    }
    false
}

/// Split on any of `separators`, ignoring the ones inside parentheses.
fn split_top_level<'a>(s: &'a str, separators: &[char]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut last = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 && separators.contains(&c) => {
                out.push(&s[last..i]);
                last = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[last..]);
    out
}

impl ScopeSelectors {
    /// Checks if any of the given selectors match the given scope stack
    ///
    /// If so, it returns a match score. Higher match scores indicate stronger matches. Scores are
    /// ordered according to the rules found at [https://manual.macromates.com/en/scope_selectors](https://manual.macromates.com/en/scope_selectors).
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
        self.selectors
            .iter()
            .filter_map(|sel| sel.does_match(stack))
            .max()
    }
}

impl FromStr for ScopeSelectors {
    type Err = ParseScopeError;

    /// Parses a series of selectors separated by commas or pipes
    ///
    /// And `&`, and `( … )`. See `SelectorExpr` for the
    /// grammar and where its precedence comes from.
    fn from_str(s: &str) -> Result<ScopeSelectors, ParseScopeError> {
        Ok(ScopeSelectors {
            selectors: parse_selector_expr(s)?.into_dnf(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selectors_work() {
        use std::str::FromStr;
        let sels = ScopeSelectors::from_str(
            "source.php meta.preprocessor - string.quoted, \
                                             source string",
        )
        .unwrap();
        assert_eq!(sels.selectors.len(), 2);
        let first_sel = &sels.selectors[0];
        assert_eq!(format!("{:?}", first_sel),
                   "ScopeSelector { path: ScopeStack { clear_stack: [], scopes: [<source.php>, <meta.preprocessor>] }, excludes: [ScopeStack { clear_stack: [], scopes: [<string.quoted>] }], conjuncts: [] }");

        let sels = ScopeSelectors::from_str(
            "source.php meta.preprocessor -string.quoted|\
                                             source string",
        )
        .unwrap();
        assert_eq!(sels.selectors.len(), 2);
        let first_sel = &sels.selectors[0];
        assert_eq!(format!("{:?}", first_sel),
                   "ScopeSelector { path: ScopeStack { clear_stack: [], scopes: [<source.php>, <meta.preprocessor>] }, excludes: [ScopeStack { clear_stack: [], scopes: [<string.quoted>] }], conjuncts: [] }");

        let sels = ScopeSelectors::from_str(
            "text.xml meta.tag.preprocessor.xml punctuation.separator.key-value.xml",
        )
        .unwrap();
        assert_eq!(sels.selectors.len(), 1);
        let first_sel = &sels.selectors[0];
        assert_eq!(format!("{:?}", first_sel),
                   "ScopeSelector { path: ScopeStack { clear_stack: [], scopes: [<text.xml>, <meta.tag.preprocessor.xml>, <punctuation.separator.key-value.xml>] }, excludes: [], conjuncts: [] }");

        let sels = ScopeSelectors::from_str("text.xml meta.tag.preprocessor.xml punctuation.separator.key-value.xml - text.html - string")
            .unwrap();
        assert_eq!(sels.selectors.len(), 1);
        let first_sel = &sels.selectors[0];
        assert_eq!(format!("{:?}", first_sel),
                   "ScopeSelector { path: ScopeStack { clear_stack: [], scopes: [<text.xml>, <meta.tag.preprocessor.xml>, <punctuation.separator.key-value.xml>] }, excludes: [ScopeStack { clear_stack: [], scopes: [<text.html>] }, ScopeStack { clear_stack: [], scopes: [<string>] }], conjuncts: [] }");

        let sels = ScopeSelectors::from_str("text.xml meta.tag.preprocessor.xml punctuation.separator.key-value.xml - text.html - string, source - comment")
            .unwrap();
        assert_eq!(sels.selectors.len(), 2);
        let first_sel = &sels.selectors[0];
        assert_eq!(format!("{:?}", first_sel),
                   "ScopeSelector { path: ScopeStack { clear_stack: [], scopes: [<text.xml>, <meta.tag.preprocessor.xml>, <punctuation.separator.key-value.xml>] }, excludes: [ScopeStack { clear_stack: [], scopes: [<text.html>] }, ScopeStack { clear_stack: [], scopes: [<string>] }], conjuncts: [] }");
        let second_sel = &sels.selectors[1];
        assert_eq!(format!("{:?}", second_sel),
                   "ScopeSelector { path: ScopeStack { clear_stack: [], scopes: [<source>] }, excludes: [ScopeStack { clear_stack: [], scopes: [<comment>] }], conjuncts: [] }");

        let sels = ScopeSelectors::from_str(" -a.b|j.g").unwrap();
        assert_eq!(sels.selectors.len(), 2);
        let first_sel = &sels.selectors[0];
        assert_eq!(format!("{:?}", first_sel),
                   "ScopeSelector { path: ScopeStack { clear_stack: [], scopes: [] }, excludes: [ScopeStack { clear_stack: [], scopes: [<a.b>] }], conjuncts: [] }");
        let second_sel = &sels.selectors[1];
        assert_eq!(
            format!("{:?}", second_sel),
            "ScopeSelector { path: ScopeStack { clear_stack: [], scopes: [<j.g>] }, excludes: [], conjuncts: [] }"
        );
    }
    #[test]
    fn matching_works() {
        use crate::parsing::{MatchPower, ScopeStack};
        use std::str::FromStr;
        assert_eq!(
            ScopeSelectors::from_str("a.b, a e, e.f")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b e.f").unwrap().as_slice()),
            Some(MatchPower(0o20u64 as f64))
        );
        assert_eq!(
            ScopeSelectors::from_str("a.b, a e.f, e.f")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b e.f").unwrap().as_slice()),
            Some(MatchPower(0o21u64 as f64))
        );
        assert_eq!(
            ScopeSelectors::from_str("a.b, a e.f - c j, e.f")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            Some(MatchPower(0o2000u64 as f64))
        );
        assert_eq!(
            ScopeSelectors::from_str("a.b, a e.f - c j, e.f - a.b")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            Some(MatchPower(0o2u64 as f64))
        );
        assert_eq!(
            ScopeSelectors::from_str("a.b, a e.f - c k, e.f - a.b")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            Some(MatchPower(0o2001u64 as f64))
        );
        assert_eq!(
            ScopeSelectors::from_str("a.b|a e.f -d, e.f -a.b")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d e.f").unwrap().as_slice()),
            Some(MatchPower(0o201u64 as f64))
        );
    }

    /// The operators, and Sublime Text's own answers for them.
    ///
    /// `sublime.score_selector(scope_string, selector)` is the oracle: it takes
    /// a scope string and a selector and answers with an integer, 0 for no
    /// match and higher for a better one. Every number in the table below came
    /// from ST 4206 rather than from a reading of the documentation, which does
    /// not give the precedence at all.
    ///
    /// | scope | selector | ST |
    /// |---|---|---|
    /// | `source.c++` | `(source.c \| source.c++) & comment.block` | **0** |
    /// | `source.c++ comment.block` | the same | 32 |
    /// | `source.c++ comment.line` | the same | 0 |
    /// | `source.c` | `source.c \| source.c++ & comment.block` | 4 |
    /// | `source.c++ comment.block` | `source.c++ & comment.block - string.quoted` | 32 |
    /// | `source.c++ comment.block string.quoted` | the same | 0 |
    /// | `source.c++ comment.block` | `comment.block source.c++` | 0 |
    ///
    /// Three things follow, and each of them was a guess before it was asked:
    /// `&` is a conjunction and not a second way of writing a descendant path;
    /// `|` binds *looser* than `&`, which is why a bare `source.c` matches
    /// `source.c | source.c++ & comment.block`; and a parenthesis changes the
    /// answer, which is what says there is a grammar here rather than a split.
    ///
    /// The scores themselves are ST's scale and not `MatchPower`'s, so what is
    /// asserted is the *ordering* — which selector beats which — and never the
    /// number.
    #[test]
    fn conjunction_and_grouping_answer_as_sublime_text_does() {
        use std::str::FromStr;
        let matches = |scope: &str, selector: &str| -> bool {
            ScopeSelectors::from_str(selector)
                .unwrap()
                .does_match(ScopeStack::from_str(scope).unwrap().as_slice())
                .is_some()
        };

        // A conjunction is not satisfied by one of its halves.
        let both = "(source.c | source.c++) & comment.block";
        assert!(!matches("source.c++", both), "ST: 0");
        assert!(matches("source.c++ comment.block", both), "ST: 32");
        assert!(!matches("source.c++ comment.line", both), "ST: 0");
        assert!(!matches("source.c", both), "ST: 0");

        // Without the grouping, and in both orders.
        assert!(!matches("source.c++", "source.c++ & comment.block"));
        assert!(matches("source.c++ comment.block", "source.c++ & comment.block"));
        assert!(matches("source.c++ comment.block", "comment.block & source.c++"));

        // `|` binds looser than `&`: this is `a | (b & c)`, so `a` alone
        // matches. ST scores it 4.
        let mixed = "source.c | source.c++ & comment.block";
        assert!(matches("source.c", mixed));
        assert!(matches("source.c++ comment.block", mixed));

        // Exclusion applies to the conjunction as a whole.
        let minus = "source.c++ & comment.block - string.quoted";
        assert!(matches("source.c++ comment.block", minus), "ST: 32");
        assert!(!matches("source.c++ comment.block string.quoted", minus), "ST: 0");

        // A descendant path is ordered; a conjunction is not.
        assert!(matches("source.c++ comment.block", "source.c++ comment.block"));
        assert!(!matches("source.c++ comment.block", "comment.block source.c++"));

        // Three conjuncts and a group on the right, which is upstream's own
        // `Symbol List` selector shape.
        let three =
            "(source.c++ | source.objc++) & (meta.class | meta.struct) & meta.block entity.name";
        assert!(matches("source.c++ meta.class meta.block entity.name", three), "ST: 2304");
        assert!(!matches("source.c++ meta.enum meta.block entity.name", three), "ST: 0");

        // The score of a conjunction is the strongest of its operands, not
        // their sum: ST scores `source.c++` 4 and `comment.block` 32 on this
        // stack, the path `source.c++ comment.block` 36, and the conjunction
        // 32. So the conjunction ranks with `comment.block` and below the path.
        let stack = ScopeStack::from_str("source.c++ comment.block").unwrap();
        let power = |selector: &str| {
            ScopeSelectors::from_str(selector)
                .unwrap()
                .does_match(stack.as_slice())
                .unwrap()
        };
        assert_eq!(power("source.c++ & comment.block"), power("comment.block"));
        assert!(power("source.c++ comment.block") > power("source.c++ & comment.block"));
        assert!(power("source.c++ & comment.block") > power("source.c++"));
    }

    #[test]
    fn empty_stack_matching_works() {
        use crate::parsing::{MatchPower, ScopeStack};
        use std::str::FromStr;
        assert_eq!(
            ScopeSelector::from_str(" - a.b")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            None
        );
        assert_eq!(
            ScopeSelector::from_str("")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            Some(MatchPower(0o1u64 as f64))
        );
        assert_eq!(
            ScopeSelector::from_str("")
                .unwrap()
                .does_match(ScopeStack::from_str("").unwrap().as_slice()),
            Some(MatchPower(0o1u64 as f64))
        );
        assert_eq!(
            ScopeSelector::from_str(" - a.b")
                .unwrap()
                .does_match(ScopeStack::from_str("").unwrap().as_slice()),
            Some(MatchPower(0o1u64 as f64))
        );
        assert_eq!(
            ScopeSelector::from_str("a.b - ")
                .unwrap()
                .does_match(ScopeStack::from_str("").unwrap().as_slice()),
            None
        );
        assert_eq!(
            ScopeSelector::from_str("a.b - ")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            None
        );
        assert_eq!(
            ScopeSelector::from_str(" - ")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            None
        );
        assert_eq!(
            ScopeSelector::from_str(" - a.b")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            None
        );
        assert_eq!(
            ScopeSelector::from_str(" - g.h")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            Some(MatchPower(0o1u64 as f64))
        );

        assert_eq!(
            ScopeSelector::from_str(" -a.b")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            None
        );
        assert_eq!(
            ScopeSelector::from_str("")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            Some(MatchPower(0o1u64 as f64))
        );
        assert_eq!(
            ScopeSelector::from_str(" -a.b")
                .unwrap()
                .does_match(ScopeStack::from_str("").unwrap().as_slice()),
            Some(MatchPower(0o1u64 as f64))
        );
        assert_eq!(
            ScopeSelector::from_str("a.b -")
                .unwrap()
                .does_match(ScopeStack::from_str("").unwrap().as_slice()),
            None
        );
        assert_eq!(
            ScopeSelector::from_str("a.b -")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            None
        );
        assert_eq!(
            ScopeSelector::from_str(" -")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            None
        );
        assert_eq!(
            ScopeSelector::from_str(" -a.b")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            None
        );
        assert_eq!(
            ScopeSelector::from_str(" -g.h")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            Some(MatchPower(0o1u64 as f64))
        );
    }

    #[test]
    fn multiple_excludes_matching_works() {
        use crate::parsing::{MatchPower, ScopeStack};
        use std::str::FromStr;
        assert_eq!(
            ScopeSelector::from_str(" - a.b - c.d")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            None
        );
        assert_eq!(
            ScopeSelector::from_str(" - a.b - c.d")
                .unwrap()
                .does_match(ScopeStack::from_str("").unwrap().as_slice()),
            Some(MatchPower(0o1u64 as f64))
        );
        assert_eq!(
            ScopeSelector::from_str("a.b - c.d -e.f")
                .unwrap()
                .does_match(ScopeStack::from_str("").unwrap().as_slice()),
            None
        );
        assert_eq!(
            ScopeSelector::from_str("a.b - c.d -")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            None
        );
        assert_eq!(
            ScopeSelector::from_str(" -g.h - h.i")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            Some(MatchPower(0o1u64 as f64))
        );
        assert_eq!(
            ScopeSelector::from_str("a.b")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            Some(MatchPower(0o2u64 as f64))
        );
        assert_eq!(
            ScopeSelector::from_str("a.b -g.h - h.i")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            Some(MatchPower(0o2u64 as f64))
        );
        assert_eq!(
            ScopeSelector::from_str("c.d")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            Some(MatchPower(0o20u64 as f64))
        );
        assert_eq!(
            ScopeSelector::from_str("c.d - j.g - h.i")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            Some(MatchPower(0o20u64 as f64))
        );
        assert_eq!(
            ScopeSelectors::from_str("j.g| -a.b")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            None
        );
        assert_eq!(
            ScopeSelectors::from_str(" -a.b|j.g")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            None
        );
        assert_eq!(
            ScopeSelectors::from_str(" -a.b,c.d - j.g - h.i")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            Some(MatchPower(0o20u64 as f64))
        );
        assert_eq!(
            ScopeSelectors::from_str(" -a.b, -d.c -f.e")
                .unwrap()
                .does_match(ScopeStack::from_str("a.b c.d j e.f").unwrap().as_slice()),
            Some(MatchPower(0o01u64 as f64))
        );
    }
}
