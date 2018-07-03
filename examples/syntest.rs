//! An example of using syntect for testing syntax definitions.
//! Basically exactly the same as what Sublime Text can do,
//! but without needing ST installed
// To run tests only for a particular package, while showing the operations, you could use:
// cargo run --example syntest -- --debug testdata/Packages/Makefile/
// to specify that the syntax definitions should be parsed instead of loaded from the dump file,
// you can tell it where to parse them from - the following will execute only 1 syntax test after
// parsing the sublime-syntax files in the JavaScript folder:
// cargo run --example syntest testdata/Packages/JavaScript/syntax_test_json.json testdata/Packages/JavaScript/
extern crate syntect;
extern crate walkdir;
#[macro_use]
extern crate lazy_static;
extern crate regex;
extern crate getopts;

//extern crate onig;
use syntect::parsing::{SyntaxSet};
use syntect::syntax_tests::{SyntaxTestFileResult, SyntaxTestOutputOptions, process_syntax_test_assertions};

use std::path::Path;
use std::io::prelude::*;
use std::io::{BufRead, BufReader};
use std::fs::File;
use std::time::Instant;

use getopts::Options;
use regex::Regex;
use walkdir::{DirEntry, WalkDir};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxTestHeaderError {
    MalformedHeader,
    SyntaxDefinitionNotFound,
}

lazy_static! {
    pub static ref SYNTAX_TEST_HEADER_PATTERN: Regex = Regex::new(r#"(?xm)
            ^(?P<testtoken_start>\s*\S+)
            \s+SYNTAX\sTEST\s+
            "(?P<syntax_file>[^"]+)"
            \s*(?P<testtoken_end>\S+)?$
        "#).unwrap();
}

fn test_file(ss: &SyntaxSet, path: &Path, out_opts: SyntaxTestOutputOptions) -> Result<SyntaxTestFileResult, SyntaxTestHeaderError> {
    let f = File::open(path).unwrap();
    let mut reader = BufReader::new(f);
    let mut header_line = String::new();

    // read the first line from the file - if we have reached EOF already, it's an invalid file
    if reader.read_line(&mut header_line).unwrap() == 0 {
        return Err(SyntaxTestHeaderError::MalformedHeader);
    }
    header_line = header_line.replace("\r", &"");

    // parse the syntax test header in the first line of the file
    let search_result = SYNTAX_TEST_HEADER_PATTERN.captures(&header_line);
    let captures = search_result.ok_or(SyntaxTestHeaderError::MalformedHeader)?;

    let testtoken_start = captures.name("testtoken_start").unwrap().as_str();
    let testtoken_end = captures.name("testtoken_end").map_or(None, |c|Some(c.as_str()));
    let syntax_file = captures.name("syntax_file").unwrap().as_str();

    // find the relevant syntax definition to parse the file with - case is important!
    if !out_opts.summary {
        println!("The test file references syntax definition file: {}", syntax_file); //" and the start test token is {} and the end token is {:?}", testtoken_start, testtoken_end);
    }
    let syntax = ss.find_syntax_by_path(syntax_file).ok_or(SyntaxTestHeaderError::SyntaxDefinitionNotFound)?;

    let mut contents = String::new();
    contents.push_str(&header_line);
    reader.read_to_string(&mut contents).expect("Unable to read file");
    contents = contents.replace("\r", &"");

    let res = process_syntax_test_assertions(&syntax, &contents, testtoken_start, testtoken_end, &out_opts);

    if out_opts.summary {
        if let SyntaxTestFileResult::FailedAssertions(failures, _) = res {
            // Don't print total assertion count so that diffs don't pick up new succeeding tests
            println!("FAILED {}: {}", path.display(), failures);
        }
    } else {
        println!("{:?}", res);
    }

    Ok(res)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut opts = Options::new();
    opts.optflag("d", "debug", "Show parsing results for each test line");
    opts.optflag("t", "time", "Time execution as a more broad-ranging benchmark");
    opts.optflag("s", "summary", "Print only summary of test failures");

    let matches = match opts.parse(&args[1..]) {
        Ok(m) => { m }
        Err(f) => { panic!(f.to_string()) }
    };

    let tests_path = if matches.free.len() < 1 {
        "."
    } else {
        &args[1]
    };

    let syntaxes_path = if matches.free.len() < 2 {
        ""
    } else {
        &args[2]
    };

    // load the syntaxes from disk if told to
    // (as opposed to from the binary dumps)
    // this helps to ensure that a recompile isn't needed
    // when using this for syntax development
    let mut ss = if syntaxes_path.is_empty() {
        SyntaxSet::load_defaults_newlines() // note we load the version with newlines
    } else {
        SyntaxSet::new()
    };
    if !syntaxes_path.is_empty() {
        println!("loading syntax definitions from {}", syntaxes_path);
        ss.load_syntaxes(&syntaxes_path, true).unwrap(); // note that we load the version with newlines
        ss.link_syntaxes();
    }

    let out_opts = SyntaxTestOutputOptions {
        debug: matches.opt_present("debug"),
        time: matches.opt_present("time"),
        summary: matches.opt_present("summary"),
    };

    let exit_code = recursive_walk(&ss, &tests_path, out_opts);
    println!("exiting with code {}", exit_code);
    std::process::exit(exit_code);

}


fn recursive_walk(ss: &SyntaxSet, path: &str, out_opts: SyntaxTestOutputOptions) -> i32 {
    let mut exit_code: i32 = 0; // exit with code 0 by default, if all tests pass
    let walker = WalkDir::new(path).into_iter();

    // accumulate and sort for consistency of diffs across machines
    let mut files = Vec::new();
    for entry in walker.filter_entry(|e|e.file_type().is_dir() || is_a_syntax_test_file(e)) {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            files.push(entry.path().to_owned());
        }
    }
    files.sort();

    for path in &files {
        if !out_opts.summary {
            println!("Testing file {}", path.display());
        }
        let start = Instant::now();
        let result = test_file(&ss, path, out_opts);
        let elapsed = start.elapsed();
        if out_opts.time {
            let ms = (elapsed.as_secs() * 1_000) + (elapsed.subsec_nanos() / 1_000_000) as u64;
            println!("{} ms for file {}", ms, path.display());
        }
        if exit_code != 2 { // leave exit code 2 if there was an error
            if let Err(_) = result { // set exit code 2 if there was an error
                println!("{:?}", result);
                exit_code = 2;
            } else if let Ok(ok) = result {
                if let SyntaxTestFileResult::FailedAssertions(_, _) = ok {
                    exit_code = 1; // otherwise, if there were failures, exit with code 1
                }
            }
        }
    }

    exit_code
}

fn is_a_syntax_test_file(entry: &DirEntry) -> bool {
    entry.file_name()
         .to_str()
         .map(|s| s.starts_with("syntax_test_"))
         .unwrap_or(false)
}
