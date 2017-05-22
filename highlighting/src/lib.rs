//! Everything having to do with turning parsed text into styled text.
//! You might want to check out `Theme` for its handy text-editor related
//! settings like selection colour, `ThemeSet` for loading themes,
//! as well as things starting with `Highlight` for how to highlight text.
extern crate walkdir;
extern crate plist;
#[macro_use]
extern crate bitflags;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate serde_derive;
extern crate serde;
extern crate serde_json;

mod selector;
mod settings;
mod style;
mod theme;
mod highlighter;
mod theme_set;
pub mod scope;

pub use self::selector::*;
pub use self::settings::SettingsError;
pub use self::style::*;
pub use self::theme::*;
pub use self::highlighter::*;
pub use self::theme_set::*;
pub use self::scope::*;

use std::io::Error as IoError;

/// Common error type used by syntax and theme loading
#[derive(Debug)]
pub enum LoadingError {
    /// error finding all the files in a directory
    WalkDir(walkdir::Error),
    /// error reading a file
    Io(IoError),
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
