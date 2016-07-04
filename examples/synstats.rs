extern crate syntect;
extern crate walkdir;
use syntect::parsing::{SyntaxSet, ParseState, ScopeStackOp, ScopeStack};
use syntect::highlighting::{ScopeSelector, ScopeSelectors};
use syntect::easy::{ScopeRegionIterator};

use std::path::Path;
use std::io::{BufRead, BufReader};
use std::fs::File;
use walkdir::{DirEntry, WalkDir, WalkDirIterator};
use std::str::FromStr;

#[derive(Debug)]
struct Selectors {
    comment: ScopeSelector,
    function: ScopeSelector,
    types: ScopeSelectors,
}

impl Default for Selectors {
    fn default() -> Selectors {
        Selectors {
            comment: ScopeSelector::from_str("comment").unwrap(),
            function: ScopeSelector::from_str("entity.name.function").unwrap(),
            types: ScopeSelectors::from_str("entity.name.class, entity.name.struct, entity.name.enum, entity.name.type").unwrap(),
        }
    }
}

#[derive(Debug, Default)]
struct Stats {
    selectors: Selectors,
    files: usize,
    functions: usize,
    types: usize,
    lines: usize,
    chars: usize,
    comment_lines: usize,
    comment_chars: usize,
    comment_tokens: usize,
}

fn is_ignored(entry: &DirEntry) -> bool {
    entry.file_name()
         .to_str()
         .map(|s| s.starts_with(".") && s.len() > 1 || s.ends_with(".md"))
         .unwrap_or(false)
}

fn count_line(ops: &[(usize, ScopeStackOp)], line: &str, stats: &mut Stats) {
    stats.lines += 1;

    let mut stack = ScopeStack::new();
    for (s, op) in ScopeRegionIterator::new(&ops, line) {
        stack.apply(op);
        if s.is_empty() { // in this case we don't care about blank tokens
            continue;
        }
        if stats.selectors.comment.does_match(stack.as_slice()).is_some() {
            stats.comment_chars += s.len();
            stats.comment_tokens += 1;
        }
        if stats.selectors.function.does_match(stack.as_slice()).is_some() {
            stats.functions += 1;
        }
        if stats.selectors.types.does_match(stack.as_slice()).is_some() {
            stats.types += 1;
        }
    }
}

fn count(ss: &SyntaxSet, path: &Path, stats: &mut Stats) {
    let syntax = match ss.find_syntax_for_file(path).unwrap_or(None) {
        Some(syntax) => syntax,
        None => return
    };
    stats.files += 1;
    let mut state = ParseState::new(syntax);

    let f = File::open(path).unwrap();
    let mut reader = BufReader::new(f);
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap() > 0 {
        {
            let ops = state.parse_line(&line);
            stats.chars += line.len();
            count_line(&ops, &line, stats);
        }
        line.clear();
    }
}

fn main() {
    let ss = SyntaxSet::load_defaults_newlines(); // note we load the version with newlines

    let args: Vec<String> = std::env::args().collect();
    let path = if args.len() < 2 {
        "."
    } else {
        &args[1]
    };

    let mut stats = Stats::default();
    let walker = WalkDir::new(path).into_iter();
    for entry in walker.filter_entry(|e| !is_ignored(e)) {
        let entry = entry.unwrap();
        println!("{}", entry.path().display());
        if entry.file_type().is_file() {
            count(&ss, entry.path(), &mut stats);
        }
    }

    println!("{:?}", stats);
}
