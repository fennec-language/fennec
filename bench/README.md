# Benchmarks

Each kernel in `kernels/` is a Fennec program that stands for one way a language has to be fast. Every kernel has a target pinned in `targets/`, and the suite reports how fast Fennec runs it as a percentage of that target. Each kernel is built by `fennec build --llvm --release` and run as an executable, the way a program ships.

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
| Shared state under a lock | `locks`, a counter and a dict changed under `lock ... as ref` in a loop |
| What falls off the fast path | `dict_field`, `struct_keys`, `method_chain`, `comp_helpers` |
| A game server's tick | `game_tick`: a `Table` of entities with inventory dicts, commands through a channel, a grid lookup with `parallel` over its cells, and a `pack` snapshot |
| Game AI | `pathfind`: A* on a 512 by 512 map for many units, with a `Heap` of structs ordered by `<` and a dict of the cells visited |
| Physics | `physics`: balls in a box as lists of `std.math`'s `Vec3`, filed in a grid by a counting sort for the broad phase, and pushed apart and bounced |
| A developer's tool | `grep_tool`: a tree of source files written and walked through an `Fs` narrowed with `only`, read in chunks and matched line by line with `std.re` |
| A chat server | `chat_server`: a hub task owning the rooms fans each message out over channels to four shard tasks owning the users' mailboxes, which send back what each user got |
| A nightly job | `data_pipeline`: CSV parsed by `std.csv` into structs, totalled, and every order through `std.pack` and `std.json` and back |
| A real program | `selfhost`, the self-hosted front end parsing the kernels' own source |
| A JSON API | `api_handler`: requests parsed from their text, bodies loaded into structs with `where` rules, users in a dict of structs written in place, orders in a dict of lists, and responses built with `json.dumps` and by joining text |
| A log cruncher | `log_crunch`: log lines split and counted into dicts of structs and counters, times appended to a dict of lists, sorted for percentiles, and a report built with f-strings and `+=` |

These are what languages have historically been judged on: most of the Computer Language Benchmarks Game, and `richards` and `deltablue` from Are We Fast Yet. **The suite is locked.** Each target records a hash of its kernel and twin, and a run fails if they've changed, or if a kernel has come or gone. Changing one means pinning it again, and the pull request has to say why. The fix for a slow kernel goes in the compiler.

## What it reports

Startup and work are told apart:

- **Startup** is what any built program takes before its `main`: the whole run of `startup/empty.fen`, built the same way. Building isn't counted.
- **Work** is the whole run minus startup. It's what's compared with the target, so a slow start can't hide a fast loop, or the other way around.

A kernel's speed is its target's instructions divided by the instructions its work takes. Instructions are the measure because they barely move when a machine is busy, and they don't depend on how fast it is. Times are shown too.

- Where the CPU's counters can be read, `perf stat` counts.
- Where they can't, as on CI's virtual machines, Valgrind's cachegrind counts by simulating. It's about 50 times slower.
- On a Mac, `time -l` counts. The targets are pinned on Linux, and starting any program on a Mac takes millions more instructions, so a Mac's percentages are only a guide, above all on the smallest kernels.

Instructions aren't time, though. They can disagree by a lot, in either direction: on an Apple M3 under load, `sieve` was 57% by instructions and 93% by time. So the table shows both. The percentage by time compares with how long the twin took when it was pinned, so it's only as good as how alike the two machines are. The floor and the ratchet go by instructions, because they're stable.

The table also counts the functions in each kernel that LLVM doesn't take yet, from `fennec dev fallbacks --llvm`, and the run warns about them. A kernel with any doesn't build, so the run fails on it too. `fallbacks.txt` holds those counts, which are all zero, and a test keeps them so. CI's "Every example and kernel built natively" job also builds every kernel natively and checks that it links and prints what it should.

Each kernel also has to print exactly what its target expects, which the target file holds as a hash.

`copies.txt` holds how many copies each kernel's run makes: values changed while something else still held them, so the change went to a copy. A test builds each kernel for release, runs it on one thread under `FENNEC_COUNTS` and keeps the counts exact. They don't move with the machine or its load, so a copy the code generator starts making fails there, however small its cost in time.

Targets come from twins built with overflow checks on, because Fennec always checks and that's the work being compared. The run's last column shows the percentage against the same twins without those checks, from `targets/unchecked/`. It's for reference only: nothing is judged on it.

## The floor and the ratchet

**The floor is 80% of target**, on every kernel, rising to 85% later. Past that is nice but not the goal. The 80% is a floor on work, not on startup. It was set for the JIT, which is gone. LLVM's release builds clear it by far, so it stays as a floor.

Once a kernel reaches the floor, the run says so. Mark it `yes` in the last column of `x86_64.txt`, the only architecture CI measures, and from then on CI fails if it drops below. Reaching the floor on x86-64 says nothing about 64-bit Arm, so that file's marks wait until Arm is measured the same way. A `.std` row is never marked. A failure is fixed in the compiler, never by changing the benchmark.

The target itself is 100%. LLVM's release builds, which the suite measures, aim for 110% of it.

The goal across the suite is 95% of target. Reports show it against both twins: unchecked, which is what's pinned today, and built with overflow checks, as Fennec is. Those checks cost Rust itself about 5% by cycles and 9% by instructions. The checked twins come when the tool adds them.

To compare against another build of `fennec`, like the previous commit's, pass `--previous path/to/fennec`, with its runtime library beside it so it can build the kernels too. CI does that on every pull request, merge and push to `main`. A kernel counts as slower when its median time is more than 10% slower than before and it also runs more than 1% more instructions. For now that's a warning, until the counts have proven stable on pull requests that change nothing on the run path.

## Keeping it honest

A twin has to do the same job as its kernel, not just print the same answer. `dicts` uses the same hash as Fennec's dicts, because the target's default hash, SipHash, is the stronger one Fennec's dicts only move to when their probes get suspiciously long, and using it would make the comparison about the hash. `dicts.std` keeps the default twin as a separate row, for what someone gets from the target's standard library without choosing.

These hold on both sides, the kernel and its twin:

- **Inputs the optimizer can't see through.** Twins read their sizes through `std::hint::black_box`. Nothing is computed at compile time.
- **Results are printed and checked.** Every result depends on all of the work, and its hash is in the target file.
- **Realistic work.** No kernel does something an optimizer can delete. A twin whose count looks too low for its work gets looked at, and noted here.

## LLVM against Rust, Go and Java

`fennec-bench llvm-suite` builds every kernel with `fennec build --llvm --release` and runs it
beside its overflow-checked twin, counting instructions where it can and timing both: ten runs
each, taking turns and swapping which goes first. Each number is the twin's count or time over
LLVM's, so 100 matches Rust. Times are each side's best run, which filters out other work on a
shared machine, with the median beside it. A kernel is marked noisy when a median run is more
than 5% slower than the best, on either side. Each kernel is also built the dev way,
`fennec build --llvm` without `--release`, cached a function at a time, and raced against the
twin too, so a slip in dev builds shows. Both builds are timed, each from an empty Fennec home
so no cache helps. The Suite report workflow runs it on
x86 Linux, where Valgrind counts, and on Arm macOS, after every push to `main` and on demand.
Where Valgrind counts, it also simulates the caches, so each kernel shows its first-level data
cache and last-level misses, Fennec's and the Rust twin's, beside its instructions. The caches
are fixed (32 KiB first level, 8 MiB last level, 64-byte lines), so the counts are as
repeatable as the instruction counts, whichever machine runs them.
It puts one table on the run's summary page and adds each push's numbers to `history.jsonl`
on the `bench-history` branch. Being slow never fails it.

A kernel can have Go and Java twins too, `name.go` and `name.java` beside `name.fen`. They do
the same work in each language's plain standard library and print the same thing. They're
built the way anyone would build them, `go build` and `javac`, with nothing tuned, and timed
against the LLVM build the same way, by best time. The workflow installs Go 1.26 and Temurin
Java 25. Each comparison has two numbers. The whole run is each program from start to exit;
Java's includes the JVM starting and its JIT warming up, as it does for anyone running the
program. Kernel time is what each program says its own work took: with `--time`, it prints
one more line, `kernel-ns N`, from a monotonic clock started at the beginning of its work.
The Rust, Go and Java twins take `--time` themselves; Java runs its work once first, to warm
its JIT up, and times the second run. A Fennec kernel stays as it is: the suite builds it
from a copy whose `main` adds the same two lines. Without `--time`, each does
the same work and prints the same thing as ever. The report leads
with kernel time, and shows each language's startup, a hello-world's best run from
`bench/startup/empty.*`, beside it. Rust is still the twin the LLVM backend is held to: a Go or Java twin
that prints something else is noted, and doesn't fail the run. Where a toolchain isn't
installed, its column is left out.

## `parallel` against rayon

`fennec-bench parallel` measures the kernels in `bench/parallel`, which run on threads, by wall clock at 1, 2, 4, 8 and 16 threads, as many of those as the machine has cores for, and at all its cores, each beside a twin using rayon. They cover each form `parallel` takes: `ecs` is a `for ref` loop of light work run 200 times, `mandel` one of heavy, uneven work, `collatz` a comprehension of uneven work, and `checksum` a reduction of even work. Each kernel's version without `parallel`, like `ecs_plain.fen`, runs beside plain Rust with no rayon, so each table says what `parallel` costs on one thread as well as what it gives on many.

- It's outside the locked suite, which counts instructions a thread at a time, and has no targets. Being slow never fails it: only printing something other than the twin does.
- The twins are a small cargo project of their own in `bench/parallel/twin`, with rayon pinned in its lock file. rayon is there only to be measured against: Fennec's pool uses `rayon-core`, the same version the twin's rayon runs on.
- It needs the runtime with threads next to `fennec`, as `libfennec_runtime_threads.a` and `.bc`. CI builds them in `suite.yml`, and each machine's table joins the suite's report.

## Real programs against Rust with crates

`fennec-bench realworld` measures `bench/realworld`: programs that use `std` the way a real program does, beside a Rust twin using the crates a Rust program would. `json_typed` loads JSON into typed structs through `std.json`, uses them and writes them back, against serde and serde_json. The `json` kernel in the suite is a parser written by hand on both sides, so it doesn't measure `std.json`.

- It's outside the locked suite, whose twins are one file each, and has no targets. Being slow never fails it: only printing something other than the twin does.
- The twins are a small cargo project of their own in `bench/realworld/twin`, built with the same flags as the suite's, with their crates pinned in its lock file. serde is there only to be measured against, and nothing shipped depends on it.
- Each machine's table joins the suite's report.

## Pinning targets

The twins beside the kernels are idiomatic code for the same work, in Rust, fully optimized: `opt-level=3`, fat link-time optimization, one codegen unit, abort on panic and no debug assertions. The CPU level is fixed, so a target doesn't depend on which machine pinned it, and is `x86-64-v3` on x86-64 and the baseline on 64-bit Arm. Each targets file says which compiler pinned it. `regex`'s twin runs on `crates/re`, the engine behind `std.re`. `fennec-bench pin` builds them, checks that each prints what its kernel prints, and writes their instruction counts to `targets/<architecture>.txt`, keeping each kernel's `enforced` mark. The twin of `selfhost` is `fennec dev tree`, the Rust front end.

Everyday runs don't build or run the twins. They only read the targets.

Re-pin only when a kernel's work or a twin changes, by running the **Pin benchmark targets** workflow, which pins each architecture the same way, then copying what it prints into `targets/`.

## Single operations

`fennec-bench ops` measures what one call of an everyday operation costs, in instructions: `find`, `split` on one byte and on two, a slice, `+`, `int.parse`, `float.parse`, `removesuffix`, an f-string, `json.loads` and `json.dumps` of a small struct, a dict's `get` and set with `str` keys, and making a struct. Each is a program in `bench/ops` that does its operation `OPS` times on inputs that vary, beside a Rust twin doing the same the idiomatic way.

- Each is built twice, as it is and with `OPS = 0`, and the difference over `OPS` is one operation's cost, with setup, startup and printing left out.
- Each operation's cost is pinned per architecture in `bench/ops/<architecture>.txt`, with its twin's and a hash of what both print. CI's bench job fails when an operation takes more than its pin, by more than 5% and 10 instructions. So a slow path fails the day it's added, and an improvement shows as a lower count until it's pinned again.
- The **Pin benchmark targets** workflow pins them with the suite's targets, checking that each prints what its twin prints. Copy what it prints into `bench/ops/`.

## Build times

`fennec-bench build-times` measures how much work Fennec does for the scoreboard's eight real programs and `selfhost`, in four steps:

- `cold`: a release build in a new Fennec folder;
- `warm`: a dev build after one line is added;
- `check`: `fennec check` after another;
- `run`: `fennec run` on a program the store has.

Each step is the median of five runs. The count is the instructions retired by every process the step starts, the compiler, LLVM's tools and the linker. On a Mac, `time -l` counts only the process it starts, so each tool runs through a script that counts it too. A run's count is `fennec`'s own work, leaving out the program's.

- Each count is pinned in `bench/build-times.txt`, and a step more than 3% over its pin fails.
- Wall time is shown, with the machine's load. A step that takes half again as long as when pinned only warns, since the load moves it.
- The morning sweep runs it on the Mac. `fennec-bench build-times --pin` writes what it measured as the new pins, which a change that makes builds cheaper does in the same pull request.

## Adding a kernel

The suite is locked, so a new kernel needs a stated reason. Add `name.fen` and `name.rs` to `kernels/`, then re-pin.

- Size the work so that Rust takes between about 10 and 100 ms. Valgrind counts every kernel on every CI run, and a slow kernel takes far longer than its twin.
- Print a result that depends on all of the work, so neither side can skip it.
- In Rust, read the input size through `std::hint::black_box`, so the answer isn't computed at compile time.
- The Fennec code must be formatted; CI checks it with `fennec fmt --check`.
