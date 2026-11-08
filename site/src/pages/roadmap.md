---
layout: ../layouts/Doc.astro
title: Roadmap
description: What Fennec does today, what's being built next, and the design questions still open.
---

# Roadmap

<p class="lede">Fennec's design is settled unless real programs or benchmarks say otherwise. Now it's a matter of building it, so this page is mostly a list of what's done and what isn't.</p>

## Works today

**Tools**

- `fennec run`, which runs the tests a change affects and then your program, and `fennec build`, which makes a native program. Both build through LLVM, a function at a time, with each function cached by its content. `--release` optimizes the whole program as one.
- `fennec test`, which skips tests that passed and whose code hasn't changed, with property tests and fake capabilities. `fennec bench` for `bench` blocks.
- `fennec new`, `fennec init`, `fennec fmt`, `fennec check`, `fennec lint` and `fennec fix`, with JSON output for agents, and a language server.
- On Linux, `fennec build --release` makes one static program that needs nothing from the machine.
- Kits: a folder with a `kit.toml`, whose files import from each other.

**The language**

- Names and `mut`, functions with written return types, defaults passed by name, nested functions and arrow functions.
- `ref` for loops, parameters and `self`. Compile errors for changing a copy nobody reads, changing a list while looping over it, and a copy made on every round of a loop.
- Structs with defaults, methods, named constructors and `where` rules, checked on every write. `copy(x, field=...)` for a changed copy.
- Fixed-size arrays, `array[T, N]`, held inline.
- Enums and `match`, with or-patterns and guards. Unions, `Circle | Square`, narrowed with `is` and `match`. Interfaces and generics. `extend` for a type's methods in another file.
- Failures with `error`, `fail`, `?`, `else` and `match`, `fails Name` in signatures, and `fails A | B`. An `else:` block must leave. Optional values with `None` and `else`.
- Lists, dicts, sets, tuples, `bytes`, comprehensions and slices. Structs work as dict keys with nothing to write.
- Lazy generators, used once.
- Errors for positional calls that are easy to get backwards.
- `export` on top-level things, and `_name` hiding a field or method from everything but its type.
- Durations like `30s` and `200ms`, raw strings, constants worked out at compile time, and `embed`.
- `@hot`, which proves a function allocates nothing, copies nothing and keeps no checks it can't prove away, or doesn't compile.
- One equality, order and hash for floats, with NaN sorted last, and the same bits on every platform.
- A compile error for ignoring a returned value, with `_ = call()` to mean it. `xs.get(i)` for an index that might not be there. `main` returning an exit code.
- t-strings, `t"..."`, which keep the text apart from the values, and `LiteralStr`, text written in the source.
- `parallel` over collections, streams and a server's connections, on every core, with reductions that combine in a fixed order. Each server round closes its connection when it ends, a failure passing up stops the server, and a crash ends only its own round.
- `shared T` and `lock`, with a crash inside `lock x as ref` rolled back so nobody sees a half-change.
- `with` and `lend`, for sources of your own and for files: `fs.create` puts a file in place only when its block ends, synced to the disk, and `fs.open` reads in lines or chunks.
- Capabilities: `Stdout`, `Stdin`, `Args`, `Fs`, `Clock`, `Env`, `Entropy`, `Process` and `Net`.

**The standard library**

- `collections`: `Table` and `Id`, `Tree`, `Graph`, `SortedMap`, `SortedSet`, `Grid`, `Packed`, `Deque`, `Heap`, `PriorityQueue` and `Counter`.
- `json`, `toml` and `pack`, which save any type whose fields can be saved. `csv`, `base64` and `compress`.
- `net`: TCP, UDP, an HTTP client with TLS, and an HTTP server.
- `math`, with `Vec2`, `Vec3`, `Vec4`, `Quat`, `Mat3` and `Mat4` for games. `layout`, `path`, `fs`, `random`, `time`, `string`, `re`, `unicode`, `log`, `cli`, `url`, `process`, `crypto` and `uuid`.

[The guide](https://github.com/fennec-language/fennec/blob/main/docs/guide.md) walks through building a real program with all of this. Every example in the repository's `examples/` folder is a test, including its error messages, and the [language reference](/reference/) is generated from them.

## Next up

- **More game math.** 32-bit math types, and `Simd` for when a loop must use vectors.
- **Wrapped C libraries for games.** Windows, input, audio and graphics, wrapped by the Fennec project behind safe APIs. Your own code never needs `unsafe`, because there isn't any.
- **Hot reload,** so a running game picks up a change.
- **Debug info and stack traces,** so a failure shows the calls that led to it.
- **Sized number types,** like `i32`, `u8` and `f32`.
- **Kit dependencies and [the Den](/den/),** Fennec's registry for kits.
- **Cross-compiling and WebAssembly.**
- **The rest of the concurrency design:** `launch` and channels, keeper pools, lock order checked across the whole program, and no waiting inside a lock. The [tour](/tour/#sharing-resources-and-tasks) describes it.
- **`std.sqlite`,** which takes t-strings, so SQL injection can't be written.
- **Hashing a file in chunks,** with `blake3(file.chunks(n))`.

## Next: speed and builds

- Everything on LLVM, so the JIT and the interpreter can go. Before they do, what doesn't build natively goes to zero, the test fake network moves into the runtime, constants get a small evaluator in the compiler, fuzzing checks against pinned outputs and the native twins, and the playground compiles to WebAssembly on a server. The commit before removal gets a tag.
- Every kernel at 110% of its hand-written twin. The [benchmarks](/benchmarks/) show where each one stands.
- A shared cache that only trusted builds can write to, checked by a clean rebuild in CI.
- A tiny launcher that fetches the compiler a kit asks for. Pinned compilers never change by themselves.

## Next: the rest of the language

Each of these is decided and not built.

- A compile error for a copy that can go stale, which `copy(x)` makes on purpose.
- Narrowing with `if x is Err(...)`, and a compile error for checking an error and then ignoring it.
- `case .Button(ref clicks):` to change a field inside an enum.
- `{x=}` in f-strings.
- `x.approx(y, within=...)`.
- Units of measure.

## After that

- Compile-time loops over a type, and `Writer` and `Reader` for every format built on them. `where` ranges, delta encoding and quantization in `std.pack`.
- WebSocket in `std.net`.
- GPU kernels written as plain functions.
- `fennec context`, a kit's whole API sized for a prompt, and an MCP server for agents.
- A Fennec compiler written in Fennec. Its lexer, parser and checker are underway.

## Only if the evidence asks

These stay out unless measurements show they're needed: a limited `View[T]` for stored views, and tighter lock patterns. `select` on channels, handles that move between tasks, and results returned from tasks wait until real programs need them.

Never in the language: `del`, destructors, moves, lifetimes, `unsafe` and global state.

## Open questions

These are still being decided. Decisions already made, and why, are in [the language page](https://github.com/fennec-language/fennec/blob/main/docs/language.md) and [the design decisions](https://github.com/fennec-language/fennec/blob/main/docs/decisions.md).

- Whether kits can define their own annotations.
- How `std.pack` data changes from one version of a program to the next.
- How an `Id` is used across servers.
- The syntax for units of measure.
- The file extension (currently `.fen`).
- `Secret[str]`, which can't be printed or logged.
