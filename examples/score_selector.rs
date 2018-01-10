//! An example of using syntect for comparing scope stacks against selectors.
//! Useful for debugging color schemes etc. to see which selector has a higher precedence when multiple selectors match the same text/scope stack.
extern crate syntect;
use syntect::highlighting::ScopeSelectors;
use syntect::parsing::ScopeStack;
use std::str::FromStr;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // format the scope selector to include a space at the beginning, because, currently, ScopeSelector expects excludes to begin with " -"
    // but somebody might just type "-punctuation" on the commandline, for example
    let selector = ScopeSelectors::from_str(&format!(" {}", &args[2])).expect("Unable to parse scope selector");
    let scope_stack = ScopeStack::from_str(&args[1]).expect("Unable to parse scope stack");

    println!("Scoring selector \"{}\" against scope stack {:?}...", &args[2], scope_stack.as_slice());
    let result = selector.does_match(scope_stack.as_slice());
    if let Some(match_power) = result {
        println!("{:?}", match_power);
    } else {
        println!("The selector does not match the scope stack.");
    }
}
