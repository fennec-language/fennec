# Design decisions

Decisions made while designing Fennec, with the reason for each. The [roadmap](../site/src/pages/roadmap.md) says when each one gets built. Newest decisions go at the top of each section.

## Values and memory

**Python's sharing is called out.** When code changes `b` after `b = a` and then reads `a`, a lint notes that `a` didn't change, because values are copies.
*Why:* Fennec looks like Python, and Python shares here. People and AI trained on Python will expect `a` to change too.

**Parameters are borrowed by default.** A parameter that's only read costs no reference-count change, on the way in or out. Reference counts are inserted precisely and dropped at each value's last use. A value with a single owner is reused in place to build its replacement. This is the Perceus approach from Koka, with Lean 4's borrowed parameters.
*Why:* Reference-count traffic around calls is most of the overhead in naive implementations. These two remove nearly all of it.

**Indexing gives a place, not a copy.** `enemies[i].health -= 1` writes straight into the list. A user-defined `def []` gets a `mut` form that does the same.
*Why:* Otherwise each change through an index copies the element out and back, or copies the whole list if it's shared.

**A value is moved, not shared, when it isn't used again.** The compiler finds each value's last use and moves it there.
*Why:* Then nearly every collection has one owner, so copy-on-write's check before each write passes and nothing is copied. The check is a predictable branch the compiler can hoist out of loops, and deletes entirely when ownership proves the value has one owner.

**Views can't escape.** A `view[T]` (a slice of a list) can be a parameter or a local, but can't be stored in a struct or returned. A function that returns a slice returns a shared slice: one reference-count bump, no copy, the way string slices already work.
*Why:* Slicing without copies, and without lifetime annotations.

**Copies are visible.** A lint lists every place the compiler had to copy a value, and why: "copied `enemies` because it's used again on line 42".
*Why:* A hidden copy in a game loop is a frame-time spike nobody can find.

**Handles are `Id[T]`.** An `Id[Enemy]` is an index plus a generation, made by an `IdMap[Enemy]`. It can't index a map of another type. When its enemy is removed, looking it up fails with `TargetGone` rather than finding whatever reuses the slot. `type EnemyId = Id[Enemy]` is an optional alias, not something the compiler makes for each struct.
*Why:* With no references, handles are how one value points at another. Game engines, compilers, graph libraries and servers use the same generational-handle pattern. Handles are two ints, so they're cheap to copy and can be saved or sent anywhere.

## Collections

**Collections are values.** Assigning or passing a `list`, `dict`, `set` or `str` gives an independent copy, like every other value.

**They're a small header over a shared buffer.** The header holds a pointer, a length and a capacity, 24 bytes, and lives inline wherever the collection is stored. The elements sit together in one reference-counted block on the heap. Copying bumps the count. The buffer is copied only on the first write while it's shared, which last-use moves make rare. Elements are stored inline too, so a `list[Vec3]` is one contiguous run of floats.

**Fixed-size arrays live entirely inline.** `array[float, 16]` has no heap block and no reference count. It's for math types like `Mat4` and small buffers inside structs.

**Short strings live in the header.** A `str` of up to 23 bytes is stored in its 24-byte header with no allocation.
*Why:* Most names, keys and identifiers fit.

## Layout

**Structs inside structs are stored inline.** There are no hidden pointers or boxes. `player.transform.pos.x += 1.0` is one write to memory.

**Fields always have names.** There's no embedding that quietly gives the outer struct an inner struct's methods.
*Why:* What a value can do is always visible where its type is declared.

**Recursive types need no annotation.** For `next: Node?`, the compiler adds the indirection itself.

**The compiler chooses the layout.** It can reorder fields, pack enums, and in a hot loop store a list of structs as one array per field. None of this can change what a program does, because nothing can observe where a value sits in memory. The debugger and `{x=}` always show the struct as written.

**Structs in `extern` modules use C's layout exactly.**
*Why:* C interop must be predictable.

## Allocation

**The runtime owns allocation.** The default allocator is mimalloc, vendored (it's MIT-licensed) and later ported to Fennec. It keeps per-thread lists of free blocks for each size, so allocating and freeing take a few instructions.

**The compiler places short-lived values itself.** A temporary that never leaves its function or loop body goes on the stack or in a bump region, with no call to the allocator. This is limited to values that provably don't escape. Full automatic region inference is known to leak memory.

**Arenas are a scope.** Everything allocated inside `with arena():` is freed at once when the block ends. There are no allocator parameters.
*Why:* Freeing in bulk is what makes the JVM's allocator fast, and this gets it without a GC. Passing allocators to every function, as Zig does, is a lot of AI pain for little gain.

**`with noalloc():` is checked at compile time.** Allocating anywhere inside the block, including in functions it calls, is a compile error. A lint suggests it where profiling shows heavy allocation.

**Debug builds report allocation.** They show allocations and bytes per frame, find leaks, and can enforce a budget ("this frame allocates under 1 MB").

## Determinism

**Dicts and sets keep insertion order.**
*Why:* The same program iterates the same way on every run and every machine. Python already does this, so it costs AI nothing.

**Evaluation is always left to right.**

**Floats give the same results on every platform.** There's no fast-math and no automatic fused multiply-add, and `sin`, `cos` and the rest are in the stdlib rather than the platform's math library. `with fast_math():` opts out for one block.
*Why:* It makes lockstep multiplayer and replays possible. It costs a little, and only in math-heavy code, which can opt out.

**Parallel reductions combine in a fixed order.** How `parallel sum(...)` splits and combines its work depends on the data's size, never the thread count.
*Why:* Float results don't drift between runs or machines.

**`Clock` and `Random` are capabilities,** passed in like `Stdout` and `Fs`.
*Why:* Pass a fixed clock and a seeded `Random` and a game or test runs the same way every time.

**Any run can be recorded and replayed exactly.** Every source of nondeterminism comes in through a capability: time, randomness, files, the network and input. So the runtime can record exactly what came through and replay the run bit for bit, in the debugger, with `fennec replay`.
*Why:* A player's crash report becomes a replay file you can step through. The determinism rules above make it almost free.

## Speed

**The target is enforced.** Every program in the benchmark suite also exists as idiomatic code in the fastest established systems language. CI fails if a benchmark runs more than 3% slower than that twin, or slower than the previous run. The fix for a failure goes into the compiler, never into tricks in the benchmark.

### Matching the fastest languages

**Proven single owners cost nothing.** When ownership proves a value has one owner, its reference counting and copy-on-write checks are deleted entirely, and the machine code is what a language with manual memory would produce. Most hot code is like this.

**Range analysis removes provably safe checks.** Overflow checks on loop counters, indices and bounded math, and bounds checks on indexing that can't go out of range, are removed. That brings checked overflow's cost close to zero.

### Going further

**Release builds optimize the whole program, guided by profiles.** Link-time optimization is on by default, and so is profile-guided optimization with `fennec build --release --profile`. Kits build from source and every call is static, so the optimizer sees everything.
*Why:* Together they're often worth 10–20%, and most projects in other languages never turn them on.

**Layout is free to change.** As described under Layout, the compiler can reorder fields, pack enums and split lists of structs into one array per field, because no reference can depend on where a value lives. Languages with references can't change layouts behind the programmer's back.

**Functions without capabilities have no side effects.** A function that isn't passed a capability can't do I/O, so the compiler can merge repeated calls, move them out of loops and vectorize loops more aggressively.
*Why:* In most languages the compiler has to assume any call might do anything.

**There's no unwinding.** Errors are values, and a runtime failure such as overflow ends the program. With no unwinding machinery, code is smaller and inlines better.

**Math types are 32-bit.** `Vec3` and the other math types use 32-bit floats, and `float` stays 64-bit.
*Why:* Twice the SIMD throughput for game math, while `float` stays accurate everywhere else.

**The compiler tells LLVM that names don't alias.** Exclusive `mut` access proves no two names point at the same memory, so every function gets that guarantee for free. Fortran's speed comes from the same guarantee.

**Integer overflow is checked in release builds too.** Wrapping is opt-in: `x.wrapping_add(y)`.
*Why:* A program that compiles should do what it says. The check costs about 1–2%, and the benchmark suite measures it.

## Functions and types

**Builtins work on anything; methods belong to a type.** Builtins work on any value that fits an interface and never change it: `len`, `sum`, `min`, `max`, `any`, `all`, `sorted`, `enumerate`, `zip`. Methods are specific to a type or change the value: `xs.append(x)`, `d.keys()`, `s.split(",")`, `mut xs.sort()`. A type supports a builtin by having the method, such as `def len(self)`. The language server offers the builtins that apply after `xs.`.
*Why:* Builtins read well with comprehensions (`sum(e.health for e in enemies)`), and Python users and AI already write them. Python mostly follows this split; Fennec always does.

**Units of measure are types.** Meters, pixels, seconds and radians can't be mixed by accident, and multiplying or dividing them gives the right unit. They're erased at compile time, so they cost nothing at run time. The syntax is still open.
*Why:* Game code mixes up degrees and radians, and world and screen space, constantly. F# has had units since 2009, and no mainstream language has followed.

**An enum fits an interface when every case does.** The compiler writes the `match`, so `total(targets)` works on a list of a `Target` enum whose cases are all `Damageable`.
*Why:* Lists of mixed types stay static, since the `match` compiles to a jump table, with nothing extra to write.

**An interface in a parameter's type means "any type that fits", chosen at compile time.** `def total(xs: list[Damageable])` takes a list of any one type that has the interface's methods. The compiler makes a copy of the function for each type, so calls are static. A list that mixes types is an `enum`.
*Why:* Errors point at the call ("`Rock` isn't `Damageable`: it has no `damage` method"), not deep inside the function; the function is checked once against the interface; and there's no dynamic dispatch.

**Closures capture by value.** Only a closure that doesn't escape, one passed straight into a call, can capture a `mut` name, and it changes the original. This replaces the earlier rule that closures can't change captured names at all.
*Why:* Capturing by value costs one reference-count bump. Capturing a `mut` in a closure that doesn't escape costs a pointer and needs no lifetimes.

## Visibility

**`pub` marks what other kits can use.** Inside a kit, every file and module sees everything. `pub` goes on each `def`, `struct`, `enum`, `error`, `interface`, `type` alias and re-exported `import` that other kits may use, and on each struct field they may see.
- If every field of a struct is `pub`, other kits get its automatic constructor. Otherwise they build it through its `pub` named constructors.
- Enum cases and interface methods come with their type and have no `pub` of their own.
- Writes from another kit still run the field's `where` rules.
- A `pub` signature can't mention anything that isn't `pub`.
- `pub` functions declare their return and error types, which the semver check compares. Everything else is inferred.
- A lint flags `pub` items that no other kit can reach.

A big app that wants walls between its own parts splits into local kits in one workspace.
*Why:* `pub` appears only where API stability matters, and never in apps. Making every field of a `pub struct` public would make every internal field part of the API, so renaming one would need a major version. Go's capitalization rule would clash with lowercase built-ins and capitalized user types.

## Concurrency

**Tasks are scoped.** Every task ends before the `with` block or `parallel(...)` call that started it. There are no detached threads.
*Why:* Because the parent outlives its tasks, a task can read the parent's values through views without touching reference counts, so reference counts stay non-atomic and fast. There's no `Send` or `Sync` to write.

**Tasks change only separate pieces.** `parallel for mut e in enemies:` gives each task `mut` access to its own elements, checked by the same rule that makes `mut` exclusive. Results combine through reductions, like `parallel sum(...)`.

**Only values with a single owner cross a channel.** They're moved, never shared.
*Why:* That's what lets reference counts stay non-atomic even with long-lived worker tasks.

## Syntax

**Doc comments are `##`, above what they document.** A `##` comment documents the function, struct, field or enum case that follows it, or the module when it's at the top of the file. Examples go in Markdown code fences and check results with `assert`. `fennec test` runs them. There are no `"""` docstrings: strings are never documentation.
*Why:* One form that works on everything, including fields and enum cases. Fences render properly in `fennec doc`. Nim uses `##` for doc comments too.

**f-strings have `{x=}` and no `!r`.** `f"{hp=}"` gives `hp=100`. Format specs like `{x:.2f}` stay.
*Why:* `{x=}` is what people want when debugging; `!r` has no meaning without Python's `repr`.

## Tooling

Every tool ships in the `fennec` binary and has no configuration unless a setting is unavoidable.

- `fennec fmt`: one style, no options.
- `fennec lint`: the glave-strict rules on by default. `fennec fix` applies every lint's automatic fix.
- `fennec check`: types, ownership and lints, without running anything.
- `fennec test`: tests, property tests and the examples in doc comments.
- `fennec doc`: documentation from `##` comments.
- `fennec explain`: the long explanation of any error or lint.
- `fennec search`: finds functions by type, locally and on the Den. `fennec search "Path -> str"` lists every function with that shape.
- `fennec diff`: a semantic diff. It reports renames with their updated call sites, moved functions, changed bodies and API changes with the version bump they need, instead of lines added and removed. A GitHub Action posts it on every pull request.
- `fennec replay`: replays a recorded run, as described under Determinism.
- The language server: inlay hints for inferred types and error sets, hover docs, go to definition, rename, and lint fixes as code actions (including "make this `pub`"). It reuses the compiler's queries and cache.
- Cost hints: the language server shows each line's copies, allocations and reference-count changes inline, the way it shows inferred types.
- The copies lint, as above.

*Why:* Inferred types and error sets need to be visible to be trusted, and a fixed style plus automatic fixes means AI and people write the same code. Cost hints show what code costs while it's written, not after a profiler finds it.

**Every breaking change ships with its own fix.** No language change that breaks code lands unless `fennec fix` rewrites old code to match.
*Why:* Upgrading Fennec should never be a project.

## For AI

**The toolchain ships an MCP server.** Through it, an AI agent can search by type, read docs, run `fennec check` and run snippets in the sandboxed interpreter.
*Why:* AI's most common failure is calling functions that don't exist. Ground truth from the compiler and the Den replaces guessing.

**Diagnostics are structured.** `fennec check --json` gives every error and lint with its location and any fix as an edit a tool can apply.

**Docs come in plain Markdown too.** Every documented item has its own versioned Markdown page, with an `llms.txt` index for each kit and for the language.

**`fennec context <kit>` fits a kit in a prompt.** It prints every `pub` signature with the first line of its doc comment.

**The language reference is executable.** Every example in it is a test, the way `examples/` already is, so the reference can't drift from the language.

## Readable by default

These are default lints, and the Den requires them.

- A `##` doc comment on every `pub` item.
- At least one tested example on every `pub` function.
- Descriptive names. One-letter and abbreviated names are flagged, except a short allowlist: `i`, `j`, `k`, `n`, `x`, `y`, `z`, `dt`, `id`.
- Limits on nesting depth and function length, as lints rather than language rules.

*Why:* Examples are the best documentation for people and AI alike, and they're tests too. Code that reads well is code AI writes well.

## Kits on the Den

**Publishing requires** that the kit is formatted, passes the default lints, passes its tests and doc examples, and meets the rules under Readable by default. `fennec publish` refuses otherwise. Each lint exception needs a written reason, and the reasons are shown on the kit's page.

**Shown, not required:** test coverage, mutation score and benchmarks, as scores on the kit's page.
*Why:* A required coverage percentage gets gamed with tests that check nothing. A visible mutation score exposes that.

**The Den checks for itself.** Publishing goes through OIDC trusted publishing, so the CI run that ran the checks is attested. The Den re-runs formatting and lints in wasm, which is safe and fast because kits have no build scripts.

**Breaking dependents is caught before release.** Before a new version is published, the Den runs the tests of every kit that depends on it against that version and reports which ones break.
*Why:* An author learns "this release breaks 14 kits" before releasing it, not from bug reports.

**The Den can be searched by type,** through `fennec search` and its website.

**Lints are pinned to the kit's Fennec version.** A lint added in Fennec 0.5 never blocks a patch release of a kit written for 0.4.

## Engines

**Development runs use the Cranelift JIT, with no tiering.** Each function is compiled on its first call, so only code that runs gets compiled. There's no bytecode interpreter that promotes hot functions to the JIT.
*Why:* Lazy compilation takes microseconds per function and the cache makes reruns instant, so a second fast engine would buy almost nothing. Tiering means two engines that must always agree, plus falling back from compiled code to the interpreter, which is the hardest part of the runtimes that do it.

**The tree-walking interpreter stays simple and correct, not fast.** It runs the REPL, the playground, compile-time evaluation and the sandbox, and it's the reference the JIT is tested against: every example must give the same output on both.

**One cache, shared per machine, keyed by content.**
- Each compiled function is stored under a hash of its body and the signatures of what it calls. A change inside a function that leaves its signature alone doesn't recompile its callers.
- Entries are written to a temporary file and renamed into place, so readers never see half an entry and the normal path takes no locks. A lock is taken only while old entries are being cleaned up.
- The JIT, release builds and the language server share the cache through the query-based compiler.

## Sandboxing

**Capabilities are the sandbox's policy in every engine.** A script can do only what the capabilities passed to its `main` allow, and sandboxed code can't use `extern` or `unsafe`. A host can show what a script asks for before running it: "this script wants `Net` and read access to `data/`".
*Why:* That makes Fennec a good language for AI agents to write tool code in: typed, with every power declared up front.

**The interpreter runs untrusted scripts, with limits.** It counts fuel on every loop and call, caps memory through its allocator, and limits recursion depth and running time. It runs the playground, the REPL and AI-written scripts, where starting instantly and being easy to sandbox matter more than speed. In the browser it runs as wasm, so the browser sandboxes it again.

**Untrusted code that needs native speed is compiled to WebAssembly and run in Wasmtime.** The development and release backends can both target wasm. Wasmtime already provides fuel, interruption and memory limits.
*Why:* Wasmtime's sandbox is heavily hardened. Making our own JIT a security boundary would mean any compiler bug is an escape.

**The native engines are for trusted code.** The Cranelift JIT and LLVM release builds run your own programs, not untrusted ones.

**On servers, sandboxed code also runs in its own process,** under seccomp on Linux or the App Sandbox on macOS, as an outer wall.

## Writing Fennec in Fennec

The current compiler stays as stage 0 until a Fennec compiler built by Fennec produces identical output when it builds itself again.

The order:

1. The tools: the formatter, the linter and the Den CLI. They can be written while the language is still changing.
2. The front end: lexer, parser and type checker.
3. The release backend, written to emit LLVM's text format (`.ll`) for `llc`. This needs no calls into C.
4. The development backend, calling Cranelift through a small wrapper with a C interface, from an `extern` module.
5. A backend of our own: only if Cranelift stops being enough.

Steps 2 to 5 wait until the core semantics above are frozen and the Fennec compiler can run on the Cranelift JIT, since the interpreter is too slow to run a compiler.
*Why:* Every language change after the compiler is self-hosted has to be made in two compilers.

## Still open

- How a function declares its errors, which `pub` functions must do.
- How closures longer than one line should look, beyond nested functions.
- Whether independent writes, not just reads, should overlap automatically.
- The exact syntax for property tests.
- The syntax for units of measure.
- The file extension (currently `.fen`).
- These security proposals:
  - `Net`, `Env` and `Process` as capabilities.
  - Narrowed capabilities like `fs.only("saves/")`.
  - Each kit's capabilities recorded in `kit.lock`, with any increase needing approval.
  - Types that make injection impossible: `sql"..."` templates, `Path`, argument lists for commands, escaping HTML templates.
  - `Secret[str]`, which can't be printed or logged.
