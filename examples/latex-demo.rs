use std::fmt::Write as FmtWrite;
use std::io::Write as IoWrite;
use syntect::highlighting::{Style, ThemeSet};
use syntect::io::HighlightedWriter;
use syntect::parsing::SyntaxSet;
use syntect::rendering::StyledOutput;

/// A [`StyledOutput`] that produces LaTeX `\textcolor[RGB]{r,g,b}{...}`
/// output. Wrap with [`ThemedRenderer`] (or pass to
/// [`HighlightedWriter::with_themed`]) to plug it into a highlighter.
struct LatexStyledOutput;

impl StyledOutput for LatexStyledOutput {
    fn begin_style(&mut self, style: Style, output: &mut String) {
        write!(
            output,
            "\\textcolor[RGB]{{{},{},{}}}{{",
            style.foreground.r, style.foreground.g, style.foreground.b
        )
        .unwrap();
    }

    fn end_style(&mut self, output: &mut String) {
        output.push('}');
    }

    fn write_text(&mut self, text: &str, output: &mut String) {
        // Because we opt into `closes_at_line_boundaries`, the adapter
        // guarantees `text` never contains '\n', so we only need to escape
        // LaTeX's three special characters.
        for ch in text.chars() {
            match ch {
                '\\' => output.push_str("\\\\"),
                '{' => output.push_str("\\{"),
                '}' => output.push_str("\\}"),
                _ => output.push(ch),
            }
        }
    }

    fn closes_at_line_boundaries(&self) -> bool {
        // fancyvrb's Verbatim environment processes lines independently, so
        // a `\textcolor[RGB]{...}{` opened on one line and closed by `}` on
        // the next would break. Force the adapter to close styles at line
        // boundaries and emit '\n' between spans.
        true
    }
}

fn main() {
    // Load these once at the start of your program
    let ps = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();

    let syntax = ps.find_syntax_by_extension("rs").unwrap();
    // No explicit `into_inner` needed at the end: we don't need the
    // `StdoutLock` back, and `Drop` runs the close-scopes / partial-line
    // cleanup on a best-effort basis when `highlight` falls out of scope.
    // Examples that *do* need the inner sink back (e.g. `parsyncat` collecting
    // a `Vec<u8>` per file) call `into_inner` explicitly to propagate errors.
    let mut highlight = HighlightedWriter::from_themed(
        syntax,
        &ps,
        &ts.themes["InspiredGitHub"],
        LatexStyledOutput,
    )
    .with_output(std::io::stdout().lock())
    .build();
    writeln!(highlight, "pub struct Wow {{ hi: u64 }}").unwrap();
    writeln!(highlight, "fn blah() -> u64 {{}}").unwrap();
}
