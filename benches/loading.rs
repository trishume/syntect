use criterion::{criterion_group, criterion_main, Bencher, Criterion};
use syntect::highlighting::ThemeSet;
use syntect::parsing::{SyntaxSet, SyntaxSetBuilder};

fn bench_load_internal_dump(b: &mut Bencher) {
    b.iter(|| SyntaxSet::load_defaults_newlines());
}

fn bench_load_internal_themes(b: &mut Bencher) {
    b.iter(|| ThemeSet::load_defaults());
}

fn bench_load_theme(b: &mut Bencher) {
    b.iter(|| ThemeSet::get_theme("testdata/spacegray/base16-ocean.dark.tmTheme"));
}

fn bench_add_from_folder(b: &mut Bencher) {
    b.iter(|| {
        let mut builder = SyntaxSetBuilder::new();
        builder.add_from_folder("testdata/Packages", false).unwrap()
    });
}

fn bench_link_syntaxes(b: &mut Bencher) {
    let mut builder = SyntaxSetBuilder::new();
    builder.add_from_folder("testdata/Packages", false).unwrap();
    b.iter(|| {
        builder.clone().build();
    });
}

fn loading_benchmark(c: &mut Criterion) {
    c.bench_function("load_internal_dump", bench_load_internal_dump);
    c.bench_function("load_internal_themes", bench_load_internal_themes);
    c.bench_function("load_theme", bench_load_theme);
    c.bench_function("add_from_folder", bench_add_from_folder);
    c.bench_function("link_syntaxes", bench_link_syntaxes);
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(50);
    targets = loading_benchmark
}
criterion_main!(benches);
