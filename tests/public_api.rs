#[test]
fn public_api() {
    // NOTE: consider switching back to using `public_api::MINIMUM_NIGHTLY_RUST_VERSION` after the
    // version is newer than the one set here
    let nightly_version = "nightly-2023-09-18";
    assert_eq!(
        public_api::MINIMUM_NIGHTLY_RUST_VERSION,
        "nightly-2023-08-25"
    );

    // Install a compatible nightly toolchain if it is missing
    rustup_toolchain::install(nightly_version).unwrap();

    // Build rustdoc JSON
    let rustdoc_json = rustdoc_json::Builder::default()
        .toolchain(nightly_version)
        .build()
        .unwrap();

    // Derive the public API from the rustdoc JSON
    let public_api = public_api::Builder::from_rustdoc_json(rustdoc_json)
        .omit_blanket_impls(true)
        .build()
        .unwrap();

    // Assert that the public API looks correct
    insta::assert_snapshot!(public_api);
}
