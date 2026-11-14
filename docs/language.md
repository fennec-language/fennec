# Fennec on one page

This page is the source of truth for the language. If `decisions.md` disagrees with it, this page wins. Most of it is built, and what isn't says so.

Fennec reads at a glance, builds almost instantly, and is built to run as fast as hand-written systems code in release builds. Everything is a plain value by default, and the compiler does the hard work.

## Four rules

- A name holds its own value.
- `mut` lets a name change.
- To change something in a list, write to it there.
- `ref` lets a loop, a function or a local name change yours.

```fennec
def heal_all(ref units: list[Unit]):
    for ref unit in units:
        unit.health += 10

def main(stdout: Stdout):
    mut units = [Unit(name="Ana", health=50), Unit(name="Bo", health=70)]
    units[0].health -= 10
    heal_all(ref units)

    target = units[1]              # your own value
    updated_health = target.health + 1
```

A local `ref target = units[target_id]` links a name to a place for several changes in a row. It lasts until its last use, inside its block. While it's alive, nothing else may change that container, and a second `ref` into it is refused, but reading it is fine and sees the change. Its index is worked out once, at the `ref`. It's checked at compile time and costs nothing at run time. Built. There's no scoped `with units[target_id] as ref target:`. The one function that may return a `ref` is a type's `ref` form of `def []`, which gives a place, so `units[i].health -= 1` works on your own types too.

## What you can declare

```fennec
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

```fennec
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

```fennec
found = (n for n in numbers if n % 2 == 0)
stdout.print(sum(found))
for even in found:
# error: this generator has already been used
#        a generator can be used once: `list(...)` keeps its items to use again
```

One silent mistake remains: changing a `mut` copy and then reading it, believing the list changed. A rule gets added for that only if testing shows real bugs.

## Loops

```fennec
for unit in units: ...
for ref unit in units: ...
for unit_id, unit in units.items(): ...
for unit_id, ref unit in units.items(): ...

for at, name in enumerate(names): ...
for at, ref score in enumerate(scores): ...
```

- `enumerate` takes anything you can loop over and gives `(index, value)`. Built. `for i, ref x in enumerate(xs)` needs `xs` to have positions, `len` and `[]`, as `for ref` does. Built.
- `for unit_id, ref unit in units.items()` changes each item of a `Table`, or each value of a dict, where it is, with its `Id` or key, which isn't `ref`. The body can't change `units` itself. Built. A `for ref` straight over a dict or a set is a compile error, since its keys and items are what find them.
- A `for ref` body may read what it loops over, as in `unit.target = nearest(units, unit.at)`, and sees the item as it is now, changes made through the loop's name included. Built for lists, arrays and `Table`s.
- `ref` over a generator is a compile error, since its items are made one at a time and aren't anywhere to change. Built.
- `case .Button(ref clicks):` changes a field inside the enum a `match` statement is over, as `for ref` changes an item: `match ui[widget_id].kind:` with `clicks += 1` in the case. The case is put back however its body ends, a failure included. Built.
- `items()` is for keyed things: a dict, a `Table`, and your own types that give `len`, `[]` and `key(i)`. Built for dicts and `Table`s.
- Looping over a dict gives its keys, as in Python, and `keys()`, `values()` and `items()` stay. Built.
- A generator passed to a call like `sum` can read anything, but it can't change the names around it, which is a compile error. Use a loop for that. Built.
- `f(ref xs[i], ref xs[j])` is always refused, since `i` and `j` may be the same, and so is `f(ref xs[0], ref xs[2])`. `xs.swap(i, j)` swaps two items, counting from the end when a place is negative, and there's no `pair`. Built.

## Copies

```fennec
uppercased = [copy(person, name=person.name.upper()) for person in people]
```

`copy(x, field=...)` makes a changed copy and re-runs `where` rules. Built: `replace` was its old name, and `fennec fix` moves it to `copy`. A plain `=` covers everything else, and the compiler skips the real copy when nobody can tell. A plain parameter can't change, so passing something huge costs nothing.

## Sized numbers

Built: all ten run natively, apart from the few gaps the list below names.

```fennec
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
- Mixing types, like `i32 + i64`, is a compile error that says to convert one side. Widening is written too, as `i64(small)`, and can't fail. Built in the checker: the ten types, `i64` as `int` and `f64` as `float`, the mixing errors, and literals taking the size they go into, checked to fit. `/` gives a `float` between integers of any one size, as between ints, and `//` keeps the size. Built natively for `i8` to `i32` and `u8` to `u32`: arithmetic checked against each size, `~` within an unsigned size's width, and `u8(x)`, `int(x)` and `float(x)`. Built natively for `u64` and `f32` too: a `u64` is unsigned on all 64 bits, with its arithmetic checked, and an `f32` is rounded to 32 bits after each operation and prints as the shortest text that reads back as the same `f32`. Both print, format in f-strings, and convert with `u64(x)`, `f32(x)`, `int(x)` and `float(x)`. `sum` of any sized numbers gives one of their size, starting at its zero. Sets and dicts of them build too, keys and values, each shown as its type is. `**` on a `u64` is checked as its other arithmetic is. Not built yet for them: a `u64` literal above the largest `int`. `u8.from(x)?` and `u8.parse(text)?` are built for every integer size, written in Fennec in the standard library, and `u64.parse` reads what's past the largest `int` a digit at a time. `wrapping_add`, `wrapping_sub`, `wrapping_mul` and their `saturating_` twins are built for every integer size too. Not built yet: packed layout, and set, dict and tuple literals where sized numbers go (`{1, 2}` for a `set[u8]`).
- Every sized type checks overflow, and a check is removed only where the compiler proves it can't fire. `wrapping_` and `saturating_` methods are for when you mean it.
- `<<` is bit work, not arithmetic: it drops the bits shifted past the top on every integer type, with no check, so `200 << 1` on a `u8` is `144`. A count at or past the type's width stops the program, as any count outside 0 to 63 does on `int`. Built, on every integer type, with the count check.
- Floats give the same bits on every platform. Arithmetic and `sqrt` are exact in IEEE 754, and `sin`, `cos`, `tan`, `acos`, `exp`, `log` and `**` are computed by one library, `libm`, on arm64, x86-64, Windows and WebAssembly alike, so replays and lockstep multiplayer agree. Each platform's own C library rounds some of these differently in the last bit. Built.
- Floats have one total order: NaN equals itself and sorts last, and `<` agrees with `sorted`. Built: `==`, hashing, sorting and `<` all follow it. A "never NaN" analysis is planned, to recover the cost where a value can't be NaN.
- `std.math`'s `Vec2`, `Vec3`, `Vec4`, `Quat`, `Mat3` and `Mat4` are `f32`, with `DVec3` and the other `D` types for doubles. They're concrete types, not generic ones, and a generic version with an `f32` default could come later without breaking anything.

## Fixed-size arrays

```fennec
struct Mat4:
    m: array[float, 16]                # 16 floats, held inline

struct Bag:
    slots = array[Item?, 20]()         # every slot starts as None

identity: array[float, 4] = [1.0, 0.0, 0.0, 1.0]
```

`array[T, N]` holds exactly `N` items where the array is, with no block of its own. Write one as a list of exactly `N` items where an array goes, or as `array[T, N]()`, which fills it with the item's zero: `0`, `0.0`, `False`, `""`, `None`, or a struct made from its defaults. It indexes, loops with `for` and `for ref`, and has `len`, `in` and `==`. A plain `=` copies it. Its size never changes, and it doesn't go where a list goes: `list(arr)` makes one. A wrong count, and a position known to be outside it, are compile errors.

## Pointing at things: tables and Ids

```fennec
struct Unit:
    health: int
    target_id: Id[Unit]?

mut units = Table[Unit]()
unit_id = units.add(Unit(health=50))

target = units.get(enemy.target_id) else return    # may be gone
units[unit_id].health -= 1                         # sure: stops loudly if wrong
```

`units.update(unit_id, value)` puts a new value at a live `Id` and gives `True`, or gives `False` if it's gone. Built.

Tables are for large, flat data. An `Id` is an index, a generation and a tag for its table, so it finds the right thing or nothing.

A copy of a `Table` keeps its tag, so an `Id` from before the copy works in both, and generations are unique across copies, so an item added to one copy can't be found through the other. Not built yet: today an item added to each copy can get the same `Id`.

An `Id` is local to its process's `Table`. To name something across servers, send a key of your own, like a player's name, and let each server find its own `Id` from it. Built: an `Id` from another `Table` finds nothing.

## Sharing, resources and tasks

Built so far: `shared T`, `shared name = value` and `lock`, on every engine, with a crash inside `lock x as ref` rolled back; `with` sources a program writes, with `lend`; server rounds for `parallel(server)`; and `with launch`, `Channel`, `Inbox` and `Sender`, natively. A `lock` inside another `lock` is refused for now, however many calls down it's taken: take them together in one. Not built yet: pools. A `shared Name:` declaration, and a `with` on a `shared` value, are refused, and `fennec fix` rewrites what's mechanical: `struct Name:`, `shared Name` where the type is written, `shared x = Name()` and `lock`. A `def del` is refused.

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

- `shared T` is a type, so it goes in fields and collections too: `dict[str, shared Room]`. It works on any type, and there's no `shared Name:` declaration. Built.
- A `shared` value is made in a function, usually `main`, and passed to whoever needs it, like a capability. There's no top-level shared state, so a function's signature says what shared state it touches.
- Making one from a plain value is written out, as `shared scores = Scores()`. A plain value never becomes `shared` at a call. Built.
- It's reached only inside `lock x as s:`, to read, or `lock x as ref s:`, to change. Any way out of the block releases it, `?`, `fail` and `return` included. Leaving with `return`, `break` or `continue` keeps the changes made so far, and a crash undoes them, below. Built. A `fail` or `?` leaving `lock ... as ref` undoes them too. Built.
- **No waiting inside a lock:** no channel sends or receives, pool takes, network calls, `clock.sleep`, `parallel`, a `lend`, or the end of a `with launch`. Printing and local files are fine. Wait first, then lock. It's a compile error, traced across the whole program through calls, methods and interfaces, however many calls down the wait is. Built. A call through a function value is followed too: a function parameter called inside a lock, or handed on to one that is, is checked against what each caller passes for it, an arrow function by its body and a `def` by what it calls, and a function value whose source can't be known, like one taken from a list, is refused where it's called. Built.
- **Deadlocks can't compile.** Lock order is checked across the whole program, by where each `shared` value is made. Two values the compiler can't tell apart, like two accounts from one dict, are locked together in one statement: `lock accounts[from_id], accounts[to_id] as ref source, ref target:`. Locking the same value twice that way fails, rather than stopping the program.
- Interfaces and function values are traced across the whole program, which is always compiled together, so nothing needs a `locks` annotation. An error names the implementation that takes the lock.
- A `shared` value holds plain values and other `shared` values, never something borrowed from a `with`. It never holds one of its own type, and never forms a cycle, so a tree uses a `Table` and `Id`s. It can't store a function that captures a `shared` value.
- It's shown as what it holds, read under its lock: printed, in an f-string, by `str`, and in a failed `assert`, as `Account(total=1)`. Decided 2026-11-14. Built for printing, f-strings and `str`; a failed `assert` still shows lowering's own declaration until the backend calls its text there.
- `==` compares what two `shared` values hold, as every other value compares, matching the oracle: a snapshot taken under both locks, which may be stale once they're released. Both are taken in a fixed order, by cell, so `a == b` on one thread and `b == a` on another can't wait for each other, and `a == a` takes one. Decided 2026-11-14. The oracle does it; native code still compares which cell they are until the backend's change.
- Inside a lock body nothing shows or compares a `shared` value, or a value holding one, the locked one included: it would wait for a lock its own thread holds. Use the name the lock gives, or do it after the block. Built.
- Its memory is freed when the last holder lets go, with no cleanup code.

### `with` and `lend`

```fennec
with fs.open("save.dat")? as file, net.connect(host)? as conn:
    data = file.read_text()?
    conn.write_line(data)?
# both are closed here, however the block ends
```

The end of a `with` block is where what it lent finishes: a file closes and a transaction commits. A failure while finishing is the `with` statement's failure, covered by the `?` on its line, so `with fs.create(path)? as file:` covers both opening and finishing. Built, for sources a program writes, as in `examples/sources.fen`, and for files, as in `examples/files.fen`: a file written goes in beside its path, synced, and takes the path's place when the block ends, and a block that fails leaves the old file. `fs.create(path, durable=False)` skips the sync. In a task, a file's disk waits pause only that task. Built for SQLite's transactions too, in `std.sqlite`: `with db.transaction()? as tx:` commits at the block's end and rolls back if it fails.

- `with` lends you something for the length of a block, then takes it back. It's the only way to open files, sockets, servers and transactions. Built for sources a program writes, below, and for files; sockets move to them next.
- What it lends can't escape the block: it can't be returned, stored outside it or put in a `shared`. A second name for it inside the block is fine.
- A struct holding something borrowed is borrowed too, so a wrapper like `LineProtocol(conn=conn)` works inside the block and can't leak out.
- Several handles in one `with` open left to right and finish innermost first, as one `with` inside another. If one fails to open, the ones already open run their `except:` cleanup. Built.
- `with conn.split() as reader, writer:` gives a connection's read and write sides, for one task reading while another writes. Several names split what a source lends as a tuple, `-> with (Reader, Writer)`. Built for sources; connections move to them with sockets.
- `with` never locks.

Writing your own takes `-> with T` and one `lend`, outside any loop. Setup goes above it and cleanup below. Built:

```fennec
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

A file or stream fails with an `IoError`, by case: `NotFound` when nothing is at the path, `PermissionDenied` when the system or an `Fs` narrowed with `only` won't allow it, `AlreadyExists` when something is at the path already, and `Other` for the rest. Each carries the `reason` in words, and `message()` gives it. Built, on the real file system and a test's fake one. `fs.read_text_if_present(path)` and `read_bytes_if_present` give `None` when nothing is there, and fail for anything else, so a file that may not be made yet is `text = fs.read_text_if_present(path)? else ""`. Built.

`fs.create` syncs the file to disk before putting it in place, and `file.sync()?` syncs at any other point. Built. Closing a writer flushes it, and a failed flush fails the `with` block. Built. `std.io` has a `Writer` (`write`, `write_text`, `write_line`, `flush`) and a buffered `Reader` (`read_chunk`, `read_line`, `lines`), each failing with `IoError`. Files are both, a connection is both through `conn.writer()` and `conn.reader()`, which keep the network's words in a note, and `MemoryStream` is both in memory. `json`, `csv` and `pack` have `dump(value, ref out)` and `load(ref src)` beside `dumps` and `loads`. Built. `stderr` is a `Writer` as it is, and standard output is one through `stdout.writer()`. Standard input is a `Reader` through `stdin.reader()`. Built.

- Code after `lend` runs when the caller's block succeeds, and it may fail. That failure becomes the `with` statement's failure. `return`, `break` and `continue` in the caller count as success, so an early `return` still runs the cleanup, and a failing cleanup makes the function fail. Built.
- `lend x except:` runs when the caller's block fails or crashes, and the block's own failure is the one that passes up. It's the one bare `except`, since cleanup doesn't handle the failure: the failure carries on up by itself, so there's nothing to name and nothing to lose. It's also the one `except` that runs for a bug, a crash, where everywhere else `except` means "failed". If the cleanup in `except:` fails too, its failure is attached to the block's as a note. With several handles, every one runs its cleanup, innermost first. Built: `lend x else:`, the form before 2026-11-11, is refused with the fix that writes `except`. Running the cleanup for a failed block is built, and so are several handles and the note: a cleanup that fails is added to the block's failure as a note, and the rest of the cleanup is skipped. Running it on a crash is built natively: the crash waits while the cleanups run, innermost first, and a cleanup that crashes too is said, with the first crash the one that goes on. A cleanup's `x = f()?` is a note too, and the rest of the cleanup is skipped. A `?` deeper in a line still replaces the block's failure.
- There's nothing for the caller to finish by hand, like a `commit()` or a `close()`, and nothing to forget.
- An interface can have a source, as `def transaction(self) -> with Transaction fails SqlError`. A generic function using it, `def saved[D: Database](db: D)`, runs the source of the type it's called with. Built for functions; a method of a generic type using one isn't yet.
- A source can't `lend` while it holds a lock it took. Opening a file inside a lock is fine.
- `for conn in parallel(server):` lends each connection to its own round's task, closed when that round ends. It's a plain loop, not `for ref`, and a connection's methods take `self`. A generator can't `lend` its items: there's no `Iterator[with T]`.
- Tasks started inside a `with` may borrow what it lent, since they always end first. From many tasks at once, only its read-only methods, like a server's `accept`.

### `launch` and channels

```fennec
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
- The `?` on its line covers the task: `with launch(...)` fails with the task's errors and `Crashed`, from `std.concurrent`, where the block ends. `inbox_size=` is 16 unless given. Checked, and written in around `std.intrinsics`' `launch` and `launch_end`. Built natively: a task runs on the runtime's threads, with what it captured copied for it.
- Channels are always bounded, and `send` waits when one is full. Both ends are plain values, so there can be many senders and receivers. `send(message)` fails with `Closed`, and `send_within(message, d)` with `Closed` or `Timeout`. `receive()` gives `T?`: the next message, or `None` once the channel is closed and empty. `receive_within(d)` gives `T? fails Timeout`. Built, on `Channel`, `Inbox` and `Sender`. A channel you set up yourself opens with a plain function, `with channel[Job](size=64) as jobs:`, as Python's `open()` is, not by calling the type. The runtime's half is built. `std.concurrent` has `Channel[T]`, its ends `Inbox[T]` and `Sender[T]`, and the errors `Closed`, `Timeout` and `Crashed`, written in Fennec over channel intrinsics. `channel[T](size)` is the only way to make one, since `Channel`'s constructor is private. It builds natively, and so do `send`, `receive` and `for job in jobs`, each message copied across as it's sent.
- Not built yet: a channel whose message is a bare `str?`, as `channel[str?]`. The build says so and stops. A `str?` inside a tuple, a struct or an enum is sent as any field is.
- A channel delivers each message to one receiver by default, as a work queue. `to=.Every` gives every listener every message, as in `with channel[Hit](size=1024, to=.Every) as hits:`, and receiving reads the same either way, `for hit in hits`. With `.Every`, `send` never waits, and a listener that falls behind fails loudly, unless it asked for `when_behind=.DropOldest`. A listener asks with `for hit in hits.listen(when_behind=.DropOldest)`. There's no separate events type. Built natively.
- Dev builds stop with a report when tasks are waiting only on each other, since a hang through channels alone can't be ruled out at compile time.

### Pools

```fennec
with postgres.connect(net, entropy, "postgres://app@db.internal/shop?pool_size=20")? as db:
    for request in parallel(server):
        with db.transaction()? as tx:
            tx.execute(t"insert into orders (item) values ({request.item})")?
```

- `postgres.connect` always lends a pool. `pool_size=10` and `wait=30s` unless given, as an argument or in the URL, and giving one in both is an error.
- `with db.transaction()? as tx:` takes a connection for a transaction, and `with db.borrow()? as conn:` takes one for a block of statements. When every connection is in use, the block waits up to `wait` for one.
- Each connection lives inside one keeper task's `with`, and the keeper lends it to one block at a time, so nothing can leak or be used twice. The block uses the connection itself: nothing is sent to the keeper to run.
- A block that fails or crashes has its connection reset before the next block gets it, and replaced if the keeper can't prove it clean, so the pool heals itself and never shrinks.
- Pools are written in plain Fennec in `std.concurrent`. Not built yet: `postgres.connect` makes one connection today.

### When tasks stop or crash

- When a `with launch` block ends normally, the task's inbox closes and the task drains it. If it's still running after `stop_after`, every wait it's in is cut short, socket and stdin reads included. A wait cut short by a stop ends the task quietly, with no failure reported, since the stop was asked for. Not built yet. A block that failed cuts them short at once.
- A task's failure comes out where its `with launch` block ends, and a crash comes out as `Crashed`. A crash ends only its own task. The HTTP server answers that request with a 500.
- **A crash inside `lock x as ref s:` rolls back.** The runtime puts back every value the block changed, in reverse order, and then releases the lock, so nobody ever sees a half-change. A `fail` or `?` leaving `lock ... as ref` does too, so a write lock either finishes or has no effect, as a database transaction does, while `return`, `break` and `continue` keep the changes. That covers one `lock` statement and all the values locked together in it, and not local variables or output. To keep a change and still fail, decide inside the lock and `fail` after it. Built in native code. There's nothing to write and no new failure type, and a lock is never left poisoned.
- The compiler saves the old value before each write in such a block, and skips the saves where it proves the block can't crash, so most lock bodies cost nothing extra. Values taken out are freed when the block ends.
- Sending to a closed channel fails cleanly, and never waits.

Not in the language: `del`, destructors, moves, lifetimes, `unsafe` and global state. Left for when real programs need them: `select`, handles that move between tasks, and results returned from tasks.

## Unions

```fennec
type Shape = Circle | Square

def report(shapes: list[Shape], stdout: Stdout):
    stdout.print(sum(shape.area() for shape in shapes))   # every member has area(), so Shape does
    for shape in shapes:
        if shape is Circle:
            stdout.print(shape.radius)                     # a Circle here
```

- A union mixes types that already exist, and an `enum` defines new cases. Each value is held in place with its member's tag, never boxed. Built.
- `if x is Circle:` narrows `x` inside the block, and `match` handles each member, checked for exhaustiveness: a case naming a member, as `case Circle(radius=_):`, narrows the subject to it inside the case. After `if x is Circle: return`, `x` is what's left, the other members, and can be given where a union of just those goes. A method every member has is the union's too, and runs the one of the type the value holds. A `with` source every member has is the union's too, and runs the source of the type the value holds: the `with` is built once for each member. Called on a field holding the union, which `is` doesn't narrow, it's a compile error that says to name it first. Built.
- `None` is never a member: `str | None` is an error whose fix writes `str?`, and an optional union is `(A | B)?`.
- Failures stay separate: a function that can fail returns `-> T fails E`, never `-> T | E`.
- A function that can fail says `fails` in its signature, so a reader sees how it can fail there. One that leaves it out is a compile error, and `fennec fix` writes it, as `-> str fails TooBig`, `fails IoError` for one that gives back nothing, or `fails ParseError | RangeError`. `main` doesn't say it, since nothing calls `main`. Built.
- A failure is always an `error` type, and `fail` takes one. Built. There's no `Ok`, `Err` or `Result`: a failure is handled at the call, under Failures. Built: writing one is a compile error that says what to write instead, and `fennec fix` rewrites most of them.

## Named arguments

```fennec
def scan(rows: list[Row], limit=8) -> list[Row]:
    ...

scan(rows, limit=100)
copy_file(source, destination=backup, overwrite=True)
```

- A parameter with a default is passed by name only, and a literal default gives it its type. An empty collection's default is its constructor: `items = list[str]()`. Built.
- Some positional arguments are a compile error whose fix names them: a bare `True` or `False`, neighboring arguments of the same type, and three or more unless no two neighbors could be swapped and still compile. The first argument and `self` are exempt. Built.

## Operators

```fennec
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

- A type may define each operator once for each right-hand type, so `Mat4 * Mat4` and `Mat4 * Vec4` can both exist. Built.
- Types must match exactly, with no conversions, so there's never a best match to choose. `now + now` is a compile error that lists the `+` that `Time` has.
- It's for operators only. A named function still has one meaning, so `load(path)` is always the same `load`.
- A user type gets this exactly as `std`'s types do.
- Units of measure use it, with no new syntax: `Meters` is a struct holding a `float`, and its operators say that `Meters / Seconds` gives `Speed` and `Meters / float` gives `Meters`. A unit with one operator of each kind is built today, and the second waits for the rule above.

## Failures

There are three outcomes. A missing value is `T?`, handled with `else`. A failure is `fails E`, passed up with `?` or handled with `except`. A bug stops the program. Chris locked this on 2026-11-11. Built, with the refusals below: `else` on a call that can fail and `else as why:`, the forms before it, are compile errors whose fix writes `except`.

```fennec
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

- A failing call is handled at the call. There's no `Ok`, `Err` or `Result`. Built.
- `except` covers only the call it follows. It comes after a whole right side, `x = f() except ...`, a bare call, `f() except ...`, or `return f() except ...`, and never inside a larger expression.
- **One case on the line**, `except InvalidValue(): 8080`, gives a value or leaves, as `except .Gone(): return` does. It must cover everything the call fails with, and an or-pattern can, as `except Closed() | Timeout(): 0`. When it leaves a case out, it's a compile error that names it and shows `except why:` with arms.
- **Any failure**, `x = f() except why:`, names it. The block gives `x` a value, or leaves with `return`, `fail`, `break` or `continue`, each checked against the function's signature. On `_ = f() except why:` the block needn't give a value.
- **`why` is used on every way out.** It's used when it's read into something that goes somewhere: an argument, an f-string, a field read, or `fail why`. Being the subject of a `match` or an `is` doesn't count, and neither does `_ = why`. A `fail` of a new error inside the block carries `why` along as a note, so it counts. Arms that name every case needn't read it.
- **The name after `except` is lowercase.** `except Timeout:` reads as Python's "catch `Timeout`", but here it would name any failure, so a capitalized name there is a compile error: "write `except Timeout():` to handle only `Timeout`, or a lowercase name to handle any failure". A second `except` line under the first is refused too, and points at `case` arms.
- **Several cases** are `case` arms directly under `except why:`, with no `match` inside. The arms cover every case. An arm wider than a named case, as `case _:` is, must use `why` or `fail`. `case other:` is refused there, since `why` already names the failure. An `except` block is all arms or all statements, never both.
- **On an error with cases, the type-wide pattern is refused under `except`.** `except IoError(): ""` would drop every disk failure by naming the type, so it's a compile error that says to name the cases or write `except why:`. An error with no cases, as `InvalidValue()`, keeps it.
- **Nothing is dropped without a word.** There's no bare `except:` after a call, no `except _:` and no `as _`. `_ = f()` on a call that can fail is a compile error, and `failure_of` is only for tests. Built for `_ =` and `failure_of`.
- **`else` means "otherwise"**: `if`/`else`, `a if c else b`, and a missing value's default. After `else` comes a value or a statement that leaves: `else return`, `else fail E`, `else break` or `else continue`. `else` on a call that can fail is a compile error that shows `except`, and `except` on a plain optional is a compile error that shows `else`.
- **On `T? fails E`**, `?` or `except` takes the failure off, and then `else` takes the `None` off: `user = db.find_user(name)? else return "Nobody here"`. An `else` for the call's own result on the same line as its `except` is refused, with the two-line form shown. An `else` inside the handler's value, as in `except InvalidValue(): 80 if secure else 8080`, is the conditional `else` and is fine.

```fennec
found = db.find_user(name) except why:
    log.error(f"couldn't look up {name}: {why}")
    return "Sorry, try again in a minute."
user = found else return f"Nobody here is called {name}."
```

- **A call that can fail gets its own line.** It's the whole right side, a bare call, or `return`'s value, unless it has `?` or `.expect("reason")`, which nest anywhere: `total = parse(a)? + parse(b)?`. Anywhere else, as an argument or an operand, it's a compile error that shows the split onto two lines.
- **A `with` or `for` line takes only `?`.** To handle a failed open or commit in place, put the `with` in a function of its own and handle that call.
- `f().expect("reason")` crashes on purpose, with that reason, for a call that can't fail here. It's not a way to handle a failure: it says there isn't one. The reason has to say something: `.expect("")` is a compile error, "say why this can't fail". Built.
- `try` isn't a keyword. It's reserved only so `try:` can be a compile error that shows `except` after the call.
- `T fails E` is made only by the runtime, as for the results of parallel work, and read only through `?` and `except`. What can fail is never a plain value: holding it in a name, printing it, comparing it, putting it in a collection or handing it on is a compile error, and `fennec fix` adds `?` where the function can pass the failure up. `.expect("reason")` and `assert ... fails` read it too, and an arrow function may pass it up with `?`, as `failure_of(() => load(path)?)` takes it. Built.
- There's no `exit()`. `fail` out of `main` prints the failure to stderr and exits with code 1, which is built. `main` may also return an `int` as the exit code, which is built too. Nothing else can end the process.
- A failure passes up only with `?`, so every place one can is marked: `return load(path)` in a function that fails is a compile error, and `fennec fix` adds the `?`. An arrow function is held to it too: `failure_of(() => load(path)?)`, whose `?` passes the failure up as the arrow's own, not the function's around it. An arrow that can fail is refused where a builtin takes a function, as `sorted`'s `key=`, which nothing there expects to fail. Built.
- `?` means the same everywhere, `parallel` included: a failure in one round passes up out of the loop once the other rounds stop. Built. To let one piece fail alone, handle it in the body with `except why:`. `break` stops new rounds from starting and lets the running ones finish, and a failure passing up cuts them short, like a failed `launch`. Each connection a server lends closes when its round ends, however it ends. Built. `server.serve(handler)` turns a handler's failure into a 500, in plain Fennec. Not built yet: today's handler can't fail.
- Ignoring a returned value is an error, with `_ = call()` to discard one. Built.
- `.Case` is required when the type is known, in values and in `case` patterns; `fennec fix` writes it. Built.
- When a union's types share a case name, as `IoError | NetError` both have `NotFound` and `Other`, `.NotFound` is a compile error that names both types, and the fix writes `IoError.NotFound()`. Not built yet.
- An error carries notes, a `list[str]`, as Python's exceptions do: `failure.add_note(text)` adds one, and `failure.notes()` reads them. When a block fails and its `lend x except:` cleanup fails too, the cleanup's failure becomes a note on the block's, so neither is lost, and `main` prints each note under its failure. Built in the checker and natively, where an error's notes are a hidden word of its own, 0 until one is added.
- `fails A | B` combines failures from different places: `?` passes any of them up as it is, and a function type can say it too, as in `(str) -> int fails ParseError | RangeError`. Built.
- A test checks a failure with `assert load(text) fails Oops(line=3)`. The right side is a `case` pattern of what it fails with, `Oops()` matching any `Oops`. A failed one says what really happened: "it gave Config(...)" or "it failed with Oops(line=4)". It's only for `assert`: `fails` in an `if` is a compile error that points at `except why:`. Built. `failure_of(() => load(text)?)` gives a test the failure itself, or `None`, to look at more closely. It's only for tests: anywhere else, a helper a test calls included, it's a compile error that points at `except why:`, which names a failure where it happens. Built.
- A one-line `x if condition else y` and a missing value's `else` can't share an expression without parentheses, since `ports.get(name) else 80 if given else 8080` reads two ways: write `ports.get(name) else (80 if given else 8080)`, or give one of them its own line. `fennec fix` writes the parentheses the way it's read. Built.
- Taking from an empty collection gives `None`: `list.pop()` returns `T?`, as a queue's `pop` does, so draining one reads `next = todo.pop() else break`. `pop(at)` stops the program on a position that isn't there, like `items[at]`. A dict's `pop(key)` stops the program on a key that isn't there, like `ages[key]`, and `remove(key)` gives `V?` instead: the value, or `None`, so `if ages.remove(name) is None:` asks whether it was there. `items.get(at)` gives `T?` for a position that may not be there, counting from the end when it's negative as `items[at]` does. Built.
- `std`'s errors have cases wherever callers would treat them differently. `IoError` has `NotFound`, `PermissionDenied`, `AlreadyExists` and `Other`, built. When `std` finds itself comparing an `Other`'s text, it adds a case.
- Text is read as a number with `int.parse(text)` and `float.parse(text)`, which fail with `InvalidValue`. A conversion `T(x)` never `fails`: `int("4")` and `float(text)` are compile errors whose fix writes `.parse`, as `f32("1.5")` already is. A conversion that doesn't fit is still a bug that stops, as `u8(300)` does. `std.math.NAN` is the one NaN, and `std.math.INFINITY` infinity, for where one was made from text. Built: `int.parse`, `float.parse`, `NAN` and `INFINITY`, with `float.parse` reading `"nan"` and `"inf"` as Python's and Rust's do. Not built yet: refusing `int(text)` and `float(text)`, which still read text as they did, until the migration.

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

- Every `def` that gives back a value says `return`. A bare value at the end of a `def` is a compile error, "a `def` gives its value with `return`", and `fennec fix` adds it. `return match x:` gives a `match`'s value, as `return f() except why:` does. Chris locked this on 2026-11-11. Built.
- A block after `x =`, as `x = match`, `x = lock` and `x = f() except why:` are, ends with its value. A `return` in it leaves the function, so this compiles and skips the line after it whenever the types match:

```fennec
def load_settings(fs: Fs, log: Log, clock: Clock) -> Settings:
    mut settings = load_saved(fs) except why:
        log.warn(f"settings.json is damaged: {why}")
        # meant: use the defaults and carry on, but this leaves the function
        return Settings()
    settings.opened_at = clock.now()
    return settings
```

  Write the block's value as its last line, `Settings()`. `fennec fix` never adds a `return` inside such a block.
- `len` is a builtin, like `sum` and `sorted`: `len(items)`, as in Python. `len(x)` calls a type's `def len(self) -> int`, as `str(x)` calls `def str` and `x in xs` calls `def in`. `x.len()` and `x.abs()` on a value are compile errors whose fix writes `len(x)` and `abs(x)`. Built.
- A value is shown by the name its type is written with, never with type arguments: a generic struct prints as `Holder(item=3)`, not `Holder[int](item=3)`, and a generic enum's case as `Choice.Picked(value=7)`, in `print`, an f-string, `str()` and a failed `assert`. Built.
- `abs` is a builtin the same way: `abs(x)` for the numeric types, and for any type with `def abs(self) -> Self`, which `abs(x)` calls. `x.abs()` gets a `fennec fix` suggestion that writes `abs(x)`. Chris decided this on 2026-11-11. `abs(x)` is built for `int`, `float` and types with `def abs`; the sized numbers and the suggestion aren't yet.
- These names are reserved, and naming a local, a function, a type or a constant after one is a compile error that suggests another name: the builtins `len`, `abs`, `sum`, `sorted`, `min`, `max`, `range`, `enumerate` and `zip`, and the built-in types `int`, `float`, `bool`, `str`, `bytes`, `list`, `dict`, `set`, `array` and the ten sized numbers. That's the whole list: a builtin added later comes with a `fennec fix` rename. Fields, methods and named arguments may still use them, as they may use keywords. Built.
- `case` is reserved too, since it starts an arm under `except why:`. Built. std's serializer method that was `case` is `enum_case` already.
- There's no cap on how many keywords Fennec has.

## Visibility

```fennec
export struct Score:
    points = 0
    _best = 0                  # seen only by Score's own methods

export def best(scores: list[Score]) -> int:
    return max(score.points for score in scores)
```

- `export` marks what other kits can use, and goes on top-level types and functions only. Without it, a name is private to its kit, and inside a kit every file sees everything. `fennec fix` rewrites the old `pub`. Built.
- A `_name` field or method of a `struct` or `shared` is hidden from everything but its type's own methods. Tests get no special access. Built.
- A top-level name never starts with `_`, since it's already private to its kit. That's a compile error, and `fennec fix` renames it everywhere. The standard library follows the same rule. Built.
- Other kits match an exported type's fields by name: `case Score(points=p)`, `case Point(x=a, y=_)`, or `case Score()` to match it without reading a field. Matching another kit's type by position is a compile error, and `fennec fix` adds the names. A kit's own code can still match by position, and cases with no fields, like `.Wait`, are unchanged. Built for the standard library's types.
- Fields with defaults come after the ones without, in a `struct`, an `error` and an enum case, as parameters with defaults do. A field without one after a field with one is a compile error. Built.
- From outside a type, its automatic constructor sets the public fields, and values given by position go to those. `_` fields take their defaults. A `_` field with no default needs a named constructor, a method without `self` that builds one, and a type nobody can build is a compile error. Built.
- So a kit can add a field with a default to a type it exports, and no other kit's code that builds or matches it breaks.

## Saving data

```fennec
from std import json

text = json.dumps(world)
back = json.loads[World](text)?
```

- JSON, TOML and `std.pack` save any type whose fields can all be saved, through one `serialize` and `deserialize`. Built.
- A `Table` saves as a list of its items in order, and an `Id` as where its item is in that list. Loading makes a new `Table`, and every `Id` loaded with it finds its item there. An `Id` has to be in a `Table` saved with it. Built.
- A union saves tagged with its member's type name, as in `{"Circle": {"radius": 1.0}}`. Built.
- A JSON `null` is a value, and a field left out is missing. A default fills only a field left out. So a field that isn't optional fails on `null`, as in "`x` should be an int, but it's null", default or not. An optional field holds `None` for `null` even when its default is something else, so `dumps` then `loads` gives back what was saved. A field left out takes its default, or `None` if it's optional, and otherwise fails as "`x` is missing". SQL's `NULL` reads the same way. Built.
- A `T??`, an optional holding an optional, can't be saved as JSON, since `null` can't tell `None` from an optional holding `None`: that's a compile error. Not built yet.
- JSON has no NaN or infinity, so `json.dumps` of a value holding one stops the program, saying where it is, as in "`[1].values[1]` is nan, which JSON can't write". It's a bug in the program, as a NaN reaching `int()` is, so `dumps` itself can't fail. Built.
- `@skip` and `@serialize(name="uid")` on a field say how it's saved. Built. A kit may define annotations of its own for fields, which generic code reads through `fields(T)`. A generic function's `for f in fields(T):` is built, checked once, with each field's name and value, and annotations a kit defines and reading them aren't built yet. A function takes only `@hot`, so routes are registered in plain code, as in `router.get("/todos/{id}", show_todo)`.
- A `Secret[str]`, from `std.crypto`, can't be saved, printed or logged, and its text comes out only through `expose_secret()`, whose name says so. Printing one, an f-string or `str()` of one, and saving one in any format are compile errors, and so is doing any of those to something that holds one. Printed through a generic function's `T`, in a failed `assert` or in the failure that ends `main`, it shows `Secret(hidden)`. `==` on two secrets compares them in constant time, so its timing never says where they differ, and a secret of anything but text or bytes can't be compared. Built. `Secret.of(text)` makes one, and `Secret[bytes]` holds a key. A server's `Certificate`, from `std.net`, holds its private key as one, beside its chain of certificates.

Loading is safe on untrusted input:

- A load always makes fresh values, never a link to anything already in the program. Built.
- Every `Id` resolves within its own document, and each `Table` carries a tag that's never reused, so an `Id` from one `Table` finds nothing in another of the same type. Built.
- `where` rules run again on load, so a value that breaks one fails the load. Built.
- Unknown fields are refused unless `extra=` says otherwise: `json.loads[Settings](text, extra=.Ignore)`. Built for JSON. TOML always refuses them, and doesn't take `extra=` yet. `std.pack` loads data from an earlier version of a type by its fields' names: a field the data has and the type doesn't is passed over, a field the type has and the data doesn't takes its default, and one with no default fails the load, naming it. Cases and union members are found by name too, and an `int` field that's a `float` now reads as one. Built. Each value from `dumps` carries its type's layout, its field and case names. A `pack.Packer` writes many values with each layout once, and `pack.Unpacker` reads them back, from a file or fed from a stream. `fennec update` locks a kit's layouts in `pack.lock`, and a locked type's single values leave the layout out, as short as before layouts. Built.
- JSON and TOML refuse what they'd have to guess at: a number too big for a float, a struct's field given twice, a dict's key given twice, and a dict's int key written any way but its plain digits, or not in quotes. A key given twice fails, naming it, as in "the text has `a` twice", never letting the last copy win, since two readers that disagree on which copy wins let a value past a check. `std.pack` refuses it too. Built.
- Untrusted input has depth and size limits. `std.pack` limits nesting, each string's length, the total it reads and how many items that take no bytes it makes, so bytes can't ask for far more memory than they are. Built. JSON and TOML limit nesting to 500 deep, and don't limit size, string length or collection length yet.

## Templates

```fennec
# the literal pieces and the values stay apart
rows = db.query(t"select name from players where team = {team} and score > {least}")?
page = html.render(t"<p>Hello, {visitor.name}</p>")?
result = process.run(t"git log --author={author}")?
```

- `std.sqlite` takes them: `db.execute(t"...")` and `db.query[T](t"...")`, with each value bound as a parameter and each row read into a `T` by its columns' names. Built, natively. In a test, each path is a real WAL database file of the test's own, so a test reads as a program does.
- A SQLite `Database` reaches only its own file, as what it was opened through allows. It takes only what ordinary queries need: reading, inserting, updating and deleting rows, transactions and savepoints, functions, creating, changing and dropping tables, indexes, views and triggers in its own database, and temporary ones of the connection's own. Anything else fails with `SqlError`, as `ATTACH` of a file, `DETACH`, `VACUUM INTO`, `ANALYZE` and any pragma but the two `std.sqlite` runs itself do. SQLite's defensive mode is on, with the schema not trusted. A plain `VACUUM` still works. Built.
- `std.postgres` speaks Postgres's protocol in plain Fennec: its messages, signing in with SCRAM-SHA-256, and a query as the extended protocol's messages, its template's values bound apart and its rows read by column type into `sql.rows_as[T]`. `with postgres.connect(net, entropy, "postgres://app@db.internal/shop", password=secret)? as db:` lends a `PgPool`, a pool of connections through `std.net`, 10 unless `pool_size=` says. `with db.transaction()? as tx:` and `with db.borrow()? as conn:` each lend one connection to a block, with `execute` and `query[T]` as `std.sqlite` has, and wait up to `wait=`, `30s` unless it's set, then fail with `Timeout`. `pool_size` and `wait` can be in the URL too, as `?pool_size=20&wait=5s`, but not in both. A URL with a password in it has to be a `Secret`, as `env.secret("DATABASE_URL")?` reads one. A plain `str` URL with a password is refused, at compile time when it's written in the source, and otherwise the password goes in `password=`, a `Secret`. A failure shows the URL with its password as `****`. Built, and tested against a real server. Its connection is TLS unless `tls=False` says otherwise, and a server that won't take TLS is refused rather than spoken to in the clear. It lives in `std` until first-party kits have a home. Its connections are TLS unless `tls=False` says otherwise, and a server that won't take TLS is refused rather than spoken to in the clear. Planned: TLS picked by host, in the clear for a loopback host and verified TLS for any other, with the URL's `sslmode=` taking `disable`, `require`, `verify-ca` or `verify-full`, and refusing `prefer` and `allow`. It lives in `std` until first-party kits have a home.
- `t"..."` makes a `std` `Template`, as Python 3.14's t-strings do. It holds the literal pieces and the values separately, and the values' type comes from where it goes, as in `Template[SqlValue]`. A `Template[T]` has `strings`, one more than its `values`, plus each value's `expressions` as written and its `format_specs`, and `interpolations()` gives each value with both.
- `html.render(t"...")` gives an `Html`, each value put in for where it is in the page, found from the text around it. Between tags and in a quoted attribute it's escaped. Where escaping isn't enough, inside a `<script>` or `<style>`, an `on...`, `style` or `srcdoc` attribute, an attribute without quotes, a name or a comment, the page fails with `HtmlError.Misplaced`, naming the value. So does a value in a link's attribute, like `href` or `src`, that would make the link anything but `http`, `https`, `mailto` or relative, like `javascript:`: nothing is put in its place silently, and a program that wants a fallback checks the URL first. An `Html` goes into another as it is, and `html.join` puts pieces one after another, so pages are built from pieces. An `Html` is never loaded from saved data. Built, natively.
- `process.run(t"git log --author={author}")` runs what the template says, its words split at the spaces in the text written in the source. A value stays part of one argument whatever it holds, so it can't add an argument or start a command. A value that starts an argument with `-` fails with `ProcessError.LooksLikeAnOption`, naming the value, since the program would read it as an option, like git's `--upload-pack=`. A value later in an argument, like `--depth={depth}`, is fine, and so is one after an argument the template writes as `--`. Built, beside `process.run("git", ["log"])`.
- A program `process.run` starts is stopped if it's still running after `timeout=`, 10 minutes unless given, or writes more than `most_output=` bytes, 16 MB unless given, to its standard output or error. Each fails with a `ProcessError` case of its own, `TimedOut` or `TooMuchOutput`. It's waited on off the task's worker, so a program that hangs holds up nothing else. On Unix it runs in a process group of its own, and stopping it stops what it started too, as `sh -c` and `make` start programs, and the timeout covers its outputs closing. Built.
- `net.get`, `net.post` and `net.request` fail with `NetError.Other` once a response's body passes `most_body=` bytes, 64 MB unless given, however it comes: by its length, in chunks, or until the server hangs up. A length past it is refused before any of the body is read. Built.
- `compress.gunzip`, `unzlib` and `inflate` fail with an `InvalidValue` once what they give back would pass `most=` bytes, 64 MB unless given, so a small file can't fill memory. Built.
- `process.only("git")` lets a `Process` start only that program, by name. It's a guard against mistakes, not against code that means harm: the program it lets start can still be told to run another, as `git -c alias.x='!sh -c ...' x` does, and it gets this program's whole environment, so a `Process` reaches what `Env` holds. Built.
- `fs.read_only()` gives an `Fs` that reads and never changes anything: writing, making a folder, removing and creating a file fail with `IoError.PermissionDenied`, "this Fs only reads, so it can't change `notes.txt`". With `only`, either way round, it reads one folder. Built.
- `net.client()` gives a `Net` that only connects out, `net.server()` one that only listens, and `net.hosts(["api.example.com", "*.cdn.example.com", "10.0.0.5:5432"])` one that only connects out, to those hosts alone. A host is matched by its name as written in the URL or `connect`, in any case: `*.` covers the names under a domain but not the domain itself, an address matches itself, and `:port` holds it to one port. A request's redirects are held to the list too, and one elsewhere fails rather than being followed. A UDP socket, which could send anywhere, isn't opened. What's refused fails with `NetError.Denied`, saying why. Narrowing again, any of these, only takes more away. `hosts` holds where a kit may connect by name. It isn't a guard against a listed name that leads to a private address: that's a later `net.public()`. Built.
- A `str` is never a `Template`, so `db.query`, `html.render` and `process.run` can't be handed text built by joining strings, and SQL, HTML and shell injection can't be written. There are no per-library string prefixes.
- `LiteralStr` is a string that came from the source code, as Python's `typing.LiteralString` is. Only a string literal, or literals joined with `+`, makes one. It goes anywhere a `str` goes, but a `str` is never a `LiteralStr`. A `Template`'s text pieces are `LiteralStr`, so building one from text read while the program runs is a compile error, and any function may ask for one. It's for strings only.

```fennec
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

C is called only through the standard library and first-party kits, whose binding to C sits in reviewed runtime code. A program sees a safe API, like `with Window.open(...)? as window:`, and can't call C itself, so there's no `unsafe` in Fennec. Not built yet.

## Untrusted code

Fennec has no sandbox for untrusted code: don't run code you don't trust, and don't add dependencies you don't understand. Capabilities limit what any dependency can do, with no `unsafe` and no C outside std and first-party kits, but that's no licence to run code nobody has read. The Den scans, lints and reports each kit's capabilities, and never runs a kit's code. No program loads and runs code at run time.

A dependency gets only the capabilities its line in `kit.toml` grants, as `http = { ..., gets = ["Net"] }`. No `gets` means nothing, `gets = []` is an error, and no tool, fix or error help ever writes it. It's checked wherever one kit hands a value to another, and `kit.lock` records what each kit `holds`. Narrowing comes later, as `{ Fs = { only = "saves/" } }`. Locked 2026-11-12; not built yet. See the guide's Dependencies.

A program's own `main` is its grant: what `main` takes is everything it may do, and `gets` is only for dependencies. `fennec fix` never adds to a `main`. A kit's program is `src/main.fen`, and every other program, for another place it runs, a tool or a task, is a file of its own in `entries/`, run as `fennec run <name>`. `kit.lock` records what each program takes, kept up to date by every build: a `main` that takes more builds, and the lock gets what was added, said as it's built, so a widening shows in the source and in the lock. A bare name is a program in `entries/`, anything else a path; a program imports the kit's `src` modules, nothing imports a program, a program's file may hold tests, and a dependency's programs are never built or reached. A program is only a `main`, forever: no `cfg`, no feature flags, no profiles, no shell-string scripts and no install, pre or post hooks. Settings are plain values in a `Config` each `main` builds. Chris settled it on 2026-11-12; not built yet. See the guide's Several programs in one kit.

## Capabilities

```fennec
def main(stdout: Stdout, fs: Fs):
    stdout.print("hello")
```

A function reaches outside itself only through what's in its signature, so there's no global `print`. The error message has to teach this, since it's the first thing newcomers hit: calling `print`, or reading `stdout` or `fs` in a function that wasn't given it, says how to give it one, or what it's called there. Built.

## A whole program

A chat server, not built yet: rooms that every connection shares, a database transaction for each message, and an autosave task.

```fennec
from std import json
from std.db import Database, DbError
from std.log import Log

struct Room:
    messages = list[str]()

struct Message:
    room: str
    author: str
    text: str

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

def serve(conn: Connection, rooms: shared dict[str, Room], db: Database) fails NetError | DbError:
    user = conn.read_line()? else return
    room_name = conn.read_line()? else return
    join(rooms, room_name, user=user)
    for line in conn.lines()?:
        say(rooms, room_name, user=user, text=line)
        with db.transaction()? as tx:
            tx.insert(Message(room=room_name, author=user, text=line))?
        # the network write waits, so it happens after the lock is released
        for message in recent(rooms, room_name):
            conn.write_line(message)?

def autosave(rooms: shared dict[str, Room], fs: Fs, clock: Clock) fails IoError:
    while True:
        clock.sleep(30s)
        # your own copy, so the lock ends before the file is written
        snapshot = lock rooms as current:
            current
        fs.write_text("rooms.json", json.dumps(snapshot))?

def main(net: Net, fs: Fs, clock: Clock, entropy: Entropy, stdout: Stdout):
    log = Log(out=stdout)
    shared rooms = dict[str, Room]()
    with postgres.connect(net, entropy, "postgres://localhost/chat")? as db:
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
- A failure in one connection is handled in its round, so it ends only that round. Without the `except why:`, it would pass up and end the server. When `main`'s block ends, the autosave task is stopped, the server and every connection close, and the pool closes its connections.
- There's no lifetime, move, destructor or `async` anywhere in it.

## Building

- `fennec run` runs the tests a change affects, then the program, built through LLVM a function at a time and cached. Where LLVM can't build something yet, it says why, as `fennec build` does, and `fennec test` fails the tests that reach it. Built: `fennec` has no interpreter. The JIT is removed, and the commit before got the tag `before-jit-interpreter-removal`. Built.
- A native program is small: `fennec build` links one runtime, and the program's syntax isn't in the image, so a hello-world is about 1.1 MB. Built.
- The launcher runs the toolchain a kit asks for, `fennec = "0.4.2"` in `kit.toml`, pinned by its signed manifest's hash in `kit.lock`. It fetches one only when it's missing, and never with `--offline`, and only `fennec update` moves a pin. Built, as `fennec-launcher` for now. No release keys are made yet, so nothing it fetches verifies today.
- A release's archives are the same bytes however often and wherever they're built, so anyone can build one again from its tag and check it against the signed manifest. CI builds each platform twice and compares them. Built.
- `fennec build` makes a native program through LLVM, and `fennec build --release` optimizes the whole program as one. Built.
- `embed_file("path")` gives a file's `bytes`, and `embed_folder("path")` a read-only map from relative path, always with `/`, to `bytes`, in sorted path order. Both are views into the program's read-only data, with no copy and nothing loaded at startup, and need no capability. A missing path, or anything above the folder holding the nearest `kit.toml` (a lone file's own folder when there's none), symlinks included, is a compile error. Hidden files are left out, and nothing is compressed. `embed_folder` gives a `std` type with `get(path) -> bytes?`, `len`, and `keys()`, `values()` and `items()` in sorted order. Built natively. Until native code holds them in the program's read-only data, each is made once, the first time it's reached. A lone file's `embed_file` can still follow a symlink out of its folder today.

## Checks

- Every check stays, unless the compiler proves it can't fail: overflow, bounds, missing keys and divide by zero. A proven check is removed in every function, not only in `@hot` ones. Built.
- `x / 0.0` stops the program, as Python's `ZeroDivisionError` does, and so does integer division by zero. Built. The compiler removes the check where it proves the divisor isn't zero.
- A broken `where` rule stops the program in code, and a load that breaks one fails cleanly. Built.
- `@hot` allows a leftover overflow check, and refuses any leftover bounds, key or integer divide check as a compile error that says why. Built. It allows a leftover float divide check too, as it does overflow. Built.
- Speed never comes from dropping a check: there's no unchecked arithmetic and no switch that turns checks off. There's no `@fast_math` either: a float result is the same bits on every machine and every build, which replays and lockstep games depend on.

## Speed

- Everyday builds stay fast, and release builds can take as long as they need.
- `sum()` over floats, `dot()`, and `std.math`'s vector and matrix reductions add in one fixed pattern, 8 running totals combined pairwise, so they're fast, the same bits on every platform, and more accurate than adding left to right. Built for `sum`, over a list or one item at a time, and for `math.dot(a, b)` over lists. `std.math`'s sums of four, as in `Vec4.dot` and `Mat4`'s products, keep glam's left-to-right order, which is as fixed, so they match glam bit for bit. `math.fma(a, b, c)` rounds `a * b + c` once, the same everywhere. Not built yet.
- A float `sum` over `parallel(...)` may round differently from the same sum in a plain loop, and from the same sum written with an `if` or a step that can fail, since each form adds in a pattern of its own. Each form gives the same bits on every run and at any thread count: `sum(x for x in parallel(items))` and `sum(x for x in parallel(items) if True)` can differ in their last digits, but neither ever changes. Built.
- `@hot` turns a hidden copy into a compile error, and the tracer shows where each copy happens and why. Built: anything that may allocate or copy in a `@hot` function, or in what it calls, is an error that says why, and so is any check native code keeps there, like an index it can't place inside its list. An overflow check the compiler can't prove away stays, as the safety net it is everywhere.

## Still to decide

Nothing right now.
