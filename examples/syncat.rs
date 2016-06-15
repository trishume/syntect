extern crate syntect;
use syntect::parsing::PackageSet;
use syntect::highlighting::{ThemeSet, Style};
use syntect::util::as_24_bit_terminal_escaped;
use syntect::easy::HighlightLines;

use std::io::BufReader;
use std::io::BufRead;
use std::path::Path;
use std::fs::File;

fn main() {
    let ps = PackageSet::load_defaults_nonewlines();
    let ts = ThemeSet::load_defaults();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Please pass in a file to highlight");
        return;
    }

    let path = Path::new(&args[1]);
    let extension = path.extension().unwrap().to_str().unwrap();
    let f = File::open(path).unwrap();
    let file = BufReader::new(&f);

    let syntax = ps.find_syntax_by_extension(extension).unwrap();
    let mut highlighter = HighlightLines::new(syntax, &ts.themes["base16-ocean.dark"]);
    for maybe_line in file.lines() {
        let line = maybe_line.unwrap();
        let regions: Vec<(Style, &str)> = highlighter.highlight(&line);
        let escaped = as_24_bit_terminal_escaped(&regions[..], true);
        println!("{}", escaped);
    }
}
