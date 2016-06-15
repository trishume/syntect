extern crate yaml_rust;
extern crate onig;
extern crate walkdir;
extern crate regex_syntax;
#[macro_use]
extern crate lazy_static;
extern crate plist;
extern crate bincode;
extern crate rustc_serialize;
#[macro_use]
extern crate bitflags;
extern crate flate2;
pub mod highlighting;
pub mod parsing;
pub mod util;
pub mod dumps;
pub mod easy;

use std::io::Error as IoError;
use parsing::ParseSyntaxError;
use highlighting::{ParseThemeError, SettingsError};

#[derive(Debug)]
pub enum LoadingError {
    WalkDir(walkdir::Error),
    Io(IoError),
    ParseSyntax(ParseSyntaxError),
    ParseTheme(ParseThemeError),
    ReadSettings(SettingsError),
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

impl From<ParseSyntaxError> for LoadingError {
    fn from(error: ParseSyntaxError) -> LoadingError {
        LoadingError::ParseSyntax(error)
    }
}
