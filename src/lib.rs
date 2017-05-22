//! Welcome to the syntect docs.
//! These are still a work in progress but a lot of the important things have
//! been documented already.
//!
//! Much more info about syntect is available on the [Github Page](https://github.com/trishume/syntect).
//!
//! May I suggest that you start by reading the `Readme.md` file in the main repo.
//! Once you're done with that you can look at the docs for `parsing::SyntaxSet`
//! and for the `easy` module.
//!
//! Almost everything in syntect is divided up into either the `parsing` module
//! for turning text into text annotated with scopes, and the `highlighting` module
//! for turning annotated text into styled/coloured text.
//!
//! Some docs have example code but a good place to look is the `syncat` example as well as the source code
//! for the `easy` module in `easy.rs` as that shows how to plug the various parts together for common use cases.
extern crate syntect_highlighting as highlighting;
#[cfg(feature = "yaml-load")]
extern crate yaml_rust;
extern crate onig;
extern crate walkdir;
extern crate regex_syntax;
extern crate bincode;
extern crate flate2;
extern crate fnv;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
pub mod parsing;
pub mod util;
pub mod dumps;
pub mod easy;
#[cfg(feature = "html")]
pub mod html;
#[cfg(feature = "html")]
mod escape;
