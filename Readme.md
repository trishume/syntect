# syntect
[![Build Status](https://travis-ci.org/trishume/syntect.svg?branch=master)](https://travis-ci.org/trishume/syntect) [![Crates.io](https://img.shields.io/crates/v/syntect.svg?maxAge=2592000)]() [![Crates.io](https://img.shields.io/crates/l/syntect.svg?maxAge=2592000)]()

`syntect` is a syntax highlighting library for Rust that uses [Sublime Text syntax definitions](http://www.sublimetext.com/docs/3/syntax.html#include-syntax). It aims to be a good solution for any Rust project that needs syntax highlighting, including deep integration with text editors written in Rust.

If you are writing a text editor (or something else needing highlighting) in Rust and this library doesn't fit your needs, I consider that a bug and you should file an issue or email me.

**Note:** I consider this project "done" in the sense that it works quite well for its intended purpose, accomplishes the major goals I had, and I'm unlikely to make any sweeping changes.
I won't be committing much anymore because the marginal return on additional work isn't very high. Rest assured if you submit PRs I will review them and likely merge promptly.
I'll also quite possibly still fix issues and definitely offer advice and knowledge on how the library works. Basically I'll be maintaining the library but not developing it further.
I've spent months working on, tweaking, optimizing, documenting and testing this library. If you still have any reasons you don't think it fits your needs, file an issue or email me.

### Rendered docs: <http://thume.ca/rustdoc/syntect/syntect/>

## Getting Started

`syntect` is [available on crates.io](https://crates.io/crates/syntect). You can install it by adding this line to your `Cargo.toml`:

```toml
syntect = "1.3"
```

After that take a look at the [documentation](http://thume.ca/rustdoc/syntect/syntect/) and the [examples](https://github.com/trishume/syntect/tree/master/examples).

**Note:** with stable Rust on Linux there is a possibility you might have to add `./target/debug/build/onig_sys-*/out/lib/` to your `LD_LIBRARY_PATH` environment variable. I dunno why or even if this happens on other places than Travis, but see `travis.yml` for what it does to make it work. Do this if you see `libonig.so: cannot open shared object file`.

If you've cloned this repository, be sure to run

```
git submodule update --init
```

to fetch all the required dependencies for running the tests.

### feature flags

Syntect makes heavy use of [cargo features](http://doc.crates.io/manifest.html#the-features-section), to support users who require only a subset of functionality. In particular, it is possible to use the highlighting component of syntect without the parser (for instance when hand-rolling a higher performance parser for a particular language), by adding `default-features = false` to the syntect entry in your `Cargo.toml`.

For more information on available features, see the features section in `Cargo.toml`.

## Features/Goals

- [x] Work with many languages (accomplished through using existing grammar formats)
- [x] Highlight super quickly, faster than every editor except Sublime Text 3
- [x] Load up quickly, currently in around 23ms but could potentially be even faster.
- [x] Include easy to use API for basic cases
- [x] API allows use in fancy text editors with piece tables and incremental re-highlighting and the like.
- [x] Expose internals of the parsing process so text editors can do things like cache parse states and use semantic info for code intelligence
- [x] High quality highlighting, supporting things like heredocs and complex syntaxes (like Rust's).
- [x] Include a compressed dump of all the default syntax definitions in the library binary so users don't have to manage a folder of syntaxes.
- [x] Well documented, I've tried to add a useful documentation comment to everything that isn't utterly self explanatory.
- [x] Built-in output to coloured HTML `<pre>` tags or 24-bit colour ANSI terminal escape sequences.
- [x] Nearly complete compatibility with Sublime Text 3, including lots of edge cases. Passes nearly all of Sublime's syntax tests, see [issue 59](https://github.com/trishume/syntect/issues/59).

## Screenshots

There's currently an example program called `syncat` that prints one of the source files using hard-coded themes and syntaxes using 24-bit terminal escape sequences supported by many newer terminals. These screenshots don't look as good as they could for two reasons: first the sRGB colours aren't corrected properly, and second the Rust syntax definition uses some fancy labels that these themes don't have highlighting for.

![Nested languages](http://i.imgur.com/bByxb1E.png)
![Base 16 Ocean Dark](http://i.imgur.com/CwiPOwZ.png)
![Solarized Light](http://i.imgur.com/l3zcO4J.png)
![InspiredGithub](http://i.imgur.com/a7U1r2j.png)

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
- [x] Detect file syntax based on first line
- [x] Make it really fast (mosty two hot-paths need caching, same places Textmate 2 caches)
- [ ] Make syncat a better demo, and maybe more demo programs
- [ ] Add sRGB colour correction (not sure if this is necessary, could be the job of the text editor)
- [ ] Add C bindings so it can be used as a C library from other languages.

## Performance

Currently `syntect` is one of the faster syntax highlighting engines, but not the fastest. The following perf features are done and to-be-done:

- [x] Pre-link references between languages (e.g `<script>` tags) so there are no tree traversal string lookups in the hot-path
- [x] Compact binary representation of scopes to allow quickly passing and copying them around
- [x] Determine if a scope is a prefix of another scope using bit manipulation in only a few instructions
- [x] Cache regex matches to reduce number of times oniguruma is asked to search a line
- [x] Accelerate scope lookups to reduce how much selector matching has to be done to highlight a list of scope operations
- [x] Lazily compile regexes so startup time isn't taken compiling a thousand regexs for Actionscript that nobody will use
- [ ] Use a better regex engine, perhaps the in progress fancy-regex crate
- [ ] Parallelize the highlighting. Is this even possible? Would it help? To be determined.

The current perf numbers are below. These numbers may get better if more of the things above are implemented, but they're better than many other text editors.
All measurements were taken on a mid 2012 15" retina Macbook Pro.

- Highlighting 9200 lines/247kb of jQuery 2.1 takes 600ms. For comparison:
    - Textmate 2, Spacemacs and Visual Studio Code all take around 2ish seconds (measured by hand with a stopwatch, hence approximate).
    - Atom takes 6 seconds
    - Sublime Text 3 dev build takes 98ms (highlighting only, takes ~200ms click to pixels), despite having a super fancy javascript syntax definition.
    - Vim is instantaneous but that isn't a fair comparison since vim's highlighting is far more basic than the other editors (Compare [vim's grammar](https://github.com/vim/vim/blob/master/runtime/syntax/javascript.vim) to [Sublime's](https://github.com/sublimehq/Packages/blob/master/JavaScript/JavaScript.sublime-syntax)).
    - These comparisons aren't totally fair, except the one to Sublime Text since that is using the same theme and the same complex defintion for ES6 syntax.
- Simple syntaxes are faster, JS is one of the most complex. It only takes 34ms to highlight a 1700 line 62kb XML file or 50,000 lines/sec.
- ~138ms to load and link all the syntax definitions in the default Sublime package set.
    - but only ~23ms to load and link all the syntax definitions from an internal pre-made binary dump with lazy regex compilation.
- ~1.9ms to parse and highlight the 30 line 791 character `testdata/highlight_test.erb` file. This works out to around 16,000 lines/second or 422 kilobytes/second.
- ~250ms end to end for `syncat` to start, load the definitions, highlight the test file and shut down. This is mostly spent loading.

### Caching

Because `syntect`'s API exposes internal cacheable data structures, there is a caching strategy that text editors can use that allows the text on screen to be re-rendered instantaneously regardless of the file size when a change is made after the initial highlight.

Basically, on the initial parse every 1000 lines or so copy the parse state into a side-buffer for that line. When a change is made to the text, because of the way Sublime Text grammars work (and languages in general), only the highlighting after that change can be affected. Thus when a change is made to the text, search backwards in the parse state cache for the last state before the edit, then kick off a background task to start re-highlighting from there. Once the background task highlights past the end of the current editor viewport, render the new changes and continue re-highlighting the rest of the file in the background.

This way from the time the edit happens to the time the new colouring gets rendered in the worst case only `999+length of viewport` lines must be re-highlighted. Given the speed of `syntect` even with a long file and the most complicated syntax and theme this should take less than 100ms. This is enough to re-highlight on every key-stroke of the world's fastest typist *in the worst possible case*. And you can reduce this asymptotically to the length of the viewport by caching parse states more often, at the cost of more memory.

Any time the file is changed the latest cached state is found, the cache is cleared after that point, and a background job is started. Any already running jobs are stopped because they would be working on old state. This way you can just have one thread dedicated to highlighting that is always doing the most up-to-date work, or sleeping.

## Examples Available

There's a number of examples of programs that use `syntect` in the `examples` folder and some code outside the repo:

- `syncat` prints a highlighted file to the terminal using 24-bit colour ANSI escape codes. It demonstrates a simple file highlighting workflow.
- `synhtml` prints an HTML file that will display the highlighted code. Demonstrates how syntect could be used by web servers and static site generators.
- `synstats` collects a bunch of statistics about the code in a folder. Includes basic things like line count but also fancier things like number of functions. Demonstrates how `syntect` can be used for code analysis as well as highlighting, as well as how to use the APIs to parse out the semantic tokenization.
- [`faiyels`](https://github.com/trishume/faiyels) is a little code minimap visualizer I wrote that uses `syntect` for highlighting.

Here's that stats that `synstats` extracts from `syntect`'s codebase (not including examples and test data) as of [this commit](https://github.com/trishume/syntect/commit/10baa6888f84ea4ae35c746526302a8ff4956eb1):
```
################## Stats ###################
File count:                               19
Total characters:                     155504

Function count:                          165
Type count (structs, enums, classes):     64

Code lines (traditional SLOC):          2960
Total lines (w/ comments & blanks):     4011
Comment lines (comment but no code):     736
Blank lines (lines-blank-comment):       315

Lines with a documentation comment:      646
Total words written in doc comments:    4734
Total words written in all comments:    5145
Characters of comment:                 41099
```

## License and Acknowledgements

Thanks to [Textmate 2](https://github.com/textmate/textmate) and @defuz's [sublimate](https://github.com/defuz/sublimate) for the existing open source code I used as inspiration and in the case of sublimate's `tmTheme` loader, copy-pasted. All code (including defuz's sublimate code) is released under the MIT license.
