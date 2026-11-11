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
- Enums and `match`, with or-patterns and guards, and `case .Button(ref clicks):` to change a field inside one. Unions, `Circle | Square`, narrowed with `is` and `match`. Interfaces and generics. `extend` for a type's methods in another file.
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
- `with launch(...)`, a task beside you until its block ends, and bounded channels, `Channel`, `Inbox` and `Sender`, natively.
- Sized numbers from `i8` to `u64` and `f32`, natively, each operation checked against its size, with `wrapping_` and `saturating_` add, sub and mul up to 32 bits, with literals taking the size they go into and `u8.from(x)?` and `u8.parse(text)?` for numbers from outside.
- Notes on errors, so a cleanup that fails after a failure hides neither one.
- A function that can fail says `fails` in its signature, a failure passes up only with `?`, and a `match` over what can fail handles both sides. `fennec fix` writes the first two.
- `with` and `lend`, for sources of your own and for files: `fs.create` puts a file in place only when its block ends, synced to the disk, and `fs.open` reads in lines or chunks.
- Capabilities: `Stdout`, `Stdin`, `Args`, `Fs`, `Clock`, `Env`, `Entropy`, `Process` and `Net`.
- Float sums and `math.dot` that add in one fixed pattern, the same bits everywhere. `xs.swap(i, j)`.

**The standard library**

- `collections`: `Table` and `Id`, `Tree`, `Graph`, `SortedMap`, `SortedSet`, `Grid`, `Packed`, `Deque`, `Heap`, `PriorityQueue` and `Counter`.
- `json`, `toml` and `pack`, which save any type whose fields can be saved. `pack` loads data from an earlier version of a type by its names, with new fields taking their defaults. `csv`, `base64` and `compress`.
- `net`: TCP, UDP, an HTTP client with TLS, and an HTTP server.
- `math`, with `Vec2`, `Vec3`, `Vec4`, `Quat`, `Mat3` and `Mat4` for games. `layout`, `path`, `fs`, `random`, `time`, `string`, `re`, `unicode`, `log`, `cli`, `url`, `process`, `crypto` and `uuid`.
- `crypto`'s `Secret`, which can't be printed, logged or saved, and hashes over a file a chunk at a time.
- `concurrent`, for tasks and channels. `sql` and `sqlite`, which take only t-strings, with a test's database a real file of its own. `postgres`, which connects over `std.net`, signs in with SCRAM-SHA-256, and runs queries and transactions over TLS.

[The guide](https://github.com/fennec-language/fennec/blob/main/docs/guide.md) walks through building a real program with all of this. Every example in the repository's `examples/` folder is a test, including its error messages, and the [language reference](/reference/) is generated from them.

## Next up

- **More game math.** 32-bit math types, and `Simd` for when a loop must use vectors.
- **Wrapped C libraries for games.** Windows, input, audio and graphics, wrapped by the Fennec project behind safe APIs. Your own code never needs `unsafe`, because there isn't any.
- **Debug info and stack traces,** so a failure shows the calls that led to it.
- **The rest of the sized numbers:** `wrapping_` and `saturating_` on a `u64`.
- **Kit dependencies and [the Den](/den/),** Fennec's registry for kits. Kits from git or a path are already pinned in `kit.lock` by `fennec update`, and importing from them is next.
- **Cross-compiling and WebAssembly.**
- **The rest of the concurrency design:** Database pools, and lock order checked across the whole program. The [tour](/tour/#sharing-resources-and-tasks) describes it.

## Next: speed and builds

- Everything on LLVM, and the JIT and the interpreter are gone. What didn't build natively is at zero, the test fake network is in the runtime, constants have a small evaluator in the compiler, fuzzing checks against pinned outputs and native builds, and the commit before removal has a tag. The JIT is gone, native programs carry no interpreter, and `fennec` runs nothing on one. The interpreter's code is deleted too. The website's playground is removed for now, and returns once the self-hosted compiler can compile in the browser.
- Every kernel at 110% of its hand-written twin. The [benchmarks](/benchmarks/) show where each one stands.
- A shared cache that only trusted builds can write to, checked by a clean rebuild in CI.
- `math.fma(a, b, c)`, rounded once. Float sums and `math.dot` already add in one fixed pattern.
- Release keys, so the launcher's checks verify a real release. The launcher itself is built, running the compiler a kit pins, and release archives are the same bytes however often they're built.

## Next: the rest of the language

Each of these is decided and not built.

- A lint for a copy that can go stale, suggesting `ref`, and a local `ref name = place`.
- No `Ok`, `Err` or `Result`, and an `else as` block that gives a value. Every failure is handled at the call.
- `{x=}` in f-strings.
- `x.approx(y, within=...)`.
- `embed_file` in place of `embed`, and `embed_folder`, a read-only map of a folder's files baked into the program.
- `else as name:`, which binds the failure in that block, and a compile error for `_ = f()` on a call that can fail, with `ignore` removed.
- Units of measure, as structs whose operators are chosen by their right-hand type, with no new syntax.
- Annotations a kit defines for fields, read by generic code.

## After that

- **Hot reload,** so a running game or server picks up a change. It's deferred, and its design is in progress.
- The rest of compile-time loops over a type, and `Serializer` and `Deserializer` for every format built on them. A generic function's loop over a type's fields already works. `where` ranges, delta encoding and quantization in `std.pack`.
- WebSocket in `std.net`.
- GPU kernels written as plain functions.
- `fennec context`, a kit's whole API sized for a prompt, and an MCP server for agents.
- A Fennec compiler written in Fennec. Its lexer, parser and checker are underway.

## Only if the evidence asks

These stay out unless measurements show they're needed: a limited `View[T]` for stored views, tighter lock patterns, and independent writes running at the same time with nothing written to ask for it. `select` on channels, handles that move between tasks, and results returned from tasks wait until real programs need them.

Never in the language: `del`, destructors, moves, lifetimes, `unsafe` and global state.

## Open questions

None right now. Decisions already made, and why, are in [the language page](https://github.com/fennec-language/fennec/blob/main/docs/language.md) and [the design decisions](https://github.com/fennec-language/fennec/blob/main/docs/decisions.md).
