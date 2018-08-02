//! Methods for dumping serializable structs to a compressed binary format
//! These are used to load and store the dumps used for fast startup times.
//!
//! Currently syntect serializes `SyntaxSet` structs with `dump_to_file`
//! into `.packdump` files and likewise `ThemeSet` structs to `.themedump` files.
//!
//! You can use these methods to manage your own caching of compiled syntaxes and
//! themes. And even your own `serde::Serialize` structures if you want to
//! be consistent with your format.
use bincode::Result;
#[cfg(any(feature = "dump-load", feature = "dump-load-rs"))]
use bincode::deserialize_from;
#[cfg(any(feature = "dump-create", feature = "dump-create-rs"))]
use bincode::serialize_into;
use std::fs::File;
#[cfg(any(feature = "dump-load", feature = "dump-load-rs"))]
use std::io::{BufRead, BufReader};
#[cfg(any(feature = "dump-create", feature = "dump-create-rs"))]
use std::io::{BufWriter, Write};
#[cfg(all(feature = "parsing", feature = "assets", any(feature = "dump-load", feature = "dump-load-rs")))]
use parsing::SyntaxSet;
#[cfg(all(feature = "assets", any(feature = "dump-load", feature = "dump-load-rs")))]
use highlighting::ThemeSet;
use std::path::Path;
#[cfg(feature = "dump-create")]
use flate2::write::ZlibEncoder;
#[cfg(any(feature = "dump-load", feature = "dump-load-rs"))]
use flate2::bufread::ZlibDecoder;
#[cfg(any(feature = "dump-create", feature = "dump-create-rs"))]
use flate2::Compression;
#[cfg(any(feature = "dump-create", feature = "dump-create-rs"))]
use serde::Serialize;
#[cfg(any(feature = "dump-load", feature = "dump-load-rs"))]
use serde::de::DeserializeOwned;

#[cfg(any(feature = "dump-create", feature = "dump-create-rs"))]
pub fn dump_to_writer<T: Serialize, W: Write>(to_dump: &T, output: W) -> Result<()> {
    let mut encoder = ZlibEncoder::new(output, Compression::best());
    serialize_into(&mut encoder, to_dump)
}

/// Dumps an object to a binary array in the same format as `dump_to_file`
#[cfg(any(feature = "dump-create", feature = "dump-create-rs"))]
pub fn dump_binary<T: Serialize>(o: &T) -> Vec<u8> {
    let mut v = Vec::new();
    dump_to_writer(o, &mut v).unwrap();
    v
}

/// Dumps an encodable object to a file at a given path. If a file already exists at that path
/// it will be overwritten. The files created are encoded with the `bincode` crate and then
/// compressed with the `flate2` crate.
#[cfg(any(feature = "dump-create", feature = "dump-create-rs"))]
pub fn dump_to_file<T: Serialize, P: AsRef<Path>>(o: &T, path: P) -> Result<()> {
    let out = BufWriter::new(File::create(path)?);
    dump_to_writer(o, out)
}

#[cfg(any(feature = "dump-load", feature = "dump-load-rs"))]
pub fn from_reader<T: DeserializeOwned, R: BufRead>(input: R) -> Result<T> {
    let mut decoder = ZlibDecoder::new(input);
    deserialize_from(&mut decoder)
}

/// Returns a fully loaded syntax set from
/// a binary dump. Panics if the dump is invalid.
#[cfg(any(feature = "dump-load", feature = "dump-load-rs"))]
pub fn from_binary<T: DeserializeOwned>(v: &[u8]) -> T {
    from_reader(v).unwrap()
}

/// Returns a fully loaded syntax set from a binary dump file.
#[cfg(any(feature = "dump-load", feature = "dump-load-rs"))]
pub fn from_dump_file<T: DeserializeOwned, P: AsRef<Path>>(path: P) -> Result<T> {
    let f = File::open(path)?;
    let reader = BufReader::new(f);
    from_reader(reader)
}

#[cfg(all(feature = "parsing", feature = "assets", any(feature = "dump-load", feature = "dump-load-rs")))]
impl SyntaxSet {
    /// Instantiates a new syntax set from a binary dump of
    /// Sublime Text's default open source syntax definitions.
    /// These dumps are included in this library's binary for convenience.
    ///
    /// This method loads the version for parsing line strings with no `\n` characters at the end.
    /// If you're able to efficiently include newlines at the end of strings, use `load_defaults_newlines`
    /// since it works better. See `SyntaxSetBuilder::load_from_folder` for more info on this issue.
    ///
    /// This is the recommended way of creating a syntax set for
    /// non-advanced use cases. It is also significantly faster than loading the YAML files.
    ///
    /// Note that you can load additional syntaxes after doing this. If you want
    /// you can even use the fact that SyntaxDefinitions are serializable with
    /// the bincode crate to cache dumps of additional syntaxes yourself.
    pub fn load_defaults_nonewlines() -> SyntaxSet {
        let ss: SyntaxSet = from_binary(include_bytes!("../assets/default_nonewlines.\
                                                             packdump"));
        ss
    }

    /// Same as `load_defaults_nonewlines` but for parsing line strings with newlines at the end.
    /// These are separate methods because thanks to linker garbage collection, only the serialized
    /// dumps for the method(s) you call will be included in the binary (each is ~200kb for now).
    pub fn load_defaults_newlines() -> SyntaxSet {
        let ss: SyntaxSet = from_binary(include_bytes!("../assets/default_newlines.packdump"));
        ss
    }
}

#[cfg(all(feature = "assets", any(feature = "dump-load", feature = "dump-load-rs")))]
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
    #[cfg(all(feature = "yaml-load", any(feature = "dump-create", feature = "dump-create-rs"), any(feature = "dump-load", feature = "dump-load-rs")))]
    #[test]
    fn can_dump_and_load() {
        use super::*;
        use parsing::SyntaxSetBuilder;
        let mut builder = SyntaxSetBuilder::new();
        builder.load_from_folder("testdata/Packages", false).unwrap();
        let ss = builder.build();

        let bin = dump_binary(&ss);
        println!("{:?}", bin.len());
        let ss2: SyntaxSet = from_binary(&bin[..]);
        assert_eq!(ss.syntaxes().len(), ss2.syntaxes().len());
    }

    #[cfg(all(feature = "yaml-load", any(feature = "dump-create", feature = "dump-create-rs"), any(feature = "dump-load", feature = "dump-load-rs")))]
    #[test]
    fn dump_is_deterministic() {
        use super::*;
        use parsing::SyntaxSetBuilder;

        let mut builder1 = SyntaxSetBuilder::new();
        builder1.load_from_folder("testdata/Packages", false).unwrap();
        let ss1 = builder1.build();
        let bin1 = dump_binary(&ss1);

        let mut builder2 = SyntaxSetBuilder::new();
        builder2.load_from_folder("testdata/Packages", false).unwrap();
        let ss2 = builder2.build();
        let bin2 = dump_binary(&ss2);
        // This is redundant, but assert_eq! can be really slow on a large
        // vector, so check the length first to fail faster.
        assert_eq!(bin1.len(), bin2.len());
        assert_eq!(bin1, bin2);
    }

    #[cfg(all(feature = "assets", any(feature = "dump-load", feature = "dump-load-rs")))]
    #[test]
    fn has_default_themes() {
        use highlighting::ThemeSet;
        let themes = ThemeSet::load_defaults();
        assert!(themes.themes.len() > 4);
    }
}
