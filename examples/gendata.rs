extern crate syntect;
use syntect::package_set::PackageSet;

fn main() {
    let mut ps = PackageSet::new();
    ps.load_syntaxes("testdata/Packages", true).unwrap();
    ps.dump_to_file("assets/default_newlines.packdump").unwrap();

    let mut ps2 = PackageSet::new();
    ps2.load_syntaxes("testdata/Packages", false).unwrap();
    ps2.dump_to_file("assets/default_nonewlines.packdump").unwrap();
}
