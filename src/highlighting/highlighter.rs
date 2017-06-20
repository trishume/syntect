//! Iterators and data structures for transforming parsing information into styled text.

// Code based on https://github.com/defuz/sublimate/blob/master/src/core/syntax/highlighter.rs
// released under the MIT license by @defuz

use std::iter::Iterator;

use parsing::{Scope, ScopeStack, BasicScopeStackOp, ScopeStackOp, MatchPower, ATOM_LEN_BITS};
use super::selector::ScopeSelector;
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
                        // we can push multiple times so this might have changed
                        let style = *m_styles.last().unwrap();
                        m_styles.push(style.apply(highlighter.get_new_style(cur_stack)));
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

    /// Like get_style but only guarantees returning any new style
    /// if the last element of `path` was just pushed on to the stack.
    /// Panics if `path` is empty.
    pub fn get_new_style(&self, path: &[Scope]) -> StyleModifier {
        let last_scope = path[path.len() - 1];
        let single_res = self.single_selectors
            .iter()
            .find(|a| a.0.is_prefix_of(last_scope));
        let mult_res = self.multi_selectors
            .iter()
            .filter_map(|&(ref sel, ref style)| sel.does_match(path).map(|score| (score, style)))
            .max_by_key(|&(score, _)| score);
        // println!("{:?}", single_res);
        if let Some((score, style)) = mult_res {
            let mut single_score: f64 = -1.0;
            if let Some(&(scope, _)) = single_res {
                single_score = (scope.len() as f64) *
                               ((ATOM_LEN_BITS * ((path.len() - 1) as u16)) as f64).exp2();
            }
            // println!("multi at {:?} score {:?} single score {:?}", path, score, single_score);
            if MatchPower(single_score) < score {
                return *style;
            }
        }
        if let Some(&(_, ref style)) = single_res {
            return *style;
        }
        StyleModifier {
            foreground: None,
            background: None,
            font_style: None,
        }
    }

    /// Returns the fully resolved style for the given stack.
    ///
    /// This operation is convenient but expensive. For reasonable performance,
    /// the caller should be caching results.
    pub fn style_for_stack(&self, stack: &[Scope]) -> Style {
        let mut style = self.get_default();
        for i in 0..stack.len() {
            let style_mod = self.get_style(&stack[0..i+1]);
            style = style.apply(style_mod);
        }
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
        for i in 0..stack.len() {
            let next_mod = self.get_style(&stack[0..i+1]);
            style_mod = style_mod.apply(next_mod);
        }
        style_mod
    }
}

#[cfg(feature = "assets")]
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
    }
}
