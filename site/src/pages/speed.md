---
layout: ../layouts/Doc.astro
title: Two speeds
description: How Fennec makes development builds nearly instant and release builds as fast as hand-tuned native code.
---

# Two speeds

<p class="lede">Developing and shipping are different jobs. Fennec gives each its own path: development builds optimize for your time, and release builds optimize for runtime.</p>

## While you work

The goal for development builds is that a change runs in moments.

- **An interpreter runs your changes first.** Fennec's compiler turns your code into compact, typed bytecode that runs immediately, with no time spent generating machine code. Because every type is known ahead of time, the interpreter never checks types while running, which makes it far faster than interpreters for dynamic languages.
- **Hot code gets compiled in the background.** When a function runs often, a fast compiler turns it into native code and swaps it in while the program keeps running.
- **Only touched code is processed.** Functions are prepared the first time they're called, so a large program starts instantly.
- **Dependencies are built once.** Libraries are compiled fully optimized a single time and shared across every project on your machine, so a development build of a game still runs its engine and physics at full speed.
- **Your code and its dependencies use the same memory layout.** Interpreted code calls compiled code directly, with no conversion in between.
- **Hot reload is a goal.** Swapping a changed function into a running game should be the normal way to iterate on gameplay.

## When you ship

Release builds take as long as they need to produce the fastest code.

- A mature optimizing compiler backend, with the whole program optimized together.
- Every generic function specialized for each type it's used with.
- The language's guarantees passed to the optimizer: an `inout` parameter is known to be the only reference to its data, which unlocks optimizations most languages can't prove are safe.
- Compact data layouts: field reordering, and optional values that take no extra space.
- No garbage collector, and a fast memory allocator by default.

## The engines

| Engine | Starts running | Runtime speed | Used for |
| --- | --- | --- | --- |
| Interpreter | Instantly | Several times slower than native | First run of changed code, compile-time evaluation, tests |
| Fast compiler | Quickly, per function | Close to native | Hot code in development builds |
| Optimizing compiler | Slowly | Full speed | Release builds |

## Why builds stay fast

Fast builds come from design decisions, not just a fast compiler.

- **Files compile in parallel, and changes stay local.** Exported functions state their types, so changing a function's body never forces code that calls it to recompile.
- **No macros.** Code generation at compile time runs through the interpreter in a sandbox, so its results can be cached.
- **Simple type checking.** Types are inferred within functions, never across them, so checking takes time proportional to the size of your code.
- **One shared cache with a size limit.** Build output is stored once per machine and shared by every project, with old entries removed automatically, so builds don't fill your disk.

## The performance target

Release builds should land within a few percent of hand-tuned native code. That's a promise we intend to check, not just make:

- A benchmark suite of real programs, including a game update loop, a JSON parser, a hash-map-heavy workload, a numeric kernel and a server.
- Each written idiomatically in Fennec and compared against expert hand-written native code.
- A check on every change: overall speed must stay within 3%, and no single benchmark may fall more than 10% behind. Falling short is a bug, not a footnote.

## What costs nothing

These features exist only at compile time and have no runtime cost: `const` and `let`, `inout`, type inference, `?`, `?.` and `??`, enums and `match`, interfaces and generics, capabilities, and `noalloc`.
