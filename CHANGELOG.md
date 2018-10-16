# Version 3.0.1

- Fix a bug with syntaxes that used captures in lookarounds (#176 #215)
- Fix the precedence order of syntaxes to match Sublime (#217 #216)

# Version 3.0

This is a major release with multiple breaking API changes, although upgrading shouldn't be too difficult. It fixes bugs and comes with some nice new features.

## Breaking changes and upgrading

- The `SyntaxSet` API has been revamped to use a builder and an arena of contexts. See [example usage](https://github.com/trishume/syntect/blob/51208d35a6d98c07468fbe044d5c6f37eb129205/examples/gendata.rs#L25-L28).
- Many functions now need to be passed the `SyntaxSet` that goes with the rest of their arguments because of this new arena.
- Filename added to `LoadingError::ParseSyntax`
- Many functions in the `html` module now take the `newlines` version of syntaxes.
  - These methods have also been renamed, partially so that code that needs updating doesn't break without a compile error.
  - The HTML they output also treats newlines slightly differently and I think more correctly but uglier when you look at the HTML.

#### Breaking rename upgrade guide

- `SyntaxSet::add_syntax -> SyntaxSetBuilder::add`
- `SyntaxSet::load_syntaxes -> SyntaxSetBuilder::add_from_folder`
- `SyntaxSet::load_plain_text_syntax -> SyntaxSetBuilder::add_plain_text_syntax`
- `html::highlighted_snippet_for_string -> html::highlighted_html_for_string`: also change to `newlines` `SyntaxSet`
- `html::highlighted_snippet_for_file -> html::highlighted_html_for_file`: also change to `newlines` `SyntaxSet`
- `html::styles_to_coloured_html -> html::styled_line_to_highlighted_html`: also change to `newlines` `SyntaxSet`
- `html::start_coloured_html_snippet -> html::start_highlighted_html_snippet`: return type also changed

## Major changes and new features

- Use arena for contexts (#182 #186 #187 #190 #195): This makes the code cleaner, enables use of syntaxes from multiple threads, and prevents accidental misuse.
  - This involves a new `SyntaxSetBuilder` API for constructing new `SyntaxSet`s
  - See the revamped [parsyncat example](https://github.com/trishume/syntect/blob/51208d35a6d98c07468fbe044d5c6f37eb129205/examples/parsyncat.rs).
- Encourage use of newlines (#197 #207 #196): The `nonewlines` mode is often buggy so we made it easier to use the `newlines` mode.
  - Added a `LinesWithEndings` utility for iterating over the lines of a string with `\n` characters.
  - Reengineer the `html` module to use `newlines` syntaxes.
- Add helpers for modifying highlighted lines (#198): For use cases like highlighting a piece of text in a blog code snippet or debugger. This allows you to reach into the highlighted spans and add styles.
  - Check out `split_at` and `modify_range` in the `util` module.
- New `ThemeSet::add_from_folder` function (#200): For modifying existing theme sets.

## Bug Fixes

- Improve nonewlines regex rewriting: #212 #211
- Reengineer theme application to match Sublime: #209
- Also mark contexts referenced by name as "no prototype" (same as ST): #180
- keep with_prototype when switching contexts with `set`: #177 #166
- Fix unused import warning: #174
- Ignore trailing dots in selectors: #173
- Fix `embed` to not include prototypes: #172 #160

## Upgraded dependencies

- plist: `0.2 -> 0.3`
- regex: `0.2 -> 1.0`
- onig: `3.2.1 -> 4.1`

# Prior versions

See the Github release notes: <https://github.com/trishume/syntect/releases>
