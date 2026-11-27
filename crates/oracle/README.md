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
on `libm` and `rusqlite`, below. It uses the compiler's parser to read a
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
`std.postgres`, `std.re`, `std.template`, `std.sql`, `std.sqlite`, `std.io`, `std.toml`, `std.json`, `std.net`
and `std.pack`. What `std.pack` writes beside a value, a type's shape, its FNV-1a hash and its layout,
is worked out in `interp/packing.rs` from `docs/pack.md`, not from the compiler's code, and held to
the doc's test vectors, a type that writes its own `serialize` shaped by what it saves. A kit with a
`pack.lock` is refused, as the oracle's lock is empty.
`std.net`'s requests, responses and router run as its Fennec code, so a program that answers
requests in-process is checked; a `Net` that reaches the network is still refused. Each is loaded
when the program imports from it, when a module loaded so imports from it, or, for `std.env` and
`std.time`, when `main` takes the `Env` or the `Clock` whose types they declare. In `std.math`, the
vectors, quaternions and matrices, `dot` and `lerp` are its Fennec code, and `sqrt`, `floor` and
`ceil` are modeled here. In `std.collections`, `Deque`, `Heap`, `Counter`, `SortedMap`, `SortedSet`,
`Grid`, `Packed`, `Tree` and the rest are its Fennec code. Its `extend`s of `Table` and `Id`, which
the oracle models on its own, are left out. `std.time`'s `Duration`, `Instant` and `Time` are its
Fennec code, and a duration written out, as `30s`, is its `Duration`. A template string, as
`t"{name}"`, builds the core's own `Template`, whose `Template` and `Interpolation` are run from
their source the same way. So are the core's functions of the sized ints written in Fennec, as
`u64.parse` and `u8.from`. The intrinsics their code calls that only read, like `text_byte`,
`text_size`, `text_part`, `text_escape_at`, `bytes_text`, and `id_tag`, `table_tag` and `id_key` of
a `Table`'s `Id`s, are modeled here (`interp/library.rs`) from what `intrinsics.fen` says each does,
and so is making a core error, like `InvalidValue`, by name. `std.random`'s draws, beneath its
`Random`, are modeled here as `docs/language.md` pins them: SplitMix64, and a range by Lemire's
method. The types of the modules' expressions are what the checker gave them as it checked the
program. So the oracle still checks how the compiler and runtime run that code, but a mistake in the
module's own Fennec would be the same on both sides. `std.json` is run for the helpers other modules
call, like `float_text` and `quoted`, but its `dumps` and `loads` are modeled on their own, as
below. `toml.dumps` and `toml.loads` are the runtime's, written in Rust, and `std.toml`'s own
`native_dumps` and `native_loads`, written in Fennec, which the compiler uses for a type with its
own `def serialize`, are run in their place, so the two are held to each other. `sin`, `cos`, `tan`,
`acos`, `exp`, `log` and a float's `**` are computed with the `libm` crate, as `docs/language.md`
says they are, at the version the workspace pins. A `std.sqlite` database is SQLite's, as its
documentation says, so SQLite answers the queries, through `rusqlite` at the version the workspace
pins: the oracle checks Fennec's binding and rules around SQLite, not SQLite itself, as it
checks how the float functions are called, not `libm`. What the runtime does around it is modeled
in `interp/sqlite.rs` on its own: a query is one statement with a place for each value, a NaN is
refused, each row's cells go into `SqlRows` by kind, each failure is its case of `SqlError`, and a
transaction is kept or undone as its block ends. A query past ordinary ones on the database's own
tables, SQLite's random and time functions, and a database through a narrowed `Fs` are refused.

## How it models a program

- Values are plain Rust values: `i64` ints checked on every operation, `f64` floats, `String`,
  `Vec<u8>` for `bytes`, `Vec` for lists, sets and dicts (kept in the order their items went in),
  and structs, errors and enum cases as their named fields. `bytes` prints as `bytes([104, 105])`,
  as a byte is an int; `bytes(items)` stops on an item past 0 to 255, `decode` fails with the words
  the runtime gives, which are Rust's for bytes that aren't UTF-8, and `hex` and `fromhex` read and
  write two hex digits a byte.
- A function the program declares is equal only to itself.
- A `Buffer` is the list it prints as. Its room is known only as far as `docs/language.md` promises
  it: `capacity()` is at least what it holds, and right after `reserve(n)`, with nothing written
  since, at least `n` more. A comparison that settles, as `capacity() >= len(b)`, answers. The
  number itself, or a comparison past what's promised, is refused, as its answer would hang on how
  the runtime grows a buffer.
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
- What each file imports from the standard library is its own, as the checker has it: a name is
  looked up in the imports of the file it's written in, by `fennec_check::place`, so two files may
  import `pick` from different places, and std's code run from its source has its own imports. A
  name written bare reaches a function or constant by the checker's own rule, `fennec_check::reaches`:
  std's code reaches only std's names, so a program's own `reversed` never stands in for the builtin
  std's `Tree` calls. A program's constant with the name of one a module run from its source
  declares is unsupported, since the tables hold both under one name.
- Every assignment, argument and return copies, so value semantics hold by construction. A `ref`
  parameter, a `for ref` loop's name and a local `ref` stand for a place, written where it is. A
  `case` with `ref` binds a copy of the field it names, put back where the case ends however it
  ends, as the checker writes it in for the compiled program.
- Evaluation is left to right, as `docs/decisions.md` says. What's indexed is read before its
  index, and what a method is called on before its arguments, when it takes its value as a copy
  or is a string's method or one of a dict's or a list's that only looks things up. A method that
  changes its value where it is, a `ref self` one or a list's `append`, reaches it as it is then.
- A failure passes up as a Rust error, `except` handles only the failure of the call it follows,
  and `else` the missing value at the end of a chain.
- A generator runs on a thread of its own, and it and its reader take turns, so a run is the same
  every time. A `with` source runs the caller's block at its `lend`. Its `lend ... except:` cleanup
  runs when the block fails: a failure out of the cleanup is noted on the block's, as it prints,
  when it's a `fail` or a line whose whole value is a `?`, and takes the block's place when it's a
  `?` deeper in a line or on a `with`. An error's notes are read with `notes()` and added with
  `add_note`, and never printed; `==` on errors that carry any is refused. A generator that can
  fail, given a name and let go of partway, leaves at its `yield` where compiled code frees it: as
  its function ends, or as the name is given another. Each `with` it's in runs its cleanup there,
  and a cleanup that fails stops the program. One made in an inner block, or two let go of at
  once, are refused, as is any other generator let go of inside a `with` block.
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
  they run here too, and are refused if they'd change a `shared` value or their item, use a
  channel, or fail after a `break`. Each round is one of its own to the channels it uses, as a
  task is, so which round's message comes first is tried every way, and two rounds taking from
  one channel at once are refused. A round that receives from a channel made outside it, or waits
  for room on one, hands the rounds after it to a thread of their own, given a copy of the names
  they read, as they run beside it: their messages are there before it takes the next, and it can
  wait on them. Refused: that round stopping the call, as the rounds after it ran where they may
  not have; and, of a channel made outside a round, a close, and any use in a stream's item. A
  stream, `(f(x) for x in parallel(xs))`, works out each item as its reader
  asks, and an item's failure is the reader's when it gets there. Refused, as their order isn't
  promised: printing or the file system inside a round, a `shared` value one round changes and
  another reaches, any `lock` in a stream's item, and a generator made outside a round, read in
  it. A float `sum` over `parallel` adds in a tree of the runtime's own, and is refused; an int
  `sum` is modeled where no way of splitting it can overflow. `parallel` anywhere else, over
  anything else, and set and dict comprehensions over it are refused.
- A `with launch(task)?:`, or `with launch((inbox) => ...)? as sends:`, runs its task beside its
  block, talking through `std.concurrent`'s channels, whose intrinsics are modeled here
  (`interp/actors.rs`), and through its inbox, a channel of `inbox_size` messages, 16 unless given,
  which closes when the block ends. Each task runs on a thread of its own, but only one runs at a
  time: it runs until it waits, for a message, for room to send one, or for a task to end, and then
  the next in launch order that can go on runs. Where several tasks send on one channel, which
  message comes next can hang on time: before a receive everything else that can go on runs, vector
  clocks tell which of the messages there could have come first, and each that differs is taken on a
  run of its own (`lib.rs`), up to 256 runs, which all have to print the same and end the same.
  Printing, and reaching the file system, from two tasks has to be in an order their messages,
  launches and ends make: one that could come before or after another task's is refused as hanging
  on time. A channel to every listener gives each listener every message from when it started
  listening, and a plain `receive()` on one stops the program, as `docs/language.md` says. It keeps
  the last `size`: a listener that hadn't read the one let go falls behind, and its next receive
  fails as a crash does, or with `when_behind=.DropOldest` goes on from the oldest kept. The
  `with` ends as the task does, with its failure, or `Crashed(why="the task crashed")` for a crash.
  A listener started beside sends, neither before nor after them, is tried at each place it could
  start among them, as racing senders are. A timed wait that times out is taken to come after
  every other task got as far as it could, its listeners started included: one that exists then
  started before what's sent after.
  Refused: two tasks taking from one channel to one receiver, or from one listener, at once, where
  neither had heard of the other's last take; several senders to a listener whose
  next messages differ; a channel to every listener letting go of a message a listener read, the
  read not before the send, or a listener finding one gone, its receive not after the send that let
  it go; a send waiting for room
  when its channel closes; a send to the inbox of a task that's ended, on a run that didn't try the
  task ending first; a `shared` value one side changes and the other reaches; a block that fails,
  which cuts its task short at once, unless it's a send failing with `Closed` to a task that's
  failed, whose failure the block ends with; waiting inside a `lock`; `stop_after=`; and every task
  waiting on another, which hangs. A wait given a timeout times out only when nothing else can go
  on, however short its time, even `1ns`: only a wait of no time, `0ms`, looks once, as the
  runtime's does. That one is refused unless nothing else holding an end of its channel could still
  send, take or close, a task waiting for room behind a send beside its own counting as one that
  could, and a task that doesn't know the channel was made, by a launch or a message, holding none;
  what it finds, a
  message, room, the messages filling it or the close, was there before it; and it isn't in a
  `parallel` round beside others, which could use any channel, one the round made included. A task that ends with messages left in its inbox that its block sent as it
  ran: whether each
  send came before the task ended or after, and failed with `Closed`, hangs on time, so a later run
  tries the task going on first at that send, and the runs have to end the same.
- `std.json`'s `dumps` and `loads[T]` are written here on their own, by the type the checker gave
  the value, never by running the standard library's reader and writer. A struct is an object of the
  fields it saves, under the names `@serialize(name=...)` and `names=` give them, and `@skip` leaves
  a field out. A case is its name, or an object holding its fields under its name. A tuple is an
  array, and a dict is an object when its first key is text or an int, and otherwise an array of
  `[key, value]` pairs. A `Table` is a list of its items and an `Id` is where its item is in it, so
  a load makes a new `Table`, with a tag of its own. `indent=` and `extra=` are modeled too. Text is
  checked as JSON first, as `std.json` checks it, and a load that fails says what `std.json` says:
  what's wrong with text that isn't JSON, by line and column, and otherwise where in the value it
  doesn't fit, and how. A failure whose message the oracle can't be sure of is refused, as
  `json.loads failing (...)`. A float JSON can't hold, or an `Id` that can't be saved, stops the
  program. A type that writes its own `def serialize` and `def deserialize`, as the standard
  library's collections do, runs them from their source, given a `Serializer` that keeps the one
  value it's told to save, and a `Deserializer` that reads the whole value as the type the checker
  gave the call, or says where the read is for a failure the type's code makes. One that saves or
  reads through `record` and its fields, or more than one value, is refused. A union, bytes, and
  `json.dump` and `json.load` are unsupported.
- A value saved through a format a program writes, a type with `Serializer`'s methods, is saved as
  `std.serialize` sets the calls out (`interp/saving.rs`), by the type the checker gave it:
  `out.int` and the like for a plain value, `none` or `some` for an optional, `sequence` for a list
  or a set, `map` for a dict's pairs, `record`, then `field` or `renamed` for each field it saves,
  then `end` for a struct, and `enum_case`, or `case_record`, its fields and two `end`s, for an
  enum's case. A type's own `def serialize` runs instead. A generic function runs once here, where
  the compiler builds it for each set of types, so each call notes what its type parameters stand
  for there (`interp/typed.rs`): `x.serialize(ref out)` of a `T` saves an `int?` holding 1 through
  `some`, as the compiled program does. Loading through a format's `Deserializer` is modeled the
  same way (`deserialize_from`): a plain value by its own method, an optional through `none` then
  `value`, a list through `sequence`, a set through `sequence` with its repeats dropped, a dict
  through `map_count` or `map`, a key that comes twice failing where `here` says, and a struct or a
  case through `ordered`, or `record`, `next_field` and `field_at`, as the checker's `def
  deserialize` reads it: a field left out takes its default, an optional one with none is `None`,
  and any other fails with `missing`. A union, a tuple, a `Table`, an `Id` and a type with a `where`
  rule, saved or loaded this way, are unsupported.
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
  such a change writes the part back too, with what changed before it, since only
  `lock ... as ref` rolls back. A `with` source given a `ref` through `[]=` is refused.
- `value? from Context(...)` passes a failure up as the context, made only then. Native code
  keeps what caused it with it, but nothing reads that yet.
- An `array[T, N]` is held as the list it prints as, and `array[T, N]()` is `N` of `T`'s zero, as
  the checker types it.
- An `f32` is held as one, so each step rounds to 32 bits, as `docs/language.md` says, and it prints
  as the shortest text that reads back as the same `f32`. `+`, `-`, `*` and `/` are modeled, and
  conversions to and from it; `//`, `%`, `**`, a format spec and `sum` of them are refused.
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

`coverage` says, for each example, whether it runs and prints its `.out` file, or why it's skipped.
Two lists beside the crate set some apart. `never.txt` holds those the oracle can never model, as
one that talks over real sockets. `excluded.txt` holds those left out of the useful set, each with
the check that covers it instead: an algorithm the runtime writes in Rust to a published standard,
like regex, DEFLATE, the Unicode tables and the hashes, which is checked against that standard's own
answers. The report ends with how much of the useful set it runs, and names every other example it
skips as a gap to build.

`runs.txt` lists the examples it runs, each held to its `.out` file by `tests/examples.rs`, and
`gaps.txt` those it could model but doesn't yet, each with what stops it. Every example is in
exactly one of the four lists, so a new one comes with a line saying which. One the oracle runs is
held from the day it comes, and one it can't is a named gap, so the share it checks can't go down
without a word. The nightly report gives that share each morning, beside the random programs'.
