//! Prints all the languages (and their file extensions) that have highlighting support.
use syntect::parsing::SyntaxSet;

fn main() {
    let ss = SyntaxSet::load_defaults_newlines();

    for syn in ss.syntaxes().iter() {
        println!("{}: {}", syn.name, syn.file_extensions.join(","));
    }
}
