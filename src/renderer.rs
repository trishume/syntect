//! Generic trait and rendering loop for class-based syntax highlighting.
//!
//! This module provides the [`ScopeRenderer`] trait and the
//! [`render_line_to_classed_spans`] function that drives it. They are
//! format-agnostic: the trait's default `write_text` passes text through
//! unchanged, and no HTML or other markup concepts leak into this module.
//!
//! For an HTML-specific implementation, see [`crate::html::HTMLScopeRenderer`].

use std::io;

use crate::parsing::{
    lock_global_scope_repo, BasicScopeStackOp, Scope, ScopeRepository, ScopeStack, ScopeStackOp,
};
use crate::Error;

/// Trait for customizing the output of class-based syntax highlighting.
///
/// The type parameter `W` is the output target. It defaults to [`Vec<u8>`],
/// which is what the built-in rendering loop ([`render_line_to_classed_spans`])
/// uses. Implementors targeting other writable types (e.g., streaming to a
/// file or a socket) can implement `ScopeRenderer<MyWriter>` and drive
/// their own rendering loop.
///
/// The methods receive pre-resolved scope atom strings so implementations
/// never need to interact with the scope repository directly.
///
/// See [`crate::html::HTMLScopeRenderer`] for an HTML implementation that produces
/// `<span class="...">` elements.
pub trait ScopeRenderer<W: io::Write = Vec<u8>> {
    /// Called at the start of each line, before any tokens.
    ///
    /// `line_index` is 0-based. `scope_stack` is the current scope stack carried
    /// over from the previous line.
    fn begin_line(&mut self, _line_index: usize, _scope_stack: &[Scope], _output: &mut W) {}

    /// Called at the end of each line, after all tokens.
    fn end_line(&mut self, _line_index: usize, _scope_stack: &[Scope], _output: &mut W) {}

    /// Called when a new scope is pushed onto the stack.
    ///
    /// - `atom_strs`: the individual atom strings of the scope
    ///   (e.g., `["keyword", "operator", "arithmetic", "r"]` for `keyword.operator.arithmetic.r`)
    /// - `scope`: the raw [`Scope`] value, for advanced matching
    /// - `scope_stack`: the full stack after the push
    /// - `output`: the buffer to write the opening tag to
    ///
    /// Return `true` if a tag was written (meaning [`end_scope`] will be called
    /// to close it), or `false` to skip this scope (no matching `end_scope` call).
    ///
    /// [`end_scope`]: ScopeRenderer::end_scope
    fn begin_scope(
        &mut self,
        atom_strs: &[&str],
        scope: Scope,
        scope_stack: &[Scope],
        output: &mut W,
    ) -> bool;

    /// Called when a scope is popped, only if the corresponding [`begin_scope`]
    /// returned `true`.
    ///
    /// [`begin_scope`]: ScopeRenderer::begin_scope
    fn end_scope(&mut self, output: &mut W);

    /// Called for text content between scope operations.
    ///
    /// The default implementation passes text through unchanged.
    fn write_text(&mut self, text: &str, output: &mut W) -> Result<(), io::Error> {
        output.write_all(text.as_bytes())
    }
}

/// Resolve atom strings for a scope from a locked repository.
pub(crate) fn resolve_atom_strs<'a>(scope: Scope, repo: &'a ScopeRepository) -> Vec<&'a str> {
    (0..scope.len() as usize)
        .map(|i| repo.atom_str(scope.atom_at(i)))
        .collect()
}

/// Core rendering loop that drives a [`ScopeRenderer`].
///
/// Locks the global scope repository once per line. Applies the transparent
/// empty-span optimization: if a scope push/pop pair contains no text, the
/// push output is truncated rather than emitting an empty element.
pub fn render_line_to_classed_spans<R: ScopeRenderer>(
    line: &str,
    ops: &[(usize, ScopeStackOp)],
    stack: &mut ScopeStack,
    renderer: &mut R,
    line_index: usize,
) -> Result<(String, isize), Error> {
    let mut s = Vec::with_capacity(line.len() + ops.len() * 8);
    let mut cur_index = 0;
    let mut span_delta = 0;

    // Empty-span optimization tracking
    let mut span_empty = false;
    let mut span_start = 0;

    // begin_line is called without the repo lock held, so renderers like
    // LineHighlightingRenderer can safely lock the repo themselves.
    renderer.begin_line(line_index, &stack.scopes, &mut s);

    {
        let repo = lock_global_scope_repo();
        for &(i, ref op) in ops {
            if i > cur_index {
                span_empty = false;
                renderer.write_text(&line[cur_index..i], &mut s)?;
                cur_index = i;
            }
            stack.apply_with_hook(op, |basic_op, stack_slice| match basic_op {
                BasicScopeStackOp::Push(scope) => {
                    let atom_strs = resolve_atom_strs(scope, &repo);
                    span_start = s.len();
                    span_empty = true;
                    let wrote = renderer.begin_scope(&atom_strs, scope, stack_slice, &mut s);
                    if wrote {
                        span_delta += 1;
                    } else {
                        span_empty = false;
                    }
                }
                BasicScopeStackOp::Pop => {
                    if span_empty {
                        s.truncate(span_start);
                    } else {
                        renderer.end_scope(&mut s);
                    }
                    span_delta -= 1;
                    span_empty = false;
                }
            })?;
        }
        renderer.write_text(&line[cur_index..line.len()], &mut s)?;
    }

    // end_line is called without the repo lock held.
    renderer.end_line(line_index, &stack.scopes, &mut s);

    // All writes go through write_all(str.as_bytes()) or write!() with Display
    // impls that produce valid UTF-8, so this conversion will never fail.
    let result = String::from_utf8(s).expect("renderer output is valid UTF-8");
    Ok((result, span_delta))
}
