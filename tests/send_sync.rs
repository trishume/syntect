//! Compile-time guarantees that the types users share across threads
//! (see the Readme's Parallelizing section) stay `Send + Sync`. A
//! regression here fails compilation of this test crate, not a runtime
//! assertion.

#![allow(dead_code)]

fn assert_send_sync<T: Send + Sync>() {}

// `ParseState` / `CommittedParser` are deliberately absent: under the
// onig backend a state's capture snapshots hold `onig::Region`s (raw
// pointers), so parse state is per-thread by construction — threads
// share the `SyntaxSet` and each build their own state.
#[cfg(feature = "parsing")]
const PARSING: fn() = || {
    assert_send_sync::<syntect::parsing::SyntaxSet>();
    assert_send_sync::<syntect::parsing::SyntaxSetBuilder>();
    assert_send_sync::<syntect::parsing::SyntaxReference>();
    assert_send_sync::<syntect::parsing::ParseLineOutput>();
    assert_send_sync::<syntect::parsing::ParseWarning>();
    assert_send_sync::<syntect::parsing::ScopeStack>();
    assert_send_sync::<syntect::parsing::Scope>();
};

const HIGHLIGHTING: fn() = || {
    assert_send_sync::<syntect::highlighting::Theme>();
    assert_send_sync::<syntect::highlighting::ThemeSet>();
    assert_send_sync::<syntect::highlighting::HighlightState>();
};

#[test]
fn send_sync_asserts_compile() {
    // The consts above are the assertions; this test exists so the
    // file registers as a test target.
}
