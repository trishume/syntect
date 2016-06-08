#![feature(test)]

extern crate test;
extern crate syntect;
use test::Bencher;

use syntect::package_set::PackageSet;
use syntect::scope::ScopeStack;
use syntect::parser::*;
use syntect::theme::highlighter::*;
use syntect::theme::style::*;
use std::fs::File;
use std::io::Read;

#[bench]
fn bench_highlighting(b: &mut Bencher) {
    let ps = PackageSet::load_from_folder("testdata/Packages").unwrap();
    let highlighter = Highlighter::new(PackageSet::get_theme("testdata/spacegray/base16-ocean.\
                                                              dark.tmTheme")
        .unwrap());
    let mut f = File::open("testdata/highlight_test.erb").unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();
    let syntax = ps.find_syntax_by_extension("erb").unwrap();
    b.iter(|| {
        let mut state = ParseState::new(syntax);
        let mut highlight_state = HighlightState::new(&highlighter, ScopeStack::new());
        for line in s.lines() {
            let ops = state.parse_line(&line);
            let iter = HighlightIterator::new(&mut highlight_state, &ops[..], &line, &highlighter);
            let regions: Vec<(Style, &str)> = iter.collect();
            test::black_box(&regions);
        }
    });
}
