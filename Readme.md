# syntect

`syntect` is a work-in-progress syntax highlighting library for Rust that uses [Sublime Text syntax definitions](http://www.sublimetext.com/docs/3/syntax.html#include-syntax). It is far from complete but eventually the goal is for it to be used in code analysis tools and text editors.

If you are writing a text editor (or something else needing highlighting) in Rust and this library doesn't fit your needs, I consider that a bug and you should file an issue or email me.

It is currently mostly complete and can parse, interpret and highlight based on Sublime Text syntax and `tmTheme` files.

## Screenshots

There's currently an example program called `syncat` that prints one of the source files using hard-coded themes and syntaxes using 24-bit terminal escape sequences supported by many newer terminals. These screenshots don't look as good as they could for two reasons: first the sRGB colours aren't corrected properly, and second the Rust syntax definition uses some fancy labels that these themes don't have highlighting for.

![Base 16 Ocean Dark](http://i.imgur.com/CwiPOwZ.png)
![Solarized Light](http://i.imgur.com/l3zcO4J.png)

## Goals

- Work with many languages (accomplished through using existing grammar formats)
- Be super fast
- API that is both easy to use, and allows use in fancy text editors with piece tables and incremental re-highlighting and the like
- High quality highlighting, supporting things like heredocs and complex syntaxes (like Rust's).

## Roadmap

- [x] Sketch out representation of a Sublime Text syntax
- [x] Parse `.sublime-syntax` files into the representation.
- [x] Write an interpreter for the `.sublime-syntax` state machine that highlights an incoming iterator of file lines into an iterator of scope-annotated text.
- [x] Parse TextMate/Sublime Text theme files
- [x] Highlight a scope-annotated iterator into a colour-annotated iterator for display.
- [ ] Make the API nicer to use
- [ ] Add good demo programs
- [ ] Document the API
- [ ] Add sRGB colour correction
- [ ] Make it really fast
- [ ] Add C bindings so it can be used as a C library from other languages.
