# Fennec on one page

This page is the source of truth for the language. If `decisions.md` disagrees with it, this page wins. Everything here is built unless it says **Not built yet**.

Fennec reads at a glance, builds almost instantly, and is built to run as fast as hand-written systems code in release builds. Everything is a plain value by default, and the compiler does the hard work.

## Four rules

- A name holds its own value.
- `mut` lets a name change.
- To change something in a list, write to it there.
- `ref` lets a loop, a function or a local name change yours.

```fennec
struct Unit:
    name: str
    health: int

def heal_all(ref units: list[Unit]):
    for ref unit in units:
        unit.health += 10

def main(stdout: Stdout):
    mut units = [Unit(name="Ana", health=50), Unit(name="Bo", health=70)]
    units[0].health -= 10
    heal_all(ref units)

    # your own value
    target = units[1]
    stdout.print(target.health + 1)
```

What's passed with `ref` has to hold all the parameter does, not only something that fits it, since the function can put in anything the parameter holds: a `list[int]` can't go where `ref items: list[int?]` does, as a `None` could be put in it, and a `Crate` can't go where `ref shape: Crate | Heap` does. A name declared as the whole type can, however it's narrowed where it's passed. It's a compile error that says what could be put in. Built.

A generic type's arguments are the same on both sides wherever one goes, not only something that fits: a `shared list[int]` can't go where a `shared list[int?]` does, nor a `Sender[int]` where a `Sender[int?]` does, nor an `Inbox[int]` where an `Inbox[int | str]` does. A `shared` value or a channel's end is one value, read and written through every name for it, so what's put in through one name is read through the other. That holds for a struct's own type arguments too, as a `Pool[int]` where a `Pool[int?]` goes, through a field, what's given back and another kit's functions. A `list`, `dict`, `set`, tuple or optional is copied where it goes, so a `list[int]` still goes where a `list[int?]` does. It's a compile error that says to make both the same. Built.

A local `ref target = units[target_id]` links a name to a place for several changes in a row. It lasts until its last use, inside its block. While it's alive, nothing else may change that container, and a second `ref` into it is refused, but reading it is fine and sees the change. Its index is worked out once, at the `ref`. It's checked at compile time and costs nothing at run time. There's no scoped `with units[target_id] as ref target:`. The one function that may return a `ref` is a type's `ref` form of `def []`, which gives a place, so `units[i].health -= 1` works on your own types too.

## What you can declare

```fennec fragment
# a value
struct Unit:
    name: str
    health = 100 where health >= 0

# a value with cases
enum Action:
    Wait
    Attack(target_id: Id[Unit])

# a failure with cases
error SellError:
    NotFound(item: Item)
    OutOfStock(item: Item)

# what a type can do
interface Shape:
    def area(self) -> float
```

There are no classes and no handle keyword. Nearly everything you write is a `struct`, and `shared` is a type, not something you declare.

## Mistakes the compiler stops

```fennec fragment
target = units[target_id]
target.health -= 10
# error: `target` isn't mut
#        to change the list: units[target_id].health -= 10

mut target = units[target_id]
target.health -= 10
# error: this changes your own copy, and nothing reads it

for unit in units:
    units[0].health -= 1
# error: you can't change a list while looping over it
#        use `for ref`, or loop over indexes
```

`dict[str, int?]` is an error too: leave the key out, since not being in a dict already means missing. The same goes for optional keys and set items.

A generator is used once. Passing it to a function that loops over it is its one use, and any use after that, on every way the code can go, is a compile error. A second use the compiler can't see, like one in a later round of a loop, stops the program instead.

```fennec fragment
found = (n for n in numbers if n % 2 == 0)
stdout.print(sum(found))
for even in found:
# error: this generator has already been used
#        a generator can be used once: `list(...)` keeps its items to use again
```

A generator that can fail, got with `?` as in `lines = file.lines()?`, is looped over in the function that got it, which passes its failure up. It can be named there and looped over later, but it can't be passed to a function, given back, kept in a field, used inside a nested function, an arrow function, a generator kept for later or a `parallel` body, or go into `enumerate` or `zip` anywhere but right in a `for` loop's own. Each is a compile error, since a written `Iterator[T]` can't fail, and `list(...)` keeps its items to use anywhere. The same goes for a list, a tuple, a union or a generic struct holding one, and for an arrow function's own parameter given one, as `sorted(held, key=gen => sum(gen))` would be; `parallel` can't go over such items either. A generator kept for later over items that are, or hold, generators that can fail fails as they do, and passes that failure up where it's looped over. `except` takes no generator that can fail out of a call: on a generator function's call it would never run, since making a generator runs none of its body, and on an arrow function's it would leave the failure its `?` counted with nowhere to go. Both are compile errors. A `def`'s call can have one, generic or not, when it's given nothing that could have made it: what it gives back was made, and counted, where it was called from. Given a function whose result is, or holds, a generator that can fail, a type parameter or another such function, or an arrow function taking a function, which may have made one, it's a compile error too; a rule giving back a plain value, as `(int) -> bool`, is fine. So is an arrow function's own parameter given one before its type is known, and a name read from around that turns out to hold one: each is looked at again once the function's types are worked out. A name keeps one kind of failure, so a name given generators that fail in different ways, or one that can and one that can't, is a compile error too. An interface's method that gives a generator that can fail is a generator function in each type, failing just as the interface says. `? from` wraps a generator's failure only where a `for` loops over it right there, as `for line in file.lines()? from LoadError(...)`.

A generator function can't take a `ref` parameter, `ref self` included: it's a compile error. A generator runs a little at a time, after its call has given it back, so its `ref` would be held across every `yield` while the code that called it changes the same place by its own name. Take a copy, or give back a `list` of the items.

One silent mistake remains: changing a `mut` copy and then reading it, believing the list changed. A rule gets added for that only if testing shows real bugs.

## Loops

```fennec fragment
for unit in units: ...
for ref unit in units: ...
for unit_id, unit in units.items(): ...
for unit_id, ref unit in units.items(): ...

for at, name in enumerate(names): ...
for at, ref score in enumerate(scores): ...
```

- `enumerate` takes anything you can loop over and gives `(index, value)`. `for i, ref x in enumerate(xs)` needs `xs` to have positions, `len` and `[]`, as `for ref` does.
- `for unit_id, ref unit in units.items()` changes each item of a `Table`, or each value of a dict, where it is, with its `Id` or key, which isn't `ref`. The body can't change `units` itself. A `for ref` straight over a dict or a set is a compile error, since its keys and items are what find them.
- A `for ref` body may read what it loops over, as in `unit.target = nearest(units, unit.at)`, and sees the item as it is now, changes made through the loop's name included. Not built yet for your own types with positions: it holds for lists, arrays and `Table`s. A `for ref` over `parallel(xs)` whose body reads `xs` runs its rounds one after another, as one with a `return` does, so each sees the items as they are now.
- `ref` over a generator is a compile error, since its items are made one at a time and aren't anywhere to change.
- `case .Button(ref clicks):` changes a field inside the enum a `match` statement is over, as `for ref` changes an item: `match ui[widget_id].kind:` with `clicks += 1` in the case. The case is put back however its body ends, a failure included.
- `items()` is for keyed things: a dict, a `Table`, and your own types that give `len`, `[]` and `key(i)`. Not built yet for your own types.
- Looping over a dict gives its keys, as in Python, and `keys()`, `values()` and `items()` stay.
- A generator passed to a call like `sum` can read anything, but it can't change the names around it, which is a compile error. Use a loop for that.
- `f(ref xs[i], ref xs[j])` is always refused, since `i` and `j` may be the same, and so is `f(ref xs[0], ref xs[2])`. `xs.swap(i, j)` swaps two items, counting from the end when a place is negative, and there's no `pair`.

## Copies

```fennec fragment
uppercased = [copy(person, name=person.name.upper()) for person in people]
```

`copy(x, field=...)` makes a changed copy and re-runs `where` rules. `fennec fix` rewrites the old name, `replace`, to `copy`. A plain `=` covers everything else, and the compiler skips the real copy when nobody can tell. A plain parameter can't change, so passing something huge costs nothing.

## Where rules

```fennec fragment
struct Stats:
    level: int where level >= 1

def lowered(ref level: int, by: int):
    # A plain int here: Stats's rule runs when it's given back.
    level -= by

def drained(ref stats: Stats, by: int):
    # A Stats: its rule runs on this write.
    stats.level -= by

# Stops here, as the call ends, if `level` is left below 1.
lowered(ref stats.level, by=5)
```

A `where` rule belongs to its struct, and runs on every write to its field through the struct: setting the field, `+=`, a method that changes it, `copy`, and making a value. A field lent alone by `ref`, to a call or to a `for ref` loop's turn, is a plain value to whoever has it, so the rule runs when it comes back, at the end of the call or of the turn. A value that breaks the rule in between and is put right before then is fine, and one left broken stops the program there. A whole struct lent by `ref` is still a struct, and its rules run on each write, as anywhere.

## Sized numbers

```fennec fragment
# the everyday types stay as they are: int is i64, float is f64
count = 0
ratio = 0.5

# a literal takes the type it's going into, checked to fit
speed: f32 = 1.5
pixel: u8 = 255
# error: 300 doesn't fit in u8
pixel: u8 = 300

# conversions are always written
# you know it fits: if it doesn't, that's a bug, so it stops
byte = u8(small)
# it came from outside: it may not fit, so it fails cleanly
level = u8.parse(text)?
volume = u8.from(raw)?

# overflow stops, as it does for int, unless you say otherwise
hash = hash.wrapping_mul(16777619)
```

- There are ten: `i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`, `f32` and `f64`. `int` is `i64` and `float` is `f64`. There's no 128-bit type and no platform-sized one, since `int` already indexes.
- A literal with nothing to go by is an `int` or a `float`, as today.
- Mixing types, like `i32 + i64`, is a compile error that says to convert one side. Widening is written too, as `i64(small)`, and can't fail. `/` gives a `float` between integers of any one size, as between ints, and `//` keeps the size. Arithmetic is checked against each size, and `~` stays within an unsigned size's width. A `u64` is unsigned on all 64 bits, and an `f32` is rounded to 32 bits after each operation and prints as the shortest text that reads back as the same `f32`. `sum` of sized numbers gives one of their size, starting at its zero, and sets, dicts and tuples of them, literals included, hold each as its type. `u8.from(x)?` and `u8.parse(text)?` work for every integer size, and `u64.parse` reads past the largest `int`. Every integer size has `wrapping_add`, `wrapping_sub`, `wrapping_mul` and their `saturating_` twins. `examples/sized_numbers.fen` shows them. Not built yet: a `u64` literal above the largest `int`, and packed layout.
- Every sized type checks overflow, and a check is removed only where the compiler proves it can't fire. `wrapping_` and `saturating_` methods are for when you mean it.
- `<<` is bit work, not arithmetic: it drops the bits shifted past the top on every integer type, with no check, so `200 << 1` on a `u8` is `144`. A count at or past the type's width stops the program, as any count outside 0 to 63 does on `int`.
- Floats give the same bits on every platform. Arithmetic and `sqrt` are exact in IEEE 754, and `sin`, `cos`, `tan`, `acos`, `exp`, `log` and `**` are computed by one library, `libm`, on arm64, x86-64, Windows and WebAssembly alike, so replays and lockstep multiplayer agree. Each platform's own C library rounds some of these differently in the last bit. `round(x, places)` scales by a power of ten that's the same bits everywhere too: exact up to ten to the 22, and past that the float nearest it.
- `std.random`'s `Random(seed=...)` draws the same numbers in the same order on every platform. Its `n`th draw is the `n`th number of the seed's SplitMix64 sequence. `randint(low, high)` and `choice` take a range by Lemire's method: a draw times the range's size, whose high 64 bits are the number, drawn again when the low 64 bits are below 2 to the 64 mod the size, which would bias it. `random()` is a draw's top 53 bits over 2 to the 53.
- Floats have one total order: NaN equals itself and sorts last, and `<` agrees with `sorted`. `==`, hashing, sorting and `<` all follow it. Not built yet: using the compiler's "never NaN" analysis to drop that cost where a value can't be NaN.
- `std.math`'s `Vec2`, `Vec3`, `Vec4`, `Quat`, `Mat3` and `Mat4` are `f32`, with `DVec3` and the other `D` types for doubles. They're concrete types, not generic ones, and a generic version with an `f32` default could come later without breaking anything. A number written out goes into them as an `f32`, as in `Vec3(x=1.5) * 2.0`, `max(speed, 0.5)` and `case 0.5:`. Each matches glam's own type bit for bit: `Vec3` glam's `Vec3` worked a part at a time, its `scalar-math` way, since glam's SIMD sums add in a different order on each kind of machine, and `DVec3` glam's `DVec3`. Not built yet: a sum of numbers written out, like `16.0 / 9.0`, going in as an `f32`.

## Fixed-size arrays

```fennec fragment
struct Mat4:
    m: array[float, 16]                # 16 floats, held inline

struct Bag:
    slots = array[Item?, 20]()         # every slot starts as None

identity: array[float, 4] = [1.0, 0.0, 0.0, 1.0]
```

`array[T, N]` holds exactly `N` items where the array is, with no block of its own. Write one as a list of exactly `N` items where an array goes, or as `array[T, N]()`, which fills it with the item's zero: `0`, `0.0`, `False`, `""`, `None`, or a struct made from its defaults. It indexes, loops with `for` and `for ref`, and has `len`, `in` and `==`. A plain `=` copies it. Its size never changes, and it doesn't go where a list goes: `list(arr)` makes one. A wrong count, and a position known to be outside it, are compile errors.

## Buffers

`Buffer[T]` is the growable run of slots that lists are built on, with `push`, `pop`, `insert`, `remove`, `extend`, `truncate` and indexing. Its room, `capacity()`, is always at least what it holds, and `reserve(n)` makes room for at least `n` more than it holds, so adding `n` items after it never grows it. How much room it has beyond that, and how it grows, is the runtime's to tune and can change between versions, so a program shouldn't depend on the exact number.

## Pointing at things: tables and Ids

```fennec fragment
struct Unit:
    health: int
    target_id: Id[Unit]?

mut units = Table[Unit]()
unit_id = units.add(Unit(health=50))

target = units.get(enemy.target_id) else return    # may be gone
units[unit_id].health -= 1                         # sure: stops loudly if wrong
```

`units.update(unit_id, value)` puts a new value at a live `Id` and gives `True`, or gives `False` if it's gone.

Tables are for large, flat data. An `Id` is an index and the tag its `add` took, so it finds the right thing or nothing.

An `Id` made before a copy of a `Table` works in both copies, and one made after works only in the copy it was made in. Each `add` gives its `Id` a tag no other `Id` has, which the item's slot keeps, so an `Id` finds its item or nothing however the `Table` was copied: changed after a copy, sent to a task, read out of a lock, or put back by a failed `lock ... as ref`. A program never sees a tag: an `Id` doesn't print it, and two can't be ordered. Built.

An `Id` is local to its process's `Table`. To name something across servers, send a key of your own, like a player's name, and let each server find its own `Id` from it. An `Id` from another `Table` finds nothing.

## Sharing, resources and tasks

Four questions, each with its own words:

| Question | Words |
|---|---|
| Can it change? | `mut`, `ref` |
| Who's touching it? | `shared`, `lock` |
| When does it go back? | `with` |
| Who talks to whom? | `launch`, channels |

Every borrow lives in a block you can see, so there are no lifetimes to write. A typical game uses none of this beyond `with`: its main loop owns the world and passes `ref world` down.

### `shared` and `lock`

```fennec
struct Scores:
    best = dict[str, int]()

def record(scores: shared Scores, name: str, points: int):
    # one task at a time can change it
    lock scores as ref current:
        current.best[name] = points

def best_of(scores: shared Scores, name: str) -> int:
    # many tasks can read it at once
    lock scores as current:
        return current.best.get(name) else 0

def main(stdout: Stdout):
    shared scores = Scores()
    record(scores, "Ana", points=10)
```

- `shared T` is a type, so it goes in fields and collections too: `dict[str, shared Room]`. It works on any type, and there's no `shared Name:` declaration.
- Not built yet: a `shared` value of a type with a type parameter in it, as `shared list[T]` in a generic struct or function. It's a compile error that says so: write the type out, as `shared list[int]`, outside generic code, and pass the shared value to what needs it. A generic struct may still hold a `shared` value whose type is written out.
- A `shared` value is made in a function, usually `main`, and passed to whoever needs it, like a capability. There's no top-level shared state, so a function's signature says what shared state it touches.
- Making one from a plain value is written out, as `shared scores = Scores()`. A plain value never becomes `shared` at a call.
- It's reached only inside `lock x as s:`, to read, or `lock x as ref s:`, to change. Any way out of the block releases it, `?`, `fail` and `return` included. Leaving with `return`, `break` or `continue` keeps the changes made so far, and a crash undoes them, below. A `fail` or `?` leaving `lock ... as ref` undoes them too.
- **No waiting inside a lock:** no channel sends or receives, pool takes, network calls, `clock.sleep`, `parallel`, a `lend`, or the end of a `with launch`. Printing and local files are fine. Wait first, then lock. It's a compile error, traced across the whole program through calls, methods and interfaces, however many calls down the wait is. A call through a function value is followed too: a function parameter called inside a lock, or handed on to one that is, is checked against what each caller passes for it, an arrow function by its body and a `def` by what it calls, and a function value whose source can't be known, like one taken from a list, is refused where it's called.
- **Deadlocks can't compile.** Lock order is checked across the whole program, by where each `shared` value is made. Two values the compiler can't tell apart, like two accounts from one dict, are locked together in one statement: `lock accounts[from_id], accounts[to_id] as ref source, ref target:`. Naming the same value twice in one `lock` is a bug, as an index that isn't there is: it's a compile error where the checker can see both are the same value, through the same name or the same path, and otherwise it stops the program with exit code 1, saying the `lock` takes the same `shared` value twice.
- A `lock` inside another `lock` is refused for now, however many calls down it's taken: take them together in one.
- A `shared Name:` declaration, a `with` on a `shared` value and a `def del` are refused, and `fennec fix` rewrites what's mechanical: `struct Name:`, `shared Name` where the type is written, `shared x = Name()` and `lock`.
- Interfaces and function values are traced across the whole program, which is always compiled together, so nothing needs a `locks` annotation. An error names the implementation that takes the lock.
- A `shared` value holds plain values and other `shared` values, never something borrowed from a `with`. It never holds one of its own type, and never forms a cycle, so a tree uses a `Table` and `Id`s. It can't store a function that captures a `shared` value.
- It's shown as what it holds, read under its lock: printed, in an f-string, by `str`, and in a failed `assert`, as `Account(total=1)`.
- `==` compares what two `shared` values hold, as every other value compares: a snapshot taken under both locks, which may be stale once they're released. Both are taken in a fixed order, by cell, so `a == b` on one thread and `b == a` on another can't wait for each other, and `a == a` takes one.
- Inside a lock body nothing shows or compares a `shared` value, or a value holding one, the locked one included: it would wait for a lock its own thread holds. Use the name the lock gives, or do it after the block.
- Its memory is freed when the last holder lets go, with no cleanup code.

### `with` and `lend`

```fennec fragment
with fs.open("save.dat")? as file, net.connect(host)? as conn:
    data = file.read_text()?
    conn.write_line(data)?
# both are closed here, however the block ends
```

The end of a `with` block is where what it lent finishes: a file closes and a transaction commits. A failure while finishing is the `with` statement's failure, covered by the `?` on its line, so `with fs.create(path)? as file:` covers both opening and finishing. `examples/sources.fen` writes sources of its own, and `examples/files.fen` uses files: a file written goes in beside its path, synced, and takes the path's place when the block ends, and a block that fails leaves the old file. `fs.create(path, durable=False)` skips the sync. In a task, a file's disk waits pause only that task. SQLite's transactions are sources too, in `std.sqlite`: `with db.transaction()? as tx:` commits at the block's end and rolls back if it fails.

- `with` lends you something for the length of a block, then takes it back. It's the only way to open files, sockets, servers and transactions.
- What it lends can't escape the block: it can't be returned, yielded, stored in a place made outside it, sent on a channel, put in a `shared`, or read by an arrow function kept outside it. Each is a compile error. A second name for it inside the block is fine, and so are reading it, handing it to a function and building values from what's read from it. This holds for a struct or an enum, or a value holding one: what a source can finish at the block's end. A number, text or bytes it lends, or a collection of them, is a copy and leaves as any value does. So does a channel's end, a `Channel`, `Sender` or `Inbox`, as the mailbox `with launch(...) as mailbox` lends: it's made to be handed to other tasks, and one used after its block fails with `Closed`.
- A struct holding something borrowed is borrowed too, so a wrapper like `LineProtocol(conn=conn)` works inside the block and can't leak out.
- Several handles in one `with` open left to right and finish innermost first, as one `with` inside another. If one fails to open, the ones already open run their `except:` cleanup.
- `with conn.split() as reader, writer:` gives a connection's read and write sides, for one task reading while another writes. Several names split what a source lends as a tuple, `-> with (Reader, Writer)`, as `examples/with_several.fen` does. Not built yet: `conn.split()`; a connection gives `conn.reader()` and `conn.writer()` today.
- `with` never locks.

Writing your own takes `-> with T` and one `lend`, outside any loop, that every way through it reaches unless it fails: a source that could finish without lending is a compile error, so once its call works, the caller's block runs. Setup goes above it and cleanup below:

```fennec fragment
def transaction(self) -> with Transaction fails DbError:
    with self._pool.take()? as conn:
        conn.begin()?
        # the caller's block runs here, with the transaction lent to it
        lend Transaction(_conn=conn) except:
            # the caller's block failed or crashed
            conn.rollback()
        # the caller's block succeeded: a failure here is the `with` statement's own
        conn.commit()?

# the `?` covers starting the transaction and committing it at the block's end
with db.transaction()? as tx:
    tx.insert(order)?

# the `?` covers opening the file and finishing it at the block's end
with fs.create("save.dat")? as file:
    file.write_text(data)?
```

A file or stream fails with an `IoError`, by case: `NotFound(path)` when nothing is at the path, `PermissionDenied(path, reason)` when the system or an `Fs` narrowed with `only` or `read_only` won't allow it, `AlreadyExists(path)` when something is at the path already, and `Other(reason)` for the rest. A case holds the facts a handler would use, the path as the program gave it, and only `PermissionDenied`'s and `Other`'s `reason` are words: "the system won't allow it", or the narrowed `Fs`'s own. `message()` makes the sentence, as "nothing is at `save.json`", so a failure that's handled builds none. A file's own methods, as `file.write_text`, fail with no path, since the file is all they were given. This holds on the real file system and a test's fake one. `fs.read_text_if_present(path)` and `read_bytes_if_present` give `None` when nothing is there, and fail for anything else, so a file that may not be made yet is `text = fs.read_text_if_present(path)? else ""`.

`fs.create` syncs the file to disk before putting it in place, and `file.sync()?` syncs at any other point. `fs.write_text` and `fs.write_bytes` do the same: the whole file is written beside the path, synced, and renamed into its place, and the rename is synced too, so neither a reader nor a crash finds half a file. `durable=False` skips both syncs, as `fs.create`'s does, for a file that doesn't matter if the power goes: a reader still never finds half of it. A device or a pipe is written where it is. A file in a folder the program can't add a file to fails with `PermissionDenied`, even where the file itself could be written, since written where it is, a full disk would leave half of it. A run killed partway leaves the file it was writing beside the path, hidden, and the next run to write in that folder clears it. Closing a writer flushes it, and a failed flush fails the `with` block. `std.io` has a `Writer` (`write`, `write_text`, `write_line`, `flush`) and a buffered `Reader` (`read_chunk`, `read_line`, `lines`), each failing with `IoError`. Files are both, a connection is both through `conn.writer()` and `conn.reader()`, which keep the network's words in a note, and `MemoryStream` is both in memory. `json`, `csv` and `pack` have `dump(value, ref out)` and `load(ref src)` beside `dumps` and `loads`. `stderr` is a `Writer` as it is, and standard output is one through `stdout.writer()`. Standard input is a `Reader` through `stdin.reader()`.

- Code after `lend` runs when the caller's block succeeds, and it may fail. That failure becomes the `with` statement's failure. `return`, `break` and `continue` in the caller count as success, so an early `return` still runs the cleanup, and a failing cleanup makes the function fail.
- `lend x except:` runs when the caller's block fails or crashes, and the block's own failure is the one that passes up. It's the one bare `except`, since cleanup doesn't handle the failure: the failure carries on up by itself, so there's nothing to name and nothing to lose. It's also the one `except` that runs for a bug, a crash, where everywhere else `except` means "failed". If the cleanup in `except:` fails too, its failure is attached to the block's as a note. With several handles, every one runs its cleanup, innermost first. A cleanup that fails is added to the block's failure as a note, and the rest of the cleanup is skipped. The old form, `lend x else:`, is refused with a fix that writes `except`. Running it on a crash is built natively: the crash waits while the cleanups run, innermost first, and a cleanup that crashes too is said, with the first crash the one that goes on. On a crash, the cleanup runs as it's written, and any failure out of it is noted under the crash and ends it: a `?` anywhere, a `fail`, or a `with` inside it whose source fails. For a failed block, a cleanup's `x = f()?` is a note too, and the rest of the cleanup is skipped. A `?` deeper in a line, or on a `with` inside the cleanup, still replaces the block's failure there.
- There's nothing for the caller to finish by hand, like a `commit()` or a `close()`, and nothing to forget.
- An interface can have a source, as `def transaction(self) -> with Transaction fails SqlError`. A generic function using it, `def saved[D: Database](db: D)`, runs the source of the type it's called with. A method can't call one: as a method has no copy of its own for each type, calling one there is a compile error that shows the generic function to write, `def saved[D: Database](repo: Repo[D])`.
- A source can't `lend` while it holds a lock it took. Opening a file inside a lock is fine.
- `for conn in parallel(server):` lends each connection to its own round's task, closed when that round ends. It's a plain loop, not `for ref`, and a connection's methods take `self`. A generator can't `lend` its items: there's no `Iterator[with T]`.
- Tasks started inside a `with` may borrow what it lent, since they always end first. From many tasks at once, only its read-only methods, like a server's `accept`.

### `launch` and channels

```fennec fragment
enum Sound:
    Play(name: str, volume: float)
    StopAll

def audio(inbox: Inbox[Sound], speakers: Speakers):
    for sound in inbox:
        match sound:
            case .Play(name, volume):
                speakers.play(name, volume=volume)
            case .StopAll:
                speakers.stop_all()

def main(speakers: Speakers, input: Input):
    with launch((inbox) => audio(inbox, speakers), stop_after=5s)? as sounds:
        for frame in input.frames():
            if frame.jumped:
                sounds.send(.Play(name="jump", volume=0.8))?
```

- `launch` runs a function alongside you until its `with` block ends, so no task outlives the code that started it. It's only ever written as `with launch(...)`.
- The arrow is the call you'd write anyway. `launch`'s own options, `stop_after=` and `inbox_size=`, sit outside it.
- A parameter typed `Inbox[T]` makes `launch` create the channel: the task gets the receiving end and you get the sending end, as `sounds`. The enum is the protocol, so you can only send real cases and the task must handle each one. A task with no messages takes no inbox: `with launch(() => autosave(rooms, fs, clock)?)?:`.
- The `?` on its line covers the task: `with launch(...)` fails with the task's errors and `Crashed`, from `std.concurrent`, where the block ends. `inbox_size=` is 16 unless given. Checked, and written in around `std.intrinsics`' `launch` and `launch_end`. A task runs on the runtime's threads, with what it captured copied for it.
- Channels are always bounded, and `send` waits when one is full. Both ends are plain values, so there can be many senders and receivers. `send(message)` fails with `Closed`, and `send_within(message, d)` with `Closed` or `Timeout`. `receive()` gives `T?`: the next message, or `None` once the channel is closed and empty. `receive_within(d)` gives `T? fails Timeout`. This holds for `Channel`, `Inbox` and `Sender`. A channel you set up yourself opens with a plain function, `with channel[Job](size=64) as jobs:`, as Python's `open()` is, not by calling the type. The runtime's half is built. `std.concurrent` has `Channel[T]`, its ends `Inbox[T]` and `Sender[T]`, and the errors `Closed`, `Timeout` and `Crashed`, written in Fennec over channel intrinsics. `channel[T](size)` is the only way to make one, since `Channel`'s constructor is private. It builds natively, and so do `send`, `receive` and `for job in jobs`, each message copied across as it's sent.
- A channel's message may be optional, as `channel[int?]` or a task's `Inbox[str?]`. `receive()` then gives an `int??`: `None` once the channel is closed and empty, kept apart from a `None` that was sent, and `for message in inbox` goes through every message, `None`s among them. An optional inside a tuple, a struct or an enum is sent as any field is.
- A channel delivers each message to one receiver by default, as a work queue. `to=.Every` gives every listener every message, as in `with channel[Hit](size=1024, to=.Every) as hits:`, and receiving reads the same either way, `for hit in hits`. With `.Every`, `send` never waits, and a listener that falls behind fails loudly, unless it asked for `when_behind=.DropOldest`. A listener asks with `for hit in hits.listen(when_behind=.DropOldest)`. `hits.inbox()` is a listener of its own too, hearing every message sent from when it's made. A plain `hits.receive()` or `receive_within` on a channel to `.Every` has no one next message to take: it's a compile error where `to=.Every` is written out and the channel is received from by the name it's lent as, and it stops the program where it runs otherwise. Loop over it, or take an inbox. There's no separate events type.
- A program stops with a report when its tasks are waiting only on each other, in every build, since a hang through channels alone can't be ruled out at compile time, and a release build would otherwise hang for good with nothing said. It covers channel sends and receives and blocks' ends: when `main` and every task are waiting in those for good, with no time limit, the program stops with exit code 1, after what it printed, saying who waits for what, as ``main is waiting at the end of a `with launch` block, for task 2``. A wait with a time limit, or on anything outside the program, is never one of them, so a program that waits on a channel nothing will send to, to run until Ctrl-C ends it, is stopped at once: wait with `signals.wait()` instead. Waits through a `parallel` call are caught too, at any number of threads: a call waiting for its ranges is one of the waits, and the report says each range waiting, by its call, as ``range 1 of main's `parallel` call is waiting to receive on a channel that's empty``, and a range that failed or crashed first. A run that's recorded is stopped the same way, and its recording ends with the deadlock, so its replay comes to the same one, and says it the same way; a replay that comes to a different deadlock, or one its recording didn't have, has diverged, and says so. A listener's wait on a channel to `.Every` is one of them too, when it's heard every message kept and the channel's still open: the report says it waits ``to receive as a listener of a channel to `.Every`, with nothing new for it``.

### Pools

```fennec fragment
with postgres.connect(net, entropy, "postgres://app@db.internal/shop?pool_size=20")? as db:
    for request in parallel(server):
        with db.transaction()? as tx:
            tx.execute(t"insert into orders (item) values ({request.item})")?
```

- `postgres.connect` always lends a pool. `pool_size=10` and `wait=30s` unless given, as an argument or in the URL, and giving one in both is an error.
- `with db.transaction()? as tx:` takes a connection for a transaction, and `with db.borrow()? as conn:` takes one for a block of statements. When every connection is in use, the block waits up to `wait` for one.
- Each connection lives inside one keeper task's `with`, and the keeper lends it to one block at a time, so nothing can leak or be used twice. The block uses the connection itself: nothing is sent to the keeper to run.
- A block that fails or crashes has its connection reset before the next block gets it, and replaced if the keeper can't prove it clean, so the pool heals itself and never shrinks.
- Pools are written in plain Fennec in `std.concurrent`, as `pool[T](open=..., size=10, wait=30s)`, which `examples/std_pool.fen` shows, and `postgres.connect` lends one.

### When tasks stop or crash

- When a `with launch` block ends normally, the task's inbox closes and the task drains it. If it's still running after `stop_after`, every wait it's in is cut short, socket and stdin reads included. A wait cut short by a stop ends the task quietly, with no failure reported, since the stop was asked for. A block that failed cuts them short at once.
- A task's failure comes out where its `with launch` block ends, and a crash comes out as `Crashed`. A crash ends only its own task. The HTTP server answers that request with a 500.
- A task that fails or crashes cuts short every wait its block is in, as a closed channel does: a `receive` gives `None` and a `send` fails with `Closed`. So the block reaches its end, and the failure or crash comes out there.
- **A crash inside `lock x as ref s:` rolls back.** The runtime puts back every value the block changed, in reverse order, and then releases the lock, so nobody ever sees a half-change. A `fail` or `?` leaving `lock ... as ref` does too, so a write lock either finishes or has no effect, as a database transaction does, while `return`, `break` and `continue` keep the changes. That covers one `lock` statement and all the values locked together in it, and not local variables or output. To keep a change and still fail, decide inside the lock and `fail` after it. There's nothing to write and no new failure type, and a lock is never left poisoned.
- **Nothing else rolls back:** outside `lock ... as ref`, a change that fails partway keeps what it changed before the failure, so `shelf["bolt"].shift(9)` failing partway still writes the part back through the type's own `def []=`, as far as it got.
- The compiler saves the old value before each write in such a block, and skips the saves where it proves the block can't crash, so most lock bodies cost nothing extra. Values taken out are freed when the block ends.
- Sending to a closed channel fails cleanly, and never waits.

Not in the language: `del`, destructors, moves, lifetimes, `unsafe` and global state. Left for when real programs need them: `select`, handles that move between tasks, and results returned from tasks.

## Unions

```fennec fragment
type Shape = Circle | Square

def report(shapes: list[Shape], stdout: Stdout):
    stdout.print(sum(shape.area() for shape in shapes))   # every member has area(), so Shape does
    for shape in shapes:
        if shape is Circle:
            stdout.print(shape.radius)                     # a Circle here
```

- A union mixes types that already exist, and an `enum` defines new cases. Each value is held in place with its member's tag, never boxed.
- `if x is Circle:` narrows `x` inside the block, and `match` handles each member, checked for exhaustiveness: a case naming a member, as `case Circle(radius=_):`, narrows the subject to it inside the case. After `if x is Circle: return`, `x` is what's left, the other members, and can be given where a union of just those goes. A method every member has is the union's too, and runs the one of the type the value holds. A `with` source every member has is the union's too, and runs the source of the type the value holds: the `with` is built once for each member. Called on a field holding the union, which `is` doesn't narrow, it's a compile error that says to name it first.
- `None` is never a member: `str | None` is an error whose fix writes `str?`, and an optional union is `(A | B)?`.
- A `T??` never collapses into a `T?`. A `T?` given where a `T??` goes, by `return`, as an argument, into a field or to a local written as `T??`, is the one inside, so a `None` it holds is kept apart from the `T??`'s own `None`. Built.
- Failures stay separate: a function that can fail returns `-> T fails E`, never `-> T | E`.
- A function that can fail says `fails` in its signature, so a reader sees how it can fail there. One that leaves it out is a compile error, and `fennec fix` writes it, as `-> str fails TooBig`, `fails IoError` for one that gives back nothing, or `fails ParseError | RangeError`. `main` doesn't say it, since nothing calls `main`.
- A failure is always an `error` type, and `fail` takes one. There's no `Ok`, `Err` or `Result`: a failure is handled at the call, under Failures. Writing one is a compile error that says what to write instead, and `fennec fix` rewrites most of them.

## Named arguments

```fennec fragment
def scan(rows: list[Row], limit=8) -> list[Row]:
    ...

scan(rows, limit=100)
copy_file(source, destination=backup, overwrite=True)
```

- A parameter with a default is passed by name only, and a literal default gives it its type. An empty collection's default is its constructor: `items = list[str]()`.
- Some positional arguments are a compile error whose fix names them: a bare `True` or `False`, neighboring arguments of the same type, and three or more unless no two neighbors could be swapped and still compile. The first argument and `self` are exempt.

## Operators

```fennec fragment
struct Mat4:
    columns: array[Vec4, 4]

    def *(self, other: Mat4) -> Mat4:
        ...

    def *(self, point: Vec4) -> Vec4:
        ...

struct Time:
    seconds: float

    # two moments in time give the time between them
    def -(self, other: Time) -> Duration:
        ...

    # a moment minus a length of time gives an earlier moment
    def -(self, length: Duration) -> Time:
        ...

world = view * model
moved = model * point
remaining = deadline - now
```

- The binary operators are `+`, `-`, `*`, `/`, `//`, `%`, `**`, `&`, `|`, `^`, `<<` and `>>`. Each has an assigning form, which applies it to what's named and writes the result back, as in Python: `+=`, `-=`, `*=`, `/=`, `//=`, `%=`, `**=`, `&=`, `|=`, `^=`, `<<=` and `>>=`.
- A type may define each operator once for each right-hand type, so `Mat4 * Mat4` and `Mat4 * Vec4` can both exist.
- Types must match exactly, with no conversions, so there's never a best match to choose. `now + now` is a compile error that lists the `+` that `Time` has.
- It's for operators only. A named function still has one meaning, so `load(path)` is always the same `load`.
- A user type gets this exactly as `std`'s types do.
- Units of measure use it, with no new syntax: `Meters` is a struct holding a `float`, and its operators say that `Meters / Seconds` gives `Speed` and `Meters / float` gives `Meters`.

## Failures

There are three outcomes. A missing value is `T?`, handled with `else`. A failure is `fails E`, passed up with `?` or handled with `except`. A bug stops the program. `else` on a call that can fail, and `else as why:`, are compile errors whose fix writes `except`.

```fennec fragment
def sell(ref self, item: Item) -> Item fails SellError:
    count = self._items.get(item) else fail .NotFound(item)
    if count == 0:
        fail .OutOfStock(item)
    self._items[item] = count - 1
    return item

# pass the failure up
value = load(path)?
# a missing value's default
count = counts.get(word) else 0
# must be there: if it isn't, that's a bug, and the program stops
first = items[0]

# one case, on the line
port = int.parse(text) except InvalidValue(): 8080

# any failure, named: the block leaves or gives a value
text = fs.read_text(filename) except why:
    stdout.print(f"Couldn't read {filename}: {why}")
    return

# several cases, as arms right under the name
score = load_score(fs, path) except why:
    case .Missing(missing_path):
        stdout.print(f"no save at {missing_path}, starting from 0")
        0
    case _:
        fail why
```

| Outcome | You write | Python |
|---|---|---|
| Missing | `else` | `d.get(key, default)` |
| Failed, pass it up | `?` | happens unseen |
| Failed, handle it | `except` | `except` |
| Fail | `fail` | `raise` |
| Bug | nothing: it stops | an uncaught exception |

- A failing call is handled at the call. There's no `Ok`, `Err` or `Result`.
- `except` covers only the call it follows. It comes after a whole right side, `x = f() except ...`, a bare call, `f() except ...`, or `return f() except ...`, and never inside a larger expression.
- **One case on the line**, `except InvalidValue(): 8080`, gives a value or leaves, as `except .Gone(): return` does. It must cover everything the call fails with, and an or-pattern can, as `except Closed() | Timeout(): 0`. When it leaves a case out, it's a compile error that names it and shows `except why:` with arms.
- **Any failure**, `x = f() except why:`, names it. The block gives `x` a value, or leaves with `return`, `fail`, `break` or `continue`, each checked against the function's signature. On `_ = f() except why:` the block needn't give a value.
- **`why` is used on every way out.** It's used when it's read into something that goes somewhere: an argument, an f-string, a field read, or `fail why`. Being the subject of a `match` or an `is` doesn't count, and neither does `_ = why`. A `fail` of a new error inside the block carries `why` along as a note, so it counts. Arms that name every case needn't read it.
- **The name after `except` is lowercase.** `except Timeout:` reads as Python's "catch `Timeout`", but here it would name any failure, so a capitalized name there is a compile error: "write `except Timeout():` to handle only `Timeout`, or a lowercase name to handle any failure". A second `except` line under the first is refused too, and points at `case` arms.
- **Several cases** are `case` arms directly under `except why:`, with no `match` inside. The arms cover every case. An arm wider than a named case, as `case _:` is, must use `why` or `fail`. In a later arm, `why` holds only what the earlier arms left: after `case Finished(at):`, `case _: fail why` passes up the rest, so the function needn't say it fails with `Finished`. An error with cases is left out once an arm has taken each of its cases, and a guarded arm takes nothing. `case other:` is refused there, since `why` already names the failure. An `except` block is all arms or all statements, never both.
- **On an error with cases, the type-wide pattern is refused under `except`.** `except IoError(): ""` would drop every disk failure by naming the type, so it's a compile error that says to name the cases or write `except why:`. An error with no cases, as `InvalidValue()`, keeps it.
- **Nothing is dropped without a word.** There's no bare `except:` after a call, no `except _:` and no `as _`. `_ = f()` on a call that can fail is a compile error, and `failure_of` is only for tests.
- **`else` means "otherwise"**: `if`/`else`, `a if c else b`, and a missing value's default. After `else` comes a value or a statement that leaves: `else return`, `else fail E`, `else break` or `else continue`. `else` on a call that can fail is a compile error that shows `except`, and `except` on a plain optional is a compile error that shows `else`.
- **On `T? fails E`**, `?` or `except` takes the failure off, and then `else` takes the `None` off: `user = db.find_user(name)? else return "Nobody here"`. An `else` for the call's own result on the same line as its `except` is refused, with the two-line form shown. An `else` inside the handler's value, as in `except InvalidValue(): 80 if secure else 8080`, is the conditional `else` and is fine.

```fennec fragment
found = db.find_user(name) except why:
    log.error(f"couldn't look up {name}: {why}")
    return "Sorry, try again in a minute."
user = found else return f"Nobody here is called {name}."
```

- **A call that can fail gets its own line.** It's the whole right side, a bare call, or `return`'s value, unless it has `?` or `.expect("reason")`, which nest anywhere: `total = parse(a)? + parse(b)?`. Anywhere else, as an argument or an operand, it's a compile error that shows the split onto two lines.
- **A `with` or `for` line takes only `?`.** To handle a failed open or commit in place, put the `with` in a function of its own and handle that call.
- `f().expect("reason")` crashes on purpose, with that reason, for a call that can't fail here. It's not a way to handle a failure: it says there isn't one. The reason has to say something: `.expect("")` is a compile error, "say why this can't fail".
- `try` isn't a keyword. It's reserved only so `try:` can be a compile error that shows `except` after the call.
- `T fails E` is made only by the runtime, as for the results of parallel work, and read only through `?` and `except`. What can fail is never a plain value: holding it in a name, printing it, comparing it, putting it in a collection or handing it on is a compile error, and `fennec fix` adds `?` where the function can pass the failure up. `.expect("reason")` and `assert ... fails` read it too, and an arrow function may pass it up with `?`, as `failure_of(() => load(path)?)` takes it.
- There's no `exit()`. `fail` out of `main` prints the failure to stderr and exits with code 1, which is built. `main` may also return an `int` as the exit code, which is built too. Nothing else can end the process.
- A failure passes up only with `?`, so every place one can is marked: `return load(path)` in a function that fails is a compile error, and `fennec fix` adds the `?`. An arrow function is held to it too: `failure_of(() => load(path)?)`, whose `?` passes the failure up as the arrow's own, not the function's around it. An arrow that can fail is refused where a builtin takes a function, as `sorted`'s `key=`, which nothing there expects to fail.
- `?` means the same everywhere, `parallel` included: a failure in one round passes up out of the loop once the other rounds stop. To let one piece fail alone, handle it in the body with `except why:`. `break` stops new rounds from starting and lets the running ones finish, and a failure passing up cuts them short, like a failed `launch`. A `return` in a round, written as a statement or in an `except` arm, leaves the function, as from a plain loop. A `parallel` body or a server's round with a `return` in it runs its rounds one after another. Each connection a server lends closes when its round ends, however it ends. `server.serve(handler)` turns a handler's failure into a 500, in plain Fennec. Not built yet: today's handler can't fail.
- `router.get("/notes/{id}", show_note)`, and `post`, `put`, `patch` and `delete`, add a route to a `std.net` `Router`, checked where it's written. The path is written out, starts at `/`, and names each parameter once, as a whole segment like `{id}`, at most eight. The handler takes the `Request`, then each parameter by name in any order, as a `str`, an `int` or a sized int, and gives back a `Response`. An arrow's parameters are `str`. A parameter that doesn't read as its type, as `abc` for an `int` or `300` for a `u8`, answers 404, as a path no route has. A handler can't fail: it handles its failures and answers every request. Two routes on one router in one function are refused when they're the same route, or when they call a parameter in the same place by two names. Each is written in as `router.handle("GET /notes/{id}", ...)`, which reads the parameters by place.
- Ignoring a returned value is an error, with `_ = call()` to discard one.
- `.Case` is required when the type is known, in values and in `case` patterns; `fennec fix` writes it.
- When a union's types share a case name, as `IoError | NetError` both have `NotFound` and `Other`, `.NotFound` is a compile error that names both types, and the fix writes `IoError.NotFound()`. Built, matched and built, with each type's form shown in the help. Not built yet: the fix itself, since a `case` pattern doesn't keep where its name is (#2462).
- An error carries notes, a `list[str]`, as Python's exceptions do: `failure.add_note(text)` adds one, and `failure.notes()` reads them. When a block fails and its `lend x except:` cleanup fails too, the cleanup's failure becomes a note on the block's, so neither is lost, and `main` prints each note under its failure.
- **Every error has words, `failure.message()`**, and so does a `fails A | B`, which gives the words of the one it holds. An error writes them with `def message(self) -> str`. One that doesn't gives the failure as it prints, as `SellError.OutOfStock(item="apple")`. They're made when they're asked for: nothing is kept in the failure for them. A field named `message` is an ordinary field. Every error and every `fails A | B` meets a bound asking for `def message(self) -> str`, or `def notes(self) -> list[str]`, whether it writes its own or has the language's. Every error in `std` writes `def message`, so printing one of them prints its words, as `"seven" isn't an int` for an `InvalidValue`. One with a `message` field gives that field.
- **Printing a failure prints its words**: `stdout.print(why)`, `f"{why}"` and `str(why)`. Inside another value, as a list, it shows as it is, and `repr(why)` always gives it as it is, as `repr(text)` gives text in quotes. When `main` fails, it prints the words, then the failure as it is on the next line, or the failure once when it has no words of its own.
- **An error's words come only from `def message`.** A `def str` on an error is a compile error whose help writes `def message`, and a `def message` that takes more than `self`, or doesn't give back a `str`, is one too. `why.message` on an error with no field of that name is a compile error whose fix writes `why.message()`.
- **A new failure raised inside `except` carries the words of the one handled as a note**, as Python's `raise ... from why` keeps its cause, and `notes()` reads it: under `except why:` and under `except Case():` alike. A failure is new unless it was made already: read from a name that holds one, or from one by a method of its own, as `why.with_note(...)` is. So `fail Wrapped(...)`, `fail Wrapped(...).with_note(...)`, a helper's result and `.Disk(why)` are new, and each side of `a if c else b` is judged on its own. `fail why` and `fail why.with_note(...)` raise the same failure, and carry what it had. A `fail` of a failure made already that isn't `why`, with `why` unused before it, is refused, as a `return` there is.
- **`why.with_note(text)` gives a copy of the failure carrying one more note**, for saying what was going on as it passes up: `fail why.with_note(f"while loading {path}")`. `add_note` changes a failure held in a mut name.
- `fails A | B` combines failures from different places: `?` passes any of them up as it is, and a function type can say it too, as in `(str) -> int fails ParseError | RangeError`.
- A function that can't fail goes where one that `fails E` is wanted, as a handler that can't fail goes where a server takes `(Request) -> Response fails E`: given by name or as an arrow, for a parameter, a field or what's given back. A generic's `E` is then nothing, so a call to it that gives back `T fails E` gives a plain `T`, and an `except` on it is refused as on any call that can't fail. Inside the generic, `?` and `except` on a call through it stay as they're written, and do nothing. It goes only where it's copied: what's passed with `ref` has to hold all its parameter does, so a `list[() -> int]` can't go where `ref list[() -> int fails E]` does, since a function that fails could be put in it.
- A test checks a failure with `assert load(text) fails Oops(line=3)`. The right side is a `case` pattern of what it fails with, `Oops()` matching any `Oops`. A failed one says what really happened: "it gave Config(...)" or "it failed with Oops(line=4)". It's only for `assert`: `fails` in an `if` is a compile error that points at `except why:`. `failure_of(() => load(text)?)` gives a test the failure itself, or `None`, to look at more closely. It's only for tests: anywhere else, a helper a test calls included, it's a compile error that points at `except why:`, which names a failure where it happens.
- A one-line `x if condition else y` and a missing value's `else` can't share an expression without parentheses, since `ports.get(name) else 80 if given else 8080` reads two ways: write `ports.get(name) else (80 if given else 8080)`, or give one of them its own line. `fennec fix` writes the parentheses the way it's read.
- Taking from an empty collection gives `None`: `list.pop()` returns `T?`, as a queue's `pop` does, so draining one reads `next = todo.pop() else break`. `pop(at)` stops the program on a position that isn't there, like `items[at]`. A dict's `pop(key)` stops the program on a key that isn't there, like `ages[key]`, and `remove(key)` gives `V?` instead: the value, or `None`, so `if ages.remove(name) is None:` asks whether it was there. A list's and a set's `remove(item)` give `T?` the same way, never stopping on what isn't there. `discard(x)` takes it out if it's there and gives nothing, on all three, for a line that only takes something out: `items.remove(x)` on a line of its own is refused, since nothing uses what it gives back, and the fix writes `discard`. `items.get(at)` gives `T?` for a position that may not be there, counting from the end when it's negative as `items[at]` does.
- `if x is not None:` lets `x` be read as what it holds, inside the block, and so does a field path, as `holder.cell`. A path stays checked until something could change it: an assignment to it or to what holds it, a `ref` call given what holds it, or a method taking `ref self` on it. A change inside it, as `holder.cell.health += 1`, or to a field beside it leaves it checked. Reading it as checked after it might have changed is refused, with a note pointing at the line. A lock, a `yield` or a task can't change it: nothing else can reach a local's fields, and a `shared` value's are reached only inside its lock.
- `std`'s errors have cases wherever callers would treat them differently. `IoError` has `NotFound`, `PermissionDenied`, `AlreadyExists` and `Other`, built. When `std` finds itself comparing an `Other`'s text, it adds a case.
- Text is read as a number with `int.parse(text)`, `float.parse(text)` and each sized number's own, as `f32.parse(text)` and `u8.parse(text)`, which fail with `InvalidValue`. `f32.parse` reads the text straight to the nearest `f32`, rounded once. A conversion `T(x)` never `fails`: `int("4")`, `float(text)` and `f32("1.5")` are compile errors whose fix writes `.parse`. A conversion that doesn't fit is still a bug that stops, as `u8(300)` does. `std.math.NAN` is the one NaN, and `std.math.INFINITY` infinity, for where one was made from text. `float.parse` reads `"nan"` and `"inf"` as Python's and Rust's do. Built: the refusal of `int(text)` and `float(text)`, whose fix `fennec fix` makes, and of `f32(text)` and the sized ints, whose help shows their own `.parse`. The standard library's own code reads text with the builtin, which its `parse` is written over.

## Functions

```fennec
def total(prices: list[float]) -> float:
    return sum(prices)

def describe(items: list[str]) -> str:
    # a block after `x =` ends with its value
    size = match len(items):
        case 0:
            "empty"
        case 1:
            "one"
        case _:
            "many"
    return f"{size} ({len(items)})"
```

- Every `def` that gives back a value says `return`. A bare value at the end of a `def` is a compile error, "a `def` gives its value with `return`", and `fennec fix` adds it. `return match x:` gives a `match`'s value, as `return f() except why:` does.
- A block after `x =`, as `x = match`, `x = lock` and `x = f() except why:` are, ends with its value. A `return` in it leaves the function, so this compiles and skips the line after it whenever the types match:

```fennec fragment
def load_settings(fs: Fs, log: Log, clock: Clock) -> Settings:
    mut settings = load_saved(fs) except why:
        log.warn(f"settings.json is damaged: {why}")
        # meant: use the defaults and carry on, but this leaves the function
        return Settings()
    settings.opened_at = clock.now()
    return settings
```

  Write the block's value as its last line, `Settings()`. `fennec fix` never adds a `return` inside such a block.
- `len` is a builtin, like `sum` and `sorted`: `len(items)`, as in Python. `len(x)` calls a type's `def len(self) -> int`, as `str(x)` calls `def str` and `x in xs` calls `def in`. `x.len()` and `x.abs()` on a value are compile errors whose fix writes `len(x)` and `abs(x)`.
- A value is shown by the name its type is written with, never with type arguments: a generic struct prints as `Holder(item=3)`, not `Holder[int](item=3)`, and a generic enum's case as `Choice.Picked(value=7)`, in `print`, an f-string, `str()` and a failed `assert`.
- `abs` is a builtin the same way: `abs(x)` for the numeric types, and for any type with `def abs(self) -> Self`, which `abs(x)` calls. Not built yet: `abs(x)` on the sized numbers.
- These names are reserved, and naming a local, a function, a type or a constant after one is a compile error that suggests another name: the builtins `len`, `abs`, `sum`, `sorted`, `min`, `max`, `range`, `enumerate` and `zip`, and the built-in types `int`, `float`, `bool`, `str`, `bytes`, `list`, `dict`, `set`, `array` and the ten sized numbers. That's the whole list: a builtin added later comes with a `fennec fix` rename. Fields, methods and named arguments may still use them, as they may use keywords.
- A builtin that isn't reserved, like `round`, `copy` or `reversed`, can name a program's own function, and that function is the program's alone: the standard library's code and a dependency's still call the builtin. The standard library's own helpers and constants are its own too, so a program's `padding` or `MOST_ROUTE_PARAMETERS` changes nothing std does, and a program reaches std's exported constants, like `math.INFINITY`, only by importing them.
- A name with two underscores in a row, like `conn__lend1`, is the compiler's: a name written by hand with them is a compile error that suggests it with one, wherever a name goes, an f-string's braces included. Text in a string is text. A program type's method, or a function written inside another, can't take a name the standard library's own functions have, like a `struct path`'s `is_device`.
- `case` is reserved too, since it starts an arm under `except why:`. std's serializer method that was `case` is `enum_case` already.
- There's no cap on how many keywords Fennec has.

## Visibility

```fennec
export struct Score:
    points = 0
    _best = 0                  # seen only by code in this file

export def best(scores: list[Score]) -> int:
    return max(score.points for score in scores)
```

- `export` marks what other kits can use, and goes on top-level types and functions only. Without it, a name is private to its kit, and inside a kit every file sees everything. `fennec fix` rewrites the old `pub`.
- A `_name` field or method of a type is private to the file that declares its type. Code written in that file reads, sets and calls it, builds the type with it and matches it in a `case`: the type's methods, the file's functions and other types, an `extend` written there and the tests written there. Code in any other file can't, in the same kit or another. Tests get no special access: a test in the file has the file's, and one anywhere else has none.
- A top-level name never starts with `_`, since it's already private to its kit. That's a compile error, and `fennec fix` renames it everywhere. The standard library follows the same rule.
- Other kits match an exported type's fields by name: `case Score(points=p)`, `case Point(x=a, y=_)`, or `case Score()` to match it without reading a field. Matching another kit's type by position is a compile error, and `fennec fix` adds the names. A kit's own code can still match by position, and cases with no fields, like `.Wait`, are unchanged.
- Fields with defaults come after the ones without, in a `struct`, an `error` and an enum case, as parameters with defaults do. A field without one after a field with one is a compile error.
- From outside a type's file, its automatic constructor sets the public fields, and values given by position go to those. `_` fields take their defaults. A `_` field with no default is set by a named constructor, a method without `self` that builds one, or by code in its file, which can always build it, as std's `channel()` builds a `Channel`.
- So a kit can add a field with a default to a type it exports, and no other kit's code that builds or matches it breaks.

## Saving data

```fennec fragment
from std import json

text = json.dumps(world)
back = json.loads[World](text)?
```

- JSON, TOML and `std.pack` save any type whose fields can all be saved, through one `serialize` and `deserialize`.
- An `f32` is saved as the `f32` it is: JSON writes its own shortest text and reads text straight to the nearest `f32`, rounded once, and `std.pack` writes its 4 bytes. Each `f32` NaN packs as the one quiet NaN, as a float's does. A `float` field loads an `f32` saved before, exactly, and an `f32` field doesn't load a `float`, which it would round. TOML reads its numbers as floats, so an `f32` there is its float rounded. The other sized numbers aren't saved yet.
- A generic function writes a `T: Serializable` and reads a `T: Deserializable`, as `def answer[T: Serializable](status: int, value: T) -> str: return json.dumps(value)`. Each call's own type is checked against the format where the call is: `answer(200, value=set[int]())` is refused there, as JSON can't hold a set, and so is a call that hands its own type parameter on to one.
- A `Table` saves as a list of its items in order, and an `Id` as where its item is in that list. Loading makes a new `Table`, and every `Id` loaded with it finds its item there. An `Id` has to be in a `Table` saved with it.
- Saved slot for slot, as `pack.dumps(value, exact=True)` does, a `Table` keeps its slots, so a load hands out the same `Id`s after it. An `Id` saves as its slot only where it finds its item. One whose item has gone saves as missing when it's optional, and otherwise stops the save, since after the load it would find whatever is in its slot. A load refuses an `Id` whose generation isn't its slot's, as one saved after its item went by a version that kept them. Built.
- A union saves tagged with its member's type name, as in `{"Circle": {"radius": 1.0}}`.
- A JSON `null` is a value, and a field left out is missing. A default fills only a field left out. So a field that isn't optional fails on `null`, as in "`x` should be an int, but it's null", default or not. An optional field holds `None` for `null` even when its default is something else, so `dumps` then `loads` gives back what was saved. A field left out takes its default, or `None` if it's optional, and otherwise fails as "`x` is missing". SQL's `NULL` reads the same way.
- A `T??`, an optional holding an optional, can't be saved as JSON, since `null` can't tell `None` from an optional holding `None`: that's a compile error. Built, for every format.
- JSON has no NaN or infinity, so `json.dumps` of a value holding one stops the program, saying where it is, as in "`[1].values[1]` is nan, which JSON can't write". It's a bug in the program, as a NaN reaching `int()` is, so `dumps` itself can't fail.
- `@skip` and `@serialize(name="uid")` on a field say how it's saved. A kit may define annotations of its own for fields, which generic code reads through `fields(T)`. A generic function's `for f in fields(T):` is built, checked once, with each field's name and value, and annotations a kit defines and reading them aren't built yet. A function takes only `@hot`, so routes are registered in plain code, as in `router.get("/todos/{id}", show_todo)`.
- A `Secret[str]`, from `std.crypto`, can't be saved, printed or logged, and its text comes out only through `expose_secret()`, whose name says so. It can be loaded, as what it holds, so a password in a request's body comes in as a `Secret` and never sits in a plain `str`: loading brings a secret in, where saving would let one out. Printing one, an f-string or `str()` of one, and saving one in any format are compile errors, and so is doing any of those to something that holds one. Printed through a generic function's `T`, in a failed `assert` or in the failure that ends `main`, it shows `Secret(hidden)`. `==` on two secrets compares them in constant time, so its timing never says where they differ, and a secret of anything but text or bytes can't be compared. `Secret.of(text)` makes one, and `Secret[bytes]` holds a key. A server's `Certificate`, from `std.net`, holds its private key as one, beside its chain of certificates.

Loading is safe on untrusted input:

- A load always makes fresh values, never a link to anything already in the program.
- Every `Id` resolves within its own document, and every `Id` a program makes carries a tag no other has, so an `Id` from one `Table` finds nothing in another of the same type.
- `where` rules run again on load, so a value that breaks one fails the load.
- Unknown fields are refused unless `extra=` says otherwise: `json.loads[Settings](text, extra=.Ignore)`. TOML always refuses them. Not built yet: `extra=` for TOML. `std.pack` loads data from an earlier version of a type by its fields' names: a field the data has and the type doesn't is passed over, a field the type has and the data doesn't takes its default, and one with no default fails the load, naming it. Cases and union members are found by name too, and an `int` field that's a `float` now reads as one. Each value from `dumps` carries its type's layout, its field and case names. A `pack.Packer` writes many values with each layout once, and `pack.Unpacker` reads them back, from a file or fed from a stream. `fennec update` locks a kit's layouts in `pack.lock`, and a locked type's single values leave the layout out, as short as before layouts. `docs/pack.md` pins the format byte for byte, its header, shape hash and layout, with test vectors.
- JSON and TOML refuse what they'd have to guess at: a number too big for a float, a struct's field given twice, a dict's key given twice, and a dict's int key written any way but its plain digits, or not in quotes. A key given twice fails, naming it, as in "the text has `a` twice", never letting the last copy win, since two readers that disagree on which copy wins let a value past a check. `std.pack` refuses it too.
- Untrusted input has depth and size limits, and a reader that meets bad input fails with something its caller can handle: it never stops the program, never takes time or memory past a constant times the input, and never gives a value the input doesn't say. Each reader is fuzzed against its own rules with inputs malformed, cut short, huge, deeply nested and not UTF-8. The limits:
  - `std.pack`: lists, dicts, structs and cases nest at most 64 deep, one text or `bytes` holds at most 16,000,000 bytes, a load reads at most 16,000,000 items in all, and at most 65,536 that take no bytes, like a struct with nothing saved in it, which would otherwise be made from nothing. A `pack.Limits` given to `loads` changes them.
  - JSON and TOML nest at most 500 deep. They don't limit the text's size, a string's length or a collection's length: what they make is never more than a few times the text, so the limit on what's read, as a server's on a body, bounds them.
  - The HTTP server reads at most 8 blank lines before a request, a request line and headers of at most 16,384 bytes together, at most 100 headers and a body of at most 10,485,760 bytes, by its `content-length`: a chunked body is answered 501 for now. Past them it answers 414 for the request line, 431 for the headers and 413 for the body, and closes the connection. All the program's servers together hold at most 268,435,456 bytes (256 MiB) of request bodies at once, each counted from when its head says how long it is until its connection starts on a next request or closes: a body that would go past that is answered 503, unread, so many clients each sending a big body at once can't take the program's memory. Each body is read into room made once at its size, filled as it comes. A server that runs out of files to take a new connection with ends the connection that's waited longest for its next request, as if its client had closed it, and takes the new one. The worker taking connections ends its own longest-waiting one, and when it has none asks the other workers in turn, so it's the longest-waiting on one worker, not of all. On Linux the new connection waits for that room; macOS drops a connection that finds no file at once, so there the room goes to the next. A request's target is a path, `*` for `OPTIONS`, or a whole `http` or `https` URL, whose host then stands for the `host` header; anything else is a 400.
  - The HTTP client reads at most 102,400 bytes in one line of a response's head, 1,048,576 in the status line and headers together, 1,000 headers, and a body of at most 67,108,864 bytes unless `most_body=` says otherwise, and follows at most 10 redirects. Past them the request fails. A status is read only from a line that starts `HTTP/1.` and a digit, with three digits from 100 to 599 after it; any other first line fails the request.
  - `Time.parse` reads the years from 1678 to 2261 in UTC, the years a `Time` holds, and fails on a moment its offset takes past either end.
  - `int.parse`, `float.parse` and the sized ints' `parse` read text of any length, in time in proportion to it. A failure quotes text past 40 characters by its first 20 and its length, so a hostile number can't make a message as long as itself.

## Strings

```fennec fragment
usage = """
    Usage: hoard [options] <path>

      --dry-run    say what would change
    """
# usage is "Usage: hoard [options] <path>\n\n  --dry-run    say what would change\n"

greeting = f"""
    Hello, {visitor.name}!
    You have {count} new messages.
    """
```

- A string is written in `"` on one line, or in `"""` across lines. `f`, `t` and `r` go before either: `f"""` is an f-string, `t"""` a template and `r"""` a raw string, each across lines.
- Between `"""`s, newlines are kept. A newline right after the opening `"""` is dropped, so the text can start on the next line.
- When the closing `"""` is on a line of its own, the spaces before it are taken off the front of every line, so the text can be indented with the code around it. The newline before that closing line is kept, so the text ends with one. A line with fewer spaces than the closing `"""` is a compile error, which says how far to indent it. A line of nothing but spaces may have fewer, and is empty. Text right after the opening `"""` isn't counted as indented. When the closing `"""` comes after text on its line, nothing is taken off.
- Escapes and `{}`s work as in a string on one line: `\n`, `\"` and the rest, and in an f-string each `{}` opens and closes on one line, with no `"` inside. `\"` writes a quote, so three in a row are written `\"\"\"`. A `\` can't end a line.
- A `"""` that's never closed is a compile error naming the line it opened on, which may be far above where the file ends.
- `fennec fmt` keeps what's between `"""`s byte for byte, and `fennec lint` and the language server read it as one string.

## Templates

```fennec fragment
# the literal pieces and the values stay apart
rows = db.query(t"select name from players where team = {team} and score > {least}")?
page = html.render(t"<p>Hello, {visitor.name}</p>")?
result = process.run(t"git log --author={author}")?
```

- `std.sqlite` takes them: `db.execute(t"...")` and `db.query[T](t"...")`, with each value bound as a parameter and each row read into a `T` by its columns' names. In a test, each path is a real WAL database file of the test's own, so a test reads as a program does.
- A SQLite `Database` reaches only its own file, as what it was opened through allows. It takes only what ordinary queries need: reading, inserting, updating and deleting rows, transactions and savepoints, functions, creating, changing and dropping tables, indexes, views and triggers in its own database, and temporary ones of the connection's own. Anything else fails with `SqlError`, as `ATTACH` of a file, `DETACH`, `VACUUM INTO`, `ANALYZE` and any pragma but the two `std.sqlite` runs itself do. SQLite's defensive mode is on, with the schema not trusted. A plain `VACUUM` still works.
- `std.postgres` speaks Postgres's protocol in plain Fennec: its messages, signing in with SCRAM-SHA-256, and a query as the extended protocol's messages, its template's values bound apart and its rows read by column type into `sql.rows_as[T]`. `with postgres.connect(net, entropy, "postgres://app@db.internal/shop", password=secret)? as db:` lends a `PgPool`, a pool of connections through `std.net`, 10 unless `pool_size=` says. `with db.transaction()? as tx:` and `with db.borrow()? as conn:` each lend one connection to a block, with `execute` and `query[T]` as `std.sqlite` has, and wait up to `wait=`, `30s` unless it's set, then fail with `Timeout`. `pool_size` and `wait` can be in the URL too, as `?pool_size=20&wait=5s`, but not in both. A URL with a password in it has to be a `Secret`, as `env.secret("DATABASE_URL")?` reads one. A plain `str` URL with a password is refused, at compile time when it's written in the source, and otherwise the password goes in `password=`, a `Secret`. A failure shows the URL with its password as `****`. Its connections are TLS unless `tls=False` says otherwise, and a server that won't take TLS is refused rather than spoken to in the clear. It lives in `std` until first-party kits have a home. Not built yet: TLS picked by host, in the clear for a loopback host and verified TLS for any other, with the URL's `sslmode=` taking `disable`, `require`, `verify-ca` or `verify-full`, and refusing `prefer` and `allow`; today the URL takes only `pool_size` and `wait`.
- `t"..."` makes a `std` `Template`, as Python 3.14's t-strings do. It holds the literal pieces and the values separately, and the values' type comes from where it goes, as in `Template[SqlValue]`. A `Template[T]` has `strings`, one more than its `values`, plus each value's `expressions` as written and its `format_specs`, and `interpolations()` gives each value with both.
- `html.render(t"...")` gives an `Html`, each value put in for where it is in the page, found from the text around it. Between tags and in a quoted attribute it's escaped. Where escaping isn't enough, inside a `<script>` or `<style>`, an `on...`, `style` or `srcdoc` attribute, an attribute without quotes, a name or a comment, the page fails with `HtmlError.Misplaced`, naming the value. So does a value in a link's attribute, like `href` or `src`, that would make the link anything but `http`, `https`, `mailto` or relative, like `javascript:`: nothing is put in its place silently, and a program that wants a fallback checks the URL first. An `Html` goes into another as it is, and `html.join` puts pieces one after another, so pages are built from pieces. An `Html` is never loaded from saved data.
- `process.run(t"git log --author={author}")` runs what the template says, its words split at the spaces in the text written in the source. A value stays part of one argument whatever it holds, so it can't add an argument or start a command. A value that starts an argument with `-` fails with `ProcessError.LooksLikeAnOption`, naming the value, since the program would read it as an option, like git's `--upload-pack=`. A value later in an argument, like `--depth={depth}`, is fine, and so is one after an argument the template writes as `--`. `process.run("git", ["log"])` takes a program and a list of arguments too.
- A program `process.run` starts is stopped if it's still running after `timeout=`, 10 minutes unless given, or writes more than `most_output=` bytes, 16 MB unless given, to its standard output or error. Each fails with a `ProcessError` case of its own, `TimedOut` or `TooMuchOutput`. It's waited on off the task's worker, so a program that hangs holds up nothing else. On Unix it runs in a process group of its own, and stopping it stops what it started too, as `sh -c` and `make` start programs, and the timeout covers its outputs closing.
- `net.get`, `net.post` and `net.request` fail with `NetError.Other` once a response's body passes `most_body=` bytes, 64 MB unless given, however it comes: by its length, in chunks, or until the server hangs up. A length past it is refused before any of the body is read.
- `compress.gunzip`, `unzlib` and `inflate` fail with an `InvalidValue` once what they give back would pass `most=` bytes, 64 MB unless given, so a small file can't fill memory.
- `process.only("git")` lets a `Process` start only that program, by name. It's a guard against mistakes, not against code that means harm: the program it lets start can still be told to run another, as `git -c alias.x='!sh -c ...' x` does, and it gets this program's whole environment, so a `Process` reaches what `Env` holds.
- `fs.read_only()` gives an `Fs` that reads and never changes anything: writing, making a folder, removing and creating a file fail with `IoError.PermissionDenied`, "`notes.txt`: this Fs only reads, so it can't change it". With `only`, either way round, it reads one folder.
- `net.client()` gives a `Net` that only connects out, `net.server()` one that only listens, and `net.hosts(["api.example.com", "*.cdn.example.com", "10.0.0.5:5432"])` one that only connects out, to those hosts alone. A host is matched by its name as written in the URL or `connect`, in any case: `*.` covers the names under a domain but not the domain itself, an address matches itself, and `:port` holds it to one port. A request's redirects are held to the list too, and one elsewhere fails rather than being followed. A UDP socket, which could send anywhere, isn't opened. What's refused fails with `NetError.Denied`, saying why. Narrowing again, any of these, only takes more away. `hosts` holds where a kit may connect by name. It isn't a guard against a listed name that leads to a private address: that's a later `net.public()`.
- A `str` is never a `Template`, so `db.query`, `html.render` and `process.run` can't be handed text built by joining strings, and SQL, HTML and shell injection can't be written. There are no per-library string prefixes.
- `LiteralStr` is a string that came from the source code, as Python's `typing.LiteralString` is. Only a string literal, or literals joined with `+`, makes one. It goes anywhere a `str` goes, but a `str` is never a `LiteralStr`. A `Template`'s text pieces are `LiteralStr`, so building one from text read while the program runs is a compile error, and any function may ask for one. It's for strings only.

```fennec fragment
def order_column(sort_by: SortBy) -> LiteralStr:
    # user input picks between literals, so the result is still a LiteralStr
    match sort_by:
        case .Name:
            return "name"
        case .Score:
            return "score"

def order_from(request: Request) -> LiteralStr:
    # a compile error: text read while the program runs is a str, never a LiteralStr
    return request.query["sort"]
```

## Calling C

C is called only through the standard library and first-party kits, whose binding to C sits in reviewed runtime code. A program sees a safe API, like `with Window.open(...)? as window:`, and can't call C itself: `extern` and `unsafe` are reserved words that nothing uses. Not built yet: first-party kits binding C, and a window.

## Untrusted code

Fennec has no sandbox for untrusted code: don't run code you don't trust, and don't add dependencies you don't understand. Capabilities limit what any dependency can do, with no `unsafe` and no C outside std and first-party kits, but that's no licence to run code nobody has read. The Den scans, lints and reports each kit's capabilities, and never runs a kit's code. No program loads and runs code at run time.

What each dependency holds is worked out by the compiler, never written. It follows every capability the program's code hands to each kit, the kits its dependencies depend on included, and `kit.lock` records what each kit holds beside where it comes from, as `holds http path:../http Net`. A check, run, build or test stops where a kit would now receive a capability `kit.lock` doesn't list for it, saying where it's first handed over, and only `fennec update --accept` accepts it: it lists each change and writes the lock, to be reviewed with the code that made it. Plain `fennec update` lists the same and fails. No tool accepts one during a build, and no fix does. A kit's grants belong to its source: a dependency pointed at another path, URL or fork holds nothing until `--accept` accepts them again. Handing a kit less never stops anything: `fennec update` narrows the lock, and `fennec update --check` fails, for CI, when the lock isn't what the code hands over, either way. `kit.toml` has no `gets`, and refuses one. See the guide's Dependencies.

- A capability is one of the eleven, `Stdout`, `Stderr`, `Stdin`, `Args`, `Env`, `Fs`, `Clock`, `Entropy`, `Process`, `Signals` and `Net`, or one of the open handles: a `File`, a `Server`, a `Connection`, a `UdpSocket` and a `Database`, which reach a file, the network or storage through what they have open. A `Secret` isn't: it carries no authority.
- A kit receives what's handed to its functions and methods (the value a method's called on included, and each member's method of a union), what its operators are given, and what goes into the building of its structs, errors and enum cases. It receives what's written into its value after it's built, too: a field set, an item appended or set, and what a place inside it lent with `ref` to other code can hold. Through a generic function, a value is what its type is in the copy the program makes: `helper.save[S: Store](fs)` hands `helper` an `Fs`, though `helper` never names `Fs`. A generic type's methods are followed for each type the program has a value of, as printing one calls its `str`.
- A function handed over holds what it reads from around it: nothing for one at the top of a file, what an arrow function or a nested `def` captures, and anything the program holds for one whose making can't be seen, as a parameter or one inside a list. A kit's function used as a value, the kit's own code handing it out included, gives that kit what its parameters take, and so does an arrow function a kit makes, which is called with that. An iterator handed over is a function too: a generator called right there holds what it was given, and any other may hold anything. A failure from one of several holds what each of its errors holds.
- What's handed to the program's own code, or to std, isn't counted, and neither is a dependency's code that nothing reaches.
- Not built yet: narrowing a capability where it's handed over.

A program's own `main` is its grant: what `main` takes is everything it may do, and what a dependency holds is worked out and kept in `kit.lock`. `fennec fix` never adds to a `main`. A kit's program is `src/main.fen`, and every other program, for another place it runs, a tool or a task, is a file of its own in `entries/`, run as `fennec run <name>`. `kit.lock` records what each program takes, kept up to date by every build: a `main` that takes more builds, and the lock gets what was added, said as it's built, so a widening shows in the source and in the lock. A bare name is a program in `entries/`, anything else a path; a program imports the kit's `src` modules, nothing imports a program, a program's file may hold tests, and a dependency's programs are never built or reached. A program is only a `main`, forever: no `cfg`, no feature flags, no profiles, no shell-string scripts and no install, pre or post hooks. Settings are plain values in a `Config` each `main` builds. Not built yet: `entries/`, `kit.lock`'s record of what each program takes, and `fennec fix` leaving `main` alone: today it gives `main` a capability it's missing, as it does any function. See the guide's Several programs in one kit.

## Capabilities

```fennec
def main(stdout: Stdout, fs: Fs):
    stdout.print("hello")
```

A function reaches outside itself only through what's in its signature, so there's no global `print`. The error message has to teach this, since it's the first thing newcomers hit: calling `print`, or reading `stdout` or `fs` in a function that wasn't given it, says how to give it one, or what it's called there.

What `stdout.print` prints goes out a line at a time to a terminal. To a pipe or a file it goes out in batches: the first line at once, then whenever 8 KB has been kept or the program is about to wait, and, once 100 ms has gone since the last write, at the next line the clock is read for. That's the 1st, 2nd, 4th, 8th and so on since the write: a program printing fast reads the clock a few times in 8 KB rather than at every line, and one printing a line now and then has each go out as it's printed. So a program killed partway loses only its last moment's lines. One case is left: lines printed just before a long stretch of work with no prints or waits stay kept until the next one, and a program killed during that stretch loses them. A recording and its replay write only at waits and at 8 KB, as both break the output in the same places whatever the clock says.

## A whole program

A chat server: rooms that every connection shares, a database transaction for each message, and an autosave task.

```fennec
from std import json, sqlite
from std.log import Log
from std.net import Connection, NetError
from std.sql import SqlError
from std.sqlite import Database

struct Room:
    messages = list[str]()

def join(rooms: shared dict[str, Room], room_name: str, user: str):
    lock rooms as ref current:
        if room_name not in current:
            current[room_name] = Room()
        current[room_name].messages.append(f"{user} joined")

def say(rooms: shared dict[str, Room], room_name: str, user: str, text: str):
    lock rooms as ref current:
        current[room_name].messages.append(f"{user}: {text}")

def recent(rooms: shared dict[str, Room], room_name: str) -> list[str]:
    lock rooms as current:
        return current[room_name].messages[-20:]

def serve(conn: Connection, rooms: shared dict[str, Room], db: Database) fails NetError | SqlError:
    user = conn.read_line()? else return
    room_name = conn.read_line()? else return
    join(rooms, room_name=room_name, user=user)
    for line in conn.lines()?:
        say(rooms, room_name=room_name, user=user, text=line)
        with db.transaction()? as tx:
            _ = tx.execute(
                t"insert into messages (room, author, text) values ({room_name}, {user}, {line})",
            )?
        # the network write waits, so it happens after the lock is released
        for message in recent(rooms, room_name):
            conn.write(f"{message}\n")?

def autosave(rooms: shared dict[str, Room], fs: Fs, clock: Clock) fails IoError:
    while True:
        clock.sleep(30s)
        # your own copy, so the lock ends before the file is written
        snapshot = lock rooms as current:
            current
        fs.write_text("rooms.json", json.dumps(snapshot))?

def main(net: Net, fs: Fs, clock: Clock, stdout: Stdout):
    log = Log(out=stdout)
    shared rooms = dict[str, Room]()
    with sqlite.open(fs, "chat.db")? as db:
        _ = db.execute(t"create table if not exists messages (room text, author text, text text)")?
        with launch(() => autosave(rooms, fs, clock)?, stop_after=5s)?:
            with net.listen(9000)? as server:
                # each connection gets its own task, and closes when its round ends
                for conn in parallel(server):
                    # one client's failure ends only its own round
                    serve(conn, rooms, db) except why:
                        log.warn(f"dropped a client: {why}")
```

- `rooms` is made in `main` and passed down, so every function that touches it says so.
- Nothing waits inside a lock: `recent` copies the messages out, and the writes to the client happen after.
- A failure in one connection is handled in its round, so it ends only that round. Without the `except why:`, it would pass up and end the server. When `main`'s block ends, the autosave task is stopped, the server and every connection close, and so does the database.
- There's no lifetime, move, destructor or `async` anywhere in it.

## Building

- `fennec run` runs the tests a change affects, then the program, built through LLVM a function at a time and cached. Where LLVM can't build something yet, it says why, as `fennec build` does, and `fennec test` fails the tests that reach it. `fennec` has no interpreter and no JIT.
- A native program is small: `fennec build` links one runtime, and the program's syntax isn't in the image, so a hello-world is about 1.1 MB.
- The launcher runs the toolchain a kit asks for, `fennec = "0.4.2"` in `kit.toml`, pinned by its signed manifest's hash in `kit.lock`. It fetches one only when it's missing, and never with `--offline`, and only `fennec update` moves a pin. It's `fennec-launcher` for now. Not built yet: the release keys, so nothing it fetches verifies today.
- A release's archives are the same bytes however often and wherever they're built, so anyone can build one again from its tag and check it against the signed manifest. CI builds each platform twice and compares them.
- `fennec build` makes a native program through LLVM, and `fennec build --release` optimizes the whole program as one.
- `embed_file("path")` gives a file's `bytes`, and `embed_folder("path")` a read-only map from relative path, always with `/`, to `bytes`, in sorted path order. Both are views into the program's read-only data, with no copy and nothing loaded at startup, and need no capability. A missing path, or anything above the folder holding the nearest `kit.toml` (a lone file's own folder when there's none), symlinks included, is a compile error. Hidden files are left out, and nothing is compressed. `embed_folder` gives a `std` type with `get(path) -> bytes?`, `len`, and `keys()`, `values()` and `items()` in sorted order. Not built yet: holding them in the program's read-only data, which the runtime can't read without `unsafe`. Today each file's bytes are copied from the program once, the first time any thread reaches them, and every thread reads that copy. An `embed_folder`'s two lists are made once on each thread that reaches them.

## Checks

- Every check stays, unless the compiler proves it can't fail: overflow, bounds, missing keys and divide by zero. A proven check is removed in every function, not only in `@hot` ones.
- `x / 0.0` stops the program, as Python's `ZeroDivisionError` does, and so does integer division by zero. The compiler removes the check where it proves the divisor isn't zero.
- A broken `where` rule stops the program in code, and a load that breaks one fails cleanly.
- `@hot` allows a leftover overflow check, and refuses any leftover bounds, key or integer divide check as a compile error that says why. It allows a leftover float divide check too, as it does overflow.
- Speed never comes from dropping a check: there's no unchecked arithmetic and no switch that turns checks off. There's no `@fast_math` either: a float result is the same bits on every machine and every build, which replays and lockstep games depend on.

## Speed

- Everyday builds stay fast, and release builds can take as long as they need.
- `sum()` over floats, `dot()`, and `std.math`'s vector and matrix reductions add in one fixed pattern, 8 running totals combined pairwise, so they're fast, the same bits on every platform, and more accurate than adding left to right. That holds for `sum`, over a list or one item at a time, and for `math.dot(a, b)` over lists. `std.math`'s sums of four, as in `Vec4.dot` and `Mat4`'s products, keep glam's left-to-right order, which is as fixed, so they match glam bit for bit. `math.fma(a, b, c)` rounds `a * b + c` once, the same everywhere. Not built yet: `math.fma`.
- A float `sum` over `parallel(...)` may round differently from the same sum in a plain loop, and from the same sum written with an `if` or a step that can fail, since each form adds in a pattern of its own. Each form gives the same bits on every run and at any thread count: `sum(x for x in parallel(items))` and `sum(x for x in parallel(items) if True)` can differ in their last digits, but neither ever changes.
- `@hot` turns a hidden copy into a compile error, and the tracer shows where each copy happens and why. Anything that may allocate or copy in a `@hot` function, or in what it calls, is an error that says why, and so is any check native code keeps there, like an index it can't place inside its list. An overflow check the compiler can't prove away stays, as the safety net it is everywhere.

## Still to decide

Nothing right now.
