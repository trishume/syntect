//! Prints highlighted HTML with CSS classes for a Rust and a C++ file to stdout.
//! Run with ```cargo run --example synhtml-css-classes```
use syntect::html::ClassedHTMLGenerator;
use syntect::parsing::SyntaxSet;

fn main() {
    let ss = SyntaxSet::load_defaults_newlines();

    // Rust
    let code_rs = "// Rust source
    fn main() {
        println!(\"Hello World!\");
    }";

    let sr_rs = ss.find_syntax_by_extension("rs").unwrap();
    let mut html_generator = ClassedHTMLGenerator::new(&sr_rs, &ss);
    for line in code_rs.lines() {
        html_generator.parse_html_for_line(&line);
    }
    let html = html_generator.finalize();
    println!("{}", html);

    println!("");

    // C++
    let code_cpp = "// C++ source
    #include <iostream>
    int main() {
        std::cout << \"Hello World!\" << std::endl;
    }";

    let sr_cpp = ss.find_syntax_by_extension("cpp").unwrap();
    let mut html_generator = ClassedHTMLGenerator::new(&sr_cpp, &ss);
    for line in code_cpp.lines() {
        html_generator.parse_html_for_line(&line);
    }
    let html = html_generator.finalize();
    println!("{}", html);
}
