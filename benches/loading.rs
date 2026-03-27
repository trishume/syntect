use criterion::{criterion_group, criterion_main, Criterion};
use syntect::highlighting::ThemeSet;
use syntect::parsing::{SyntaxSet, SyntaxSetBuilder};

fn bench_load_internal_dump(b: &mut Bencher) {
    b.iter(SyntaxSet::load_defaults_newlines);
}

fn bench_load_internal_themes(b: &mut Bencher) {
    b.iter(ThemeSet::load_defaults);
}

    c.bench_function("load_internal_themes", |b| {
        b.iter(|| ThemeSet::load_defaults());
    });

    c.bench_function("load_theme", |b| {
        b.iter(|| ThemeSet::get_theme("testdata/spacegray/base16-ocean.dark.tmTheme"));
    });

    c.bench_function("add_from_folder", |b| {
        b.iter(|| {
            let mut builder = SyntaxSetBuilder::new();
            builder.add_from_folder("testdata/Packages", false).unwrap()
        });
    });

    c.bench_function("link_syntaxes", |b| {
        let mut builder = SyntaxSetBuilder::new();
        builder.add_from_folder("testdata/Packages", false).unwrap();
        b.iter(|| {
            builder.clone().build();
        });
    });

    c.bench_function("from_dump_file", |b| {
        b.iter(|| {
            let _: SyntaxSet =
                syntect::dumps::from_uncompressed_dump_file("assets/default_newlines.packdump")
                    .unwrap();
        });
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(50);
    targets = loading_benchmark
}
criterion_main!(benches);
