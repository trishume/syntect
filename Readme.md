# syntect

`syntect` is a syntax highlighting library for Rust that uses [Sublime Text syntax definitions](http://www.sublimetext.com/docs/3/syntax.html#include-syntax). It aims to be a good solution for any Rust project that needs syntax highlighting, including deep integration with text editors written in Rust.

If you are writing a text editor (or something else needing highlighting) in Rust and this library doesn't fit your needs, I consider that a bug and you should file an issue or email me.

It is currently mostly complete and can parse, interpret and highlight based on Sublime Text syntax and `tmTheme` files.

Note: the build is currently failing on Travis Linux stable, but succeeding on nightly. The tests work fine on stable for me on OSX so it might just be a Linux issue, or a Travis issue. I'll see if I can figure it out.

### Rendered docs: <http://thume.ca/rustdoc/syntect/syntect/>

## Features/Goals

- [x] Work with many languages (accomplished through using existing grammar formats)
- [ ] Highlight super quickly, as fast as Sublime Text (not there yet but matching most editors)
- [x] Load up quickly, currently in around 23ms but could potentially be even faster.
- [x] Include easy to use API for basic cases
- [x] API allows use in fancy text editors with piece tables and incremental re-highlighting and the like.
- [x] Expose internals of the parsing process so text editors can do things like cache parse states and use semantic info for code intelligence
- [x] High quality highlighting, supporting things like heredocs and complex syntaxes (like Rust's).
- [x] Include a compressed dump of all the default syntax definitions in the library binary so users don't have to manage a folder of syntaxes.
- [x] Well documented, I've tried to add a useful documentation comment to everything that isn't utterly self explanatory.

## Screenshots

There's currently an example program called `syncat` that prints one of the source files using hard-coded themes and syntaxes using 24-bit terminal escape sequences supported by many newer terminals. These screenshots don't look as good as they could for two reasons: first the sRGB colours aren't corrected properly, and second the Rust syntax definition uses some fancy labels that these themes don't have highlighting for.

![Nested languages](http://i.imgur.com/bByxb1E.png)
![Base 16 Ocean Dark](http://i.imgur.com/CwiPOwZ.png)
![Solarized Light](http://i.imgur.com/l3zcO4J.png)

## Roadmap

- [x] Sketch out representation of a Sublime Text syntax
- [x] Parse `.sublime-syntax` files into the representation.
- [x] Write an interpreter for the `.sublime-syntax` state machine that highlights an incoming iterator of file lines into an iterator of scope-annotated text.
- [x] Parse TextMate/Sublime Text theme files
- [x] Highlight a scope-annotated iterator into a colour-annotated iterator for display.
- [x] Ability to dump loaded packages as binary file and load them with lazy regex compilation for fast start up times.
- [x] Bundle dumped default syntaxes into the library binary so library users don't need an assets folder with Sublime Text packages.
- [x] Add nice API wrappers for simple use cases. The base APIs are designed for deep high performance integration with arbitrary text editor data structures.
- [x] Document the API better and make things private that don't need to be public
- [ ] Make syncat a better demo, and maybe more demo programs
- [ ] Make it really fast (mosty two hot-paths need caching, same places Textmate 2 caches)
- [ ] Add sRGB colour correction (not sure if this is necessary, could be the job of the text editor)
- [ ] Add C bindings so it can be used as a C library from other languages.

## Performance

Currently `syntect` is reasonably fast but not as fast as it could be. The following perf features are done and to-be-done:

- [x] Pre-link references between languages (e.g `<script>` tags) so there are no tree traversal string lookups in the hot-path
- [x] Compact binary representation of scopes to allow quickly passing and copying them around
- [x] Determine if a scope is a prefix of another scope using bit manipulation in only a few instructions
- [ ] Cache regex matches to reduce number of times oniguruma is asked to search a line
- [ ] Cache scope lookups to reduce how much scope matching has to be done to highlight a list of scope operations
- [x] Lazily compile regexes so startup time isn't taken compiling a thousand regexs for Actionscript that nobody will use
- [ ] Use a better regex engine, perhaps the in progress fancy-regex crate

The current perf numbers are below. These numbers should get better once I implement more of the things above, but they're on par with many other text editors.

- Highlighting 9200 lines of jQuery 2.1 takes 1.76s. For comparison:
    - Textmate 2, Spacemacs and Visual Studio Code all take around the same time (2ish seconds)
    - Atom takes 6s
    - Sublime Text 3 dev build takes ~0.22s, despite having a super fancy javascript syntax definition
    - Vim is instantaneous but that isn't a fair comparison since vim's highlighting is far more basic than the other editors.
    - These comparisons aren't totally fair, except the one to Sublime Text since that is using the same theme and the same complex defintion for ES6 syntax.
- ~220ms to load and link all the syntax definitions in the default Sublime package set. This is ~60% regex compilation and ~35% YAML parsing.
    - but only ~23ms to load and link all the syntax definitions from an internal pre-made binary dump with lazy regex compilation.
- ~1.9ms to parse and highlight the 30 line 791 character `testdata/highlight_test.erb` file. This works out to around 16,000 lines/second or 422 kilobytes/second.
- ~250ms end to end for `syncat` to start, load the definitions, highlight the test file and shut down. This is mostly spent loading.

## License and Acknowledgements

Thanks to [Textmate 2](https://github.com/textmate/textmate) and @defuz's [sublimate](https://github.com/defuz/sublimate) for the existing open source code I used as inspiration and in the case of sublimate's `tmTheme` loader, copy-pasted. All code (including defuz's sublimate code) is released under the MIT license.
