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

**Indexing a string takes constant time when it's ASCII.** A string knows whether all its characters are ASCII, so `s[i]` is direct for those. On other strings it walks from the start, and `for c in s` is the fast way through.
*Why:* Source code, identifiers and most text in programs are ASCII, and lexers index them in loops.

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

**Parallel reductions combine in a fixed order.** How `sum(... for e in parallel(xs))` splits and combines its work depends on the data's size, never the thread count.
*Why:* Float results don't drift between runs or machines.

**`Clock` is a capability,** passed to `main` like `Stdout` and `Fs`. `clock.now()` reads a monotonic clock. A fixed clock for tests is still to come.
*Why:* Code that reads the time says so in its parameters, and can be handed a fixed clock instead.

**`Random` is a seeded value.** `Random(seed=42)` draws the same numbers in the same order on every machine. It's SplitMix64, done in integer math, and `random_int(seed, n, low, high)`, imported from `std.random`, gives the `n`th number of a seed's sequence without keeping a generator. Seeding from the OS will be a capability `main` receives; it isn't built yet.
*Why:* A seed is all a replay or a lockstep peer needs to see the same numbers. Drawing by position also suits procedural generation, where each chunk of a world takes numbers from its own index.

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

**Every `def` writes its return type. Arrow functions never do. No `->` means the function returns nothing.**
```fennec
def visible(world: World) -> list[Enemy]:
    def in_view(enemy: Enemy) -> bool:
        enemy.distance_to(world.camera) < world.view_range
    [e for e in world.enemies if in_view(e)]

enemies.sort(key=e => e.health)          # an arrow's type comes from where it's passed

def main(stdout: Stdout):                # no arrow: returns nothing
    stdout.print("hi")
```
- Nested `def`s follow the same rule as top-level ones. There's one rule: `def` has an arrow when it returns a value, and `=>` never has one.
- A `def` that returns a value without an arrow is an error whose fix writes the inferred type in. `fennec fix` does it for a whole kit.
- `-> None` has a fix that removes it, since no arrow already means the function returns nothing.
- Everything inside a body is still inferred: locals, empty collections (`dict[str, int]()` when a type must be said), closures, comprehensions and type arguments at calls. Errors are still inferred too, and `fails` stays optional.
- Recursion needs nothing special: every named function already has its type written, and an arrow can't call itself.

*Why:* When AI writes most code, writing costs little and reading is what matters, for people reviewing it and for agents reading other files. A complete signature summarizes a function without its body. A mistake in a body is reported inside that function ("returns `float`, but the signature says `int`") instead of at a caller in another file, where an agent tends to "fix" the wrong place. Rust, Swift, Kotlin and well-kept TypeScript settled on the same line: explicit where a function meets the rest of the program, inferred inside. Annotating locals is left out because it's what makes bodies tiring to read. It's cheap to reverse: `fennec fix` can strip written return types again if measurement shows they don't help.

This replaces the earlier rules that return types are inferred, and that recursive functions must declare theirs.

**Builtins work on anything; methods belong to a type.** Builtins work on any value that fits an interface and never change it: `len`, `sum`, `min`, `max`, `any`, `all`, `sorted`, `enumerate`, `zip`. Methods are specific to a type or change the value: `xs.append(x)`, `d.keys()`, `s.split(",")`, `mut xs.sort()`. A type supports a builtin by having the method, such as `def len(self)`. The language server offers the builtins that apply after `xs.`.
*Why:* Builtins read well with comprehensions (`sum(e.health for e in enemies)`), and Python users and AI already write them. Python mostly follows this split; Fennec always does.

**No global function serves only one type.** A builtin has to work across types, through an interface, or be a type's constructor, like `int(...)`, `float(...)` and `range(...)`. An operation on one type is a method. So a character's code is `"A".code()`, not Python's `ord("A")`, and `ord(c)` is an error whose fix writes `c.code()`.
*Why:* Global functions that only fit one type crowd the namespace, can't be extended by other types, and hide where an operation lives.

**A type is said by calling it with its type arguments:** `counts = dict[str, int]()`, `mut queue = Deque[Job]()`. There are no annotations on local names. An empty `{}` or `[]` takes its type from how it's used later.
*Why:* It needs no new syntax, it's one line, and Python 3.9 and later accept the same thing.

**A leading-dot case resolves from the type expected where it's written.** A parameter, a field, a return type, a variable whose type is known, or the other side of `==` decides which enum `.Int(value)` belongs to:
```fennec
def literal(tok: Tok) -> ExprKind:
    match tok:
        case .Int(value): .Int(value)   # the return type makes it ExprKind.Int
```
Only with no expected type is a name shared by two enums ambiguous, and the error lists them. The search covers only the enums a file can see: its own and those it imports.
*Why:* The type is usually right there, as in Swift. Otherwise adding an unrelated file could break code that never imports it.

**`extend` adds methods to a type from another file of the same kit.**
```fennec
# parser/expr.fen
extend Parser:
    def parse_expr(mut self) -> Expr:
        ...
```
An extension adds methods only, never fields, so a type's shape stays in its declaration. It works only on the kit's own types, never another kit's or the standard library's. Two methods with the same name on one type are an error, wherever they're written. The editor, `fennec doc` and `fennec context` list every method of a type in one place.
*Why:* A type that many responsibilities attach to, like a parser, an interpreter or a game's `World`, is otherwise one huge file, or its methods become free functions with noisier calls. Swift, C#, Kotlin, Go and Rust all let a type's methods live in several files. Keeping it to the kit's own types means no one can change a type they don't own.

**Units of measure are types.** Meters, pixels, seconds and radians can't be mixed by accident, and multiplying or dividing them gives the right unit. They're erased at compile time, so they cost nothing at run time. The syntax is still open.
*Why:* Game code mixes up degrees and radians, and world and screen space, constantly. F# has had units since 2009, and no mainstream language has followed.

**They're called interfaces, not protocols.** `interface Shape:` declares one, and a type fits it by having its methods, with nothing to declare.
*Why:* Go and TypeScript, the best-known languages where types fit by their methods alone, both say `interface`, and it's the word people and AI reach for first. Python's `typing.Protocol` is a library class most code never uses, and Swift's protocols need a declared conformance, which is the opposite of Fennec's rule. Python's informal sense, like the iterator protocol, is still fine for describing `def len` and `def []`.

**An enum fits an interface when every case does.** The compiler writes the `match`, so `total(targets)` works on a list of a `Target` enum whose cases are all `Damageable`.
*Why:* Lists of mixed types stay static, since the `match` compiles to a jump table, with nothing extra to write.

**Type parameters are written the way Python 3.12 writes them.** The bound says what the type must be able to do:
```fennec
def total_area[T: Shape](shapes: list[T]) -> f32:
def largest[T: Ordered](xs: list[T]) -> T:
def merge[T: Ordered & Hashable](a: list[T], b: list[T]) -> list[T]:
struct Pool[T]:
interface Container[T]:
```
Type arguments are inferred at calls; `largest[int](xs)` is allowed when they can't be. This replaces the earlier shorthand where an interface in a parameter's type, `list[Shape]`, meant "any one type that fits". Writing that now is an error, with a fix to `[T: Shape]`.
*Why:* `T` lets the type flow through, so `largest(enemies)` gives back an `Enemy`, and it relates parameters to each other. There's one way to write it, and it's Python's. In Python, Java, C# and Go, `list[Shape]` means a list of mixed types dispatched at run time, so the shorthand would have misled exactly the people and models Fennec is written for.

**Generic code is checked once, and errors name the missing piece.** A generic body may use only what its bounds promise, so errors never surface at an instantiation. A call with a type that doesn't fit says what's missing and the nearest match: "`Rock` isn't a `Shape`: it has no `area(self) -> f32`. Its `area` returns `int`."

**Generics are compiled per type everywhere, including development builds.** Each instantiation is a function compiled on its first call and cached by content. There's no dictionary passing and no dynamic dispatch, so development and release builds behave the same and every call is direct. If instantiations multiply, sharing code between ones with identical layout is a later optimization.

**Mixed types are an `enum`; open sets are function values.** A list that holds different types is a list of an enum: each element is inline with a one-byte tag, and `match` compiles to a jump table. When other kits must be able to add kinds, as with plugins, a struct with function fields does it, and a call through a function value is the only run-time dispatch in the language.

**`Eq`, `Hash`, `Ordered` and `Format` are built-in interfaces,** satisfied automatically wherever the automatic `==`, hashing, ordering and printing apply.

**`=>` is one expression; anything longer is a nested `def`.** There are no multi-line anonymous functions, as in Python. Function types are written `(int, str) -> bool`.

**Functions used as values never change anything around them.** An arrow function, or a nested `def` that's passed, stored or returned, captures the names it reads by value and can't change any of them. Capturing a `mut` name is an error, with a fix: bind it to a new name first.
*Why:* Every closure that escapes is then a pure function of its inputs, safe to run in parallel, cache or replay. It needs no analysis of which closures escape and no marker on parameters that store one, and capturing costs a reference-count bump at most. Handlers return commands instead of changing state, and builtins and comprehensions build up values.

**A nested `def` that's only called directly can change the enclosing function's `mut` names.**
```fennec
def collect_leaves(tree: Tree) -> list[Tree]:
    mut leaves = []
    def visit(node: Tree):
        if len(node.children) == 0:
            leaves.append(node)
        for child in node.children:
            visit(child)
    visit(tree)
    leaves
```
*Why:* This is Python's most common closure pattern, and it needs no `nonlocal`. Whether a name is only ever called directly is trivial to check, and such a helper works on the enclosing function's variables in place, at no cost. `parallel` bodies still can't change outer names, including through helpers.

This replaces the earlier rule that closures which don't escape may change captured `mut` names.

## Cleanup

**`with` runs cleanup on every way out of its block:** the end, `return`, `break` and `?`. Nested `with` blocks clean up innermost first. There's no unwinding.

**The value after `with` must be a plain value.** An error from opening it is dealt with first: `with fs.open(path)? as file:`.

**`def del` can't fail.** It does best-effort cleanup, such as closing, rolling back or freeing, and its signature can't have `fails`. Nothing can make an error disappear, unlike Python's `__exit__`.

**Finishing that can fail is an explicit method,** called and checked:
```fennec
with db.transaction() as mut tx:
    tx.insert(player)?
    tx.insert(inventory)?
    tx.commit()?
# not committed? `del` rolls back
```

**A type can require its finish.** For a type like a buffered writer, cleaning up on the success path without calling its finishing method is a compile error: "`writer` was cleaned up without `close()`, so unflushed data would be lost". On error paths, best-effort cleanup is enough. How a type marks this is still open.
*Why:* Cleanup that fails with nowhere to report the error silently loses data.

## Standard library

**Names live in their modules.** A standard library module provides nothing until it's imported: `from std.math import Vec3, sqrt`, or `from std import math` and then `math.Vec3` and `math.sqrt(x)`. That's every module so far: `math`, `game`, `layout`, `path`, `json`, `toml`, `collections`, `random`, `time` and `re`. Using a name without its import is reported with the import to add.
*Why:* A file's imports show everything it uses from the standard library, as in Python.

**What the language itself needs is available everywhere.** Capabilities, `Stdout`, `Fs`, `Clock`, `Args` and `Stdin`, which `main` names to say what the program may do. The errors that standard operations fail with, `IoError`, `InvalidValue` and `ParseError`, so code can match them without knowing which module failed. `Ok` and `Err`, which wrap any result. And the Python builtins, like `len`, `range` and `abs`.
*Why:* These belong to the language, not to one module. A capability in particular can't come from an import: it's handed to `main`, and only what `main` is given exists.

**Command-line arguments and standard input are capabilities.** `def main(stdout: Stdout, args: Args, stdin: Stdin):`. `for arg in args:` gives the arguments, not the program's name, which is `args.program`. `stdin.lines()` reads input a line at a time. A program that doesn't name them can't read them, and both are recorded for replay.
*Why:* A program's input decides what it does, so it comes in the same visible way as files and the clock.

**`int(text)` and `float(text)` fail with `InvalidValue`** when the text isn't a number or doesn't fit, handled with `?`, `else` or `match`. `int(3.7)` still truncates to `3`. `int()` of NaN, or of a float too big for an int, stops the program, like overflow.
*Why:* Text often comes from input, so a bad number is expected and must be handled. A NaN reaching `int()` is a bug in the program.

**A string's characters are reached by code and as bytes without copying.** `"A".code()` gives `65`, `str.from_code(65)` gives `"A"` and fails with `InvalidValue` for a number that isn't a character, and `s.codes()` gives every code. `.code()` on anything but one character stops the program, like an index out of range. `s.encode()` is a view of the UTF-8 bytes, with no copy, so `len(s.encode())` is the length in bytes.
*Why:* A lexer needs character codes and byte lengths, and strings are stored as UTF-8 already, so the bytes cost nothing.

**`bytes` is binary data, a run of ints from 0 to 255.** It's a built-in type, like `str`: `len`, indexing and looping give ints, slicing and `+` give `bytes`, and it compares, orders and hashes. `bytes([137, 80])` makes some from a list, and a number outside 0 to 255 stops the program, like an index out of range. `text.encode()` gives text's UTF-8 bytes, and `data.decode()` gives the text back, failing with `InvalidValue` when the bytes aren't UTF-8. Bytes print as `bytes([137, 80])`, which is also how to write them; there's no literal like Python's `b"..."` yet.
*Why:* Files, hashes and network data are bytes, and keeping them apart from `str` means text is checked once, when it's decoded. A byte out of range is a mistake in the program, while text that isn't UTF-8 comes from outside and has to be handled. Printing bytes the way they're written means a printed value can be pasted back into code.

**Imports bind names exactly as in Python.** `import std.json` binds `std`, and the call is `std.json.loads(...)`. `from std import json` binds `json`, for `json.loads(...)`. `from std.json import loads` binds `loads`. A kit's own modules follow the same rule: `import game.physics` binds `game`, and `from game import physics` binds `physics`. Writing `json.loads` after `import std.json` is an error whose help suggests `from std import json`. There's no `import *`.
*Why:* It's one rule that Python developers and AI models already know, and an import never binds a name behind your back. Earlier, `import std.json` also bound `json`, which Python doesn't do.

**`as` renames an import, in both of Python's forms.**
```fennec
from std.json import loads as json_loads
from std.toml import loads as toml_loads
import game.world.physics as physics
```
- The descriptive-names lint covers aliases: `import std.json as j` is flagged like a variable named `j`.
- A module's own name has one spelling. `import std.json as json` has a fix that rewrites it to `from std import json`, which means the same.
- An alias can't take a name the language provides everywhere, such as `len`, `Ok` or `Stdout`.
- An alias to the same name (`from std import json as json`), or importing one thing twice under two names, is a lint.
- `pub import x as y` re-exports under a new name, which helps a kit rename part of its API.

*Why:* Two modules with the same function names, like `std.json` and `std.toml`, or two kits with a `utils` module, need a way out other than full paths everywhere. Python developers and AI write `as` without thinking, so refusing it only causes errors. Aliases are resolved at compile time and cost nothing.

**Two modules can have functions with the same name.** Without an import, `loads` doesn't exist, and the error says which modules have it.
*Why:* `std.json` and `std.toml` both have `loads` and `dumps`, as in Python, so the name alone can't say which format is meant.

**JSON and TOML read into plain values.** Objects and tables become dicts in the order written, arrays become lists, and TOML's dates and times stay strings as written. The checker doesn't know the shape of text it hasn't seen, so the values' types are unknown and checked as they're used. Writing takes dicts, lists, tuples and structs. Typed reading, `loads[Settings](text)`, comes later.

**Bad text fails with `ParseError`.** It has the `message`, `line` and `column`, and the message says the line and column too. A value a format can't hold, like a set in JSON or `None` in TOML, stops the program, like overflow.
*Why:* Bad input is expected and has to be handled. Writing a set as JSON is a mistake in the program.

**`Path` is a struct written in Fennec.** Its methods only look at its text: `join` (also `/`), `parent`, `name`, `stem`, `extension` and `normalize`. `extension` has no dot: `"png"`, not `".png"`. `fs` methods take a `Path` or a `str`.
*Why:* Path handling needs nothing the language can't already do, and code written in Fennec is checked and compiled like everyone else's.

**Files and folders go through `fs`.** The `Fs` capability has `read_text`, `write_text`, `exists`, `list_dir`, `make_dir`, `remove` and `metadata`, and each takes a `Path` or a `str`. `list_dir` gives the folder's `Path`s sorted by name. `make_dir` makes any missing parent folders too, and succeeds if the folder is already there. `remove` removes a file, or a folder only if it's empty. `metadata` gives a `Metadata` from `std.fs`, with `size`, `is_file` and `is_dir`. `read_bytes` and `write_bytes` read and write `bytes`. Every failure is an `IoError`.
*Why:* Sorting makes a listing the same on every system, so programs and their tests behave the same everywhere. Making the parents and accepting a folder that's already there is what code almost always wants, like Python's `os.makedirs(path, exist_ok=True)`. Removing only empty folders keeps a wrong path from deleting a whole tree; a recursive remove can come later, under its own name.

## Visibility

**`pub` marks what other kits can use.** Inside a kit, every file and module sees everything. `pub` goes on each `def`, `struct`, `enum`, `error`, `interface`, `type` alias and re-exported `import` that other kits may use, and on each struct field they may see.
- If every field of a struct is `pub`, other kits get its automatic constructor. Otherwise they build it through its `pub` named constructors.
- Enum cases and interface methods come with their type and have no `pub` of their own.
- Writes from another kit still run the field's `where` rules.
- A `pub` signature can't mention anything that isn't `pub`.
- A lint flags `pub` items that no other kit can reach.

**Errors are inferred, even on `pub` functions.** A kit's API is checked against its last published version instead, cached from the Den. When a change would break other kits, `fennec check` says so at once, in the editor: "`load` can now also fail with `ParseError` (since v1.2.0). This is a breaking change: handle or wrap it, or release v2.0." `fennec publish` refuses a version number that undersells the change. Adding an error is a breaking change; removing one isn't.

**Declaring errors is optional,** written with `fails` after the return type and checked like a declared return type: `def load(fs: Fs, path: Path) -> Config fails NotFound, ConfigUnreadable:`. `type LoadError = NotFound | ConfigUnreadable` names a set of errors.
*Why:* The same protection as required declarations, with nothing to write. Return types are written on every `def` (see Functions and types), so the API check matters most for errors.

A big app that wants walls between its own parts splits into local kits in one workspace.
*Why:* `pub` appears only where API stability matters, and never in apps. Making every field of a `pub struct` public would make every internal field part of the API, so renaming one would need a major version. Go's capitalization rule would clash with lowercase built-ins and capitalized user types.

## Files and kits

**A kit is a folder with a `kit.toml` and its code under `src`.** `src/main.fen` holds `main`, and the module `enemies.ai` is the file `src/enemies/ai.fen`. `kit.toml` has `name`, `version` and `fennec`, the toolchain the kit is built with (see Toolchain). Until the launcher exists, a kit asking for a newer Fennec than the one running is an error. Its `[dependencies]` table is reserved for kits from the Den. `fennec run` and `fennec test` with no file run the kit around the current folder; `fennec test` runs the tests in every file under `src`.
*Why:* One layout means no configuration, and finding a module's file needs no search.

**A file uses another file's names only after importing them.** `from enemies import spawn` brings in names, and `import physics` brings in the module, used as `physics.step`. For a module in a folder, `import world.physics` is used as `world.physics.step`, and `from world import physics` as `physics.step`. A file's own names need no import.
*Why:* Where every name comes from is visible in the file that uses it, for people and for AI.

**A kit's files share one set of top-level names.** Two files of a kit can't both define `update`; the error names both files.
*Why:* A name means the same thing everywhere in the kit, and a value always prints with the one name its type has. Methods belong to their type, so the names that clash are few.

**Imports can't go in a circle.**
*Why:* Two files that need each other usually share something that belongs in a third.

## Concurrency

**Tasks are scoped.** Every task ends before the `with` block or `parallel(...)` call that started it. There are no detached threads.
*Why:* Because the parent outlives its tasks, a task can read the parent's values through views without touching reference counts, so reference counts stay non-atomic and fast. There's no `Send` or `Sync` to write.

**The first slice is parallel loops over collections.** `parallel(xs)` wraps anything iterable, with no new keyword:
```fennec
for mut particle in parallel(particles):
    particle.position += particle.velocity * dt

total = sum(e.mass for e in parallel(enemies))
hits = [check(b) for b in parallel(bullets)]
```
- Each iteration can change only its own element. It can read anything around it, but can't change outer `mut` names or use capabilities.
- Work runs on a work-stealing thread pool, and reading outer values changes no reference counts.
- The interpreter runs the loop in order. Results are deterministic, so that's a correct implementation, and the playground needs no threads.

Scoped tasks with `with`, channels, locks and overlapping I/O come in later slices.
*Why:* It's most of what games need from many cores, it's the smallest sound piece, and it exercises the ownership rules everything later depends on.

**Tasks change only separate pieces.** `for mut e in parallel(enemies):` gives each iteration `mut` access to its own element, checked by the same rule that makes `mut` exclusive. Results combine through reductions, like `sum(... for e in parallel(xs))`.

**Only values with a single owner cross a channel.** They're moved, never shared.
*Why:* That's what lets reference counts stay non-atomic even with long-lived worker tasks.

## Syntax

**A module can hold constants.** `KEYWORDS = {"def", "if", "else"}` at the top level is an immutable name, computed when compiling from literals, collections and calls to functions that take no capabilities. There's still no `mut` at the top level, so there's no global state. UPPER_CASE is a convention the lint suggests, not a rule.
*Why:* A table rebuilt inside a function on every call is what people write only when the language leaves them no choice. A constant is computed once, before the program runs.

**String escapes are Python's:** `\n`, `\t`, `\r`, `\0`, `\\`, `\"`, `\xhh`, `\uhhhh` and `\Uhhhhhhhh`.
*Why:* Control and invisible characters need a way to be written, and these are the forms people and AI already know.

**There's no walrus (`:=`).** A loop that runs while a lookup succeeds is a `for` over a generator, and an `if` on a lookup is two lines, with the same narrowing:
```fennec
for tok in lexer:
    use(tok)

entry = table.get(key)
if entry is not None:
    use(entry)
```
*Why:* The walrus needs parentheses to mean what it looks like, which is a trap, and generators and narrowing already cover what it's for.

**Raw strings are `r"..."`.** Backslashes stay as written, so a pattern reads `r"\d+"`. A raw string can't contain `"`.
*Why:* Regular expressions are unreadable with every backslash doubled. Python has the same strings, so people and AI already write them.

**Doc comments are `##`, above what they document.** A `##` comment documents the function, struct, field or enum case that follows it, or the module when it's at the top of the file. Examples go in Markdown code fences and check results with `assert`. `fennec test` runs them. There are no `"""` docstrings: strings are never documentation.
*Why:* One form that works on everything, including fields and enum cases. Fences render properly in `fennec doc`. Nim uses `##` for doc comments too.

**f-strings have `{x=}` and no `!r`.** `f"{hp=}"` gives `hp=100`. Format specs like `{x:.2f}` stay.
*Why:* `{x=}` is what people want when debugging; `!r` has no meaning without Python's `repr`.

## Standard library

**Failures inside the standard library point at the program's call.** When a standard library function written in Fennec fails, the error shows the line in the program that called it, in both engines.
*Why:* The library's own source isn't something the reader can fix, or even see.

**Taking from an empty `Deque`, `Heap` or `PriorityQueue` gives `None`.** `pop`, `popleft` and `peek` return optionals, so the checker makes the empty case visible. `list.pop` keeps Python's behavior and fails on an empty list.
*Why:* Draining a queue until it's empty is the normal way to use one, so the empty case is expected, not a mistake.

**Regular expressions take linear time.** `std.re` runs patterns on a Pike VM, which follows every way a pattern could match at once, so no pattern can make matching blow up. There are no backreferences or lookarounds, which need backtracking. A pattern written in the program is checked before it runs; `Regex(text)` with a pattern built while it runs gives a result, like any struct with a `where` rule.
*Why:* A pattern from a mod, a config file or a player can't hang a game or a server.

## Toolchain

**The installed `fennec` is a small launcher.** It reads the version a kit asks for, `fennec = "0.4.2"` in `kit.toml`, with no separate version file. On first use it fetches exactly that toolchain into `~/.fennec/toolchains/0.4.2/`, verifies its checksum and signature, records the toolchain's hash in `kit.lock`, and hands off to it.
- `fennec toolchain list`, `install`, `remove` and `default` manage toolchains.
- It never updates itself or fetches "latest" without being asked.
- Single-file scripts outside a kit use the default toolchain.

*Why:* Every project gets its own Fennec version without anyone thinking about it, and teammates, CI and AI agents all build with exactly the same toolchain. It's also what makes lints pinned to a kit's Fennec version work.

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

The JIT, release builds and the language server all share one build engine and cache, described under Builds.

## Builds

**The unit of work is the function, keyed by its content.** A function's key is a hash of its own syntax tree plus the signatures of what it uses, not their bodies. A caller depends on a callee's body only if it inlined that callee. A change inside a function that leaves its signature alone recompiles nothing else.

**Results are pure.** Cache keys never include paths, timestamps or environment variables, and every key includes the compiler version. Builds are deterministic, so each function of a kit compiles once per machine, ever.

**There's one cache per machine,** `~/.fennec/cache`. It has a size cap, and old entries are cleaned up. Entries are written to a temporary file and renamed into place, so readers never see half an entry and the normal path takes no locks; a lock is taken only during cleanup.
- There's no build folder inside each project. Development runs compile straight from the cache with the JIT, and only `fennec build --release` writes an executable.

**Work runs in parallel on every core,** one function at a time per core.

**One engine stays running.** A background daemon serves the compiler, checker, formatter and language server from the same queries. It hot-reloads changed functions into a running program.

**Caches can be shared** between machines and CI. Results from elsewhere are used only when they're signed or verified locally.

**Release builds use LLVM with whole-program optimization,** cached per function where possible.

*Why:* These avoid what makes builds slow elsewhere: large compilation units, build scripts and macros that run code, a build folder in every project that grows to gigabytes, and separate engines for the compiler and the editor.

**Hashing works in two layers.**
1. A BLAKE3 hash of each file's bytes. An unchanged file isn't even parsed again, and timestamps are never used, so switching branches or cloning in CI doesn't rebuild anything.
2. A hash of each function's syntax tree that leaves out positions and comments. Reformatting, editing comments and moving code around recompile nothing. Positions are kept separately for error messages.

BLAKE3 is written in-house rather than taken as a dependency, and checked against the official test vectors.

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

- How a type marks that its finishing method must be called.
- Decorators. If they come, they'd be typed annotations: data attached to a function, read by kits through reflection at compile time in the sandboxed interpreter, never wrappers that change the function. `@get("/todos/{id}")` on a plain function would let a router build its table at compile time.
- Whether independent writes, not just reads, should overlap automatically.
- The exact syntax for property tests.
- The syntax for units of measure.
- The file extension (currently `.fen`).
- Whether files of a kit may reuse a top-level name, reached through `import` by their module's name.
- These security proposals:
  - `Net`, `Env` and `Process` as capabilities.
  - Narrowed capabilities like `fs.only("saves/")`.
  - Each kit's capabilities recorded in `kit.lock`, with any increase needing approval.
  - Types that make injection impossible: `sql"..."` templates, `Path`, argument lists for commands, escaping HTML templates.
  - `Secret[str]`, which can't be printed or logged.
