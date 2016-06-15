#![feature(test)]

extern crate test;
extern crate syntect;
use test::Bencher;

use syntect::parsing::PackageSet;
use syntect::highlighting::ThemeSet;
use syntect::easy::HighlightLines;
use std::fs::File;
use std::path::Path;
use std::io::Read;

fn highlight_file(b: &mut Bencher, path_s: &str) {
    // don't load from dump so we don't count lazy regex compilation time
    let ps = PackageSet::load_from_folder("testdata/Packages").unwrap();
    let ts = ThemeSet::load_defaults();

    let path = Path::new(path_s);
    let extension = path.extension().unwrap().to_str().unwrap();
    let mut f = File::open(path).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();

    let syntax = ps.find_syntax_by_extension(extension).unwrap();
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
fn bench_highlighting_jquery(b: &mut Bencher) {
    highlight_file(b, "testdata/jquery.js");
}

#[bench]
fn bench_highlighting_rustc(b: &mut Bencher) {
    highlight_file(b, "testdata/parser.rs");
}
