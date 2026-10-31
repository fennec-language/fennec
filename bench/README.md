# Benchmarks

Each kernel in `kernels/` is a Fennec program that stands for one way a language has to be fast. Every kernel has a target pinned in `targets/`, and the suite reports how fast Fennec runs it as a percentage of that target.

```bash
cargo build --release -p fennec -p fennec-bench
./target/release/fennec-bench
```

| Area | Kernels |
|---|---|
| Calls, recursion | `fib` |
| Integer math and arrays | `sieve`, `fannkuch` |
| Float math | `nbody`, `spectral_norm`, `mandelbrot` |
| Allocation and recursive data | `binary_trees` |
| Hash tables | `dicts` |
| Strings and text | `strings`, `json`, `k_nucleotide`, `regex` |
| Structs in a game loop | `particles` |
| Sorting and generics | `sort`, `generics` |
| Closures and iterators | `comprehensions`, `closures` |
| Object-heavy code with dispatch | `richards`, `deltablue` |
| A real program | `selfhost`, the self-hosted front end parsing the kernels' own source |

These are what languages have historically been judged on: most of the Computer Language Benchmarks Game, and `richards` and `deltablue` from Are We Fast Yet. **The suite is locked.** Each target records a hash of its kernel and twin, and a run fails if they've changed, or if a kernel has come or gone. Changing one means pinning it again, and the pull request has to say why. The fix for a slow kernel goes in the compiler.

## What it reports

Startup and work are told apart:

- **Startup** is everything before `main`: loading, checking, compiling or reusing cached code. `fennec dev startup` stops there.
- **Work** is the whole run minus startup. It's what's compared with the target, so a slow start can't hide a fast loop, or the other way around.

A kernel's speed is its target's instructions divided by the instructions its work takes. Instructions are the measure because they barely move when a machine is busy, and they don't depend on how fast it is. Times are shown too.

- Where the CPU's counters can be read, `perf stat` counts.
- Where they can't, as on CI's virtual machines, Valgrind's cachegrind counts by simulating. It's about 50 times slower.
- On a Mac, `time -l` counts. The targets are pinned on Linux, and starting any program on a Mac takes millions more instructions, so a Mac's percentages are only a guide, above all on the smallest kernels.

Each kernel also has to print exactly what its target expects, which the target file holds as a hash.

## The floor and the ratchet

**The JIT's floor is 80% of target**, on every kernel. Past that is nice but not the goal. The 80% is a floor on work, not on startup.

Once a kernel reaches the floor, the run says so. Mark it `yes` in the last column of the targets file for the architecture CI runs on, `x86_64.txt`, and from then on CI fails if it drops below. A failure is fixed in the compiler, never by changing the benchmark.

The target itself is 100%. Each engine aims for its own share of it:

| Engine | Aim |
|---|---|
| Release builds | 110% of target, when they exist |
| The JIT | 80% now, 85% later |
| The interpreter | 10–30% |

To compare against another build of `fennec`, like the previous commit's, pass `--previous path/to/fennec`. CI does that on every pull request, merge and push to `main`. A kernel counts as slower when its median time is more than 10% slower than before and it also runs more than 1% more instructions. For now that's a warning, until the counts have proven stable on pull requests that change nothing on the run path.

## Keeping it honest

These hold on both sides, the kernel and its twin:

- **Inputs the optimizer can't see through.** Twins read their sizes through `std::hint::black_box`. Nothing is computed at compile time.
- **Results are printed and checked.** Every result depends on all of the work, and its hash is in the target file.
- **Realistic work.** No kernel does something an optimizer can delete. A twin whose count looks too low for its work gets looked at, and noted here.

## Pinning targets

The twins beside the kernels are idiomatic code for the same work, in Rust, fully optimized: `opt-level=3`, fat link-time optimization, one codegen unit, abort on panic and no debug assertions, for the architecture's baseline CPU, so a target doesn't depend on which machine pinned it. `regex`'s twin runs on `crates/re`, the engine behind `std.re`. `fennec-bench pin` builds them, checks that each prints what its kernel prints, and writes their instruction counts to `targets/<architecture>.txt`, keeping each kernel's `enforced` mark. The twin of `selfhost` is `fennec dev tree`, the Rust front end.

Everyday runs don't build or run the twins. They only read the targets.

Re-pin only when a kernel's work or a twin changes, by running the **Pin benchmark targets** workflow, which pins each architecture the same way, then copying what it prints into `targets/`.

## Adding a kernel

The suite is locked, so a new kernel needs a stated reason. Add `name.fen` and `name.rs` to `kernels/`, then re-pin.

- Size the work so that Rust takes between about 10 and 100 ms. Valgrind counts every kernel on every CI run, and a kernel the JIT is still slow on takes far longer than its twin.
- Print a result that depends on all of the work, so neither side can skip it.
- In Rust, read the input size through `std::hint::black_box`, so the answer isn't computed at compile time.
- The Fennec code must be formatted; CI checks it with `fennec fmt --check`.
