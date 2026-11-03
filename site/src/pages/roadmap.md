---
layout: ../layouts/Doc.astro
title: Roadmap
description: What Fennec does today, what's being built next, and the design questions still open.
---

# Roadmap

<p class="lede">Fennec's design is settled unless real programs or benchmarks say otherwise. Now it's a matter of building it, so this page is mostly a list of what's done and what isn't.</p>

## Works today

- `fennec run`, which runs the tests a change affects and then your program. `fennec test`, `fennec bench`, `fennec new`, `fennec init`, `fennec fmt`, `fennec check`, `fennec lint` (each with JSON for agents), `fennec fix`, `fennec repl` and a language server.
- Programs over several files: a kit is a folder with a `kit.toml`, and its files import from each other.
- Names and `mut`, functions with written return types, default parameters passed by name, nested functions and arrow functions.
- Structs with defaults, methods, named constructors and `where` rules; enums and `match`; operators; interfaces and generics.
- Errors with `error`, `fail`, `?`, `else`, `match` and `from`; optional values with `None` and `else`.
- Lists, dicts, sets, tuples, comprehensions and slices. Structs work as dict keys with nothing to write.
- Lazy generators, used once, with `yield` allowed inside `with`.
- `with` and `def del`: any way out of a block closes what it opened.
- Tables: `IdMap`, `Id` and `Tree`, and `SortedMap` on a B-tree.
- A type checker that runs before every program and test.
- The first standard library: `math`, `game`, `layout`, `path`, `json`, `toml`, `collections`, `random`, `time` and `re`.
- Native code: `fennec run` uses a cached LLVM build, and says so in one line when LLVM can't build a program yet and the JIT runs it instead. `fennec build` compiles with Cranelift, and `fennec build --llvm` handles part of the language. Every example gives exactly the interpreter's output.
- Function-level caching of checked and compiled code, shared across runs.
- Every example in the repository's `examples/` folder is a test, including its error messages, and the [language reference](/reference/) is generated from them.

## Next: speed and builds

- One backend: LLVM, with per-function `-O3` and a function-level cache. Whole-program work happens only in `--release`.
- LLVM becomes the default once about 90% of examples build natively, and that retires Cranelift. A program either builds natively or is refused, and refused examples go on a tracked list that must reach zero.
- One content store per machine, with results named by hash and no `target` folders. `fennec run` makes real binaries in it.
- A shared cache that only trusted builds can write to, and a clean rebuild in CI to check it against.
- A tiny launcher that fetches the compiler a kit asks for. Pinned compilers never change by themselves.

## Next: the rest of the language

Each of these is decided and not built.

- `for mut` over user types, through `def len` and the `mut` form of `def []`.
- A compile error for changing a collection while it's being looped over or written through.
- `xs.pair(i, j)`, and compile errors for copies that can go stale and for a copy on every iteration of a loop.
- `fails Name` in signatures, with one `error` listing several cases. `?` required wherever a failure passes up, narrowing with `if x is Err(...)`, and a compile error for checking an error and then ignoring it.
- `xs.get(i)`, and `sum(xs, start=...)` replacing today's positional start.
- `_name` hiding a field, and `pub` on top-level things only.
- Constructor calls like `dict[str, int]()` as defaults, giving the type as a literal does, and the only form for an empty collection.
- Fields named with any word, keywords included, like `from: str`. Soft keywords like `type` and `match` work as field names today.
- `names=` and `extra=` on `json.loads` and `json.dumps`, and `@serialize(name=...)`.
- Union types, `Circle | Square`, inline with a one-byte tag, narrowed with `is` and `match`.
- Hard errors for positional calls that are easy to get backwards.
- `fennec fix` applying only fixes that can't change what a program does, with the rest shown as hints to make by hand.
- `with a() as x, b() as y:` and a block's last line as its value.
- A compile error for a dict key struct with its own `def ==`.
- `@hot`, which proves a function allocates nothing, copies nothing and has no leftover count checks, or doesn't compile. It replaces today's `noalloc def`.
- One equality, order and hash for floats, with NaN equal to itself and sorted last, `x.approx(y, within=...)`, and `@fast_math`.

## After that: concurrency and sharing

- `parallel` over collections and streams, on real cores, with `limit=`. Today it compiles for reading, and runs in order.
- User types splitting for `parallel` through their positions, and `(f(x) for x in parallel(xs))` streaming results in order.
- `with launch(fn) as task:` for long-running tasks, and bounded channels with `send`, `pending()` and `for x in ch`.
- One scheduler, with I/O multiplexed by the OS.
- `shared Name:`, with methods that lock automatically.
- A failure in a server task staying in that task, designed when servers are built.

## After that: libraries and C

- Compile-time loops over a type, `Writer` and `Reader` for every format, annotations as data, and `std.pack`.
- `SortedSet`, `Grid` and `Packed`.
- `std.net`, `std.sql`, `std.process` and `std.crypto`.
- `extern "C"` and `unsafe`, for what can't be written in Fennec, and first-party native kits.
- SIMD and GPU types, and [the Den](/den/), Fennec's registry for kits.
- A Fennec compiler written in Fennec.

## Only if the evidence asks

These stay out unless measurements show they're needed: `class` with `link` fields, a limited `View[T]` for stored views, and tighter lock patterns. `yield mut` helpers, a `Graph` type, `@arena` and compile-time channel deadlock checks were cut from the first version, and each can come later without breaking anything.

## Open questions

These are still being decided. Decisions already made, and why, are in [the design decisions](https://github.com/fennec-language/fennec/blob/main/docs/decisions.md).

- Whether kits can define their own annotations.
- Whether independent writes, not just reads, should overlap automatically.
- How a type marks that its finishing method has to be called.
- How replay records the order tasks reach a `shared` value or a channel.
- The exact syntax for property tests, and for units of measure.
- The file extension (currently `.fen`).
- Whether files of a kit may reuse a top-level name, reached through `import` by their module's name.
- SQLite: whether and how Fennec includes it.
