---
layout: ../layouts/Doc.astro
title: Roadmap
description: Where Fennec is today, what gets built first, and the design questions still open.
---

# Roadmap

<p class="lede">Fennec is in early design. The plan is to start with a small, solid core, then add each feature when real programs need it.</p>

## Now: design and the first slice

- A first interpreter that runs simple programs: functions, `if`/`elif`/`else`, loops, lists, comprehensions, f-strings, `mut` and Fennec's error messages. Every example in the repository's `examples/` folder is a test.
- This site, describing the language as it's currently designed.
- A written specification of the core language.
- A short guide for AI models, the same one that will ship with the compiler.
- The AI evaluation suite: tasks for models to solve in Fennec, so syntax decisions can be tested before the compiler exists.

## v0.1: the core runs

The first milestone is running the examples on this site.

- Parser, type checker and formatter.
- The intermediate representation, designed for native code from the start.
- The interpreter.
- Names and `mut`, `def` with inferred return types, structs with defaults, named constructors and `where`, `enum` and `match`.
- Errors with `error`, `fail`, `?`, `else`, `match` and `from`, and optional types with `None` and `?.`.
- Implicit interfaces and generics.
- Values without references: automatic moves, sharing until changed, and the check that `mut` access is exclusive.
- `list`, `dict`, `set`, `tuple`, comprehensions, generators and the builtins.
- `with` and `test` blocks.

## v0.2: native speed

- The typed bytecode interpreter.
- The fast compiler for development builds, with hot functions promoted from the interpreter.
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

These are still being decided:

- Whether ID types are written with an alias (`type EnemyId = Id[Enemy]`) or provided automatically (`Enemy.Id`).
- Whether integer overflow checks stay on in release builds by default. The benchmark suite will decide.
- How closures longer than one line should look, beyond nested functions.
- Whether independent writes, not just reads, should overlap automatically.
- The exact syntax for property tests.
- How views into collections are returned from functions without lifetime annotations.
- The file extension (currently `.fen`).
