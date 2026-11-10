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
- Several handles in one `with` open left to right and finish innermost first, as one `with` inside another. If one fails to open, the ones already open run their `else:`. Built.
- `with conn.split() as reader, writer:` gives a connection's read and write sides, for one task reading while another writes. Several names split what a source lends as a tuple, `-> with (Reader, Writer)`. Built for sources; connections move to them with sockets.
- `with` never locks.

Writing your own takes `-> with T` and one `lend`, outside any loop. Setup goes above it and cleanup below. Built:

```fennec
def transaction(self) -> with Transaction fails DbError:
    with self._pool.take()? as conn:
        conn.begin()?
        # the caller's block runs here, with the transaction lent to it
        lend Transaction(_conn=conn) else:
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

`fs.create` syncs the file to disk before putting it in place, and `file.sync()?` syncs at any other point. Built. Closing a writer flushes it, and a failed flush fails the `with` block. Built. `std.io` has a `Writer` (`write`, `write_text`, `write_line`, `flush`) and a buffered `Reader` (`read_chunk`, `read_line`, `lines`), each failing with `IoError`. Files are both, a connection is both through `conn.writer()` and `conn.reader()`, which keep the network's words in a note, and `MemoryStream` is both in memory. `json`, `csv` and `pack` have `dump(value, ref out)` and `load(ref src)` beside `dumps` and `loads`. Built. `stderr` is a `Writer` as it is, and standard output is one through `stdout.writer()`. Standard input is a `Reader` through `stdin.reader()`. Built.

- Code after `lend` runs when the caller's block succeeds, and it may fail. That failure becomes the `with` statement's failure. `return`, `break` and `continue` in the caller count as success, so an early `return` still runs the cleanup, and a failing cleanup makes the function fail. Built.
- `lend x else:` runs when the caller's block fails or crashes, and the block's own failure is the one that passes up. If the cleanup in `else:` fails too, its failure is attached to the block's as a note. With several handles, every one runs its cleanup, innermost first. Running `else:` for a failed block is built, and so are several handles and the note: a cleanup that fails is added to the block's failure as a note, and the rest of the cleanup is skipped. Running it on a crash is built natively: the crash waits while the cleanups run, innermost first, and a cleanup that crashes too is said, with the first crash the one that goes on. A cleanup's `x = f()?` is a note too, and the rest of the cleanup is skipped. A `?` deeper in a line still replaces the block's failure.
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
- Channels are always bounded, and `send` waits when one is full. Both ends are plain values, so there can be many senders and receivers. `send` and `receive` take `timeout=`, and `receive` fails with `Closed` or `Timeout`. A channel you set up yourself opens with a plain function, `with channel[Job](size=64) as jobs:`, as Python's `open()` is, not by calling the type. The runtime's half is built. `std.concurrent` has `Channel[T]`, its ends `Inbox[T]` and `Sender[T]`, and the errors `Closed`, `Timeout` and `Crashed`, written in Fennec over channel intrinsics. `channel[T](size)` is the only way to make one, since `Channel`'s constructor is private. It builds natively, and so do `send`, `receive` and `for job in jobs`, each message copied across as it's sent.
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
- **A crash inside `lock x as ref s:` rolls back.** The runtime puts back every value the block changed, in reverse order, and then releases the lock, so nobody ever sees a half-change. A `fail` or `?` leaving `lock ... as ref` does too, so a write lock either finishes or has no effect, as a database transaction does, while `return`, `break` and `continue` keep the changes. That covers one `lock` statement and all the values locked together in it, and not local variables or output. To keep a change and still fail, decide inside the lock and `fail` after it. Built in native code; the interpreter doesn't roll back. There's nothing to write and no new failure type, and a lock is never left poisoned.
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

```fennec
def sell(ref self, item: Item) -> Item fails SellError:
    count = self._items.get(item) else fail .NotFound(item)
    if count == 0:
        fail .OutOfStock(item)
    self._items[item] = count - 1
    item

value = load(path)?              # pass the failure up
count = counts.get(word) else 0  # use a default
first = items[0]                 # must be there: a bug stops the program

stream = fs.open(filename) else:
    stdout.print(f"Couldn't open file: {filename}")
    return

# name the failure, and leave or give a value
score = load_score(fs, path) else as why:
    match why:
        case .Missing(path):
            stdout.print(f"no save at {path}, starting from 0")
            0
        case _:
            fail why
```

- A failing call is handled at the call, in one of three ways: `x = f()?` passes the failure up, `x = f() else value` uses a value instead, and `x = f() else as why:` names it in a block. There's no `Ok`, `Err` or `Result`. Built.
- `f().expect("reason")` crashes on purpose, with that reason, for a call that can't fail here. It's not a way to handle a failure: it says there isn't one. The reason has to say something: `.expect("")` is a compile error, "say why this can't fail". Built.
- An `else` block leaves, with `return`, `fail`, `break` or `continue`, each checked against the function's signature, or gives a value of the right type as its last line. Anything else is a compile error that says which it needs, and what its last line is. Built.
- Different failures are handled with an ordinary `match why:` inside the block, with `_` the only catch-all, and a failure passes on as `fail why`. There's no bare `fail`. Built.
- `else as` on a plain optional is a compile error, since there's no failure to name. On `T? fails E`, `?` handles the failure and `else` the `None`: `find(n)? else 5`. A plain `else` there would handle both, so it's a compile error, and `else as why:` handles only the failure, its block giving a `T?`. Built.
- `T fails E` is made only by the runtime, as for the results of parallel work, and read only through `?` and `else`. What can fail is never a plain value: holding it in a name, printing it, comparing it, putting it in a collection or handing it on is a compile error, and `fennec fix` adds `?` where the function can pass the failure up. `.expect("reason")` and `assert ... fails` read it too, and an arrow function may pass it up with `?`, as `failure_of(() => load(path)?)` takes it. Built.
- A block that stands where a value goes ends with that value, or leaves. That's `x = match ...:`, `x = lock ... as ...:` and `x = f() else as why:`, and not bare blocks or a multi-line `if`, which the one-line `x if c else y` covers. Built, and `x = if ...:` over several lines is a compile error that points at `x = a if c else b` or `match`.

- There's no `exit()`. `fail` out of `main` prints the failure to stderr and exits with code 1, which is built. `main` may also return an `int` as the exit code, which is built too. Nothing else can end the process.
- A failure passes up only with `?`, so every place one can is marked: `return load(path)` and a last line `load(path)` in a function that fails are compile errors, and `fennec fix` adds the `?`. An arrow function is held to it too: `failure_of(() => load(path)?)`, whose `?` passes the failure up as the arrow's own, not the function's around it. An arrow that can fail is refused where a builtin takes a function, as `sorted`'s `key=`, which nothing there expects to fail. Built.
- `?` means the same everywhere, `parallel` included: a failure in one round passes up out of the loop once the other rounds stop. Built. To let one piece fail alone, handle it in the body, with `else as why:` or `else: continue`. `break` stops new rounds from starting and lets the running ones finish, and a failure passing up cuts them short, like a failed `launch`. Each connection a server lends closes when its round ends, however it ends. Built. `server.serve(handler)` turns a handler's failure into a 500, in plain Fennec. Not built yet: today's handler can't fail.
- `?` is only for failures, `else` ends a chain and can leave, and ignoring a returned value is an error, with `_ = call()` to discard one. Built. On a call that can fail, `_ = f()` is a compile error, so a failure must always be handled, and there's no `ignore`. Built.
- `.Case` is required when the type is known, in values and in `case` patterns; `fennec fix` writes it. Built.
- An error carries notes, a `list[str]`, as Python's exceptions do: `failure.add_note(text)` adds one, and `failure.notes()` reads them. When a block fails and its `lend x else:` cleanup fails too, the cleanup's failure becomes a note on the block's, so neither is lost, and `main` prints each note under its failure. Built in the checker and natively, where an error's notes are a hidden word of its own, 0 until one is added.
- `fails A | B` combines failures from different places: `?` passes any of them up as it is, and a function type can say it too, as in `(str) -> int fails ParseError | RangeError`. Built.
- A test checks a failure with `assert load(text) fails Oops(line=3)`. The right side is a `case` pattern of what it fails with, `Oops()` matching any `Oops`. A failed one says what really happened: "it gave Config(...)" or "it failed with Oops(line=4)". It's only for `assert`: `fails` in an `if` is a compile error that points at `else as why:`. Built.
- A one-line `x if condition else y` and a fallback `else` can't share an expression without parentheses, since `int(text) else 80 if given else 8080` reads two ways: write `int(text) else (80 if given else 8080)`, or give one of them its own line. `fennec fix` writes the parentheses the way it's read. Built.
- Taking from an empty collection gives `None`: `list.pop()` returns `T?`, as a queue's `pop` does, so draining one reads `next = todo.pop() else break`. `pop(at)` stops the program on a position that isn't there, like `items[at]`. A dict's `pop(key)` stops the program on a key that isn't there, like `ages[key]`, and `remove(key)` gives `V?` instead: the value, or `None`, so `if ages.remove(name) is None:` asks whether it was there. `items.get(at)` gives `T?` for a position that may not be there, counting from the end when it's negative as `items[at]` does. Built.

## Visibility

```fennec
export struct Score:
    points = 0
    _best = 0                  # seen only by Score's own methods

export def best(scores: list[Score]) -> int:
    max(score.points for score in scores)
```

- `export` marks what other kits can use, and goes on top-level types and functions only. Without it, a name is private to its kit, and inside a kit every file sees everything. `fennec fix` rewrites the old `pub`. Built.
- A `_name` field or method of a `struct` or `shared` is hidden from everything but its type's own methods. Tests get no special access. Built.
- A top-level name never starts with `_`, since it's already private to its kit. That's a compile error, and `fennec fix` renames it everywhere. The standard library follows the same rule. Built.
- Other kits match an exported type's fields by name: `case IoError(reason=r)`, `case Point(x=a, y=_)`, or `case IoError()` to match it without reading a field. Matching another kit's type by position is a compile error, and `fennec fix` adds the names. A kit's own code can still match by position, and cases with no fields, like `.Wait`, are unchanged. Built for the standard library's types.
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
- `@skip` and `@serialize(name="uid")` on a field say how it's saved. Built. A kit may define annotations of its own for fields, which generic code reads through `fields(T)`. A generic function's `for f in fields(T):` is built, checked once, with each field's name and value, and annotations a kit defines and reading them aren't built yet. A function takes only `@hot`, so routes are registered in plain code, as in `router.get("/todos/{id}", show_todo)`.
- A `Secret[str]`, from `std.crypto`, can't be saved, printed or logged, and its text comes out only through `expose_secret()`, whose name says so. Printing one, an f-string or `str()` of one, and saving one in any format are compile errors, and so is doing any of those to something that holds one. Printed through a generic function's `T`, in a failed `assert` or in the failure that ends `main`, it shows `Secret(hidden)`. `==` on two secrets compares them in constant time, so its timing never says where they differ, and a secret of anything but text or bytes can't be compared. Built. `Secret.of(text)` makes one, and `Secret[bytes]` holds a key. A server's `Certificate`, from `std.net`, holds its private key as one, beside its chain of certificates.

Loading is safe on untrusted input:

- A load always makes fresh values, never a link to anything already in the program. Built.
- Every `Id` resolves within its own document, and each `Table` carries a tag that's never reused, so an `Id` from one `Table` finds nothing in another of the same type. Built.
- `where` rules run again on load, so a value that breaks one fails the load. Built.
- Unknown fields are refused unless `extra=` says otherwise: `json.loads[Settings](text, extra=.Ignore)`. Built for JSON. TOML always refuses them, and doesn't take `extra=` yet. `std.pack` loads data from an earlier version of a type by its fields' names: a field the data has and the type doesn't is passed over, a field the type has and the data doesn't takes its default, and one with no default fails the load, naming it. Cases and union members are found by name too, and an `int` field that's a `float` now reads as one. Built. Each value from `dumps` carries its type's layout, its field and case names. A `pack.Packer` writes many values with each layout once, and `pack.Unpacker` reads them back, from a file or fed from a stream. `fennec update` locks a kit's layouts in `pack.lock`, and a locked type's single values leave the layout out, as short as before layouts. Built.
- JSON and TOML refuse what they'd have to guess at: a number too big for a float, a struct's field given twice, and a dict's int key written any way but its plain digits, or not in quotes. Built.
- Untrusted input has depth and size limits. `std.pack` limits nesting, each string's length, the total it reads and how many items that take no bytes it makes, so bytes can't ask for far more memory than they are. Built. JSON and TOML limit nesting to 500 deep, and don't limit size, string length or collection length yet.

## Templates

```fennec
# the literal pieces and the values stay apart
rows = db.query(t"select name from players where team = {team} and score > {least}")?
page = html.render(t"<p>Hello, {visitor.name}</p>")?
result = process.run(t"git log --author={author}")?
```

- `std.sqlite` takes them: `db.execute(t"...")` and `db.query[T](t"...")`, with each value bound as a parameter and each row read into a `T` by its columns' names. Built, natively. In a test, each path is a real WAL database file of the test's own, so a test reads as a program does.
- `std.postgres` speaks Postgres's protocol in plain Fennec: its messages, signing in with SCRAM-SHA-256, and a query as the extended protocol's messages, its template's values bound apart and its rows read by column type into `sql.rows_as[T]`. `with postgres.connect(net, entropy, "postgres://app@db.internal/shop", password=secret)? as db:` lends a `PgPool`, a pool of connections through `std.net`, 10 unless `pool_size=` says. `with db.transaction()? as tx:` and `with db.borrow()? as conn:` each lend one connection to a block, with `execute` and `query[T]` as `std.sqlite` has, and wait up to `wait=`, `30s` unless it's set, then fail with `Timeout`. `pool_size` and `wait` can be in the URL too, as `?pool_size=20&wait=5s`, but not in both. A URL with a password in it has to be a `Secret`, as `env.secret("DATABASE_URL")?` reads one. A plain `str` URL with a password is refused, at compile time when it's written in the source, and otherwise the password goes in `password=`, a `Secret`. A failure shows the URL with its password as `****`. Built, and tested against a real server. Its connection is TLS unless `tls=False` says otherwise, and a server that won't take TLS is refused rather than spoken to in the clear. It lives in `std` until first-party kits have a home. Its connections are TLS unless `tls=False` says otherwise, and a server that won't take TLS is refused rather than spoken to in the clear. Planned: TLS picked by host, in the clear for a loopback host and verified TLS for any other, with the URL's `sslmode=` taking `disable`, `require`, `verify-ca` or `verify-full`, and refusing `prefer` and `allow`. It lives in `std` until first-party kits have a home.
- `t"..."` makes a `std` `Template`, as Python 3.14's t-strings do. It holds the literal pieces and the values separately, and the values' type comes from where it goes, as in `Template[SqlValue]`. A `Template[T]` has `strings`, one more than its `values`, plus each value's `expressions` as written and its `format_specs`, and `interpolations()` gives each value with both.
- `html.render(t"...")` gives an `Html`, each value put in for where it is in the page, found from the text around it. Between tags and in a quoted attribute it's escaped. Where escaping isn't enough, inside a `<script>` or `<style>`, an `on...`, `style` or `srcdoc` attribute, an attribute without quotes, a name or a comment, the page fails with `HtmlError.Misplaced`, naming the value. So does a value in a link's attribute, like `href` or `src`, that would make the link anything but `http`, `https`, `mailto` or relative, like `javascript:`: nothing is put in its place silently, and a program that wants a fallback checks the URL first. An `Html` goes into another as it is, and `html.join` puts pieces one after another, so pages are built from pieces. An `Html` is never loaded from saved data. Built, natively.
- `process.run(t"git log --author={author}")` runs what the template says, its words split at the spaces in the text written in the source. A value stays part of one argument whatever it holds, so it can't add an argument or start a command. A value that starts an argument with `-` fails with `ProcessError.LooksLikeAnOption`, naming the value, since the program would read it as an option, like git's `--upload-pack=`. A value later in an argument, like `--depth={depth}`, is fine, and so is one after an argument the template writes as `--`. Built, beside `process.run("git", ["log"])`.
- A `str` is never a `Template`, so `db.query`, `html.render` and `process.run` can't be handed text built by joining strings, and SQL, HTML and shell injection can't be written. There are no per-library string prefixes.
- `LiteralStr` is a string that came from the source code, as Python's `typing.LiteralString` is. Only a string literal, or literals joined with `+`, makes one. It goes anywhere a `str` goes, but a `str` is never a `LiteralStr`. A `Template`'s text pieces are `LiteralStr`, so building one from text read while the program runs is a compile error, and any function may ask for one. It's for strings only.

```fennec
def order_column(sort_by: SortBy) -> LiteralStr:
    # user input picks between literals, so the result is still a LiteralStr
    match sort_by:
        case .Name:
            "name"
        case .Score:
            "score"

def order_from(request: Request) -> LiteralStr:
    # a compile error: text read while the program runs is a str, never a LiteralStr
    request.query["sort"]
```

## Calling C

C is called only through the standard library and first-party kits, whose binding to C sits in reviewed runtime code. A program sees a safe API, like `with Window.open(...)? as window:`, and can't call C itself, so there's no `unsafe` in Fennec. Not built yet.

## Untrusted code

Den tests and agents' snippets are compiled to WebAssembly by the server build and run in Wasmtime, with fuel and memory limits and no files or network unless they're granted. No program runs untrusted code inside itself. Not built yet.

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
        lock rooms as current:
            # your own copy, so the lock ends before the file is written
            snapshot = current
        fs.write_text("rooms.json", json.dumps(snapshot))?

def main(net: Net, fs: Fs, clock: Clock, entropy: Entropy):
    shared rooms = dict[str, Room]()
    with postgres.connect(net, entropy, "postgres://localhost/chat")? as db:
        with launch(() => autosave(rooms, fs, clock)?, stop_after=5s)?:
            with net.listen(9000)? as server:
                # each connection gets its own task, and closes when its round ends
                for conn in parallel(server):
                    # one client's failure ends only its own round
                    serve(conn, rooms, db) else:
                        continue
```

- `rooms` is made in `main` and passed down, so every function that touches it says so.
- Nothing waits inside a lock: `recent` copies the messages out, and the writes to the client happen after.
- A failure in one connection is handled in its round, so it ends only that round. Without the `else:`, it would pass up and end the server. When `main`'s block ends, the autosave task is stopped, the server and every connection close, and the pool closes its connections.
- There's no lifetime, move, destructor or `async` anywhere in it.

## Building

- `fennec run` runs the tests a change affects, then the program, built through LLVM a function at a time and cached. Where LLVM can't build something yet, it says why, as `fennec build` does, and `fennec test` fails the tests that reach it. Built: `fennec` has no interpreter. The JIT is removed, and the commit before got the tag `before-jit-interpreter-removal`. Built.
- A native program carries no interpreter: `fennec build` links one runtime with no interpreter in it, and the program's syntax isn't in the image, so a hello-world is about 1.1 MB. Built.
- The launcher runs the toolchain a kit asks for, `fennec = "0.4.2"` in `kit.toml`, pinned by its signed manifest's hash in `kit.lock`. It fetches one only when it's missing, and never with `--offline`, and only `fennec update` moves a pin. Built, as `fennec-launcher` for now. No release keys are made yet, so nothing it fetches verifies today.
- A release's archives are the same bytes however often and wherever they're built, so anyone can build one again from its tag and check it against the signed manifest. CI builds each platform twice and compares them. Built.
- `fennec build` makes a native program through LLVM, and `fennec build --release` optimizes the whole program as one. Built.
- `embed_file("path")` gives a file's `bytes`, and `embed_folder("path")` a read-only map from relative path, always with `/`, to `bytes`, in sorted path order. Both are views into the program's read-only data, with no copy and nothing loaded at startup, and need no capability. A missing path, or anything above the folder holding the nearest `kit.toml` (a lone file's own folder when there's none), symlinks included, is a compile error. Hidden files are left out, and nothing is compressed. `embed_folder` gives a `std` type with `get(path) -> bytes?`, `len()`, and `keys()`, `values()` and `items()` in sorted order. Built natively. Until native code holds them in the program's read-only data, each is made once, the first time it's reached. A lone file's `embed_file` can still follow a symlink out of its folder today.

## Checks

- Every check stays, unless the compiler proves it can't fail: overflow, bounds, missing keys and divide by zero. A proven check is removed in every function, not only in `@hot` ones. Built.
- `x / 0.0` stops the program, as Python's `ZeroDivisionError` does, and so does integer division by zero. Built. The compiler removes the check where it proves the divisor isn't zero.
- A broken `where` rule stops the program in code, and a load that breaks one fails cleanly. Built.
- `@hot` allows a leftover overflow check, and refuses any leftover bounds, key or integer divide check as a compile error that says why. Built. It allows a leftover float divide check too, as it does overflow. Built.
- Speed never comes from dropping a check: there's no unchecked arithmetic and no switch that turns checks off. There's no `@fast_math` either: a float result is the same bits on every machine and every build, which replays and lockstep games depend on.

## Speed

- Everyday builds stay fast, and release builds can take as long as they need.
- `sum()` over floats, `dot()`, and `std.math`'s vector and matrix reductions add in one fixed pattern, 8 running totals combined pairwise, so they're fast, the same bits on every platform, and more accurate than adding left to right. Built for `sum`, over a list or one item at a time, and for `math.dot(a, b)` over lists. `std.math`'s sums of four, as in `Vec4.dot` and `Mat4`'s products, keep glam's left-to-right order, which is as fixed, so they match glam bit for bit. `math.fma(a, b, c)` rounds `a * b + c` once, the same everywhere. Not built yet.
- `@hot` turns a hidden copy into a compile error, and the tracer shows where each copy happens and why. Built: anything that may allocate or copy in a `@hot` function, or in what it calls, is an error that says why, and so is any check native code keeps there, like an index it can't place inside its list. An overflow check the compiler can't prove away stays, as the safety net it is everywhere.

## Still to decide

Nothing right now.
