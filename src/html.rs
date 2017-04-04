//! Rendering highlighted code as HTML+CSS
use std::fmt::Write;
use parsing::{ScopeStackOp, BasicScopeStackOp, Scope, ScopeStack, SyntaxDefinition, SyntaxSet, SCOPE_REPO};
use easy::{HighlightLines, HighlightFile};
use highlighting::{self, Style, Theme, Color};
use escape::Escape;
use std::io::{self, BufRead};
use std::path::Path;

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

/// Convenience method that combines `start_coloured_html_snippet`, `styles_to_coloured_html`
/// and `HighlightLines` from `syntect::easy` to create a full highlighted HTML snippet for
/// a string (which can contain many lines).
///
/// Note that the `syntax` passed in must be from a `SyntaxSet` compiled for no newline characters.
/// This is easy to get with `SyntaxSet::load_defaults_nonewlines()`. If you think this is the wrong
/// choice of `SyntaxSet` to accept, I'm not sure of it either, email me.
pub fn highlighted_snippet_for_string(s: &str, syntax: &SyntaxDefinition, theme: &Theme) -> String {
    let mut output = String::new();
    let mut highlighter = HighlightLines::new(syntax, theme);
    let c = theme.settings.background.unwrap_or(highlighting::WHITE);
    write!(output,
           "<pre style=\"background-color:#{:02x}{:02x}{:02x};\">\n",
           c.r,
           c.g,
           c.b)
        .unwrap();
    for line in s.lines() {
        let regions = highlighter.highlight(line);
        let html = styles_to_coloured_html(&regions[..], IncludeBackground::IfDifferent(c));
        output.push_str(&html);
        output.push('\n');
    }
    output.push_str("</pre>\n");
    output
}

/// Convenience method that combines `start_coloured_html_snippet`, `styles_to_coloured_html`
/// and `HighlightFile` from `syntect::easy` to create a full highlighted HTML snippet for
/// a file.
///
/// Note that the `syntax` passed in must be from a `SyntaxSet` compiled for no newline characters.
/// This is easy to get with `SyntaxSet::load_defaults_nonewlines()`. If you think this is the wrong
/// choice of `SyntaxSet` to accept, I'm not sure of it either, email me.
pub fn highlighted_snippet_for_file<P: AsRef<Path>>(path: P,
                                                    ss: &SyntaxSet,
                                                    theme: &Theme)
                                                    -> io::Result<String> {
    // TODO reduce code duplication with highlighted_snippet_for_string
    let mut output = String::new();
    let mut highlighter = try!(HighlightFile::new(path, ss, theme));
    let c = theme.settings.background.unwrap_or(highlighting::WHITE);
    write!(output,
           "<pre style=\"background-color:#{:02x}{:02x}{:02x};\">\n",
           c.r,
           c.g,
           c.b)
        .unwrap();
    for maybe_line in highlighter.reader.lines() {
        let line = try!(maybe_line);
        let regions = highlighter.highlight_lines.highlight(&line);
        let html = styles_to_coloured_html(&regions[..], IncludeBackground::IfDifferent(c));
        output.push_str(&html);
        output.push('\n');
    }
    output.push_str("</pre>\n");
    Ok(output)
}

/// Output HTML for a line of code with `<span>` elements
/// specifying classes for each token. The span elements are nested
/// like the scope stack and the scopes are mapped to classes based
/// on the `ClassStyle` (see it's docs).
///
/// For this to work correctly you must concatenate all the lines in a `<pre>`
/// tag since some span tags opened on a line may not be closed on that line
/// and later lines may close tags from previous lines.
pub fn tokens_to_classed_html(line: &str,
                              ops: &[(usize, ScopeStackOp)],
                              style: ClassStyle)
                              -> String {
    let mut s = String::with_capacity(line.len() + ops.len() * 8); // a guess
    let mut cur_index = 0;
    let mut stack = ScopeStack::new();
    for &(i, ref op) in ops {
        if i > cur_index {
            write!(s, "{}", Escape(&line[cur_index..i])).unwrap();
            cur_index = i
        }
        stack.apply_with_hook(op, |basic_op, _| {
            match basic_op {
                BasicScopeStackOp::Push(scope) => {
                    s.push_str("<span class=\"");
                    scope_to_classes(&mut s, scope, style);
                    s.push_str("\">");
                }
                BasicScopeStackOp::Pop => {
                    s.push_str("</span>");
                }
            }
        });
    }
    s
}

/// Determines how background colour attributes are generated
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum IncludeBackground {
    /// Don't include `background-color`, for performance or so that you can use your own background.
    No,
    /// Set background colour attributes on every node
    Yes,
    /// Only set the `background-color` if it is different than the default (presumably set on a parent element)
    IfDifferent(Color),
}

/// Output HTML for a line of code with `<span>` elements using inline
/// `style` attributes to set the correct font attributes.
/// The `bg` attribute determines if the spans will have the `background-color`
/// attribute set. See the `IncludeBackground` enum's docs.
///
/// The lines returned don't include a newline at the end.
/// # Examples
///
/// ```
/// use syntect::easy::HighlightLines;
/// use syntect::parsing::SyntaxSet;
/// use syntect::highlighting::{ThemeSet, Style};
/// use syntect::html::{styles_to_coloured_html, IncludeBackground};
///
/// // Load these once at the start of your program
/// let ps = SyntaxSet::load_defaults_nonewlines();
/// let ts = ThemeSet::load_defaults();
///
/// let syntax = ps.find_syntax_by_name("Ruby").unwrap();
/// let mut h = HighlightLines::new(syntax, &ts.themes["base16-ocean.dark"]);
/// let regions = h.highlight("5");
/// let html = styles_to_coloured_html(&regions[..], IncludeBackground::No);
/// assert_eq!(html, "<span style=\"color:#d08770;\">5</span>");
/// ```
pub fn styles_to_coloured_html(v: &[(Style, &str)], bg: IncludeBackground) -> String {
    let mut s: String = String::new();
    for &(ref style, text) in v.iter() {
        write!(s, "<span style=\"").unwrap();
        let include_bg = match bg {
            IncludeBackground::Yes => true,
            IncludeBackground::No => false,
            IncludeBackground::IfDifferent(c) => (style.background != c),
        };
        if include_bg {
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
               Escape(text))
            .unwrap();
    }
    s
}

/// Returns a `<pre style="...">\n` tag with the correct background color for the given theme.
/// This is for if you want to roll your own HTML output, you probably just want to use
/// `highlighted_snippet_for_string`.
///
/// If you don't care about the background color you can just prefix the lines from
/// `styles_to_coloured_html` with a `<pre>`. This is meant to be used with `IncludeBackground::IfDifferent`.
///
/// You're responsible for creating the string `</pre>` to close this, I'm not gonna provide a
/// helper for that :-)
pub fn start_coloured_html_snippet(t: &Theme) -> String {
    let c = t.settings.background.unwrap_or(highlighting::WHITE);
    format!("<pre style=\"background-color:#{:02x}{:02x}{:02x}\">\n",
            c.r,
            c.g,
            c.b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsing::{SyntaxSet, ParseState, ScopeStack};
    use highlighting::{ThemeSet, Style, Highlighter, HighlightIterator, HighlightState};
    #[test]
    fn tokens() {
        let ps = SyntaxSet::load_defaults_nonewlines();
        let syntax = ps.find_syntax_by_name("Markdown").unwrap();
        let mut state = ParseState::new(syntax);
        let line = "[w](t.co) *hi* **five**";
        let ops = state.parse_line(line);

        // use util::debug_print_ops;
        // debug_print_ops(line, &ops);

        let html = tokens_to_classed_html(line, &ops[..], ClassStyle::Spaced);
        println!("{}", html);
        assert_eq!(html, include_str!("../testdata/test2.html").trim_right());

        let ts = ThemeSet::load_defaults();
        let highlighter = Highlighter::new(&ts.themes["InspiredGitHub"]);
        let mut highlight_state = HighlightState::new(&highlighter, ScopeStack::new());
        let iter = HighlightIterator::new(&mut highlight_state, &ops[..], line, &highlighter);
        let regions: Vec<(Style, &str)> = iter.collect();

        let html2 = styles_to_coloured_html(&regions[..], IncludeBackground::Yes);
        println!("{}", html2);
        assert_eq!(html2, include_str!("../testdata/test1.html").trim_right());
    }

    #[test]
    fn strings() {
        let ss = SyntaxSet::load_defaults_nonewlines();
        let ts = ThemeSet::load_defaults();
        let s = include_str!("../testdata/highlight_test.erb");
        let syntax = ss.find_syntax_by_extension("erb").unwrap();
        let html = highlighted_snippet_for_string(s, syntax, &ts.themes["base16-ocean.dark"]);
        assert_eq!(html, include_str!("../testdata/test3.html"));
        let html2 = highlighted_snippet_for_file("testdata/highlight_test.erb",
                                                 &ss,
                                                 &ts.themes["base16-ocean.dark"])
            .unwrap();
        assert_eq!(html2, html);

        // YAML is a tricky syntax and InspiredGitHub is a fancy theme, this is basically an integration test
        let html3 = highlighted_snippet_for_file("testdata/Packages/Rust/Cargo.sublime-syntax",
                                                 &ss,
                                                 &ts.themes["InspiredGitHub"])
            .unwrap();
        println!("{}", html3);
        assert_eq!(html3, include_str!("../testdata/test4.html"));
    }

    #[test]
    fn tricky_test_syntax() {
        // This syntax I wrote tests edge cases of prototypes
        // I verified the output HTML against what ST3 does with the same syntax and file
        let ss = SyntaxSet::load_from_folder("testdata").unwrap();
        let ts = ThemeSet::load_defaults();
        let html = highlighted_snippet_for_file("testdata/testing-syntax.testsyntax",
                                                &ss,
                                                &ts.themes["base16-ocean.dark"])
            .unwrap();
        println!("{}", html);
        assert_eq!(html, include_str!("../testdata/test5.html"));
    }
}
