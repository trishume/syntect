//! Rendering highlighted code as HTML+CSS
use crate::escape::Escape;
use crate::highlighting::{Color, FontStyle, Style, Theme};
use crate::io::HighlightedWriter;
use crate::parsing::{
    lock_global_scope_repo, Scope, ScopeRepository, ScopeStack, ScopeStackOp, SyntaxReference,
    SyntaxSet,
};
use crate::rendering::{render_line, MarkupAdapter, ScopeMarkup, StyledOutput};
use crate::Error;
use std::fmt::Write;

use std::io::Write as IoWrite;
use std::path::Path;

/// An HTML renderer that produces `<span class="...">` elements with
/// CSS class names derived from scope atoms.
pub struct ClassedHTMLScopeRenderer {
    style: ClassStyle,
}

impl ClassedHTMLScopeRenderer {
    pub fn new(style: ClassStyle) -> Self {
        Self { style }
    }
}

impl ScopeMarkup for ClassedHTMLScopeRenderer {
    fn begin_scope(&mut self, atom_strs: &[&str], output: &mut String) {
        output.push_str("<span class=\"");
        for (i, atom) in atom_strs.iter().enumerate() {
            if i != 0 {
                output.push(' ');
            }
            if let ClassStyle::SpacedPrefixed { prefix } = self.style {
                output.push_str(prefix);
            }
            output.push_str(atom);
        }
        output.push_str("\">");
    }

    fn end_scope(&mut self, output: &mut String) {
        output.push_str("</span>");
    }

    fn write_text(&mut self, text: &str, output: &mut String) {
        write!(output, "{}", Escape(text)).expect("writing to a String never fails");
    }
}

/// HTML-specific convenience wrapper around [`HighlightedWriter`].
///
/// Uses [`ClassedHTMLScopeRenderer`] to produce `<span class="...">` output with
/// CSS class names derived from scope atoms.
///
/// Note that because CSS classes have slightly different matching semantics
/// than Textmate themes, this may produce somewhat less accurate
/// highlighting than the other highlighting functions which directly use
/// inline colors as opposed to classes and a stylesheet.
///
/// There is a [`finalize()`] method that must be called in the end in order
/// to close all open `<span>` tags.
///
/// [`finalize()`]: ClassedHTMLGenerator::finalize
///
/// # Example
///
/// ```
/// use syntect::html::{ClassedHTMLGenerator, ClassStyle};
/// use syntect::parsing::SyntaxSet;
/// use syntect::util::LinesWithEndings;
///
/// let current_code = r#"
/// x <- 5
/// y <- 6
/// x + y
/// "#;
///
/// let syntax_set = SyntaxSet::load_defaults_newlines();
/// let syntax = syntax_set.find_syntax_by_name("R").unwrap();
/// let mut html_generator = ClassedHTMLGenerator::new_with_class_style(syntax, &syntax_set, ClassStyle::Spaced);
/// for line in LinesWithEndings::from(current_code) {
///     html_generator.parse_html_for_line_which_includes_newline(line);
/// }
/// let output_html = html_generator.finalize();
/// ```
pub struct ClassedHTMLGenerator<'a> {
    inner: HighlightedWriter<'a, MarkupAdapter<ClassedHTMLScopeRenderer>>,
}

impl<'a> ClassedHTMLGenerator<'a> {
    #[deprecated(since = "4.2.0", note = "Please use `new_with_class_style` instead")]
    pub fn new(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
    ) -> ClassedHTMLGenerator<'a> {
        Self::new_with_class_style(syntax_reference, syntax_set, ClassStyle::Spaced)
    }

    pub fn new_with_class_style(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        style: ClassStyle,
    ) -> ClassedHTMLGenerator<'a> {
        ClassedHTMLGenerator {
            inner: HighlightedWriter::from_markup(
                syntax_reference,
                syntax_set,
                ClassedHTMLScopeRenderer::new(style),
            )
            .build(),
        }
    }

    /// Parse the line of code and update the internal HTML buffer with tagged HTML
    ///
    /// *Note:* This function requires `line` to include a newline at the end and
    /// also use of the `load_defaults_newlines` version of the syntaxes.
    pub fn parse_html_for_line_which_includes_newline(&mut self, line: &str) -> Result<(), Error> {
        self.inner.write_all(line.as_bytes())?;
        Ok(())
    }

    /// Parse the line of code and update the internal HTML buffer with tagged HTML
    ///
    /// ## Warning
    /// Due to an unfortunate oversight this function adds a newline after the HTML line,
    /// and thus requires lines to be passed without newlines in them, and thus requires
    /// usage of the `load_defaults_nonewlines` version of the default syntaxes.
    ///
    /// These versions of the syntaxes can have occasionally incorrect highlighting
    /// but this function can't be changed without breaking compatibility so is deprecated.
    #[deprecated(
        since = "4.5.0",
        note = "Please use `parse_html_for_line_which_includes_newline` instead"
    )]
    pub fn parse_html_for_line(&mut self, line: &str) {
        // The deprecated quirk: append a newline so the renderer treats the
        // input as a complete line and the output ends with `\n`.
        let mut owned = String::with_capacity(line.len() + 1);
        owned.push_str(line);
        owned.push('\n');
        self.inner
            .write_all(owned.as_bytes())
            .expect("Please use `parse_html_for_line_which_includes_newline` instead");
    }

    /// Close any remaining open `<span>` tags and return the finished HTML.
    pub fn finalize(self) -> String {
        let bytes = self
            .inner
            .into_inner()
            .expect("renderer produces valid UTF-8");
        String::from_utf8(bytes).expect("renderer produces valid UTF-8")
    }
}

#[deprecated(
    since = "4.2.0",
    note = "Please use `css_for_theme_with_class_style` instead."
)]
pub fn css_for_theme(theme: &Theme) -> String {
    css_for_theme_with_class_style(theme, ClassStyle::Spaced)
        .expect("Please use `css_for_theme_with_class_style` instead.")
}

/// Create a complete CSS for a given theme. Can be used inline, or written to a CSS file.
pub fn css_for_theme_with_class_style(theme: &Theme, style: ClassStyle) -> Result<String, Error> {
    let mut css = String::new();

    css.push_str("/*\n");
    let name = theme
        .name
        .clone()
        .unwrap_or_else(|| "unknown theme".to_string());
    css.push_str(&format!(" * theme \"{}\" generated by syntect\n", name));
    css.push_str(" */\n\n");

    match style {
        ClassStyle::Spaced => {
            css.push_str(".code {\n");
        }
        ClassStyle::SpacedPrefixed { prefix } => {
            let class = escape_css_identifier(&format!("{}code", prefix));
            css.push_str(&format!(".{} {{\n", class));
        }
    };
    if let Some(fgc) = theme.settings.foreground {
        css.push_str(&format!(
            " color: #{:02x}{:02x}{:02x};\n",
            fgc.r, fgc.g, fgc.b
        ));
    }
    if let Some(bgc) = theme.settings.background {
        css.push_str(&format!(
            " background-color: #{:02x}{:02x}{:02x};\n",
            bgc.r, bgc.g, bgc.b
        ));
    }
    css.push_str("}\n\n");

    let repo = lock_global_scope_repo();
    for i in &theme.scopes {
        for scope_selector in &i.scope.selectors {
            let scopes = scope_selector.extract_scopes();
            for k in &scopes {
                scope_to_selector(&mut css, *k, style, &repo);
                css.push(' '); // join multiple scopes
            }
            css.pop(); // remove trailing space
            css.push_str(", "); // join multiple selectors
        }
        let len = css.len();
        css.truncate(len - 2); // remove trailing ", "
        css.push_str(" {\n");

        if let Some(fg) = i.style.foreground {
            css.push_str(&format!(" color: #{:02x}{:02x}{:02x};\n", fg.r, fg.g, fg.b));
        }

        if let Some(bg) = i.style.background {
            css.push_str(&format!(
                " background-color: #{:02x}{:02x}{:02x};\n",
                bg.r, bg.g, bg.b
            ));
        }

        if let Some(fs) = i.style.font_style {
            if fs.contains(FontStyle::UNDERLINE) {
                css.push_str("text-decoration: underline;\n");
            }
            if fs.contains(FontStyle::BOLD) {
                css.push_str("font-weight: bold;\n");
            }
            if fs.contains(FontStyle::ITALIC) {
                css.push_str("font-style: italic;\n");
            }
        }
        css.push_str("}\n");
    }

    Ok(css)
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
#[non_exhaustive]
pub enum ClassStyle {
    /// The classes are the atoms of the scope separated by spaces
    /// (e.g `source.php` becomes `source php`).
    /// This isn't that fast since it has to use the scope repository
    /// to look up scope names.
    Spaced,
    /// Like `Spaced`, but the given prefix will be prepended to all
    /// classes. This is useful to prevent class name collisions, and
    /// can ensure that the theme's CSS applies precisely to syntect's
    /// output.
    ///
    /// The prefix must be a valid CSS class name. To help ennforce
    /// this invariant and prevent accidental foot-shooting, it must
    /// be statically known. (If this requirement is onerous, please
    /// file an issue; the HTML generator can also be forked
    /// separately from the rest of syntect, as it only uses the
    /// public API.)
    SpacedPrefixed { prefix: &'static str },
}

fn scope_to_selector(s: &mut String, scope: Scope, style: ClassStyle, repo: &ScopeRepository) {
    for i in 0..(scope.len()) {
        let atom = scope.atom_at(i as usize);
        let atom_s = repo.atom_str(atom);
        s.push('.');
        let mut class = String::new();
        match style {
            ClassStyle::Spaced => {}
            ClassStyle::SpacedPrefixed { prefix } => {
                class.push_str(prefix);
            }
        }
        class.push_str(atom_s);
        s.push_str(&escape_css_identifier(&class));
    }
}

/// Escape special characters in a CSS identifier.
///
/// See <https://www.w3.org/International/questions/qa-escapes#css_identifiers>.
fn escape_css_identifier(identifier: &str) -> String {
    identifier.char_indices().fold(
        String::with_capacity(identifier.len()),
        |mut output, (i, c)| {
            if c.is_ascii_alphabetic() || c == '-' || c == '_' || (i > 0 && c.is_ascii_digit()) {
                output.push(c);
            } else {
                output.push_str(&format!("\\{:x} ", c as u32));
            }
            output
        },
    )
}

/// Output HTML for a line of code with `<span>` elements
/// specifying classes for each token. The span elements are nested
/// like the scope stack and the scopes are mapped to classes based
/// on the `ClassStyle` (see it's docs).
///
/// **Deprecated.** This function cannot correctly handle cross-line
/// branch-point failures: when the parser retroactively replays a span of
/// previously-emitted ops, the rendered output for those lines has already
/// been written to the caller and cannot be retracted. Use
/// [`ClassedHTMLGenerator`] (or [`crate::io::HighlightedWriter::from_markup`]
/// directly) instead — both buffer rendered output during speculative
/// parsing and replay corrected ops once speculation resolves.
///
/// Returns the HTML string and the number of `<span>` tags opened
/// (negative for closed). So that you can emit the correct number of closing
/// tags at the end.
#[deprecated(
    since = "6.0.0",
    note = "Cannot correctly handle cross-line branch-point failures. Use `ClassedHTMLGenerator` or `HighlightedWriter::from_markup` instead, which buffer rendered output during speculative parsing and replay corrected ops."
)]
pub fn line_tokens_to_classed_spans(
    line: &str,
    ops: &[(usize, ScopeStackOp)],
    style: ClassStyle,
    stack: &mut ScopeStack,
) -> Result<(String, isize), Error> {
    let mut renderer = MarkupAdapter::new(ClassedHTMLScopeRenderer::new(style));
    render_line(line, ops, stack, &mut renderer, 0)
}

// ---------------------------------------------------------------------------
// HtmlStyledOutput — inline-styled HTML via StyledOutput
// ---------------------------------------------------------------------------

/// A [`StyledOutput`] that produces `<span style="...">` elements with
/// inline CSS resolved from a theme.
///
/// Wrap with [`crate::rendering::ThemedRenderer`] to use it as a
/// [`crate::rendering::ScopeRenderer`], or pass directly to
/// [`HighlightedWriter::from_themed`].
///
/// `default_bg` is the background colour of the containing element; the
/// `background-color` CSS property is only emitted when the token's
/// background differs from this default.
///
/// Adjacent text tokens with the same resolved [`Style`] are automatically
/// merged into a single `<span>`. Whitespace-only tokens additionally fold
/// into the previous span when their backgrounds match, even when the
/// foreground differs — whitespace doesn't reveal a foreground difference.
pub struct HtmlStyledOutput {
    default_bg: Color,
}

impl HtmlStyledOutput {
    /// Create a new inline HTML emitter.
    ///
    /// `default_bg` is the background colour of the containing element.
    pub fn new(default_bg: Color) -> Self {
        Self { default_bg }
    }

    /// Returns the configured default background colour.
    pub fn default_bg(&self) -> Color {
        self.default_bg
    }
}

impl StyledOutput for HtmlStyledOutput {
    fn begin_style(&mut self, style: Style, output: &mut String) {
        output.push_str("<span style=\"");
        if style.background != self.default_bg {
            output.push_str("background-color:");
            write_css_color(output, style.background);
            output.push(';');
        }
        if style.font_style.contains(FontStyle::UNDERLINE) {
            output.push_str("text-decoration:underline;");
        }
        if style.font_style.contains(FontStyle::BOLD) {
            output.push_str("font-weight:bold;");
        }
        if style.font_style.contains(FontStyle::ITALIC) {
            output.push_str("font-style:italic;");
        }
        output.push_str("color:");
        write_css_color(output, style.foreground);
        output.push_str(";\">");
    }

    fn end_style(&mut self, output: &mut String) {
        output.push_str("</span>");
    }

    fn write_text(&mut self, text: &str, output: &mut String) {
        write!(output, "{}", Escape(text)).expect("writing to a String never fails");
    }

    fn should_merge(&self, prev: Style, next: Style, text: &str) -> bool {
        // Merge into the previous span when styles match exactly, or when
        // the text is only whitespace and the background colours agree.
        // Whitespace reveals no foreground difference, so collapsing the
        // boundary is safe and produces smaller output.
        prev == next || (prev.background == next.background && text.trim().is_empty())
    }
}

/// Convenience method that creates a full highlighted HTML snippet for
/// a string (which can contain many lines), using inline `style` attributes.
///
/// Uses [`HighlightedWriter`] with [`HtmlStyledOutput`] internally,
/// which correctly handles branch-point backtracking.
///
/// Note that the `syntax` passed in must be from a `SyntaxSet` compiled for newline characters.
/// This is easy to get with `SyntaxSet::load_defaults_newlines()`. (Note: this was different before v3.0)
pub fn highlighted_html_for_string(
    s: &str,
    ss: &SyntaxSet,
    syntax: &SyntaxReference,
    theme: &Theme,
) -> Result<String, Error> {
    let (mut output, bg) = start_highlighted_html_snippet(theme);
    let mut w =
        HighlightedWriter::from_themed(syntax, ss, theme, HtmlStyledOutput::new(bg)).build();
    w.write_all(s.as_bytes())?;
    output.push_str(&String::from_utf8(w.into_inner()?).expect("renderer produces valid UTF-8"));
    output.push_str("</pre>\n");
    Ok(output)
}

/// Convenience method that creates a full highlighted HTML snippet for
/// a file, using inline `style` attributes.
///
/// Uses [`HighlightedWriter`] with [`HtmlStyledOutput`] internally,
/// which correctly handles branch-point backtracking.
///
/// Note that the `syntax` passed in must be from a `SyntaxSet` compiled for newline characters.
/// This is easy to get with `SyntaxSet::load_defaults_newlines()`. (Note: this was different before v3.0)
pub fn highlighted_html_for_file<P: AsRef<Path>>(
    path: P,
    ss: &SyntaxSet,
    theme: &Theme,
) -> Result<String, Error> {
    let path_ref: &Path = path.as_ref();
    let mut f = std::fs::File::open(path_ref)?;
    let syntax = ss
        .find_syntax_for_file(path_ref)?
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let (mut output, bg) = start_highlighted_html_snippet(theme);
    let mut w =
        HighlightedWriter::from_themed(syntax, ss, theme, HtmlStyledOutput::new(bg)).build();
    std::io::copy(&mut f, &mut w)?;
    output.push_str(&String::from_utf8(w.into_inner()?).expect("renderer produces valid UTF-8"));
    output.push_str("</pre>\n");
    Ok(output)
}

/// Preserved for compatibility. Use [`ClassedHTMLGenerator`] instead.
#[deprecated(
    since = "4.6.0",
    note = "Use `ClassedHTMLGenerator` instead, this can panic and highlight incorrectly and cannot handle cross-line branch-point failures"
)]
#[allow(deprecated)]
pub fn tokens_to_classed_spans(
    line: &str,
    ops: &[(usize, ScopeStackOp)],
    style: ClassStyle,
) -> (String, isize) {
    line_tokens_to_classed_spans(line, ops, style, &mut ScopeStack::new())
        .expect("Use `ClassedHTMLGenerator` instead")
}

#[deprecated(
    since = "3.1.0",
    note = "Use `ClassedHTMLGenerator` instead to avoid incorrect highlighting, panics, and cross-line branch-point failures"
)]
#[allow(deprecated)]
pub fn tokens_to_classed_html(
    line: &str,
    ops: &[(usize, ScopeStackOp)],
    style: ClassStyle,
) -> String {
    line_tokens_to_classed_spans(line, ops, style, &mut ScopeStack::new())
        .expect("Use `ClassedHTMLGenerator` instead")
        .0
}

/// Determines how background color attributes are generated
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum IncludeBackground {
    /// Don't include `background-color`, for performance or so that you can use your own background.
    No,
    /// Set background color attributes on every node
    Yes,
    /// Only set the `background-color` if it is different than the default (presumably set on a parent element)
    IfDifferent(Color),
}

fn write_css_color(s: &mut String, c: Color) {
    if c.a != 0xFF {
        write!(s, "#{:02x}{:02x}{:02x}{:02x}", c.r, c.g, c.b, c.a).unwrap();
    } else {
        write!(s, "#{:02x}{:02x}{:02x}", c.r, c.g, c.b).unwrap();
    }
}

/// Output HTML for a line of code with `<span>` elements using inline
/// `style` attributes to set the correct font attributes.
/// The `bg` attribute determines if the spans will have the `background-color`
/// attribute set. See the `IncludeBackground` enum's docs.
///
/// The lines returned don't include a newline at the end.
/// # Examples
///
/// ```
/// use syntect::highlighting::{HighlightIterator, HighlightState, Highlighter, Style, ThemeSet};
/// use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};
/// use syntect::html::{styled_line_to_highlighted_html, IncludeBackground};
///
/// let ps = SyntaxSet::load_defaults_newlines();
/// let ts = ThemeSet::load_defaults();
///
/// let syntax = ps.find_syntax_by_name("Ruby").unwrap();
/// let mut parse_state = ParseState::new(syntax);
/// let highlighter = Highlighter::new(&ts.themes["base16-ocean.dark"]);
/// let mut highlight_state = HighlightState::new(&highlighter, ScopeStack::new());
/// let ops = parse_state.parse_line("5", &ps).unwrap().ops;
/// let iter = HighlightIterator::new(&mut highlight_state, &ops, "5", &highlighter);
/// let regions: Vec<(Style, &str)> = iter.collect();
/// let html = styled_line_to_highlighted_html(&regions[..], IncludeBackground::No).unwrap();
/// assert_eq!(html, "<span style=\"color:#d08770;\">5</span>");
/// ```
pub fn styled_line_to_highlighted_html(
    v: &[(Style, &str)],
    bg: IncludeBackground,
) -> Result<String, Error> {
    let mut s: String = String::new();
    append_highlighted_html_for_styled_line(v, bg, &mut s)?;
    Ok(s)
}

/// Like `styled_line_to_highlighted_html` but appends to a `String` for increased efficiency.
/// In fact `styled_line_to_highlighted_html` is just a wrapper around this function.
pub fn append_highlighted_html_for_styled_line(
    v: &[(Style, &str)],
    bg: IncludeBackground,
    s: &mut String,
) -> Result<(), Error> {
    let mut prev_style: Option<&Style> = None;
    for &(ref style, text) in v.iter() {
        let unify_style = if let Some(ps) = prev_style {
            style == ps || (style.background == ps.background && text.trim().is_empty())
        } else {
            false
        };
        if unify_style {
            write!(s, "{}", Escape(text))?;
        } else {
            if prev_style.is_some() {
                write!(s, "</span>")?;
            }
            prev_style = Some(style);
            write!(s, "<span style=\"")?;
            let include_bg = match bg {
                IncludeBackground::Yes => true,
                IncludeBackground::No => false,
                IncludeBackground::IfDifferent(c) => style.background != c,
            };
            if include_bg {
                write!(s, "background-color:")?;
                write_css_color(s, style.background);
                write!(s, ";")?;
            }
            if style.font_style.contains(FontStyle::UNDERLINE) {
                write!(s, "text-decoration:underline;")?;
            }
            if style.font_style.contains(FontStyle::BOLD) {
                write!(s, "font-weight:bold;")?;
            }
            if style.font_style.contains(FontStyle::ITALIC) {
                write!(s, "font-style:italic;")?;
            }
            write!(s, "color:")?;
            write_css_color(s, style.foreground);
            write!(s, ";\">{}", Escape(text))?;
        }
    }
    if prev_style.is_some() {
        write!(s, "</span>")?;
    }

    Ok(())
}

/// Returns a `<pre style="...">\n` tag with the correct background color for the given theme.
/// This is for if you want to roll your own HTML output, you probably just want to use
/// `highlighted_html_for_string`.
///
/// If you don't care about the background color you can just prefix the lines from
/// `styled_line_to_highlighted_html` with a `<pre>`. This is meant to be used with
/// `IncludeBackground::IfDifferent`.
///
/// As of `v3.0` this method also returns the background color to be passed to `IfDifferent`.
///
/// You're responsible for creating the string `</pre>` to close this, I'm not gonna provide a
/// helper for that :-)
pub fn start_highlighted_html_snippet(t: &Theme) -> (String, Color) {
    let c = t.settings.background.unwrap_or(Color::WHITE);
    (
        format!(
            "<pre style=\"background-color:#{:02x}{:02x}{:02x};\">\n",
            c.r, c.g, c.b
        ),
        c,
    )
}

#[cfg(all(feature = "default-syntaxes", feature = "default-themes",))]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlighting::{HighlightIterator, HighlightState, Highlighter, Style, ThemeSet};
    use crate::parsing::{ParseState, ScopeStack, SyntaxDefinition, SyntaxSet, SyntaxSetBuilder};
    use crate::util::LinesWithEndings;
    #[test]
    #[allow(deprecated)]
    fn tokens() {
        let ss = SyntaxSet::load_defaults_newlines();
        let syntax = ss.find_syntax_by_name("Markdown").unwrap();
        let mut state = ParseState::new(syntax);
        let line = "[w](t.co) *hi* **five**";
        let ops = state.parse_line(line, &ss).expect("#[cfg(test)]").ops;
        let mut stack = ScopeStack::new();

        // use util::debug_print_ops;
        // debug_print_ops(line, &ops);

        let (html, _) =
            line_tokens_to_classed_spans(line, &ops[..], ClassStyle::Spaced, &mut stack)
                .expect("#[cfg(test)]");
        println!("{}", html);
        assert_eq!(html, include_str!("../testdata/test2.html").trim_end());

        let ts = ThemeSet::load_defaults();
        let highlighter = Highlighter::new(&ts.themes["InspiredGitHub"]);
        let mut highlight_state = HighlightState::new(&highlighter, ScopeStack::new());
        let iter = HighlightIterator::new(&mut highlight_state, &ops[..], line, &highlighter);
        let regions: Vec<(Style, &str)> = iter.collect();

        let html2 = styled_line_to_highlighted_html(&regions[..], IncludeBackground::Yes)
            .expect("#[cfg(test)]");
        println!("{}", html2);
        assert_eq!(html2, include_str!("../testdata/test1.html").trim_end());
    }

    #[test]
    fn strings() {
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let s = include_str!("../testdata/highlight_test.erb");
        let syntax = ss.find_syntax_by_extension("erb").unwrap();
        let html = highlighted_html_for_string(s, &ss, syntax, &ts.themes["base16-ocean.dark"])
            .expect("#[cfg(test)]");
        // println!("{}", html);
        assert_eq!(html, include_str!("../testdata/test3.html"));
        let html2 = highlighted_html_for_file(
            "testdata/highlight_test.erb",
            &ss,
            &ts.themes["base16-ocean.dark"],
        )
        .unwrap();
        assert_eq!(html2, html);

        // YAML is a tricky syntax and InspiredGitHub is a fancy theme, this is basically an integration test
        let html3 = highlighted_html_for_file(
            "testdata/Packages/Rust/Cargo.sublime-syntax",
            &ss,
            &ts.themes["InspiredGitHub"],
        )
        .unwrap();
        println!("{}", html3);
        assert_eq!(html3, include_str!("../testdata/test4.html"));
    }

    #[test]
    fn tricky_test_syntax() {
        // This syntax I wrote tests edge cases of prototypes
        // I verified the output HTML against what ST3 does with the same syntax and file
        let mut builder = SyntaxSetBuilder::new();
        builder.add_from_folder("testdata", true).unwrap();
        let ss = builder.build();
        let ts = ThemeSet::load_defaults();
        let html = highlighted_html_for_file(
            "testdata/testing-syntax.testsyntax",
            &ss,
            &ts.themes["base16-ocean.dark"],
        )
        .unwrap();
        println!("{}", html);
        assert_eq!(html, include_str!("../testdata/test5.html"));
    }

    #[test]
    fn test_classed_html_generator_doesnt_panic() {
        let current_code = "{\n    \"headers\": [\"Number\", \"Title\"],\n    \"records\": [\n        [\"1\", \"Gutenberg\"],\n        [\"2\", \"Printing\"]\n    ],\n}\n";
        let syntax_def = SyntaxDefinition::load_from_str(
            include_str!("../testdata/JSON.sublime-syntax"),
            true,
            None,
        )
        .unwrap();
        let mut syntax_set_builder = SyntaxSetBuilder::new();
        syntax_set_builder.add(syntax_def);
        let syntax_set = syntax_set_builder.build();
        let syntax = syntax_set.find_syntax_by_name("JSON").unwrap();

        let mut html_generator =
            ClassedHTMLGenerator::new_with_class_style(syntax, &syntax_set, ClassStyle::Spaced);
        for line in LinesWithEndings::from(current_code) {
            html_generator
                .parse_html_for_line_which_includes_newline(line)
                .expect("#[cfg(test)]");
        }
        html_generator.finalize();
    }

    #[test]
    fn test_classed_html_generator() {
        let current_code = "x + y\n";
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let syntax = syntax_set.find_syntax_by_name("R").unwrap();

        let mut html_generator =
            ClassedHTMLGenerator::new_with_class_style(syntax, &syntax_set, ClassStyle::Spaced);
        for line in LinesWithEndings::from(current_code) {
            html_generator
                .parse_html_for_line_which_includes_newline(line)
                .expect("#[cfg(test)]");
        }
        let html = html_generator.finalize();
        assert_eq!(html, "<span class=\"source r\">x <span class=\"keyword operator arithmetic r\">+</span> y\n</span>");
    }

    #[test]
    fn test_classed_html_generator_prefixed() {
        let current_code = "x + y\n";
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let syntax = syntax_set.find_syntax_by_name("R").unwrap();
        let mut html_generator = ClassedHTMLGenerator::new_with_class_style(
            syntax,
            &syntax_set,
            ClassStyle::SpacedPrefixed { prefix: "foo-" },
        );
        for line in LinesWithEndings::from(current_code) {
            html_generator
                .parse_html_for_line_which_includes_newline(line)
                .expect("#[cfg(test)]");
        }
        let html = html_generator.finalize();
        assert_eq!(html, "<span class=\"foo-source foo-r\">x <span class=\"foo-keyword foo-operator foo-arithmetic foo-r\">+</span> y\n</span>");
    }

    #[test]
    fn test_classed_html_generator_no_empty_span() {
        let code = "// Rust source
fn main() {
    println!(\"Hello World!\");
}
";
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let syntax = syntax_set.find_syntax_by_extension("rs").unwrap();
        let mut html_generator =
            ClassedHTMLGenerator::new_with_class_style(syntax, &syntax_set, ClassStyle::Spaced);
        for line in LinesWithEndings::from(code) {
            html_generator
                .parse_html_for_line_which_includes_newline(line)
                .expect("#[cfg(test)]");
        }
        let html = html_generator.finalize();
        assert_eq!(html, "<span class=\"source rust\"><span class=\"comment line double-slash rust\"><span class=\"punctuation definition comment rust\">//</span> Rust source\n</span><span class=\"meta function rust\"><span class=\"meta function rust\"><span class=\"storage type function rust\">fn</span> </span><span class=\"entity name function rust\">main</span></span><span class=\"meta function rust\"><span class=\"meta function parameters rust\"><span class=\"punctuation section parameters begin rust\">(</span></span><span class=\"meta function rust\"><span class=\"meta function parameters rust\"><span class=\"punctuation section parameters end rust\">)</span></span></span></span><span class=\"meta function rust\"> </span><span class=\"meta function rust\"><span class=\"meta block rust\"><span class=\"punctuation section block begin rust\">{</span>\n    <span class=\"support macro rust\">println!</span><span class=\"meta group rust\"><span class=\"punctuation section group begin rust\">(</span></span><span class=\"meta group rust\"><span class=\"string quoted double rust\"><span class=\"punctuation definition string begin rust\">&quot;</span>Hello World!<span class=\"punctuation definition string end rust\">&quot;</span></span></span><span class=\"meta group rust\"><span class=\"punctuation section group end rust\">)</span></span><span class=\"punctuation terminator rust\">;</span>\n</span><span class=\"meta block rust\"><span class=\"punctuation section block end rust\">}</span></span></span>\n</span>");
    }

    #[test]
    fn test_escape_css_identifier() {
        assert_eq!(&escape_css_identifier("abc"), "abc");
        assert_eq!(&escape_css_identifier("123"), "\\31 23");
        assert_eq!(&escape_css_identifier("c++"), "c\\2b \\2b ");
    }

    /// See issue [syntect#308](<https://github.com/trishume/syntect/issues/308>).
    #[test]
    fn test_css_for_theme_with_class_style_issue_308() {
        let theme_set = ThemeSet::load_defaults();
        let theme = theme_set.themes.get("Solarized (dark)").unwrap();
        let css = css_for_theme_with_class_style(theme, ClassStyle::Spaced).unwrap();
        assert!(!css.contains(".c++"));
        assert!(css.contains(".c\\2b \\2b "));
    }

    #[test]
    fn test_custom_renderer_receives_atom_strs() {
        use crate::rendering::ScopeMarkup;
        use std::cell::RefCell;

        struct CapturingMarkup {
            captured: RefCell<Vec<Vec<String>>>,
        }
        impl ScopeMarkup for CapturingMarkup {
            fn begin_scope(&mut self, atom_strs: &[&str], output: &mut String) {
                self.captured
                    .borrow_mut()
                    .push(atom_strs.iter().map(|s| s.to_string()).collect());
                output.push_str("<span>");
            }
            fn end_scope(&mut self, output: &mut String) {
                output.push_str("</span>");
            }
        }

        let code = "x + y\n";
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let syntax = syntax_set.find_syntax_by_name("R").unwrap();
        let markup = CapturingMarkup {
            captured: RefCell::new(Vec::new()),
        };
        let mut gen =
            crate::io::HighlightedWriter::from_markup(syntax, &syntax_set, markup).build();
        gen.write_all(code.as_bytes()).expect("#[cfg(test)]");

        // The R syntax should produce "source r" as the first scope
        let captured = gen.renderer().inner().captured.borrow().clone();
        assert!(!captured.is_empty());
        assert_eq!(captured[0], vec!["source", "r"]);

        gen.into_inner().expect("#[cfg(test)]");
    }

    #[test]
    fn test_scope_with_atom_strs() {
        let scope = Scope::new("keyword.operator.arithmetic").unwrap();
        let atoms: Vec<String> =
            scope.with_atom_strs(|atoms| atoms.iter().map(|s| s.to_string()).collect());
        assert_eq!(atoms, vec!["keyword", "operator", "arithmetic"]);
    }
}
