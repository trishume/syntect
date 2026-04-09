//! Example: ANSI terminal highlighting using `DocumentGenerator` with a custom
//! `ScopeRenderer` that streams directly to stdout via `io::Write`.
//!
//! Run with: `cargo run --example ansi-scope-renderer`

use std::io::{self, BufWriter, Write};
use syntect::easy::{HighlightLines, ScopeRenderer};
use syntect::parsing::{Scope, SyntaxSet};
use syntect::util::LinesWithEndings;

/// A `ScopeRenderer` that emits ANSI 256-color escape codes based on scope names.
struct AnsiScopeRenderer {
    /// Stack of ANSI color codes for nested scopes, so `end_scope` can
    /// restore the previous color.
    color_stack: Vec<&'static str>,
}

const RESET: &str = "\x1b[0m";

impl AnsiScopeRenderer {
    fn new() -> Self {
        Self {
            color_stack: vec![RESET],
        }
    }

    fn color_for_scope(atoms: &[&str]) -> &'static str {
        match atoms.first().copied() {
            Some("comment") => "\x1b[38;5;242m", // gray
            Some("keyword") => "\x1b[38;5;197m", // pink
            Some("string") => "\x1b[38;5;114m",  // green
            Some("constant") => "\x1b[38;5;209m", // orange
            Some("storage") => "\x1b[38;5;81m",  // cyan
            Some("entity") => "\x1b[38;5;81m",   // cyan
            Some("support") => "\x1b[38;5;149m", // lime
            Some("punctuation") => "\x1b[38;5;252m", // light gray
            _ => RESET,
        }
    }
}

impl ScopeRenderer for AnsiScopeRenderer {
    fn begin_scope(
        &mut self,
        atom_strs: &[&str],
        _scope: Scope,
        _scope_stack: &[Scope],
        output: &mut String,
    ) -> bool {
        let color = Self::color_for_scope(atom_strs);
        self.color_stack.push(color);
        output.push_str(color);
        true
    }

    fn end_scope(&mut self, output: &mut String) {
        self.color_stack.pop();
        let prev = self.color_stack.last().copied().unwrap_or(RESET);
        output.push_str(prev);
    }
}

fn main() {
    let code = r#"// A simple Rust example
fn main() {
    let x = 42;
    let msg = "hello";
    println!("{}: {}", msg, x);
}
"#;

    let ss = SyntaxSet::load_defaults_newlines();
    let syntax = ss.find_syntax_by_extension("rs").unwrap();

    // Stream directly to stdout — no intermediate String accumulation.
    let stdout = BufWriter::new(io::stdout().lock());
    let mut gen = HighlightLines::new_with_output(syntax, &ss, AnsiScopeRenderer::new(), stdout);

    for line in LinesWithEndings::from(code) {
        gen.highlight_line(line).unwrap();
    }

    let mut writer = gen.finalize();
    writer.write_all(RESET.as_bytes()).unwrap();
    writer.flush().unwrap();
}
