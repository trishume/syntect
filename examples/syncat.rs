extern crate syntect;
use syntect::parsing::SyntaxSet;
use syntect::highlighting::{ThemeSet, Style};
use syntect::util::as_24_bit_terminal_escaped;
use syntect::easy::HighlightLines;

use std::io::BufRead;

fn main() {
    let ss = SyntaxSet::load_defaults_newlines(); // note we load the version with newlines
    let ts = ThemeSet::load_defaults();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Please pass in a file to highlight");
        return;
    }

    let mut highlighter = HighlightFile::new(&args[1], &ss, &ts.themes["base16-ocean.dark"]).unwrap();

    // We use read_line instead of `for line in highlighter.reader.lines()` because that
    // doesn't return strings with a `\n`, and including the `\n` gets us more robust highlighting.
    // See the documentation for `SyntaxSet::load_syntaxes`.
    // It also allows re-using the line buffer, which should be a tiny bit faster.
}
