use criterion::{Bencher, Criterion, criterion_group, criterion_main};
use std::fs::File;
use std::io::Read;
use std::time::Duration;
use syntect::parsing::{ParseState, SyntaxReference, SyntaxSet};

fn do_parse(s: &str, ss: &SyntaxSet, syntax: &SyntaxReference) -> usize {
    let mut state = ParseState::new(syntax);
    let mut count = 0;
    for line in s.lines() {
        let ops = state.parse_line(line, ss);
        count += ops.len();
    }
    count
}

fn parse_file(b: &mut Bencher, file: &str) {
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

    let syntax = ss.find_syntax_for_file(path).unwrap().unwrap();
    let mut f = File::open(path).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();

    b.iter(|| do_parse(&s, &ss, syntax));
}

fn parsing_benchmark(c: &mut Criterion) {
    c.bench_function_over_inputs(
        "parse",
        |b, s| parse_file(b, s),
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
    config = Criterion::default().sample_size(50).warm_up_time(Duration::from_secs(30));
    targets = parsing_benchmark
}
criterion_main!(benches);
