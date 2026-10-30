# Contributing to Fennec

Fennec is early and changes quickly. Ideas, bug reports and pull requests are welcome.

## Building and testing

The compiler has no dependencies beyond Rust's standard library. The Rust version is pinned in `rust-toolchain.toml`, and rustup installs it on first use.

```bash
cargo test
```

```bash
cargo run -q -p fennec -- run examples/game.fen
```

## Examples are the specification

Every program in `examples/` is a test: it must print exactly its `.out` file and pass its own `test` blocks. Every program in `examples/errors/` must fail with exactly its `.err` message. When you add a feature, add an example that uses it; when you add a guardrail, add an error example that shows its message.

## Adding a standard library module

Each module under `std` has its own files, so modules added in parallel don't touch the same lines:

- `crates/syntax/src/stdlib/`: the names it provides, in `<module>.rs`, and any code written in Fennec, in `<module>.fen`.
- `crates/interp/src/stdlib/`: its functions and methods written in Rust, if it has any.
- `crates/check/src/stdlib/`: their signatures.

Then add the module's name to the `modules!` list in each crate's `stdlib.rs`, one line each.

## Changing the lexer, parser or checker

`selfhost/` is Fennec's front end written in Fennec, and a test checks that it matches the Rust one exactly. A change to `crates/syntax` or `crates/check` usually needs the same change in `selfhost/`. "Changing the front end" in [selfhost/README.md](selfhost/README.md) says where.

## The bar a change has to clear

CI runs all of this on every pull request, and a change merges only when it all passes:

- `cargo fmt --check`, and `cargo clippy` with every warning denied. Safety lints like `unwrap_used`, `panic` and `indexing_slicing` are forbidden outside tests.
- Tests on Linux (x86 and ARM), macOS and Windows.
- Mutation testing on the code the pull request changes: every small change `cargo-mutants` makes must make a test fail.
- `cargo deny` for licenses, advisories and sources, and dependency review for anything new.
- CodeQL on the Rust, the website's code and the workflows.

New dependencies need a reason, and new versions must be at least 7 days old.

## Language design

Design discussion happens in issues. The [website](https://fennec-lang.dev/) describes the current design, and the [roadmap](https://fennec-lang.dev/roadmap/) lists the open questions.
