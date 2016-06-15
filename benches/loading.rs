#![feature(test)]

extern crate test;
extern crate syntect;
use test::Bencher;

use syntect::parsing::SyntaxSet;
use syntect::highlighting::ThemeSet;

#[bench]
fn bench_load_internal_dump(b: &mut Bencher) {
    b.iter(|| {
        let ps = SyntaxSet::load_defaults_newlines();
        test::black_box(&ps);
    });
}

#[bench]
fn bench_load_internal_themes(b: &mut Bencher) {
    b.iter(|| {
        let ts = ThemeSet::load_defaults();
        test::black_box(&ts);
    });
}

#[bench]
fn bench_load_theme(b: &mut Bencher) {
    b.iter(|| {
        let theme = ThemeSet::get_theme("testdata/spacegray/base16-ocean.dark.tmTheme");
        test::black_box(&theme);
    });
}

#[bench]
fn bench_load_syntaxes(b: &mut Bencher) {
    b.iter(|| {
        let mut ps = SyntaxSet::new();
        ps.load_syntaxes("testdata/Packages", false).unwrap();
    });
}

#[bench]
fn bench_link_syntaxes(b: &mut Bencher) {
    let mut ps = SyntaxSet::new();
    ps.load_syntaxes("testdata/Packages", false).unwrap();
    b.iter(|| {
        ps.link_syntaxes();
    });
}
