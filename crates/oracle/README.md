# fennec-oracle

A reference evaluator for Fennec, used as a test oracle. It runs a Fennec program a second,
independent way: it walks the program's syntax tree with plain Rust values, and never touches the
compiler's IR, LLVM or the runtime. When it and the compiled program print different things or
end differently, one of them has a bug.

## A test oracle only

It's never a dependency of `fennec` or of any crate that ships, and never a way to run a user's
program. Fennec has no interpreter, on purpose. A test in this crate fails if any crate but
`fennec-fuzz` and `fennec-bench` takes it as a normal dependency; a dev-dependency is fine anywhere.

## What it shares with the compiler

It depends only on `fennec-syntax` and `fennec-check`. It uses the compiler's parser to read a
program and its checker to accept it and to type its expressions, where the source alone can't
say what a value is, as for a leading-dot case like `.Circle`. So a bug in the parser or the
checker can hide from it: both sides would see the same wrong tree. Everything after the checker,
how values are held, copied, compared and printed, is modeled here on its own, from
`docs/language.md` and the examples.

## How it models a program

- Values are plain Rust values: `i64` ints checked on every operation, `f64` floats, `String`,
  `Vec` for lists, sets and dicts (kept in the order their items went in), and structs, errors and
  enum cases as their named fields.
- A function the program declares is equal only to itself.
- A `Table` is a list of slots, each with the generation of its item and the item while it's
  there, and a tag of its own, which a copy keeps. An item goes in the lowest slot that's free, at
  that slot's next generation, and an `Id` is its slot, generation and tag, so one whose item is
  gone or that's from another `Table` finds nothing. It prints and is gone through in slot order,
  and two are equal when their slots are. `add`, `remove`, `get`, `update`, `modify`,
  `remove_where`, `clear`, `values`, `ids`, `items`, `in`, `len`, `[]` and `for ref` are modeled.
- A kit's modules, and the modules beside a single file that it imports, are found the way the
  compiler finds them and merged into one module, as the compiler merges them: `module.name`
  becomes `name`, a name imported with `as` becomes the name it stands for, and a leading-dot case
  that only one enum the file can see has is given that enum. The compiler's loader is in
  `fennec-kit`, which also fetches and builds other kits, so this is written here on its own. A kit
  that depends on another, a file whose own names hide a name an import binds, and `embed_file`
  are unsupported.
- Every assignment, argument and return copies, so value semantics hold by construction. A `ref`
  parameter, a `for ref` loop's name and a local `ref` stand for a place, written where it is.
- A failure passes up as a Rust error, `except` handles only the failure of the call it follows,
  and `else` the missing value at the end of a chain.
- A generator runs on a thread of its own, and it and its reader take turns, so a run is the same
  every time. A `with` source runs the caller's block at its `lend`.
- `std.json`'s `dumps` and `loads[T]` are written here on their own, by the type the checker gave
  the value, never by running the standard library's reader and writer. A struct is an object of
  the fields it saves, under the names `@serialize(name=...)` and `names=` give them, and `@skip`
  leaves a field out. A case is its name, or an object holding its fields under its name. A tuple
  is an array, and a dict is an object when its first key is text or an int, and otherwise an
  array of `[key, value]` pairs. A `Table` is a list of its items and an `Id` is where its item is
  in it, so a load makes a new `Table`, with a tag of its own. `indent=` and `extra=` are modeled
  too. Text is checked as JSON first, as `std.json` checks it, and a load that fails says what
  `std.json` says: what's wrong with text that isn't JSON, by line and column, and otherwise where
  in the value it doesn't fit, and how. A failure whose message the oracle can't be sure of is
  refused, as `json.loads failing (...)`. A float JSON can't hold, or an `Id` that can't be saved,
  stops the program. A union, bytes, the standard library's collections, a type with its own
  `serialize` or `deserialize`, and `json.dump` and `json.load` are unsupported.
- Standard output is real. `Args` and `Stdin` are empty. A program that takes any other capability
  is skipped.
- Anything it doesn't model it refuses, as `unsupported: <feature>`, rather than guess. A run has a
  budget of steps, items and nesting, and one that goes past it is unsupported too, as `ran too
  long`.

## Using it

```text
fennec-oracle run <program.fen>                      run it the oracle's way
fennec-oracle compare <program.fen> [--fennec PATH]  hold the compiled program, debug and release, to it
fennec-oracle coverage [examples dir]                run it over the examples, and say what it covers
```

`run` exits as the program does: its own code, or 1 when it fails or stops. `compare` exits 0 when
the compiled program and the oracle agree on standard output and exit code, and 1 when they don't.
Both exit 3 when the program uses something the oracle doesn't model. Run every one of them under
a time and memory limit, as any Fennec program is run.
