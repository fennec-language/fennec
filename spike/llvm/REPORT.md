# Can LLVM replace Cranelift for development?

A throwaway spike, run 2026-10-31 on an Apple M3 (8 cores, 16 GB) with LLVM 23.1.1 from the Rust toolchain. Nothing was installed. Load was 5–20 from other sessions, so times are noisy; instruction counts aren't.

## Verdict

**By the rule agreed up front, no, narrowly.** Function-by-function LLVM easily makes the edit-time limit. Its best cacheable tier reaches 83% of target by cycles, against the rule's 85%, with its slowest kernel at 69%. Otherwise Cranelift stays.

**Caching isn't the blocker.** An edit costs under 10 ms of LLVM time when one function changes, at any optimization level. What's left is run speed, the first build, and hot reload, which is untested.

**Recommendation:** keep Cranelift for now, and build the release backend function by function with the cache from the start. It's needed anyway, and it's the cheapest way to get the real answer: if it beats the JIT on every kernel once the IR carries the JIT's facts, drop Cranelift then.

## Results

Run speed is the work of six kernels against the Rust twins (fib, sieve, mandelbrot, nbody, spectral_norm, fannkuch). Compile figures are for a generated program of 1061 functions, about 20,000 lines. All 468 kernel builds printed what the twins print, and all the large program's builds matched the JIT.

| Tier | Edit, 1 function (LLVM ms) | Edit, small function (LLVM ms) | Cold build | Compile memory | Cached output | Speed, cycles (geomean) | Slowest kernel, cycles | Speed, instructions (geomean) |
|---|---|---|---|---|---|---|---|---|
| **Cranelift JIT** | see below | see below | 0.83 s | 120–130 MB (whole process) | 3.1 MB | **85%** | 77% | 49% |
| No optimization, per function | 3 | 2 | 2.3 s | 28–34 MB | 3.1 MB | not re-run | not re-run | 11% |
| Trimmed `-O1`, per function | 7 | 4 | 6.7 s | 36–42 MB | 1.2 MB | 70% | 37% | 55% |
| Trimmed `-O1`, with copies | 7 | 41 (12 functions) | 8.2 s | 36–42 MB | 1.2 MB | 75% | 53% | 59% |
| `-O1`, per function | 7 | 5 | 16 s | 37–43 MB | 1.2 MB | 69% | 37% | 57% |
| `-O1`, with copies | 10 | 56 (12 functions) | 11 s | 37–42 MB | 1.2 MB | 73% | 53% | 61% |
| `-O3`, per function | 10 | 5 | 15 s | 38–46 MB | 1.4 MB | 77% | 43% | 53% |
| **`-O3`, with copies** | 9 | 57 (12 functions) | 13 s | 38–46 MB | 1.5 MB | **83%** | 69% | 58% |
| `-O3`, whole program | full rebuild | full rebuild | 8.5 s | 294 MB | 0.7 MB | 87% | 70% | 67% |
| Rust twin with overflow checks | | | | | | 95% | 75% | 91% |

"With copies" puts a copy of each small callee in the caller's module so it can inline, and the cache key covers those copies. Editing a small function then rebuilds its callers: exactly 12 here, so the dependency tracking worked.

**Whole edit, separate processes, on the large program:**

| Stage | JIT | LLVM, one function |
|---|---|---|
| Nothing changed | 555–572 ms | |
| One function edited | 594–786 ms | |
| Front end | included above | 190–250 ms |
| LLVM | included above | 3–60 ms |
| Link | none | 50–100 ms (`ld` and `rust-lld` about equal) |
| **Total** | **about 600–790 ms** | **about 250–400 ms** |

The LLVM total leaves out hashing, reading the cache and writing out the changed function, so don't read it as beating the JIT. A daemon removes most of the front end for both.

**By kernel, cycles / instructions, % of the plain twin:**

| Kernel | JIT | `-O1` with copies | `-O3` with copies | `-O3` whole |
|---|---|---|---|---|
| fib | 92 / 59 | 53 / 46 | 72 / 49 | 73 / 49 |
| sieve | 91 / 55 | 73 / 66 | 69 / 36 | 80 / 55 |
| mandelbrot | 92 / 75 | 98 / 99 | 100 / 99 | 99 / 99 |
| nbody | 83 / 37 | 72 / 63 | 93 / 72 | 97 / 75 |
| spectral_norm | 79 / 49 | 53 / 40 | 69 / 45 | 70 / 45 |
| fannkuch | 77 / 31 | 102 / 65 | 101 / 64 | 110 / 101 |
| **Geomean** | **85 / 49** | **73 / 61** | **83 / 58** | **87 / 67** |

These compare work with work, so the startup of an empty program is subtracted from both sides. Cycles and instructions disagree a lot: the JIT runs many more instructions at a high rate.

**The rule, line by line:**

| Requirement | Result |
|---|---|
| Edit to running under about 200 ms | Met with a daemon. Without one, about 250–400 ms, still less than the JIT's 600–790 ms on this program |
| 85% of target speed | **Missed narrowly:** 83% geomean by cycles, 69% on the slowest kernel. No cacheable tier passes 61% by instructions |
| Memory not dramatically worse | Met for per-function builds. Whole-program builds take about 300 MB |
| Disk not dramatically worse | Cache met, at about 1.1–1.4 KB a function. Binary: see below |

**Size:** `libLLVM.dylib` is 143.6 MB with every target; the whole `fennec` binary, Cranelift included, is 5.1 MB. A static build with only the targets we need would be smaller, but I didn't measure it. [decisions.md](../../docs/decisions.md) already bundles LLVM into each release's binary, so dropping Cranelift wouldn't lose that 5 MB; it's already gone once LLVM is linked in.

## Why the fast tiers aren't fast

- **Most optimization happens on one function.** Going from no optimization to trimmed `-O1` takes instructions from 11% to 55%. Stepping up to `-O3` adds little by instructions, and costs little per edit.
- **Whole-program work adds about 4 points by cycles** (83% to 87%). `-O3` whole only wins clearly on fannkuch, and partly because it sees the constant `9` passed to `fannkuch`, which the twin hides from its compiler.
- **Inlining small callees matters most,** which is why copies help: spectral_norm goes from 43% to 69% at `-O3`.
- **Codegen can't be trimmed much.** `-O1` passes with no-optimization codegen fall from 56% to 26% by instructions.

**Where the LLVM path loses to the JIT:**
- **fib:** every call pays a stack check and a failure flag, and LLVM won't inline a recursive function into itself. The JIT inlines one level and keeps the stack limit in a register. With no check or flag, `-O3` reaches 73% by instructions against 48%.
- **spectral_norm:** the JIT's syntax-tree path knows a loop count's range and drops overflow and divide-by-zero checks. The IR doesn't carry that yet.
- **sieve:** a comprehension in the IR appends one item at a time, growing the list. `-O3`'s unrolling also makes this loop use more instructions than `-O1`. A reserve in the IR might help; I didn't try one.

The first three emitter fixes mattered more than any pass: telling LLVM a list's header and items don't overlap, giving list lengths a range, and treating loop counters as safe took nbody from 41% to 88% and fannkuch from 55% to 97% by instructions at `-O3` whole. The early runs are in `results/old/`.

## Where LLVM's time goes

From `-time-passes` on a 200-function whole-program module (`results/passes/`):

- **`-O1`:** 0.62 s in passes and 0.45 s in codegen.
- **Passes:** InstCombine is 24%, IndVarSimplify 9%, loop unrolling 8%, SimplifyCFG 6%.
- **Codegen:** instruction selection is 42%, loop strength reduction 10%. At `-O3`, loop strength reduction is 18%.
- **Parsing text is not the problem.** `.ll` parses in 269 ms for 12.2 MB, bitcode in 162 ms, and a single function in 0.2–0.5 ms. Cloning a module, my rough stand-in for building it through the C API, took 402 ms, so the C API might not save anything. Emitting text is fine.

## What I couldn't measure

- **Hot reload.** My path makes object files and runs a linker. Patching a running program needs LLVM's ORC JIT linker, which is untested.
- **Tests.** The IR doesn't lower them yet, so "rebuild only what's needed, including tests" is untested for LLVM.
- **Most of the language.** The prototype covers ints, floats, bools, structs, tuples, lists, direct calls, printing and f-strings. There's no reference counting, strings, enums, optionals, closures or dicts, and it never frees anything. Speed on other kernels could differ in either direction, and these results favor LLVM by skipping reference counting.
- **Failure paths.** Failures carry a span but print no source location, and there's no debug info, which would add compile time.
- **Daemon memory.** How much LLVM holds resident while a daemon runs.
- **The real JIT cost of one function.** Its edit figure is mostly front end, and the difference from a no-change run (about 40–130 ms) is noisy.
- **x86-64, and a quiet machine.** The cycle comparison took the minimum of 15 rounds, with engines taking turns at load 6–7.
- **Other trims.** `O1g` (GlobalISel at `-O1`) gave identical results to `O1`, so that setting may not have taken effect through the C API. Turning off loop strength reduction wasn't tried.

## Reproducing

```bash
cd spike/llvm
CARGO_TARGET_DIR=/Volumes/BuildSSD/fennec-worktrees/llvm-spike/proto CARGO_BUILD_JOBS=2 cargo build --release
python3 measure.py baseline
python3 measure.py speed
python3 measure.py scale 1000 L0 L1d O1 O3
python3 cycles.py
python3 report.py c
```

`measure.py` expects `fennec` built at `/Volumes/BuildSSD/fennec-worktrees/llvm-spike/release/fennec`. Raw numbers are in `results/`. Every run went through `wd.py`, one at a time, niced, and limited to 4 GB and 60 s; nothing hit a limit.
