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
- A `shared` value is one cell, which every copy of it reaches. A `lock` copies what each cell
  holds into the name it gives it, runs its block, and as the block ends writes back what each
  `ref` name holds, unless a failure or a crash left it, which undoes the block's changes, while
  `return`, `break` and `continue` keep them. Nothing else reaches a cell a `lock` holds, as a
  `lock` inside another is a compile error, so that's exact; one that's reached anyway is refused,
  as are a `yield` inside a `lock`, one `lock` taking a value twice, and printing a `shared` value,
  which waits for its fix to print what it holds. `==` on two `shared` values compares what they
  hold, as decided on 2026-11-14, and a cell is equal to itself; one a `lock` holds is refused.
- The model is one thread, and what the compiled program runs side by side is run one after
  another here only where every order it could take gives the same. `parallel(xs)` over a list or
  a `Table` runs its rounds in the list's order: a comprehension's results are in that order, as
  promised, and the failure passed up is the first round's that fails, by position, as the
  runtime keeps the lowest range's. Rounds after one that fails or breaks out may run or not, so
  they run here too, and are refused if they'd change a `shared` value or their item, or fail
  after a `break`. A stream, `(f(x) for x in parallel(xs))`, works out each item as its reader
  asks, and an item's failure is the reader's when it gets there. Refused, as their order isn't
  promised: printing or the file system inside a round, a `shared` value one round changes and
  another reaches, any `lock` in a stream's item, and a generator made outside a round, read in
  it. A float `sum` over `parallel` adds in a tree of the runtime's own, and is refused; an int
  `sum` is modeled where no way of splitting it can overflow. `parallel` anywhere else, over
  anything else, and set and dict comprehensions over it are refused.
- `with launch(task)? as sends:` runs its block first, keeping what it sends in order, and then
  its task, which reads it and then finds its inbox closed. That's exact only where neither sees
  the other run, so printing, the file system and a `shared` value one of them changes are refused
  where both reach them, and so is a generator made before the task, read in it. Messages go only
  from the block to the task: `channel[T]` is unsupported. The `with` ends as the task does, with
  its failure, or `Crashed(why="the task crashed")` for a crash. Refused: a block that fails, which
  cuts its task short at once, a task that ends with messages left unread, which closes its inbox
  so that a send after that fails with `Closed` and one before it doesn't, and `stop_after=`.
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
- Standard output is real. `Args` and `Stdin` are empty. `Fs` is modeled, as below. A program that
  takes any other capability is skipped.
- The file system an `Fs` reaches is modeled in memory, in `interp/disk.rs`. It starts as the
  folder the program runs in was: for `run` and `coverage`, the folder the oracle runs in, as the
  gate runs the examples from the checkout's root, and for `compare`, a fresh empty one, as the
  compiled program gets. What the program writes, makes and removes is held in memory, and the disk
  is never written. Where the folder isn't a fresh empty one, the oracle looks at the disk, a name
  at a time and never through a link, only for whether a file or a folder is there: reading a file
  that was there before, its size, and what's in a folder that was there before are refused. A
  path outside the folder is refused, but for the folders on the way to it, which are there.
- Each of `Fs`'s methods fails as the runtime's does, with its case of `IoError` and its words, and
  a path goes through the folders on its way as the system goes, a `..` back up. An `Fs` narrowed
  with `only` reaches what's inside its folder, by the path's text with `.` and `..` worked out, and
  one narrowed with `read_only` changes nothing: either fails with `PermissionDenied`, saying why.
  A file `fs.create` lends takes its path's place when its `with` block ends, and one that fails
  leaves the path as it was; one `fs.open` lends reads what the file held when it was opened.
- A folder's size, which differs from one system to another, is a value the oracle can't know: it
  can be held and passed on, and anything else done with it is refused. Refused too: a name beside
  another that differs from it only in case, which macOS and Windows take as the same name, a
  folder's listing while a file is being made in it, under a hidden name holding the process's
  number, bytes, and on Windows what it reads its own way.
- `std.path`'s `Path` is its text, and its methods only look at it, each written here on its own.
- Anything it doesn't model it refuses, as `unsupported: <feature>`, rather than guess. A run has a
  budget of steps, items and nesting, and one that goes past it is unsupported too, as `ran too
  long`.

## Using it

```text
fennec-oracle run <program.fen>                      run it the oracle's way
fennec-oracle compare <program.fen> [--fennec PATH]  hold the compiled program, debug and release, to it
fennec-oracle coverage [examples dir]                run it over the examples, and say what it covers
```

`run` exits as the program does: its own code, or 1 when it fails or stops. `compare` runs each
compiled build in a fresh empty folder, made at one path in the temporary folder, so what it writes
goes nowhere else, and the oracle as if in an empty folder at that path. It exits 0 when the
compiled program and the oracle agree on standard output and exit code, and 1 when they don't.
Both exit 3 when the program uses something the oracle doesn't model. Run every one of them under
a time and memory limit, as any Fennec program is run.
