use std::fmt::Write;
use syntect::easy::{HighlightLines, StyleWriter};
use syntect::highlighting::{Style, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

const LATEX_REPLACE: [(&str, &str); 3] = [("\\", "\\\\"), ("{", "\\{"), ("}", "\\}")];

/// A [`StyleWriter`] that produces LaTeX `\textcolor[RGB]{r,g,b}{...}` output.
struct LatexStyleWriter;

impl StyleWriter for LatexStyleWriter {
    fn open(&mut self, style: Style, output: &mut String) {
        write!(
            output,
            "\\textcolor[RGB]{{{},{},{}}}{{",
            style.foreground.r, style.foreground.g, style.foreground.b
        )
        .unwrap();
    }

    fn close(&mut self, output: &mut String) {
        output.push('}');
    }

    fn text(&mut self, text: &str, output: &mut String) {
        let mut content = text.to_string();
        for &(old, new) in LATEX_REPLACE.iter() {
            content = content.replace(old, new);
        }
        output.push_str(&content);
    }
}

fn main() {
    let ps = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();

    let syntax = ps.find_syntax_by_extension("rs").unwrap();
    let s = "pub struct Wow { hi: u64 }\nfn blah() -> u64 {}\n";

    let mut highlight =
        HighlightLines::new_styled(syntax, &ps, &ts.themes["InspiredGitHub"], LatexStyleWriter);
    for line in LinesWithEndings::from(s) {
        highlight.highlight_line(line).unwrap();
    }
    let output = String::from_utf8(highlight.finalize()).unwrap();
    println!("{}", output);
}
