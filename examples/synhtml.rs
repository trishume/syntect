//! Prints highlighted HTML for a file to stdout.
//! Basically just wraps a body around `highlighted_snippet_for_file`
extern crate syntect;
extern crate syntect_highlighting as highlighting;
use syntect::dumps::load_default_themeset;
use syntect::parsing::SyntaxSet;
use syntect::html::highlighted_snippet_for_file;

fn main() {
    let ss = SyntaxSet::load_defaults_nonewlines();
    let ts = load_default_themeset();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Please pass in a file to highlight");
        return;
    }

    let style = "
        pre {
            font-size:13px;
            font-family: Consolas, \"Liberation Mono\", Menlo, Courier, monospace;
        }";
    println!("<head><title>{}</title><style>{}</style></head>", &args[1], style);
    let theme = &ts.themes["base16-ocean.dark"];
    let c = theme.settings.background.unwrap_or(highlighting::WHITE);
    println!("<body style=\"background-color:#{:02x}{:02x}{:02x};\">\n", c.r, c.g, c.b);
    let html = highlighted_snippet_for_file(&args[1], &ss, theme).unwrap();
    println!("{}", html);
    println!("</body>");
}
