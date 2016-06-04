/// Code based on https://github.com/defuz/sublimate/blob/master/src/core/syntax/highlighter.rs
/// released under the MIT license by @defuz

use std::iter::Iterator;

use scope::{Scope, ScopeStack, ScopeStackOp};
use theme::theme::Theme;
use theme::style::{Style, StyleModifier, FontStyle, BLACK, WHITE};

#[derive(Debug)]
pub struct Highlighter {
    theme: Theme,
    // TODO add caching or accelerator structure
}

pub struct HighlightIterator<'a> {
    index: usize,
    pos: usize,
    path: ScopeStack,
    changes: &'a [(usize, ScopeStackOp)],
    text: &'a str,
    highlighter: &'a Highlighter,
    styles: Vec<Style>
}

impl<'a> HighlightIterator<'a> {
    pub fn new(path: ScopeStack,
           changes: &'a [(usize, ScopeStackOp)],
           text: &'a str,
           highlighter: &'a Highlighter) -> HighlightIterator<'a> {

        let style = highlighter.get_default();
        for i in 1..path.len() {
            style.apply(highlighter.get_style(path.bottom_n(i)));
        }

        HighlightIterator {
            index: 0,
            pos: 0,
            path: path,
            changes: changes,
            text: text,
            highlighter: highlighter,
            styles: vec![style]
        }
    }
}

impl<'a> Iterator for HighlightIterator<'a> {
    type Item = (Style, &'a str);

    fn next(&mut self) -> Option<(Style, &'a str)> {
        if self.pos == self.text.len() {
            return None
        }
        let (end, command) = if self.index < self.changes.len() {
            self.changes[self.index].clone()
        } else {
            (self.text.len(), ScopeStackOp::Noop)
        };
        let style = self.styles.last().unwrap().clone();
        let text = &self.text[self.pos..end];
        match command {
            ScopeStackOp::Push(scope) => {
                self.path.push(scope);
                self.styles.push(style.apply(self.highlighter.get_style(self.path.as_slice())));
            },
            ScopeStackOp::Pop(n) => {
                for _ in 0..n {
                    self.path.pop();
                    self.styles.pop();
                }
            }
            ScopeStackOp::Noop => ()
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
        Highlighter {theme: theme}
    }

    pub fn get_default(&self) -> Style {
        Style {
            foreground: self.theme.settings.foreground.unwrap_or(WHITE),
            background: self.theme.settings.background.unwrap_or(BLACK),
            font_style: FontStyle::empty()
        }
    }

    pub fn get_style(&self, path: &[Scope]) -> StyleModifier {
        let max_item = self.theme.scopes.iter()
            .filter_map(|item| item.scope.does_match(path)
                .map(|score| (score, item)))
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
        let highlighter = Highlighter::new(PackageSet::get_theme("testdata/themes.tmbundle/Themes/Amy.tmTheme").unwrap());

        // TODO having to clone this isn't nice
        let start_stack = state.scope_stack.clone();
        let line = "module Bob::Wow::Troll::Five; 5; end";
        let ops = state.parse_line(line);
        let iter = HighlightIterator::new(start_stack, &ops[..], line, &highlighter);
        let regions: Vec<(Style, &str)> = iter.collect();
        println!("{:#?}", regions);
        assert_eq!(regions[11], (Style {
            foreground: Color { r: 0x70, g: 0x90, b: 0xB0, a: 0xFF},
            background: Color { r: 0x20, g: 0x00, b: 0x20, a: 0xFF},
            font_style: FontStyle::empty(),
        }, "5"));

        // assert!(false);
    }
}
