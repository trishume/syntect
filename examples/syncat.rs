extern crate syntect;
use syntect::package_set::PackageSet;
use syntect::parser::*;
use syntect::theme::highlighter::*;
use syntect::theme::style::*;
use syntect::util::{as_24_bit_terminal_escaped, debug_print_ops};

fn main() {
    let ps = PackageSet::load_from_folder("testdata/Packages").unwrap();
    let mut state = {
        let syntax = ps.find_syntax_by_name("Ruby on Rails").unwrap();
        ParseState::new(syntax)
    };
    let highlighter = Highlighter::new(PackageSet::get_theme("testdata/spacegray/base16-ocean.dark.tmTheme")
        .unwrap());

    let start_stack = state.scope_stack.clone();
    let line = "module Bob::Wow::Troll::Five; lol(5,\"wow #{lel('hi',5)}\"); end";
    let ops = state.parse_line(line);
    debug_print_ops(line, &ops);
    let iter = HighlightIterator::new(start_stack, &ops[..], line, &highlighter);
    let regions: Vec<(Style, &str)> = iter.collect();
    let escaped = as_24_bit_terminal_escaped(&regions[..], true);
    println!("{}", escaped);
}
