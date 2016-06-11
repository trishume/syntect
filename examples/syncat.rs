extern crate syntect;
use syntect::scope::ScopeStack;
use syntect::package_set::PackageSet;
use syntect::parser::*;
use syntect::theme::highlighter::*;
use syntect::theme::style::*;
use syntect::util::as_24_bit_terminal_escaped;

use std::io::BufReader;
use std::io::BufRead;
use std::path::Path;
use std::fs::File;

fn main() {
    let ps = PackageSet::load_defaults_nonewlines();
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
    let f = File::open(path).unwrap();
    let file = BufReader::new(&f);

    let mut state = {
        let syntax = ps.find_syntax_by_extension(extension).unwrap();
        ParseState::new(syntax)
    };

    let mut highlight_state = HighlightState::new(&highlighter, ScopeStack::new());
    for maybe_line in file.lines() {
        let line = maybe_line.unwrap();
        // println!("{}", state.scope_stack);
        let ops = state.parse_line(&line);
        // debug_print_ops(&line, &ops);
        let iter = HighlightIterator::new(&mut highlight_state, &ops[..], &line, &highlighter);
        let regions: Vec<(Style, &str)> = iter.collect();
        let escaped = as_24_bit_terminal_escaped(&regions[..], true);
        println!("{}", escaped);
    }
}
