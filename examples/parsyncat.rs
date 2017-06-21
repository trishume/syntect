extern crate syntect;
extern crate crossbeam;

use syntect::parsing::{SyntaxSetPool, SyntaxSet};
use syntect::highlighting::ThemeSet;
use syntect::util::as_24_bit_terminal_escaped;
use syntect::easy::HighlightLines;

use std::io::{stdout, BufReader, BufRead, Write};
use std::fs::File;

trait HasSync: Sync {}

fn main() {
  let ssp = SyntaxSetPool::new(SyntaxSet::load_defaults_newlines);
  let ssp_ref = &ssp;
  let ts = ThemeSet::load_defaults();
  let theme = &ts.themes["base16-ocean.dark"];
  let out = stdout();
  let out_ref = &out;

  let args: Vec<String> = std::env::args().collect();
  if args.len() < 2 {
    println!("Please pass in files to highlight");
    return;
  }

  crossbeam::scope(|scope| {
    for file in args.iter().skip(1) {
      scope.spawn(move || {
        let mut lines = Vec::new();

        {
          let f = File::open(file).unwrap();
          let mut bufread = BufReader::new(f);
          let mut line = String::new();
          while bufread.read_line(&mut line).unwrap() > 0 {
            lines.push(line);
            line = String::new();
          }
        }
         
        {
          let mut regions = Vec::new();

          ssp_ref.with_syntax_set(|ss| {
            let syntax = ss.find_syntax_for_file(file).unwrap().unwrap();
            let mut highlighter = HighlightLines::new(syntax, theme);
            for line in &lines {
              for region in highlighter.highlight(&line) {
                regions.push(region);
              }
            }
          });

          let mut out = out_ref.lock();
          write!(out, "{}", as_24_bit_terminal_escaped(&regions[..], true))
            .unwrap();
        }
      });
    }
  });
}
