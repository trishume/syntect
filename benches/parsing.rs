#![feature(test)]

extern crate test;
extern crate syntect;
use test::Bencher;

use syntect::parsing::{SyntaxSet, ParseState};
use std::fs::File;
use std::io::Read;

fn parse_file(b: &mut Bencher, path_s: &str) {
    // don't load from dump so we don't count lazy regex compilation time
    let ps = SyntaxSet::load_defaults_nonewlines();

    let syntax = ps.find_syntax_for_file(path_s).unwrap().unwrap();
    let mut f = File::open(path_s).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();

    let mut state = ParseState::new(syntax);
    b.iter(|| {
        for line in s.lines() {
            let ops = state.parse_line(line);
            test::black_box(&ops);
        }
    });
}

#[bench]
fn bench_parsing_nesting(b: &mut Bencher) {
    parse_file(b, "testdata/highlight_test.erb");
}

#[bench]
fn bench_parsing_xml(b: &mut Bencher) {
    parse_file(b, "testdata/InspiredGitHub.tmtheme/InspiredGitHub.tmTheme");
}

#[bench]
fn bench_parsing_yaml(b: &mut Bencher) {
    parse_file(b, "testdata/Packages/Ruby/Ruby.sublime-syntax");
}

#[bench]
fn bench_parsing_jquery(b: &mut Bencher) {
    parse_file(b, "testdata/jquery.js");
}

#[bench]
fn bench_parsing_rustc(b: &mut Bencher) {
    parse_file(b, "testdata/parser.rs");
}

#[bench]
fn bench_parsing_scope(b: &mut Bencher) {
    parse_file(b, "src/parsing/scope.rs");
}
