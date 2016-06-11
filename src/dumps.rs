use bincode::SizeLimit;
use bincode::rustc_serialize::*;
use std::fs::File;
use std::io::BufReader;
use package_set::PackageSet;
use std::path::Path;

impl PackageSet {
    pub fn dump_binary(&self) -> Vec<u8> {
        assert!(!self.is_linked);
        encode(self, SizeLimit::Infinite).unwrap()
    }

    pub fn dump_to_file<P: AsRef<Path>>(&self, path: P) -> EncodingResult<()> {
        let mut f = try!(File::create(path).map_err(EncodingError::IoError));
        encode_into(self, &mut f, SizeLimit::Infinite)
    }

    /// Returns a fully loaded and linked package set from
    /// a binary dump. Panics if the dump is invalid.
    pub fn from_binary(v: Vec<u8>) -> PackageSet {
        let mut ps: PackageSet = decode(&v[..]).unwrap();
        ps.link_syntaxes();
        ps
    }

    /// Returns a fully loaded and linked package set from
    /// a binary dump file.
    pub fn from_dump_file<P: AsRef<Path>>(path: P) -> DecodingResult<PackageSet> {
        let f = try!(File::open(path).map_err(DecodingError::IoError));
        let mut reader = BufReader::new(f);
        decode_from(&mut reader, SizeLimit::Infinite)
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
        let ps2 = PackageSet::from_binary(bin);
        assert_eq!(ps.syntaxes.len(), ps2.syntaxes.len());
    }
}
