# Self-hosting plan

**Goal:** the Fennec compiler written in Fennec, building itself, so the language is developed in the language. The runtime stays in reviewed Rust for now. Until the switch, today's compiler stays as the reference, and every step below is checked against it.

## Where it stands

`selfhost/` is a Fennec kit of 3,761 lines that mirrors the Rust front end file for file. `crates/fennec/tests/selfhost.rs` builds it through LLVM and compares its output with the Rust front end's on 151 examples, 202 error examples, 58 kit files, the standard library's Fennec sources, selfhost's own 13 files and about 100 small cases.

| Part | Lines | How far it gets |
|---|---|---|
| Lexer | 477 | Prints the same tokens as Rust on every file compared |
| Parser | 1,598 | Prints the same tree as Rust on every file compared |
| Checker | 786 | Compares 11 kinds of error, mostly declarations and extensions, out of about 258 places the Rust checker reports one |

There's no lowering or code generation in Fennec yet.

## What the language needs first

| Needed | On main? |
|---|---|
| Bit operations on `int` | Yes (#251) |
| Sized numbers: `u8`, `u32`, `u64`, `f64.to_bits()` and the rest | No: decided, not built |
| `with` and `lend`, for files and the compiler's own resources | No: decided, not built |
| Dicts and strings close to Rust | Partly: at 70% of Rust on `dicts`, 64% on `strings` and 52% on `k_nucleotide`, measured 2026-11-05 |

## The backend

The Fennec compiler writes LLVM's text IR (`.ll`) and runs LLVM's tools, `opt` and `llc`, as separate programs, the same tools today's compiler ships. So there's no C binding and no `unsafe` anywhere in the compiler.

## Steps, in order

Each step has an owner and a check that proves it's done.

1. **The language prerequisites land.** Owners: checker and LLVM lanes. *Check:* sized numbers, bit operations on them, and `with`/`lend` each have an example that builds natively and prints what its `.out` says.
2. **The self-hosted checker reports everything the Rust checker does.** Owner: selfhost lane. *Check:* `CHECK_KINDS` is removed, and the two checkers print the same diagnostics on every example, error example, kit, standard library source and `bench/kernels` program.
3. **The front end is fast enough to live in.** Owner: selfhost lane, with the LLVM lane for any slow paths found. *Check:* checking the self-hosted compiler's own source and the standard library fits in the edit-loop budget that today's compiler is held to.
4. **Lowering to IR in Fennec.** Owner: selfhost lane. *Check:* a dump of the typed IR, compared like the trees are today, matches Rust's lowering on every example and kernel.
5. **Code generation in Fennec, writing `.ll`.** Owners: selfhost and LLVM lanes. *Check:* every example and kernel built by the Fennec compiler prints the same as when built by today's compiler, and each kernel's speed stays within 5% of today's build. The Fennec compiler's own edit-to-run time stays under twice today's. That's a ceiling, not a target, and the optimizer loop closes the gap.
6. **It builds itself.** Owner: selfhost lane. *Check:* today's compiler builds the Fennec compiler (stage 1), stage 1 builds it again (stage 2), stage 2 builds it once more (stage 3), and stages 2 and 3 are identical byte for byte. Then the Fennec compiler becomes the one that ships, as long as it's still under the edit-to-run ceiling, and today's compiler stays as stage 0.

Until step 6, a change to the front end is made in both compilers, and the selfhost test fails when they disagree.

## Standard library pieces to write in Fennec early

Each of these replaces Rust in the standard library, and makes sized numbers and bit operations prove themselves on real code. Owner: stdlib lane.

1. **A table-driven `pow`,** once `u64` and `f64.to_bits()` land. *Check:* it gives the same bits as today's `pow` on a fixed set of inputs, and it's at least as fast.
2. **Writing and reading floats as text,** in shortest round-trip form. *Check:* the same text as today, and every float reads back to the same bits.
3. **`exp`, `log`, `sin` and `cos`,** which decisions.md already puts in the standard library. *Check:* the same bits on every platform, against a fixed table.
4. **BLAKE3,** which decisions.md says is written in-house. *Check:* the official test vectors.
