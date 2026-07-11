//! Differential harness for parser engine changes.
//!
//! Walks a directory tree (default `testdata/Packages`), parses every file
//! whose syntax can be resolved, and prints one fingerprint line per input
//! line: an FNV-1a 64 hash over that line's folded scope-stack regions.
//! `--full` prints the regions themselves instead, for human diffing.
//!
//! Cross-line backtracking (`ParseLineOutput::replayed`) is folded in before
//! anything is printed, so the dump reflects the final corrected scopes, not
//! the provisional op stream. Two engines that emit differently-shaped op
//! streams but reach the same scopes per region produce identical dumps —
//! this is deliberate: the comparison unit is observable highlighting
//! behavior, not op-stream stability.
//!
//! Typical use — capture a baseline, change the engine, diff:
//!
//! ```sh
//! cargo run --release --example scopedump > before.dump
//! # ... engine change ...
//! cargo run --release --example scopedump > after.dump
//! diff before.dump after.dump
//! ```
//!
//! Run once per regex backend (the fancy backend swaps in via the same
//! flags the syntest targets use):
//!
//! ```sh
//! cargo run --features default-fancy --no-default-features --release --example scopedump
//! ```
//!
//! To inspect a divergence flagged by the hash diff:
//!
//! ```sh
//! cargo run --release --example scopedump -- --full testdata/Packages/Java > java.dump
//! ```

use std::fmt::Write as _;
use std::path::Path;

use getopts::Options;
use walkdir::{DirEntry, WalkDir};

use syntect::easy::ScopeRegionIterator;
use syntect::parsing::{ParseLineOutput, ParseState, ScopeStack, SyntaxSet, SyntaxSetBuilder};
use syntect::util::LinesWithEndings;

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

struct LineRecord {
    text: String,
    /// Scope stack at the start of this line. Kept so a later replay window
    /// beginning at this line can rebuild from the corrected baseline.
    stack_before: ScopeStack,
    /// Canonical serialization of the line's scope regions: one
    /// `charcol:charlen scope scope ...` entry per non-empty region.
    folded: String,
}

/// Applies `ops` to `stack`, returning the canonical folded-region
/// serialization for the line.
fn fold_line(
    ops: &[(usize, syntect::parsing::ScopeStackOp)],
    text: &str,
    stack: &mut ScopeStack,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut folded = String::new();
    let mut col = 0usize;
    for (region, op) in ScopeRegionIterator::new(ops, text) {
        stack.apply(op)?;
        if region.is_empty() {
            continue;
        }
        let len = region.chars().count();
        write!(folded, "{}:{}", col, len)?;
        for scope in stack.as_slice() {
            write!(folded, " {}", scope)?;
        }
        folded.push('\n');
        col += len;
    }
    Ok(folded)
}

/// Hidden files and directories are environment, not corpus: a submodule's
/// `.git` is a pointer file whose content is checkout-specific and would
/// make dumps from different clones incomparable.
fn is_hidden(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
}

enum FileOutcome {
    Dumped { lines: usize },
    SkippedNoSyntax,
    SkippedNotUtf8,
}

fn dump_file(ss: &SyntaxSet, path: &Path, rel: &Path, full: bool) -> FileOutcome {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return FileOutcome::SkippedNotUtf8,
    };
    let syntax = match ss.find_syntax_for_file(path) {
        Ok(Some(syntax)) => syntax,
        _ => return FileOutcome::SkippedNoSyntax,
    };

    println!("== {} [{}]", rel.display(), syntax.name);

    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut records: Vec<LineRecord> = Vec::new();
    // (1-based line number, message) — printed after the per-line dump so a
    // replay can't interleave with fingerprint lines.
    let mut notes: Vec<(usize, String)> = Vec::new();

    'lines: for line in LinesWithEndings::from(&text) {
        let current_line_number = records.len() + 1;
        let ParseLineOutput {
            ops,
            replayed,
            warnings,
        } = match state.parse_line(line, ss) {
            Ok(output) => output,
            Err(e) => {
                notes.push((current_line_number, format!("parse error: {}", e)));
                break 'lines;
            }
        };
        for warning in warnings {
            notes.push((current_line_number, format!("warning: {}", warning)));
        }

        // Fold corrected ops for previously-parsed lines back into their
        // records, rebuilding the running stack from the window base.
        if !replayed.is_empty() {
            let start = records.len() - replayed.len();
            stack = records[start].stack_before.clone();
            for (i, replayed_ops) in replayed.iter().enumerate() {
                let record = &mut records[start + i];
                record.stack_before = stack.clone();
                match fold_line(replayed_ops, &record.text, &mut stack) {
                    Ok(folded) => record.folded = folded,
                    Err(e) => {
                        notes.push((start + i + 1, format!("replay fold error: {}", e)));
                        break 'lines;
                    }
                }
            }
        }

        // Snapshot post-replay, pre-current-ops: this is the corrected
        // baseline a future replay covering this line must reset to.
        let stack_before = stack.clone();
        match fold_line(&ops, line, &mut stack) {
            Ok(folded) => records.push(LineRecord {
                text: line.to_string(),
                stack_before,
                folded,
            }),
            Err(e) => {
                notes.push((current_line_number, format!("fold error: {}", e)));
                break 'lines;
            }
        }
    }

    for (i, record) in records.iter().enumerate() {
        if full {
            for region in record.folded.lines() {
                println!("{:04} {}", i + 1, region);
            }
        } else {
            println!("{:04} {:016x}", i + 1, fnv1a64(record.folded.as_bytes()));
        }
    }
    for (line_number, note) in &notes {
        println!("!! line {}: {}", line_number, note);
    }

    FileOutcome::Dumped {
        lines: records.len(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut opts = Options::new();
    opts.optflag("", "full", "print scope regions instead of per-line hashes");
    opts.optflag("h", "help", "print this help menu");
    let matches = opts.parse(&args[1..]).unwrap_or_else(|e| {
        eprintln!("{}", e);
        std::process::exit(2);
    });
    if matches.opt_present("h") {
        println!(
            "{}",
            opts.usage("Usage: scopedump [--full] [TARGET] [SYNTAXES_DIR]")
        );
        return;
    }
    let full = matches.opt_present("full");
    let target = matches
        .free
        .first()
        .map(String::as_str)
        .unwrap_or("testdata/Packages");
    let syntaxes_dir = matches
        .free
        .get(1)
        .map(String::as_str)
        .unwrap_or("testdata/Packages");

    let mut builder = SyntaxSetBuilder::new();
    builder
        .add_from_folder(syntaxes_dir, true) // with newlines, matching syntest
        .unwrap();
    let ss = builder.build();

    let mut dumped = 0usize;
    let mut total_lines = 0usize;
    let mut skipped_no_syntax = 0usize;
    let mut skipped_not_utf8 = 0usize;

    let target_path = Path::new(target);
    for entry in WalkDir::new(target_path)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_hidden(e))
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let rel = entry
            .path()
            .strip_prefix(target_path)
            .unwrap_or(entry.path());
        match dump_file(&ss, entry.path(), rel, full) {
            FileOutcome::Dumped { lines } => {
                dumped += 1;
                total_lines += lines;
            }
            FileOutcome::SkippedNoSyntax => skipped_no_syntax += 1,
            FileOutcome::SkippedNotUtf8 => skipped_not_utf8 += 1,
        }
    }

    eprintln!(
        "{} files dumped ({} lines); skipped: {} without a resolvable syntax, {} not UTF-8",
        dumped, total_lines, skipped_no_syntax, skipped_not_utf8
    );
}
