extern crate syntect;
use syntect::package_set::PackageSet;
use syntect::parser::*;
use syntect::theme::highlighter::*;
use syntect::theme::style::*;
use syntect::util::{as_24_bit_terminal_escaped, debug_print_ops};

use std::io::Read;
use std::path::Path;
use std::fs::File;

fn main() {
    let ps = PackageSet::load_from_folder("testdata/Packages").unwrap();
    let highlighter = Highlighter::new(PackageSet::get_theme("testdata/spacegray/base16-ocean.\
                                                              dark.tmTheme")
        .unwrap());

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Please pass in a file to highlight");
        return;
    }

    let path = Path::new(&args[1]);
    let extension = path.extension().unwrap().to_str().unwrap();
    let mut f = File::open(path).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();

    let syntax = ps.find_syntax_by_extension(extension).unwrap();

    for _ in 1..10000 {
        let mut state = ParseState::new(syntax);
        let mut highlight_state = HighlightState::new(&highlighter, state.scope_stack.clone());
        for line in s.lines() {
            // println!("{}", state.scope_stack);
            let ops = state.parse_line(&line);
            // debug_print_ops(&line, &ops);
            let iter = HighlightIterator::new(&mut highlight_state, &ops[..], &line, &highlighter);
            let regions: Vec<(Style, &str)> = iter.collect();
            // let escaped = as_24_bit_terminal_escaped(&regions[..], true);
            print!("{}", regions.len());
        }
    }
}
