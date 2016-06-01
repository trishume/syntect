/// Code based on https://github.com/defuz/sublimate/blob/master/src/core/settings.rs
/// released under the MIT license by @defuz

use std::io::{Read, Seek};
use plist::{Plist, Error as PlistError};

pub use rustc_serialize::json::Json as Settings;
pub use rustc_serialize::json::Array as SettingsArray;
pub use rustc_serialize::json::Object as SettingsObject;

pub trait FromSettings : Sized {
    fn from_settings(settings: Settings) -> Self;
}

pub trait ParseSettings : Sized {
    type Error;
    fn parse_settings(settings: Settings) -> Result<Self, Self::Error>;
}

#[derive(Debug)]
pub enum SettingsError {
    Plist(PlistError)
}

impl From<PlistError> for SettingsError {
    fn from(error: PlistError) -> SettingsError {
        SettingsError::Plist(error)
    }
}

pub fn read_plist<R: Read+Seek>(reader: R) -> Result<Settings, SettingsError> {
    Ok(try!(Plist::read(reader)).into_rustc_serialize_json())
}
