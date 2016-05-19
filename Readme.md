# syntect

`syntect` is a work-in-progress syntax highlighting library for Rust that uses [Sublime Text syntax definitions](http://www.sublimetext.com/docs/3/syntax.html#include-syntax). It is far from complete but eventually the goal is for it to be used in code analysis tools and text editors.

Currently it at least fully parses and compiles regexs for the complete Sublime Text syntax format, and it seems to work for every default Sublime Text syntax.

## Roadmap

- [x] Sketch out representation of a Sublime Text syntax
- [x] Parse `.sublime-syntax` files into the representation.
- [ ] Write an interpreter for the `.sublime-syntax` state machine that highlights an incoming iterator of file lines into an iterator of scope-annotated text.
- [ ] Parse TextMate/Sublime Text theme files
- [ ] Highlight a scope-annotated iterator into a colour-annotated iterator for display.
- [ ] Add C bindings so it can be used as a C library from other languages.
- [ ] Make it really fast
