---
layout: ../layouts/Doc.astro
title: Two speeds
description: How Fennec makes development builds nearly instant and release builds as fast as hand-tuned native code.
---

# Two speeds

<p class="lede">Developing and shipping are different jobs. Fennec gives each its own path: development builds optimize for your time, and release builds optimize for runtime.</p>

## While you work

The goal for development builds is that a change runs in moments.

- **Your code runs as native code right away.** A fast compiler turns each function into machine code the first time it's called, in microseconds. Only the code that runs gets compiled, so a large program starts instantly.
- **Reruns reuse what's already compiled.** Compiled functions are cached on your machine by their content, so a change recompiles only what it touched.
- **Dependencies are built once.** Libraries are compiled fully optimized a single time and shared across every project on your machine, so a development build of a game still runs its engine and physics at full speed.
- **Your code and its dependencies use the same memory layout.** Your code calls into them directly, with no conversion in between.
- **Hot reload is a goal.** Swapping a changed function into a running game should be the normal way to iterate on gameplay.

## When you ship

Release builds take as long as they need to produce the fastest code.

- A mature optimizing compiler backend, with the whole program optimized together.
- Every generic function specialized for each type it's used with.
- The language's guarantees passed to the optimizer: a `mut` parameter is known to be the only reference to its data, which unlocks optimizations most languages can't prove are safe.
- Compact data layouts: field reordering, and optional values (`Enemy?`) that take no extra space.
- Immutable updates done in place: `replace()` reuses the original's memory when the original isn't used again.
- Generators compiled to state machines on the stack, so streaming through data allocates nothing.
- No garbage collector, and a fast memory allocator by default.

## Values without references

Fennec code never handles references, but the compiler uses them behind the scenes wherever it's safe:

| You write | What the compiler does |
| --- | --- |
| `describe(player)` | Passes a hidden read-only reference; no copy |
| `apply_damage(mut player, 10)` | Passes a hidden exclusive reference |
| `b = a`, and `a` isn't used again | Moves it, for free |
| `b = a`, and both are used | Shares the memory until one of them changes, then copies once |
| Small values like `Vec3` | Copies them in registers, for free |

The cost of sharing is a reference count on large data, mostly optimized away, and a copy the first time shared data changes. That copy isn't written in the code, so three things keep it in check: the compiler warns when a change inside a loop might copy, `noalloc` code rejects it outright, and the profiler reports hidden copies by name.

## Libraries can't change silently

Errors are inferred, so a library could change how a function fails without any visible edit to its signature. When a package is published, Fennec compares the inferred signatures of everything it exports, including the errors each function can fail with, against the previous version, and flags every change.

## The engines

| Engine | Starts running | Runtime speed | Used for |
| --- | --- | --- | --- |
| Interpreter | Instantly | Several times slower than native | First run of changed code, compile-time evaluation, tests |
| Fast compiler | Quickly, per function | Close to native | Hot code in development builds |
| Optimizing compiler | Slowly | Full speed | Release builds |

## Why builds stay fast

Fast builds come from design decisions, not just a fast compiler.

- **Changes stay local.** When you edit a function, the compiler checks whether its signature changed, including the errors it can fail with. Almost always it hasn't, and nothing that calls the function is checked again.
- **No macros.** Code generation at compile time runs through the interpreter in a sandbox, so its results can be cached.
- **Simple type checking.** Parameters are typed, so each function is checked on its own once the signatures it calls are known. There are no program-wide inference puzzles, and files are checked in parallel.
- **One shared cache with a size limit.** Build output is stored once per machine and shared by every project, with old entries removed automatically, so builds don't fill your disk.

## The performance target

Each benchmark has a target: idiomatic code for the same work in the fastest established systems language, fully optimized. Release builds aim for 110% of its speed, the fast compiler for development for 80% to start and then 85%, and the interpreter for 10–30%. That's a promise we intend to check, not just make:

- A benchmark suite of real programs, including a game update loop, a JSON parser, a hash-map-heavy workload, a numeric kernel and a server.
- Each written idiomatically in Fennec and compared against expert hand-written native code.
- Realistic work that neither side's optimizer can delete.
- Targets are built for a fixed, modern CPU level so they're reproducible. Where the target's standard library does something Fennec doesn't, like a slower default hash, the target matches Fennec, and the standard default is shown as its own number.
- A check on every change: a benchmark that misses its target or gets slower fails. Falling short is a bug, not a footnote.

## What costs nothing

These features exist only at compile time and have no runtime cost: `mut`, type inference, `?`, `?.` and `else`, optional types, error handling, enums and `match`, interfaces and generics, generators, tuples, `with`, capabilities, and `noalloc`.
