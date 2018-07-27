#[macro_use]
extern crate criterion;
extern crate syntect;

use criterion::{Bencher, Criterion};

use syntect::parsing::{SyntaxSet, SyntaxReference, ScopeStack};
use syntect::highlighting::{ThemeSet, Theme};
use syntect::easy::HighlightLines;
use std::str::FromStr;
use std::fs::File;
use std::io::Read;

fn do_highlight(s: &str, syntax_set: &SyntaxSet, syntax: &SyntaxReference, theme: &Theme) -> usize {
    let mut h = HighlightLines::new(syntax, theme);
    let mut count = 0;
    for line in s.lines() {
        let regions = h.highlight(line, syntax_set);
        count += regions.len();
    }
    count
}

fn highlight_file(b: &mut Bencher, file: &str) {
    let path = match file {
        "highlight_test.erb" => "testdata/highlight_test.erb",
        "InspiredGitHub.tmTheme" => "testdata/InspiredGitHub.tmtheme/InspiredGitHub.tmTheme",
        "Ruby.sublime-syntax" => "testdata/Packages/Ruby/Ruby.sublime-syntax",
        "jquery.js" => "testdata/jquery.js",
        "parser.rs" => "testdata/parser.rs",
        "scope.rs" => "src/parsing/scope.rs",
        _ => panic!("Unknown test file {}", file),
    };

    // don't load from dump so we don't count lazy regex compilation time
    let ss = SyntaxSet::load_defaults_nonewlines();
    let ts = ThemeSet::load_defaults();

    let syntax = ss.find_syntax_for_file(path).unwrap().unwrap();
    let mut f = File::open(path).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();

    b.iter(|| {
        do_highlight(&s, &ss, syntax, &ts.themes["base16-ocean.dark"])
    });
}


fn stack_matching(b: &mut Bencher) {
    let s = "source.js meta.group.js meta.group.js meta.block.js meta.function-call.method.js meta.group.js meta.object-literal.js meta.block.js meta.function-call.method.js meta.group.js variable.other.readwrite.js";
    let stack = ScopeStack::from_str(s).unwrap();
    let selector = ScopeStack::from_str("source meta.function-call.method").unwrap();
    b.iter(|| {
        selector.does_match(stack.as_slice())
    });
}

fn highlighting_benchmark(c: &mut Criterion) {
    c.bench_function("stack_matching", stack_matching);
    c.bench_function_over_inputs(
        "highlight",
        |b, s| highlight_file(b, s),
        vec![
            "highlight_test.erb",
            "InspiredGitHub.tmTheme",
            "Ruby.sublime-syntax",
            "jquery.js",
            "parser.rs",
            "scope.rs",
        ],
    );
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = highlighting_benchmark
}
criterion_main!(benches);
