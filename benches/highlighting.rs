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
use std::path::Path;
use std::io::Read;

fn highlight_file(b: &mut Bencher, path_s: &str) {
    let ps = PackageSet::load_from_folder("testdata/Packages").unwrap();
    let highlighter = Highlighter::new(PackageSet::get_theme("testdata/spacegray/base16-ocean.\
                                                              dark.tmTheme")
        .unwrap());
    let path = Path::new(path_s);
    let extension = path.extension().unwrap().to_str().unwrap();
    let mut f = File::open(path).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();
    let syntax = ps.find_syntax_by_extension(extension).unwrap();
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

#[bench]
fn bench_highlighting_nesting(b: &mut Bencher) {
    highlight_file(b, "testdata/highlight_test.erb");
}

#[bench]
fn bench_highlighting_jquery(b: &mut Bencher) {
    highlight_file(b, "testdata/jquery.js");
}
