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
use std::io;

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
/// let output: Vec<u8> = generator.finalize();
/// let html = String::from_utf8(output).unwrap();
/// ```
pub struct DocumentGenerator<'a, R: ScopeRenderer, W: io::Write = Vec<u8>> {
    syntax_set: &'a SyntaxSet,
    open_scopes: isize,
    parse_state: ParseState,
    scope_stack: ScopeStack,
    output: W,
    renderer: R,
    line_index: usize,
}

impl<'a, R: ScopeRenderer> DocumentGenerator<'a, R> {
    /// Create a new document generator with a custom renderer.
    ///
    /// The output is collected into a `Vec<u8>` that is returned by
    /// [`finalize`]. Use [`new_with_output`] to stream output to an
    /// arbitrary [`io::Write`] sink instead.
    ///
    /// [`finalize`]: DocumentGenerator::finalize
    /// [`new_with_output`]: DocumentGenerator::new_with_output
    pub fn new(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        renderer: R,
    ) -> DocumentGenerator<'a, R> {
        Self::new_with_output(syntax_reference, syntax_set, renderer, Vec::new())
    }
}

impl<'a, R: ScopeRenderer, W: io::Write> DocumentGenerator<'a, R, W> {
    /// Create a new document generator that writes to the given output sink.
    ///
    /// This allows streaming rendered output directly to a file, socket,
    /// or buffered writer without intermediate allocation.
    pub fn new_with_output(
        syntax_reference: &'a SyntaxReference,
        syntax_set: &'a SyntaxSet,
        renderer: R,
        output: W,
    ) -> DocumentGenerator<'a, R, W> {
        DocumentGenerator {
            syntax_set,
            open_scopes: 0,
            parse_state: ParseState::new(syntax_reference),
            scope_stack: ScopeStack::new(),
            output,
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

    /// Append a character to the output.
    ///
    /// This is primarily for backward-compatibility helpers that need to
    /// inject extra characters (e.g., a trailing newline) into the output.
    pub(crate) fn push_output(&mut self, ch: char) {
        let mut buf = [0u8; 4];
        let bytes = ch.encode_utf8(&mut buf);
        let _ = self.output.write_all(bytes.as_bytes());
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
        self.output.write_all(formatted_line.as_bytes())?;
        self.line_index += 1;

        Ok(())
    }

    /// Close any remaining open scopes and return the finished output sink.
    pub fn finalize(mut self) -> W {
        let mut buf = String::new();
        for _ in 0..self.open_scopes {
            self.renderer.end_scope(&mut buf);
        }
        let _ = self.output.write_all(buf.as_bytes());
        self.output
    }
}
