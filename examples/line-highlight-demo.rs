use syntect::highlighting::ThemeSet;
use syntect::html::css_for_theme_with_class_style;
use syntect::html::{ClassStyle, ClassedHTMLGenerator};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

fn highlighted_snippet(
    ss: &SyntaxSet,
    code: &str,
    extension: &str,
    highlighted_lines: &[usize],
    style: ClassStyle,
) -> String {
    let syntax = ss.find_syntax_by_extension(extension).unwrap();
    let mut gen = ClassedHTMLGenerator::new_with_class_style_and_highlighted_lines(
        syntax,
        ss,
        style,
        highlighted_lines,
    );
    for line in LinesWithEndings::from(code) {
        gen.parse_html_for_line_which_includes_newline(line)
            .unwrap();
    }
    gen.finalize()
}

fn main() {
    let ss = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();

    let dark_style = ClassStyle::SpacedPrefixed { prefix: "d-" };
    let light_style = ClassStyle::SpacedPrefixed { prefix: "l-" };

    let dark_theme = &ts.themes["base16-ocean.dark"];
    let light_theme = &ts.themes["InspiredGitHub"];

    let dark_css = css_for_theme_with_class_style(dark_theme, dark_style).unwrap();
    let light_css = css_for_theme_with_class_style(light_theme, light_style).unwrap();

    let rust_code = r#"use std::collections::HashMap;

fn main() {
    let mut scores: HashMap<&str, i32> = HashMap::new();
    scores.insert("Alice", 10);
    scores.insert("Bob", 20);

    for (name, score) in &scores {
        println!("{name}: {score}");
    }
}
"#;

    let python_code = r#"import json
from pathlib import Path

def load_config(path: str) -> dict:
    """Load configuration from a JSON file."""
    with Path(path).open() as f:
        return json.load(f)

config = load_config("settings.json")
print(config)
"#;

    let cpp_code = r#"#include <iostream>
#include <vector>
#include <algorithm>

int main() {
    std::vector<int> nums = {5, 3, 1, 4, 2};
    std::sort(nums.begin(), nums.end());

    for (int n : nums) {
        std::cout << n << " ";
    }
    std::cout << std::endl;
    return 0;
}
"#;

    // Non-adjacent highlights
    let rust_hl = &[1, 4, 8, 9];
    let python_hl = &[1, 2, 6, 7, 10];
    let cpp_hl = &[1, 2, 3, 7, 13];

    let dark_rust = highlighted_snippet(&ss, rust_code, "rs", rust_hl, dark_style);
    let dark_python = highlighted_snippet(&ss, python_code, "py", python_hl, dark_style);
    let dark_cpp = highlighted_snippet(&ss, cpp_code, "cpp", cpp_hl, dark_style);

    let light_rust = highlighted_snippet(&ss, rust_code, "rs", rust_hl, light_style);
    let light_python = highlighted_snippet(&ss, python_code, "py", python_hl, light_style);
    let light_cpp = highlighted_snippet(&ss, cpp_code, "cpp", cpp_hl, light_style);

    let page = format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>syntect line-highlight demo</title>
  <style>
{dark_css}
{light_css}
.d-hl {{
  display: inline-block;
  width: 100%;
  background-color: rgba(255, 255, 255, 0.07);
}}
.l-hl {{
  display: inline-block;
  width: 100%;
  background-color: rgba(255, 220, 50, 0.15);
}}
* {{ box-sizing: border-box; }}
body {{
  font-family: system-ui, -apple-system, sans-serif;
  margin: 0;
  padding: 2rem 1rem;
}}
.container {{
  max-width: 1200px;
  margin: 0 auto;
}}
h1 {{
  text-align: center;
  font-weight: 300;
  margin-bottom: 0.25rem;
}}
.subtitle {{
  text-align: center;
  color: #888;
  margin-bottom: 2rem;
}}
.columns {{
  display: flex;
  gap: 1.5rem;
}}
.col {{
  flex: 1;
  min-width: 0;
}}
.col h2 {{
  text-align: center;
  font-weight: 400;
  margin-top: 0;
  padding-bottom: 0.5rem;
}}
.dark-col {{
  background: #1b2b34;
  color: #c0c5ce;
  border-radius: 12px;
  padding: 1.5rem;
}}
.dark-col h2 {{ color: #a7adba; }}
.dark-col h3 {{ color: #65737e; font-weight: 400; }}
.dark-col .caption {{ color: #65737e; }}
.light-col {{
  background: #f8f8f8;
  color: #333;
  border-radius: 12px;
  padding: 1.5rem;
  border: 1px solid #e0e0e0;
}}
.light-col h2 {{ color: #555; }}
.light-col h3 {{ color: #888; font-weight: 400; }}
.light-col .caption {{ color: #888; }}
pre.code {{
  border-radius: 6px;
  padding: 1rem;
  overflow-x: auto;
  font-size: 13px;
  line-height: 1.5;
  margin: 0.5rem 0;
}}
h3 {{
  margin-bottom: 0.25rem;
}}
.caption {{
  font-size: 0.8rem;
  margin-top: 0.25rem;
  margin-bottom: 1.25rem;
}}
@media (max-width: 800px) {{
  .columns {{ flex-direction: column; }}
}}
  </style>
</head>
<body>
  <div class="container">
    <h1>syntect line-highlight demo</h1>
    <p class="subtitle">Non-adjacent line highlighting with dark and light themes</p>

    <div class="columns">
      <div class="col dark-col">
        <h2>Dark (base16-ocean)</h2>

        <h3>Rust</h3>
        <pre class="d-code">{dark_rust}</pre>
        <p class="caption">Lines 1, 4, 8, 9 (imports, init, iteration)</p>

        <h3>Python</h3>
        <pre class="d-code">{dark_python}</pre>
        <p class="caption">Lines 1, 2, 6, 7, 10 (imports, body, usage)</p>

        <h3>C++</h3>
        <pre class="d-code">{dark_cpp}</pre>
        <p class="caption">Lines 1, 2, 3, 7, 13 (includes, sort, return)</p>
      </div>

      <div class="col light-col">
        <h2>Light (InspiredGitHub)</h2>

        <h3>Rust</h3>
        <pre class="l-code">{light_rust}</pre>
        <p class="caption">Lines 1, 4, 8, 9 (imports, init, iteration)</p>

        <h3>Python</h3>
        <pre class="l-code">{light_python}</pre>
        <p class="caption">Lines 1, 2, 6, 7, 10 (imports, body, usage)</p>

        <h3>C++</h3>
        <pre class="l-code">{light_cpp}</pre>
        <p class="caption">Lines 1, 2, 3, 7, 13 (includes, sort, return)</p>
      </div>
    </div>
  </div>
</body>
</html>
"##,
        dark_css = dark_css,
        light_css = light_css,
        dark_rust = dark_rust,
        dark_python = dark_python,
        dark_cpp = dark_cpp,
        light_rust = light_rust,
        light_python = light_python,
        light_cpp = light_cpp,
    );

    let out_path = "line-highlight-demo.html";
    std::fs::write(out_path, page).unwrap();
    println!("Wrote {out_path}");
}
