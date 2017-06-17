#![feature(test)]

extern crate test;
extern crate syntect;
extern crate crossbeam;

use syntect::parsing::{SyntaxSetPool, SyntaxSet, SyntaxDefinition, ParseState};
use test::Bencher;
use std::fs::File;
use std::io::Read;

fn do_parse(s: &str, syntax: &SyntaxDefinition) {
    let mut state = ParseState::new(syntax);
    for line in s.lines() {
        let ops = state.parse_line(line);
        test::black_box(&ops);
    }
}

fn parse_file(path_s: &str, ps: &SyntaxSet) {
    let syntax = ps.find_syntax_for_file(path_s).unwrap().unwrap();
    let mut f = File::open(path_s).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();

    do_parse(&s, syntax);
}

#[bench]
fn bench_parallel_parsing(b: &mut Bencher) {
    let files =
        [ "audit.rb"
        , "download_strategy.rb"
        , "formula.rb"
        , "load_commands.rb"
        , "parser.rs"
        , "README.md"
        , "core_unix.ml"
        , "Core.hs"
        ];
    let syntaxes = SyntaxSetPool::new(SyntaxSet::load_defaults_nonewlines);
    let syntaxes_ref = &syntaxes;

    b.iter(|| {
        crossbeam::scope(|scope| {
            for file in &files {
                let filepath = format!("testdata/issue20/{}", file);
                test::black_box(
                    scope.spawn(move || {
                        syntaxes_ref.with_syntax_set(|ps| {
                            parse_file(&filepath, ps);
                        });
                    })
                );
            }
        });
    });
}
