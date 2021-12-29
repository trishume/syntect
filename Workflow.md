# Workflow tips for iterating on syntect

## Use synhtml to quickly test files

```
# Option 1
cargo build --release --example synhtml # first time
./target/release/examples/synhtml path/to/file.ext

# Option 2 (cargo adds 50ms+ overhead)
cargo run --release --example synhtml -- path/to/file.ext
```

Combine with [`entr`](https://github.com/eradman/entr) if there is no hang.

## Check both regex engines

When reducing a bug, it may be helpful to check
if it reproduces both with Oniguruma and fancy-regex.
This can be done by tweaking `Cargo.toml`.

```
default = ["default-onig"]
# change to
default = ["default-fancy"]
```

## Viewing stacks for hangs

* Linux: `eu-stack -p <pid>` (`sudo apt install elfutils`).
* macOS: `sample <pid>`.
