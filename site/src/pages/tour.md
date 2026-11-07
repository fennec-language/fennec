---
layout: ../layouts/Doc.astro
title: A tour of Fennec
description: A walk through Fennec's design, from values and tables to lifetimes, streaming, errors and sharing, with what's built and what's planned marked as you go.
---

# A tour of Fennec

<p class="lede">If you can read Python, you can read Fennec. What's different is underneath: data is plain values in flat tables, types are checked before anything runs, errors can't be ignored, and there are no references to manage.</p>

> **What you can trust on this page.** Fennec is in progress. Every plain code sample here compiles today, and a test checks that it does. Anything that doesn't yet sits under a **Planned** label: that part of the design is decided but not built, so don't expect it to run. A section marked **Being redesigned** describes something that's still being decided and may change.

## Hello

```fennec
def main(stdout: Stdout):
    stdout.print("Hello, Fennec")
```

`main` receives what the program is allowed to touch, here standard output. There's no global `print`: code can only write output if it's given somewhere to write it. More on that in [capabilities](#capabilities).

## Names

A name is immutable unless you declare it `mut`. There's no `let` or `const`, because those two forms already say everything.

```fennec
def main(stdout: Stdout):
    name = "Ash"
    mut health = 100
    health -= 10
    stdout.print(name, health)
```

Writing `name = "Vex"` next would be a compile error, and it tells you to declare it `mut name = "Ash"`.

An immutable name is set once, by a single expression. When the value depends on a condition, use a conditional expression, or `match` for more than two choices:

```fennec
enum Kind:
    Runner
    Walker
    Turret

def speed_of(kind: Kind, health: int) -> float:
    label = "boss" if health > 500 else "minion"
    speed = match kind:
        case .Runner: 8.0
        case .Walker: 3.0
        case .Turret: 0.0
    speed
```

Names belong to the block they're created in. A name created inside an `if`, `for` or `with` doesn't exist after it, so nothing leaks into the rest of the function.

## Functions

Parameters and return types are written; everything inside a function is inferred. The last expression in a function is its value.

```fennec
from std.math import PI

def area(radius: float) -> float:
    PI * radius * radius

def factorial(n: int) -> int:
    1 if n <= 1 else n * factorial(n - 1)
```

- Use `return` to leave early. A function with no `->` returns nothing. If you leave a return type out, the error's fix writes in the one the compiler inferred.
- Short functions can be passed inline with an arrow: `enemies.sort(key=enemy => enemy.health)`.
- Closures read the names around them but can't change them. A nested function that's only called directly, never passed around, can change the enclosing function's `mut` names.

A parameter can have a default, which is passed by name only. Defaults are plain values computed at compile time, so Python's shared-mutable-default surprise can't happen:

```fennec
def scan(rows: list[int], limit=8) -> list[int]:
    rows[:limit]

def main(stdout: Stdout):
    stdout.print(scan([1, 2, 3]), scan([1, 2, 3], limit=1))
```

A literal default gives the parameter its type: an `int`, `float`, `str` or `bool`, or an enum case like `mode=Mode.fast`. Writing `limit: int = 8` is an error whose fix drops the `: int`. The type is written when the default is `None`, as in `target: Unit? = None`, or isn't the literal's own type.

A constructor call with its type arguments written, like `dict[str, int]()` or `Packed[int]()`, gives the type the same way, for struct fields and parameters alike. For an empty collection it's the only form of default: `items = list[str]()`, and `items: list[str] = []` is an error that says so. Inside a function body, `items = []` stays fine.

The positional calls that are easy to get backwards are compile errors: a bare `True` or `False`, and neighboring arguments of the same type, which could be swapped and still compile. The error says which one to name. The first argument and `self` are exempt.

## Built-in types

| Type | Example |
| --- | --- |
| `int`, `float` | `42`, `3.14` (64-bit) |
| Sized numbers | **Planned:** `i32`, `u8`, `f32` when you want control over size |
| `bool` | `True`, `False` |
| `str` | `"fennec"`, `f"{count} foxes"` |
| `list`, `dict`, `set` | `[1, 2]`, `{"hp": 10}`, `{1, 2}` |
| `tuple` | `(3, 4)` |

Built-in types are lowercase. Everything you define or import is capitalized.

## Structs

A struct is a set of named fields, like a Python dataclass. You get a constructor automatically, and fields can have defaults. A field with a default takes its type from it, so only fields without one need a type.

```fennec
struct Config:
    volume = 0.8
    fullscreen = False
    levels = ["intro"]

def main(stdout: Stdout):
    config = Config(volume=0.5)
    defaults = Config()            # every field has a default
    stdout.print(config, defaults)
```

A method without `self` is a named constructor, for anything the automatic one can't do:

```fennec
from std.math import Vec3

struct Enemy:
    position: Vec3
    health = 100

    def at(position: Vec3) -> Enemy:
        Enemy(position=position)

def main(stdout: Stdout):
    boss = Enemy.at(Vec3(x=1.0, y=2.0, z=3.0))
    stdout.print(boss.health)
```

Rules about a field's value go right next to it with `where`. They look only at their own field. An invariant across several fields goes in a named constructor or a method.

```fennec
struct Player:
    level: int where level >= 1

def main(stdout: Stdout):
    player = Player(level=1)
    stdout.print(player)
```

Equality, hashing and printing work automatically for any type whose fields support them. There's nothing to derive.

## Values and tables

Fennec has no object graphs. Data is plain values, nested inline, and the things that point at each other live in tables and hold each other's `Id`.

```fennec
from std.collections import Id, Table

struct Unit:
    health = 100
    target: Id[Unit]? = None

def main(stdout: Stdout):
    mut units = Table[Unit]()
    ash = units.add(Unit())
    vex = units.add(Unit(target=ash))
    units[ash].health -= 10
    stdout.print(units[ash].health, units[vex].target == ash)

    units.remove(ash)
    stdout.print(units.get(ash))      # None: a stale Id finds nothing
```

An `Id` is a small plain value with a generation in it, so it stays safe after its slot is reused, and a cycle of them can't leak. `Tree`, in `std.collections`, does the same bookkeeping for parents and children, and a `Graph` built the same way is planned. There are no classes, so this is how graphs and trees are done.

Assigning or passing a value never lets anyone change yours. This doesn't mean everything gets copied: the compiler passes values by reference when it's safe, moves them when the original isn't used again, and shares large data until one side changes it. You write simple value code, and the compiler does the rest.

Nested structs are stored inline, so editing one is a single write to memory. To change something in a list or a table, write to it there:

```fennec
struct Stats:
    health = 100

struct Unit:
    stats: Stats

def main(stdout: Stdout):
    mut units = [Unit(stats=Stats())]
    units[0].stats.health -= 10       # edits the unit in the list

    mut backup = units[0]             # your own copy
    backup.stats.health = 0
    stdout.print(units[0].stats.health, backup.stats.health)
```

A name always holds its own value, `mut` or not. So `mut unit = units[0]` is a copy, and changing it and then never reading it is a compile error, since the change would be lost. The fix writes to the list directly: `units[0].stats.health -= 10`. To let a loop or a function change something in place, use [`ref`](#mutation-is-visible). There's no inline `ref` name.

**Planned:** a compile error for a copy that can quietly go stale:

```fennec planned
world.selected = world.units[i]          # error: edits to one won't reach the other
world.selected = copy(world.units[i])    # fine: a separate copy, on purpose
```

A copy that would happen on every iteration of a loop is a compile error, naming the line and why the value is shared, as in "`state.xs` is copied each time round the loop, since it was put into `history`, which is used again". A copy that happens once, on the first write, is only a hint.

A write that goes nowhere is a compile error as well: a local that's changed and then never read, returned or passed on. A write through a `ref` doesn't count, since that reaches the original. It doesn't catch a copy that's read afterwards.

## Mutation is visible

Parameters are read-only. A function that changes its argument says `ref` in its signature, and every call to it says `ref` too.

```fennec
struct Player:
    health = 100

def apply_damage(ref player: Player, amount: int):
    player.health -= amount

def main(stdout: Stdout):
    mut player = Player()
    apply_damage(ref player, 10)   # you can see player changes here
    stdout.print(player)           # and that it doesn't change here
```

Reading any call, you know whether it can modify your data without opening the function. A `ref` needs a `mut` name or another `ref` to start from, and it can't be stored, returned or captured, so there are no lifetimes to write. Methods and loops say it the same way:

```fennec
struct Enemy:
    health: int

    def take_damage(ref self, amount: int):
        self.health -= amount

def main(stdout: Stdout):
    mut enemies = [Enemy(health=3)]
    for ref enemy in enemies:
        enemy.take_damage(1)
    stdout.print(enemies)
```

## Enums and match

An `enum` is one of several cases, each with its own data. `match` must handle every case.

```fennec
from std.math import PI

enum Shape:
    Circle(radius: float)
    Rect(width: float, height: float)

def area(shape: Shape) -> float:
    match shape:
        case .Circle(radius): PI * radius * radius
        case .Rect(width, height): width * height
```

The leading dot marks an enum case. You only write the enum's name if two enums in scope share a case name.

## Missing values

A value that might be missing has a `?` on its type: `Enemy?` holds either an `Enemy` or `None`. The compiler won't let you use it until you've checked.

```fennec
struct Enemy:
    health = 3

def find_target(enemies: list[Enemy]) -> Enemy?:
    if enemies.len() == 0: return None
    enemies[0]

def main(stdout: Stdout):
    target = find_target([Enemy()])
    if target is None:
        return
    stdout.print(target.health)              # the compiler knows it's an Enemy here
```

A chain that ends in `else` can read through something that might be missing. It stops at the first missing step and gives the default. `?` only ever means a failure passes up.

```fennec
struct Enemy:
    health = 3

def find_target(enemies: list[Enemy]) -> Enemy?:
    if enemies.len() == 0: return None
    enemies[0]

def main(stdout: Stdout):
    health = find_target([]).health else 1      # 1 if there's no target
    scores = {"red": {"ash": 12}}
    score = scores.get("red").get("vex") else 0
    stdout.print(health, score)
```

`else` covers only the chain right before it, so a larger expression needs brackets: `(find_target(enemies).health else 1) + 1`. Without the `else`, it's a compile error that suggests one or an `is None` check. Indexing with `[]` in such a chain isn't built yet.

## Errors

There are no exceptions. An error is a value a function returns when it fails, and you can't use the result without dealing with it.

```fennec
error NotFound:
    path: str

def load_score(path: str) -> int:
    if path == "":
        fail NotFound(path)
    int(path)?
```

`fail` returns an error, and `?` passes one up to your caller, so you can see every line a function can leave from. There are a few ways to deal with a call that can fail:

```fennec
error NotFound:
    path: str

def load_score(path: str) -> int:
    if path == "":
        fail NotFound(path)
    10

def doubled(path: str) -> int:
    load_score(path)? * 2                       # pass any error up

def main(stdout: Stdout):
    stdout.print(load_score("") else 0)         # use a default if it failed

    match load_score(""):                       # handle specific errors
        case Ok(score): stdout.print(score)
        case NotFound(path): stdout.print("no file")
        case Err(error): stdout.print(error)
```

The default after `else` is only evaluated if it's needed. Failing is a plain return, with no stack unwinding.

A run-time bug is different from an error. `xs[i]` out of range stops the program with a clear message, because there's nothing sensible to handle.

**Errors in the signature.** A function can say what it fails with, `fails Name`, where one `error` can list several cases, so a signature stays short. `fails A | B` combines failures from different places, and `?` passes any of them up as it is.

```fennec
error LoadError:
    NotFound(path: str)
    BadRow(line: int)

def load(path: str) -> float fails LoadError:
    if path == "":
        fail .NotFound(path)
    1.0
```

**Also planned:** `if result is Err(NotFound):`, which checks one error and narrows the rest; `?` required everywhere a failure passes up, including in `with` and `for`; a compile error for checking an error and then ignoring it; and `xs.get(i)`, which returns an optional when an index might not be there.

## Collections

Comprehensions build collections, and the brackets tell you something new is being allocated. Without brackets, a generator expression is lazy.

```fennec
struct Enemy:
    name: str
    health = 3

def main(stdout: Stdout):
    enemies = [Enemy(name="Ash"), Enemy(name="Vex", health=0)]
    alive = [enemy for enemy in enemies if enemy.health > 0]   # a new list
    by_name = {enemy.name: enemy for enemy in alive}           # a new dict
    total = sum(enemy.health for enemy in enemies)             # just a loop
    strongest = max(enemies, key=enemy => enemy.health)
    stdout.print(by_name, total, strongest)
```

- Python's looping builtins come along: `sum`, `min`, `max`, `any`, `all`, `sorted`, `enumerate`, `zip`, `range`. Questions about one value are methods: `xs.len()`, `x.abs()`.
- A `dict` keeps insertion order, so iterating it gives the same order every time.
- `d.get(key)` returns an optional, and a fallback reads the way it does anywhere else: `d.get(key) else 0`.
- A struct works as a dict key with nothing to write. It hashes all its fields.

```fennec
struct Tile:
    x: int
    y: int

def main(stdout: Stdout):
    mut seen = {Tile(x=1, y=2): 1}
    stdout.print(seen.get(Tile(x=1, y=2)) else 0, seen.get(Tile(x=0, y=0)) else 0)
```

`SortedMap`, in `std.collections`, keeps its keys in order on a B-tree:

```fennec
from std.collections import SortedMap

def main(stdout: Stdout):
    mut scores = SortedMap[str, int]()
    scores["vex"] = 7
    scores["ash"] = 12
    stdout.print(scores.keys(), scores["ash"])
```

A key struct with its own `def ==` is a compile error, which suggests keying by a field instead. `std.collections` also has `SortedSet`, `Grid`, `Packed`, `Deque`, `Counter` and `Tree`, and a sum can start somewhere other than zero, as in `sum(prices, start=1.5)`.

## Strings

`str` is UTF-8 text that works like Python's: f-strings with format specs, slicing, `split`, `strip`, `join`, `startswith`, `in` and the rest.

```fennec
def main(stdout: Stdout):
    name = "Ash"
    coins = 1200
    stdout.print(f"Hello, {name}! You have {coins:,} coins and {3.5:.1f} HP")

    first = name[0]                    # a one-character str
    words = "fire ice storm".split()   # ["fire", "ice", "storm"]
    clean = "  Fox ".strip().lower()
    stdout.print(first, words, clean)
```

`bytes` is a separate type for binary data, and text read from outside is checked once, when it's decoded.

## Generators and streaming

A generator is a lazy stream, used once. A function with `yield` and `(x for x in xs)` both make one, and nothing runs until the first item is asked for. So a pipeline reads like the steps it is, and it can handle input too big to hold:

```fennec
def naturals() -> Iterator[int]:
    mut n = 0
    while True:
        yield n
        n += 1

def main(stdout: Stdout):
    squares = (n * n for n in naturals())
    big = (n for n in squares if n > 50)
    for n in big:
        stdout.print(n)
        break
```

Using one twice is a compile error, `this generator has already been used`, rather than quietly giving you nothing, which is Python's trap. A second use the compiler can't see, like one in a later round of a loop, stops the program with the same message. `yield` is allowed inside `with`, and what it holds is closed when the generator's owner goes away. A failure comes out when the item that fails is asked for, and acts like a failure from any call.

`map` and `filter` could come later, though `(f(x) for x in xs if ok(x))` already says both.

## A lifetime is a block

**Being redesigned.** `with`, `def del` and how a thing is closed are still being decided, so this section may change.

`with` is how Fennec spells ownership and lifetime. A thing opened by `with` lasts for the block, and any way out of the block closes it, inner blocks first. You can read the order from the indentation, and that's the whole lifetime system: there's nothing to annotate.

```fennec planned
struct Lease:
    name: str

    def del(self):
        pass

def main(stdout: Stdout):
    with Lease(name="outer") as outer:
        with Lease(name="inner") as inner:
            stdout.print(outer.name, inner.name)
    # inner closes first, then outer
```

There's nothing special to implement: any type that defines cleanup with `def del` works with `with`. `def del` can't fail. A finish that can fail, like committing a transaction, is a method you call and check.

**Planned:** several in one line, `with a() as x, b() as y:`; a `with` block's last line as its value; and `with fs.open(path)? as file:`, since today `Fs` reads whole files with `read_text` and `read_bytes`.

## Parallel work

`parallel` is the one word for using more cores. Everything it starts ends when its loop ends, and there's no `async`, no `await` and no thread pool to manage.

```fennec
struct Particle:
    position: float
    velocity: float

def main(stdout: Stdout):
    mut particles = [Particle(position=0.0, velocity=1.5)]
    for ref particle in parallel(particles):
        particle.position += particle.velocity * 2.0

    bullets = [1, 2, 3]
    total = sum(b for b in parallel(bullets))
    hits = [b * 2 for b in parallel(bullets)]
    stdout.print(particles, total, hits)
```

A sum through `parallel` combines its parts in a fixed order, so a float sum gives the same bits however many cores it ran on.

**Planned:** `limit=`, and servers that take each connection in its own task.

```fennec planned
hits = [check(b) for b in parallel(bullets, limit=8)]

with net.listen(8080) as server:
    for conn in parallel(server):
        handle(conn, app)
```

**Do generators work with `parallel`?** Yes. A generator is read in order, and the work on its items runs in parallel.

**How do I work in parallel, then go in order?** Stream the results, then loop over them: `results = (f(x) for x in parallel(xs))`, then `for r in results:`. They arrive in input order, and a slow loop makes the workers wait. A list comprehension through `parallel` keeps input order too.

Each iteration can change only its own element, so data can't race. There'll be one scheduler, with I/O multiplexed by the OS, so a task that's waiting parks and frees its core while its code still reads as if it blocks. A bad index in one server task is meant to stay in that task; that gets designed when servers are built.

## Launch and channels

**Planned.** A long-running task is started with `launch`, which makes a channel, passes one end to your function, runs it, and hands you the other end. The block's end stops it, so a task can't leak.

```fennec planned
def physics_loop(inbox: Channel[Cmd]):
    mut sim = Sim()
    while sim.running:
        for cmd in inbox.pending():      # what's waiting, without blocking
            apply(ref sim, cmd)
        step(ref sim)

with launch(physics_loop) as physics:
    physics.send(Cmd.Push(id, force))
```

Channels are bounded ring buffers with `send`, `pending()` and `for x in ch`. You choose what a full one does when you make it: `.Wait`, the default, `.DropOldest` or `.Fail`. The rule of thumb is that a long-running loop should never wait on another, since deadlock needs someone waiting.

Event channels are planned too, for many listeners each reading at its own pace. Their exact API isn't decided yet.

## Sharing

**Being redesigned.** `shared` is built, but parts of it are still being decided, so this section may change.

Most programs share nothing. For the few things that really are shared, like a cache or a server's stats, `shared Name:` is Fennec's one reference type. Its methods lock automatically: `ref self` writes and `self` reads.

```fennec
struct Page:
    text: str

shared Cache:
    _pages = dict[str, Page]()

    def get(self, key: str) -> Page?:
        self._pages.get(key)

    def set(ref self, key: str, page: Page):
        self._pages[key] = page
```

Calling `cache.set(key, page)` takes the lock for that call. To change several handles together, one `with` takes them all at once:

```fennec
shared Account:
    total = 0

def transfer(source: Account, destination: Account, amount: int):
    with source as ref from_account, destination as ref to_account:   # both locked for the block
        from_account.total -= amount
        to_account.total += amount
```

It can hold another `shared` type, but never in a cycle, which is a compile error, and locks are taken outer type first, so nesting can't deadlock. A lock body can do I/O, which only makes it slow, and can leave early; the lock is released on the way out. There's no module-level `shared`: a handle in a signature acts like a capability, so a function's parameters show what it can reach. And a search for `shared` finds every shared thing in your program.

## Visibility

`export` marks what other kits can use, and it goes on top-level things only. Inside a kit, every file sees everything. A big program that wants walls between its parts splits into local kits.

```fennec
export struct Score:
    points = 0

export def best(scores: list[Score]) -> int:
    max(score.points for score in scores)
```

A `_name` field or method is hidden from everything but its type's own methods.

## Interfaces and generics

A type matches an interface by having its methods. There's nothing to declare.

```fennec
from std.math import PI

interface Shape:
    def area(self) -> float

struct Circle:
    radius: float

    def area(self) -> float:
        PI * self.radius * self.radius     # Circle is a Shape because it has area()

def total_area[T: Shape](shapes: list[T]) -> float:
    sum(shape.area() for shape in shapes)
```

For a collection of different kinds of things, use a union or an `enum`. A union mixes types you already have, and an enum defines new cases. Either way each item is stored inline with a one-byte tag, never boxed.

```fennec
from std.math import PI

struct Circle:
    radius: float

    def area(self) -> float:
        PI * self.radius * self.radius

struct Square:
    side: float

    def area(self) -> float:
        self.side * self.side

type Shape = Circle | Square

def report(shapes: list[Shape], stdout: Stdout):
    stdout.print(sum(s.area() for s in shapes))   # every member has area(), so Shape does
    for s in shapes:
        if s is Circle:
            stdout.print(s.radius)                 # s is a Circle here
```

```fennec
enum Pickup:
    Coin(value: int)
    Heart(amount: int)

def worth(pickup: Pickup) -> int:
    match pickup:
        case .Coin(value): value
        case .Heart(amount): amount * 10
```

## Compile-time loops and serialization

**Planned.** Instead of macros or `derive`, a generic function can loop over a type at compile time, and that's all. `fields(T)` gives a struct's fields, `cases(T)` an enum's cases, and `*Cs` a list of types. Each set of types gets its own specialized copy, and the checker sees ordinary code. There's no I/O at compile time and no generating code from text, so a kit can't run anything on your machine while it compiles.

```fennec planned
def write[T, W: Writer](x: T, ref out: W):
    for f in fields(T):
        out.field(f.name)
        write(f.get(x), ref out)
```

That's what serialization will be built on. Each format implements a small `Writer` and `Reader`, and one generic function breaks any type down, so a struct is exactly what's on the wire, with no per-format overrides. A field may be named any word, keywords included, and case style is an option on the call:

```fennec
from std import json

struct Account:
    type: str
    display_name: str

def main(stdout: Stdout):
    stdout.print(json.dumps(Account(type="admin", display_name="Ash"), names=.camel_case))
    # {"type": "admin", "displayName": "Ash"}
```

Every field is saved, private `_` ones included, and `where` rules check a value as it loads. `@skip` leaves a field out, and it needs a default so loading still builds a whole value. A key the struct doesn't have is an error unless the call says `extra=.ignore`, and `@serialize(name="uid")` renames a field when an API's name isn't yours.

`std.pack` is the default binary format, written in plain Fennec: no field names on the wire, and a number for the layout's shape at the start, so loading into a type of another shape fails at once. **Planned:** `where` ranges that shrink values, delta encoding and opt-in quantization. Annotations like `@serialize(name="uid")` are data read at compile time, and they never change what they're on.

A `Table` is saved as a list of its items in order, with each `Id` as where its item is in that list, and loading makes a new `Table` whose `Id`s find the same items. JSON, TOML and `std.pack` all work today, including typed reading:

```fennec
from std import json

struct Settings:
    volume = 0.8

def main(stdout: Stdout):
    text = json.dumps(Settings())
    loaded = json.loads[Settings](text) else Settings()
    stdout.print(text, loaded)
```

## Events and callbacks

With no references, a callback can't hold on to the world and change it later. Instead, handlers return what should happen, and the main loop applies it:

```fennec
enum Command:
    Damage(target: int, amount: int)

def on_explosion(healths: list[int]) -> list[Command]:
    [.Damage(target=i, amount=50) for i, health in enumerate(healths) if health > 0]

def main(stdout: Stdout):
    mut healths = [100, 0]
    for command in on_explosion(healths):
        match command:
            case .Damage(target, amount):
                healths[target] -= amount
    stdout.print(healths)
```

Every change happens in one visible place, which also makes replays and networking simpler.

Functions are still values you can pass and store, as callbacks, dicts of handlers, or the routes a web kit might take with `router.get("/users", list_users)`. The whole program is compiled, so every function such a value could hold is known at compile time, and choosing one at run time is a branch. There's no `dyn` and no code the compiler hasn't seen. Use a function value for one answer now, and a channel for many parties later.

## Capabilities

A function can only touch the outside world through what it's given. Without an `Fs` parameter, it can't read files. Without a `Clock`, it can't read the time.

```fennec
def load_save(fs: Fs, clock: Clock, slot: str) -> str:
    fs.read_text(slot)?                 # can read files and the time

def parse_save(data: str) -> list[str]:
    data.split()                        # pure: no I/O possible
```

`clock.now()` reads a monotonic clock, for measuring, and `clock.time()` reads the wall clock as a `Time` in UTC, for saving and showing. A `Time` prints as RFC 3339 text, like `2026-11-06T21:55:56Z`, and `Time.parse` reads it back. A `Duration` prints the way it's written, like `1h30min`.

Signatures show what code can do. `Stdout`, `Fs`, `Clock`, `Args`, `Stdin`, `Process`, `Entropy`, `Net` and `Env` work today. `Process` runs other programs, with their arguments always a list, `Entropy` gives secure random bytes, `Net` makes TCP connections, and `Env` reads environment variables, with `env.get("HOME")` giving a `str?`.

## Tests

Tests live next to the code they test.

```fennec
def clamped(health: int) -> int:
    max(health, 0)

test "clamping stops at zero":
    assert clamped(-5) == 0

test "clamping never goes below zero" for n: int:
    assert clamped(n) >= 0
```

The second test is a property test: Fennec generates inputs and shrinks a failure to the smallest example. `fennec run` runs the tests a change affects before it runs your program, and `fennec test` skips the ones whose code hasn't changed.

A test names the capabilities it needs, like `main`, and gets fakes: an in-memory `Fs`, a fixed `Clock` and captured `Stdout`, fresh for each test.

```fennec
def save_score(fs: Fs, stdout: Stdout, score: int):
    fs.write_text("score.txt", str(score))?
    stdout.print("saved")

test "saves the score"(fs: Fs, stdout: Stdout):
    save_score(fs, stdout, 120)?
    assert fs.read_text("score.txt")? == "120"
    assert stdout.captured() == "saved\n"
```

Sleeping on the fixed clock moves it on at once, and its `clock.time()` starts at `2000-01-01T00:00:00Z`. `stdout.captured()` works only in a test's own body. `fennec test --real` gives tests the real capabilities. A scripted `Net` is planned.

## Performance controls

`@hot` on a function means prove it or don't compile. The compiler has to show the function and everything it calls does no allocation, makes no copies and keeps no checks it can't prove away, like an index it can't place inside its list, or it's a compile error naming the line and why.

```fennec
struct Particle:
    position: float
    velocity: float

@hot
def update(ref particles: list[Particle], dt: float):
    for ref particle in particles:
        particle.position += particle.velocity * dt
```

Floats have one equality, order and hash: NaN equals itself and sorts last, and `0.0` equals `-0.0`. There are no fused operations, so results stay the same on every machine. **Planned:** `@fast_math`, which opts one function out of that, and `a.approx(b, within=...)`, which compares computed values with a tolerance.

## C and unsafe code

**Planned.** Fennec is safe by default, and libraries are written in Fennec wherever possible. Calling C is for what can't be: the operating system, GPU drivers and platform SDKs. It's the one place unsafe code is allowed, and it's fenced in:

```fennec planned
extern "C":
    struct Sqlite3:
        _opaque: u8

    def sqlite3_close(db: owned ptr[Sqlite3]) -> i32

export struct Database:
    handle: owned ptr[Sqlite3]

    def del(self):
        unsafe("handle came from sqlite3_open and is closed only here"):
            sqlite3_close(self.handle)
```

- Pointers and `unsafe` are only allowed in files with an `extern` block, and every call into C states its reason.
- Exported functions can't expose pointers, so callers only ever see a safe API.
- A C callback is a plain top-level function that gets an `Id` through user data, so C never holds anything that can go wrong.
- How a native handle is closed, here with `def del`, is part of the [lifetime redesign](#a-lifetime-is-a-block).
- For now, only the Fennec project publishes native kits. Your own app can still use `extern` in its own code.
