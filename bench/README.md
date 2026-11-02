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
| Streaming | `generators`, stages chained through names and functions |
| Closures and iterators | `comprehensions`, `closures` |
| Object-heavy code with dispatch | `richards`, `deltablue` |
| What falls off the fast path | `dict_field`, `struct_keys`, `method_chain`, `comp_helpers` |
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

Instructions aren't time, though. They can disagree by a lot, in either direction: on an Apple M3 under load, `sieve` was 57% by instructions and 93% by time. So the table shows both. The percentage by time compares with how long the twin took when it was pinned, so it's only as good as how alike the two machines are. The floor and the ratchet go by instructions, because they're stable.

The table also counts the places in each kernel's compiled code that aren't native, from `fennec dev fallbacks`, and the run warns about them, since code that falls off the fast path prints nothing when it runs.

Each kernel also has to print exactly what its target expects, which the target file holds as a hash.

Targets come from twins built with overflow checks on, because Fennec always checks and that's the work being compared. The run's last column shows the percentage against the same twins without those checks, from `targets/unchecked/`. It's for reference only: nothing is judged on it.

## The floor and the ratchet

**The JIT's floor is 80% of target**, on every kernel, rising to 85% later. Past that is nice but not the goal. The 80% is a floor on work, not on startup.

Once a kernel reaches the floor, the run says so. Mark it `yes` in the last column of `x86_64.txt`, the only architecture CI measures, and from then on CI fails if it drops below. Reaching the floor on x86-64 says nothing about 64-bit Arm, so that file's marks wait until Arm is measured the same way. A `.std` row is never marked. A failure is fixed in the compiler, never by changing the benchmark.

The target itself is 100%. Each engine aims for its own share of it:

| Engine | Aim |
|---|---|
| Release builds | 110% of target, when they exist |
| The JIT | 80% now, 85% later |
| The interpreter | 10–30% |

The goal across the suite is 95% of target. Reports show it against both twins: unchecked, which is what's pinned today, and built with overflow checks, as Fennec is. Those checks cost Rust itself about 5% by cycles and 9% by instructions. The checked twins come when the tool adds them.

To compare against another build of `fennec`, like the previous commit's, pass `--previous path/to/fennec`. CI does that on every pull request, merge and push to `main`. A kernel counts as slower when its median time is more than 10% slower than before and it also runs more than 1% more instructions. For now that's a warning, until the counts have proven stable on pull requests that change nothing on the run path.

## Keeping it honest

A twin has to do the same job as its kernel, not just print the same answer. `dicts` uses the same hash as Fennec's dicts, because the target's default hash, SipHash, is the stronger one Fennec's dicts only move to when their probes get suspiciously long, and using it would make the comparison about the hash. `dicts.std` keeps the default twin as a separate row, for what someone gets from the target's standard library without choosing.

These hold on both sides, the kernel and its twin:

- **Inputs the optimizer can't see through.** Twins read their sizes through `std::hint::black_box`. Nothing is computed at compile time.
- **Results are printed and checked.** Every result depends on all of the work, and its hash is in the target file.
- **Realistic work.** No kernel does something an optimizer can delete. A twin whose count looks too low for its work gets looked at, and noted here.

## LLVM against Rust

`fennec-bench llvm-suite` builds every kernel with `fennec build --llvm --release` and runs it
beside its overflow-checked twin, counting instructions where it can and timing both: ten runs
each, taking turns and swapping which goes first. Each number is the twin's count or time over
LLVM's, so 100 matches Rust. Times are each side's best run, which filters out other work on a
shared machine, with the median beside it. A kernel is marked noisy when a median run is more
than 5% slower than the best, on either side. The Suite report workflow runs it on
x86 Linux, where Valgrind counts, and on Arm macOS, after every push to `main` and on demand.
It puts one table on the run's summary page and adds each push's numbers to `history.jsonl`
on the `bench-history` branch. Being slow never fails it.

## Pinning targets

The twins beside the kernels are idiomatic code for the same work, in Rust, fully optimized: `opt-level=3`, fat link-time optimization, one codegen unit, abort on panic and no debug assertions. The CPU level is fixed, so a target doesn't depend on which machine pinned it, and matches what the JIT uses: `x86-64-v3` on x86-64 and the baseline on 64-bit Arm. Each targets file says which compiler pinned it. `regex`'s twin runs on `crates/re`, the engine behind `std.re`. `fennec-bench pin` builds them, checks that each prints what its kernel prints, and writes their instruction counts to `targets/<architecture>.txt`, keeping each kernel's `enforced` mark. The twin of `selfhost` is `fennec dev tree`, the Rust front end.

Everyday runs don't build or run the twins. They only read the targets.

Re-pin only when a kernel's work or a twin changes, by running the **Pin benchmark targets** workflow, which pins each architecture the same way, then copying what it prints into `targets/`.

## Adding a kernel

The suite is locked, so a new kernel needs a stated reason. Add `name.fen` and `name.rs` to `kernels/`, then re-pin.

- Size the work so that Rust takes between about 10 and 100 ms. Valgrind counts every kernel on every CI run, and a kernel the JIT is still slow on takes far longer than its twin.
- Print a result that depends on all of the work, so neither side can skip it.
- In Rust, read the input size through `std::hint::black_box`, so the answer isn't computed at compile time.
- The Fennec code must be formatted; CI checks it with `fennec fmt --check`.
