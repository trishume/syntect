use syntect::highlighting::{HighlightIterator, HighlightState, Highlighter, Style, ThemeSet};
use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};
use syntect::util::{as_latex_escaped, LinesWithEndings};

fn main() {
    // Load these once at the start of your program
    let ps = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();

    let syntax = ps.find_syntax_by_extension("rs").unwrap();
    let s = "pub struct Wow { hi: u64 }\nfn blah() -> u64 {}\n";

    let highlighter = Highlighter::new(&ts.themes["InspiredGitHub"]);
    let mut highlight_state = HighlightState::new(&highlighter, ScopeStack::new());
    let mut parse_state = ParseState::new(syntax);
    for line in LinesWithEndings::from(s) {
        // LinesWithEndings enables use of newlines mode
        let ops = parse_state.parse_line(line, &ps).unwrap().ops;
        let ranges: Vec<(Style, &str)> =
            HighlightIterator::new(&mut highlight_state, &ops[..], line, &highlighter).collect();
        let escaped = as_latex_escaped(&ranges[..]);
        println!("\n{:?}", line);
        println!("\n{}", escaped);
    }
}
