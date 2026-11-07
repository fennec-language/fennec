---
layout: ../layouts/Doc.astro
title: Roadmap
description: What Fennec does today, what's being built next, and the design questions still open.
---

# Roadmap

<p class="lede">Fennec's design is settled unless real programs or benchmarks say otherwise. Now it's a matter of building it, so this page is mostly a list of what's done and what isn't.</p>

## Works today

- `fennec run`, which runs the tests a change affects and then your program. `fennec build`, `fennec test`, `fennec bench`, `fennec new`, `fennec init`, `fennec fmt`, `fennec check`, `fennec lint` (each with JSON for agents), `fennec fix` and a language server.
- Programs over several files: a kit is a folder with a `kit.toml`, and its files import from each other.
- Names and `mut`, functions with written return types, default parameters passed by name, nested functions and arrow functions. Constructor calls like `dict[str, int]()` as defaults, the only form for an empty collection.
- `ref` for loops, parameters and `self`, with compile errors for changing a collection while it's looped over, for a copy made on every round of a loop, and for a local that's changed and then never read.
- Structs with defaults, methods, named constructors and `where` rules; enums and `match`; union types, `Circle | Square`, narrowed with `is` and `match`; operators; interfaces and generics.
- Errors with `error`, `fail`, `?`, `else`, `match` and `from`, and `fails Name` in signatures, with one `error` listing several cases. `?` is required wherever a failure passes up. Optional values with `None` and `else`.
- Lists, dicts, sets, tuples, comprehensions and slices. Structs work as dict keys with nothing to write, and one with its own `def ==` is a compile error. `sum(xs, start=...)`.
- Lazy generators, used once, with a second use a compile error wherever the compiler can see it.
- Hard errors for positional calls that are easy to get backwards.
- `export` on top-level things only, and `_name` hiding a field or method from everything but its type.
- Fields named with any word, keywords included, like `from: str`.
- `@hot`, which proves a function allocates nothing, copies nothing and keeps no checks it can't prove away, or doesn't compile.
- One equality, order and hash for floats, with NaN equal to itself and sorted last.
- `parallel` over collections and streams, on every core, with `for ref` through it and reductions that combine in a fixed order.
- `shared Name:`, with methods that lock automatically, on real threads. It's being redesigned in places.
- Tables: `Table`, `Id` and `Tree`, and `SortedMap`, `SortedSet`, `Grid`, `Packed`, `Deque` and `Counter`.
- The standard library: `math`, `layout`, `path`, `json`, `toml`, `pack`, `csv`, `collections`, `random`, `time`, `re`, `log`, `cli`, `net`, `process` and `crypto`. JSON takes `names=`, `extra=` and `@serialize(name=...)`.
- A type checker that runs before every program and test.
- Native code through LLVM: `fennec run` builds a function at a time and caches each one, and `fennec build` makes a native program, with `--release` optimizing the whole program as one. Where LLVM can't build something yet, a JIT stands in and says so, and the interpreter runs the little that neither can.
- Function-level caching of checked and compiled code, shared across runs.
- Every example in the repository's `examples/` folder is a test, including its error messages, and the [language reference](/reference/) is generated from them.

## Next: speed and builds

- Everything on LLVM, so the JIT and the interpreter can go. What doesn't build natively yet is on a tracked list that must reach zero.
- One content store per machine, with results named by hash and no `target` folders. `fennec run` makes real binaries in it.
- A shared cache that only trusted builds can write to, and a clean rebuild in CI to check it against.
- A tiny launcher that fetches the compiler a kit asks for. Pinned compilers never change by themselves.

## Next: the rest of the language

Each of these is decided and not built.

- A compile error for a copy that can go stale, which `copy(x)` makes on purpose.
- Narrowing with `if x is Err(...)`, and a compile error for checking an error and then ignoring it.
- `xs.get(i)`, which gives an optional when an index might not be there.
- `fennec fix` applying only fixes that can't change what a program does, with the rest shown as hints to make by hand.
- Sized numbers, like `i32`, `u8` and `f32`.
- `x.approx(y, within=...)`, and `@fast_math`.

## Being redesigned

- `with`, `def del` and how a thing is closed, including several in one line and a block's last line as its value.
- Parts of `shared`.

## After that: concurrency

- `limit=` on `parallel`, and user types splitting for it through their positions.
- `with launch(fn) as task:` for long-running tasks, and bounded channels with `send`, `pending()` and `for x in ch`.
- Event channels, for many listeners each reading at its own pace.
- A failure in a server task staying in that task.

## After that: libraries and C

- Compile-time loops over a type, and `Writer` and `Reader` for every format built on them. `where` ranges, delta encoding and quantization in `std.pack`.
- `Graph`, built on `Table` as `Tree` is.
- `std.sql`.
- `extern "C"` and `unsafe`, for what can't be written in Fennec, and first-party native kits.
- SIMD and GPU types, and [the Den](/den/), Fennec's registry for kits.
- A Fennec compiler written in Fennec. Its lexer, parser and checker are underway.

## Only if the evidence asks

These stay out unless measurements show they're needed: a limited `View[T]` for stored views, and tighter lock patterns. `yield mut` helpers, `@arena` and compile-time channel deadlock checks were cut from the first version, and each can come later without breaking anything.

## Open questions

These are still being decided. Decisions already made, and why, are in [the design decisions](https://github.com/fennec-language/fennec/blob/main/docs/decisions.md).

- Whether kits can define their own annotations.
- Whether independent writes, not just reads, should overlap automatically.
- How a type marks that its finishing method has to be called.
- How replay records the order tasks reach a `shared` value or a channel.
- The exact API of event channels.
- The API of `Graph`.
- The exact syntax for property tests, and for units of measure.
- The file extension (currently `.fen`).
- Whether files of a kit may reuse a top-level name, reached through `import` by their module's name.
- SQLite: whether and how Fennec includes it.
