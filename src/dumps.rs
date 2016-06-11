use bincode::SizeLimit;
use bincode::rustc_serialize::*;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use package_set::PackageSet;
use std::path::Path;
use flate2::write::ZlibEncoder;
use flate2::read::ZlibDecoder;
use flate2::Compression;

impl PackageSet {
    /// Instantiates a new package set from a binary dump of
    /// Sublime Text's default open source syntax definitions and then links it.
    /// These dumps are included in this library's binary for convenience.
    /// This method loads the version for parsing line strings with no `\n` characters at the end.
    ///
    /// This is the recommended way of creating a package set for
    /// non-advanced use cases. It is also significantly faster than loading the YAML files.
    ///
    /// Note that you can load additional syntaxes after doing this,
    /// you'll just have to link again. If you want you can even
    /// use the fact that SyntaxDefinitions are serializable with
    /// the bincode crate to cache dumps of additional syntaxes yourself.
    pub fn load_defaults_nonewlines() -> PackageSet {
        let mut ps = Self::from_binary(include_bytes!("../assets/default_nonewlines.packdump"));
        ps.link_syntaxes();
        ps
    }

    /// Same as `load_defaults_nonewlines` but for parsing line strings with newlines at the end.
    /// These are separate methods because thanks to linker garbage collection, only the serialized
    /// dumps for the method(s) you call will be included in the binary (each is ~200kb for now).
    pub fn load_defaults_newlines() -> PackageSet {
        let mut ps = Self::from_binary(include_bytes!("../assets/default_newlines.packdump"));
        ps.link_syntaxes();
        ps
    }

    pub fn dump_binary(&self) -> Vec<u8> {
        assert!(!self.is_linked);
        let mut v = Vec::new();
        {
            let mut encoder = ZlibEncoder::new(&mut v, Compression::Best);
            encode_into(self, &mut encoder, SizeLimit::Infinite).unwrap();
        }
        v
    }

    pub fn dump_to_file<P: AsRef<Path>>(&self, path: P) -> EncodingResult<()> {
        let f = BufWriter::new(try!(File::create(path).map_err(EncodingError::IoError)));
        let mut encoder = ZlibEncoder::new(f, Compression::Best);
        encode_into(self, &mut encoder, SizeLimit::Infinite)
    }

    /// Returns a fully loaded and linked package set from
    /// a binary dump. Panics if the dump is invalid.
    pub fn from_binary(v: &[u8]) -> PackageSet {
        let mut decoder = ZlibDecoder::new(v);
        let mut ps: PackageSet = decode_from(&mut decoder, SizeLimit::Infinite).unwrap();
        ps.link_syntaxes();
        ps
    }

    /// Returns a fully loaded and linked package set from
    /// a binary dump file.
    pub fn from_dump_file<P: AsRef<Path>>(path: P) -> DecodingResult<PackageSet> {
        let f = try!(File::open(path).map_err(DecodingError::IoError));
        let mut decoder = ZlibDecoder::new(BufReader::new(f));
        decode_from(&mut decoder, SizeLimit::Infinite)
    }
}

#[cfg(test)]
mod tests {
    use package_set::PackageSet;
    #[test]
    fn can_dump_and_load() {
        let mut ps = PackageSet::new();
        ps.load_syntaxes("testdata/Packages", false).unwrap();

        let bin = ps.dump_binary();
        let ps2 = PackageSet::from_binary(&bin[..]);
        assert_eq!(ps.syntaxes.len(), ps2.syntaxes.len());
    }
}
