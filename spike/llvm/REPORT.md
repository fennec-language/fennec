# Can LLVM replace Cranelift for development?

## Follow-up: no JIT at all, with range facts

Run later the same day at load 2–10, with the disk stalling at times. From here the main target is the Rust twin **with overflow checks**, as decided; the unchecked twin is for reference. The rest of this report, from "A throwaway spike" on, is the first round and uses the unchecked twin.

**Added to the emitter:** range facts, known-inside bounds, list reservation, and one level of self-inlining, all copied from what the JIT does. The first round's numbers below don't have them.

### Edit to running, no JIT, 1000-function program (about 20,000 lines)

Per-function `-O3`, cached, each stage timed for real on a quiet run (load about 3). Each edited program's output matched the JIT's.

| Stage | One function edited | Small function, inlined into 12 callers |
|---|---|---|
| Front end, whole program | 155 ms | 155 ms |
| — load / check / check again / lower / hash | 20 / 52 / 52 / 21 / 10 | same |
| Emit changed functions | under 1 ms | 5 ms |
| LLVM `-O3` | 3–11 ms | about 80 ms |
| Link | 45–90 ms | about 70–90 ms |
| Start and run this small program | 20–60 ms | 30 ms |
| **Total** | **about 250–300 ms** | **about 350–400 ms** |

The JIT on the same edits took 413–712 ms, without running `main`, with stalls up to 17.9 s.

**Linkers:** `ld` and `rust-lld` are about equal, at 45–100 ms. `mold` isn't installed, and as far as I recall it no longer supports Mac. Flags like `-no_uuid` changed nothing visible. Sixteen pre-merged shards barely help a normal edit: 30–50 ms to link, plus 27–70 ms to re-merge the changed shard.

**To stay well under 1 s:**

1. **Bound the ripple from inlining.** A helper called by everything rebuilt 1001 functions: **7.9 s** on one core at `-O3`. The same edit without inlining took 265 ms. So rebuild only the edited function at first, with no inlining, and redo callers in the background.
2. **Stop redoing the front end as programs grow.** At 5000 functions (about 100,000 lines) it took 0.8 s or more, and it's the largest stage at every size measured. It checks everything twice today; a daemon keeping its state should remove nearly all of it. That isn't built yet.
3. **Keep the cache out of thousands of small files.** Reading 1061 tiny objects from the USB SSD took 44–72 s on some passes and 0.02 ms a file on others, and I couldn't isolate why. Linking stalled the same way, up to 3.8 s, and `rust-lld` once passed the 60 s limit. The JIT stalled too, up to 17.9 s, so it isn't only LLVM, but a pack file or merged shards is the safer design. It wasn't tested on the internal disk.

**At 5000 functions** (load about 3, noisy): first build took 66 s of LLVM time, 239 s with file writes. Cache is 7.4 MB, the binary 4 MB, link 240–370 ms. Only one edit finished before the watchdog stopped a stalled `rust-lld`, so I have no reliable total. From its stages I'd expect over 1 s without a daemon.

### Kernels: instructions / cycles, % of the Rust twin with overflow checks

Minimum of 15 rounds taking turns, startup subtracted. All outputs matched the twins. Above 100 beats the twin.

| Kernel | JIT | `-O3` per function | `-O3` with copies (cacheable) | `-O3` whole program | Unchecked twin |
|---|---|---|---|---|---|
| fib | 75 / 125 | 95 / 131 | **95 / 134** | 95 / 130 | 128 / 133 |
| sieve | 48 / 91 | 32 / 77 | **32 / 74** | 48 / 87 | 86 / 100 |
| mandelbrot | 75 / 92 | 98 / 100 | **99 / 100** | 99 / 100 | 100 / 100 |
| nbody | 37 / 81 | 74 / 94 | **74 / 93** | 76 / 93 | 100 / 101 |
| spectral_norm | 73 / 84 | 47 / 47 | **74 / 69** | 74 / 69 | 150 / 99 |
| fannkuch | 32 / 80 | 68 / 102 | **68 / 102** | 107 / 113 | 106 / 102 |
| **Geomean** | **54 / 91** | 64 / 87 | **69 / 93** | 81 / 97 | 110 / 105 |
| **Slowest** | 32 / 80 | 32 / 47 | **32 / 69** | 48 / 69 | 86 / 99 |

Against the unchecked twin, for reference: JIT 49 / 86, `-O3` with copies 63 / 88, whole program 73 / 92.

- **Cacheable `-O3` now beats the JIT on both geomeans,** but not on every kernel.
- **It loses to the JIT on sieve** by both measures, and **on spectral_norm by cycles** (69 against 84); spectral_norm ties on instructions (74 against 73).
- **sieve's** low instruction count comes from `-O3`'s unrolling, which costs instructions; trimmed `-O1` scores 69 / 95 on it.
- **Reference counting is still missing,** which flatters LLVM on any kernel that would need it.

### ORC, briefly

It works, and it's optional. Running `step` was swapped in a live program through slots, and later rounds printed the new values (checked by hand).

| | Result |
|---|---|
| Swap, one function | 4 ms best, 4–13 ms in the running program |
| Swap, 12 functions | 31 ms |
| Load the whole program from one merged object | 28 ms, against 45–90 ms to link |
| Run speed through ORC | 74 / 96, no worse than the executable |

A call through a slot is one extra load and an indirect call. A function already on the stack keeps old code until it returns, and a caller that inlined the old code has to be swapped too.

**Hot reload doesn't exist in the JIT either.** Nothing in `crates/jit` or `daemon.rs` does it, and it would need the same slots. So it isn't a point in Cranelift's favor today.

### Memory

| Process | Resident | Its own (footprint) |
|---|---|---|
| Today's `fennec daemon` on this program | 37 MB | not measured |
| LLVM loaded, nothing else | 20 MB | 4 MB |
| LLVM with all 1061 functions loaded through ORC | 42 MB | 21 MB |
| After 10 swaps, old code kept | 55 MB | 24 MB |
| Compiling, per-function `-O3` | about 45 MB | |
| JIT, whole process including front end | about 120 MB | about 100 MB |

Much of the resident figure is a shared, mapped `libLLVM.dylib`. No front-end state is included for LLVM.

### What would stop one LLVM backend

1. **The language isn't covered.** No reference counting, strings, enums, optionals, closures or dicts, and only 6 of the 25 kernels. This is the biggest risk, and today's numbers can only get worse from adding it.
2. **sieve and spectral_norm still lose to the JIT,** and nothing's beaten the checked twin overall except on a few kernels.
3. **The first build** took 16 s of LLVM time on one core at 1000 functions (up to 45 s with file writes), against 0.8 s, and again after every compiler upgrade.
4. **Toolchain size:** 143.6 MB against 5.1 MB, unless a static build with few targets is much smaller. Not measured.
5. **A daemon that reuses front-end state** is needed for large programs, and isn't built.
6. **x86-64** and failure paths with real source locations and debug info are unmeasured.

---

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
python3 edit_to_run.py 1000
python3 orc_measure.py live llvm_idle kernels scale1000
python3 cycles_report.py
python3 e2r_report.py
```

`measure.py` expects `fennec` built at `/Volumes/BuildSSD/fennec-worktrees/llvm-spike/release/fennec`. Raw numbers are in `results/`. Every run went through `wd.py`, one at a time, niced, and limited to 4 GB and 60 s; nothing hit a limit.
