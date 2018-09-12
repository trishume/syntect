//! Iterators and data structures for transforming parsing information into styled text.

// Code based on https://github.com/defuz/sublimate/blob/master/src/core/syntax/highlighter.rs
// released under the MIT license by @defuz

use std::iter::Iterator;

use parsing::{Scope, ScopeStack, BasicScopeStackOp, ScopeStackOp, MatchPower, ATOM_LEN_BITS};
use super::selector::ScopeSelector;
use super::theme::{Theme, ThemeItem};
use super::style::{Color, FontStyle, Style, StyleModifier};

/// Basically a wrapper around a `Theme` preparing it to be used for highlighting.
/// This is part of the API to preserve the possibility of caching
/// matches of the selectors of the theme on various scope paths
/// or setting up some kind of accelerator structure.
///
/// So for now this does very little but eventually if you keep it around between
/// highlighting runs it will preserve its cache.
#[derive(Debug)]
pub struct Highlighter<'a> {
    theme: &'a Theme,
    /// Cache of the selectors in the theme that are only one scope
    /// In most themes this is the majority, hence the usefullness
    single_selectors: Vec<(Scope, StyleModifier)>,
    multi_selectors: Vec<(ScopeSelector, StyleModifier)>,
    // TODO single_cache: HashMap<Scope, StyleModifier, BuildHasherDefault<FnvHasher>>,
}

/// Keeps a stack of scopes and styles as state between highlighting different lines.
/// If you are highlighting an entire file you create one of these at the start and use it
/// all the way to the end.
///
/// # Caching
///
/// One reason this is exposed is that since it implements `Clone` you can actually cache
/// these (probably along with a `ParseState`) and only re-start highlighting from the point of a change.
/// You could also do something fancy like only highlight a bit past the end of a user's screen and resume
/// highlighting when they scroll down on large files.
///
/// Alternatively you can save space by caching only the `path` field of this struct
/// then re-create the `HighlightState` when needed by passing that stack as the `initial_stack`
/// parameter to the `new` method. This takes less space but a small amount of time to re-create the style stack.
///
/// **Note:** Caching is for advanced users who have tons of time to maximize performance or want to do so eventually.
/// It is not recommended that you try caching the first time you implement highlighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightState {
    styles: Vec<Style>,
    pub path: ScopeStack,
}

/// Highlights a line of parsed code given a `HighlightState`
/// and line of changes from the parser.
///
/// It splits a line of text into different pieces each with a `Style`
#[derive(Debug)]
pub struct HighlightIterator<'a, 'b> {
    index: usize,
    pos: usize,
    changes: &'a [(usize, ScopeStackOp)],
    text: &'b str,
    highlighter: &'a Highlighter<'a>,
    state: &'a mut HighlightState,
}

impl HighlightState {
    /// Note that the `Highlighter` is not stored, it is used to construct the initial
    /// stack of styles. Most of the time you'll want to pass an empty stack as `initial_stack`
    /// but see the docs for `HighlightState` for discussion of advanced caching use cases.
    pub fn new(highlighter: &Highlighter, initial_stack: ScopeStack) -> HighlightState {
        let mut initial_styles = vec![highlighter.get_default()];
        for i in 0..initial_stack.len() {
            let style = highlighter.style_for_stack(initial_stack.bottom_n(i));
            initial_styles.push(style);
        }

        HighlightState {
            styles: initial_styles,
            path: initial_stack,
        }
    }
}

impl<'a, 'b> HighlightIterator<'a, 'b> {
    pub fn new(state: &'a mut HighlightState,
               changes: &'a [(usize, ScopeStackOp)],
               text: &'b str,
               highlighter: &'a Highlighter)
               -> HighlightIterator<'a, 'b> {
        HighlightIterator {
            index: 0,
            pos: 0,
            changes: changes,
            text: text,
            highlighter: highlighter,
            state: state,
        }
    }
}

impl<'a, 'b> Iterator for HighlightIterator<'a, 'b> {
    type Item = (Style, &'b str);

    /// Yields the next token of text and the associated `Style` to render that text with.
    /// the concatenation of the strings in each token will make the original string.
    fn next(&mut self) -> Option<(Style, &'b str)> {
        if self.pos == self.text.len() && self.index >= self.changes.len() {
            return None;
        }
        let (end, command) = if self.index < self.changes.len() {
            self.changes[self.index].clone()
        } else {
            (self.text.len(), ScopeStackOp::Noop)
        };
        // println!("{} - {:?}   {}:{}", self.index, self.pos, self.state.path.len(), self.state.styles.len());
        let style = *self.state.styles.last().unwrap();
        let text = &self.text[self.pos..end];
        {
            // closures mess with the borrow checker's ability to see different struct fields
            let m_path = &mut self.state.path;
            let m_styles = &mut self.state.styles;
            let highlighter = &self.highlighter;
            m_path.apply_with_hook(&command, |op, cur_stack| {
                // println!("{:?} - {:?}", op, cur_stack);
                match op {
                    BasicScopeStackOp::Push(_) => {
                        m_styles.push(highlighter.style_for_stack(cur_stack));
                    }
                    BasicScopeStackOp::Pop => {
                        m_styles.pop();
                    }
                }
            });
        }
        self.pos = end;
        self.index += 1;
        if text.is_empty() {
            self.next()
        } else {
            Some((style, text))
        }
    }
}

impl<'a> Highlighter<'a> {
    pub fn new(theme: &'a Theme) -> Highlighter<'a> {
        let mut single_selectors = Vec::new();
        let mut multi_selectors = Vec::new();
        for item in &theme.scopes {
            for sel in &item.scope.selectors {
                if let Some(scope) = sel.extract_single_scope() {
                    single_selectors.push((scope, item.style));
                } else {
                    multi_selectors.push((sel.clone(), item.style));
                }
            }
        }
        // So that deeper matching selectors get checked first
        single_selectors.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

        Highlighter {
            theme: theme,
            single_selectors: single_selectors,
            multi_selectors: multi_selectors,
        }
    }

    /// The default style in the absence of any matched rules.
    /// Basically what plain text gets highlighted as.
    pub fn get_default(&self) -> Style {
        Style {
            foreground: self.theme.settings.foreground.unwrap_or(Color::BLACK),
            background: self.theme.settings.background.unwrap_or(Color::WHITE),
            font_style: FontStyle::empty(),
        }
    }

    /// Figures out which scope selectors in the color scheme match this scope stack.
    /// Then, collects the style modifications in score order, to return a modifier
    /// that encompasses all matching rules from the color scheme.
    pub fn get_style(&self, path: &[Scope]) -> StyleModifier {
        let mut matching_items : Vec<(MatchPower, &ThemeItem)> = self.theme
            .scopes
            .iter()
            .filter_map(|item| {
                item.scope
                    .does_match(path)
                    .map(|score| (score, item))
            })
            .collect();
        matching_items.sort_by_key(|&(score, _)| score);
        let sorted = matching_items.iter()
            .map(|(_, item)| item);
        // let mut modifier = sorted.next();
        let mut modifier = StyleModifier {
            background: None,
            foreground: None,
            font_style: None,
        };
        for item in sorted {
            modifier = modifier.apply(item.style);
        }
        return modifier;
    }

    /// Returns the fully resolved style for the given stack.
    ///
    /// This operation is convenient but expensive. For reasonable performance,
    /// the caller should be caching results.
    pub fn style_for_stack(&self, stack: &[Scope]) -> Style {
        let mut style = self.get_default();
        style = style.apply(self.get_style(&stack));
        style
    }

    /// Returns a `StyleModifier` which, if applied to the default style,
    /// would generate the fully resolved style for this stack.
    ///
    /// This is made available to applications that are using syntect styles
    /// in combination with style information from other sources.
    ///
    /// This operation is convenient but expensive. For reasonable performance,
    /// the caller should be caching results.
    pub fn style_mod_for_stack(&self, stack: &[Scope]) -> StyleModifier {
        let mut style_mod = StyleModifier::default();
        style_mod = style_mod.apply(self.get_style(&stack));
        style_mod
    }
}

#[cfg(all(feature = "assets", feature = "parsing", any(feature = "dump-load", feature = "dump-load-rs")))]
#[cfg(test)]
mod tests {
    use super::*;
    use highlighting::{ThemeSet, Style, Color, FontStyle};
    use parsing::{ SyntaxSet, ScopeStack, ParseState};

    #[test]
    fn can_parse() {
        let ps = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        let mut state = {
            let syntax = ps.find_syntax_by_name("Ruby on Rails").unwrap();
            ParseState::new(syntax)
        };
        let ts = ThemeSet::load_defaults();
        let highlighter = Highlighter::new(&ts.themes["base16-ocean.dark"]);

        let mut highlight_state = HighlightState::new(&highlighter, ScopeStack::new());
        let line = "module Bob::Wow::Troll::Five; 5; end";
        let ops = state.parse_line(line, &ps);
        let iter = HighlightIterator::new(&mut highlight_state, &ops[..], line, &highlighter);
        let regions: Vec<(Style, &str)> = iter.collect();
        // println!("{:#?}", regions);
        assert_eq!(regions[11],
                   (Style {
                       foreground: Color {
                           r: 208,
                           g: 135,
                           b: 112,
                           a: 0xFF,
                       },
                       background: Color {
                           r: 43,
                           g: 48,
                           b: 59,
                           a: 0xFF,
                       },
                       font_style: FontStyle::empty(),
                   },
                    "5"));
    }
    
    #[test]
    fn can_parse_incremental_styles() {
        use parsing::ScopeStack;
        use std::str::FromStr;
        use highlighting::{ThemeSettings, ScopeSelectors};

        let test_color_scheme = Theme {
            name: None,
            author: None,
            settings: ThemeSettings::default(),
            scopes: vec![
                ThemeItem {
                    scope: ScopeSelectors::from_str("comment.line").unwrap(),
                    style: StyleModifier {
                        foreground: None,
                        background: Some(Color { r: 64, g: 255, b: 64, a: 255 }),
                        font_style: None,
                    },
                },
                ThemeItem {
                    scope: ScopeSelectors::from_str("comment").unwrap(),
                    style: StyleModifier {
                        foreground: Some(Color { r: 255, g: 0, b: 0, a: 255 }),
                        background: None,
                        font_style: Some(FontStyle::ITALIC),
                    },
                },
                ThemeItem {
                    scope: ScopeSelectors::from_str("comment.line.rs").unwrap(),
                    style: StyleModifier {
                        foreground: None,
                        background: None,
                        font_style: Some(FontStyle::BOLD),
                    },
                },
                ThemeItem {
                    scope: ScopeSelectors::from_str("no.match").unwrap(),
                    style: StyleModifier {
                        foreground: None,
                        background: Some(Color { r: 255, g: 255, b: 255, a: 255 }),
                        font_style: Some(FontStyle::UNDERLINE),
                    },
                },
            ],
        };
        let highlighter = Highlighter::new(&test_color_scheme);

        let scope_stack = ScopeStack::from_str("comment.line.rs").unwrap();
        let style = highlighter.style_for_stack(&scope_stack.as_slice());
        assert_eq!(style, Style {
                       foreground: Color {
                           r: 255,
                           g: 0,
                           b: 0,
                           a: 0xFF,
                       },
                       background: Color {
                           r: 64,
                           g: 255,
                           b: 64,
                           a: 0xFF,
                       },
                       font_style: FontStyle::BOLD,
                   });
    }
}
