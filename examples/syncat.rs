extern crate syntect;
use std::borrow::Cow;
use std::path::Path;
use syntect::parsing::SyntaxSet;
use syntect::highlighting::{Theme, ThemeSet, Style};
use syntect::util::as_24_bit_terminal_escaped;
use syntect::easy::HighlightFile;
use syntect::dumps::{from_dump_file, dump_to_file};

use std::io::BufRead;

fn load_theme(tm_file: &String) -> Theme {
    let tm_path = Path::new(tm_file);
    let tm_cache = tm_path.with_extension("tmdump");

    if tm_cache.exists() {
        from_dump_file(tm_cache).unwrap()
    } else {
        let theme = ThemeSet::get_theme(tm_path).unwrap();
        dump_to_file(&theme, tm_cache).unwrap();
        theme

    }
}

fn main() {
    let ss = SyntaxSet::load_defaults_newlines(); // note we load the version with newlines
    let ts = ThemeSet::load_defaults();

    let args: Vec<String> = std::env::args().collect();
    let theme;

    if args.len() < 2 {
        println!("USAGE: ./syncat [THEME_FILE] SRC_FILE");
        println!("       ./syncat --list-file-types");
        return;

    } else if args.len() > 1 && args[1] == "--list-file-types" {
        println!("Supported file types:");

        for sd in ss.syntaxes() {
            println!("- {} (.{})", sd.name, sd.file_extensions.join(", ."));
        }

        return;

    } else if args.len() == 2 {
        theme = Cow::Borrowed(&ts.themes["base16-ocean.dark"]);

    } else {
        theme = Cow::Owned(load_theme(&args[1]));
    }

    let src = args.last().unwrap();
    let mut highlighter = HighlightFile::new(src, &ss, &theme).unwrap();

    // We use read_line instead of `for line in highlighter.reader.lines()` because that
    // doesn't return strings with a `\n`, and including the `\n` gets us more robust highlighting.
    // See the documentation for `SyntaxSet::load_syntaxes`.
    // It also allows re-using the line buffer, which should be a tiny bit faster.
    let mut line = String::new();
    while highlighter.reader.read_line(&mut line).unwrap() > 0 {
        {
            let regions: Vec<(Style, &str)> = highlighter.highlight_lines.highlight(&line);
            print!("{}", as_24_bit_terminal_escaped(&regions[..], true));
        }
        line.clear();
    }
}
