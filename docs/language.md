# Fennec on one page

This page is the source of truth for the language. If `decisions.md` disagrees with it, this page wins. Most of it is built, and what isn't says so.

Fennec reads at a glance, builds almost instantly, and is built to run as fast as hand-written systems code in release builds. Everything is a plain value by default, and the compiler does the hard work.

## Four rules

- A name holds its own value.
- `mut` lets a name change.
- To change something in a list, write to it there.
- `ref` lets a loop or a function change yours.

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

There's no inline `ref`, and no scoped `with units[target_id] as ref target:` either: write directly, `units[target_id].health -= 1`, or call a `ref self` method on the element's type. The one function that may return a `ref` is a type's `ref` form of `def []`, which gives a place, so `units[i].health -= 1` works on your own types too.

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

## Copies

```fennec
uppercased = [copy(person, name=person.name.upper()) for person in people]
```

`copy(x, field=...)` makes a changed copy and re-runs `where` rules. Built: `replace` was its old name, and `fennec fix` moves it to `copy`. A plain `=` covers everything else, and the compiler skips the real copy when nobody can tell. A plain parameter can't change, so passing something huge costs nothing.

## Sized numbers

Not built yet.

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
- Mixing types, like `i32 + i64`, is a compile error that says to convert one side. Widening is written too, as `i64(small)`, and can't fail.
- Every sized type checks overflow, and a check is removed only where the compiler proves it can't fire. `wrapping_` and `saturating_` methods are for when you mean it.
- Floats give the same bits on every platform. Arithmetic and `sqrt` are exact in IEEE 754, and `sin`, `cos`, `tan`, `acos`, `exp`, `log` and `**` are computed by one library, `libm`, on arm64, x86-64, Windows and WebAssembly alike, so replays and lockstep multiplayer agree. Each platform's own C library rounds some of these differently in the last bit. Built.
- Floats have one total order: NaN equals itself and sorts last, and `<` agrees with `sorted`. `==`, hashing and sorting are built, and `<` on a NaN isn't yet. A "never NaN" analysis is planned, to recover the cost where a value can't be NaN.
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

Tables are for large, flat data. An `Id` is an index, a generation and a tag for its table, so it finds the right thing or nothing.

## Sharing, resources and tasks

Built so far: `shared T`, `shared name = value`, and `lock`, reaching a `shared` value only in a block, on every engine. A `lock` inside another `lock` is refused for now; take them together in one. The rest of this section isn't built yet. Today's `shared Name:` declarations, `with x as ref y:` and `def del` still compile, and `fennec fix` rewrites them when this lands.

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
- It's reached only inside `lock x as s:`, to read, or `lock x as ref s:`, to change. Any way out of the block releases it, `?`, `fail` and `return` included. Leaving with `fail` or `return` keeps the changes made so far, and only a crash undoes them, below. Built, apart from undoing a crash.
- **No waiting inside a lock:** no channel receives, pool takes, network calls, `parallel`, or the end of a `with launch`. Printing and local files are fine. Wait first, then lock.
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

The end of a `with` block is where what it lent finishes: a file closes and a transaction commits. A failure while finishing is the `with` statement's failure, covered by the `?` on its line, so `with fs.create(path)? as file:` covers both opening and finishing. Built for sources a program writes, as in `examples/sources.fen`; the standard library's files and transactions move to it next.

- `with` lends you something for the length of a block, then takes it back. It's the only way to open files, sockets, servers and transactions. Built for sources a program writes, below; the standard library's files and sockets move to them next.
- What it lends can't escape the block: it can't be returned, stored outside it or put in a `shared`. A second name for it inside the block is fine.
- A struct holding something borrowed is borrowed too, so a wrapper like `LineProtocol(conn=conn)` works inside the block and can't leak out.
- `with conn.split() as reader, writer:` gives a connection's read and write sides, for one task reading while another writes.
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

- Code after `lend` runs when the caller's block succeeds, and it may fail. That failure becomes the `with` statement's failure. `return`, `break` and `continue` in the caller count as success, so an early `return` still runs the cleanup, and a failing cleanup makes the function fail. Built.
- `lend x else:` runs when the caller's block fails or crashes, and the block's own failure is the one that passes up. Built for a failed block; running it on a crash, and letting the block's failure win over one in `else:`, aren't built yet.
- There's nothing for the caller to finish by hand, like a `commit()` or a `close()`, and nothing to forget.
- A source can't `lend` while it holds a lock it took. Opening a file inside a lock is fine.
- A generator can `lend` each item, closed when that round of the loop ends: `def clients(server: Server) -> Iterator[with Connection]:`. That's how `for conn in parallel(server):` works, with each connection lent to its own round's task. It's a plain loop, not `for ref`, and a connection's methods take `self`.
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
    with launch((inbox) => audio(inbox, speakers), stop_after=5s) as sounds:
        for frame in input.frames():
            if frame.jumped:
                sounds.send(.Play(name="jump", volume=0.8))?
```

- `launch` runs a function alongside you until its `with` block ends, so no task outlives the code that started it. It's only ever written as `with launch(...)`.
- The arrow is the call you'd write anyway. `launch`'s own options, `stop_after=` and `inbox_size=`, sit outside it.
- A parameter typed `Inbox[T]` makes `launch` create the channel: the task gets the receiving end and you get the sending end, as `sounds`. The enum is the protocol, so you can only send real cases and the task must handle each one. A task with no messages takes no inbox: `with launch(() => autosave(rooms, fs, clock)):`.
- `TaskGroup`, from `std.concurrent`, holds a number of tasks known only at run time, and stops them all when its block ends.
- Channels are always bounded, and `send` waits when one is full. Both ends are plain values, so there can be many senders and receivers. `send` and `receive` take `timeout=`, and `receive` fails with `Closed` or `Timeout`. A channel you set up yourself is `with Channel[Job](size=64) as jobs:`. Built.
- A channel delivers each message to one receiver by default, as a work queue. `to=.Every` gives every listener every message, as in `with Channel[Hit](size=1024, to=.Every) as hits:`, and receiving reads the same either way, `for hit in hits`. With `.Every`, `send` never waits, and a listener that falls behind fails loudly, unless it asked for `when_behind=.DropOldest`. There's no separate events type. Not built yet.
- Dev builds stop with a report when tasks are waiting only on each other, since a hang through channels alone can't be ruled out at compile time.

### Pools

```fennec
with Database.open(net, url, pool_size=10)? as db:
    for request in parallel(server):
        with db.transaction()? as tx:
            tx.insert(order)?
```

- Pools in `std.concurrent` are written in plain Fennec. Each connection lives inside one keeper task's `with`, and the pool keeps only the keepers' inboxes, so nothing can leak or be used twice.
- A failed block resets the connection it used. A crashed job's connection is replaced, so the pool heals itself and never shrinks.
- Libraries hand out what users need, like `with db.transaction() as tx:`, so keepers stay inside the library.

### When tasks stop or crash

- When a `with launch` block ends normally, the task's inbox closes and the task drains it. If it's still running after `stop_after`, every wait it's in is cut short, socket and stdin reads included. A wait cut short by a stop ends the task quietly, with no failure reported, since the stop was asked for. Not built yet. A block that failed cuts them short at once.
- A task's failure comes out where its `with launch` block ends, and a crash comes out as `Crashed`. A crash ends only its own task. The HTTP server answers that request with a 500.
- **A crash inside `lock x as ref s:` rolls back.** The runtime puts back every value the block changed, in reverse order, and then releases the lock, so nobody ever sees a half-change. Only crashes roll back, since `fail` and `return` keep their changes. There's nothing to write and no new failure type, and a lock is never left poisoned.
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
- `if x is Circle:` narrows `x` inside the block, and `match` handles each member, checked for exhaustiveness. A method every member has is the union's too.
- `None` is never a member: `str | None` is an error whose fix writes `str?`, and an optional union is `(A | B)?`.
- Failures stay separate: a function that can fail returns `-> T fails E`, never `-> T | E`.

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

- A type may define each operator once for each right-hand type, so `Mat4 * Mat4` and `Mat4 * Vec4` can both exist. Not built yet: today a type has one `def *`, and the checker lane builds this next.
- Types must match exactly, with no conversions, so there's never a best match to choose. `now + now` is a compile error that lists the `+` that `Time` has.
- It's for operators only. A named function still has one meaning, so `load(path)` is always the same `load`.
- A user type gets this exactly as `std`'s types do.

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
```

- There's no `exit()`. `fail` out of `main` prints the failure to stderr and exits with code 1, which is built. `main` may also return an `int` as the exit code, which isn't built yet. Nothing else can end the process.
- `?` means the same everywhere, `parallel` included: a failure in one round passes up out of the loop once the other rounds stop. Built. To let one piece fail alone, handle it in the body, with `match` or `else: continue`. `break` stops new rounds from starting and lets the running ones finish, and a failure passing up cuts them short, like a failed `launch`. Each connection a server lends closes when its round ends, however it ends. Not built yet, apart from the failure passing up. `server.serve(handler)` turns a handler's failure into a 500, in plain Fennec. Not built yet: today's handler can't fail.
- `?` is only for failures, `else` ends a chain and can leave, and ignoring a returned value is an error, with `_ = call()` to discard one. Built.
- `.Case` is required when the type is known, in values and in `case` patterns; `fennec fix` writes it. Built.
- `fails A | B` combines failures from different places: `?` passes any of them up as it is, and a function type can say it too, as in `(str) -> int fails ParseError | RangeError`. Built.
- An `else:` block must leave: it ends with `return`, `fail`, `continue` or `break`. Built.
- Taking from an empty collection gives `None`: `list.pop()` returns `T?`, as a queue's `pop` does, so draining one reads `next = todo.pop() else break`. `pop(at)` fails on a position that isn't there, like `items[at]`. A dict's `pop(key)` fails on a key that isn't there, like `ages[key]`, and `remove(key)` gives `V?` instead: the value, or `None`, so `if ages.remove(name) is None:` asks whether it was there. `items.get(at)` gives `T?` for a position that may not be there, counting from the end when it's negative as `items[at]` does. Built.

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

Loading is safe on untrusted input:

- A load always makes fresh values, never a link to anything already in the program. Built.
- Every `Id` resolves within its own document, and each `Table` carries a tag that's never reused, so an `Id` from one `Table` finds nothing in another of the same type. Built.
- `where` rules run again on load, so a value that breaks one fails the load. Built.
- Unknown fields are refused unless `extra=` says otherwise: `json.loads[Settings](text, extra=.ignore)`. Built for JSON. TOML always refuses them, and doesn't take `extra=` yet. `std.pack` saves no field names, and refuses data packed from a different type.
- Untrusted input has depth and size limits. `std.pack` limits nesting, each string's length and the total it reads, before allocating anything. Built. JSON and TOML limit nesting to 500 deep, and don't limit size, string length or collection length yet.

## Calling C

C is called only through the standard library and first-party kits, whose binding to C sits in reviewed runtime code. A program sees a safe API, like `with Window.open(...)? as window:`, and can't call C itself, so there's no `unsafe` in Fennec. Not built yet.

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

def main(net: Net, fs: Fs, clock: Clock):
    shared rooms = dict[str, Room]()
    with Database.open(net, "postgres://localhost/chat", pool_size=10)? as db:
        with launch(() => autosave(rooms, fs, clock), stop_after=5s):
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

- `fennec run` runs the tests a change affects, then the program, built through LLVM a function at a time and cached. Where LLVM can't build something yet, the JIT stands in and says so. Built. The JIT and the interpreter are then removed, once LLVM builds everything, and the commit before gets a tag. Not built yet.
- `fennec build` makes a native program through LLVM, and `fennec build --release` optimizes the whole program as one. Built.

## Checks

- Every check stays, unless the compiler proves it can't fail: overflow, bounds, missing keys and divide by zero. A proven check is removed in every function, not only in `@hot` ones. Built.
- `x / 0.0` stops the program, as Python's `ZeroDivisionError` does, and so does integer division by zero. Built. The compiler removes the check where it proves the divisor isn't zero.
- A broken `where` rule stops the program in code, and a load that breaks one fails cleanly. Built.
- `@hot` allows a leftover overflow check, and refuses any leftover bounds, key or integer divide check as a compile error that says why. Built. It will allow a leftover float divide check too, as it does overflow. Not built yet.
- Speed never comes from dropping a check: there's no unchecked arithmetic and no switch that turns checks off.

## Speed

- Everyday builds stay fast, and release builds can take as long as they need.
- `@hot` turns a hidden copy into a compile error, and the tracer shows where each copy happens and why. Built: anything that may allocate or copy in a `@hot` function, or in what it calls, is an error that says why, and so is any check native code keeps there, like an index it can't place inside its list. An overflow check the compiler can't prove away stays, as the safety net it is everywhere.

## Still to decide

Nothing right now.
