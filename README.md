# triviality

This scanner recursively walks one or more paths containing extracted crate
files to determine if they implement non-trivial code.

Non-trivial code is defined as:

1. For a `bin`, a `main.rs` that does more than just `println!("Hello world!")`.
2. For a `lib`, a `lib.rs` that exports one or more types, functions, or pretty
   much anything that can sensibly have a visibility modifier.

## Usage

Minimally:

```sh
cargo run -- PATH/TO/ONE/OR/MORE/EXTRACTED/CRATES
```

## Known issues

Nested `Cargo.toml` manifests within a crate file may result in false positives.
