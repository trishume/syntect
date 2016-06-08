/// Code based on https://github.com/defuz/sublimate/blob/master/src/core/syntax/highlighter.rs
/// released under the MIT license by @defuz

use std::iter::Iterator;

use scope::{Scope, ScopeStack, ScopeStackOp};
use theme::theme::Theme;
use theme::style::{Style, StyleModifier, FontStyle, BLACK, WHITE};

#[derive(Debug)]
pub struct Highlighter {
    theme: Theme, // TODO add caching or accelerator structure
}

#[derive(Debug, Clone)]
pub struct HighlightState {
    styles: Vec<Style>,
    path: ScopeStack,
}

#[derive(Debug)]
pub struct HighlightIterator<'a> {
    index: usize,
    pos: usize,
    changes: &'a [(usize, ScopeStackOp)],
    text: &'a str,
    highlighter: &'a Highlighter,
    state: &'a mut HighlightState,
}

impl HighlightState {
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

impl<'a> HighlightIterator<'a> {
    pub fn new(state: &'a mut HighlightState,
               changes: &'a [(usize, ScopeStackOp)],
               text: &'a str,
               highlighter: &'a Highlighter)
               -> HighlightIterator<'a> {
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

impl<'a> Iterator for HighlightIterator<'a> {
    type Item = (Style, &'a str);

    fn next(&mut self) -> Option<(Style, &'a str)> {
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

impl Highlighter {
    pub fn new(theme: Theme) -> Highlighter {
        Highlighter { theme: theme }
    }

    pub fn get_default(&self) -> Style {
        Style {
            foreground: self.theme.settings.foreground.unwrap_or(WHITE),
            background: self.theme.settings.background.unwrap_or(BLACK),
            font_style: FontStyle::empty(),
        }
    }

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
    use package_set::PackageSet;
    use scope::ScopeStack;
    use parser::*;
    use theme::highlighter::*;
    use theme::style::*;

    #[test]
    fn can_parse() {
        let ps = PackageSet::load_from_folder("testdata/Packages").unwrap();
        let mut state = {
            let syntax = ps.find_syntax_by_name("Ruby on Rails").unwrap();
            ParseState::new(syntax)
        };
        let highlighter = Highlighter::new(PackageSet::get_theme("testdata/themes.\
                                                                  tmbundle/Themes/Amy.tmTheme")
            .unwrap());

        let mut highlight_state = HighlightState::new(&highlighter, ScopeStack::new());
        let line = "module Bob::Wow::Troll::Five; 5; end";
        let ops = state.parse_line(line);
        let iter = HighlightIterator::new(&mut highlight_state, &ops[..], line, &highlighter);
        let regions: Vec<(Style, &str)> = iter.collect();
        println!("{:#?}", regions);
        assert_eq!(regions[11],
                   (Style {
                       foreground: Color {
                           r: 0x70,
                           g: 0x90,
                           b: 0xB0,
                           a: 0xFF,
                       },
                       background: Color {
                           r: 0x20,
                           g: 0x00,
                           b: 0x20,
                           a: 0xFF,
                       },
                       font_style: FontStyle::empty(),
                   },
                    "5"));

        // assert!(false);
    }
}
