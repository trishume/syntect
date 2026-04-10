use std::fmt::Write as FmtWrite;
use std::io::Write as IoWrite;
use syntect::highlighting::{Highlighter, Style, ThemeSet};
use syntect::io::{HighlightedWriter, ScopeRenderer};
use syntect::parsing::{Scope, SyntaxSet};

const LATEX_REPLACE: [(&str, &str); 3] = [("\\", "\\\\"), ("{", "\\{"), ("}", "\\}")];

/// A [`ScopeRenderer`] that resolves theme styles and produces LaTeX
/// `\textcolor[RGB]{r,g,b}{...}` output.
struct LatexScopeRenderer<'a> {
    highlighter: Highlighter<'a>,
    style_stack: Vec<Style>,
    last_written_style: Option<Style>,
}

impl<'a> LatexScopeRenderer<'a> {
    fn new(theme: &'a syntect::highlighting::Theme) -> Self {
        let highlighter = Highlighter::new(theme);
        let default_style = highlighter.style_for_stack(&[]);
        Self {
            highlighter,
            style_stack: vec![default_style],
            last_written_style: None,
        }
    }

    fn current_style(&self) -> Style {
        self.style_stack.last().copied().unwrap_or_default()
    }
}

impl ScopeRenderer for LatexScopeRenderer<'_> {
    fn begin_scope(
        &mut self,
        _atom_strs: &[&str],
        _scope: Scope,
        scope_stack: &[Scope],
        _output: &mut String,
    ) -> bool {
        let style = self.highlighter.style_for_stack(scope_stack);
        self.style_stack.push(style);
        false
    }

    fn end_scope(&mut self, _output: &mut String) {
        self.style_stack.pop();
    }

    fn write_text(&mut self, text: &str, output: &mut String) {
        if text.is_empty() {
            return;
        }
        // Pass spaces through without wrapping in \textcolor and skip
        // newlines (line breaks are emitted by end_line), matching the
        // behavior of `as_latex_escaped`.
        match text {
            " " => {
                output.push(' ');
                return;
            }
            "\n" => return,
            _ => {}
        }
        let style = self.current_style();
        if self.last_written_style != Some(style) {
            if self.last_written_style.is_some() {
                output.push('}');
            }
            write!(
                output,
                "\\textcolor[RGB]{{{},{},{}}}{{",
                style.foreground.r, style.foreground.g, style.foreground.b
            )
            .unwrap();
            self.last_written_style = Some(style);
        }
        let mut content = text.to_string();
        for &(old, new) in LATEX_REPLACE.iter() {
            content = content.replace(old, new);
        }
        output.push_str(&content);
    }

    fn end_line(&mut self, _line_index: usize, _scope_stack: &[Scope], output: &mut String) {
        if self.last_written_style.take().is_some() {
            output.push('}');
        }
        output.push('\n');
    }
}

fn main() {
    let ps = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();

    let syntax = ps.find_syntax_by_extension("rs").unwrap();
    let s = "pub struct Wow { hi: u64 }\nfn blah() -> u64 {}\n";

    let out = std::io::stdout().lock();
    let mut highlight = HighlightedWriter::new_with_renderer_and_output(
        syntax,
        &ps,
        LatexScopeRenderer::new(&ts.themes["InspiredGitHub"]),
        out,
    );
    highlight.write_all(s.as_bytes()).unwrap();
    let _ = highlight.finalize().unwrap();
}
