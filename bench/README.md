# Benchmarks

Each kernel in `kernels/` is a Fennec program and a Rust twin that does the same work, the
idiomatic way. `fennec-bench` builds the twin with `rustc -C opt-level=3 -C target-cpu=native`,
runs both on the same machine, taking turns, 7 times each, and compares medians. They must
print the same thing.

```bash
cargo build --release -p fennec -p fennec-bench
./target/release/fennec-bench
```

To compare against another build of `fennec`, like the previous commit's, pass
`--previous path/to/fennec`. CI does that on every pull request, merge and push to `main`.
A kernel fails when its median time is more than 10% slower than before and it also runs
more than 3% more instructions. Time on a shared CI machine can jump by 10% or more when a
neighbour is busy, or when code merely moves around in the binary; instruction counts don't.

- Where the CPU's counters can be read, `perf stat` counts every run.
- Where they can't, as on CI's virtual machines, Valgrind's cachegrind counts by simulating. It's
  about 50 times slower, so it only confirms a kernel that looks slower.
- With neither, a slowdown is measured again with three times the runs before it fails.

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
