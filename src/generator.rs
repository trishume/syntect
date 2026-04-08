//! Generic document generator that drives syntax parsing and rendering.
//!
//! [`DocumentGenerator`] pairs a syntax parser with a [`ScopeRenderer`] to
//! produce highlighted output in any format. It is format-agnostic: the
//! output format is determined entirely by the renderer.
//!
//! For an HTML-specific convenience wrapper, see
//! [`crate::html::ClassedHTMLGenerator`].

use crate::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use crate::renderer::{render_line, ScopeRenderer};
use crate::Error;

/// Drives syntax parsing and delegates rendering to a [`ScopeRenderer`].
///
/// This struct parses lines of code and emits rendering events (scope push/pop,
/// text content, line boundaries) to a pluggable renderer. The output format
/// is determined entirely by the `R` parameter.
///
/// There is a [`finalize()`] method that must be called in the end in order
/// to close any open scopes.
///
/// [`finalize()`]: #method.finalize
///
/// # Example
///
/// ```
/// use syntect::generator::DocumentGenerator;
/// use syntect::html::HTMLScopeRenderer;
/// use syntect::html::ClassStyle;
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
/// let renderer = HTMLScopeRenderer::new(ClassStyle::Spaced);
/// let mut generator = DocumentGenerator::new(syntax, &syntax_set, renderer);
/// for line in LinesWithEndings::from(current_code) {
///     generator.parse_line(line);
/// }
/// let output = generator.finalize();
/// ```
pub struct DocumentGenerator<'a, R: ScopeRenderer> {
    syntax_set: &'a SyntaxSet,
    open_scopes: isize,
    parse_state: ParseState,
    scope_stack: ScopeStack,
    output: String,
    renderer: R,
    line_index: usize,
}

impl<'a, R: ScopeRenderer> DocumentGenerator<'a, R> {
    /// Create a new document generator with a custom renderer.
    pub fn new(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        renderer: R,
    ) -> DocumentGenerator<'a, R> {
        DocumentGenerator {
            syntax_set,
            open_scopes: 0,
            parse_state: ParseState::new(syntax_reference),
            scope_stack: ScopeStack::new(),
            output: String::new(),
            renderer,
            line_index: 0,
        }
    }

    /// Returns a reference to the renderer.
    pub fn renderer(&self) -> &R {
        &self.renderer
    }

    /// Returns a mutable reference to the renderer.
    pub fn renderer_mut(&mut self) -> &mut R {
        &mut self.renderer
    }

    /// Append a character to the output buffer.
    ///
    /// This is primarily for backward-compatibility helpers that need to
    /// inject extra characters (e.g., a trailing newline) into the output.
    pub(crate) fn push_output(&mut self, ch: char) {
        self.output.push(ch);
    }

    /// Parse a line and render it using the configured renderer.
    ///
    /// *Note:* This function requires `line` to include a newline at the end and
    /// also use of the `load_defaults_newlines` version of the syntaxes.
    pub fn parse_line(&mut self, line: &str) -> Result<(), Error> {
        let parsed_line = self.parse_state.parse_line(line, self.syntax_set)?.ops;
        let (formatted_line, delta) = render_line(
            line,
            parsed_line.as_slice(),
            &mut self.scope_stack,
            &mut self.renderer,
            self.line_index,
        )?;
        self.open_scopes += delta;
        self.output.push_str(formatted_line.as_str());
        self.line_index += 1;

        Ok(())
    }

    /// Close any remaining open scopes and return the finished output.
    pub fn finalize(mut self) -> String {
        let mut closing = Vec::new();
        for _ in 0..self.open_scopes {
            self.renderer.end_scope(&mut closing);
        }
        // All renderer output is valid UTF-8 (see render_line).
        self.output
            .push_str(&String::from_utf8(closing).expect("renderer output is valid UTF-8"));
        self.output
    }
}
