//! Iterators and data structures for transforming parsing information into styled text.

// Code based on https://github.com/defuz/sublimate/blob/master/src/core/syntax/highlighter.rs
// released under the MIT license by @defuz

use std::iter::Iterator;

use parsing::{Scope, ScopeStack, ScopeStackOp};
use super::theme::Theme;
use super::style::{Style, StyleModifier, FontStyle, BLACK, WHITE};

/// Basically a wrapper around a `Theme` preparing it to be used for highlighting.
/// This is part of the API to preserve the possibility of caching
/// matches of the selectors of the theme on various scope paths
/// or setting up some kind of accelerator structure.
///
/// So for now this does very little but eventually if you keep it around between
/// highlighting runs it will preserve its cache.
#[derive(Debug)]
pub struct Highlighter<'a> {
    theme: &'a Theme, // TODO add caching or accelerator structure
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
/// then re-create the HighlightState when needed by passing that stack as the `initial_stack`
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
            let style = initial_styles[i];
            style.apply(highlighter.get_style(initial_stack.bottom_n(i)));
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
        // println!("{} - {:?}", self.index, self.pos);
        let style = self.state.styles.last().unwrap().clone();
        let text = &self.text[self.pos..end];
        match command {
            ScopeStackOp::Push(scope) => {
                self.state.path.push(scope);
                self.state
                    .styles
                    .push(style.apply(self.highlighter.get_style(self.state.path.as_slice())));
            }
            ScopeStackOp::Pop(n) => {
                for _ in 0..n {
                    self.state.path.pop();
                    self.state.styles.pop();
                }
            }
            ScopeStackOp::Noop => (),
        };
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
        Highlighter { theme: theme }
    }

    /// The default style in the absence of any matched rules.
    /// Basically what plain text gets highlighted as.
    pub fn get_default(&self) -> Style {
        Style {
            foreground: self.theme.settings.foreground.unwrap_or(BLACK),
            background: self.theme.settings.background.unwrap_or(WHITE),
            font_style: FontStyle::empty(),
        }
    }

    /// Figures out which scope selector in the theme best matches this scope stack.
    /// It only returns any changes to the style that should be applied when the top element
    /// is pushed on to the stack. These actually aren't guaranteed to be different than the current
    /// style. Basically what this means is that you have to gradually apply styles starting with the
    /// default and working your way up the stack in order to get the correct style.
    ///
    /// Don't worry if this sounds complex, you shouldn't need to use this method.
    /// It's only public because I default to making things public for power users unless
    /// I have a good argument nobody will ever need to use the method.
    pub fn get_style(&self, path: &[Scope]) -> StyleModifier {
        let max_item = self.theme
            .scopes
            .iter()
            .filter_map(|item| {
                item.scope
                    .does_match(path)
                    .map(|score| (score, item))
            })
            .max_by_key(|&(score, _)| score)
            .map(|(_, item)| item);
        StyleModifier {
            foreground: max_item.and_then(|item| item.style.foreground),
            background: max_item.and_then(|item| item.style.background),
            font_style: max_item.and_then(|item| item.style.font_style),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsing::{SyntaxSet, ScopeStack, ParseState};
    use highlighting::{ThemeSet, Style, Color, FontStyle};

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
        let ops = state.parse_line(line);
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

        // assert!(false);
    }
}
