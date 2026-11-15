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

Of Fennec's crates, it depends only on `fennec-syntax` and `fennec-check`, and besides them only
on `libm`, below. It uses the compiler's parser to read a
program and its checker to accept it and to type its expressions, where the source alone can't
say what a value is, as for a leading-dot case like `.Circle`. So a bug in the parser or the
checker can hide from it: both sides would see the same wrong tree. Everything after the checker,
how values are held, copied, compared and printed, is modeled here on its own, from
`docs/language.md` and the examples.

Where a standard library module is written in Fennec, the oracle can run its own source, as
`fennec-syntax` gives it to the compiler, and model only the functions the runtime writes in Rust
beneath it. These are run this way (`RUN_FROM_SOURCE` in `src/load.rs`): `std.math`,
`std.collections`, `std.time`, `std.string`, `std.env`, `std.base64`, `std.random`, `std.uuid`,
`std.cli`, `std.crypto`, `std.csv`, `std.html`, `std.log`, `std.url`, `std.concurrent`,
`std.postgres`, `std.re`, `std.template`, `std.sql` and `std.io`. Each is loaded when the program
imports from it, when a module loaded so imports from it, or, for `std.env` and `std.time`, when
`main` takes the `Env` or the `Clock` whose types they declare. In `std.math`, the vectors,
quaternions and matrices, `dot` and `lerp` are its Fennec code, and `sqrt`, `floor` and `ceil` are
modeled here. In `std.collections`, `Deque`, `Heap`, `Counter`, `SortedMap`, `SortedSet`, `Grid`,
`Packed`, `Tree` and the rest are its Fennec code. Its `extend`s of `Table` and `Id`, which the
oracle models on its own, are left out. `std.time`'s `Duration`, `Instant` and `Time` are its Fennec
code, and a duration written out, as `30s`, is its `Duration`. The intrinsics their code calls that
only read, like `text_byte`, `text_size`, `text_part` and `bytes_text`, are modeled here
(`interp/library.rs`) from what `intrinsics.fen` says each does, and so is making a core error, like
`InvalidValue`, by name. `std.random`'s draws, beneath its `Random`, are modeled here as
`docs/language.md` pins them: SplitMix64, and a range by Lemire's method. The types of the modules'
expressions are what the checker gave them as it checked the program. So the oracle still checks how
the compiler and runtime run that code, but a mistake in the module's own Fennec would be the same
on both sides. A module modeled on its own, like `std.json`, isn't run from source. `sin`, `cos`,
`tan`, `acos`, `exp`, `log` and a float's `**` are computed with the `libm` crate, as
`docs/language.md` says they are, at the version the workspace pins.

## How it models a program

- Values are plain Rust values: `i64` ints checked on every operation, `f64` floats, `String`,
  `Vec<u8>` for `bytes`, `Vec` for lists, sets and dicts (kept in the order their items went in),
  and structs, errors and enum cases as their named fields. `bytes` prints as `bytes([104, 105])`,
  as a byte is an int; `bytes(items)` stops on an item past 0 to 255, `decode` fails with the words
  the runtime gives, which are Rust's for bytes that aren't UTF-8, and `hex` and `fromhex` read and
  write two hex digits a byte.
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
- Standard output is real. `Args` and `Stdin` are empty. `Fs` is modeled, as below. Standard
  error is written as the program writes it, and never compared. `Env` reads the variables the
  oracle was started with, as the compiled program reads its own. No signal comes during a run, so
  `Signals` never says to stop and waiting on one is refused. A `Clock` reads the system's clocks,
  and its `sleep` moves them on without waiting, so a program sees the time it slept pass, and its
  output can't depend on more than what comes before what. An `Entropy`'s bytes and seeds differ
  every run, as the compiled program's do, so a program's output can't depend on them. `main` may
  take `Net`, and any use of it is refused, so what a program prints without reaching the network,
  as with no arguments, is still checked. A program that takes any other capability is skipped.
- The file system an `Fs` reaches is modeled in memory, in `interp/disk.rs`. It starts as the
  folder the program runs in was: for `run` and `coverage`, the folder the oracle runs in, as the
  gate runs the examples from the checkout's root, and for `compare`, a fresh empty one, as the
  compiled program gets. What the program writes, makes and removes is held in memory, and the disk
  is never written. Where the folder isn't a fresh empty one, the oracle looks at the disk, a name
  at a time and never through a link, only for whether a file or a folder is there: reading a file
  that was there before, its size, and what's in a folder that was there before are refused.
  Outside the folder the disk is looked at the same way, and only that way, so the compiled
  program on the same machine sees the same: the root is there, a path through nothing fails as
  not found, and reading, listing or changing anything that's there is refused.
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
- Places through a type's own `def []` are as `docs/decisions.md` sets them out
  (`interp/own_index.rs`). Through the `ref` form, which the parser names `[]mut`, an int position
  is a place inside the value: indexing, `=`, a `ref` argument, `for ref` and `parallel` reach it
  where it is, and `for ref` and `parallel` skip the positions its `is_live` says are empty.
  Through the plain form beside a `def []=`, a change reads the part with `[]`, changes that copy,
  and writes it back with `[]=` once the change is done: once for `shelf[0].x += 1`, nested too,
  and once for a call that takes it as `ref`, even one that changes it twice. A key that isn't an
  int goes to the plain form, so a type can have both, as `Grid` does. A failure partway through
  such a change is refused, since whether what changed before it is written back isn't promised,
  and so is a `with` source given a `ref` through `[]=`.
- `value? from Context(...)` passes a failure up as the context, made only then. Native code
  keeps what caused it with it, but nothing reads that yet.
- An `array[T, N]` is held as the list it prints as, and `array[T, N]()` is `N` of `T`'s zero, as
  the checker types it.
- `std.crypto`'s `equal` compares two texts or two runs of bytes; its time, which depends only on
  their lengths, isn't something a program can see. The intrinsics that write into a byte builder
  through a `ref`, `bytes_push` and the rest, are modeled as `intrinsics.fen` says they work, and
  `std.io`, run from its source, leaves out its `Stdout`, `Stdin` and `Stderr`, which the oracle
  models as capabilities.
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
