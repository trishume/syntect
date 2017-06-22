extern crate syntect;
extern crate getopts;

use getopts::Options;
use std::borrow::Cow;
use std::io::BufRead;
use std::path::Path;
use syntect::parsing::SyntaxSet;
use syntect::highlighting::{Theme, ThemeSet, Style};
use syntect::util::as_24_bit_terminal_escaped;
use syntect::easy::HighlightFile;
use syntect::dumps::{from_dump_file, dump_to_file};


fn load_theme(tm_file: &String, enable_caching: bool) -> Theme {
    let tm_path = Path::new(tm_file);

    if enable_caching {
        let tm_cache = tm_path.with_extension("tmdump");

        if tm_cache.exists() {
            from_dump_file(tm_cache).unwrap()
        } else {
            let theme = ThemeSet::get_theme(tm_path).unwrap();
            dump_to_file(&theme, tm_cache).unwrap();
            theme
        }
    } else {
        ThemeSet::get_theme(tm_path).unwrap()
    }
}

fn main() {
    let ss = SyntaxSet::load_defaults_newlines(); // note we load the version with newlines
    let ts = ThemeSet::load_defaults();

    let args: Vec<String> = std::env::args().collect();
    let mut opts = Options::new();
    opts.optflag("l", "list-file-types", "Lists supported file types");
    opts.optopt("t", "theme-file", "Theme file to use. Default: base16-ocean (embedded)", "THEME_FILE");
    opts.optflag("c", "cache-theme", "Cache the parsed theme file.");

    let matches = match opts.parse(&args[1..]) {
        Ok(m) => { m }
        Err(f) => { panic!(f.to_string()) }
    };

    if matches.opt_present("list-file-types") {
        println!("Supported file types:");

        for sd in ss.syntaxes() {
            println!("- {} (.{})", sd.name, sd.file_extensions.join(", ."));
        }

    } else if matches.free.len() == 0 {
        let brief = format!("USAGE: {} [options] FILES", args[0]);
        println!("{}", opts.usage(&brief));

    } else {
        let theme = matches.opt_str("theme-file").map_or(
            Cow::Borrowed(&ts.themes["base16-ocean.dark"]),
            |tf| Cow::Owned(load_theme(&tf, matches.opt_present("cache-theme")))
        );

        for src in &matches.free[..] {
            if matches.free.len() > 1 {
                println!("==> {} <==", src);
            }

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

            // Clear the formatting
            println!("\x1b[0m");
        }
    }
}
