//! Highlights the files given on the command line, in parallel.
//! Prints the highlighted output to stdout.

use rayon::prelude::*;
use syntect::highlighting::ThemeSet;
use syntect::io::HighlightedWriter;
use syntect::parsing::SyntaxSet;

use std::fs::File;
use std::io::{self, Write};

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();

    if files.is_empty() {
        println!("Please provide some files to highlight.");
        return;
    }

    let syntax_set = SyntaxSet::load_defaults_newlines();
    let theme_set = ThemeSet::load_defaults();

    // Highlight each file in parallel, buffering each result into its own
    // `Vec<u8>` so we can stream them out in order at the end.
    let outputs: Vec<Vec<u8>> = files
        .par_iter()
        .map(|filename| {
            let theme = &theme_set.themes["base16-ocean.dark"];
            let syntax = syntax_set
                .find_syntax_for_file(filename)
                .unwrap()
                .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
            let mut f = File::open(filename).unwrap();
            let mut w = HighlightedWriter::new(syntax, &syntax_set, theme);
            io::copy(&mut f, &mut w).unwrap();
            w.finalize().unwrap()
        })
        .collect();

    // ...and then print them all out.
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for output in outputs {
        out.write_all(&output).unwrap();
    }
}
