---
layout: ../layouts/Doc.astro
title: How Fennec gets fast
description: How Fennec runs near native speed and builds almost instantly, with one LLVM backend and a function-level cache.
---

# How Fennec gets fast

<p class="lede">The plan is simple: one optimizing backend, and a cache good enough that you rarely wait for it. What you run while you work is what you ship.</p>

## Today

`fennec run` builds through LLVM one function at a time, and each function is cached by its content, so a change recompiles only what it touched. Where LLVM can't build a program yet, it says why, as `fennec build` does. `fennec build` always makes a native program through LLVM, and `--release` optimizes the whole program as one.

In CI, release builds of the benchmark suite run at about the speed of hand-written twins in a fast, C-class systems language. That's the whole run of each program, startup included. A development build of one kernel takes well under a second. The [benchmarks](/benchmarks/) have every number and how it's measured.

## The plan: one backend

- **LLVM for everything,** with per-function optimization. It's the default today. There's no separate development engine and no tiering.
- **Whole-program optimization only in `--release`.** It's the one step that sees everything at once, so it's the one step you opt into. Built.
- **The JIT is gone, and `fennec` runs nothing on the interpreter,** since everything builds through LLVM. A program either builds natively or is refused, with the reason. The commit before the JIT went has a tag.

## The plan: one store

Everything Fennec keeps between commands is in one store per machine, in `~/.fennec`. Built:

- **Results are named by hash** and written by atomic rename, with a size cap and eviction. There are no `target` folders, and a kit's folder holds only its own files.
- **A key covers what matters:** a function's typed IR, the signatures of what it calls, the bodies of anything inlined into it, and the compiler version, target and flags.
- **An unchanged signature skips the callers,** so a change inside a function stays inside it.
- **A daemon is optional and purely a speedup.** The plain command gives identical results without it.
- **Planned: a shared cache,** which can be written only by trusted builds, and CI does a clean rebuild and compares it, because a wrong cache is the worst bug a build system can have.

## What the language hands the optimizer

These come from the design, so they apply whichever backend runs:

- A `ref` parameter is known to be the only way to reach its data, which most languages can't prove.
- Fields can be reordered and optional values packed, because nothing can observe where a value sits in memory.
- No garbage collector, and flat data with no hidden pointers.
- A function with no capabilities can't do I/O or change anything outside itself, so repeated calls can be merged and moved out of loops.
- Generic code is copied for each type it's used with, so every call is direct.

## Values without references

Fennec code never handles references, but the compiler uses them behind the scenes wherever it's safe:

| You write | What the compiler does |
| --- | --- |
| `describe(player)` | Passes it without copying, since a plain parameter can't change |
| `apply_damage(ref player, 10)` | Passes the one way to reach it |
| `b = a`, and `a` isn't used again | Hands it over, with no copy |
| `b = a`, and both are used | Shares the memory until one of them changes, then copies once |
| Small values like `Vec3` | Copies them in registers, for free |

The cost of sharing is a reference count on large data, mostly optimized away, and a copy the first time shared data changes. That copy isn't written in the code, so it's kept in check three ways: a copy made on every round of a loop is a compile error, `@hot` rejects any copy at all, and `fennec dev trace` names each copy and why it happened.

## Safe and fast together

Safety checks stay unless the compiler proves them unnecessary. Speed comes only from those proofs, never from switching checks off.

- **Overflow is checked everywhere,** release builds included. Range analysis removes the checks on loop counters, indices and bounded math, where it can show a number can't overflow.
- **Bounds checks go** where an index provably can't be out of range.

## Why builds stay fast

Fast builds come from design decisions, not just a fast compiler.

- **Changes stay local.** When you edit a function, the compiler checks whether its signature changed, including the failures it can fail with. Almost always it hasn't, and nothing that calls the function is checked again.
- **No macros and no build scripts.** Nothing runs code while compiling, so every result can be cached.
- **Simple type checking.** Parameters and return types are written, so each function is checked on its own once the signatures it calls are known. There are no program-wide inference puzzles.

## The performance target

Each benchmark has a twin: idiomatic code for the same work in a fast, C-class systems language, fully optimized and with overflow checks, as Fennec has. Release builds aim for 110% of the twin's speed on every kernel. That's a promise we intend to check, not just make:

- A suite of kernels that cover the ways a language has to be fast: calls, float math, hash tables, strings, allocation, generics, closures and object-heavy code.
- Each written idiomatically in Fennec and compared against expert hand-written native code.
- Realistic work that neither side's optimizer can delete.
- Twins are built for a fixed CPU level so they're reproducible. Where the twin's standard library does something Fennec doesn't, like a slower default hash, the twin matches Fennec, and the standard default is shown as its own number.
- A slow kernel is fixed in the compiler, never by changing the benchmark. Once a kernel reaches its floor, CI fails if it drops below it.

## What costs nothing

These exist only at compile time and have no runtime cost: `mut` and `ref`, type inference, `?` and `else`, optional types, interfaces, unions, tuples and capabilities.
