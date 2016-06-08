# syntect

`syntect` is a work-in-progress syntax highlighting library for Rust that uses [Sublime Text syntax definitions](http://www.sublimetext.com/docs/3/syntax.html#include-syntax). It is not quite complete but eventually the goal is for it to be used in code analysis tools and text editors.

If you are writing a text editor (or something else needing highlighting) in Rust and this library doesn't fit your needs, I consider that a bug and you should file an issue or email me.

It is currently mostly complete and can parse, interpret and highlight based on Sublime Text syntax and `tmTheme` files.

## Goals

- Work with many languages (accomplished through using existing grammar formats)
- Be super fast
- API that is both easy to use, and allows use in fancy text editors with piece tables and incremental re-highlighting and the like
- High quality highlighting, supporting things like heredocs and complex syntaxes (like Rust's).

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
- [ ] Add nice API wrappers for simple use cases. The base APIs are designed for deep high performance integration with arbitrary text editor data structures.
- [ ] Make syncat a better demo, and maybe more demo programs
- [ ] Document the API better and make things private that don't need to be public
- [ ] Add sRGB colour correction (not sure if this is necessary, could be the job of the text editor)
- [ ] Make it really fast (mosty two hot-paths need caching, same places Textmate 2 caches)
- [ ] Add C bindings so it can be used as a C library from other languages.

## License and Acknowledgements

Thanks to [Textmate 2](https://github.com/textmate/textmate) and @defuz's [sublimate](https://github.com/defuz/sublimate) for the existing open source code I used as inspiration and in the case of sublimate's `tmTheme` loader, copy-pasted. All code (including defuz's sublimate code) is released under the MIT license.
