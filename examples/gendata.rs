//! This program is mainly intended for generating the dumps that are compiled in to
//! syntect, not as a helpful example for beginners.
//! Although it is a valid example for serializing syntaxes, you probably won't need
//! to do this yourself unless you want to cache your own compiled grammars.
extern crate syntect;
use syntect::parsing::SyntaxSetBuilder;
use syntect::highlighting::ThemeSet;
use syntect::dumps::*;
use std::env;

fn usage_and_exit() -> ! {
    println!("USAGE: gendata synpack source-dir newlines.packdump nonewlines.packdump\n
              gendata themepack source-dir themepack.themedump");
    ::std::process::exit(2);
}

fn main() {

    let mut a = env::args().skip(1);
    match (a.next(), a.next(), a.next(), a.next()) {
        (Some(ref cmd),
         Some(ref package_dir),
         Some(ref packpath_newlines),
         Some(ref packpath_nonewlines)) if cmd == "synpack" => {
            let mut builder = SyntaxSetBuilder::new();
            builder.add_plain_text_syntax();
            builder.load_from_folder(package_dir, true).unwrap();
            let ss = builder.build();
            dump_to_file(&ss, packpath_newlines).unwrap();

            let mut builder_nonewlines = SyntaxSetBuilder::new();
            builder_nonewlines.add_plain_text_syntax();
            builder_nonewlines.load_from_folder(package_dir, false).unwrap();
            let ss_nonewlines = builder_nonewlines.build();
            dump_to_file(&ss_nonewlines, packpath_nonewlines).unwrap();
        }
        (Some(ref s), Some(ref theme_dir), Some(ref packpath), None) if s == "themepack" => {
            let ts = ThemeSet::load_from_folder(theme_dir).unwrap();
            dump_to_file(&ts, packpath).unwrap();
        }
        _ => usage_and_exit(),
    }
}
