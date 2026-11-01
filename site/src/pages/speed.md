---
layout: ../layouts/Doc.astro
title: How Fennec gets fast
description: Fennec's plan for speed, one LLVM backend with a function-level cache, along with what runs today.
---

# How Fennec gets fast

<p class="lede">The plan is simple: one optimizing backend, and a cache good enough that you rarely wait for it. What you run while you work is what you ship.</p>

## Today

`fennec run` and `fennec build` compile to native code with Cranelift, one function at a time, and each function is cached by its content, so a change recompiles only what it touched. `fennec build --llvm` handles part of the language. The [benchmarks](/benchmarks/) show where this stands against its targets.

## The plan: one backend

**Planned.** These are decided, and not built yet.

- **LLVM for everything,** with per-function `-O3`. There's no separate development engine, no tiering and no hot reload.
- **`fennec run` makes real binaries** in a store, so what you try is what you ship.
- **Whole-program optimization only in `--release`.** It's the one step that sees everything at once, so it's the one step you opt into.
- **Cranelift goes away** when LLVM wins across the benchmark suite, edits stay well under a second, and CI's rebuild comparison stays clean.

## The plan: one store

**Planned.** Today's cache becomes one content store per machine.

- **Results are named by hash** and written by atomic rename, with a size cap and eviction. There are no `target` folders.
- **A key covers what matters:** a function's typed IR, the signatures of what it calls, the bodies of anything inlined into it, and the compiler version, target and flags.
- **An unchanged signature skips the callers,** so a change inside a function stays inside it.
- **A daemon is optional and purely a speedup.** The plain command gives identical results without it.
- **A shared cache** can be written only by trusted builds, and CI does a clean rebuild and compares it, because a wrong cache is the worst bug a build system can have.

## What the language hands the optimizer

These come from the design, so they apply whichever backend runs:

- A `mut` parameter is known to be the only way to reach its data, which most languages can't prove.
- Fields can be reordered and optional values packed, because nothing can observe where a value sits in memory.
- No garbage collector, and flat data with no hidden pointers.

## Values without references

Fennec code never handles references, but the compiler uses them behind the scenes wherever it's safe:

| You write | What the compiler does |
| --- | --- |
| `describe(player)` | Passes a hidden read-only reference; no copy |
| `apply_damage(mut player, 10)` | Passes a hidden exclusive reference |
| `b = a`, and `a` isn't used again | Moves it, for free |
| `b = a`, and both are used | Shares the memory until one of them changes, then copies once |
| Small values like `Vec3` | Copies them in registers, for free |

The cost of sharing is a reference count on large data, mostly optimized away, and a copy the first time shared data changes. That copy isn't written in the code, so the plan is to keep it in check three ways: a warning when a change inside a loop might copy, `@hot` rejecting it outright, and a report of hidden copies by name.

## Libraries can't change silently

When a package is published, Fennec will compare the signatures of everything it exports, including the errors each function can fail with, against the previous version, and flag every change.

## Why builds stay fast

Fast builds come from design decisions, not just a fast compiler.

- **Changes stay local.** When you edit a function, the compiler checks whether its signature changed, including the errors it can fail with. Almost always it hasn't, and nothing that calls the function is checked again.
- **No macros.** Compile-time loops are ordinary checked code with no I/O, so their results can be cached.
- **Simple type checking.** Parameters are typed, so each function is checked on its own once the signatures it calls are known. There are no program-wide inference puzzles.

## The performance target

Each benchmark has a target: idiomatic code for the same work in the fastest established systems language, fully optimized and with overflow checks, as Fennec has. Release builds aim for 110% of its speed, the fast compiler for development for 80% to start and then 85%, and the interpreter for 10–30%. That's a promise we intend to check, not just make:

- A benchmark suite of real programs, including a game update loop, a JSON parser, a hash-map-heavy workload, a numeric kernel and a server.
- Each written idiomatically in Fennec and compared against expert hand-written native code.
- Realistic work that neither side's optimizer can delete.
- Targets are built for a fixed, modern CPU level so they're reproducible. Where the target's standard library does something Fennec doesn't, like a slower default hash, the target matches Fennec, and the standard default is shown as its own number.
- A check on every change: a benchmark that misses its target or gets slower fails. Falling short is a bug, not a footnote.

## What costs nothing

These exist only at compile time and have no runtime cost: `mut`, type inference, `?` and `else`, optional types, interfaces, tuples and capabilities.
