---
layout: ../layouts/Doc.astro
title: Roadmap
description: Where Fennec is today, what gets built first, and the design questions still open.
---

# Roadmap

<p class="lede">Fennec is in early design. The plan is to start with a small, solid core, then add each feature when real programs need it.</p>

## Now: the interpreter runs most of the language

- `fennec run`, `fennec test` (including property tests that shrink failures), `fennec new` and `fennec repl`.
- `fennec fmt`: one style with no options, and `fennec fmt --check` for CI. It keeps every comment and checks that the syntax tree is unchanged before it writes.
- Programs over several files: a kit is a folder with a `kit.toml`, and its files import from each other.
- Names and `mut`, functions with inferred returns, nested functions, arrow functions.
- Structs with defaults, methods, named constructors and `where` rules; enums and `match`; operators with `def +` and `def []`.
- Errors with `error`, `fail`, `?`, `else`, `match` and `from`; optional values with `None`, `?.` and `else`.
- Lists, dicts, sets, tuples, comprehensions, slices, generators, `with`, `IdMap` and `Vec3`.
- Values without references: copies are independent, and changes happen in place without copying when nothing else shares the value.
- Every example in the repository's `examples/` folder is a test, including the error messages.
- A first type checker, run before every program and test: unknown names, `mut`, arguments and fields, bool conditions, int and float mixing, unhandled errors and `None`, `match` over every case, and inferred return types.
- The first standard library modules: `std.collections` (`Deque`, `Heap`, `PriorityQueue`, `Counter`), `std.random` (seeded, the same on every machine), `std.time` (`Duration`, and a monotonic `Clock` passed to `main`) `std.re` (regular expressions in linear time) and `std.hash` (BLAKE3, written in-house).
- A first native compiler: `fennec run` compiles programs to machine code with Cranelift, and every example runs with exactly the interpreter's output. Numbers stay in machine registers, so numeric code runs about 70 times faster than in the interpreter; other values still go through the interpreter's runtime for now.

## v0.1: checked before it runs

The next milestone moves Fennec's checks from runtime to compile time.

- The type checker: inferred types, exhaustive `match`, and errors and `None` that can't be ignored.
- The language's lints.
- The intermediate representation, designed for native code from the start.
- The interpreter.
- Names and `mut`, `def` with written return types, structs with defaults, named constructors and `where`, `enum` and `match`.
- Errors with `error`, `fail`, `?`, `else`, `match` and `from`, and optional types with `None` and `?.`.
- Implicit interfaces and generics.
- Values without references: automatic moves, sharing until changed, and the check that `mut` access is exclusive.
- `list`, `dict`, `set`, `tuple`, comprehensions, generators and the builtins.
- `with` and `test` blocks.

## v0.2: native speed

- The fast compiler for development builds, compiling each function on its first call.
- The optimizing compiler for release builds.
- The shared build cache.
- The benchmark suite and the performance check on every change.
- Standard library core: `fs`, `path`, `json`, `toml`, `time`, `random`, `re`, `collections`.

## v0.3: games

- `noalloc`, per-frame memory arenas, struct-of-arrays layout, SIMD and math types.
- C interop with `extern`, `ptr[...]`, `def del` and `unsafe`.
- A platform layer for windows, input, audio and graphics.

## v0.4: concurrency and networking

- Waiting on I/O without `async`: the runtime runs other work, and independent reads overlap automatically.
- `parallel(...)` loops, with the compiler suggesting where they're safe and worthwhile.
- Tasks scoped to a `with` block, locks and channels.
- `http`, `net`, `tls` and `sqlite`.

## Later

- Views into lists that avoid copies, as string slices already do.
- Contracts with automatic testing, and property tests for every type.
- Hot reload.
- Shared objects with their own identity, if real programs turn out to need them.
- A Fennec compiler written in Fennec.

## Open questions

These are still being decided. Decisions already made, and why, are in [the design decisions](https://github.com/fennec-language/fennec/blob/main/docs/decisions.md).

- Whether to add decorators, as annotations read at compile time.
- Whether independent writes, not just reads, should overlap automatically.
- The exact syntax for property tests.
- The file extension (currently `.fen`).
- Whether files of a kit may reuse a top-level name, reached through `import` by their module's name.
