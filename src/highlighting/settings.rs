/// Code based on https://github.com/defuz/sublimate/blob/master/src/core/settings.rs
/// released under the MIT license by @defuz

use std::io::{Read, Seek};
use plist::{Plist, Error as PlistError};
use serde_json::Number;

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
    let plist = Plist::read(reader)?;
    Ok(to_json(plist))
}

fn to_json(plist: Plist) -> Settings {
    match plist {
        Plist::Array(elements) =>
            SettingsArray(elements.into_iter().map(to_json).collect()),
        Plist::Dictionary(entries) =>
            SettingsObject(entries.into_iter().map(|(k, v)| (k, to_json(v))).collect()),
        Plist::Boolean(value) => Settings::Bool(value),
        Plist::Data(bytes) => Settings::Array(bytes.into_iter().map(|b| b.into()).collect()),
        Plist::Date(value) => Settings::String(value.to_string()),
        Plist::Real(value) =>
            Settings::Number(Number::from_f64(value).expect("Error converting plist real value to JSON number")),
        Plist::Integer(value) => Settings::Number(value.into()),
        Plist::String(s) => Settings::String(s),
    }
}
