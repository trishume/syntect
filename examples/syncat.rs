extern crate syntect;
use syntect::parsing::SyntaxSet;
use syntect::highlighting::{ThemeSet, Style};
use syntect::util::as_24_bit_terminal_escaped;
use syntect::easy::HighlightFile;

use std::io::BufRead;

fn main() {
    let ss = SyntaxSet::load_defaults_nonewlines();
    // use this format to load your own set of packages
    // let ss = SyntaxSet::load_from_folder("testdata/Packages").unwrap();
    let ts = ThemeSet::load_defaults();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Please pass in a file to highlight");
        return;
    }

    let mut highlighter = HighlightFile::new(&args[1], &ss, &ts.themes["base16-ocean.dark"]).unwrap();
    for maybe_line in highlighter.reader.lines() {
        let line = maybe_line.unwrap();
        let regions: Vec<(Style, &str)> = highlighter.highlight_lines.highlight(&line);
        println!("{}", as_24_bit_terminal_escaped(&regions[..], true));
    }
}
