# syntect-no-panic

This is a fork of [Syntect](https://crates.io/crates/syntect).

This fork is very similar to the original version 5.2 of Syntect, but modified to avoid panicking.

With this fork, the behavior on a syntax having an invalid rule isn't to panic anymore, and is tuned with options:

```rust
let options = HighlightOptions {
    ignore_errors: true,
    ..Default::default()
};
HighlightLines::new(syntax, theme, options)
```

With `ignore_errors: false`, a faulty regex in a syntax, detected when highlighting some lines, will result in an error.

With `ignore_errors: true`, the rule containing the faulty regex won't be applied but rest of the highlighting process will proceed.

Please try to use the original Syntect instead of this temporary fork.

There's no project to add features: as soon as I can use the normal syntect, this repository will be considered obsolete to avoid fragmenting the ecosystem.

If you really feel like you need to use this, contact @dystroy on [the Miaou chat](https://miaou.dystroy.org/3768).
