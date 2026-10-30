# Design decisions

Decisions made while designing Fennec, with the reason for each. The [roadmap](../site/src/pages/roadmap.md) says when each one gets built. Newest decisions go at the top of each section.

## Values and memory

**Indexing gives a place, not a copy.** `enemies[i].health -= 1` writes straight into the list. A user-defined `def []` gets a `mut` form that does the same.
*Why:* Otherwise each change through an index copies the element out and back, or copies the whole list if it's shared.

**A value is moved, not shared, when it isn't used again.** The compiler finds each value's last use and moves it there.
*Why:* Then nearly every collection has one owner, so copy-on-write's check before each write passes and nothing is copied. The check is a predictable branch the compiler can hoist out of loops.

**Views can't escape.** A `view[T]` (a slice of a list) can be a parameter or a local, but can't be stored in a struct or returned. A function that returns a slice returns a shared slice: one reference-count bump, no copy, the way string slices already work.
*Why:* Slicing without copies, and without lifetime annotations.

**Copies are visible.** A lint lists every place the compiler had to copy a value, and why: "copied `enemies` because it's used again on line 42".
*Why:* A hidden copy in a game loop is a frame-time spike nobody can find.

**Handles are `Id[T]`.** An `Id[Enemy]` is an index plus a generation, made by an `IdMap[Enemy]`. It can't index a map of another type. When its enemy is removed, looking it up fails with `TargetGone` rather than finding whatever reuses the slot. `type EnemyId = Id[Enemy]` is an optional alias, not something the compiler makes for each struct.
*Why:* With no references, handles are how one value points at another. This is the generational-handle pattern game engines already use, as one generic type rather than a type generated inside every struct.

## Functions and types

**An interface in a parameter's type means "any type that fits", chosen at compile time.** `def total(xs: list[Damageable])` takes a list of any one type that has the interface's methods. The compiler makes a copy of the function for each type, so calls are static. A list that mixes types is an `enum`.
*Why:* Errors point at the call ("`Rock` isn't `Damageable`: it has no `damage` method"), not deep inside the function; the function is checked once against the interface; and there's no dynamic dispatch.

**Exported functions declare their return and error types.** They're inferred everywhere else.
*Why:* Otherwise editing a private helper could silently change a kit's public API. The semver check compares what's declared.

**Closures capture by value.** Only a closure that doesn't escape, one passed straight into a call, can capture a `mut` name, and it changes the original.
*Why:* Capturing by value costs one reference-count bump. Capturing a `mut` in a closure that doesn't escape costs a pointer and needs no lifetimes.

**Integer overflow is checked in release builds too.** Wrapping is opt-in: `x.wrapping_add(y)`.
*Why:* A program that compiles should do what it says. The check costs about 1–2%. The benchmark suite will confirm that.

## Concurrency

**Tasks are scoped.** Every task ends before the `with` block or `parallel(...)` call that started it. There are no detached threads.
*Why:* Because the parent outlives its tasks, a task can read the parent's values through views without touching reference counts, so reference counts stay non-atomic and fast. There's no `Send` or `Sync` to write.

**Tasks change only separate pieces.** `parallel for mut e in enemies:` gives each task `mut` access to its own elements, checked by the same rule that makes `mut` exclusive. Results combine through reductions, like `parallel sum(...)`.

**Only values with a single owner cross a channel.** They're moved, never shared.
*Why:* That's what lets reference counts stay non-atomic even with long-lived worker tasks.

## Syntax

**Doc comments are `##`.** A `##` comment documents the module, function, struct, field or enum case after it. Code examples in doc comments run as tests. Strings are never documentation, so there are no `"""` docstrings.
*Why:* One form that works on everything, including fields and enum cases. Nim uses the same thing.

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
- The language server: inlay hints for inferred types and error sets, hover docs, go to definition, rename, and lint fixes as code actions. It reuses the compiler's queries and cache.
- The copies lint, as above.

*Why:* Inferred types and error sets need to be visible to be trusted, and a fixed style plus automatic fixes means AI and people write the same code.

## Writing Fennec in Fennec

The compiler in Rust stays as stage 0 until a Fennec compiler built by Fennec produces identical output when it builds itself again.

The order:

1. The tools: the formatter, the linter and the Den CLI. They can be written while the language is still changing.
2. The front end: lexer, parser and type checker.
3. The release backend, written to emit LLVM's text format (`.ll`) for `llc`. This needs no calls into C.
4. The development backend, calling Cranelift through a small Rust wrapper with a C interface, from an `extern` module.
5. A backend of our own: only if Cranelift stops being enough.

Steps 2 to 5 wait until the core semantics above are frozen and the Fennec compiler can run on the Cranelift JIT, since the interpreter is too slow to run a compiler.
*Why:* Every language change after the compiler is self-hosted has to be made in two compilers.

## Still open

- Visibility: everything visible inside a kit with `pub` for what other kits see, or a `_` prefix for private, or both.
- How closures longer than one line should look, beyond nested functions.
- Whether independent writes, not just reads, should overlap automatically.
- The exact syntax for property tests.
- The file extension (currently `.fen`).
