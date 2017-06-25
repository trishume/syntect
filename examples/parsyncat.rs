//! Highlights the files given on the command line, in parallel.
//! Prints the highlighted output to stdout.

#[macro_use] extern crate lazy_static;
extern crate rayon;
extern crate syntect;

use syntect::parsing::SyntaxSet;
use syntect::highlighting::{ThemeSet, Style};
use syntect::easy::HighlightFile;
use rayon::prelude::*;

use std::fs::File;
use std::io::{BufReader, BufRead};

thread_local! {
    static SYNTAX_SET: SyntaxSet = SyntaxSet::load_defaults_newlines();
}

lazy_static! {
    static ref THEME_SET: ThemeSet = ThemeSet::load_defaults();
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();

    if files.is_empty() {
        println!("Please provide some files to highlight.");
        return;
    }

    // We first collect the contents of the files...
    let contents: Vec<Vec<String>> = files.par_iter()
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

    // ...so that the highlighted regions have valid lifetimes...
    let regions: Vec<Vec<(Style, &str)>> = files.par_iter()
        .zip(&contents)
        .map(|(filename, contents)| {
            SYNTAX_SET.with(|ss| {
                let mut regions = Vec::new();
                let theme = &THEME_SET.themes["base16-ocean.dark"];
                let mut highlighter = HighlightFile::new(filename, ss, theme).unwrap();

                for line in contents {
                    for region in highlighter.highlight_lines.highlight(line) {
                        regions.push(region);
                    }
                }

                regions
            })
        })
        .collect();

    // ...and then print them all out.
    for file_regions in regions {
        print!("{}", syntect::util::as_24_bit_terminal_escaped(&file_regions[..], true));
    }
}
