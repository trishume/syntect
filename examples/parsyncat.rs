//! Highlights the files given on the command line, in parallel.
//! Prints the highlighted output to stdout.

use rayon::prelude::*;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();

    if files.is_empty() {
        println!("Please provide some files to highlight.");
        return;
    }

    let syntax_set = SyntaxSet::load_defaults_newlines();
    let theme_set = ThemeSet::load_defaults();

    // We first collect the contents of the files...
    let contents: Vec<Vec<String>> = files
        .par_iter()
        .map(|filename| {
            let mut lines = Vec::new();
            // We use `String::new()` and `read_line()` instead of `BufRead::lines()`
            // in order to preserve the newlines and get better highlighting.
            let mut line = String::new();
            let mut reader = BufReader::new(File::open(filename).unwrap());
            while reader.read_line(&mut line).unwrap() > 0 {
                lines.push(line);
                line = String::new();
            }
            lines
        })
        .collect();

    // ...then highlight each file in parallel, collecting rendered output...
    let outputs: Vec<Vec<u8>> = files
        .par_iter()
        .zip(&contents)
        .map(|(filename, contents)| {
            let theme = &theme_set.themes["base16-ocean.dark"];
            let syntax = syntax_set
                .find_syntax_for_file(filename)
                .unwrap()
                .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
            let mut h = HighlightLines::new(syntax, &syntax_set, theme);

            for line in contents {
                h.highlight_line(line).unwrap();
            }

            h.finalize()
        })
        .collect();

    // ...and then print them all out.
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for output in outputs {
        out.write_all(&output).unwrap();
    }
}
