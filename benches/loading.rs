#![feature(test)]

extern crate test;
extern crate syntect;
use test::Bencher;

use syntect::package_set::PackageSet;
use syntect::theme_set::ThemeSet;

#[bench]
fn bench_load_internal_dump(b: &mut Bencher) {
    b.iter(|| {
        let ps = PackageSet::load_defaults_newlines();
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
fn bench_load_syntaxes(b: &mut Bencher) {
    b.iter(|| {
        let mut ps = PackageSet::new();
        ps.load_syntaxes("testdata/Packages", false).unwrap();
    });
}

#[bench]
fn bench_link_syntaxes(b: &mut Bencher) {
    let mut ps = PackageSet::new();
    ps.load_syntaxes("testdata/Packages", false).unwrap();
    b.iter(|| {
        ps.link_syntaxes();
    });
}
