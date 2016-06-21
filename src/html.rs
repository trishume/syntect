//! Rendering highlighted code as HTML+CSS
use std::fmt::Write;
use parsing::{ScopeStackOp, Scope, SCOPE_REPO};
use highlighting::{Style, self};

/// Only one style for now, I may add more class styles later.
/// Just here so I don't have to change the API
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ClassStyle {
    /// The classes are the atoms of the scope separated by spaces
    /// (e.g `source.php` becomes `source php`).
    /// This isn't that fast since it has to use the scope repository
    /// to look up scope names.
    Spaced,
}

fn scope_to_classes(s: &mut String, scope: Scope, style: ClassStyle) {
    assert!(style == ClassStyle::Spaced); // TODO more styles
    let repo = SCOPE_REPO.lock().unwrap();
    for i in 0..(scope.len()) {
        let atom = scope.atom_at(i as usize);
        let atom_s = repo.atom_str(atom);
        if i != 0 {
            s.push_str(" ")
        }
        s.push_str(atom_s);
    }
}

/// Output HTML for a line of code with `<span>` elements
/// specifying classes for each token. The span elements are nested
/// like the scope stack and the scopes are mapped to classes based
/// on the `ClassStyle` (see it's docs).
pub fn tokens_to_classed_html(line: &str, ops: &[(usize, ScopeStackOp)], style: ClassStyle) -> String {
    let mut s = String::with_capacity(line.len()+ops.len()*8); // a guess
    let mut cur_index = 0;
    for &(i, ref op) in ops {
        if i > cur_index {
            s.push_str(&line[cur_index..i]);
            cur_index = i
        }
        match op {
            &ScopeStackOp::Push(scope) => {
                s.push_str("<span class=\"");
                scope_to_classes(&mut s, scope, style);
                s.push_str("\">");
            },
            &ScopeStackOp::Pop(n) => {
                for _ in 0..n {
                    s.push_str("</span>");
                }
            },
            &ScopeStackOp::Noop => panic!("ops shouldn't have no-ops")
        }
    }
    s
}

/// Output HTML for a line of code with `<span>` elements using inline
/// `style` attributes to set the correct font attributes.
/// The `bg` attribute determines if the spans will have the `background-color`
/// attribute set. This adds a lot more text but allows different backgrounds.
pub fn styles_to_coloured_html(v: &[(Style, &str)], bg: bool) -> String {
    let mut s: String = String::new();
    for &(ref style, text) in v.iter() {
        write!(s,"<span style=\"").unwrap();
        if bg {
            write!(s,
                   "background-color:#{:02x}{:02x}{:02x};",
                   style.background.r,
                   style.background.g,
                   style.background.b)
                .unwrap();
        }
        if style.font_style.contains(highlighting::FONT_STYLE_UNDERLINE) {
            write!(s, "text-decoration:underline;").unwrap();
        }
        if style.font_style.contains(highlighting::FONT_STYLE_BOLD) {
            write!(s, "font-weight:bold;").unwrap();
        }
        if style.font_style.contains(highlighting::FONT_STYLE_ITALIC) {
            write!(s, "font-style:italic;").unwrap();
        }
        write!(s,
               "color:#{:02x}{:02x}{:02x};\">{}</span>",
               style.foreground.r,
               style.foreground.g,
               style.foreground.b,
               text)
            .unwrap();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsing::{SyntaxSet, ParseState, ScopeStack};
    use highlighting::{ThemeSet, Style, Highlighter, HighlightIterator, HighlightState};
    #[test]
    fn tokens() {
        let ps = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
        let syntax = ps.find_syntax_by_name("Markdown").unwrap();
        let mut state = ParseState::new(syntax);
        let line = "[w](t.co) *hi* **five**";
        let ops = state.parse_line(line);

        // use util::debug_print_ops;
        // debug_print_ops(line, &ops);

        let html = tokens_to_classed_html(line, &ops[..], ClassStyle::Spaced);
        assert_eq!(html, include_str!("../testdata/test2.html").trim_right());

        let ts = ThemeSet::load_defaults();
        let highlighter = Highlighter::new(&ts.themes["InspiredGitHub"]);
        let mut highlight_state = HighlightState::new(&highlighter, ScopeStack::new());
        let iter = HighlightIterator::new(&mut highlight_state, &ops[..], line, &highlighter);
        let regions: Vec<(Style, &str)> = iter.collect();

        let html2 = styles_to_coloured_html(&regions[..], true);
        assert_eq!(html2, include_str!("../testdata/test1.html").trim_right());
    }
}
