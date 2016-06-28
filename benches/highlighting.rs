#![feature(test)]

extern crate test;
extern crate syntect;
use test::Bencher;

use syntect::parsing::SyntaxSet;
use syntect::highlighting::ThemeSet;
use syntect::easy::HighlightLines;
use std::fs::File;
use std::io::Read;

fn highlight_file(b: &mut Bencher, path_s: &str) {
    // don't load from dump so we don't count lazy regex compilation time
    let ps = SyntaxSet::load_defaults_nonewlines();
    let ts = ThemeSet::load_defaults();

    let syntax = ps.find_syntax_for_file(path_s).unwrap().unwrap();
    let mut f = File::open(path_s).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();

    let mut h = HighlightLines::new(syntax, &ts.themes["base16-ocean.dark"]);
    b.iter(|| {
        for line in s.lines() {
            let regions = h.highlight(line);
            test::black_box(&regions);
        }
    });
}

#[bench]
fn bench_highlighting_nesting(b: &mut Bencher) {
    highlight_file(b, "testdata/highlight_test.erb");
}

#[bench]
fn bench_highlighting_xml(b: &mut Bencher) {
    highlight_file(b, "testdata/InspiredGitHub.tmtheme/InspiredGitHub.tmTheme");
}

#[bench]
fn bench_highlighting_yaml(b: &mut Bencher) {
    highlight_file(b, "testdata/Packages/Ruby/Ruby.sublime-syntax");
}

#[bench]
fn bench_highlighting_jquery(b: &mut Bencher) {
    highlight_file(b, "testdata/jquery.js");
}

#[bench]
fn bench_highlighting_rustc(b: &mut Bencher) {
    highlight_file(b, "testdata/parser.rs");
}

#[bench]
fn bench_highlighting_scope(b: &mut Bencher) {
    highlight_file(b, "src/parsing/scope.rs");
}
