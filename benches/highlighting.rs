#![feature(test)]

extern crate test;
extern crate syntect;
use test::Bencher;

use syntect::parsing::{SyntaxSet, SyntaxDefinition, ScopeStack};
use syntect::highlighting::{ThemeSet, Theme};
use syntect::easy::HighlightLines;
use std::str::FromStr;
use std::fs::File;
use std::io::Read;

/// Iterator yielding every line in a string. The line includes newline character(s).
pub struct LinesWithEndings<'a> {
    input: &'a str,
}

impl<'a> LinesWithEndings<'a> {
    pub fn from(input: &'a str) -> LinesWithEndings<'a> {
        LinesWithEndings {
            input: input,
        }
    }
}

impl<'a> Iterator for LinesWithEndings<'a> {
    type Item = &'a str;

    #[inline]
    fn next(&mut self) -> Option<&'a str> {
        if self.input.is_empty() {
            return None;
        }
        let split = self.input.find('\n').map(|i| i + 1).unwrap_or(self.input.len());
        let (line, rest) = self.input.split_at(split);
        self.input = rest;
        Some(line)
    }
}

fn do_highlight(s: &str, syntax: &SyntaxDefinition, theme: &Theme) {
    let mut h = HighlightLines::new(syntax, theme);
    for line in LinesWithEndings::from(s) {
        let regions = h.highlight(line);
        test::black_box(&regions);
    }
}

fn highlight_file(b: &mut Bencher, path_s: &str) {
    // don't load from dump so we don't count lazy regex compilation time
    let ps = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();

    let syntax = ps.find_syntax_for_file(path_s).unwrap().unwrap();
    let mut f = File::open(path_s).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();

    do_highlight(&s, syntax, &ts.themes["base16-ocean.dark"]);
    b.iter(|| {
        do_highlight(&s, syntax, &ts.themes["base16-ocean.dark"]);
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

#[bench]
fn bench_stack_matching(b: &mut Bencher) {
    let s = "source.js meta.group.js meta.group.js meta.block.js meta.function-call.method.js meta.group.js meta.object-literal.js meta.block.js meta.function-call.method.js meta.group.js variable.other.readwrite.js";
    let stack = ScopeStack::from_str(s).unwrap();
    let selector = ScopeStack::from_str("source meta.function-call.method").unwrap();
    b.iter(|| {
        let res = selector.does_match(stack.as_slice());
        test::black_box(res);
    });
}
