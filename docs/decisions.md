# Design decisions

**[`language.md`](language.md) is the source of truth for the language. If this file disagrees with it, `language.md` wins.** This file stays for the reasoning and for details that page doesn't cover.

Decisions made while designing Fennec, with the reason for each. The [roadmap](../site/src/pages/roadmap.md) says when each one gets built. Newest decisions go at the top of each section.

## Principles

**Pure style is encouraged, not promised, and `ref` is the guaranteed in-place form.** Comprehensions make new things. A comprehension that replaces its own source should compile to the same in-place loop as `for ref`, when the old value isn't used again, nothing else holds it and the result is no larger. That's a goal for the compiler, not a rule a program can count on.
*Why:* Plain functions that read values and return new ones are the easiest code to read, and `for ref` is there for when in-place has to be guaranteed.

**Built-in types get no powers user types can't have.** They use the same protocols a user type implements:

| What a program writes | What the type defines |
| --- | --- |
| `print(x)`, `str(x)`, `f"{x}"` | `def str(self, ref out: StrWriter)` |
| `a == b`, `a != b` | `def ==` |
| `a + b`, `a < b`, `sum(xs)`, `sorted(xs)` | operators: `def +`, `def <` and the rest |
| `x in xs` | `def in` |
| `len(xs)` | `def len` |
| `abs(x)` | `def abs` |
| `for x in xs`, `list(xs)` | `def iter(self) -> Iterator[T]`, with `yield` |
| `xs[i]`, and writes through it | `def []`, and its `ref` form |
| `for ref x in xs`, `parallel(xs)` | positions: `def len` plus the `ref` form of `def []` |

The only exceptions are the primitives, `int`, `float`, `bool` and the raw `Buffer`; literal syntax, `[...]`, `{...}` and `"..."`; and `std.intrinsics`, which only the standard library can import. Today `list`, `dict` and `str` are still built into the compiler, and making them ordinary `std` structs (#104) is how main gets there.
*Why:* It's the language from first principles. A reader can learn the whole language from its protocols, and anything `std` does, a user type can do.

## Values and memory

**`mut` is a local variable that can change, and `ref` is a link to something else.** `mut` is always your own value, so nothing outside the function sees its changes unless it's returned or handed to a `ref`. `ref` is a loop, a function or a local being handed a place to change, and that includes `ref self`.
```fennec
mut total = 0
mut scores_copy = scores                 # your own copy

def heal(ref unit: Unit):
    unit.health += 10

heal(ref units[unit_id])
for ref unit in units:
    unit.health -= 1
```
- A `ref` needs a `mut` source or another `ref`. So the chain always starts at a `mut` local, and a plain name never changes. `heal(ref make_unit())` is an error, since the change would be lost.
- A `ref` can't be stored, returned or captured by a closure that outlives it, so there are no lifetimes to write.
- A local `ref name = place` links a name to a place for several changes in a row, as in `ref target = units[target_id]`. It lasts until its last use, and never past the end of its block. While it's alive it's the only way to the container it points into, so nothing else reads or changes that container meanwhile. It's checked at compile time and costs nothing at run time. Chris decided this on 2026-11-09. Not built yet: today `ref first = units[0]` is refused with "there's no inline `ref`". This replaces the rule that `ref` is only for loops and parameters, with no inline `ref x = place`.
- During the rename, the old `mut x = place` that made a link is a compile error, so nothing silently changes meaning.

Built: the compiler takes `ref` in these places, and `fennec fix --mut-to-ref` rewrites the old `mut` forms. This replaces the rule that `mut x = place` edits a place, along with `mut a, b = xs.pair(i, j)`.
*Why:* `mut` was doing two opposite jobs with one word. `mut unit = units[unit_id]` was a link and `mut unit = find(units, unit_id)` was a copy, and a write to the second was silently lost. Now a line says which it is.

**The one-line `x if c else y` stays,** as in `kind = "boss" if health > 500 else "minion"`. It's Python's conditional expression, for a value that depends on one condition, and `match` covers more. Chris decided this on 2026-11-09. Built.
*Why:* A name that can't change is set once, by one expression, and this is the form Python people already write for it.

**A loop can change items, but not how many there are.** There are four forms, and `ids()` and `values()` go away:
```fennec
for unit in units: ...
for ref unit in units: ...
for unit_id, unit in units.items(): ...
for unit_id, ref unit in units.items(): ...
```
`enumerate` takes anything you can loop over and gives `(index, value)`, and `for i, ref x in enumerate(xs)` is allowed when `xs` has positions, `len` and `[]`. `ref` over a generator is a compile error. `items()` is for keyed things: a dict, a `Table`, and a user type that gives `len`, `[]` and `key(i)`. Looping over a dict gives its keys, as in Python, and `keys()`, `values()` and `items()` stay. Built, apart from `items()` on a user type.
*Why:* `enumerate` is what Python people expect, a generator's items aren't anywhere to change, and the compiler's own tables need an `Id` and a `ref` in one loop.

Adding and removing happen outside the loop, and so does writing to other items: a loop's collection can't be changed while it's looped over, even in a plain loop. Collect the changes first and apply them after:
```fennec
hits = [(attacker.target_id, attacker.damage) for _, attacker in units.items()]
for target_id, damage in hits:
    units[target_id].health -= damage
```

**Nothing can be reached two ways while it's being changed.** Overlapping `ref`s are refused, including `f(ref x, ref x)`, `f(ref xs[i], ref xs[j])` always, since `i` may equal `j`, and writing to `units` inside `for ref unit in units`. A `ref self` method can't be given what it changes either: `bag.grab(bag.items)` is refused when `grab` changes `items`, and `bag.grow(bag.size)` is fine, since a number is passed as its value. `xs.swap(i, j)` swaps two items, and `pair` is gone. Built, `swap` too. So is an argument that changes what its call changes, like `self.items.append(self.grow())` when `grow` appends to `self.items`, since which change wins would depend on the engine.
```fennec
for ref unit in units:
    units[unit.target_id].health -= 1    # error: the target may be `unit`
```
*Why:* The compiler can't tell whether a unit targets itself, and two names for one slot are the aliasing Fennec leaves out. Collecting the hits first says the same thing with no overlap.

**Everything is a value unless the code says otherwise.** Structs nest freely and are stored in place. Nothing is shared unless the code says `ref`, `Id` or `shared`.
*Why:* A reader can see every place two names reach the same thing, and the compiler keeps flat memory and every optimization everywhere else.

**Fennec has no object graphs. Data lives in tables.** A thing that others point at lives in a `Table` or a list, and they hold its `Id`.
*Why:* Flat tables are what keep memory contiguous, copies cheap and cycles impossible, and they're what game engines and databases already use.

**A copy that can go stale gets a lint suggesting `ref`.** Copying a struct into a longer-lived place, when the copy can diverge from a source still in use, is linted, and the lint asks whether a `ref` was meant:
```fennec
# lint: edits to one won't reach the other; a `ref` changes the original
world.selected = world.units[i]
```
Every copy is already a snapshot, through copy-on-write, so there's no plain `copy(x)` to mark one on purpose. `copy(x, field=...)`, a changed copy as Python's `dataclasses.replace` makes, stays. Immutable local snapshots stay allowed, and so does sharing that never changes. Chris decided this on 2026-11-09. Built: a plain `copy(x)` is an error, and `fennec fix` takes the call off; a loop over `copy(xs)` goes over a name made the line before instead, since a loop can't change what it goes over. The line above is a `stale-copy` hint, which suggests holding the item's position or `Id`, or a local `ref` to change it where it is. A copy made each time round a loop is a hint, and `@hot` still refuses it. This replaces the compile error whose fix was `copy(x)`, and the `needless-copy` lint.
*Why:* This is the mistake people and AI trained on references will make. It compiles and looks right, then one side silently stops updating. A snapshot is what a plain assignment already gives, so the useful question is whether a link was meant.

**A write that goes nowhere is a compile error.** That's a local that's changed and then never read, returned or passed on:
```fennec
def heal(hero: Hero):
    mut healed = hero
    healed.health += 10          # error: `healed` is changed, then never read, returned or passed on
```
- `mut unit = world.units[id]` is your own value, so changing it and stopping there is this error. The fix writes directly: `world.units[id].health -= 1`. A write through a `ref` isn't this case, since it reaches the source.
- It doesn't catch a copy that's read afterwards. Print `healed.health` at the end of `heal` and it compiles, while the caller's `hero` stays as it was.

*Why:* A change that goes nowhere is a bug.

**Cut from the first version, and each can be added later without breaking anything:**
- Helpers that `yield` a place to change. `world.units[id].health -= 10` covers most cases.
- Inferring which fields a helper touches, so the rest of a struct stays usable across the call. Passing `ref world.units` does the job.
- Compile-time checks for channel deadlocks. Detection starts at run time.

*Why:* It's less to build and less to get wrong. If something turns out to be missing, it's added with evidence.

**There are no classes, and that's final.** Graphs and trees are solved in the standard library instead, with `Table`, `Tree` and `Graph`. Anything that hides the table, like `Id`s carrying their map or `with units:` blocks, is ruled out too. This removes the fallback of a `class` with `link` fields.
*Why:* A class wouldn't slow code that avoids it, but it would spread through habit and libraries, bringing pointer-chasing, cycles and harder `parallel` safety.

**Named fallbacks, used only on evidence.** There's no `Frozen[T]` and no `with pool()`.
- A limited `View[T]`, if having no stored views costs more than about 10–15% on parser, zero-copy server and B-tree benchmarks.
- Tighter lock patterns, if a B-tree benchmark fails.
- Running independent writes at the same time on their own, with no `parallel` or `launch` written, if programs show they need it. This was an open question until 2026-11-07, when Chris moved it here.

*Why:* All of them are additions that break nothing later. `Frozen[T]` adds nothing while a plain value can already cross tasks for one count bump.

**Parameters are borrowed by default.** A parameter that's only read costs no reference-count change, on the way in or out. Reference counts are inserted precisely and dropped at each value's last use. A value with a single owner is reused in place to build its replacement. This is the Perceus approach from Koka, with Lean 4's borrowed parameters.
*Why:* Reference-count traffic around calls is most of the overhead in naive implementations. These two remove nearly all of it.

**Indexing gives a place, not a copy.** `enemies[i].health -= 1` writes straight into the list. A user-defined `def []` gets a `ref` form that does the same.
*Why:* Otherwise each change through an index copies the element out and back, or copies the whole list if it's shared.

**`for ref` over a user type writes back through positions.** A type supports it by defining `def len(self) -> int` and the `ref` form of `def []` taking an `int`, which returns the element as a place. It's the one function that may return a `ref`, since indexing, `for ref` and `parallel` all rest on it. A type whose positions can be empty, like a pool with free slots, also defines `def is_live(self, i: int) -> bool`, and empty positions are skipped.
```fennec
struct Units:
    _items = Buffer[Unit]()
    _live = Buffer[bool]()

    def len(self) -> int:
        return len(self._items)

    def is_live(self, i: int) -> bool:
        return self._live[i]

    def [](ref self, i: int) -> Unit:      # the `ref` form: returns a place in `_items`
        return self._items[i]

def step(ref units: Units, dt: float):
    for ref unit in units:
        unit.pos += unit.vel * dt
```
- `for ref unit in units:` means `for i in range(len(units)):`, skipping positions that aren't live, with `unit` as a `ref` to `units[i]` inside.
- `units` stays locked for the whole loop, as any path behind a `ref` is, so the body can't add or remove items or reach other elements. Nothing is copied or counted, and `unit` can't escape.
- Positions are `int`s. A grid maps an `int` position to its own cell.
- `Table`'s positions are its storage slots, and dead ones are skipped. A plain `for` still reads through `def iter(self)` with `yield`.

*Why:* A get-and-set pair, like Python's `__getitem__` and `__setitem__`, copies each element out and back, and with copy-on-write an element that holds a collection would then copy on every iteration. A `yield` that lends a place stays cut: a lent place would have to stay locked across each suspension of a generator, and it can't be split for `parallel`. Positions add no new concept, never copy, and are what `parallel` splits by. Swift's `MutableCollection` works the same way, through an index and a subscript that writes in place.

**A collection can't change size while it's being looped over, or be changed another way while a `ref` reaches it.** Inside `for x in xs:` or `for ref x in xs:`, adding or removing items is a compile error naming the loop's line. Inside `for ref x in xs:` or the life of any `ref` into `xs`, changing `xs` by any other route is one too. This is the one exclusivity rule behind the existing errors for `for ref` and `parallel`, like Swift's law of exclusivity, and it needs no annotations.
```fennec
for enemy in enemies:
    if enemy.splits:
        enemies.append(spawn_child(enemy))      # error
```
```
error: `enemies` is changed while it's being looped over
  --> game.fen:3:9
   |
1  | for enemy in enemies:
   |              ------- the loop over `enemies` starts here
3  |         enemies.append(spawn_child(enemy))
   |         ^^^^^^^ changed here
   = fix: loop over a snapshot, `snapshot = enemies`, then `for enemy in snapshot:`
   = fix: collect the changes in a list and apply them after the loop
```
Neither fix is applied by `fennec fix`, since each changes what the program does. This replaces any reading where a loop silently works on a snapshot.
*Why:* A loop that quietly sees the old collection, or skips and repeats items as it changes, is a classic bug in every language with references. One rule covers reading loops, writing loops and `parallel`, and the fix says which behavior was meant.

**A value is moved, not shared, when it isn't used again.** The compiler finds each value's last use and moves it there.
*Why:* Then nearly every collection has one owner, so copy-on-write's check before each write passes and nothing is copied. The check is a predictable branch the compiler can hoist out of loops, and deletes entirely when ownership proves the value has one owner.

**Views can't escape.** A `view[T]` (a slice of a list) can be a parameter or a local, but can't be stored in a struct or returned. A function that returns a slice returns a shared slice: one reference-count bump, no copy, the way string slices already work. Stored views aren't added: a slice that's kept is a counted value or an inline short string.
*Why:* Slicing without copies, and without lifetime annotations.

**Copies are visible.** A lint lists every place the compiler had to copy a value, and why: "copied `enemies` because it's used again on line 42".
*Why:* A hidden copy in a game loop is a frame-time spike nobody can find.

**A copy that would happen on every iteration of a loop is a compile error,** including one in a function the loop calls. The error names the line and why the value is shared: "`state` is copied every iteration because `history` keeps it". The fix is to drop the other holder or restructure.
- A one-time copy inside a loop, where the first write copies and the value has one owner after that, is only a hint, and the compiler may move the copy before the loop.
- `@hot` still forbids any copy at all.

*Why:* In a values language every implicit copy happens at a known line, at most once per sharing. So a loop that copies on every iteration is either a bug or should say `copy` on purpose, and a frame loop can never quietly copy.

**`Table` and `Id[T]` are the way to point at things.** `Table` was called `IdMap`, in `std.game`, until 2026-11-05; both are in `std.collections` now. An `Id[Enemy]` is an index, a generation and a tag for its map, made by a `Table[Enemy]`. It can't index a map of another type, and the tag means an ID from another map can't find the wrong item.
- `map[id]` stops the program on a dead ID, like an index out of range. `map.get(id)` returns an optional, for when the item may be gone.
- `map.update(id, value) -> bool` puts a new value at a live ID and says whether it was there, for when the item may be gone. Chris decided this on 2026-11-09. Built for `Table`.
- A map reports how many items are live, which makes a forgotten item visible.
- Its storage is in chunks, so one write after a snapshot copies a chunk rather than the whole map.
- A copy of a `Table` keeps its tag, so an `Id` made before the copy finds its item in both. Generations are unique across copies, so an item added to one copy has an `Id` that finds nothing in the other. Chris decided this. Not built yet: today an item added to each copy can get the same `Id`, so one copy's `Id` finds the other's item.
- `type EnemyId = Id[Enemy]` is an optional alias, not something the compiler makes for each struct.

*Why:* Game engines, compilers, graph libraries and servers use the same generational-handle pattern. IDs are small plain values, so they're cheap to copy and can be saved or sent anywhere. A cycle of them is just numbers, with nothing counted, so it can't leak. A stale ID that silently became `None` would hide bugs, so the short form is the loud one.

**An `Id` crosses the network as a plain compact value, and there's no `NetId` type.** A client keeps its own table from a server's `Id` to its local entity. An `Id` is never a secret or a permission: a server authorizes every action, even if every `Id` is known.

**An `Id` is local to its process's `Table`, and across servers a program sends its own key.** A key is a value the program chose, like a player's name or a number it hands out, and each server finds its own `Id` from it, the way a client keeps a table from a server's `Id`. Built, in the sense that matters: each `Table` carries a tag that's never reused, so an `Id` from another `Table`, in this process or any other, finds nothing. This settles how an `Id` is used across servers. Chris decided this on 2026-11-07.
*Why:* An `Id` is an index and a generation into one process's memory, so it means nothing to another server. A key the program owns says what it means, survives restarts, and needs nothing new in the language.

## Collections

**A struct works as a dict key with nothing to write.** It hashes all its fields automatically. A struct that defines its own `def ==` can't be a key, and the compile error suggests keying by a field. There's no `def key`. `==` and `!=` call a type's own `def ==` when it has one; today they don't yet, which is a bug.
*Why:* A hash can't then disagree with `==`, which is the classic bug in hand-written keys. Float fields are fine, because float equality and hashing agree.

**Dicts use a fast keyed hash with a random seed per process,** fixed in tests and replays. A dict whose probes get suspiciously long rehashes itself with SipHash.
*Why:* Normal programs get the fast hash. An attacker can't work out colliding keys without the seed, and if collisions turn up anyway, that one dict pays for the stronger hash. Iteration follows insertion order, so the seed can't change what a program does.

**Collections are values.** `b = a` or passing a `list`, `dict`, `set` or `str` gives a value that never changes under you, like every other value. `mut b = a` gives one that can change separately.

**A `list`, `dict` or `Buffer` is one word: the address of a reference-counted block.** The block holds the length, the capacity and the elements together. Copying bumps the count. The block is copied only on the first write while it's shared, which last-use moves make rare. Elements are stored inline, so a `list[Vec3]` is one contiguous run of floats.

**A `str` is a 24-byte header held inline.** The header points at a reference-counted block, or holds a short string itself.

**This layout is hidden from programs.** No program can see it, so it can change if benchmarks ever argue for it.
*Why:* One word keeps structs that hold collections small and copies cheap, and it's what the backends build on. Strings are worth the extra words because most are short enough to need no allocation.

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

**The compiler chooses the layout.** It can reorder fields, pack enums, and in a hot loop store a list of structs as one array per field. None of this can change what a program does, because nothing can observe where a value sits in memory. Printing and `{x=}` always show the struct as written.

## Allocation

**The compiler and runtime choose allocation, and code never names an allocator.** mimalloc is the invisible last step, for what the compiler can't place itself. It's vendored (it's MIT-licensed) and later ported to Fennec. It keeps per-thread lists of free blocks for each size, so allocating and freeing take a few instructions.

**The compiler places short-lived values itself.** A temporary that never leaves its function or loop body goes on the stack or in a bump region, with no call to the allocator. This is limited to values that provably don't escape. Full automatic region inference is known to leak memory.

**`@hot` on a function means prove it or don't compile.** The compiler must prove the function and everything it calls does no allocation, makes no copies and has no leftover count checks, or it's a compile error that names the line. A lint suggests it where profiling shows heavy allocation. This replaces `with noalloc():` and the `noalloc def` prefix. Built, apart from the lint that suggests it, which waits for a profile to read: each check native code keeps (bounds, a missing key, dividing by zero, a shift's range, a builtin that can fail) is an error. An overflow check stays: wherever the compiler can show a number can't overflow, hot or not, the check goes, and where it can't, the check stays as a safety net. Chris decided this on 2026-11-06. `noalloc` is no longer a keyword, so it's an ordinary name (2026-11-09).
*Why:* It's a claim about a function, and nothing opens or closes, so it doesn't belong in `with`. Speed in a hot path becomes something the compiler checks rather than something people hope for.

**`@arena` is cut for now,** along with `with arena():`. It's expected back at function level for tree-heavy allocation, after measurement. There are no allocator parameters, now or then.
*Why:* The compiler handles much of it, and web servers get it per request, so it hasn't earned its place yet. Passing allocators to every function, as Zig does, is a lot of AI pain for little gain.

**Development builds report allocation.** They show allocations and bytes per frame, find leaks, and can enforce a budget ("this frame allocates under 1 MB").

## Determinism

**Dicts and sets keep insertion order.**
*Why:* The same program iterates the same way on every run and every machine. Python already does this, so it costs AI nothing.

**Evaluation is always left to right.**

**Floats have one equality, order and hash everywhere.**
- `==` treats NaN as equal to NaN, and `0.0` as equal to `-0.0`.
- `<`, `<=`, `>` and `>=` use the total order `sorted`, `min` and `max` use, with NaN last, so a NaN's sign can't change a result.
- Hashing follows that equality, so floats and structs with float fields work as dict keys and in sets.
- Arithmetic stays IEEE.
- `a.approx(b, within=...)` compares computed values with a tolerance. It isn't used for keys, because "close to" isn't transitive.
- `x != x` no longer finds NaN. Write `x.is_nan()`.

This replaces IEEE comparison operators, and the rule that a struct with a float field can't be a dict key. Built: `==`, hashing, sorting and `<` all follow it. A "never NaN" analysis is planned, so a comparison of values that can't be NaN costs what IEEE's does.
*Why:* Floats compare, sort, hash and key consistently. Measured with LLVM, every benchmark kernel stayed within ±0.4% of IEEE's instruction count once comparisons against a constant compile to one comparison, and vectorizing was unchanged.

**Floats give the same results on every platform and every build.** There are no fused operations, except a fused multiply-add written as `.fma`, and `sin`, `cos` and the rest are in the stdlib rather than the platform's math library. There's no `@fast_math` and no other way to opt out. Speed comes from checks the compiler proves it can remove. This replaces `@fast_math` on a function, and before it `with fast_math():`.
*Why:* Replays and lockstep multiplayer depend on the same bits everywhere, and an opt-out would break that for every function it reaches.

**Parallel reductions combine in a fixed order.** `sum(... for e in parallel(xs))` combines its parts in a tree whose leaves are ranges of at most 1,024 positions, set by the length alone, never by element size or thread count. So a float sum gives identical bits on every machine and thread count.
*Why:* Float results don't drift between runs or machines.

**Float reductions add in one fixed pattern on every platform.** `sum()` over floats, `dot()`, and `std.math`'s vector and matrix reductions keep 8 running totals, item `i` going to total `i % 8`, and then combine the totals pairwise. The pattern is the same on every machine and in every build, so the bits are too. It replaces anything fast-math offered for reductions. Chris decided this on 2026-11-07. Built for `sum`: a list of floats is summed by std's own `math.fixed_sum`, which isn't exported, written in plain Fennec with its eight totals as eight names, which only `sum` calls, and a sum one item at a time, over a generator, keeps the same eight totals in every engine, so the two give the same bits. The start is added last. `examples/float_sums.fen` holds the bits, which arm64, x86-64 and WebAssembly all give, in every engine and in `--release`. A sum over `parallel(...)` keeps its own fixed tree, below. Ints add as before, since their sum is exact. `math.dot(a, b)` over two lists adds the same way, so it's `sum(a[at] * b[at] for at in range(len(a)))` to the bit. One exception, decided on 2026-11-07: `std.math`'s fixed sums of four, in `Vec4.dot`, `Quat.dot`, `Quat *`, `Mat4.mul_vec4`, `transform_point` and the determinant, keep glam's left-to-right order. That order is as fixed, a sum of four gains no speed from eight totals, and `std.math` matches glam bit for bit. Its sums of two and three already add in the pattern's order.
*Why:* Eight totals fill a vector unit and hide the latency of each add, so a sum runs at the speed fast-math would give, without letting the compiler reorder anything. Pairwise combining is also more accurate than one long left-to-right chain.

**`math.fma(a, b, c)` is an explicit fused multiply-add,** `a * b + c` rounded once. It's exactly rounded, so it gives the same bits everywhere, and on a chip without the instruction it's only slower, never different. It's the one way a scalar fused operation happens, as `.fma` is for `Simd`. Chris decided this on 2026-11-07. Planned, not built yet.
*Why:* Some numeric code needs the extra precision of one rounding, and writing it out keeps every other `a * b + c` the same bits on every machine.

**`Clock` is a capability,** passed to `main` like `Stdout` and `Fs`. `clock.now()` reads a monotonic clock, for measuring. `clock.time()` reads the wall clock as a `Time`, a moment in UTC, for saving and showing. Tests get a fixed clock by default, as under Tooling.
*Why:* Code that reads the time says so in its parameters, and can be handed a fixed clock instead.

**`Random` is a seeded value.** `Random(seed=42)` draws the same numbers in the same order on every machine. It's SplitMix64, done in integer math, and `random_int(seed, n, low, high)`, imported from `std.random`, gives the `n`th number of a seed's sequence without keeping a generator. Seeding from the OS comes through `Entropy`, below.
*Why:* A seed is all a replay or a lockstep peer needs to see the same numbers. Drawing by position also suits procedural generation, where each chunk of a world takes numbers from its own index.

**Secure randomness is a capability, `Entropy`,** handed to `main` like the others. `entropy.bytes(n)` gives `n` secure random bytes from the operating system, and `entropy.seed()` gives an unpredictable seed for `Random(seed=...)`. Tests receive a fake `Entropy` with fixed output, so they stay repeatable. Hashing, HMAC and other pure cryptography need no capability, since they read nothing from outside.
*Why:* Randomness comes in through a capability, like every source of nondeterminism, and `Random` stays a plain seeded value.

**Any run can be recorded and replayed exactly.** Every source of nondeterminism comes in through a capability: time, randomness, files, the network and input. So the runtime can record exactly what came through and replay the run bit for bit with `fennec replay`. It comes in two steps. First, a recorded run logs every outside input at the standard library's boundary, which covers programs with one task. Later, it also logs the order locks are taken and channel messages arrive, for programs with many tasks. Normal runs pay nothing, and since floats give the same bits on every platform, a replay holds across machines. Not built yet.
*Why:* A player's crash report becomes a replay file that runs again exactly. The determinism rules above make it almost free.

## Speed

**Speed targets are measured against each benchmark's twin, fully optimized.** Every program in the benchmark suite also exists as idiomatic code in the fastest established systems language.
- Release builds: 110% of the twin's speed.

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

**Functions without capabilities have no side effects.** A `shared` value in a signature acts like a capability. A function that isn't passed either can't do I/O or change anything outside itself, so the compiler can merge repeated calls, move them out of loops and vectorize loops more aggressively.
*Why:* In most languages the compiler has to assume any call might do anything.

**There's no unwinding.** Errors are values, and a runtime failure such as overflow ends the program. Containing one to a single server task is the intent, under Concurrency. With no unwinding machinery, code is smaller and inlines better.

**There are ten sized number types: `i8` to `i64`, `u8` to `u64`, `f32` and `f64`.** `int` is `i64` and `float` is `f64`, and they stay the everyday types. There's no 128-bit type and no platform-sized one. Built in the checker, all ten, with `i64` as `int` and `f64` as `float`. Built natively, all ten, apart from a few gaps `docs/language.md` names, like a `u64` literal above the largest `int`.
*Why:* Game math, file formats, GPUs and hashes need exact sizes, and `int` already covers indexes.

**A literal takes the type it's going into, and is checked to fit at compile time.** `speed: f32 = 1.5` needs nothing more, and `pixel: u8 = 300` is a compile error. With nothing to go by, a literal is an `int` or a `float`, as today. Built, for names, parameters, returns, the other side of an operator, and the items of list, set, dict and tuple literals. Native code doesn't hold sets and dicts of sized numbers yet.
*Why:* There's nothing new to write, and a value that can't fit is caught before the program runs.

**Conversions are always written, in two forms.** `u8(x)` stops the program if `x` doesn't fit, since that's a bug. `u8.from(x)?` and `u8.parse(text)?` fail cleanly, for anything from outside, and the standard library's decoders use only those. Mixing types, like `i32 + i64`, is a compile error that says to convert one side, and widening, like `i64(small)`, is written too. Built: the mixing errors, `u8(x)`, and `u8.from(x)?` and `u8.parse(text)?`, written in Fennec, for the sizes that run.
*Why:* Automatic mixing is where C and C++ get subtle bugs, and the two forms keep a bug apart from bad input, so a packet with too big a number can't stop a server.

**Overflow on a sized type is checked, as on `int`.** A check is removed only where the compiler proves it can't fire, and `wrapping_` and `saturating_` methods are for when you mean it. Built for every size: an overflow stops with "this calculation overflows a u8, which holds 0 to 255". `wrapping_` and `saturating_` add, sub and mul are built for `i8` to `i32` and `u8` to `u32`. Not built yet: them for `u64`.
*Why:* We're a safe language, so a small type mustn't wrap quietly.

**`<<` drops the bits shifted past the top, on every integer type, with no check.** That's `int`, every sized integer and `u64` alike: `200 << 1` on a `u8` is `144`. A shift count at or past the type's width stops the program, as any count outside 0 to 63 does on `int`: below 8 for a `u8`, below 32 for an `i32`, and so on. Chris decided this on 2026-11-08. Built, natively: dropping the bits on every integer type, and the count check on every type, with "can't shift u8 by a count outside 0 to 7". `examples/bits.fen` shows `int`.
*Why:* A shift is bit work, not arithmetic, and it's how std builds 64-bit patterns, in `uuid` and `std.pack`. A count past the width has no meaning, so it's a bug.

**Math types are 32-bit.** `std.math`'s `Vec2`, `Vec3`, `Vec4`, `Quat`, `Mat3` and `Mat4` are concrete `f32` types, and `DVec3` and the other `D` types hold doubles. They're not generic: a generic version with an `f32` default could come later without breaking anything. Not built yet: until sized numbers are in, they're `float` and are checked against glam's 64-bit types.
*Why:* Twice the SIMD throughput for game math, as glam, GLSL and GPUs have it, while `float` stays accurate everywhere else.

**Float functions give the same bits on every platform.** `sin`, `cos`, `tan`, `acos`, `exp`, `log` and `**` are computed by one pure-Rust library, `libm`, in every engine, on arm64, x86-64, Windows and WebAssembly alike. Native code calls the runtime for them, never a platform intrinsic, and never fuses `a*b+c`. Built.
*Why:* Each platform's own C library rounds some results differently in the last bit, as `cos(-0.65)` does in WebAssembly and `cos(-0.1)` on x86-64 macOS, and replays and lockstep multiplayer need every machine to agree. It costs a little speed in a loop of nothing but these functions, mostly in `**`. The coordinator decided this on 2026-11-07.

**The compiler tells LLVM that names don't alias.** Exclusive `ref` access proves no two names point at the same memory, so every function gets that guarantee for free. Fortran's speed comes from the same guarantee.

**Dividing by zero stops the program, for floats as for ints.** `x / 0.0` stops, as Python's `ZeroDivisionError` does. The compiler removes the check where it proves the divisor isn't zero, and `@hot` allows a leftover float divide check as it allows overflow checks. There's no special method for now. Built, apart from what `@hot` allows.
*Why:* A silent infinity spreads through later results and hides the bug, and we're a safe language.

**Integer overflow is checked in release builds too.** Wrapping is opt-in: `x.wrapping_add(y)`.
*Why:* A program that compiles should do what it says. The check costs about 1–2%, and the benchmark suite measures it.

## Functions and types

**Failures have a word of their own, `except`, and `else` means only "otherwise".** Chris locked this on 2026-11-11. There are three outcomes: a missing value, `T?`, handled with `else`; a failure, `fails E`, passed up with `?` or handled with `except`; and a bug, which stops the program.
```fennec
# pass it up
score = load_score(fs, path)?

# one case, on the line
port = int.parse(text) except InvalidValue(): 8080

# any failure, named: the block leaves or gives a value
settings = json.loads[Settings](text) except why:
    log.warn(f"settings.json is damaged, using defaults: {why}")
    Settings()

# several cases, as arms right under the name
score = load_score(fs, path) except why:
    case .Missing(missing_path):
        stdout.print(f"no save at {missing_path}, starting from 0")
        0
    case _:
        fail why
```
- `except` covers only the call it follows. It comes after a whole right side, a bare call or `return`, and never inside a larger expression.
- `except Case(): value` handles one case on the line. It must cover everything the call fails with, and an or-pattern can, as `except Closed() | Timeout(): 0`.
- `except why:` handles any failure. `why` must be used on every way out: read into something that goes somewhere, as an argument, an f-string, a field read or `fail why`. Being the subject of a `match` or an `is` doesn't count, and neither does `_ = why`. A `fail` of a new error in the block carries `why` along as a note, so it counts. Arms that name every case needn't read it. On `_ = f() except why:` the block needn't give a value.
- A capitalized name after `except`, as `except Timeout:`, is a compile error that points to `except Timeout():`, or to a lowercase name for any failure. A second `except` line under the first points at `case` arms.
- On an error with cases, the type-wide pattern, as `IoError()`, is refused under `except`, since it drops every case by naming the type. An error with no cases, as `InvalidValue()`, keeps it.
- When a union's types share a case name, as `IoError | NetError` both have `NotFound` and `Other`, `.NotFound` is a compile error that names both types, and the fix writes `IoError.NotFound()`.
- Several cases are `case` arms directly under `except why:`, with no nested `match`. An arm wider than a named case, as `case _:` is, must use `why` or `fail`. `case other:` is refused under `except`, since `why` names the failure already. An `except` block is all arms or all statements.
- Nothing is dropped without a word: no bare `except:` after a call, no `except _:`, no `as _`, and `_ = f()` on a call that can fail is refused. `failure_of` is for tests only, which is built.
- `else` keeps `if`/`else`, `a if c else b` and a missing value's default, with `else return`, `else fail E`, `else break` and `else continue` to leave. `else` on a call that can fail is a compile error that shows `except`, and so is `except` on a plain optional, which shows `else`.
- On `T? fails E`, `?` or `except` takes the failure off, then `else` the `None`: `user = db.find_user(name)? else return None`. An `else` for the call's own result on the same line as its `except` is refused, with the two-line form shown. A conditional `else` inside the handler's value is fine.
- `lend x except:` is the cleanup that runs when the caller's block fails or crashes, and it keeps the note rule: a cleanup that fails becomes a note on the block's failure. It's the one bare `except`, since cleanup doesn't handle the failure, which carries on up by itself.
- A call that can fail is the whole right side, a bare call or `return`'s value, unless it has `?` or `.expect("reason")`, which nest anywhere. Anywhere else, the compile error shows the split onto two lines. A `with` or `for` line takes only `?`.
- `case` is reserved, and `try` is reserved only so `try:` can be a compile error that shows `except` after the call.
- `receive()` gives `T?`, `None` once the channel is closed, and `receive_within(d)` gives `T? fails Timeout`. `send(message)` fails with `Closed`, and `send_within(message, d)` with `Closed` or `Timeout`.
- `IoError` gets the cases `NotFound`, `PermissionDenied`, `AlreadyExists` and `Other`. Text is read as a number with `int.parse` and `float.parse`, which fail with `InvalidValue`. A conversion `T(x)` never `fails`: `int(text)` and `float(text)` are compile errors whose fix writes `.parse`, and `std.math.NAN` replaces `float("nan")`. A conversion that doesn't fit, as `u8(300)`, still stops as a bug.
- `lend x except:` is also the one `except` that runs for a bug, a crash.
- `std`'s serializer method `case` is renamed `enum_case`, with its parameter `case_name`, so `case` can be reserved. Done in the same PR as these docs.
- Fable 5.2's review of the locked syntax (2026-11-11) clarified the rules above without changing a decision: what "used" means, the type-wide pattern, shared case names, the capitalized name, the coverage of one case, `int(text)`, `send_within`, and the edges in the next two entries.
- `fennec fix` rewrites `else` on a failure, `else as why:`, `match why:` inside it and `lend x else:` to these forms. A plain `else value` on a failing call drops every failure, so each is rewritten to name what it drops, and each needs a look.
- Fable 5.2 proposed it, and three Fable 5.2 passes and Opus 5.5 at extra-high effort reviewed it. Its six fixes are in the list above: wider arms use `why`, `case` is reserved, `?` then `else` on `T? fails E`, `lend except` keeps its note rule, `except` never inside an expression, and the two `receive`s. Not built yet.

This replaces the 2026-11-09 decision below, that a failing call is handled with `else`, `else value` or `else as why:`.
*Why:* Missing and failed shared one word, and every rule that felt like a trick existed only to pull them apart again: `else as`, a nested `match why:`, `case _` as a failure's catch-all, and the special cases for `T? fails E`. Each outcome now has its own word, so none of those rules is needed. `except` is the word a Python reader already knows, at the place the failure happens.

**Every `def` returns with `return`.** A bare value at the end of a `def` is a compile error, "a `def` gives its value with `return`", and `fennec fix` adds the `return`. `return match x:` gives a `match`'s value. A block after `x =`, as `x = match`, `x = lock` and `x = f() except why:` are, ends with its value, and `fennec fix` never adds a `return` inside one: a `return` there leaves the function, which type-checks whenever the block's type and the function's match, so `language.md` shows that pitfall with an example. Chris locked this on 2026-11-11, saying explicit `return` "reads more cleanly". Not built yet.
*Why:* A reader finds every way out of a function by looking for `return` and `fail`, and the one place a bare value ends a block is right after the `=` it goes to.

**`len` is a builtin, as `sum` and `sorted` are, and the builtins' names are reserved.** `len(items)`, as in Python. It's a protocol, as in the table under Principles: `len(x)` calls a type's `def len(self) -> int`, as `str(x)` calls `def str`. `x.len()` gets a `fennec fix` suggestion that writes `len(x)`. `abs` is a protocol the same way: `abs(x)` for the numeric types and for any type with `def abs(self) -> Self`, and `x.abs()` gets the same suggestion. Chris decided `abs` later on 2026-11-11. A closed list of names can't name a local, which is a compile error that suggests another name: `len`, `abs`, `sum`, `sorted`, `min`, `max`, `range`, `enumerate` and `zip`, and the built-in types `int`, `float`, `bool`, `str`, `bytes`, `list`, `dict`, `set`, `array` and the ten sized numbers. A builtin added later comes with a `fennec fix` rename, and fields and named arguments stay free. There's no cap on how many keywords Fennec has. Chris locked this on 2026-11-11. Not built yet.
*Why:* `.len()` was the one place Fennec broke a Python habit for no gain. A local named `len` would hide the builtin for the rest of its block, as Python's `list = [...]` hides `list`.

**Ignoring a returned value is a compile error.** `_ = call()` discards one on purpose. Built. On a call that can fail, `_ = f()` is a compile error too, so a failure must always be handled, and `ignore` is removed. Built. Chris decided this on 2026-11-08, since `ignore` was used only by its own tests across the whole repository. `Ok`, `Err` and `Result` are removed, as below.
```fennec
sorted(scores)       # error: the result is ignored
_ = sorted(scores)   # fine
```
*Why:* A function that returns something usually returns the point of calling it.

**`copy(x, field=...)` makes a changed copy,** and re-runs any `where` rules on the fields it changes. `list(iterable)` builds a list, as in Python.
```fennec
healed = copy(unit, health=100)
names = list(unit.name for unit in units)
```

**`case .Button(ref clicks):` changes a field inside an enum.** The `match` is a statement over a place in a mut name, and the case is put back, with the changed field, wherever its body ends: when it finishes, leaves by `return`, `break` or `continue`, or fails. Its other fields are matched by name, `_` or a literal. Built.
```fennec
match ui[widget_id].kind:
    case .Button(ref clicks):
        clicks += 1
    case _:
        pass
```

**Parameters can have defaults, and a parameter with a default is passed by name only.**
```fennec
def scan(rows: list[Row], limit=8) -> list[Row]:
    ...

scan(rows)
scan(rows, limit=100)
```
- A literal default gives the parameter its type, for `int`, `float`, `str` and `bool` literals and for enum cases: `mode=Mode.fast`.
- A call to a type's constructor with its type arguments written, like `Packed[int]()` or `dict[str, int]()`, gives the type the same way, for struct fields and parameters alike. `neighbors = Packed[int]()` needs no `: Packed[int]`.
- For an empty collection, the constructor is the only form of default: `items = list[str]()`. `items: list[str] = []` is a lint error whose fix, which `fennec fix` applies, rewrites it to `items = list[str]()`. Inside a function body, `items = []` stays fine, typed from its use.
- The type is written when the default is `None`, as in `target: Unit? = None`, a constant's name or any other expression, or when the type isn't the literal's own: `limit: i32 = 8`, `ratio: f32 = 0.5`.
- Writing the type the default already gives is a compile error: `name: str = "Chris"`, `limit: int = 8` and `neighbors: Packed[int] = Packed[int]()`. The fix removes the type, and `fennec fix` applies it.
- A default is a plain value computed at compile time, under the same rule as a module constant. Calling without it costs the same as passing the value.
- There are no exceptions in `std` either. `d.get(key)` returns an optional, so a fallback is written `d.get(key) else 0`, and `sum` takes `start=`.
- Required parameters may be positional, within the limits below.

This replaces `limit: int = 8` as the written form.
*Why:* Options read at the call, as `parallel(xs, limit=8)` does, and adding one never moves another argument. A default is a value, so no call can change it for the next one, which is Python's shared-mutable-default trap. The literal already states the type, so the signature stays complete, and the declaration reads like the call. Making the repeated type an error keeps one way to write it.

**Some positional arguments are a hard error.** These are bare `True` or `False`, positional arguments to neighboring parameters of the same type, and three or more positional arguments, unless every two neighboring parameters have types that can't hold each other's values, so a swap wouldn't compile: `report(stdout, world, board)` stays as it is. The first argument and `self` are exempt. The fix names them:
```fennec
copy_file(src, overwrite=True)
move(from_tile=a, to_tile=b)
```
*Why:* A call like `move(a, b, True)` compiles and reads fine when its arguments are swapped. Naming them makes that mistake visible to people and AI, and the first argument is almost always obvious from the function's name. Where no two neighbors could be swapped and still compile, the compiler already catches the mistake, so names would only add length.

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

total = load(fs, path) except why:
    case .NotFound(path):
        return Response(status=404)
    case _:
        fail why
```
- One `error Name:` can list several cases, so a signature stays short however many ways it can fail. `fennec fix` writes it. Built: a function that can fail and doesn't say so is a compile error, apart from `main`.
- `?` is required wherever a failure passes up, including in `with` and `for`. Built for `def`s: `return load(path)` and a last line `load(path)` are compile errors, and `fennec fix` adds the `?`.
- A failure is always an `error` type. Built.
- `if x is None:` narrows `x`. A failure isn't a value to test: it's handled at the call, as below.
- `xs[i]` out of range is a bug that stops the program. `xs.get(i)` returns an optional, for when it might not be there. Built.

This replaces the earlier rules that errors are inferred and `fails` is optional.
*Why:* A reader sees at a glance how a function can fail and every line where it can leave. Swallowed errors are how reliable-looking programs go wrong.

**Replaced on 2026-11-11 by `except`, above: a failing call is handled at the call, and there's no `Ok`, `Err` or `Result`.** It had three ways:
```fennec
# pass it up
score = load_score(fs, path)?

# use a value instead
score = load_score(fs, path) else 0

# name it, and leave or give a value
score = load_score(fs, path) else as why:
    match why:
        case .Missing(path):
            stdout.print(f"no save at {path}, starting from 0")
            0
        case _:
            fail why
```
- `f().expect("reason")` crashes on purpose, with that reason, for a call that can't fail here: the explicit "crash here, for this reason" form. Chris decided this on 2026-11-08. Built.
- An `else as why:` block leaves, with `return`, `fail`, `break` or `continue`, each checked against the function's signature, or gives a value as its last line.
- Different failures are handled with an ordinary `match why:` inside the block, and `_` is the only catch-all.
- A failure passes on as `fail why`. There's no bare `fail`.
- An `else` block that neither gives a value of the right type nor leaves is a compile error that says which it needs.
- `else as` on a plain optional is a compile error, since there's no failure to name. On `T? fails E`, `?` handles the failure and `else` handles the `None`: `find(n)? else 5`.
- `T fails E` is a type only the runtime makes, as for the results of parallel work, and it's read only through `?` and `else`.
- Held in reserve, not built: an `else match:` shortcut, if real code shows that handling one failure and passing the rest up is common. It's 2 of 141 `match`es in the repository today.
- Chris decided this on 2026-11-09, after three reviews. Built: `?`, `else value`, `else as why:` with a block that leaves, `match why:` inside it, `fail why`, refusing a bare `fail`, refusing `else as` on a plain optional, and `?` then `else` on `T? fails E`. Not built yet: removing `Ok`, `Err` and `Result`, which a `match` and a written `Result[int, Bad]` still take, and an `else` block giving a value, which is refused today with "an `else:` block must leave". This replaces matching on `Ok` and `Err`, `Result` types written in code, `return Err(...)`, and narrowing with `if x is Err(...)`.
*Why:* One failure system instead of two: a failure is never a value that can be stored and forgotten, so nothing silently becomes a placeholder. It's close to Zig's `catch |err| switch`.

**A block that stands where a value goes ends with that value, or leaves.** Three blocks can: `x = match ...:`, `x = lock ... as ...:` and `x = f() except why:`, which was `x = f() else as why:` until 2026-11-11. Bare blocks and a multi-line `if` can't, since the one-line `x if c else y` covers that. Chris decided this on 2026-11-09. Built: all three, and refusing a multi-line `if` as a value.
*Why:* One rule covers every block that gives a value, and a value that depends on a condition already has a one-line form.

**An error carries notes, a `list[str]`, as Python's exceptions have since PEP 678.** Every `error Name:` type has `failure.add_note(text)`, which needs a `mut` place, and `failure.notes() -> list[str]`. When a block fails and its `lend x except:` cleanup fails too, the cleanup's failure is added to the block's as a note, in its printed form, so neither is lost. A failure that reaches `main` prints each note under it as `note: ...`. Notes are left out of `==`, hashing, printing and saving, and a failure with none costs one empty word. Built: `notes()`, `add_note(text)`, and a failed cleanup's failure as a note.
*Why:* When cleanup fails after a failure, the first failure is usually the cause and the second is the clue, so both have to reach the reader. Notes as text need no new kind of type.

**There's no `exit()`.** `fail` out of `main` prints the failure to stderr and exits with code 1, and `main` may return an `int` as the exit code. Nothing else can end the process. Built.
*Why:* A program ends in one visible place, and no library can stop the process behind its caller's back.

**`?` only ever passes a failure up, and there's no `?.`.** `rows = load(path)?.rows` passes the failure up, then reads `.rows`.
This replaces optional chaining with `?.`.
*Why:* Every `?` marks a place the function can leave, and a reader can see that without knowing any types.

**A value that might be missing is handled where it's read:** with an `is None` check, `match`, or `else` at the end of a chain.
```fennec
name = units.get(id).name else "?"
score = scores.get(team).get(player) else 0
hp = (units.get(id).hp else 0) + 1
target = units.get(target_id) else continue
unit = units.get(unit_id) else return 0
unit = units.get(unit_id) else fail ApiError.NotFound
```
- Fields, calls and `[]` on an optional are allowed only in a chain that ends in `else`. The chain stops at the first missing or failing step and gives the default.
- `else` covers only the chain right before it. A larger expression needs brackets.
- `else` gives a value or leaves, with `else continue`, `else return` or `else fail why`. It doesn't cover actions. Calling a method for its effect on something that might be missing needs `if x is not None:`.
- Without `else`, it's a compile error whose fix adds one or a check.

*Why:* Each spelling says what the writer meant. `?` means this line can leave the function. `else` means missing or failed is fine here, so use this instead, or leave, which the line says. `[]` means it must be there, so a missing value is a bug that stops with an error naming the step and the key.

**`else as name:` binds the failure in that block,** as Python's `except E as e` does. It leaves, or gives a value, as above. Chris decided this on 2026-11-08. Replaced on 2026-11-11: it's written `except name:` now.
```fennec
score = load_score(path) else as failure:
    stdout.print(failure)
    return
```
*Why:* A block that handles a failure usually wants to say which one, and `match` is a lot to write for that.

**`where` rules run on every write to a guarded field,** and on defaults at compile time: `health = 100 where health >= 0`. A rule looks only at its own field. An invariant across several fields goes in a named constructor or a method. A broken rule stops the program in code, and a load that breaks one fails cleanly. Both are built, writes after construction included.
*Why:* A rule that only holds at construction promises something it doesn't deliver. Checking one field keeps each write's cost obvious.

**Every `def` writes its return type. Arrow functions never do. No `->` means the function returns nothing.**
```fennec
def visible(world: World) -> list[Enemy]:
    def in_view(enemy: Enemy) -> bool:
        return enemy.distance_to(world.camera) < world.view_range
    return [e for e in world.enemies if in_view(e)]

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

**`Buffer[T]` is the one public building block for memory.** It's safe, bounds-checked, growable and reference-counted: the same block as a `list`, with public methods. `push` fills the next slot, growing if it's full, `b[i]` reads and writes a filled slot, and `reserve`, `capacity`, `pop`, `truncate` and `is_unique` do the rest, the last being what copy-on-write needs. A buffer tracks how many slots are filled, so reading a slot nothing wrote is an ordinary out-of-bounds failure, and freeing it knows what to release. It's designed with the runtime work now, and `list`, `dict` and `str` move onto it once benchmarks show no slowdown. Written in Fennec on top of it, they use it the same way any program can.
*Why:* The built-in collections need no machinery a program can't use, so a game's own pool or ring buffer is built from the same safe piece.

**A buffer's room grows by one rule everywhere.** A new buffer has room for 4 items, or for the `capacity=` it's given if that's more. Pushing into a full one gives it room for twice the items it held, as Rust's `Vec` grows, or for what it needs if that's more, so from 4 its room goes 8, 16, 32. `reserve(n)` makes room for at least `n`, and `pop` and `truncate` keep the room. `capacity()` is the room.
*Why:* `capacity()` is a number a program can print, so it has to be the same whichever way the program runs. The rule is the one lists grow by, so a buffer and a list grow alike. Doubling what's held, not what's needed, keeps rooms at powers of two from the start, which the allocator's sizes fit.

**`is_unique()` is true only when nothing else holds the buffer, and may be false when unsure.** Use it to choose a faster path, like changing a buffer in place rather than copying it, never to change what a program does or prints.
*Why:* How many things hold a value isn't the same in every way of running a program: an interpreter keeps temporaries a compiled program doesn't. A promise that's only ever true when it's safe lets each one answer from what it knows, and keeps output the same.

**Intrinsics are bodiless functions in `std.intrinsics`, marked `@intrinsic`, and the compiler supplies each body.** Only the standard library can import `std.intrinsics`. A program that tries gets an error saying the module is the standard library's own.
*Why:* The machine-level operations the built-in types stand on look like any other function to the code that calls them, so the checker and every tool treat them the same way. Keeping them inside `std` means a program's safety never rests on how one is used.

**A function is global when it works on any sequence through looping and operators, or when it's one of the protocols a type defines, as `str(x)` and `len(x)` are; anything else that asks a value about itself is a method.**
- The globals are `sum`, `min`, `max`, `any`, `all`, `sorted`, `zip`, `enumerate` and `range`, which use only iteration and a type's operators (`def +`, `def <`), and `len` and `abs`, which call a type's `def len` and `def abs`, as the table under Principles has it. A user type joins them by defining its operators and protocols. There are no other hooks and no dunders.
- Everything else type-specific is a method. `len(xs)` and `abs(x)` went global on 2026-11-11, under "`len` is a builtin" above.
- `int(...)`, `str(...)` and `float(...)` are the types' constructors, not global functions.
- `copy(x, field=...)` changes fields as it copies. It works on any value and can't be defined by a type. There's no plain `copy(x)`, since every copy is already a snapshot.
```fennec
interface Signed:
    def abs(self) -> Self

# global: only looping and +
total = sum(e.health for e in enemies)
# global: a protocol, which calls the type's own `def abs`
distance = abs(a - b)
# method: a question about one value
alive = enemy.is_alive()
```
*Why:* A looping function belongs to no one type: it works on lists, sets, dicts and generator expressions alike. A question about a value is answered differently by each type, so it lives on the type.

**`sum` needs a start for anything but numbers.** `sum(numbers)` needs nothing, and an empty `list[float]` gives `0.0`. Any other type passes a start, by name: `sum(wallet, start=Money(cents=0))`. A missing start, a type with no `def +`, and a start of the wrong type are all compile errors.
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
        case .Int(value): return .Int(value)   # the return type makes it ExprKind.Int
```
Only with no expected type is a name shared by two enums ambiguous, and the error lists them. The search covers only the enums a file can see: its own and those it imports.
*Why:* The type is usually right there, as in Swift. Otherwise adding an unrelated file could break code that never imports it.

**`extend` adds methods to a type from another file of the same kit.**
```fennec
# parser/expr.fen
extend Parser:
    def parse_expr(ref self) -> Expr:
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
    def sort(ref self): ...
```
`count` exists only on lists whose elements can be compared with `==`, and `sort` only on lists whose elements can be ordered. Two methods with the same name on one type are an error, wherever they're written, apart from an operator defined for different right-hand types, as below. The editor, `fennec doc` and `fennec context` list every method of a type in one place.
*Why:* A type that many responsibilities attach to, like a parser, an interpreter or a game's `World`, is otherwise one huge file, or its methods become free functions with noisier calls. Swift, C#, Kotlin and Go all let a type's methods live in several files. Keeping it to the kit's own types means no one can change a type they don't own.

**Units of measure are types.** Meters, pixels, seconds and radians can't be mixed by accident, and multiplying or dividing them gives the right unit. They need no new syntax: a unit is a struct holding its number, and its operators say what each combination gives, using one operator for each right-hand type. Chris decided this on 2026-11-07, which settles the syntax. Built: a unit struct with one operator of each kind, like `Meters + Meters`. A second operator of the same kind, like `Meters / float` beside `Meters / Seconds`, is built too.
```fennec
# Planned: the second `def /` is refused today
struct Meters:
    value: float

    def +(self, other: Meters) -> Meters:
        Meters(value=self.value + other.value)

    # a distance over a time gives a speed
    def /(self, time: Seconds) -> Speed:
        Speed(value=self.value / time.value)

    # a distance over a plain number is still a distance
    def /(self, parts: float) -> Meters:
        Meters(value=self.value / parts)
```
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
Type arguments are inferred at calls; `largest[int](xs)` is allowed when they can't be. This replaces the earlier shorthand where an interface inside a parameter's type, `list[Shape]`, meant "any one type that fits". Writing that now is an error, and its fix offers `[T: Shape]` for one type or a union for a mix.
*Why:* `T` lets the type flow through, so `largest(enemies)` gives back an `Enemy`, and it relates parameters to each other. There's one way to write it, and it's Python's. In Python, Java, C# and Go, `list[Shape]` means a list of mixed types dispatched at run time, so the shorthand would have misled exactly the people and models Fennec is written for.

**A parameter typed with an interface is resolved at compile time.** `shape: Shape` is allowed, and each call is specialized to the type it passes, exactly as `[T: Shape]` is, with no hidden dispatch or boxing.
```fennec
def describe(shape: Shape) -> str:
    return f"area {shape.area()}"
```
An interface inside another type, like `list[Shape]`, stays an error, as above.
*Why:* A signature can name what it needs and still cost what a direct call costs, and a reader never has to wonder whether a call goes through a table.

**Generic code is checked once, and errors name the missing piece.** A generic body may use only what its bounds promise, so errors never surface at an instantiation. A call with a type that doesn't fit says what's missing and the nearest match: "`Rock` isn't a `Shape`: it has no `area(self) -> f32`. Its `area` returns `int`."

**A generic function can loop over a type at compile time, and that's the whole of compile-time generation.**
```fennec
def write[T, S: Serializer](x: T, ref out: S):
    for f in fields(T):
        out.field(f.name)
        write(f.get(x), ref out)
```
- `fields(T)` gives a struct's fields, each with its name, type, value and annotations. `cases(T)` gives an enum's cases. `*Cs` is a list of types, as in `world.query(Position, Velocity)`. `if T is int:` is decided at compile time. Built: `for f in fields(T):`, with `f.name` and `f.get(x)`. The body is checked once, with `f.get(x)` of a type that stands for each field's and goes only where any type can, and it's written out for each field when each copy is made. Not built yet: a field's type, value and annotations beyond those, `cases(T)`, `*Cs` and `if T is int:`, and formats still get their fields from the compiler.
- Each set of types gets its own specialized copy, whatever the build.
- There are no macros, no `derive` and no generating code from text. There's no I/O at compile time beyond `embed_file` and `embed_folder`, which read files that are part of the build, and no new types can be invented.
- New declarations, like a client struct from an API schema, come from separate programs that write `.fen` files.

*Why:* It covers serialization, SQL rows, CLI parsing, ECS queries and test helpers, and the checker sees ordinary code. With no I/O, builds stay deterministic and cacheable, and a kit can't run anything on your machine while it compiles, which closes a real supply-chain hole.

**Annotations are data, read at compile time.** `@serialize(name="uid")` on a field says something a generic function can read. An annotation never wraps or changes what it's on.
- A kit may define its own annotations for fields, like `@column("todo_id")` for a database kit, and generic code reads them through `fields(T)`.
- A function takes only the language's own: `@hot`, and std's `@intrinsic`. A kit can't define one for functions, so a route is registered in plain code, `router.get("/todos/{id}", show_todo)`, as a function value, and the table it builds is ordinary data.
- Built: `@skip` and `@serialize` on fields, and refusing any other annotation, on a field or a function, with an error that lists the ones there are. Not built yet: annotations a kit defines, reading a field's annotations through `fields(T)`, and a router in `std`. `for f in fields(T):` is built, with each field's name and value.
- Chris decided the kit and function rules on 2026-11-07, which settles whether kits can define annotations, and confirmed on 2026-11-09 that a kit's annotations on fields are data only.
*Why:* A few things can't be said any other way, like a field's name in someone else's format, and data can't hide behavior. On a field, an annotation only describes data. On a function, it would invite decorators that change what a call does out of sight, and a route list in plain code is already one place to read every route.

**Generic Fennec code compiles to a copy for each type, in every build.** A generic function or type is copied for each set of types a program uses it with, before any backend sees it, so `[T: Shape]` and `shape: Shape` compile the same and every call is direct. Development and release builds lower the same copies, and each is cached by a hash of its text. A program can ask for up to 2,000 copies. The runtime's own list and dict operations are still shared by design, taking an element number, since they're runtime code, not generic Fennec code. This corrects the entry that said generics compile the way Swift's do, with development builds sharing one copy.
*Why:* One rule for every build, with no hidden dispatch in Fennec code, so what runs while editing is what ships.

**`A | B` is a union: a value that's exactly one of those types.** It's stored inline with a one-byte tag, sized for its largest member, and never boxed. `type Shape = Circle | Square` names one.
```fennec
type Shape = Circle | Square

def report(shapes: list[Shape], stdout: Stdout):
    stdout.print(sum(s.area() for s in shapes))   # every member has area(), so Shape does
    for s in shapes:
        if s is Circle:
            stdout.print(s.radius)                 # s is a Circle here
```
- A `Circle` goes anywhere a `Circle | Square` is expected. `if s is Circle:` narrows `s` inside the block, and `match` handles each member, checked for exhaustiveness.
- When every member has a method, the union has it too, and the union fits any interface all its members fit. Calls go through a jump table.
- Order and nesting don't matter: `A | B` is `B | A`, and a union inside a union flattens. A duplicate member, like `int | int`, is an error. Two union values are equal when they hold the same member with equal values, and they hash the same way.
- `None` is never a member. `str | None` is an error whose fix writes `str?`, and an optional union is `(A | B)?`, so an optional of an optional, `T??`, never collapses into `T?`.
- Failures stay separate: a function that can fail returns `-> T fails E`, never `-> T | E`.
- Members are concrete types, not type parameters; generic code uses an enum, like `enum Either[L, R]`. A union refers to itself only through a collection, as in `type Json = str | int | float | bool | list[Json] | dict[str, Json]`, and direct recursion is an error.
- A union mixes types that already exist, and an enum defines new cases. A union may have an enum type as a member.
- A literal assigned to a union takes its default type: `3` is an `int`.
- Built in the checker and native code: writing unions, `is` and `match` with their narrowing and exhaustiveness, methods every member has, equality and hashing, and the mistakes above. A union is a dict key or set item when each type in it can be one, and it fits an interface every type in it fits. Native code holds a union in place, as an enum with a case for each member.
- JSON always writes a union tagged with its member's type name, `{"Circle": {...}}` or `{"Tile": "Grass"}`, so loading is never ambiguous. The tag is the full name only when two members share a short name. Built for fields: one holding a union, as it is, in a list, a dict's values or an optional, saves tagged in JSON and TOML, and by where the member is among the union's in pack. A member has to be a type's name alone, like `Circle` or `int`, so a union with `list[int]` in it isn't saved yet, nor is one in a set or a dict's keys, and a recursive union like `Json` isn't either.

*Why:* Mixed lists stay fast and unboxed, and they read like a Python type hint. Unions add nothing new to learn beyond `is` and `match`.

**There's no run-time open set.** There are no boxed, dynamically dispatched interface values, and that's a hard no. Every type is known at compile time, since the whole program is compiled, Den packages included, and plugins or scripts loaded at run time aren't a thing.
*Why:* The goal is as many facts as possible about a program at compile time.

**Mixed types are a union or an `enum`; kinds other kits add go through function values.** A list that holds different existing types is a list of a union, `list[Circle | Square]`, and a list of new cases is a list of an enum. Either way each element is inline with a one-byte tag, and `match` compiles to a jump table. Other kits add kinds through a struct with function fields, all compiled into the one program.

**Function values stay, and they're closed.** `router.get("/users", list_users)`, callbacks and dicts of handlers are all allowed. The whole program is compiled, so every function a value could hold is known at compile time, and choosing one at run time is a branch. There's no `dyn` and no unknown code. How such a call compiles is engineering, not a language rule.
*Why:* Routes, callbacks and dispatch tables are ordinary code, and none of them needs anything the compiler hasn't seen.

**A program makes two kinds of closed run-time call.** One goes through a function value. The other resumes a generator that's been passed around, since which generator it is can depend on the run. The compiler knows every target of both. This corrects the entry that called a function value the only run-time dispatch.

**`Self` in an interface's method means the type that fits it.** `interface Signed: def abs(self) -> Self` says `abs` returns the same type it's called on: a `Money` for a `Money`.

**`Eq`, `Hash`, `Ordered` and `Format` are built-in interfaces,** satisfied automatically wherever the automatic `==`, hashing, ordering and printing apply. A type with its own `def str` still satisfies `Format`.

**A type may define each operator once for each right-hand type.** `Mat4 * Mat4` and `Mat4 * Vec4` can both exist, and so can `Time - Time -> Duration` and `Time - Duration -> Time`. The types must match exactly, with no conversions, and it's for operators only: a named function still has one meaning. Built.
*Why:* `m * v` and `deadline - now` read like the math, and exact matching keeps it simple to check, unlike overloading in languages with conversions, whose best-match rules give confusing errors and slow compiles.

**`in` is an operator a type defines,** like `def +`, `def <` and `def []`.
```fennec
struct Inventory:
    _items: SortedMap[str, int]

    def in(self, name: str) -> bool:
        return name in self._items
```
`key in m` calls the right-hand value's `def in`, and `not in` follows from it. `list`, `dict`, `set` and `str` define it in `std` the same way, so there's no hook.
*Why:* User collections like `SortedMap` then read like built-in ones, with no `map.has(key)`.

**`=>` is one expression; anything longer is a nested `def`.** There are no multi-line anonymous functions, as in Python. Function types are written `(int, str) -> bool`.

**Functions used as values never change anything around them.** An arrow function, or a nested `def` that's passed, stored or returned, captures the names it reads by value and can't change any of them. Capturing a `mut` name is an error, with a fix: bind it to a new name first.
*Why:* Every closure that escapes is then a pure function of its inputs, safe to run in parallel, cache or replay. It needs no analysis of which closures escape and no marker on parameters that store one, and capturing costs a reference-count bump at most. Handlers return commands instead of changing state, and builtins and comprehensions build up values.

**A nested function can't change the names around it.** It takes a `ref` parameter instead.
```fennec
def collect_leaves(tree: Tree) -> list[Tree]:
    def visit(node: Tree, ref leaves: list[Tree]):
        if len(node.children) == 0:
            leaves.append(node)
        for child in node.children:
            visit(child, ref leaves)
    mut leaves = []
    visit(tree, ref leaves)
    return leaves
```
This reverses the decision that a directly called nested function can change outer names, and the earlier rule that closures which don't escape may change captured `mut` names.
*Why:* It was the one place something changed through a hidden link, with nothing at the call to show it. Now one rule covers it, and most counters are `sum(...)` anyway.

## Lifetimes

**A lifetime is a block.** `with` is how ownership and lifetime are spelled. Any exit closes the block, inner blocks first, and you can see the order in the indentation.
```fennec
with db.transaction()? as tx, fs.open(path)? as file:
    tx.insert(parse(file)?)?
```
There are no moves. A resource that has to live a long time, like a pooled database connection, lives inside one keeper task's `with`, and everyone else sends it messages. This replaces the rule that a long-lived resource is an owned value that moves and closes when its last owner goes away.
*Why:* If everything temporary lives inside a block, there's nothing for lifetime annotations to describe.

**`with` is only for real lifecycles:** things that begin and have to end, like files, connections, transactions and tasks. A `with` block isn't a value: what it works out goes in a name declared before it, or it returns. `with` never locks: that's `lock`, under Concurrency.
*Why:* Every `with` then means the same thing, so a reader knows something gets closed at the end of the block. Claims about a function, like `@hot`, go on the function.

**Names don't leak out of blocks, and reusing a name still in scope is a compile error.** There's no `const` or `let`: `a = 5` is immutable and `mut a = 5` isn't.
*Why:* A silently shadowed name is a classic bug for people and AI, and picking another name costs little.

**`with` runs cleanup on every way out of its block:** the end, `return`, `break` and `?`. There's no unwinding.

**The value after `with` must be a plain value.** An error from opening it is dealt with first: `with fs.open(path)? as file:`.

**`with` lends you something for the length of a block, and it can't escape.** What a `with` gives can't be returned, stored outside the block or put in a `shared`. A second name for it inside the block is fine, and a struct holding something borrowed is borrowed too, so wrappers work. Built for sources a program writes.
*Why:* A file, socket or transaction then closes exactly once, at a line you can see, and nobody can use it after.

**There's no `def del` and no destructor.** Everything that needs cleanup is opened with `with`. This replaces `def del` on `shared` types, and the standard library's `Connection` closing when its last owner goes away. `fennec fix` rewrites existing code. Built for the standard library: `net.connect`, `net.listen`, `net.udp` and `server.accept()` are `with` sources, and a `for` over a server closes each connection when the loop moves on. `def del` is out of the language: one is refused, with how to write a source instead. Built. A generator let go while a source's block is open runs that source's cleanup then, as a `break` out of the block would. For one made for a loop, a failure in that cleanup fails the loop at a `break` or a `return`, and a failure already leaving through the loop carries it as a note, as a `with` block's does. One held in a name, let go when its last owner goes, has nothing there to take that failure, so the task stops with it, saying so, as for a crash.
*Why:* A destructor runs at a moment no line shows, and has nowhere to report a failure. Memory frees itself, so only real resources need cleanup, and `with` shows where.

**A `with` source is a function returning `-> with T`, with exactly one `lend`.** Setup goes above it and cleanup below. Code after `lend` runs when the caller's block succeeds, and `lend x except:` runs when it fails or crashes, with the block's own failure passing up. It was `lend x else:` until 2026-11-11. If the cleanup in `except:` fails too, its failure is attached to the block's as a note, and with several handles every one runs its cleanup, innermost first. Several handles in one `with` are built. So is the note, for a `fail`, a whole line's `?` and `x = f()?` in the cleanup. `return`, `break` and `continue` in the caller count as success. Built for a block that fails or ends well; running the cleanup on a crash isn't built yet.
```fennec
def transaction(self) -> with Transaction fails DbError:
    with self._pool.take()? as conn:
        conn.begin()?
        lend Transaction(_conn=conn) except:
            conn.rollback()
        conn.commit()?
```
*Why:* One `lend` makes every source read the same way, open, lend, close, and a user's `with` source can do anything the standard library's can.

**The end of a `with` block is where what it lent finishes, and finishing may fail.** A failure after `lend` becomes the `with` statement's failure, covered by the `?` on its line, so `with fs.create(path)? as file:` covers both opening and finishing. Files close themselves at the block's end, and a transaction commits there and rolls back in `lend x except:`. An early `return` still runs the cleanup, and a failing cleanup makes the function fail. Built for sources a program writes, and for files: `fs.create` writes beside its path and renames into place at the block's end, synced unless `durable=False`, and `fs.open` reads in lines or chunks. Transactions move to it next. This replaces the rule that nothing after `lend` may fail, along with types the caller must finish, how that was inferred, and callers finishing written files with `close()?`.
*Why:* A cleanup failure still reaches the caller, through a `?` that's already there, and there's no `commit()` or `close()` to forget.

**A source can't `lend` while it holds a lock it took.** Opening a file inside a lock is fine.
*Why:* The caller's block would run under a lock it can't see, and could wait in it.

**A generator can't `lend` its items: there's no `Iterator[with T]`.** `for conn in parallel(server)` lends each connection to its own round's task, and that's built in. Chris decided this on 2026-11-09. It was never built. This replaces the plan for a generator to `lend` each item, as `def clients(server: Server) -> Iterator[with Connection]:`.
*Why:* Servers are the one case that needed it, and `parallel(server)` already covers them.

**Tasks started inside a `with` may borrow what it lent.** From many tasks at once, only its read-only methods, like a server's `accept`. `with conn.split() as reader, writer:` gives a connection's two sides, for one task reading while another writes.
*Why:* Those tasks always end before the block does, so the borrow can't outlive the resource.

## Standard library

**`SortedMap`, `SortedSet`, `Heap`, `Grid` and `Packed` are in `std`,** with `SortedMap` and `SortedSet` on B-trees. All of them hide their internals.
*Why:* These are the structures people would otherwise reach for stored references to build. Written once in `std`, no program has to rebuild them, and their layout can improve without breaking anyone.

**Each serialization format implements a `Serializer` and `Deserializer` interface,** with one method per basic value and container. One generic function breaks any type down recursively, so structs need no code.
*Why:* Types don't know about formats and formats don't know about types, so a new format is written once and works with every struct, and a new struct works with every format.

**`def serialize(self, ref out: Serializer)` and `def deserialize(ref src: Deserializer) -> Self fails ParseError` are the serialization protocol,** shaped like `def str`.
- Both are generated from the fields by default: every field, with `@skip` and `@serialize(name=...)` honored, and loading goes through the normal constructor, so `where` rules run. Most types write neither.
- A type overrides both or neither. Writing one alone is a compile error.
- A load goes through the constructor, so a rule that spans several fields belongs in a named constructor, which `from_data` uses, like `Map.from_data(json.loads[MapData](text))`.
- A collection writes them in plain Fennec: `out.sequence(self)`, which goes through `def iter`, or `out.map(pairs)`, read back with `src.sequence[T]()` or `src.map[K, V]()`. `std`'s `list`, `dict`, `Deque` and `SortedMap` do exactly this.
- One pair serves every format, JSON, TOML and `std.pack` alike, so there are still no per-format overrides.
```fennec
struct Ring[T]:
    _items = Buffer[T]()
    _start = 0

    def serialize(self, ref out: Serializer):
        out.sequence(self)                  # its items in order, through def iter

    def deserialize(ref src: Deserializer) -> Self fails ParseError:
        mut ring = Ring[T]()
        for item in src.sequence[T]()?:
            ring.push(item)
        return ring
```
A `Ring[int]` saves as `[3, 4, 5]` in JSON, not as its buffer and `_start`.

**A `Table` saves as a list of its items in order, and an `Id` as where its item is in that list.** Loading makes a new `Table`, and every `Id` loaded with it finds its item there. An `Id` has to be in a `Table` saved with it, one `Table` of each type. In `std.pack`, `exact=True` saves a `Table` slot for slot instead, with the slots taken out and their generations, so a replay's later `add`s hand out the same `Id`s. Built in JSON and `std.pack`.

**A type is serializable when all its fields are.** Otherwise it's a compile error naming the full path, with two fixes: `@skip` the field, or write the two methods.
```
error: `Character` can't be saved: `Character.inventory.on_change` is a function
   = fix: `@skip` the field `on_change`, with a default
   = fix: write `def serialize` and `def deserialize` for `Inventory`
```
A `shared` value, a function value, a `Secret[str]` and an `Instant` are the values that can't be saved. Built: each is refused by name, with what to save instead.
*Why:* The name matches `@serialize`, and never collides with game words like `save` and `load`. Collections reach their natural shapes through the same public protocol any type can use, which replaces the idea that `std` collections map to those shapes inside `std`.

**Structs mirror the wire format.** In JSON:
- Numbers and strings are themselves, `True` and `False` are `true` and `false`, and `None` is `null`.
- Lists, tuples and sets are arrays. Dicts with `str` keys, and structs, are objects.
- An enum case is `"Case"`, or `{"Case": {...}}` when it has data. A union is tagged with its member's type name, as under Functions and types.
- `std` collections take their natural shape through `def serialize`: `SortedMap` is an object, `SortedSet` and `Deque` are arrays, and `Grid` is a list of rows.
- A `Time`, a moment in UTC, is an ISO 8601 string, with fractions of a second only when it has some. In TOML it's a datetime. An `Instant` from the monotonic clock can't be saved, which is a compile error, since it means nothing outside the run that read it.
- `bytes` are a base64 string (RFC 4648, with padding). Loading text that isn't base64 fails with `ParseError`.
- A `Duration` is a string in the form its literals take: `"30s"`, `"1.5s"`, `"200ms"`, `"1h30min"`. Loading accepts the same forms.
- An `int` is its exact digits, all 64 bits of it. `@serialize(string=True)` on a field writes it as a string of digits instead, for readers like JavaScript that lose ints beyond 2^53. Loading that field takes either form.

```fennec
struct Upload:
    at: Time
    data: bytes
    @serialize(string=True)
    id: int
    timeout = 30s

json.dumps(Upload(at=clock.time(), data="hi".encode(), id=9007199254740993))
# {"at": "2026-11-02T07:18:00Z", "data": "aGk=", "id": "9007199254740993", "timeout": "30s"}
```
*Why:* ISO 8601 and base64 are what every other language's JSON reader expects for times and binary data. Ints stay exact by default, since most readers hold 64 bits, and a field a browser reads says so where it's declared.

There's no per-format override, like a `def json` method on a type; one `def serialize` and `def deserialize` serve every format. This replaces the earlier rule that a type overrides one format with a method named for it.
*Why:* What's on the wire is what's in the struct, with nothing hidden.

**Case style is an option on each call,** not on the type: `json.dumps(x, names=.CamelCase)` and `json.loads[T](text, names=.CamelCase)`. The styles are `.SnakeCase`, the default, `.CamelCase`, `.PascalCase` and `.KebabCase`. A name is converted word by word, with no acronym rules: `user_id` becomes `userId` in camel case, never `userID`.

**`@serialize(name="uid")` on a field renames it,** for the rare real case like an API's `"uid"`. A key that isn't a word, like `"first name"`, loads into a dict instead. This replaces `@json(name=...)`.

**Unknown keys are an error when loading,** and `extra=.Ignore` skips them: `json.loads[T](text, extra=.Ignore)`. Against mass assignment, load a request body into a type that holds only the fields a caller may set.

**Every field is serialized by default, private `_` fields included.** This replaces the rule that `_name` fields are skipped.
- `@skip` on a field leaves it out, and the field must have a default, so loading still builds a valid value. `@skip` on a field without a default is a compile error.
- `where` rules run when a value is loaded, as on any write, so a bad save fails with a clear error.
- `@skip` is for derived data. There's no after-load hook: derived data is built by the type's normal constructor from what was loaded, like `Map.from_data(json.loads[MapData](text))`, or computed when it's asked for.

*Why:* Saving a value and loading it gives back exactly the same value, and leaving something out is visible. Private means other code can't touch a field, not that it isn't part of the value, and skipping private fields breaks round-trips silently: a `Grid` saved with no cells.

**Printing is one more format.** `print`, `str(x)` and f-strings all go through it.
- By default a value prints from its fields, through the same generic breakdown as other formats.
- A type that needs something else writes `def str(self, ref out: StrWriter)`.
- `std` collections like `SortedMap` and `Deque` print their items, not their internals.

*Why:* One mechanism covers printing and every data format, and it's how `std` types keep their internals hidden. The method is `str`, not `text`, because `text` would clash with common fields like `message.text`, while `str(x)` already means "make text of this".

**`std.pack` is the default binary format,** written in plain Fennec.
- v1 is flat positional binary: little-endian, small ints in fewer bytes (varints), and no names among the values. Union members and enum cases are written by position. The names go once, before the values, in the type's layout.
- Every pack stream or file carries a fingerprint: a compile-time hash of the exact layout of every type reachable from what's sent, its types, fields and cases, in order. A mismatch is an error value, never misread bytes.
- `where` ranges shrink values: a field that's `where hp >= 0 and hp <= 100` takes one byte.
- Delta encoding is built in, and quantization is opt-in.
- Data from an earlier version of a program loads into the new one: fields are matched by name, and a field the data doesn't have takes its default, as hot reload does. A field with no default that the data doesn't have fails the load. A lock file of past layouts comes later, adding to v1 without breaking it. Chris decided this on 2026-11-07, which settles how `std.pack` data evolves. Built: `dumps` writes the type's layout after its shape's number, every struct's field names and every enum's case names, with what each holds. `loads` into a type of the same shape passes over it and reads in order, as fast as before. Into another shape, it reads by the layout's names: a field that's gone is passed over, a new one takes its default, one with no default fails naming itself, a case or a union's member is found by name wherever it is now, and an `int` that's a `float` now reads as one. A field that's gone and holds a type with its own `def serialize` can't be passed over, since only that type knows where it ends, so that load fails saying so. Data from before layouts, `FPK1`, still loads into the same shape. A layout costs its names, about 100 bytes for a struct of a few fields with an enum in it, on each value `dumps` saves. So many values go through a `pack.Packer`, which writes each type's layout once, before its first value, and each value after with a byte for which layout it has and its length: a hundred small values take 638 bytes, where a hundred `dumps` take 5300. `pack.Unpacker.of(data)` reads them back a value at a time, in order or by name, and `pack.Unpacker.stream()` takes a stream a piece at a time with `feed`, giving each value once all of it has come. A `Packer`'s header can come again partway, starting a new set of layouts, so a file one version of a program wrote can be added to by the next, each value read by its own layout. Built.
- The lock file: `pack.lock` at a kit's root keeps every layout each saved type has had, by its shape's number, a line each, and only ever grows. `fennec update` adds the layout of each type the program `dumps`, as it pins kits in `kit.lock`, and nothing else changes it. The program holds the file, so a change to it is a change to the program. A type whose layout is locked is written without it, as `FPK1` and its shape's number, as short as before layouts, and `loads` reads data written that way by the locked layout's names. Built. Each line names its type, so `fennec check` warns, as it compiles, when a type can't load one of its locked layouts any more: a new field with no default, a field whose type changed past what loads, a case or a union member taken out that saved data may hold, or a field gone that holds a type saving itself. It's a warning, since leaving old data behind can be what's meant. Built. Still planned: the typed message layer sending each layout once, when it connects.

*Why:* Saves and network messages are where games and servers feel size most, and the types already say most of what a compact format needs to know.

**HTTP is read strictly, so nothing in front of a server can read a request differently.** A second `content-length`, `host` or `transfer-encoding`, a length that isn't only digits, a space before a header's `:`, a folded header, a version other than 1.0 or 1.1, and a control character in a method, target or header each get a 400. A response header that would break its line makes the response a 500, and the client refuses a line break or control character in a URL, a method or a header before it connects. Built, in `examples/http_strict.fen`. A blank line or two before a request line is passed over, as RFC 9112 says a server should. Before closing a connection, the server reads and drops what the client is still sending, up to 64 MB, so the system doesn't reset the connection and lose the answer just written, and those before it. A bare line feed ending a line is still accepted, until reading a line can tell.

**A protocol is a union of message types.** `net.connect` stays about bytes, and `conn.messages[GameMsg]()?` puts a typed message layer on any byte stream: TCP, later QUIC, the test fake or a file. It sends in `std.pack` and checks fingerprints when it starts.
```fennec
struct Move:
    unit: Id[Unit]
    to: Vec2

struct Attack:
    unit: Id[Unit]
    target: Id[Unit]

struct Chat:
    text: str

type GameMsg = Move | Attack | Chat

def play(ref conn: Connection, ref world: World) fails NetError:
    mut wire = conn.messages[GameMsg]()?
    wire.send(Chat(text="hello"))?
    for msg in wire:
        match msg:
            case Move(unit, to): world.move(unit, to)
            case Attack(unit, target): world.attack(unit, target)
            case Chat(text): world.say(text)
```
- A fingerprint mismatch is `NetError.ProtocolMismatch(ours, theirs)`. The program decides whether to exit, prompt an update or retry.
- `wire.send(x)` accepts only members of the union; anything else is a compile error.
- Receiving is `for msg in wire:` and `match`, with no handler registry.

*Why:* The union already says every message a protocol has, so `match` checks that each is handled, and both sides compile the same fingerprint from the same types. Keeping the typed layer separate from the connection means one protocol runs over any byte stream.

**A module is in `std` if most programs need it, it's security-sensitive, or it has to be consistent everywhere.** Everything else is a kit. Only reachable code gets compiled, so unused `std` costs nothing.

**`std.net` holds TCP, UDP, TLS, an HTTP client and server, and WebSocket, all through the `Net` capability.** The HTTP server is the plain kind, a function that handles requests; routing frameworks stay kits. Its design takes lessons from Go's `net` and `net/http`, in Fennec's own code. QUIC comes later as a first-party kit.

**`std.sql` is one standard database interface,** modeled on the ideas of Python's DB-API. Drivers are first-party kits, with Postgres and MySQL in pure Fennec.

**`std.sqlite` is SQLite itself, compiled in, under `std.sql`.** `with sqlite.open(fs, path)? as db:` opens a file in WAL mode, `db.execute(t"...")` and `db.query[T](t"...")` take only templates, binding each value as a parameter, and `with db.transaction()? as tx:` commits at the block's end and rolls back in `lend except:`. Rows read into a `T` through the same `deserialize` JSON uses, a struct's fields by the columns' names, and a column that doesn't fit, or a field with no column, fails as `SqlError.Column` with its name. Unknown columns are refused, as unknown JSON keys are. A transaction takes a connection of its own and the database's one writer at its start (`begin immediate`), so `parallel` rounds can share one database. SQLite never waits on a lock itself: a query that finds the writer taken gives its blocking thread back, and its task pauses and tries again for up to 5 seconds, so waiting tasks never hold the threads the writer needs. In a test, each path is a database of the test's own, fresh for each test, kept in a temporary folder that's gone when the test ends, and `--real` uses the path itself. Both are WAL files, so a test sees what a program does: a query outside a transaction reads what was there before the transaction, while it's still going. `std.sql` holds `SqlValue`, `SqlError` and the `Sql` interface, and gives drivers `SqlRows`, which a driver fills with what its database sent back, and `sql.rows_as[T](rows)`, which reads each row into a `T`, so every driver maps rows to types the same way. `std.postgres` is built on it, in Fennec over `std.net`. Its messages have no network of their own, so the same code runs against a socket or against bytes a test wrote. Built: Postgres's messages, signing in with SCRAM-SHA-256 (checked against RFC 7677's own exchange), a query as Parse, Bind, Describe, Execute and Sync, with each value bound apart and rows read by column type, and a connection lending `execute`, `query[T]` and `transaction`, tested against a real Postgres 17. `postgres.connect(net, entropy, url, password=secret)` lends a `PgPool` of `pool_size` connections, 10 unless it's set, kept by the pools of `std.concurrent`, with `transaction()` and `borrow()` waiting up to `wait`, `30s` unless it's set, then failing with `Timeout`. One connects at once, so a wrong URL or password fails at the `with`. The URL may set `pool_size` and `wait`, each in it or as an argument but not both, and a URL with a password in it has to be a `Secret`, with a plain one refused, at compile time when it's written in the source, so a password never shows where a URL is printed. Failures show the URL with its password as `****`. A connection handed back after a block that failed is rolled back and checked with a query of the pool's own, and replaced if the server doesn't answer as it should within 2 seconds. Given a password, a server has to prove it knows it: one that lets the client in with no proof is refused, and one that asks for the password as it is never gets it, unless `allow_weak_sign_in=True` says that's how the server is set up. A commit the server answers with anything but `COMMIT` fails, since that's how it says a failure in the transaction rolled it all back. A connection runs one query at a time and a `lock` can't be held across the network, so one task uses a connection at a time, and tasks in `parallel` each borrow one from the pool. A transaction runs on its database's connection. Its connection is TLS unless `tls=False` says otherwise: it asks with Postgres's `SSLRequest`, reads exactly one byte, and starts TLS over the same socket with `start_tls`, the certificate checked against what the system trusts and the host. A server that won't take TLS is refused, never spoken to in the clear, and there's no mode that falls back. The tests that need a server read `FENNEC_TEST_POSTGRES`, and with none set they check only that the driver builds and reports a refused connection; CI has a Postgres service for them. It lives in `std` until first-party kits have a home. Built, natively: `query` needs native code, as `std.pack` does.
*Why:* SQLite is the most tested database there is, and porting it isn't realistic, so it's compiled in from its own C source behind a reviewed, safe wrapper whose `unsafe` stays its own, with its handles in a table as sockets' are. Hoard keeps its index in it.

**Postgres picks TLS by its host.** A loopback host, like `localhost`, `127.0.0.1` or `::1`, connects in the clear by default. Any other host needs TLS with the server's certificate verified, and a server that won't take it is refused, with no fallback. The URL's `sslmode=` can say otherwise, as `disable`, `require`, `verify-ca` or `verify-full`. `prefer` and `allow` are refused, since each may fall back to the clear without saying so. Chris decided this. Not built yet: today every host asks for verified TLS unless `tls=False` says otherwise.
*Why:* A database on the same machine gains nothing from TLS, and one anywhere else shouldn't be spoken to in the clear by accident. The modes that fall back are the ones that make a downgrade silent.

**`Tree` and `Graph` are in `std`,** both built on `Table`. `Tree` keeps parents and children in sync. `Graph[N, E]` holds nodes in a `Table[N]` and directed edges holding an `E`: `add_node` gives an `Id[N]`, `add_edge(from_id=, to_id=, edge=)` links two, `edges_from` and `edges_to` give a node's `Edge`s, and `topological_order()` and `shortest_path(from_id=, to_id=, cost=)` give `Id`s, or `None` for a cycle or no path. It's `Id`s throughout, with no references. Built, and measured against petgraph in `bench/realworld/graph`. This moves `Graph` up from the list of things cut from the first version.
*Why:* Hierarchies are where `Id` bookkeeping is most tedious and easiest to get wrong, so it's written once.

**`std.process`, `std.crypto`, `std.log`, `std.cli` and `std.csv` are in `std` too.**

**A program's log levels come from `FENNEC_LOG`.** `Log.from_env(env, out=...)` makes a `Log` from it, as in `FENNEC_LOG="info,physics=debug"`: `info` for everything, and `debug` for the part named `physics`. `log.named("physics")` gives a child `Log` with that name, and each child takes its own level from the variable. Chris decided this on 2026-11-09. Not built yet: today a `Log` is made with its `level=` given in code, and has no names. How logging handles its own failures is still open.
*Why:* Turning up one part's logging without a rebuild is what people reach for when something goes wrong, and the levels come from `env`, so nothing global is read.

**`std.crypto` holds hashing, including BLAKE3, so there's no separate `std.hash`.** `blake3` and the other hashes take any iterable of bytes, so a file or a stream hashes without being read into one block first. Built: `blake3` and `sha256` take text, `bytes`, or a list or iterator of either, so `blake3(file.chunks(1_000_000)?)` hashes a file a megabyte at a time. The keyed hashes and HMAC still take text or `bytes`.

**`Secret[str]` in `std.crypto` holds text that can't be printed, logged or saved.** Printing one, putting it in an f-string, a log line or a failure's message, and saving it in any format are compile errors. The text comes out only through one method, whose name says it's revealing a secret, so every place that reads it can be found by searching for that name. Chris decided this on 2026-11-07. Built: `Secret.of(value)` makes a `Secret[str]` or a `Secret[bytes]`, and `expose_secret()` is the one method that reads it. Printing it, an f-string or `str()` of it, and `json`, `toml` and `pack` saving it are compile errors, which name the field when it's inside a struct. A log line takes text, so a secret only reaches one through those. Printed some other way, as through a generic function's `T`, it shows `Secret(hidden)`. `==` on two secrets compares them in constant time, as `crypto.equal` does, so nobody has to remember to call it, and a `Secret` of anything but text or bytes can't be compared, since it can't be compared that way. Built.
*Why:* Keys and passwords often leak through logs and error messages, and a type the compiler checks catches that where it's written, not after it ships.

**First-party kits, not `std`:** `fennec/ml`, database drivers, windowing and input for games, and image formats.

**Names live in their modules.** A standard library module provides nothing until it's imported: `from std.math import Vec3, sqrt`, or `from std import math` and then `math.Vec3` and `math.sqrt(x)`. That's every module so far: `math`, `game`, `layout`, `path`, `json`, `toml`, `pack`, `csv`, `collections`, `random`, `time`, `re`, `log`, `cli`, `net`, `process` and `crypto`. Using a name without its import is reported with the import to add.
*Why:* A file's imports show everything it uses from the standard library, as in Python.

**What the language itself needs is available everywhere.** Capabilities, `Stdout`, `Fs`, `Clock`, `Entropy`, `Args` and `Stdin`, which `main` names to say what the program may do. The errors that standard operations fail with, `IoError`, `InvalidValue` and `ParseError`, so code can match them without knowing which module failed. And the global functions, like `sum`, `range` and `sorted`.
*Why:* These belong to the language, not to one module. A capability in particular can't come from an import: it's handed to `main`, and only what `main` is given exists.

**Network access is a capability, `Net`, like `Fs`.** How it narrows is under Supply chain.

**`std.net` is shaped like Python's `socket` and `requests`, and every call blocks.** There's no `async` or `await`, so no function is coloured by how it waits. A server handles many connections by giving each its own `parallel` task. A task waiting on the network is parked, not its thread, so a few threads serve thousands of connections, as in Go.
```fennec
from std.net import Connection, NetError

def main(net: Net, stdout: Stdout):
    with net.listen(9000)? as server:
        for conn in parallel(server):
            echo(conn) except error:
                stdout.print(f"{conn.peer} dropped: {error}")
                continue
            stdout.print(f"{conn.peer} left")

## Sends back each line it's sent, until the client hangs up.
def echo(conn: Connection) fails NetError:
    for line in conn.lines()?:
        conn.write(f"{line}\n")?
```
- **TCP:** `net.connect(host, port)` gives a `Connection`, and `net.listen(port)` gives a server whose items are connections. `tls=True` on `connect`, or a `Certificate` as `tls=` on `listen`, makes them TLS, and `conn.start_tls(host)` upgrades a plain connection. Both are used in a `with` block, which closes them. A connection has `read(n)`, which gives up to `n` bytes and empty bytes at the end, `read_line()`, which gives `None` at the end, `lines()`, `write(data)`, taking `bytes` or `str`, and `peer`, the other side's address.
- **UDP:** `net.udp(port=9001)` gives a socket whose items are `Packet(data, sender)`, and `send_to(data, address)` answers.
- **A failing connection is handled in its own round,** as in `main` above, since a failure that passes up ends the loop. A crash, like overflow, ends only that task and closes its connection, and the server goes on. This is the containment recorded under Concurrency.
- **Shared state** between connections is a `shared` value made in `main` and passed down, and one task hands work to another through a channel, as under Concurrency.

**The HTTP client is `net.get`, `net.post` and `net.request`.**
```fennec
from std import json

def weather(net: Net, city: str) -> Weather fails NetError:
    response = net.get("https://api.example.com/weather", params={"city": city})?
    if not response.ok():
        fail NetError.Status(response.status)
    return json.loads[Weather](response.text())?
```
- `net.post(url, json=value)` sends a value as JSON, and `body=` sends bytes. `headers=`, `params=`, `timeout=` and `most_body=`, the most a response's body can be, 64 MB unless given, work on every call.
- A `Response` has `status`, `headers` and `body`, plus `text()`, `ok()` for a 2xx status, and `json[T]()`. A 404 is a response, not an error, since what it means is the caller's to say.
- Redirects are followed, up to 10. TLS checks certificates, and there's no option to stop it: a test uses the fake instead.

**Every call that waits takes `timeout=`, a `Duration`.** Connecting and HTTP requests stop after 30 seconds unless told otherwise, so nothing hangs forever by accident. Reads on an open connection wait as long as it takes unless given a timeout, since a server waits on idle clients.
*Why:* Python's `requests` has no default timeout, and hung clients are among the most common bugs it causes.

**Network failures are one error, `NetError`, with a case for each way it fails:**
```fennec
error NetError:
    NotFound(host: str)
    Refused(host: str, port: int)
    TimedOut(after: Duration)
    Closed
    Denied(reason: str)
    Tls(reason: str)
    Status(code: int)
    ProtocolMismatch(ours: Fingerprint, theirs: Fingerprint)
    Other(reason: str)
```
- `NotFound` is a name that didn't resolve, and `Closed` is the other side hanging up during a write. Reaching the end of what was sent isn't an error.
- `Denied` is a narrowed `Net` refusing a host or port: "this Net can reach only `api.example.com`, so it can't reach `evil.example`".
- `Status` is for callers that turn a response into an error, as above. The client itself never returns it.
- `ProtocolMismatch` is a typed message layer finding that the other side was built from different message types, under `std.pack`.

**A test's `Net` is a fake that never touches the network.** `net.script("GET https://api.example.com/weather", Response(status=200, body=...))` sets what a request answers, and `net.requests()` lists what was sent. `listen` and `connect` work in memory, so a test can run a server and a client against each other. `fennec test --real` uses the real network.
*Why:* One API on every platform, shaped like the libraries people and AI already know. Blocking calls in `parallel` tasks keep server code as plain as a script, which is what the MMO's server needs.

**Environment variables and running other programs are capabilities too, `Env` and `Process`,** handed to `main`. `std.process` goes through `Process`. `Process` is the most powerful capability, since a program it runs can reach files and the network too.

**Command-line arguments and standard input are capabilities.** `def main(stdout: Stdout, args: Args, stdin: Stdin):`. `for arg in args:` gives the arguments, not the program's name, which is `args.program`. `stdin.lines()` reads input a line at a time. A program that doesn't name them can't read them, and both are recorded for replay.
*Why:* A program's input decides what it does, so it comes in the same visible way as files and the clock.

**`int(text)` and `float(text)` fail with `InvalidValue`** when the text isn't a number or doesn't fit, handled with `?` or `except`. To be replaced, as Chris locked on 2026-11-11: text is read with `int.parse(text)` and `float.parse(text)`, which fail with `InvalidValue`, and `T(x)` never fails, so `int(text)` becomes a compile error that shows `int.parse`. Not built yet. `int(3.7)` still truncates to `3`. `int()` of NaN, or of a float too big for an int, stops the program, like overflow.
*Why:* Text often comes from input, so a bad number is expected and must be handled. A NaN reaching `int()` is a bug in the program.

**A string's characters are reached by code and as bytes without copying.** `"A".code()` gives `65`, `str.from_code(65)` gives `"A"` and fails with `InvalidValue` for a number that isn't a character, and `s.codes()` gives every code. `.code()` on anything but one character stops the program, like an index out of range. `s.encode()` is a view of the UTF-8 bytes, with no copy, so `len(s.encode())` is the length in bytes.
*Why:* A lexer needs character codes and byte lengths, and strings are stored as UTF-8 already, so the bytes cost nothing.

**`bytes` is binary data, a run of ints from 0 to 255.** It's a built-in type, like `str`: `len(b)` gives its length, indexing and looping give ints, slicing and `+` give `bytes`, and it compares, orders and hashes. `bytes([137, 80])` makes some from a list, and a number outside 0 to 255 stops the program, like an index out of range. `text.encode()` gives text's UTF-8 bytes, and `data.decode()` gives the text back, failing with `InvalidValue` when the bytes aren't UTF-8. `data.hex()` writes them as hex digits, like `"89504e47"`, with `data.hex(" ")` putting a separator between bytes, and `bytes.fromhex(text)` reads them back, failing with `InvalidValue`, both as in Python. Bytes print as `bytes([137, 80])`, which is also how to write them; there's no literal like Python's `b"..."` yet.
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
- An alias can't take a name the language provides everywhere, such as `sum` or `Stdout`.
- An alias to the same name (`from std import json as json`), or importing one thing twice under two names, is a lint.
- `export import x as y` re-exports under a new name, which helps a kit rename part of its API.

*Why:* Two modules with the same function names, like `std.json` and `std.toml`, or two kits with a `utils` module, need a way out other than full paths everywhere. Python developers and AI write `as` without thinking, so refusing it only causes errors. Aliases are resolved at compile time and cost nothing.

**Two modules can have functions with the same name.** Without an import, `loads` doesn't exist, and the error says which modules have it.
*Why:* `std.json` and `std.toml` both have `loads` and `dumps`, as in Python, so the name alone can't say which format is meant.

**JSON and TOML read into plain values.** Objects and tables become dicts in the order written, arrays become lists, and TOML's dates and times stay strings as written. The checker doesn't know the shape of text it hasn't seen, so the values' types are unknown and checked as they're used. Writing takes dicts, lists, tuples and structs. Typed reading, `loads[Settings](text)`, gives a struct, checked as it loads.

**Bad text fails with `ParseError`.** It has the `message`, `line` and `column`, and the message says the line and column too. A value a format can't hold, like a set in JSON or `None` in TOML, stops the program, like overflow.
*Why:* Bad input is expected and has to be handled. Writing a set as JSON is a mistake in the program.

**`Path` is a struct written in Fennec.** Its methods only look at its text: `join` (also `/`), `parent`, `name`, `stem`, `extension` and `normalize`. `extension` has no dot: `"png"`, not `".png"`. `fs` methods take a `Path` or a `str`.
*Why:* Path handling needs nothing the language can't already do, and code written in Fennec is checked and compiled like everyone else's.

**Files and folders go through `fs`.** The `Fs` capability has `read_text`, `write_text`, `exists`, `list_dir`, `make_dir`, `remove` and `metadata`, and each takes a `Path` or a `str`. `list_dir` gives the folder's `Path`s sorted by name. `make_dir` makes any missing parent folders too, and succeeds if the folder is already there. `remove` removes a file, or a folder only if it's empty. `metadata` gives a `Metadata` from `std.fs`, with `size`, `is_file` and `is_dir`. `read_bytes` and `write_bytes` read and write `bytes`. Every failure is an `IoError`. `exists` fails with an `IoError` when it can't look, as in a folder it isn't allowed to read or outside what a narrowed `Fs` reaches, rather than saying `False`, and `fennec fix` adds the `?` where a condition used its answer. Chris decided this. Built. `fs.create(path)` writes a new file and puts it in place at the end of its `with` block, syncing it to disk before the rename unless `durable=False` says otherwise, and `file.sync()?` syncs at any other point. Built.
*Why:* Sorting makes a listing the same on every system, so programs and their tests behave the same everywhere. Making the parents and accepting a folder that's already there is what code almost always wants, like Python's `os.makedirs(path, exist_ok=True)`. Removing only empty folders keeps a wrong path from deleting a whole tree; a recursive remove can come later, under its own name.

**Closing a writer flushes it, and a failed flush fails the `with` block.** Whatever was written but held back is sent when the block ends, and a failure doing so is the `with` line's failure. Chris decided this. Built: every writer `std` lends already does it, and a test keeps `fs.create` doing it.
- `std` will have two interfaces for streams: `Writer`, with `write`, `write_text`, `write_line` and `flush`, and a buffered `Reader`, with `read`, `read_line` and `lines`. Not built yet.
*Why:* A writer that drops what it held back when it closes loses data without saying so, and the end of the block is the one place that can report it.

## Visibility

**`export` marks what other kits can use, and goes on top-level things only.** It goes on each `def`, `struct`, `enum`, `error`, `interface`, `type` alias and re-exported `import` that other kits may use. There's no field- or method-level `export`. It was spelled `pub`, and `fennec fix` rewrites `pub` as `export`, dropping it from methods.
- `_name` makes a field or method private to its type. Only the type's own methods can use it, in its own kit too. Tests get no special access.
- A top-level `_name` (function, type or constant) is a compile error, and `fennec fix` renames it. Top-level names are already private to the kit. The standard library follows the same rule, with no exemption.
- Enum cases, methods and interface methods come with their type and have no `export` of their own.
- Other kits match an exported type's fields by name: `case IoError(reason=r)`, `case Point(x=a, y=_)`, or `case IoError()` to match the type without reading a field. Matching another kit's type by position is a compile error, and `fennec fix` adds the names. A kit's own code keeps positional patterns. Cases with no fields, like `.Wait`, are unchanged.
- Fields with defaults come after the ones without, as parameters with defaults do; a field without one after a field with one is a compile error.
- The automatic constructor sets the public fields, and `_` fields take their defaults. A `_` field with no default needs a named constructor. A type nobody can build is an error.
- Writes from another kit still run the field's `where` rules.
- An exported signature can't mention anything that isn't exported.
- A lint flags exported items that no other kit can reach.

**A kit's API is checked against its last published version,** cached from the Den. When a change would break other kits, `fennec check` says so at once, in the editor: "`load` can now also fail with `ParseError` (since v1.2.0). This is a breaking change: handle or wrap it, or release v2.0." `fennec publish` refuses a version number that undersells the change. Adding an error case is a breaking change; removing one isn't.

A big program that wants walls between its own parts splits into local kits in one workspace.
*Why:* One rule covers every field, and `export` appears only where API stability matters, never in apps. Go's capitalization rule would clash with lowercase built-ins and capitalized user types. What keeps a library free to add a field with a default is two rules together: defaults come last, so the new field goes at the end, where no value given by position can land in it, and the API checker flags any other change to an exported type as breaking. Named patterns across kits mean the new field never breaks a `match` either. A rule that other kits build an exported type by name only was considered and dropped: these two already cover it, and `Path("assets")` stays short.

## Files and kits

**A kit is a folder with a `kit.toml` and its code under `src`.** `src/main.fen` holds `main`, and the module `enemies.ai` is the file `src/enemies/ai.fen`. `kit.toml` has `name`, `version`, `edition` and `fennec`, the toolchain the kit is built with (see Toolchain). Until the launcher exists, a kit asking for a newer Fennec than the one running is an error. Its `[dependencies]` table names kits from the Den, or from git or a path (see Supply chain). `fennec run` and `fennec test` with no file run the kit around the current folder; `fennec test` runs the tests in every file under `src`.
*Why:* One layout means no configuration, and finding a module's file needs no search.

**The file extension is `.fen`, and that's closed.** Built. Chris closed the question on 2026-11-07.
*Why:* It's short and already in every kit, tool and editor setting, so changing it would cost everyone and buy nothing.

**A file uses another file's names only after importing them.** `from enemies import spawn` brings in names, and `import physics` brings in the module, used as `physics.step`. For a module in a folder, `import world.physics` is used as `world.physics.step`, and `from world import physics` as `physics.step`. A file's own names need no import.
*Why:* Where every name comes from is visible in the file that uses it, for people and for AI.

**A kit's files share one set of top-level names.** Two files of a kit can't both define `update`, even when one is reached by its module's name; the error names both files. Built.
*Why:* A name means the same thing everywhere in the kit, and a value always prints with the one name its type has. Methods belong to their type, so the names that clash are few.

**Imports can't go in a circle.**
*Why:* Two files that need each other usually share something that belongs in a third.

## Concurrency

**Four questions, each with its own words:** `mut` and `ref` say whether something can change; `shared` and `lock` say who's touching it; `with` says when it goes back; `launch` and channels say who talks to whom.
*Why:* Each question gets one answer you can see in the code, and every borrow lives in a block, so there are no lifetimes to write.

**`shared T` is a type, and it's the only way tasks reach the same value.** It works on any type, and goes in fields and collections: `dict[str, shared Room]`. Making one from a plain value is written out, as `shared rooms = dict[str, Room]()`, and a plain value never becomes `shared` at a call. This replaces `shared Name:` declarations, whose methods locked automatically: one is refused, and `fennec fix` rewrites what's mechanical of it. Built.
*Why:* Sharing then shows up in types and signatures, and a search for `shared` finds every place it happens.

**There's no top-level shared state.** A `shared` value is made in a function, usually `main`, and passed to whoever needs it, like a capability.
*Why:* A function's signature then says what shared state it can touch, so nobody has to read its body to find out.

**A `shared` value is reached only inside `lock`.** `lock x as s:` reads, and many tasks can read at once. `lock x as ref s:` changes it, one task at a time. Any way out of the block releases it. `return`, `break` and `continue` keep the changes made so far, and a crash undoes them, as a `fail` or `?` will, below. There's no upgrading a read to a write. This replaces `with x as ref y:` for locking, which is refused, with a fix to `lock`. Built, apart from a `lock` inside another, which is refused for now: take them together in one.
```fennec
def record(scores: shared Scores, name: str, points: int):
    lock scores as ref current:
        current.best[name] = points
```
*Why:* The block shows exactly how long the lock is held, and code can't touch the value without it.

**Nothing waits inside a lock.** No channel send or receive, pool take, network call, `clock.sleep`, `parallel`, `lend`, or end of a `with launch`, traced across the whole program. Printing and local files are fine. This replaces the rule that I/O inside a lock only makes it slow: network calls now count as waiting.
*Why:* A lock held while waiting on another task, or on a stalled client, can freeze every task, so the rule is to wait first, then lock.

**Lock deadlocks can't compile.** Lock order is checked across the whole program, keyed by where each `shared` value is made. Values the compiler can't tell apart, like two accounts from one dict, are locked together in one `lock` statement, never one inside the other, and locking the same value twice there fails rather than stopping the program. Interfaces and function values are traced across the whole program, with no annotations, and an error names the implementation that takes the lock.
*Why:* Every program is compiled whole from source, so the compiler can see every lock any call might take, and nobody writes lock annotations.

**A `shared` value holds plain values and other `shared` values, never something borrowed from a `with`.** It can't hold one of its own type or form a cycle of types, directly or through other types and channels, so trees use `Table` and `Id`s. It can't store a function that captures a `shared` value. It's freed when its last holder lets go, with no cleanup code.
*Why:* With no cycles nothing can leak, and with no resources inside, freeing it is only giving memory back.

**`with launch(...)` runs a task until its block ends.** The arrow is the call you'd write anyway, and `launch`'s own options, `stop_after=` and `inbox_size=`, sit outside it. A parameter typed `Inbox[T]` makes `launch` create the channel, giving the task the receiving end and you the sending end. A task with no messages takes none: `with launch(() => autosave(rooms, fs, clock))?:`. `launch` is only ever written as `with launch(...)`, and the `?` on its line covers the task's failure and `Crashed`. This replaces `with launch(physics_loop) as physics:`, where the first parameter was the inbox. Built natively: the task runs on the runtime's threads, with what it captured copied for it, and its failure or `Crashed` passes up where the block ends.
```fennec
with launch((inbox) => audio(inbox, speakers), stop_after=5s)? as sounds:
    sounds.send(.Play(name="jump", volume=0.8))?
```
*Why:* `launch`'s options can never collide with the function's arguments, and the inbox is in the task's signature, not a convention about its first parameter.

**An inbox's enum is its protocol.** You can only send real cases, and the task's `match` must handle every one.
*Why:* A typed message is checked at both ends at compile time.

**There's no `TaskGroup`.** Chris cut it on 2026-11-09. It was never built. `to=.Every` and `when_behind=` on channels stay. This replaces `TaskGroup`, in `std.concurrent`, for tasks whose number is known only at run time.
*Why:* `parallel` and `with launch` cover the programs written so far, and every task still belongs to a block you can see.

**Stopping a task drains it, then cuts it short.** When a `with launch` block ends normally, the task's inbox closes and the task finishes what's in it. If it's still running after `stop_after`, every wait it's in is cut short, socket and stdin reads included. A block that failed cuts them short at once. A wait cut short by a stop ends the task quietly, with no failure reported.
*Why:* A normal end never loses queued work, which would make tests flaky, and a stuck task can't hang its parent forever.

**A task's failure comes out where its `with launch` ends, and a crash comes out as `Crashed`.** A crash ends only its own task, and the HTTP server answers that request with a 500.
*Why:* A parent, like a pool's keeper, has to see a child die to replace it, and one bad request shouldn't stop a server.

**A crash inside `lock x as ref s:` rolls back the block's writes.** The compiler saves the old value before each write, and on a crash the runtime puts them back in reverse order, then releases the lock. Today only crashes roll back. A `fail` or `?` will too, below. The compiler skips the saves where it proves the block can't crash, and values taken out are freed when the block ends. There's no new syntax, no new failure type and no poisoned lock. This replaces the rule that a crash while holding a `ref` lock stops the program. Built.
*Why:* Nobody ever sees a half-change, and one crashing task no longer takes everything else down with it.
The build lanes make this one general piece that undoes the writes in a block. Other uses wait until a real program asks for them, like a failing function leaving its `ref` arguments unchanged, or undo in a game.

**A `fail` or `?` leaving `lock ... as ref` rolls back the block's changes, as a crash does,** so a write lock either finishes or has no effect, as a database transaction does. `return`, `break` and `continue` keep the changes.
- Rollback covers one `lock` statement, including all the values locked together in it. An inner lock that already finished stays committed, so values that must change together are locked in one statement.
- Local variables and output aren't rolled back.
- To keep a change and still fail, decide inside the lock and `fail` after it.
- A lint may warn when a `fail` follows a write in the same block.
- Chris decided this on 2026-11-08. Built. This replaces the rule that only crashes roll back.

*Why:* A failure halfway through a write would otherwise leave a half-change for the next reader, and finishing or having no effect is the rule people already know from database transactions.

**Channels are bounded.** `send` waits when one is full, and both ends are plain values, so there can be many senders and receivers. `send` and `receive` take `timeout=`, and `receive` fails with `Closed` or `Timeout`. Sending to a closed channel fails at once. A channel you set up yourself opens with a plain function in `std.concurrent`, `with channel[Job](size=64) as jobs:`, as Python's `open()` is, not by calling the type. Chris decided this on 2026-11-08. Built natively, written in Fennec over channel intrinsics. `Channel`'s constructor is private, so `channel()` is the only way to make one.
*Why:* A bounded buffer pushes back instead of growing without limit, and saying why a receive stopped lets the task decide what to do.

**Dev builds stop with a report when tasks are waiting only on each other.**
*Why:* Lock deadlocks can't compile, but two tasks sending into each other's full inboxes can't be ruled out at compile time, so it has to show up loudly in testing.

**A database connects as a pool, and a keeper lends each connection for one block.** `postgres.connect(url)` always lends a pool, with no single-connection form.
- `pool_size=10` and `wait=30s` unless given. Either can be given as an argument or in the URL, and giving one in both is an error.
- `with db.transaction()? as tx:` takes a connection for a transaction, and `with db.borrow()? as conn:` takes one for a block of statements. When every connection is in use, the block waits up to `wait` for one.
- Each connection lives inside one keeper task's `with`. The keeper lends it to one block at a time and gets it back when the block ends, so nothing can leak or be used twice. Nothing is sent to the keeper to run: the block uses the connection itself.
- A block that fails or crashes has its connection reset before the next block gets it. If the keeper can't prove the connection clean, it replaces it, so the pool heals itself and never shrinks.
- Pools are written in plain Fennec in `std.concurrent`.
- Chris decided this. Built: `pool[T](open=..., size=, wait=)` in `std.concurrent`, its keepers each a task of its own, and `postgres.connect(net, entropy, url, password=)` lending a `PgPool`. A block that fails and one that crashes run the same cleanup, so both have their value reset, and Postgres's reset keeps a connection only when the server proves it clean. This replaces pools as keepers that blocks send jobs to.
*Why:* No connection ever leaves its keeper's block, so nothing can leak or be used twice, and the pool needs no powers a user's code lacks. A block that uses the connection itself reads like code with one connection, and a pool by default means a server never has to be rewritten when it gets busy.

**Not in the language: `del`, destructors, moves, lifetimes, `unsafe` and global state.** Left for when real programs need them: `select`, handles that move between tasks, and results returned from tasks.
*Why:* Each would bring back a hidden moment or a hidden owner that the rules above keep visible.

**One `Channel[T]` serves one receiver or every listener.** By default it delivers each message to one receiver, as a work queue. `to=.Every` gives every listener every message, and receiving reads the same either way, `for msg in ch`. With `.Every`, `send` never waits, and a listener that falls behind fails loudly, unless it asked for `when_behind=.DropOldest`. There's no separate events type. This replaces the plan for separate event channels.
*Why:* One type with one way to receive is less to learn, `send` never stalling keeps a slow listener from holding up the rest, and nothing is lost silently.

**A long-running loop should never wait on another.** It takes what's waiting, does its work and publishes. Waiting belongs to simple consumers at the end of a one-way chain.
*Why:* Deadlock needs someone waiting, so two loops that never wait can't deadlock.

**There's one scheduler, with I/O multiplexed by the OS.** Code looks blocking, and a task that waits parks and frees its core.
*Why:* Thousands of parked tasks cost almost nothing, and nobody has to manage a pool or color functions `async`.

**`?` means the same inside `parallel` as anywhere else.** A failure in one round passes up out of the loop once the other rounds stop, for any source, with no special case for servers. To let one piece fail alone, handle it in the body, with `match` or `else: continue`. `break` stops new rounds from starting and lets the running ones finish, and a failure passing up cuts them short, like a failed `launch`. Each connection a server lends closes when its round ends, however it ends. `server.serve(handler)` turns a handler's failure into a 500, in plain Fennec. Built, servers included, apart from `server.serve(handler)`, whose handler can't fail yet. This replaces the pick that `?` inside `parallel` ends only that task.
*Why:* One meaning for `?` is easier to read than a special case, and whoever writes the body decides what a failure there should do.

**A crash in a server task stays in that task.** A runtime failure in a task of `parallel(server)` ends that task and closes its connection, and the server goes on, as under `std.net`. A crash inside a `ref` lock rolls back that lock's writes first, as under Concurrency.
*Why:* One bad index in one request shouldn't drop every connection. Tasks don't share state a failure could corrupt, which makes containment unusually safe here.

**Reference counts are biased for values that cross tasks.** The thread that owns a block counts cheaply, and other threads count atomically. Programs with no tasks compile the check away.
*Why:* This resolves a conflict between two earlier rules: tasks read parent values without touching counts, and a function returning a slice bumps a count. A task calling that function would have raced on a non-atomic count. Plain reads still touch no counts, and a bump from a task is now safe.

**Tasks are scoped.** Every task ends before the `with` block or `parallel(...)` call that started it. There are no detached threads.
*Why:* Because the parent outlives its tasks, a task can read the parent's values through views without touching reference counts. There's no `Send` or `Sync` to write.

**`parallel` takes only collections and streams,** with no new keyword and an optional `limit=`:
```fennec
for ref particle in parallel(particles):
    particle.position += particle.velocity * dt

total = sum(e.mass for e in parallel(enemies))
hits = [check(b) for b in parallel(bullets, limit=8)]

with net.listen(8080)? as server:
    for conn in parallel(server):
        handle(conn, app)
```
- Each iteration can change only its own element. It can read anything around it, but can't change outer `mut` names.
- A body may use capabilities and `shared` values. Results are deterministic when it uses neither. This replaces the earlier rule that a body can't use capabilities.
- Work over data runs on a work-stealing thread pool, and reading outer values changes no reference counts.

*Why:* One word covers data across cores and connections from a server, and everything in it ends when its loop ends.

**Tasks change only separate pieces.** `for ref e in parallel(enemies):` gives each iteration `ref` access to its own element, checked by the same rule that makes `ref` exclusive. Results combine through reductions, like `sum(... for e in parallel(xs))`.

**`parallel` splits a collection by position.** Anything with `def len` and an `int` `def []`, plus `def is_live` if it has one, splits with nothing more to write. These are the same positions `for ref` writes back through.
```fennec
def step(ref units: Table[Unit], dt: float):
    for ref unit in parallel(units):
        unit.pos += unit.vel * dt
    alive = sum(1 for unit in parallel(units) if unit.health > 0)
```
- A reduction combines in a tree whose leaves are ranges of at most 1,024 positions, set by the length alone, as under Determinism. How work is scheduled for a `for` loop or a comprehension may still use the element size, since their order doesn't show in results: the work-stealing pool halves a range when a thread runs out of work, down to a smallest chunk the runtime picks.
- `for ref x in parallel(xs)` gives each task a disjoint range. That's safe only if writing position `i` touches nothing but element `i`, so the compiler checks that the `ref` form of `def []` returns a place rooted in one field of `self` and indexed by `i` itself, like `return self._items[i]`. If it doesn't, `for ref` over `parallel(...)` is a compile error naming that method, and a plain `for ref` still works.
- `list` and `Buffer` split into contiguous ranges, which also vectorize. `Table` is cut at its chunk boundaries, so a task's writes touch only its own chunks. `Grid` is cut at rows. `Packed` gives each task whole lists to read. `dict`, `set`, `SortedMap` and `SortedSet` are read-only in parallel, split by their own internal ranges.
- A type with only `def iter(self)` and `yield` can't be split, since each item depends on the last. It works as a source, as below, and `for ref` over it in parallel is a compile error that says to add positions.

*Why:* The positions a type already has for `def []` and `for ref` are exactly what splitting needs, and a fixed combine order falls out of splitting by length. Work-stealing libraries elsewhere have each type implement a splittable producer instead: a second interface every collection writes, whose writes the compiler can't check without lifetimes. Here a user type gets real parallelism for free, and one that can't prove separate writes still runs, in order.

**What `parallel` keeps in order:**
1. `for x in parallel(xs):` runs the body in parallel, in any order.
2. `[f(x) for x in parallel(xs)]` runs in parallel, with results in input order.
3. `(f(x) for x in parallel(xs))` does the same, streamed in order through a bounded window. A slow consumer makes the workers wait.
4. Working in parallel and then going in order is (3) followed by a plain `for` loop.
5. A generator works as a source: it's read in order, and the work runs in parallel. Pure stages feeding `parallel` may be fused into the workers by the compiler, invisibly.

```fennec
results = (simulate(seed) for seed in parallel(seeds))
for result in results:
    stdout.print(result)        # in input order, while later seeds still run
```
There's no `ordered=` option and no `map()`; comprehensions are the one form.
*Why:* Each form already says what order it needs. A loop body has no result, a list keeps its positions, and a stream is consumed in order, so none of them needs an option. A bounded window keeps a fast producer from filling memory.

**Printing inside `parallel` follows real call order.**
- Each `print` comes out whole, with no lost lines and no text mixed within a line.
- One task's prints stay in order. Across tasks, output follows real call order and can differ between runs.
- Output appears when it's printed: a task writes its output before it parks or waits, and output is never held until the call ends.
- Anything printed after the loop comes after all the loop's output.

For input order, use the streamed form:
```fennec
for line in (render(x) for x in parallel(xs)):
    stdout.print(line)
```
Tests comparing against pinned outputs sort output from parallel bodies, and heavy printing from many threads shares one output. Those are the accepted costs.
*Why:* Position order would let one long-lived connection hold back every later connection's logs, for hours and in memory with no limit. Having one rule for lists and another for connections would be worse, since the same loop would print differently by what it runs over.

**A value crosses a channel like it goes anywhere else.** It's moved at its last use, and otherwise costs one count bump. Any write copies first, so neither side sees the other's changes. `shared` values can cross too. This replaces the earlier rule that only values with a single owner cross a channel.
*Why:* Biased counts make it safe, so the restriction no longer buys anything.

## Syntax

**Not added for now: `filter`, `where`, `->`, `char`, pipes or range types.** That's `units.filter(is_alive)` and `units.where(...)`, where a comprehension or generator already says it, `target_id->health` for following an `Id`, and a `char` type. `where` rules on fields stay, and so does `->` in signatures.

**A module can hold constants.** `KEYWORDS = {"def", "if", "else"}` at the top level is an immutable name, computed when compiling from literals, collections and calls to functions that take no capabilities. There's still no `mut` at the top level, so there's no global state. UPPER_CASE is a convention the lint suggests, not a rule.
*Why:* A table rebuilt inside a function on every call is what people write only when the language leaves them no choice. A constant is computed once, before the program runs.

**Constants are computed at compile time when they can be,** with no syntax to ask for it: no `comptime` and no `const fn`. A top-level constant built only from literals and pure functions, which take no capabilities, is evaluated while compiling and baked into the program.
```fennec
from std.math import PI, sin

SINE = [sin(float(i) * PI / 512.0) for i in range(1024)]    # one full turn
```
- A small evaluator inside the compiler works them out, over the code a build makes of them. A constant reaching what it doesn't do yet, like a generator or a `shared` value, is computed at startup instead, as one over the budget is.
- Evaluation has a build-time budget. A constant that goes over it is computed at startup instead, with a hint naming it.
- It applies only to top-level constants.

*Why:* Purity is already known from capabilities, so the compiler can tell what to evaluate without the programmer marking anything. It covers what other systems languages need `const fn` or macros for, and Fennec has no macros.

**`embed_file("path")` gives a file's bytes, and `embed_folder("path")` a folder's, baked in at build time.**
```fennec
MAP = embed_file("assets/level1.bin")
SHADERS = embed_folder("assets/shaders")
```
- `embed_file` renames today's `embed`, and gives `bytes`. `embed_folder` gives a read-only map from relative path, always written with `/`, to `bytes`, in sorted path order. It's a `std` type over two read-only lists, the sorted paths and their bytes, with `get(path) -> bytes?`, `len()`, and `keys()`, `values()` and `items()` in sorted order.
- Both are views into the program's read-only data: no copy, nothing loaded at startup, and readable whole or in chunks, since the OS pages it in on demand.
- The path is relative to the source file. A missing path, or anything outside the kit, symlinks included, is a compile error.
- The kit's edge is the folder holding the nearest `kit.toml`, and for a lone `.fen` file with no `kit.toml`, the file's own folder. Embedding anything above that folder, or anything a symlink reaches outside it, is a compile error. In Chris's words, it's "wherever the kit.toml is, it cannot be above it", and "only things in that folder". This closes the hole where a lone file's `embed` could follow a symlink out of its folder.
- `embed_folder` leaves out hidden files, and nothing is compressed.
- No `Fs` capability is needed, since the files are inputs to the build, tracked by their content. It's the same in development and release builds.
- Chris locked this on 2026-11-08. `embed_file` is built, and `fennec fix` renames an old `embed(...)` to it. It refuses a missing path and, in a kit, anything outside it, a symlink included. A lone `.fen` file stops at its own folder, as the rule above says. `embed_folder` is built: an `EmbeddedFolder`, with `get(path) -> bytes?`, `len()`, and `keys()`, `values()` and `items()` in path order. A file or folder added or taken away is seen at the next build. Native code makes each file once, the first time it's reached, until it holds them in the program's read-only data.

*Why:* Games ship levels, shaders and fonts inside the binary. This is what other systems languages use an `include_bytes!` macro for. There are two names because a single `embed` would have a type that depends on what's on disk.

**Durations have literals:** `30s`, `1.5s`, `200ms`, `16ms`, `5min`, `2h`. The units are exactly `ns`, `us`, `ms`, `s`, `min` and `h`. Minutes are `min`, never `m`, so a duration can't be read as meters. Durations are the only unit literals: there's no `kb`, `px` or `deg`.
```fennec
clock.sleep(16ms)
response = net.get(url, timeout=5s)?
```
*Why:* Timeouts and frame times are written everywhere, and `5s` reads better than `Duration.seconds(5)`. With durations the only unit literals, a literal never means two things.

**On Windows, the runtime asks for a 1 ms timer with `timeBeginPeriod(1)`,** so `clock.sleep` and timeouts wake within about a millisecond, not at Windows' default tick of about 15.6 ms. Chris decided this. Not built yet.
*Why:* A game sleeping `16ms` a frame would otherwise sleep up to 31 ms, and macOS and Linux already wake to the millisecond.

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
- It's a handle: `b = a` is two names for one generator. A second use is a compile error where that can be proved. Otherwise it fails at run time with `this generator has already been used`, never quietly giving nothing. Built: a second use on every way on from the first is the compile error, by any of its names; a use in only some branches, or in a loop's body, is left to the run.
- A chain of stages is fused into one loop where possible. `map` can come later, and `filter` isn't added for now.
- It isn't a list: `gen()[0]` and `len(gen())` are compile errors. `list(gen())` keeps the items.
- What it sees is fixed when it's made, as with a nested `def`. `(x * k for x in xs)` works out `xs` at once and copies `k`.
- Written right inside `sum`, `min`, `max`, `sorted`, `list`, `set`, `dict`, `any` or `all`, it runs in place, and may read anything, but it can't change the names around it, which is a compile error: use a loop for that. Built. This replaces the rule that such a generator can change the names around it.
- `yield` is allowed inside `with`. A generator left partway closes what it holds when its owner goes away.
- A failure comes out when the item that fails is asked for, and acts like a failure from a call: the loop exits, `with` blocks close, and it passes up. A generator that wants to go on past bad items yields errors as values. Getting one that can fail takes `?`, as in `for x in checked()?:`.
- Printing one shows `<Iterator[int]>` and doesn't use it.

A stage used once becomes part of what uses it. A stage kept in a name that doesn't change moves to where it's used, leaving behind what it works out when it's made, and a `for` over a stage becomes a plain loop, with a yield function's body copied in. The order things happen in stays the same, so this is purely about speed. Anything that can't fuse, like a stage that's passed around, still works, by asking for each item.

*Why:* Streaming lets a chain of stages read like the steps it is, handles input too big to hold, and can compile to a single loop with nothing allocated per item. Being used once is what keeps that honest: a second loop quietly finding nothing is Python's trap.

**A type is loopable when it has `def iter(self) -> Iterator[T]`,** written with `yield`, so it makes its items one at a time and never builds a list. `for x in thing` calls it, and `list(thing)` makes a list when one is wanted. The standard library's own collections loop this way too.
```fennec
struct Ring:
    slots: list[int]
    start = 0

    def iter(self) -> Iterator[int]:
        for at in range(len(self.slots)):
            yield self.slots[(self.start + at) % len(self.slots)]
```
- `for ref` over a type with only `iter` is a compile error that says to add positions, since the items it makes are copies and a change to one would be lost. `list`, `Buffer` and `Table` keep `for ref`.
- A loop over `iter` fuses into a plain loop where it can, as any stage does.

*Why:* A loop shouldn't copy a collection before it starts, and a `break` should stop the work. A `for` that quietly lost its changes was the bug this replaces.

**`t"..."` makes a `Template`, and injection can't be written.** It's Python 3.14's t-string: a `std` `Template` holding the literal pieces and the values separately, with the values' type coming from where it goes, as in `Template[SqlValue]`. A `str` is never a `Template`, and `db.query`, `html.render` and `process.run` take Templates, so text joined from strings never reaches a database, a page or a shell. There are no per-library string prefixes. Built. This replaces the proposal of `sql"..."` templates and separate escaping types.

**`html.render(t"...")` puts each value where it can't change what the page does.** Escaping alone doesn't keep the promise that injection can't be written: an escaped value is still code inside `<script>` or an `onclick`, and still a `javascript:` link in an `href`. So `render` reads the template's own text as a browser does, to find where each value sits. Between tags and in a quoted attribute it's escaped. Inside `<script>` or `<style>`, an `on...`, `style` or `srcdoc` attribute, an attribute without quotes, a name or a comment, escaping isn't enough, and the page fails with `HtmlError.Misplaced`, naming the value. So does a value in a link's attribute that would make the link anything but `http`, `https`, `mailto` or relative. It gives an `Html`, which goes into another page as it is, so pages are built from pieces, and a template that ends inside a tag fails, so a piece can't open what another closes. An `Html` is never loaded from saved data. Built, natively.

*Why:* The alternatives each leave a hole. Escaping everything the same way leaves scripts, handlers and links open. Refusing every value outside text makes ordinary attributes unwritable. A bad link fails rather than being replaced, as Go's `html/template` replaces one, because Fennec doesn't substitute silently: a program that wants a fallback checks the URL before rendering.
*Why:* One form covers every kind of injection, it's what Python people already learn, and the type stops the mistake before it compiles.

**`LiteralStr` is a string that came from the source code,** as Python's `typing.LiteralString` is. Only a string literal, or literals joined with `+`, makes one. It goes anywhere a `str` goes, but a `str` is never a `LiteralStr`. A `Template`'s text pieces are `LiteralStr`, so building one from text read while the program runs is a compile error, and any function may ask for one. It's for strings only. Built.
*Why:* It keeps "injection can't be written" true for dynamic queries too, where user input chooses between literals through a `match`.

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
- A guard is read-only: it can't take `ref` arguments, so it can never change anything.
- A guarded arm never counts toward covering every case, so the exhaustiveness check stays sound: a `match` still needs an unguarded arm for each case, or it doesn't compile. Arms are tried in order.

*Why:* An or-pattern says once what several arms would repeat, and a guard keeps a condition in the arm it belongs to. Neither costs anything at run time.

**`"  " * depth` repeats a string.** It's `str`'s own `def *`.

**Fields may be named any word, keywords included; locals and functions can't be keywords.** `type: str`, `user.type` and `User(type="admin")` all work, since a field name is unambiguous where it's declared, after a dot and before `=`. This replaces the rule that field names can't be keywords.
*Why:* Structs mirror the wire format, and real formats use keys like `type`, `class` and `from`. Locals and functions stay as Python has them, where a name that's sometimes a keyword would be harder to read.

**f-strings have `{x=}` and no `!r`.** `f"{hp=}"` gives `hp=100`. Format specs like `{x:.2f}` stay, after the `=` too: `{total=:,}`. As in Python, what's written is shown with its spaces, so `{ hp = }` gives ` hp = 100`. Built.
*Why:* `{x=}` is what people want when debugging; `!r` has no meaning without Python's `repr`.

## Standard library

**Failures inside the standard library point at the program's call.** When a standard library function written in Fennec fails, the error shows the line in the program that called it, in both engines.
*Why:* The library's own source isn't something the reader can fix, or even see.

**Taking from an empty `list`, `Deque`, `Heap` or `PriorityQueue` gives `None`.** `pop`, `popleft` and `peek` return optionals, so the checker makes the empty case visible: `next = todo.pop() else break`. A list's `pop(at)` still fails on a position that isn't there, as `items[at]` does. Built.
*Why:* Draining a list or a queue until it's empty is the normal way to use one, so the empty case is expected, not a mistake.

**Regular expressions take linear time.** `std.re` runs patterns on a Pike VM, which follows every way a pattern could match at once, so no pattern can make matching blow up. There are no backreferences or lookarounds, which need backtracking. A pattern written in the program is checked before it runs; `Regex(text)` with a pattern built while it runs gives a result, like any struct with a `where` rule.
*Why:* A pattern from a mod, a config file or a player can't hang a game or a server.

## Toolchain

**Windows ships self-contained, after a license check.** `opt`, `llc`, `llvm-link`, `lld` and MinGW's libraries ship with `fennec`, so nothing else needs installing. A person has to check the licenses first.

**Every release ships LLD,** LLVM's linker, beside `fennec`, so on Linux and Windows building a program needs no system linker or C compiler. macOS still needs Xcode's command-line tools for now, from `xcode-select --install`, since linking there without them needs Apple's SDK stubs. Shipping stubs of Fennec's own is deferred. Chris decided this on 2026-11-09. Not built yet: Windows releases ship LLD, and on Linux programs still link through the system's `cc`.
*Why:* A linker that comes with the release links the same way on every machine, and a missing or different system linker can't break a build. Apple's SDK stubs are more to ship and keep current, and most people building on a Mac already have Xcode's tools, so macOS waits.

**Editions let syntax change after 1.0.** A kit names its edition in `kit.toml`, as in `edition = "2026"`, and `fennec fix` moves it to the next one.

**Each Fennec version is one self-contained compiler binary.** It holds `run`, `build`, `fmt`, `test`, and Den search and install, with LLVM linked into it. It needs no system libraries and no separate LLVM download.
*Why:* Installing a version is copying one file, the way Zig works. Nothing on the machine can be missing or the wrong version.

**A tiny, stable launcher is what you install, and it needs no Homebrew.** It reads the version a kit asks for, `fennec = "0.4.2"` in `kit.toml`, with no separate version file. If that version isn't there, it fetches its binary into `~/.fennec/toolchains/0.4.2/` on first use, verifies its checksum and signature, records its hash in `kit.lock`, and hands off to it.
- Only the launcher updates itself, and an update needs 2 of the 3 release keys, as below.
- A pinned compiler never changes by itself, and nothing fetches "latest" without being asked.
- `fennec toolchain list`, `install` and `remove` manage toolchains.
- A kit's version is `fennec = "0.4.2"` at the top of its `kit.toml`, found by walking up from the current folder.
- A script outside a kit runs with `--version 0.4.2`, or else the version `fennec default 0.4.2` set. With neither, the command stops and says how to set one. In a kit, `--version` is an error, since `kit.toml` says.
- `fennec +0.4.2 build` runs that version anywhere, this once, and changes no pin.
- `fennec toolchain link dev <path>` is for whoever works on Fennec itself: it runs the `fennec` in a folder of their own, like a checkout's `target/release`, with whatever LLVM that build finds, and fetches nothing. A kit asks for it with `fennec = "dev"`, which is never pinned, and `+dev` runs it once. `fennec toolchain remove dev` unlinks it and leaves the build.
- `fennec doctor` reports how much disk the toolchains and the store use, and offers to remove versions no kit uses anymore.
- A release's archives are the same bytes however often and wherever they're built, so anyone can build one again from its tag and check it against the signed manifest, and CI builds each platform twice and compares them.
- Built, as `fennec-launcher` for now: running a kit's pinned toolchain, `kit.lock` holding its signed manifest's hash, fetching only what's missing, `--offline`, `fennec update`, and `fennec toolchain`. Releases built the same twice is built too, on macOS, Linux and Windows. Not built yet: the release keys, which Chris makes (`docs/releases.md`), so nothing verifies today; the launcher updating itself; and `fennec doctor`.

*Why:* Every project gets its own Fennec version without anyone thinking about it, and teammates, CI and AI agents all build with exactly the same toolchain. It's also what makes lints pinned to a kit's Fennec version work. Keeping the part that updates tiny means it rarely has to change, and two keys mean one stolen key can't push an update.

**Releases are signed by Chris on hardware keys, and CI never signs.** The release key is Ed25519 and lives on a YubiKey, so it never leaves the device and signs only when touched. CI builds a release and writes its manifest, and Chris signs the manifest offline. A toolchain needs one signature. The launcher's own update needs 2 of 3 keys, from different keys: two YubiKeys and a paper backup kept offline. Each kind is signed in its own namespace, so a toolchain's signature never counts for a launcher. Chris locked this on 2026-11-08. Not built yet: the keys are made when releases go public, as `docs/releases.md` describes. The checks are in `fennec-dist` today, with no keys built in, so nothing verifies.
*Why:* A stolen CI token can't ship a release, since CI holds no key. The launcher checks everything else, so one lost or stolen key can't replace it.

**Building Fennec itself stays fast.** Only the release packaging of the toolchain links LLVM in, through a build feature CI turns on for each target. Everyday builds of the compiler call a pinned, prebuilt `llc` as a separate program instead.
*Why:* Linking LLVM into every build of the compiler would make working on Fennec as slow as the builds Fennec exists to avoid.

## Tooling

Every tool ships in the `fennec` binary and has no configuration unless a setting is unavoidable.

- `fennec fmt`: one style, no options.
- `fennec lint`: the glave-strict rules on by default. `fennec fix` applies the fixes that can't change what a program does (below).
- `fennec check`: types, ownership and lints, without running anything.
- `fennec test`: tests, property tests and the examples in doc comments.
- `fennec doc`: documentation from `##` comments.
- `fennec bench`: runs `bench` blocks, as below.
- `fennec explain`: the long explanation of any error or lint.
- `fennec search`: finds functions by type, locally and on the Den. `fennec search "Path -> str"` lists every function with that shape.
- `fennec diff`: a semantic diff. It reports renames with their updated call sites, moved functions, changed bodies and API changes with the version bump they need, instead of lines added and removed. A GitHub Action posts it on every pull request.
- `fennec replay`: replays a recorded run, as described under Determinism.
- The language server: inlay hints for inferred types and error sets, hover docs, go to definition, rename, and lint fixes as code actions (including "export this"). It reuses the compiler's queries and cache.
- Cost hints: the language server shows each line's copies, allocations and reference-count changes inline, the way it shows inferred types.
- The copies lint, as above.
- The tracer, which names the line and the reason for each copy and allocation: "`units` is copied here, because `backup` on line 9 still holds it". It's a core feature, along with `@hot`, which turns "should be free" into "must be free, or it won't compile". The tracer isn't built yet.

*Why:* Inferred types and error sets need to be visible to be trusted, and a fixed style plus automatic fixes means AI and people write the same code. Cost hints show what code costs while it's written, not after a profiler finds it.

**A test names the capabilities it needs, like `main`, and gets fakes by default.**
```fennec
test "saves the score"(fs: Fs, clock: Clock):
    save_score(fs, clock, 120)?
    assert fs.read_text("score.txt")? == "120"
```
The fakes are an in-memory `Fs`, a fixed `Clock`, an `Entropy` with fixed output, captured `Stdout` and a scripted `Net`. `fennec test --real` uses actual files and the network, on a developer's machine and in their CI.
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

**Every breaking change ships with its own fix, or says why it can't.** A change `fennec fix` can't rewrite, because the rewrite would change meaning, becomes a compile error with a hint, listed in that edition's notes. Not built yet, as editions aren't. Otherwise, no language change that breaks code lands unless `fennec fix` rewrites old code to match.
*Why:* Upgrading Fennec should never be a project.

**`fennec fix` applies only fixes that provably can't change what a program does.** Formatting, removing a redundant type, naming arguments, writing in an inferred return type, removing a needless `else` and the like. It never prompts or asks questions, so agents and CI can run it on a whole kit.
- A fix that would change what a program does or may do, like adding `mut`, `ref`, `?` or an `else` default, is never applied by `fennec fix`. The compiler error shows it as a hint, "fix: declare it `mut people`", and the person or agent makes the edit by hand, so whoever writes it knows what they're changing. The editor may offer it as an explicit one-click action for people. Built: a diagnostic's `fixes` are the edits `fennec fix` makes, and its `hint` is the one it never makes, shown as `= fix: ...`, given in `--json` as `hint` with its edits, and offered by the language server as an action that isn't the preferred one. Adding `mut` is the first: `name = ...` given a new value gets "fix: declare it `mut name`", unless it was read from a place, where `mut` would change a copy and the help says where to write instead.
- A check that every automatic fix leaves every example's output unchanged keeps the first group honest.

*Why:* An automatic fix that changes meaning can quietly bless a bug: adding `mut` so `pop` compiles, when the real bug was calling `pop`. Interactive prompts break agents.

## For AI

**The toolchain ships an MCP server.** Through it, an AI agent can search by type, read docs, run `fennec check` and run snippets with only the capabilities they're given. A snippet runs because its caller chose to run it: there's no sandbox, as Untrusted code says.
*Why:* AI's most common failure is calling functions that don't exist. Ground truth from the compiler and the Den replaces guessing.

**Diagnostics are structured.** `fennec check --json` gives every error and lint with its location and any fix as an edit a tool can apply.

**Docs come in plain Markdown too.** Every documented item has its own versioned Markdown page, with an `llms.txt` index for each kit and for the language.

**`fennec context <kit>` fits a kit in a prompt.** It prints every exported signature with the first line of its doc comment.

**The language reference is executable.** Every example in it is a test, the way `examples/` already is, so the reference can't drift from the language.

## Readable by default

These are default lints, and the Den requires them.

- A `##` doc comment of at least one line on every exported item, enforced by the linter, which every build runs.
- Descriptive names. One-letter and abbreviated names are flagged, except a short allowlist: `i`, `j`, `k`, `n`, `x`, `y`, `z`, `dt`, `id`.
- Limits on nesting depth and function length, as lints rather than language rules.

*Why:* A one-line doc comment on every exported item costs little and tells people and AI what each piece of the API is for. Code that reads well is code AI writes well.

**Runnable examples in doc comments are optional.** `fennec test` runs them, and `fennec doc`, `fennec context` and the Den show them. Whether they become required is left to the eval of what helps AI write Fennec (#76). This replaces the earlier rule that every exported function needs a tested example.

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

**The Den scans, lints and reports, and never runs a kit's code.** It re-runs formatting and lints, scans the source, and shows a diff of the kit's capabilities and API against the previous version. It runs no tests and builds none of the kit's programs: kit authors run their own tests, and CI badges can show the results. Kits have no build scripts, so publishing or installing one runs nothing. Native kits, which in v1 only the Fennec project can publish, are tested under sanitizers in the project's own CI. Chris decided this on 2026-11-11, under Untrusted code: nothing runs code nobody has read. This replaces running each kit's tests in Wasmtime.

**An existing AI model reviews each release's diff,** and its findings are shown. There's no custom fine-tuned model.

**Kits have three tiers, so the audit scales with what a kit can touch.**
- **Pure** (no capabilities and no C): automatic.
- **Capability** (`Fs`, `Net` and so on): automatic plus AI review, with capabilities shown on the page. A newly added capability needs the author's own passkey tap at publish. Consumers' approval of an update that asks for more is unchanged.
- **Process** (`Process`): reviewed like native code, and flagged prominently on the kit's page.
- **Native** (a binding to C in the runtime): human audit, and first-party kits only.

*Why:* Most kits can't touch anything, so they need no human, and the people's time goes to the few kits that could do harm.

**Deferred until needed:** the phone approval web app, delegated publish grants for automation, a public transparency log, a fine-tuned security model, Sigstore signing, and running dependents' tests before a release.

**One known gap until the transparency log exists:** a hacked Den could swap an author's key undetected. The log should be built before the Den gets popular.

**Each version has a size cap,** around 10 MB of source, and publishing is rate-limited.

**Later, breaking dependents is caught before release.** v1 defers this. Before a new version is published, the Den checks the kits that depend on it directly against that version's API, without running them, and reports which ones would break.
*Why:* An author learns "this release breaks 14 kits" before releasing it, not from bug reports.

**The Den can be searched by type,** through `fennec search` and its website.

**Lints are pinned to the kit's Fennec version.** A lint added in Fennec 0.5 never blocks a patch release of a kit written for 0.4.

## Security goal

**The goal is near-zero CVEs, and it's public policy.** True zero isn't possible: the compiler, the runtime and the C that can't be ported can all have bugs. What Fennec can do is remove the usual causes:
- **Memory bugs are impossible.** Fennec has no `unsafe`, and C is called only through the standard library and first-party kits. The standard library's internals are private to the compiler, not a second kind of `unsafe`.
- **C is ported rather than linked,** so less C runs at all.
- **A batteries-included standard library** keeps dependency trees shallow.
- **Capabilities limit the blast radius** of any bug: code can reach only what it was handed.
- **Typed standard library APIs** stop injection.
- **The runtime is small, in-house and fuzzed continuously.**
- **`fennec audit` is backed by a public advisory database.**
- **Programs ship as one static binary in an empty container,** so there's no OS image full of other people's CVEs. Built on Linux: `fennec build --release` links a program with musl's C library and a runtime built for musl, so it needs no C library and no loader from the machine, and `FENNEC_STATIC=0` links one against the system's instead. Each such program holds musl's license notices as text, and `fennec notices <program>` prints them. A program that makes HTTPS requests still needs certificates to check against, so its container gets a CA bundle at `/etc/ssl/certs/ca-certificates.crt`. macOS can't do this, since every program there has to link `libSystem`.

*Why:* Most CVEs in the languages people use today come from memory errors, deep dependency trees, injection and base images. Each item removes one of those. `SECURITY.md` states the same policy publicly.

## Supply chain

**Nothing runs code you didn't read unless you choose to run it.** There's no sandbox to catch it, so every rule below follows from that.

**Installing a kit never runs anything.** There are no install or build scripts.

**Updates are always explicit.** `kit.lock` pins every dependency by hash, and nothing changes without `fennec update`. It shows each update's semantic diff and any capability it newly asks for, and a new capability needs your approval. No version range ever resolves silently.
*Why:* What builds today builds the same tomorrow, and every change to a dependency is one you saw.

**A dependency gets only the capabilities its line in `kit.toml` grants, with `gets`.** `http = { ..., gets = ["Net"] }` reads "http gets Net", and a line without `gets` reads "this kit gets nothing". Chris locked it on 2026-11-12, from Fable 5.2's package review.
- No `gets` means nothing, and `gets = []` is an error whose fix takes it out, so there's one way to write nothing.
- Entries are capability types as code writes them: `"Fs"`, `"Net"`, `"Clock"`.
- No tool, fix or error help ever writes `gets`. What a dependency gets is a person's decision, and an error about it says so and fixes nothing. `fennec fix` once handed a pinned kit the program's `Fs`, by the fix its own error offered.
- It's checked at every hand-over between kits. The handing kit's line for the receiver has to list every capability the value it hands holds. A kit can hand on only what it holds, so the main kit's lines bound the whole tree, and each `kit.toml` speaks only for its own kit's dependencies.
- `kit.lock` records `holds`: `gets` is what a person wrote, and `holds` is what the compiler saw.
- It goes on the dependency's line, not in a `[grants]` table, so a kit and what it gets are one line in a diff, and removing a kit can't leave a grant behind. TOML's long form, `[dependencies.http]`, takes the same key.
- Narrowing comes later with no redesign: an entry may be a table from the type to how it's narrowed, in the narrowing method's own words, as `{ Fs = { only = "saves/" } }` or `{ Net = { client = true, hosts = ["api.example.com"] } }`. A plain `"Fs"` keeps meaning the whole thing. Since narrowing happens at run time and gives back the same type, the check asks that the value is narrowed where it's handed over, written out, as `saves.load(fs.only("saves/"))`.
- Not built yet. It's built before the Den opens, after the security holes the package review found, as a record per kit in the compiler of what each holds.

*Why:* Capabilities limit what a dependency can do only if something says what each one may hold. Written by a person, beside the line that brings the kit in, it's reviewed where the kit is, and no tool mending a red build can widen it.

**An update that asks for more capabilities stops until you approve it.** `kit.lock` records the capabilities each dependency receives, as `holds`. A logging kit that starts asking for `Net` doesn't install until you say so. The same goes for an update that adds `unsafe` code.
*Why:* A trusted package turning malicious in an update is the attack that hit event-stream and ua-parser-js.

**Capabilities can be narrowed.** `main` hands each kit only the part it needs:
- `fs.only("saves/")` reaches only that folder.
- `net.client()` can only connect out, and `net.server(port=8080)` can only listen on that port.
- `net.hosts(["api.example.com"])` can reach only those hosts.
- `process.only("git")` can run only that program.

Narrowing chains, so `net.client().hosts(["api.example.com"])` can only connect out, and only to that host. A kit asking for `Process` is flagged prominently and reviewed like native code.
*Why:* Connecting out and serving are different powers, and a kit that needs `git` shouldn't get a shell.

**Every kit has an owner's namespace,** `owner/kit`. The Den rejects names a small edit away from popular ones. There's one public registry, and private code comes from git or a path rather than a private registry (below).
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

**Private kits and work in progress come from git or a path, fenced off from the Den.** `kit.toml` can name a git URL pinned to an exact commit hash, never a branch or tag, or a local path. No private registry is needed.
```toml
[dependencies]
physics = { git = "https://git.example.com/studio/physics", commit = "3f9c2a1e7b4d" }
tools = { path = "../tools" }
```
- A kit with git or path dependencies can't be published to the Den, so they never spread into other people's trees.
- Capabilities are enforced as for any other code.
- Built: `kit.toml` reads both, with `folder` for a kit inside a repository, like the first-party kits in this repository's `kits/`. `fennec update` fetches a kit from git into `~/.fennec/kits/<hash>` without checking anything out, and pins it in `kit.lock` by the hash of its `kit.toml` and `src`. A kit from a path isn't pinned, and a kit from git can't name one. Importing from them is built too: `import physics` reaches its `src/lib.fen`, each kit keeps its own names, and another kit reaches only what's marked `export`.

*Why:* Go's convenience for private code, while public code stays on the checked Den. An app may already use C in its own code, and a game engine split into local kits (`../engine`) needs C for Metal and Vulkan; the Den fence still stops that C from spreading. Capabilities mean even unreviewed code can't touch files or the network unless it's handed `Fs` or `Net`.

## Engines

**There's one backend: LLVM, with per-function `-O3` and a function-level cache.** `fennec run` makes real binaries in the store. Whole-program work happens only in `--release`. There's no tiering.
*Why:* What people run is what ships, so a first benchmark shows real speed. The cache is what makes it fast to edit, and one backend is less to maintain and keep in agreement.

**LLVM is the default backend.** `fennec run` and `fennec test` use the cached LLVM build, and building natively and refusing to build, with the reason, are the only two outcomes. Built. Since every program in the repository built natively, `fennec build` always makes a native program through LLVM, with no way back to the JIT's compiler. This replaces the rule that Cranelift is removed when LLVM wins across the benchmark suite, edits stay well under a second, and CI's rebuild comparison stays clean.

**Fennec has no REPL.** Running a small file is already quick, and one can come back as a pure addition if people ask.
*Why:* This is a compiled language written mostly by AI, and it needs to be readable, fast to build and fast to run.

**The interpreter is removed, and the JIT with it.** Before that, each job it does moves somewhere else:
- **Code LLVM can't build** goes to zero, on the coverage lane's tracked list.
- **The test fake network** moves into the runtime.
- **Constants at compile time** are worked out by a small evaluator inside the compiler.
- **The fuzzing reference** becomes pinned outputs and the native twins.
- **The website playground** is removed for now, and returns once the self-hosted compiler can compile in the browser. Until then the site shows examples with the output they really print. Chris decided this on 2026-11-08. Built: the playground page, its links and its build are gone. This replaces compiling the playground on a server with a cache that returns WebAssembly.

The commit before removal gets a tag, so the old engines can still be found. Built so far: the JIT is removed, after the tag `before-jit-interpreter-removal`; native programs carry no interpreter; the test fakes, the network's included, are the runtime's; constants are worked out by the compiler's own evaluator; fuzzing's reference is pinned outputs and native builds; and `fennec` runs nothing on the interpreter, so `run --interp` and the fallbacks are gone. Not built yet: deleting the interpreter's code. This replaces the rule that the interpreter keeps constants, fuzzing and the playground for now.
*Why:* One engine is what ships and what's tested, so the interpreter's double work and its differences from native code go away.

**Until then, every example that builds natively must give the same output as the interpreter.** An example that's refused goes on a tracked list, which must reach zero. Tests now hold native code to the interpreter's pinned answers, since there's no live interpreter run to compare with.

**While it remained, the JIT aimed for 80% of the speed of the benchmark suite's twins to start, then 85%, and no further.** The JIT is removed now, so this no longer applies. Closing the rest is the release build's job. Optimizations that would slow the JIT's compiling to win the last few percent aren't made.
*Why:* Development builds are for changing code and seeing it run at once. The 110% target applies to release builds.

**On x86-64, native code is built for `x86-64-v3` (AVX2, FMA and BMI) by default, and `--cpu baseline` builds for the first x86-64 processors.** On 64-bit Arm the baseline already has what's wanted, so nothing changes there.
*Why:* It's the level the JIT already uses and the benchmark twins are built for, so comparisons are fair, and nearly every x86 machine since 2013 has it. Those older than that still get a working build by asking for one.

**A release build optimizes what it uses of the runtime together with the program, so runtime calls can inline.** The runtime's own LLVM bitcode ships beside `fennec`, and a release links only the runtime's C parts (mimalloc and the compiler's helpers) afterwards. Development builds are unchanged.
*Why:* It takes about 1.3% off a kernel's instructions on Arm, up to 4%, for release builds that take about 20 seconds instead of one, which release builds can afford.

Every build and the language server share one build engine and store, described under Builds.

## Builds

**There's one kind of development build, optimized and cached, and no separate debug build.** A failure says what went wrong and where: the function, file and line, and the calls that led there, from Fennec's own source positions, which the runtime gets with each failure.
*Why:* What runs while editing is what ships, and failure messages and deterministic replay do most of a debugger's job.

**Debug info is planned,** so a debugger can step through Fennec code, a stack trace is readable, and a crash maps back to its source line. Not built yet: today no DWARF or stepping information is written. This replaces the rule that builds carry no debug info.
*Why:* Games and native code need a debugger, even when tests and replay do most of the work.

**Hot reload is deferred, and its design is in progress.** It's for changing code while a game or a server runs. The plan so far is all or nothing:
- Changed functions are swapped in only at a safe point: the top of a loop, or between requests. The state the main loop owns stays as it was.
- Before anything changes, the live data is checked against the new code: fields matched by name, new fields taking their defaults, and every `where` rule run again. When a struct's fields change, the state is saved with `std.pack` and loaded back into the new types.
- If every check passes, the reload applies completely. If any fails, it's refused with a message saying why, and the old code keeps running. A reload is never half-applied.
- Chris decided this on 2026-11-08, and deferred it on 2026-11-09 while its design is worked out. Not built yet. This replaces the rule that there's no hot reload, and the plan that swapped code between frames without checking first.
*Why:* A game's state is plain data its main loop owns, with nothing pointing into code, so swapping code is unusually clean, and the function-level cache already makes each change small. Checking first means a reload that can't work costs nothing, and a half-applied one would leave data the new code can't trust.

**The unit of work is the function, keyed by its content.** A function's key covers its typed IR, the signatures of what it calls, the bodies of anything inlined into it, and the compiler version, target and flags. A caller depends on a callee's body only if it inlined that callee. Summaries let a change that leaves a signature alone skip the callers.

**Results are pure.** Cache keys never include paths, timestamps or environment variables, and every key includes the compiler version. Builds are deterministic, so each function of a kit compiles once per machine, ever.

**There's one content store per machine,** `~/.fennec/cache`. Results are named by their hash and written by atomic rename, so readers never see half an entry and the normal path takes no locks; a lock is taken only during eviction. It has a size cap.
- There are no `target` folders, and no build folder inside a project. `fennec run` makes its binary in the store.
- Built: the store, its format and what's in it are in `docs/store.md`, and a test holds that running, testing, checking, linting, formatting and building leave a kit's folder as it was. Keeping built programs under a build's root hash is next.

**Work runs in parallel on every core,** one function at a time per core.

**A daemon is optional and purely a speedup.** It starts itself, and keeps the compiler, checker, formatter and language server's queries in memory. The plain command gives identical results without it.

**A cache can be shared** between machines and CI, and only trusted builds can write to it.

**CI does a clean rebuild and compares it with the store.** Any mismatch blocks a release.
*Why:* A wrong cache is the worst bug a build system can have: it silently tests or ships code that doesn't match the source.

**`--release` adds whole-program optimization,** cached per function where possible.

*Why:* These avoid what makes builds slow elsewhere: large compilation units, build scripts and macros that run code, a build folder in every project that grows to gigabytes, and separate engines for the compiler and the editor.

**Hashing works in two layers.**
1. A BLAKE3 hash of each file's bytes. An unchanged file isn't even parsed again. A timestamp may skip re-reading a file that hasn't changed, but the hash decides, so switching branches or cloning in CI doesn't rebuild anything.
2. A hash of each function's typed IR, which leaves out positions and comments. Reformatting, editing comments and moving code around recompile nothing. Positions are kept separately for error messages.

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
- LLVM maps `Simd` onto its vector types.

*Why:* One generic type covers every width and element type without dozens of names, and it reads like math. Zig, Mojo, C# and Swift settled on the same shape. Hand-written intrinsics tie code to one CPU and are unreadable.

**`f16` and `bf16` are number types.** Arithmetic uses the hardware where it exists, and otherwise widens to `f32` and narrows back, with the same results. 8-bit quantization already works with `i8` and `u8`.
*Why:* ML inference is mostly limited by memory, and 16-bit weights halve it.

**`.lanes[N]()` walks a buffer `N` elements at a time.** It's a method on `Buffer[T]`, `list[T]` and `GpuBuffer[T]`, giving `Simd[T, N]` values and a final partial chunk with a mask. `for ref` writes back. Bounds are checked once per chunk, and the tail mask means nothing reads past the end.
```fennec
def scale(ref xs: list[f32], factor: f32):
    k = Simd[f32, 8].splat(factor)
    for ref v in xs.lanes[8]():
        v *= k
```
Plain loops and `parallel(...)` loops are still vectorized automatically where LLVM can. `Simd` is for when you want a guarantee.

**GPU access is a capability, `Gpu`,** passed to `main` like `Fs`. A kit gets the GPU only if it's handed one.

**`GpuBuffer[T]` is GPU memory.** It holds numbers, `f16`, `bf16`, `Simd` values, or structs made only of those. `gpu.upload(xs)` and `gpu.download(buffer)` move data. Fennec values never point into GPU memory, so the CPU and the GPU can't race over data.

**Kernels are ordinary functions, run with `gpu.run`.** Any function that follows the GPU rules can run on the GPU, and the compiler checks the rules at the `gpu.run` call. It calls the function once for each index from 0 up to `count`, in parallel.
```fennec
from std import gpu

def brighten(i: int, ref pixels: GpuBuffer[f32], amount: f32):
    pixels[i] = min(pixels[i] + amount, 1.0)

def main(stdout: Stdout, gpu: Gpu):
    mut image = gpu.upload(load_pixels())
    gpu.run(brighten, count=len(image), ref image, 0.2)
    pixels = gpu.download(image)            # waits for the GPU here
```
- A kernel can't allocate, recurse, use capabilities, or use strings, lists or dicts. It works on numbers, `Simd` values, structs of numbers and `GpuBuffer`s.
- Invocation `i` writes only element `i` of each `ref` buffer, and reads anything, the same rule as `for ref x in parallel(xs)`. Kernels can't race, and their results are deterministic. A write anywhere else is a compile error naming the line.
- Buffer indexing is bounds-checked, and range analysis removes the check where the index is provably the invocation's own.
- `gpu.run` queues work and returns, and `gpu.download` waits for what was queued before it.

*Why:* A kernel that's just a function is tested on the CPU with `fennec test`, debugged like any code, and run by the interpreter too. A separate shader language means separate tools.

**Kernels compile through two small in-house backends:** SPIR-V for Vulkan (Windows, Linux, Android) and Metal Shading Language source for Metal (macOS, iOS). No third-party translator is involved. Kernels are compiled at build time and kept in the build cache by content, like every other build output, and no compiler runs inside the program. Chris decided this on 2026-11-11, in place of compiling each kernel when it first runs. WGSL for WebGPU can come later. `std.gpu` reaches Vulkan and Metal through the runtime's binding to C, the one way any C is reached.

**GPU floats are the same on the same device, not across vendors.** `sqrt`, `exp` and division can differ in the last bit between GPU makers. `gpu.run(..., exact=True)` turns off contraction and uses Fennec's own math functions for results identical across vendors, at some cost in speed. Lockstep game simulation stays on the CPU.

**Later slices, in order:** workgroup memory with barriers the compiler checks, for tiled kernels; subgroup operations; a `Tile[T, M, N]` type for matrix hardware, through Vulkan's cooperative matrices and Metal's simdgroup matrices; and training, with automatic differentiation as a library in `fennec/ml`. Graphics shaders can reuse the kernel compiler later, through `fennec/gpu`. A tensor type with operator fusion stays out of the language.

## Native code

**C is called only through the standard library and first-party kits.** Their binding to C sits in reviewed runtime code, and Fennec programs see only a safe API, like `with Window.open(...)? as window:`. A program or a third-party kit can't call C itself. Not built yet. This replaces `extern "C":` blocks and `unsafe` in Fennec code, and C in your own app or in git and path dependencies.
*Why:* "No `unsafe` in Fennec" stays true for every program, and games still get libraries like SDL and Vulkan.

**If it can be Fennec, it is Fennec.** Libraries are ported to Fennec wherever possible. C is called only for what can't be ported: the operating system's own APIs, GPU drivers, and closed platform or console SDKs.
- **A port is tested against the original:** the C library is the oracle, giving the same outputs for the same inputs, and fuzz tests compare them continuously.
- **A port is benchmarked against the original:** CI fails if it's more than 3% slower.
- **Only permissively licensed code is ported:** MIT, BSD, Apache, zlib or public domain. Anything under the GPL or LGPL is written from scratch without reading the original.

*Why:* Ported code is memory-safe, bound by capabilities, and runs in wasm. A C binding is none of those.

**A C callback reaches Fennec through an `Id`.** C never holds a closure or a place.
*Why:* C can call back at any time, so anything it keeps has to be safe to keep, and a stale `Id` finds nothing.

**No C is called inside `parallel` bodies.**
*Why:* Most C libraries aren't safe to call from several threads at once.

**Every first-party native kit has a plan to become pure Fennec,** with differential tests against the original C, and its runtime binding runs its tests under AddressSanitizer, UndefinedBehaviorSanitizer and LeakSanitizer.

**There's no C compiled to wasm in v1.** It can be reconsidered later if the ecosystem needs it.

This replaces the earlier plan for C in Fennec code: `extern "C":` blocks, `unsafe("reason")`, pointer types, `def del` releasing owned pointers, `fennec audit --unsafe`, a `[native]` table in `kit.toml`, and apps or path dependencies calling C themselves.

## Untrusted code

**Fennec has no sandbox for untrusted code.** Don't run code you don't trust, and don't add dependencies you don't understand. Capabilities limit what any dependency can do: only what its caller passes it, no `unsafe`, and no C outside std and first-party kits. The Den scans kits. Neither is a licence to run code nobody has read. Chris decided this on 2026-11-11. This replaces the plans for sandboxed code: Den tests and agents' snippets run by the interpreter with limits, then compiled to WebAssembly and run in Wasmtime, in a process of their own on servers.

**No program runs scripts inside itself.** A program can't load and run code at run time.
*Why:* A script loaded at run time is code the compiler never saw, which is the open set Fennec has already ruled out.

## Writing Fennec in Fennec

The current compiler stays as stage 0 until a Fennec compiler built by Fennec produces identical output when it builds itself again.

The order:

1. The tools: the formatter, the linter and the Den CLI. They can be written while the language is still changing.
2. The front end: lexer, parser and type checker.
3. The backend, written to emit LLVM's text format (`.ll`) for `llc`. This needs no calls into C.
4. A backend of our own: only if LLVM stops being enough.

Steps 2 to 4 wait until the core semantics above are frozen and the Fennec compiler can run natively, since the interpreter is too slow to run a compiler.
*Why:* Every language change after the compiler is self-hosted has to be made in two compilers.

## Recording decisions

**Architecture decision records (ADRs) come at the language freeze.** They're generated from this document and its pull request history, and labeled as reconstructed. After the freeze, a language change means a new ADR.

## License

**The compiler and tools are `MIT OR Apache-2.0`, and the runtime library and standard library add a runtime library exception,** modeled on Swift's. A program compiled with Fennec owes no attribution for the runtime and standard library code built into it. The exception is in `RUNTIME-LIBRARY-EXCEPTION`.
*Why:* Every compiled program carries pieces of the runtime and standard library. Without the exception, every game or tool shipped would have to include Fennec's license notice for code its author never chose to use. Swift and GCC have the same kind of exception for the same reason. The compiler and tools themselves keep the usual dual license, which most open-source compiler tooling uses, so code can move between projects in either direction.

**Contributions are dual licensed the same way,** unless the contributor explicitly says otherwise. The README states it.

## Still open

Nothing right now.
