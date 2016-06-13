extern crate syntect;
use syntect::package_set::PackageSet;
use syntect::dumps::*;

fn main() {
    let mut ps = PackageSet::new();
    ps.load_syntaxes("testdata/Packages", true).unwrap();
    dump_to_file(&ps, "assets/default_newlines.packdump").unwrap();

    let mut ps2 = PackageSet::new();
    ps2.load_syntaxes("testdata/Packages", false).unwrap();
    dump_to_file(&ps2, "assets/default_nonewlines.packdump").unwrap();
}
