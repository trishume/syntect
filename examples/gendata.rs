//! This program is mainly intended for generating the dumps that are compiled in to
//! syntect, not as a helpful example for beginners.
//! Although it is a valid example for serializing syntaxes, you probably won't need
//! to do this yourself unless you want to cache your own compiled grammars.
extern crate syntect;
use syntect::parsing::SyntaxSet;
use syntect::highlighting::ThemeSet;
use syntect::dumps::*;

fn main() {
    let mut ps = SyntaxSet::new();
    ps.load_plain_text_syntax();
    ps.load_syntaxes("testdata/Packages", true).unwrap();
    dump_to_file(&ps, "assets/default_newlines.packdump").unwrap();

    let mut ps2 = SyntaxSet::new();
    ps2.load_plain_text_syntax();
    ps2.load_syntaxes("testdata/Packages", false).unwrap();
    dump_to_file(&ps2, "assets/default_nonewlines.packdump").unwrap();

    let ts = ThemeSet::load_from_folder("testdata").unwrap();
    dump_to_file(&ts, "assets/default.themedump").unwrap();
}
