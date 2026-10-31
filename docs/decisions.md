# Design decisions

Decisions made while designing Fennec, with the reason for each. The [roadmap](../site/src/pages/roadmap.md) says when each one gets built. Newest decisions go at the top of each section.

## Values and memory

**Everything is a value unless the code says otherwise.** Structs nest freely and are stored in place. Nothing is shared unless the code says `mut`, `Id` or `shared`.
*Why:* A reader can see every place two names reach the same thing, and the compiler keeps flat memory and every optimization everywhere else.

**Fennec has no object graphs. Data lives in tables.** A thing that others point at lives in an `IdMap` or a list, and they hold its `Id`.
*Why:* Flat tables are what keep memory contiguous, copies cheap and cycles impossible, and they're what game engines and databases already use.

**`mut x = place` edits a place, however deeply it's nested.**
```fennec
mut unit = world.units[id]
unit.health -= damage
```
Writing through the name edits the real place, in one write to memory. Most edits don't need a name at all: `world.units[id].health -= damage`.
- Replacing the name makes a new value, as in Python: `best = x`, `i += 1`. Doing both on one name is a compile error.
- `mut roster = hp` followed by reading `hp` is a compile error, and the fix is `mut roster = copy(hp)`.
- The name can't be stored, returned or captured by an escaping closure, so there are no lifetimes to write.
- The path it came from stays locked until the name's last use. Names for separate fields of one struct are fine.
- `mut a, b = xs.pair(i, j)` gives two elements of one list, checked at run time.

This replaces the earlier rule that `mut x = place` makes a copy, and the lint that called out Python's sharing. The compiler catches every case the change affects.
*Why:* Editing nested structs is the one thing values make awkward. This is the form people already write, it does what they expect from Python, and it needs no new type.

**A copy that can go stale is a compile error.** Copying a struct into a longer-lived place, when the copy can diverge from a source still in use, needs `copy(x)`:
```fennec
world.selected = world.units[i]          # error: edits to one won't reach the other
world.selected = copy(world.units[i])    # a separate copy, on purpose
```
Immutable local snapshots stay allowed, and so does sharing that never changes.
*Why:* This is the mistake people and AI trained on references will make. It compiles and looks right, then one side silently stops updating. The error asks which was meant.

**Cut from the first version, and each can be added later without breaking anything:**
- `yield mut` helpers that lend a place. `world.units[id].health -= 10` covers most cases.
- Inferring which fields a helper touches, so the rest of a struct stays usable across the call. Passing `mut world.units` works today.
- `Graph` in `std`. `Tree` comes first.
- Compile-time checks for channel deadlocks. Detection starts at run time.

*Why:* It's less to build and less to get wrong. If something turns out to be missing, it's added with evidence.

**Named fallbacks, used only on evidence.** There's no `class`, no `Frozen[T]` and no `with pool()`.
- `class` with `link` fields, if the AI evaluation shows `Id` code doing badly on combat and UI tasks.
- A limited `View[T]`, if having no stored views costs more than about 10–15% on parser, zero-copy server and B-tree benchmarks.
- Tighter lock patterns, if a B-tree benchmark fails.

*Why:* All of them are additions that break nothing later. `Frozen[T]` adds nothing while a plain value can already cross tasks for one count bump.

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

**`IdMap` and `Id[T]` are the way to point at things.** An `Id[Enemy]` is an index, a generation and a tag for its map, made by an `IdMap[Enemy]`. It can't index a map of another type, and the tag means an ID from another map can't find the wrong item.
- `map[id]` stops the program on a dead ID, like an index out of range. `map.get(id)` returns an optional, for when the item may be gone.
- A map reports how many items are live, which makes a forgotten item visible.
- Its storage is in chunks, so one write after a snapshot copies a chunk rather than the whole map.
- `type EnemyId = Id[Enemy]` is an optional alias, not something the compiler makes for each struct.

*Why:* Game engines, compilers, graph libraries and servers use the same generational-handle pattern. IDs are small plain values, so they're cheap to copy and can be saved or sent anywhere. A cycle of them is just numbers, with nothing counted, so it can't leak. A stale ID that silently became `None` would hide bugs, so the short form is the loud one.

## Collections

**Collections are values.** `b = a` or passing a `list`, `dict`, `set` or `str` gives a value that never changes under you, like every other value. `copy(a)` gives one that can change separately.

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

**The compiler and runtime choose allocation, and code never names an allocator.** mimalloc is the invisible last step, for what the compiler can't place itself. It's vendored (it's MIT-licensed) and later ported to Fennec. It keeps per-thread lists of free blocks for each size, so allocating and freeing take a few instructions.

**The compiler places short-lived values itself.** A temporary that never leaves its function or loop body goes on the stack or in a bump region, with no call to the allocator. This is limited to values that provably don't escape. Full automatic region inference is known to leak memory.

**`@hot` on a function means prove it or don't compile.** The compiler must prove the function and everything it calls does no allocation, makes no copies and has no leftover count checks, or it's a compile error that names the line. A lint suggests it where profiling shows heavy allocation. This replaces `with noalloc():` and the `noalloc def` prefix.
*Why:* It's a claim about a function, and nothing opens or closes, so it doesn't belong in `with`. Speed in a hot path becomes something the compiler checks rather than something people hope for.

**`@arena` is cut for now,** along with `with arena():`. It's expected back at function level for tree-heavy allocation, after measurement. There are no allocator parameters, now or then.
*Why:* The compiler handles much of it, and web servers get it per request, so it hasn't earned its place yet. Passing allocators to every function, as Zig does, is a lot of AI pain for little gain.

**Debug builds report allocation.** They show allocations and bytes per frame, find leaks, and can enforce a budget ("this frame allocates under 1 MB").

## Determinism

**Dicts and sets keep insertion order.**
*Why:* The same program iterates the same way on every run and every machine. Python already does this, so it costs AI nothing.

**Evaluation is always left to right.**

**Floats give the same results on every platform.** Operators follow IEEE, and `sorted`, `min` and `max` use one total order with NaN last, so a NaN's sign can't change a result. There are no fused operations and no fast-math outside an opt-in, and `sin`, `cos` and the rest are in the stdlib rather than the platform's math library. `@fast_math` on a function opts out. This replaces `with fast_math():`.
*Why:* It makes lockstep multiplayer and replays possible. It costs a little, and only in math-heavy code, which can opt out.

**Parallel reductions combine in a fixed order.** How `sum(... for e in parallel(xs))` splits and combines its work depends on the data's size, never the thread count.
*Why:* Float results don't drift between runs or machines.

**`Clock` is a capability,** passed to `main` like `Stdout` and `Fs`. `clock.now()` reads a monotonic clock. Tests get a fixed clock by default, as under Tooling.
*Why:* Code that reads the time says so in its parameters, and can be handed a fixed clock instead.

**`Random` is a seeded value.** `Random(seed=42)` draws the same numbers in the same order on every machine. It's SplitMix64, done in integer math, and `random_int(seed, n, low, high)`, imported from `std.random`, gives the `n`th number of a seed's sequence without keeping a generator. Seeding from the OS will be a capability `main` receives; it isn't built yet.
*Why:* A seed is all a replay or a lockstep peer needs to see the same numbers. Drawing by position also suits procedural generation, where each chunk of a world takes numbers from its own index.

**Any run can be recorded and replayed exactly.** Every source of nondeterminism comes in through a capability: time, randomness, files, the network and input. So the runtime can record exactly what came through and replay the run bit for bit, in the debugger, with `fennec replay`. The order tasks reach a `shared` value or a channel is the one source outside this, and how replay records it is still open.
*Why:* A player's crash report becomes a replay file you can step through. The determinism rules above make it almost free.

## Speed

**Speed targets are measured against each benchmark's twin, fully optimized.** Every program in the benchmark suite also exists as idiomatic code in the fastest established systems language.
- Release builds: 110% of the twin's speed.
- The JIT: 80% to start, then 85%.
- The interpreter: 10–30%.

Benchmarks do realistic work that neither side's optimizer can delete.

"Fully optimized" means optimization level 3, fat link-time optimization, one codegen unit, abort on panic and no debug assertions, with overflow checks on. Fennec always checks for overflow, so the target does the same work. Numbers against twins without those checks are kept in the benchmark reports for reference, labeled as such. Twins are built for x86-64-v3 on x86-64 and for the baseline CPU on Arm, so their counts are reproducible and don't hold the twin back against a JIT that uses the running CPU's features.

Where a twin's standard library choice measures something Fennec doesn't do, like a slower default hash in `dicts`, the target uses a twin with the same hash as Fennec, and the standard default is shown as a separately labeled number.
*Why:* The whole 20-benchmark suite meeting these targets shows the language's fundamentals carry well.

**The targets are enforced.** CI fails if a benchmark misses its target or runs slower than the previous run. The fix for a failure goes into the compiler, never into tricks in the benchmark.

### Matching the fastest languages

**Proven single owners cost nothing.** When ownership proves a value has one owner, its reference counting and copy-on-write checks are deleted entirely, and the machine code is what a language with manual memory would produce. Most hot code is like this.

**Range analysis removes provably safe checks.** Overflow checks on loop counters, indices and bounded math, and bounds checks on indexing that can't go out of range, are removed. That brings checked overflow's cost close to zero.

### Going further

**Release builds optimize the whole program, guided by profiles.** Link-time optimization is on by default, and so is profile-guided optimization with `fennec build --release --profile`. Kits build from source and every call is static, so the optimizer sees everything.
*Why:* Together they're often worth 10–20%, and most projects in other languages never turn them on.

**Layout is free to change.** As described under Layout, the compiler can reorder fields, pack enums and split lists of structs into one array per field, because no reference can depend on where a value lives. Languages with references can't change layouts behind the programmer's back.

**Functions without capabilities have no side effects.** A `shared` handle in a signature acts like a capability. A function that isn't passed either can't do I/O or change anything outside itself, so the compiler can merge repeated calls, move them out of loops and vectorize loops more aggressively.
*Why:* In most languages the compiler has to assume any call might do anything.

**There's no unwinding.** Errors are values, and a runtime failure such as overflow ends the program. Containing one to a single server task is the intent, under Concurrency. With no unwinding machinery, code is smaller and inlines better.

**Math types are 32-bit.** `Vec3` and the other math types use 32-bit floats, and `float` stays 64-bit.
*Why:* Twice the SIMD throughput for game math, while `float` stays accurate everywhere else.

**The compiler tells LLVM that names don't alias.** Exclusive `mut` access proves no two names point at the same memory, so every function gets that guarantee for free. Fortran's speed comes from the same guarantee.

**Integer overflow is checked in release builds too.** Wrapping is opt-in: `x.wrapping_add(y)`.
*Why:* A program that compiles should do what it says. The check costs about 1–2%, and the benchmark suite measures it.

## Functions and types

**A function that can fail says so with `fails Name`, and every exit is visible.**
```fennec
error LoadError:
    NotFound(path: str)
    BadRow(line: int)

def load(fs: Fs, path: str) -> float fails LoadError:
    mut total = 0.0
    with fs.open(path)? as file:
        for row in rows_of(file)?:
            total += row.amount
    return total

result = load(fs, path)
if result is Err(NotFound):
    return Response(status=404)
```
- One `error Name:` can list several cases, so a signature stays short however many ways it can fail. `fennec fix` writes it.
- `?` is required wherever a failure passes up, including in `with` and `for`.
- `if x is Err(...)` and `if x is None:` narrow `x`. `match` handles several cases.
- Checking an error and then ignoring it is a compile error.
- `xs[i]` out of range is a bug that stops the program. `xs.get(i)` returns an optional, for when it might not be there.

This replaces the earlier rules that errors are inferred and `fails` is optional.
*Why:* A reader sees at a glance how a function can fail and every line where it can leave. Swallowed errors are how reliable-looking programs go wrong.

**`where` rules run on every write to a guarded field,** and on defaults at compile time: `health = 100 where health >= 0`. A rule looks only at its own field. An invariant across several fields goes in a named constructor or a method. Writes after construction aren't checked today, which is a bug.
*Why:* A rule that only holds at construction promises something it doesn't deliver. Checking one field keeps each write's cost obvious.

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
- Everything inside a body is still inferred: locals, empty collections (`dict[str, int]()` when a type must be said), closures, comprehensions and type arguments at calls. Errors are named in the signature, with `fails`.
- Recursion needs nothing special: every named function already has its type written, and an arrow can't call itself.

*Why:* When AI writes most code, writing costs little and reading is what matters, for people reviewing it and for agents reading other files. A complete signature summarizes a function without its body. A mistake in a body is reported inside that function ("returns `float`, but the signature says `int`") instead of at a caller in another file, where an agent tends to "fix" the wrong place. Swift, Kotlin and well-kept TypeScript settled on the same line: explicit where a function meets the rest of the program, inferred inside. Annotating locals is left out because it's what makes bodies tiring to read. It's cheap to reverse: `fennec fix` can strip written return types again if measurement shows they don't help.

This replaces the earlier rules that return types are inferred, and that recursive functions must declare theirs.

**Built-in types are ordinary structs in the standard library.** `int`, `float`, `str`, `list` and `dict` are structs in `std`, with methods and the interfaces they satisfy written like any other type's. Only machine-level operations, such as adding two machine integers or loading from memory, are intrinsics the compiler provides.
*Why:* There's no second kind of type that only the compiler can make. A user type does everything a built-in one does, by the same means.

**`Buffer[T]` is the one public building block for memory.** It's safe, bounds-checked, growable and reference-counted. It allocates, reads and writes a slot, grows, and says whether it's unique, which is what copy-on-write needs. `list`, `dict` and `str` are written in Fennec on top of it, and any program can use it the same way.
*Why:* The built-in collections need no machinery a program can't use, so a game's own pool or ring buffer is built from the same safe piece.

**A function is global only when it works on any sequence using nothing but looping and operators; anything that asks a value about itself is a method.**
- The globals are `sum`, `min`, `max`, `any`, `all`, `sorted`, `zip`, `enumerate` and `range`. They use only iteration and a type's operators (`def +`, `def <`), so a user type joins them by defining its operators. There are no hooks and no dunders.
- Everything type-specific is a method, including `xs.len()` and `x.abs()`. `len(xs)` and `abs(x)` are ordinary "no such function" errors, and the message points to the method.
- `int(...)`, `str(...)` and `float(...)` are the types' constructors, not global functions.
- `copy(x)` is the language's mark for a copy made on purpose. It works on any value and can't be defined by a type.
```fennec
interface Signed:
    def abs(self) -> Self

total = sum(e.health for e in enemies)    # global: only looping and +
distance = (a - b).abs()                  # method: a question about one value
```
*Why:* A looping function belongs to no one type: it works on lists, sets, dicts and generator expressions alike. A question about a value is answered differently by each type, so it lives on the type.

**`sum` needs a start for anything but numbers.** `sum(numbers)` needs nothing, and an empty `list[float]` gives `0.0`. Any other type passes a start: `sum(wallet, Money(cents=0))`. A missing start, a type with no `def +`, and a start of the wrong type are all compile errors.
*Why:* It keeps the rule that a global uses nothing but looping and operators, with no `zero()` interface.

**No global function serves only one type.** A global has to follow the rule above, or be a type's constructor, like `int(...)`, `float(...)` and `str(...)`. An operation on one type is a method. So a character's code is `"A".code()`, not Python's `ord("A")`, and `ord(c)` is an error whose fix writes `c.code()`.
*Why:* Global functions that only fit one type crowd the namespace, can't be extended by other types, and hide where an operation lives.

**The standard library uses only what any program can.** Its types get no powers user types lack: `str`'s `*` is a `def *` written in Fennec, like any user type's operator, and its methods are ordinary methods. Where something can be written in Fennec, it is.
*Why:* The language's features are tested by the language's own heaviest user, and nothing in the standard library behaves in a way programs can't copy.

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
An extension adds methods only, never fields, so a type's shape stays in its declaration. It works only on the kit's own types, never another kit's or the standard library's; the standard library extends its own types the same way.

**A generic type's extension names its type parameter, and a bound makes it conditional,** as in Swift:
```fennec
extend list[T]:
    def first(self) -> T?: ...

extend list[T: Eq]:
    def count(self, x: T) -> int: ...
    def index(self, x: T) -> int?: ...

extend list[T: Ordered]:
    def sort(mut self): ...
```
`count` exists only on lists whose elements can be compared with `==`, and `sort` only on lists whose elements can be ordered. Two methods with the same name on one type are an error, wherever they're written. The editor, `fennec doc` and `fennec context` list every method of a type in one place.
*Why:* A type that many responsibilities attach to, like a parser, an interpreter or a game's `World`, is otherwise one huge file, or its methods become free functions with noisier calls. Swift, C#, Kotlin and Go all let a type's methods live in several files. Keeping it to the kit's own types means no one can change a type they don't own.

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

**Generics compile the way Swift's do.** Development builds, including the JIT, compile a generic function once and share that code across every type it's used with, passing each type's methods alongside its values. Release builds compile a copy for each type, so every call is direct and can be inlined, then merge copies whose machine code comes out identical. What a program does is the same either way.
*Why:* Development builds compile once per generic function instead of once per type, which keeps them fast to build. Release builds get full speed, without paying in binary size for copies that are identical anyway. This replaces the earlier rule that generics are compiled per type in every build.

**Mixed types are an `enum`; open sets are function values.** A list that holds different types is a list of an enum: each element is inline with a one-byte tag, and `match` compiles to a jump table. When other kits must be able to add kinds, as with plugins, a struct with function fields does it, and a call through a function value is the only run-time dispatch a program can ask for. Generic code shared in development builds is the compiler's choice, and release builds compile it away.

**`Self` in an interface's method means the type that fits it.** `interface Signed: def abs(self) -> Self` says `abs` returns the same type it's called on: a `Money` for a `Money`.

**`Eq`, `Hash`, `Ordered` and `Format` are built-in interfaces,** satisfied automatically wherever the automatic `==`, hashing, ordering and printing apply.

**`=>` is one expression; anything longer is a nested `def`.** There are no multi-line anonymous functions, as in Python. Function types are written `(int, str) -> bool`.

**Functions used as values never change anything around them.** An arrow function, or a nested `def` that's passed, stored or returned, captures the names it reads by value and can't change any of them. Capturing a `mut` name is an error, with a fix: bind it to a new name first.
*Why:* Every closure that escapes is then a pure function of its inputs, safe to run in parallel, cache or replay. It needs no analysis of which closures escape and no marker on parameters that store one, and capturing costs a reference-count bump at most. Handlers return commands instead of changing state, and builtins and comprehensions build up values.

**A nested `def` that's only called directly can change the enclosing function's `mut` names.**
```fennec
def collect_leaves(tree: Tree) -> list[Tree]:
    mut leaves = []
    def visit(node: Tree):
        if node.children.len() == 0:
            leaves.append(node)
        for child in node.children:
            visit(child)
    visit(tree)
    leaves
```
*Why:* This is Python's most common closure pattern, and it needs no `nonlocal`. Whether a name is only ever called directly is trivial to check, and such a helper works on the enclosing function's variables in place, at no cost. `parallel` bodies still can't change outer names, including through helpers.

This replaces the earlier rule that closures which don't escape may change captured `mut` names.

## Lifetimes

**A lifetime is a block.** `with` is how ownership and lifetime are spelled. Any exit closes the block, inner blocks first, and you can see the order in the indentation.
```fennec
with db.transaction() as mut tx, fs.open(path)? as file:
    tx.insert(parse(file)?)?
    tx.commit()?
```
A long-lived thing, like a file opened in one function and closed in another, is an owned value that moves, and closes when its last owner goes away.
*Why:* If everything temporary lives inside a block, there's nothing for lifetime annotations to describe.

**`with` is only for real lifecycles:** things that begin and have to end, like files, transactions, locks and tasks. A `with` block's last line is its value.
*Why:* Every `with` then means the same thing, so a reader knows something gets closed at the end of the block. Claims about a function, like `@hot`, go on the function.

**Names don't leak out of blocks, and reusing a name still in scope is a compile error.** There's no `const` or `let`: `a = 5` is immutable and `mut a = 5` isn't.
*Why:* A silently shadowed name is a classic bug for people and AI, and picking another name costs little.

**`with` runs cleanup on every way out of its block:** the end, `return`, `break` and `?`. There's no unwinding.

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

**A module is in `std` if most programs need it, it's security-sensitive, or it has to be consistent everywhere.** Everything else is a kit. Only reachable code gets compiled, so unused `std` costs nothing.

**`std.net` holds TCP, UDP, TLS, an HTTP client and server, and WebSocket, all through the `Net` capability.** The HTTP server is the plain kind, a function that handles requests; routing frameworks stay kits. Its design takes lessons from Go's `net` and `net/http`, in Fennec's own code. QUIC comes later as a first-party kit.

**`std.sql` is one standard database interface,** modeled on the ideas of Python's DB-API. Drivers are first-party kits, with Postgres and MySQL in pure Fennec.

**`Tree` is in `std`.** It's built on `IdMap` and keeps parents and children in sync.
*Why:* Hierarchies are where `Id` bookkeeping is most tedious and easiest to get wrong, so it's written once.

**`std.process`, `std.crypto`, `std.log`, `std.cli` and `std.csv` are in `std` too.**

**`std.crypto` holds hashing, including BLAKE3, so there's no separate `std.hash`.**

**First-party kits, not `std`:** `fennec/ml`, database drivers, windowing and input for games, and image formats.

**Names live in their modules.** A standard library module provides nothing until it's imported: `from std.math import Vec3, sqrt`, or `from std import math` and then `math.Vec3` and `math.sqrt(x)`. That's every module so far: `math`, `game`, `layout`, `path`, `json`, `toml`, `collections`, `random`, `time` and `re`. Using a name without its import is reported with the import to add.
*Why:* A file's imports show everything it uses from the standard library, as in Python.

**What the language itself needs is available everywhere.** Capabilities, `Stdout`, `Fs`, `Clock`, `Args` and `Stdin`, which `main` names to say what the program may do. The errors that standard operations fail with, `IoError`, `InvalidValue` and `ParseError`, so code can match them without knowing which module failed. `Ok` and `Err`, which wrap any result. And the global functions, like `sum`, `range` and `sorted`.
*Why:* These belong to the language, not to one module. A capability in particular can't come from an import: it's handed to `main`, and only what `main` is given exists.

**Network access is a capability, `Net`, like `Fs`.** Its API, for HTTP, sockets and limiting it to certain hosts, is decided when it's built.

**Environment variables and running other programs are capabilities too, `Env` and `Process`,** handed to `main`. `std.process` goes through `Process`.

**Command-line arguments and standard input are capabilities.** `def main(stdout: Stdout, args: Args, stdin: Stdin):`. `for arg in args:` gives the arguments, not the program's name, which is `args.program`. `stdin.lines()` reads input a line at a time. A program that doesn't name them can't read them, and both are recorded for replay.
*Why:* A program's input decides what it does, so it comes in the same visible way as files and the clock.

**`int(text)` and `float(text)` fail with `InvalidValue`** when the text isn't a number or doesn't fit, handled with `?`, `else` or `match`. `int(3.7)` still truncates to `3`. `int()` of NaN, or of a float too big for an int, stops the program, like overflow.
*Why:* Text often comes from input, so a bad number is expected and must be handled. A NaN reaching `int()` is a bug in the program.

**A string's characters are reached by code and as bytes without copying.** `"A".code()` gives `65`, `str.from_code(65)` gives `"A"` and fails with `InvalidValue` for a number that isn't a character, and `s.codes()` gives every code. `.code()` on anything but one character stops the program, like an index out of range. `s.encode()` is a view of the UTF-8 bytes, with no copy, so `s.encode().len()` is the length in bytes.
*Why:* A lexer needs character codes and byte lengths, and strings are stored as UTF-8 already, so the bytes cost nothing.

**`bytes` is binary data, a run of ints from 0 to 255.** It's a built-in type, like `str`: `.len()` gives its length, indexing and looping give ints, slicing and `+` give `bytes`, and it compares, orders and hashes. `bytes([137, 80])` makes some from a list, and a number outside 0 to 255 stops the program, like an index out of range. `text.encode()` gives text's UTF-8 bytes, and `data.decode()` gives the text back, failing with `InvalidValue` when the bytes aren't UTF-8. Bytes print as `bytes([137, 80])`, which is also how to write them; there's no literal like Python's `b"..."` yet.
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
- An alias can't take a name the language provides everywhere, such as `sum`, `Ok` or `Stdout`.
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

**`pub` marks what other kits can use, and goes on top-level things only.** It goes on each `def`, `struct`, `shared`, `enum`, `error`, `interface`, `type` alias and re-exported `import` that other kits may use. There's no field-level `pub`.
- `_name` hides a field from everything but its type, in its own kit too.
- A `pub struct`'s other fields are its API. If it has no `_name` fields, other kits get its automatic constructor. Otherwise they build it through its `pub` named constructors.
- Enum cases and interface methods come with their type and have no `pub` of their own.
- Writes from another kit still run the field's `where` rules.
- A `pub` signature can't mention anything that isn't `pub`.
- A lint flags `pub` items that no other kit can reach.

**A kit's API is checked against its last published version,** cached from the Den. When a change would break other kits, `fennec check` says so at once, in the editor: "`load` can now also fail with `ParseError` (since v1.2.0). This is a breaking change: handle or wrap it, or release v2.0." `fennec publish` refuses a version number that undersells the change. Adding an error case is a breaking change; removing one isn't.

A big program that wants walls between its own parts splits into local kits in one workspace.
*Why:* One rule covers every field, and `pub` appears only where API stability matters, never in apps. Go's capitalization rule would clash with lowercase built-ins and capitalized user types.

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

**`shared Name:` is the only reference type.** It's a counted handle whose methods lock automatically: `mut self` writes and `self` reads.
```fennec
shared Cache:
    _pages = dict[str, Page]()

    def get(self, key: str) -> Page?:
        return self._pages.get(key)

    def set(mut self, key: str, page: Page):
        self._pages[key] = page

with a, b as mut x, mut y:
    x.total -= amount
    y.total += amount
```
- `a = cache` gives a second name for the same cell. Fields are private with `_name`.
- It holds plain values and `Id`s, never another `shared`, so handles can't form a cycle. There's no `weak` and no cycle collector.
- `with a, b as mut x, mut y:` takes several at once, in a fixed order.
- There's no module-level `shared`. A handle in a signature acts like a capability, so what a function can reach is in its parameters.

*Why:* Some things really are shared, like a cache or a server's stats. Holding the lock for every access rules out data races, and a method call can't forget to take it. Plain structs pay nothing for it, and a search for `shared` finds every shared resource in a program.

**Lock bodies are short and can't deadlock.**
- They do no I/O, including in functions they call, and can't use `?` or `fail`.
- Locks never nest out of order.
- `try_with` exists but isn't the default.

*Why:* I/O under a lock is the usual cause of stalls, and leaving a lock early is how state gets left half changed. A "busy" error from a try-lock passes single-threaded tests and then fails under load, so waiting is what the short form does.

**`with launch(physics_loop) as physics:` runs a long-lived task.** `launch` creates a channel, passes one end to the function, runs it, and hands back the other end. The block's end stops it.
```fennec
def physics_loop(inbox: Channel[Cmd]):
    mut sim = Sim()
    while sim.running:
        for cmd in inbox.pending():
            apply(mut sim, cmd)
        step(mut sim)

with launch(physics_loop) as physics:
    physics.send(Cmd.Push(id, force))
```
There's no `with tasks()`.
*Why:* One word, no setup, and no way to leak a task.

**Channels are bounded ring buffers.** They have `send`, `pending()`, which takes what's waiting without blocking, and `for x in ch`, which waits. What a full channel does is set when it's made, with `when_full=`: `.Wait`, the default, `.DropOldest` or `.Fail`.
```fennec
commands = Channel[Cmd](size=1024)
latest = Channel[Snapshot](size=1, when_full=.DropOldest)
```
*Why:* A bounded buffer pushes back instead of growing without limit, and a one-slot channel is how a renderer always gets the newest frame.

**A long-running loop should never wait on another.** It takes what's waiting, does its work and publishes. Waiting belongs to simple consumers at the end of a one-way chain.
*Why:* Deadlock needs someone waiting, so two loops that never wait can't deadlock.

**There's one scheduler, with I/O multiplexed by the OS.** Code looks blocking, and a task that waits parks and frees its core.
*Why:* Thousands of parked tasks cost almost nothing, and nobody has to manage a pool or color functions `async`.

**A failure in a server task is meant to stay in that task.** That's recorded as the intent, and it's designed when servers are built.
*Why:* One bad index in one request shouldn't drop every connection. Tasks don't share state a failure could corrupt, which makes containment unusually safe here.

**Reference counts are biased for values that cross tasks.** The thread that owns a block counts cheaply, and other threads count atomically. Programs with no tasks compile the check away.
*Why:* This resolves a conflict between two earlier rules: tasks read parent values without touching counts, and a function returning a slice bumps a count. A task calling that function would have raced on a non-atomic count. Plain reads still touch no counts, and a bump from a task is now safe.

**Tasks are scoped.** Every task ends before the `with` block or `parallel(...)` call that started it. There are no detached threads.
*Why:* Because the parent outlives its tasks, a task can read the parent's values through views without touching reference counts. There's no `Send` or `Sync` to write.

**`parallel` takes only collections and streams,** with no new keyword and an optional `limit=`:
```fennec
for mut particle in parallel(particles):
    particle.position += particle.velocity * dt

total = sum(e.mass for e in parallel(enemies))
hits = [check(b) for b in parallel(bullets, limit=8)]

with net.listen(8080) as server:
    for conn in parallel(server):
        handle(conn, app)
```
- Each iteration can change only its own element. It can read anything around it, but can't change outer `mut` names.
- A body may use capabilities and `shared` handles. Results are deterministic when it uses neither. This replaces the earlier rule that a body can't use capabilities.
- Work over data runs on a work-stealing thread pool, and reading outer values changes no reference counts.
- The interpreter runs the loop in order, which is a correct implementation of a deterministic body, and the playground needs no threads.

*Why:* One word covers data across cores and connections from a server, and everything in it ends when its loop ends.

**Tasks change only separate pieces.** `for mut e in parallel(enemies):` gives each iteration `mut` access to its own element, checked by the same rule that makes `mut` exclusive. Results combine through reductions, like `sum(... for e in parallel(xs))`.

**A value crosses a channel like it goes anywhere else.** It's moved at its last use, and otherwise costs one count bump. Any write copies first, so neither side sees the other's changes. `shared` handles can cross too. This replaces the earlier rule that only values with a single owner cross a channel.
*Why:* Biased counts make it safe, so the restriction no longer buys anything.

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

**A generator is a lazy stream, used once.** A function with `yield` and `(x for x in xs)` both make an `Iterator[T]`, which gives one item each time it's asked and can go on forever. Nothing runs until the first item is asked for. Its opening lines included.
```fennec
def naturals() -> Iterator[int]:
    mut n = 0
    while True:
        yield n
        n += 1

lines = (line.strip() for line in source)
rows = (parse(line) for line in lines if line != "")
total = sum(row.amount for row in rows)
```
- It's a handle: `b = a` is two names for one generator. A second use is a compile error where that can be proved. Otherwise it fails at run time with `this generator has already been used`, never quietly giving nothing.
- A chain of stages is fused into one loop where possible. `map` and `filter` can come later.
- It isn't a list: `gen()[0]` and `gen().len()` are compile errors. `list(gen())` keeps the items.
- What it sees is fixed when it's made, as with a nested `def`. `(x * k for x in xs)` works out `xs` at once and copies `k`.
- Written right inside `sum`, `min`, `max`, `sorted`, `list`, `set`, `dict`, `any` or `all`, it runs in place, so it can change the names around it.
- `yield` is allowed inside `with`. A generator left partway closes what it holds when its owner goes away.
- A failure comes out when the item that fails is asked for, and acts like a failure from a call: the loop exits, `with` blocks close, and it passes up. A generator that wants to go on past bad items yields errors as values. Getting one that can fail takes `?`, as in `for x in checked()?:`.
- Printing one shows `<Iterator[int]>` and doesn't use it.

A stage used once becomes part of what uses it. A stage kept in a name that doesn't change moves to where it's used, leaving behind what it works out when it's made, and a `for` over a stage becomes a plain loop, with a yield function's body copied in. The order things happen in stays the same, so this is purely about speed. Anything that can't fuse, like a stage that's passed around, still works, by asking for each item.

*Why:* Streaming lets a chain of stages read like the steps it is, handles input too big to hold, and can compile to a single loop with nothing allocated per item. Being used once is what keeps that honest: a second loop quietly finding nothing is Python's trap.

**Raw strings are `r"..."`.** Backslashes stay as written, so a pattern reads `r"\d+"`. A raw string can't contain `"`.
*Why:* Regular expressions are unreadable with every backslash doubled. Python has the same strings, so people and AI already write them.

**Doc comments are `##`, above what they document.** A `##` comment documents the function, struct, field or enum case that follows it, or the module when it's at the top of the file. Examples go in Markdown code fences and check results with `assert`. `fennec test` runs them. There are no `"""` docstrings: strings are never documentation.
*Why:* One form that works on everything, including fields and enum cases. Fences render properly in `fennec doc`. Nim uses `##` for doc comments too.

**`case` takes or-patterns and guards, as in Python.**
```fennec
match tok:
    case .Int(_) | .Float(_): "a number"
    case .Name(word) if is_declaration(word): self.declaration()
    case .Name(word): self.expression()
```
- Every alternative of an or-pattern binds the same names, with the same types.
- A guard is read-only: it can't take `mut` arguments, so it can never change anything.
- A guarded arm never counts toward covering every case, so the exhaustiveness check stays sound: a `match` still needs an unguarded arm for each case, or it doesn't compile. Arms are tried in order.

*Why:* An or-pattern says once what several arms would repeat, and a guard keeps a condition in the arm it belongs to. Neither costs anything at run time.

**`"  " * depth` repeats a string.** It's `str`'s own `def *`.

**Keywords can't be names, including field names.** A field holding a function is `function`, not `def`.
*Why:* Python doesn't allow it either, and a name that's sometimes a keyword is harder to read.

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

**Each Fennec version is one self-contained `fennec` binary.** It holds `run`, `build`, `fmt`, `test`, the version launcher, and Den search and install, with Cranelift and LLVM linked into it. It needs no system libraries and no separate LLVM download.
*Why:* Installing Fennec is copying one file, the way Zig works. Nothing on the machine can be missing or the wrong version.

**The binary is also the version launcher.** It reads the version a kit asks for, `fennec = "0.4.2"` in `kit.toml`, with no separate version file. If that isn't the running version, it fetches exactly that version's binary into `~/.fennec/toolchains/0.4.2/` on first use, verifies its checksum and signature, records its hash in `kit.lock`, and hands off to it.
- `fennec toolchain list`, `install`, `remove` and `default` manage toolchains.
- It never updates itself or fetches "latest" without being asked.
- Single-file scripts outside a kit use the default toolchain.
- `fennec doctor` reports how much disk the toolchains and the cache use, and offers to remove versions no kit uses anymore.

*Why:* Every project gets its own Fennec version without anyone thinking about it, and teammates, CI and AI agents all build with exactly the same toolchain. It's also what makes lints pinned to a kit's Fennec version work.

**Building Fennec itself stays fast.** Only the release packaging of the toolchain links LLVM in, through a build feature CI turns on for each target. Everyday builds of the compiler call a pinned, prebuilt `llc` as a separate program instead.
*Why:* Linking LLVM into every build of the compiler would make working on Fennec as slow as the builds Fennec exists to avoid.

## Tooling

Every tool ships in the `fennec` binary and has no configuration unless a setting is unavoidable.

- `fennec fmt`: one style, no options.
- `fennec lint`: the glave-strict rules on by default. `fennec fix` applies every lint's automatic fix.
- `fennec check`: types, ownership and lints, without running anything.
- `fennec test`: tests, property tests and the examples in doc comments.
- `fennec doc`: documentation from `##` comments.
- `fennec bench`: runs `bench` blocks, as below.
- `fennec explain`: the long explanation of any error or lint.
- `fennec search`: finds functions by type, locally and on the Den. `fennec search "Path -> str"` lists every function with that shape.
- `fennec diff`: a semantic diff. It reports renames with their updated call sites, moved functions, changed bodies and API changes with the version bump they need, instead of lines added and removed. A GitHub Action posts it on every pull request.
- `fennec replay`: replays a recorded run, as described under Determinism.
- The language server: inlay hints for inferred types and error sets, hover docs, go to definition, rename, and lint fixes as code actions (including "make this `pub`"). It reuses the compiler's queries and cache.
- Cost hints: the language server shows each line's copies, allocations and reference-count changes inline, the way it shows inferred types.
- The copies lint, as above.

*Why:* Inferred types and error sets need to be visible to be trusted, and a fixed style plus automatic fixes means AI and people write the same code. Cost hints show what code costs while it's written, not after a profiler finds it.

**A test names the capabilities it needs, like `main`, and gets fakes by default.**
```fennec
test "saves the score"(fs: Fs, clock: Clock):
    save_score(fs, clock, 120)?
    assert fs.read_text("score.txt")? == "120"
```
The fakes are an in-memory `Fs`, a fixed `Clock`, captured `Stdout` and a scripted `Net`. `fennec test --real` uses actual files and the network, on a developer's machine and in their CI. The Den only ever uses fakes.
*Why:* Tests are fast and give the same result every run, and they can't touch the machine unless someone asks. The same signature as `main` means nothing new to learn.

**Benchmarks are `bench "name":` blocks,** written next to the code they measure, like `test` blocks, and left out of release builds.
```fennec
bench "collide 10k bullets":
    world = World.with_bullets(10_000)
    world.collide()
```
`fennec bench` runs them and reports instruction counts and median times, compared against the previous run. It reuses the machinery CI already uses to benchmark every change.
*Why:* A benchmark next to its code gets kept up to date, and instruction counts are steady enough to compare between runs even on a noisy machine.

**The format depends only on the code.** Two files with the same syntax tree format to the same text, however they were laid out, so people and AI write identical code. `docs/formatting.md` has the details.
- Lines are at most 100 characters. A bracketed list that doesn't fit puts one item per line, with a trailing comma; a list of plain literals fills its lines instead. A comma after the last item doesn't keep a list broken.
- A line with nothing to break is wrapped in parentheses and broken before its loosest operators, so 100 is a real limit.
- Indentation is 4 spaces. There's one blank line around top-level declarations and methods, and at most one anywhere else.
- A body goes on its own lines, `case` arms included. The exception is a guard clause that fits: `if hp <= 0: return`, an `if` with no `else` whose body is one `return`, `fail`, `break` or `continue`.
- Imports are sorted: `std` first, then other kits, then the kit's own modules.
- A trailing comment has two spaces before its `#`, and a comment one space after it. Line endings are always LF.

*Why:* Formatting is never worth discussing, and output that depends on how code was typed isn't one style. Wide enough for real game code and two panes side by side, with breaks that keep a diff to the lines that changed.

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

- A `##` doc comment of at least one line on every `pub` item, enforced by the linter, which every build runs.
- Descriptive names. One-letter and abbreviated names are flagged, except a short allowlist: `i`, `j`, `k`, `n`, `x`, `y`, `z`, `dt`, `id`.
- Limits on nesting depth and function length, as lints rather than language rules.

*Why:* A one-line doc comment on every `pub` item costs little and tells people and AI what each piece of the API is for. Code that reads well is code AI writes well.

**Runnable examples in doc comments are optional.** `fennec test` runs them, and `fennec doc`, `fennec context` and the Den show them. Whether they become required is left to the eval of what helps AI write Fennec (#76). This replaces the earlier rule that every `pub` function needs a tested example.

## Kits on the Den

**The Den lives at `fennec-lang.dev/den`,** on the site's one domain. It's static pages plus one Cloudflare Worker and SQLite (D1), with kits in R2. Kit READMEs become sanitized static HTML with no user scripts. This replaces `den.fennec-lang.dev`.
*Why:* One domain and one Worker are the smallest thing that works, and a README can't run code in a visitor's browser.

**Publishing requires** that the kit is formatted, passes the default lints, passes its tests and doc examples, and meets the rules under Readable by default. `fennec publish` refuses otherwise. Each lint exception needs a written reason, and the reasons are shown on the kit's page.

**Shown, not required:** test coverage, mutation score and benchmarks, as scores on the kit's page.
*Why:* A required coverage percentage gets gamed with tests that check nothing. A visible mutation score exposes that.

**In v1, `fennec publish` uploads the source with its signed hash.** The browser opens, the author taps Touch ID or Face ID, and the CLI uploads the kit's source. The Den stores it in R2, keyed by its BLAKE3 hash. There are never prebuilt binaries. This replaces OIDC trusted publishing, which assumed GitHub Actions.
*Why:* It needs no phone app and no git host, so publishing works from day one.

**Later, a publish becomes a signed statement naming a commit.** "Kit `owner/name` version 1.2.0 is commit `H` at this URL", which the Den fetches from whatever git host it's on and checks against `H`. v1 defers this.
*Why:* Then what anyone reads in the repository is exactly what runs. The xz backdoor hid in a release archive that didn't match its source.

**The Den keeps every kit's source permanently.** Installing never touches the git host. Versions are immutable: a bad one can be yanked, never deleted or replaced. Because everything is keyed by its hash, anyone can mirror the Den, and the hashes in `kit.lock` prove a mirror served the real thing.

**The Den checks for itself.** It re-runs formatting and lints, and runs the kit's tests, in the interpreter compiled to wasm, with fake capabilities and a limit on how much work a run may do. Kits have no build scripts, so nothing else ever runs. Native kits, which in v1 only the Fennec project can publish, also run in real CI under sanitizers. The kit's page shows a diff of its capabilities and API against the previous version.

**An existing AI model reviews each release's diff,** and its findings are shown. There's no custom fine-tuned model.

**Kits have three tiers, so the audit scales with what a kit can touch.**
- **Pure** (no capabilities, no `extern`): automatic.
- **Capability** (`Fs`, `Net` and so on): automatic plus AI review, with capabilities shown on the page. A newly added capability needs the author's own passkey tap at publish. Consumers' approval of an update that asks for more is unchanged.
- **Native** (`extern`, C code): human audit, first-party kits only in v1. Verified third-party authors are the later path, if there's a real need.

*Why:* Most kits can't touch anything, so they need no human, and the people's time goes to the few kits that could do harm.

**Deferred until needed:** the phone approval web app, delegated publish grants for automation, a public transparency log, a fine-tuned security model, Sigstore signing, and running dependents' tests before a release.

**One known gap until the transparency log exists:** a hacked Den could swap an author's key undetected. The log should be built before the Den gets popular.

**Each version has a size cap,** around 10 MB of source, and publishing is rate-limited.

**Later, breaking dependents is caught before release.** v1 defers this. Before a new version is published, the Den runs the tests of the kits that depend on it directly against that version, within a compute budget, and reports which ones break.
*Why:* An author learns "this release breaks 14 kits" before releasing it, not from bug reports.

**The Den can be searched by type,** through `fennec search` and its website.

**Lints are pinned to the kit's Fennec version.** A lint added in Fennec 0.5 never blocks a patch release of a kit written for 0.4.

## Security goal

**The goal is near-zero CVEs, and it's public policy.** True zero isn't possible: the compiler, the runtime and the C that can't be ported can all have bugs. What Fennec can do is remove the usual causes:
- **Memory bugs are impossible in safe code.** `unsafe` exists only for C, in extern modules, and that will never change. The standard library's internals are private to the compiler, not a second kind of `unsafe`.
- **C is ported rather than linked,** so less C runs at all.
- **A batteries-included standard library** keeps dependency trees shallow.
- **Capabilities limit the blast radius** of any bug: code can reach only what it was handed.
- **Typed standard library APIs** stop injection.
- **The runtime is small, in-house and fuzzed continuously.**
- **`fennec audit` is backed by a public advisory database.**
- **Programs ship as one static binary in an empty container,** so there's no OS image full of other people's CVEs.

*Why:* Most CVEs in the languages people use today come from memory errors, deep dependency trees, injection and base images. Each item removes one of those. `SECURITY.md` states the same policy publicly.

## Supply chain

**Anything that could run code you didn't read is either impossible or sandboxed.** Every rule below follows from that.

**Installing a kit never runs anything.** There are no install or build scripts.

**An update that asks for more capabilities stops until you approve it.** `kit.lock` records the capabilities each dependency receives. A logging kit that starts asking for `Net` doesn't install until you say so. The same goes for an update that adds `unsafe` code.
*Why:* A trusted package turning malicious in an update is the attack that hit event-stream and ua-parser-js.

**Capabilities can be narrowed:** `fs.only("saves/")`, `net.only("api.example.com")`. A kit gets only the part it needs.

**Every kit has an owner's namespace,** `owner/kit`. The Den rejects names a small edit away from popular ones. There's one public registry, and private kits live in their own namespace.
*Why:* That stops typosquatting and dependency confusion.

**Den accounts sign in with passkeys only.** There are no passwords and no recovery by email.
- At least one passkey is required; adding a second is encouraged, not required.
- One-time recovery codes are issued at signup, and you have to save them.
- If both your passkeys and your codes are lost, the last resort is a manual review by the Den team.
- Email is only for notices, such as a new passkey added, a new release or a change of ownership. It's never used to sign in or recover an account.
- Publishing still needs the signed hash, as above. Sigstore signing is deferred.

*Why:* Stolen passwords and hijacked email are how package accounts get taken over. With no password and no email reset, there's nothing for an attacker to phish or reset.

**`fennec audit` is built in.** It checks every dependency against the advisory database, and CI fails on a known vulnerability.

**New versions wait a week** before `fennec update` picks them up (already decided), and versions are chosen by minimal version selection.

## Engines

**Development runs use the Cranelift JIT, with no tiering.** Each function is compiled on its first call, so only code that runs gets compiled. There's no bytecode interpreter that promotes hot functions to the JIT.
*Why:* Lazy compilation takes microseconds per function and the cache makes reruns instant, so a second fast engine would buy almost nothing. Tiering means two engines that must always agree, plus falling back from compiled code to the interpreter, which is the hardest part of the runtimes that do it.

**The tree-walking interpreter stays simple and correct, not fast.** It runs the REPL, the playground, compile-time evaluation and the sandbox, and it's the reference the JIT is tested against: every example must give the same output on both.

**`fennec build` makes an executable with Cranelift.** It's the fast build, and the path the self-hosted compiler bootstraps through. `fennec build --release` uses the bundled LLVM at full optimization for small, stripped, portable executables.

**The JIT aims for 80% of the speed of the benchmark suite's twins to start, then 85%, and no further.** Closing the rest is the release build's job. Optimizations that would slow the JIT's compiling to win the last few percent aren't made.
*Why:* Development builds are for changing code and seeing it run at once. The 110% target applies to release builds.

The JIT, release builds and the language server all share one build engine and cache, described under Builds.

## Builds

**The unit of work is the function, keyed by its content.** A function's key is a hash of its own syntax tree plus the signatures of what it uses, not their bodies. A caller depends on a callee's body only if it inlined that callee. A change inside a function that leaves its signature alone recompiles nothing else.

**Results are pure.** Cache keys never include paths, timestamps or environment variables, and every key includes the compiler version. Builds are deterministic, so each function of a kit compiles once per machine, ever.

**There's one cache per machine,** `~/.fennec/cache`. It has a size cap, and old entries are cleaned up. Entries are written to a temporary file and renamed into place, so readers never see half an entry and the normal path takes no locks; a lock is taken only during cleanup.
- There's no build folder inside each project. Development runs compile straight from the cache with the JIT, and only `fennec build --release` writes an executable.

**Work runs in parallel on every core,** one function at a time per core.

**One engine stays running.** A background daemon serves the compiler, checker, formatter and language server from the same queries. It hot-reloads changed functions into a running program.

**Caches can be shared** between machines and CI. Results from elsewhere are used only when they're signed or verified locally.

**Release builds use the bundled LLVM with whole-program optimization,** cached per function where possible.

*Why:* These avoid what makes builds slow elsewhere: large compilation units, build scripts and macros that run code, a build folder in every project that grows to gigabytes, and separate engines for the compiler and the editor.

**Hashing works in two layers.**
1. A BLAKE3 hash of each file's bytes. An unchanged file isn't even parsed again, and timestamps are never used, so switching branches or cloning in CI doesn't rebuild anything.
2. A hash of each function's syntax tree that leaves out positions and comments. Reformatting, editing comments and moving code around recompile nothing. Positions are kept separately for error messages.

BLAKE3 is written in-house rather than taken as a dependency, and checked against the official test vectors.

## SIMD and GPU

Six primitives cover a game engine's hot loops and ML inference. Everything else, like matrix multiplication, convolution, softmax and attention, is ordinary Fennec code in the first-party `fennec/ml` kit.

**`Simd[T, N]` is the one vector type.** It holds `N` lanes of a number type `T`, where `N` is a power of two up to 64. It's a `std` struct whose operators are machine-level intrinsics, and comparisons give a `Mask[N]`.
```fennec
from std.simd import Simd

a = Simd[f32, 8].splat(1.5)
b = Simd[f32, 8](0, 1, 2, 3, 4, 5, 6, 7)
c = a * b + a                              # lane by lane
bright = c > Simd[f32, 8].splat(4.0)       # Mask[8]
d = bright.select(c, a)
total = sum(d)                             # a Simd is a sequence
```
- Operators work lane by lane wherever `T` has them. Integer lanes wrap, which the type makes visible; scalar ints still check overflow.
- Methods cover the rest: `.select`, `.shuffle[...]` with indices fixed at compile time, `.min`, `.max`, `.abs`, `.sqrt` and `.fma`. A fused multiply-add happens only when written as `.fma`.
- `sum`, `min`, `max`, `any` and `all` reduce lanes in a fixed tree order, so results match on every machine.
- `simd_width[T]` is a compile-time constant for the target's best width, so portable code writes `Simd[f32, simd_width[f32]]`.
- LLVM maps `Simd` onto its vector types. Cranelift runs vectors wider than 128 bits as several 128-bit operations. The interpreter runs lanes one at a time, with the same results.

*Why:* One generic type covers every width and element type without dozens of names, and it reads like math. Zig, Mojo, C# and Swift settled on the same shape. Hand-written intrinsics tie code to one CPU and are unreadable.

**`f16` and `bf16` are number types.** Arithmetic uses the hardware where it exists, and otherwise widens to `f32` and narrows back, with the same results. 8-bit quantization already works with `i8` and `u8`.
*Why:* ML inference is mostly limited by memory, and 16-bit weights halve it.

**`.lanes[N]()` walks a buffer `N` elements at a time.** It's a method on `Buffer[T]`, `list[T]` and `GpuBuffer[T]`, giving `Simd[T, N]` values and a final partial chunk with a mask. `for mut` writes back. Bounds are checked once per chunk, and the tail mask means nothing reads past the end.
```fennec
def scale(mut xs: list[f32], factor: f32):
    k = Simd[f32, 8].splat(factor)
    for mut v in xs.lanes[8]():
        v *= k
```
Plain loops and `parallel(...)` loops are still vectorized automatically where LLVM can. `Simd` is for when you want a guarantee.

**GPU access is a capability, `Gpu`,** passed to `main` like `Fs`. A kit gets the GPU only if it's handed one, and a sandboxed script only if its host grants it.

**`GpuBuffer[T]` is GPU memory.** It holds numbers, `f16`, `bf16`, `Simd` values, or structs made only of those. `gpu.upload(xs)` and `gpu.download(buffer)` move data. Fennec values never point into GPU memory, so the CPU and the GPU can't race over data.

**Kernels are ordinary functions, run with `gpu.run`.** Any function that follows the GPU rules can run on the GPU, and the compiler checks the rules at the `gpu.run` call. It calls the function once for each index from 0 up to `count`, in parallel.
```fennec
from std import gpu

def brighten(i: int, mut pixels: GpuBuffer[f32], amount: f32):
    pixels[i] = min(pixels[i] + amount, 1.0)

def main(stdout: Stdout, gpu: Gpu):
    mut image = gpu.upload(load_pixels())
    gpu.run(brighten, count=image.len(), mut image, 0.2)
    pixels = gpu.download(image)            # waits for the GPU here
```
- A kernel can't allocate, recurse, use capabilities, or use strings, lists or dicts. It works on numbers, `Simd` values, structs of numbers and `GpuBuffer`s.
- Invocation `i` writes only element `i` of each `mut` buffer, and reads anything, the same rule as `for mut x in parallel(xs)`. Kernels can't race, and their results are deterministic. A write anywhere else is a compile error naming the line.
- Buffer indexing is bounds-checked, and range analysis removes the check where the index is provably the invocation's own.
- `gpu.run` queues work and returns, and `gpu.download` waits for what was queued before it.

*Why:* A kernel that's just a function is tested on the CPU with `fennec test`, debugged like any code, and run by the interpreter too. A separate shader language means separate tools.

**Kernels compile through two small in-house backends:** SPIR-V for Vulkan (Windows, Linux, Android) and Metal Shading Language source for Metal (macOS, iOS). No third-party translator is involved. Release builds compile kernels ahead of time and embed them; the JIT compiles each one when it first runs and caches it by content. WGSL for WebGPU can come later. `std.gpu` reaches Vulkan and Metal through `extern`, which only the standard library's OS layer may do.

**GPU floats are the same on the same device, not across vendors.** `sqrt`, `exp` and division can differ in the last bit between GPU makers. `gpu.run(..., exact=True)` turns off contraction and uses Fennec's own math functions for results identical across vendors, at some cost in speed. Lockstep game simulation stays on the CPU.

**Later slices, in order:** workgroup memory with barriers the compiler checks, for tiled kernels; subgroup operations; a `Tile[T, M, N]` type for matrix hardware, through Vulkan's cooperative matrices and Metal's simdgroup matrices; and training, with automatic differentiation as a library in `fennec/ml`. Graphics shaders can reuse the kernel compiler later, through `fennec/gpu`. A tensor type with operator fusion stays out of the language.

## Native code

**If it can be Fennec, it is Fennec.** Libraries are ported to Fennec wherever possible. C is called only for what can't be ported: the operating system's own APIs, GPU drivers, and closed platform or console SDKs.
- **A port is tested against the original:** the C library is the oracle, giving the same outputs for the same inputs, and fuzz tests compare them continuously.
- **A port is benchmarked against the original:** CI fails if it's more than 3% slower.
- **Only permissively licensed code is ported:** MIT, BSD, Apache, zlib or public domain. Anything under the GPL or LGPL is written from scratch without reading the original.

*Why:* Ported code is memory-safe, bound by capabilities, and runs in wasm and the playground. A C binding is none of those.

**C is declared in an `extern "C":` block.** A file with one is an extern module, the only kind of file where pointers and `unsafe` may appear.
```fennec
extern "C":
    struct Sqlite3:
        _opaque: u8

    def sqlite3_open(path: ptr[u8], db: ptr[owned ptr[Sqlite3]?]) -> i32
    def sqlite3_close(db: owned ptr[Sqlite3]) -> i32

pub struct Database:
    handle: owned ptr[Sqlite3]

    def del(self):
        unsafe("handle came from sqlite3_open and is closed only here"):
            sqlite3_close(self.handle)
```

**Every call into C is inside `unsafe("reason")`.** The reason is required and says which rule the code relies on.

**A small, fixed set of types crosses the boundary:** numbers with explicit widths (`i8` to `i64`, `u8` to `u64`, `f32`, `f64`), `bool`, `extern` structs (in C's layout exactly), `ptr[T]` and `CString`, which owns its bytes and ends them with the zero byte C expects. Fennec's `str`, `list` and other structs are converted explicitly. Passing Fennec functions to C as callbacks comes later.

**Pointers are as safe as the compiler can make them.**
- `ptr[T]` is never null. Anything C may return as null is `ptr[T]?`, and the checker makes you handle `None`.
- An `owned ptr` must be used up exactly once, either moved into a struct whose `def del` releases it or passed to a C function that takes `owned`. Dropping one, copying one or losing one on any path is a compile error naming the path.
- A pointer into a Fennec value, such as a `CString`'s, is usable only inside the `unsafe` block where it was taken.
- Reading, writing or offsetting through a pointer happens only inside `unsafe`.
- `pub` signatures never contain pointers, so other files and kits see only a safe API.

**Declarations are checked against the real headers.** When building, every `extern` struct's size and field offsets, and every function's signature, are compared with the C headers, and a mismatch fails the build. `fennec bindgen header.h` writes the `extern` block from the headers in the first place, and the generated file is checked in.
*Why:* A wrong signature corrupts memory silently, and it's the mistake AI-written bindings make most.

**Native code is caught misbehaving early.** Debug builds track every owned pointer and stop on a double free or a use after free. Native kits run their tests under AddressSanitizer, UndefinedBehaviorSanitizer and LeakSanitizer, and the Den fuzzes every `extern` wrapper.

**When the program stops itself,** as on overflow, `def del` still runs for every value holding an `owned ptr`, so C resources are closed. Only a crash inside C skips it.

**No C is called inside `parallel` bodies.**
*Why:* Most C libraries aren't safe to call from several threads at once.

**`fennec audit --unsafe` lists every `unsafe` block in the dependency tree,** with its reason, and the Den shows each kit's count.

**Libraries are named in `kit.toml`:**
```toml
[native]
libraries = ["sqlite3"]
```
The JIT loads them when the program runs, and release builds link them. The C standard library is always available.

**In v1, raw C is allowed only in the standard library's OS layer, first-party `fennec/` kits and your own app.** An app can use `extern` in its own code, which isn't publishing; a game engine needs that for Metal and Vulkan. No third-party native kits go on the Den in v1, even from verified authors, until there's a real need. Verified authors are the later path.
*Why:* Native code is the one place Fennec's guarantees stop, since C can ignore capabilities and corrupt memory.

**There's no C compiled to wasm in v1.** It can be reconsidered later if the ecosystem needs it.

**Every first-party native kit has a plan to become pure Fennec,** with differential tests against the original C.

**An app allows each native kit by name:** `sqlite = { version = "1.2", native = true }`. A native kit's API still takes the capabilities it uses, `sqlite.open(fs, path)`, so its signatures stay honest.

**The interpreter calls only the standard library's own OS layer, never a kit's `extern` functions.** The playground, the sandbox, compile-time evaluation and the REPL can't run native kits; the error points to `fennec run`, which uses the JIT. Native kits' differential tests compare the JIT with release builds. A REPL that switches to the JIT when a native kit is imported can come later.

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

## Recording decisions

**Architecture decision records (ADRs) come at the language freeze.** They're generated from this document and its pull request history, and labeled as reconstructed. After the freeze, a language change means a new ADR.

## License

**The compiler and tools are `MIT OR Apache-2.0`, and the runtime library and standard library add a runtime library exception,** modeled on Swift's. A program compiled with Fennec owes no attribution for the runtime and standard library code built into it. The exception is in `RUNTIME-LIBRARY-EXCEPTION`.
*Why:* Every compiled program carries pieces of the runtime and standard library. Without the exception, every game or tool shipped would have to include Fennec's license notice for code its author never chose to use. Swift and GCC have the same kind of exception for the same reason. The compiler and tools themselves keep the usual dual license, which most open-source compiler tooling uses, so code can move between projects in either direction.

**Contributions are dual licensed the same way,** unless the contributor explicitly says otherwise. The README states it.

## Still open

- How a type marks that its finishing method must be called.
- How replay records the order tasks reach a `shared` value or a channel.
- Decorators, beyond the compiler's own `@hot` and `@fast_math`. If they come, they'd be typed annotations: data attached to a function, read by kits through reflection at compile time in the sandboxed interpreter, never wrappers that change the function. `@get("/todos/{id}")` on a plain function would let a router build its table at compile time.
- SQLite: whether and how Fennec includes it. Porting it isn't realistic.
- Whether independent writes, not just reads, should overlap automatically.
- The exact syntax for property tests.
- The syntax for units of measure.
- The file extension (currently `.fen`).
- Whether files of a kit may reuse a top-level name, reached through `import` by their module's name.
- These security proposals:
  - Types that make injection impossible: `sql"..."` templates, `Path`, argument lists for commands, escaping HTML templates.
  - `Secret[str]`, which can't be printed or logged.
