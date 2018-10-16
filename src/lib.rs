//! Welcome to the syntect docs.
//!
//! Much more info about syntect is available on the [Github Page](https://github.com/trishume/syntect).
//!
//! May I suggest that you start by reading the `Readme.md` file in the main repo.
//! Once you're done with that you can look at the docs for `parsing::SyntaxSet`
//! and for the `easy` module.
//!
//! Almost everything in syntect is divided up into either the `parsing` module
//! for turning text into text annotated with scopes, and the `highlighting` module
//! for turning annotated text into styled/colored text.
//!
//! Some docs have example code but a good place to look is the `syncat` example as well as the source code
//! for the `easy` module in `easy.rs` as that shows how to plug the various parts together for common use cases.

#![doc(html_root_url = "https://docs.rs/syntect/3.0.1")]

#[cfg(feature = "yaml-load")]
extern crate yaml_rust;
#[cfg(feature = "parsing")]
extern crate onig;
extern crate walkdir;
#[cfg(feature = "parsing")]
extern crate regex_syntax;
#[macro_use]
extern crate lazy_static;
extern crate lazycell;
extern crate plist;
#[cfg(any(feature = "dump-load-rs", feature = "dump-load", feature = "dump-create"))]
extern crate bincode;
#[macro_use]
extern crate bitflags;
#[cfg(any(feature = "dump-load", feature = "dump-create", feature = "dump-load-rs", feature = "dump-create-rs"))]
extern crate flate2;
#[cfg(feature = "parsing")]
extern crate fnv;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
#[cfg(test)]
#[macro_use]
extern crate pretty_assertions;
#[cfg(test)]
extern crate rayon;

pub mod highlighting;
pub mod parsing;
pub mod util;
#[cfg(any(feature = "dump-load-rs", feature = "dump-load", feature = "dump-create", feature = "dump-create-rs"))]
pub mod dumps;
#[cfg(feature = "parsing")]
pub mod easy;
#[cfg(feature = "html")]
pub mod html;
#[cfg(feature = "html")]
mod escape;

use std::io::Error as IoError;
use std::error::Error;
use std::fmt;
#[cfg(all(feature = "yaml-load", feature = "parsing"))]
use parsing::ParseSyntaxError;
use highlighting::{ParseThemeError, SettingsError};

/// Common error type used by syntax and theme loading
#[derive(Debug)]
pub enum LoadingError {
    /// error finding all the files in a directory
    WalkDir(walkdir::Error),
    /// error reading a file
    Io(IoError),
    /// a syntax file was invalid in some way
    #[cfg(feature = "yaml-load")]
    ParseSyntax(ParseSyntaxError, Option<String>),
    /// a theme file was invalid in some way
    ParseTheme(ParseThemeError),
    /// a theme's Plist syntax was invalid in some way
    ReadSettings(SettingsError),
    /// A path given to a method was invalid.
    /// Possibly because it didn't reference a file or wasn't UTF-8.
    BadPath,
}

impl From<SettingsError> for LoadingError {
    fn from(error: SettingsError) -> LoadingError {
        LoadingError::ReadSettings(error)
    }
}

impl From<IoError> for LoadingError {
    fn from(error: IoError) -> LoadingError {
        LoadingError::Io(error)
    }
}

impl From<ParseThemeError> for LoadingError {
    fn from(error: ParseThemeError) -> LoadingError {
        LoadingError::ParseTheme(error)
    }
}

#[cfg(all(feature = "yaml-load", feature = "parsing"))]
impl From<ParseSyntaxError> for LoadingError {
    fn from(error: ParseSyntaxError) -> LoadingError {
        LoadingError::ParseSyntax(error, None)
    }
}

impl fmt::Display for LoadingError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use LoadingError::*;

        match *self {
            WalkDir(ref error) => error.fmt(f),
            Io(ref error) => error.fmt(f),
            #[cfg(feature = "yaml-load")]
            ParseSyntax(ref error, ref filename) => {
                if let Some(ref file) = filename {
                    write!(f, "{}: {}", file, error.description())
                } else {
                    error.fmt(f)
                }
            },
            _ => write!(f, "{}", self.description()),
        }
    }
}

impl Error for LoadingError {
    fn description(&self) -> &str {
        use LoadingError::*;

        match *self {
            WalkDir(ref error) => error.description(),
            Io(ref error) => error.description(),
            #[cfg(feature = "yaml-load")]
            ParseSyntax(ref error, ..) => error.description(),
            ParseTheme(_) => "Invalid syntax theme",
            ReadSettings(_) => "Invalid syntax theme settings",
            BadPath => "Invalid path",
        }
    }

    fn cause(&self) -> Option<&Error> {
        use LoadingError::*;

        match *self {
            WalkDir(ref error) => Some(error),
            Io(ref error) => Some(error),
            _ => None,
        }
    }
}
