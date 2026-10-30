---
layout: ../layouts/Doc.astro
title: Roadmap
description: Where Fennec is today, what gets built first, and the design questions still open.
---

# Roadmap

<p class="lede">Fennec is in early design. The plan is to start with a small, solid core, then add each feature when real programs need it.</p>

## Now: design

- This site, describing the language as it's currently designed.
- A written specification of the core language.
- A short guide for AI models, the same one that will ship with the compiler.
- The AI evaluation suite: tasks for models to solve in Fennec, so syntax decisions can be tested before the compiler exists.

## v0.1: the core runs

The first milestone is running the examples on this site.

- Parser, type checker and formatter.
- The intermediate representation, designed for native code from the start.
- The interpreter.
- `const` and `let`, `def` with inferred return types, `struct` with methods, `enum` and `match`.
- `Result` with `?`, and optional types with `None`, `?.` and `??`.
- Interfaces and generics.
- Values with automatic moves, `mut`, and the ownership checker.
- `list`, `dict`, `set`, `tuple`, comprehensions, generator expressions and the builtins.
- `test` blocks.

## v0.2: native speed

- The fast compiler for development builds, with hot functions promoted from the interpreter.
- The optimizing compiler for release builds.
- The shared build cache.
- The benchmark suite and the performance check on every change.

## v0.3: games

- `noalloc`, per-frame memory arenas, struct-of-arrays layout, SIMD and math types.
- C interop with `extern`, `__del__` and `unsafe`.
- A platform layer for windows, input, audio and graphics.

## Later

- `class` with reference counting and compile-time cycle checks.
- `frozen[...]` for large shared immutable data.
- Views into strings and lists that avoid copies.
- Contracts with automatic testing, and property tests for every type.
- Capabilities throughout the standard library.
- Hot reload.
- A Fennec compiler written in Fennec.

## Open questions

These are still being decided:

- Whether integer overflow checks stay on in release builds by default. The benchmark suite will decide.
- How closures longer than one line should look, beyond nested functions.
- The exact syntax for property tests.
- How views into collections are returned from functions without lifetime annotations.
- The file extension (currently `.fen`).
