//! Methods for dumping serializable structs to a compressed binary format
//! These are used to load and store the dumps used for fast startup times.
//!
//! Currently syntect serializes `SyntaxSet` structs with `dump_to_file`
//! into `.packdump` files and likewise `ThemeSet` structs to `.themedump` files.
//!
//! You can use these methods to manage your own caching of compiled syntaxes and
//! themes. And even your own `serde::Serialize` structures if you want to
//! be consistent with your format.
use bincode::{ErrorKind, Infinite, Result, deserialize_from, serialize_into};
use std::fs::File;
use std::io::{BufReader, BufWriter};
#[cfg(all(feature = "parsing", feature = "assets"))]
use parsing::SyntaxSet;
#[cfg(feature = "assets")]
use highlighting::ThemeSet;
use std::path::Path;
use flate2::write::ZlibEncoder;
use flate2::read::ZlibDecoder;
use flate2::Compression;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Dumps an object to a binary array in the same format as `dump_to_file`
pub fn dump_binary<T: Serialize>(o: &T) -> Vec<u8> {
    let mut v = Vec::new();
    {
        let mut encoder = ZlibEncoder::new(&mut v, Compression::Best);
        serialize_into(&mut encoder, o, Infinite).unwrap();
    }
    v
}

/// Dumps an encodable object to a file at a given path. If a file already exists at that path
/// it will be overwritten. The files created are encoded with the `bincode` crate and then
/// compressed with the `flate2` crate.
pub fn dump_to_file<T: Serialize, P: AsRef<Path>>(o: &T, path: P) -> Result<()> {
    let f = BufWriter::new(try!(File::create(path).map_err(ErrorKind::IoError)));
    let mut encoder = ZlibEncoder::new(f, Compression::Best);
    serialize_into(&mut encoder, o, Infinite)
}

/// Returns a fully loaded syntax set from
/// a binary dump. Panics if the dump is invalid.
pub fn from_binary<T: DeserializeOwned>(v: &[u8]) -> T {
    let mut decoder = ZlibDecoder::new(v);
    deserialize_from(&mut decoder, Infinite).unwrap()
}

/// Returns a fully loaded syntax set from
/// a binary dump file.
pub fn from_dump_file<T: DeserializeOwned, P: AsRef<Path>>(path: P) -> Result<T> {
    let f = try!(File::open(path).map_err(ErrorKind::IoError));
    let mut decoder = ZlibDecoder::new(BufReader::new(f));
    deserialize_from(&mut decoder, Infinite)
}

#[cfg(all(feature = "parsing", feature = "assets"))]
impl SyntaxSet {
    /// Instantiates a new syntax set from a binary dump of
    /// Sublime Text's default open source syntax definitions and then links it.
    /// These dumps are included in this library's binary for convenience.
    ///
    /// This method loads the version for parsing line strings with no `\n` characters at the end.
    /// If you're able to efficiently include newlines at the end of strings, use `load_defaults_newlines`
    /// since it works better. See `SyntaxSet#load_syntaxes` for more info on this issue.
    ///
    /// This is the recommended way of creating a syntax set for
    /// non-advanced use cases. It is also significantly faster than loading the YAML files.
    ///
    /// Note that you can load additional syntaxes after doing this,
    /// you'll just have to link again. If you want you can even
    /// use the fact that SyntaxDefinitions are serializable with
    /// the bincode crate to cache dumps of additional syntaxes yourself.
    pub fn load_defaults_nonewlines() -> SyntaxSet {
        let mut ps: SyntaxSet = from_binary(include_bytes!("../assets/default_nonewlines.\
                                                             packdump"));
        ps.link_syntaxes();
        ps
    }

    /// Same as `load_defaults_nonewlines` but for parsing line strings with newlines at the end.
    /// These are separate methods because thanks to linker garbage collection, only the serialized
    /// dumps for the method(s) you call will be included in the binary (each is ~200kb for now).
    pub fn load_defaults_newlines() -> SyntaxSet {
        let mut ps: SyntaxSet = from_binary(include_bytes!("../assets/default_newlines.packdump"));
        ps.link_syntaxes();
        ps
    }
}

#[cfg(feature = "assets")]
impl ThemeSet {
    /// Loads the set of default themes
    /// Currently includes (these are the keys for the map):
    ///
    /// - `base16-ocean.dark`,`base16-eighties.dark`,`base16-mocha.dark`,`base16-ocean.light`
    /// - `InspiredGitHub` from [here](https://github.com/sethlopezme/InspiredGitHub.tmtheme)
    /// - `Solarized (dark)` and `Solarized (light)`
    pub fn load_defaults() -> ThemeSet {
        from_binary(include_bytes!("../assets/default.themedump"))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "yaml-load")]
    #[test]
    fn can_dump_and_load() {
        use super::*;
        use parsing::SyntaxSet;
        let mut ps = SyntaxSet::new();
        ps.load_syntaxes("testdata/Packages", false).unwrap();

        let bin = dump_binary(&ps);
        let ps2: SyntaxSet = from_binary(&bin[..]);
        assert_eq!(ps.syntaxes().len(), ps2.syntaxes().len());

    }
    
    #[cfg(feature = "assets")]
    #[test]
    fn has_default_themes() {
        use highlighting::ThemeSet;
        let themes = ThemeSet::load_defaults();
        assert!(themes.themes.len() > 4);
    }
}
