/// Code based on https://github.com/defuz/sublimate/blob/master/src/core/settings.rs
/// released under the MIT license by @defuz

use std::io::{Read, Seek};
use plist::{Error as PlistError};

pub use serde_json::Value as Settings;
pub use serde_json::Value::Array as SettingsArray;
pub use serde_json::Value::Object as SettingsObject;

pub trait FromSettings: Sized {
    fn from_settings(settings: Settings) -> Self;
}

pub trait ParseSettings: Sized {
    type Error;
    fn parse_settings(settings: Settings) -> Result<Self, Self::Error>;
}


/// An error parsing a settings file
#[derive(Debug)]
pub enum SettingsError {
    /// Incorrect Plist syntax
    Plist(PlistError),
}

impl From<PlistError> for SettingsError {
    fn from(error: PlistError) -> SettingsError {
        SettingsError::Plist(error)
    }
}

pub fn read_plist<R: Read + Seek>(reader: R) -> Result<Settings, SettingsError> {
    let settings = plist::from_reader(reader)?;
    Ok(settings)
}
