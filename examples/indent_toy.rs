extern crate syntect;
extern crate getopts;

use getopts::Options;
use syntect::easy::IndentFile;
use syntect::parsing::SyntaxSet;

fn main() -> Result<(), std::io::Error> {


    let args: Vec<String> = std::env::args().collect();
    let mut opts = Options::new();
    opts.optflag("t", "tabs", "reindent using tabs");
    opts.optopt("s", "spaces", "reindent using spaces", "NUM_SPACES");
    opts.reqopt("p", "extra-syntaxes", "SYNTAX_FOLDER", "Additional folder to search for .sublime-syntax files in.");

    let matches = match opts.parse(&args[1..]) {
        Ok(m) => { m }
        Err(f) => { panic!(f.to_string()) }
    };

    if matches.opt_present("tabs") && matches.opt_present("spaces") {
        panic!("tabs or spaces? (not both)");
    }

    let tab_text = if matches.opt_present("tabs") {
        "\t"
    } else {
        let num_spaces = matches.opt_str("spaces")
        .map(|s| s.parse::<usize>().expect("spaces argument must be an integer"))
        .unwrap_or(4);
        &"                                            "[..num_spaces]
    };

    let mut syntax_set = SyntaxSet::new();
    syntax_set.load_syntaxes(matches.opt_str("extra-syntaxes").unwrap(), false);

    for src in &matches.free[..] {
        if matches.free.len() > 1 {
            println!("==> {} <==", src);
        }

        let mut indenter = IndentFile::new(src, &syntax_set, tab_text)?;
        for line in indenter {
            println!("{}", line);
        }
    }
    Ok(())
}
