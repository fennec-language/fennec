# Benchmarks

Each kernel in `kernels/` is a Fennec program and a Rust twin that does the same work, the
idiomatic way. `fennec-bench` builds the twin with `rustc -C opt-level=3 -C target-cpu=native`,
runs both on the same machine, taking turns, and keeps each one's fastest of 5 runs. They must
print the same thing.

```bash
cargo build --release -p fennec -p fennec-bench
./target/release/fennec-bench
```

To compare against another build of `fennec`, like the previous commit's, pass
`--previous path/to/fennec`. CI does that on every pull request, merge and push to `main`, and
fails if a kernel runs more than 10% slower than before. 10% is the noise of a shared CI
machine, even taking the best of several runs.

## Targets

These are the speed goals from `docs/decisions.md`:

- **Release builds:** within 3% of the Rust twin. This becomes a failing check when release
  builds exist.
- **The JIT:** at least 80% of Rust's speed. The table shows where each kernel stands.

A failure is fixed in the compiler, never by changing the benchmark.

## Adding a kernel

Add `name.fen` and `name.rs` to `kernels/`.

- Size the work so today's JIT finishes in a second or two, since every run happens 5 times in
  CI.
- Print a result that depends on all of the work, so neither side can skip it.
- In Rust, read the input size through `std::hint::black_box`, so the answer isn't computed
  at compile time.
- The Fennec code must be formatted; CI checks it with `fennec fmt --check`.
