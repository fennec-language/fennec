---
layout: ../layouts/Doc.astro
title: A tour of Fennec
description: A walk through Fennec's design, from values and tables to lifetimes, streaming, errors and sharing, with what's built and what's planned marked as you go.
---

# A tour of Fennec

<p class="lede">If you can read Python, you can read Fennec. What's different is underneath: data is plain values in flat tables, types are checked before anything runs, errors can't be ignored, and there are no references to manage.</p>

> **What you can trust on this page.** Fennec is in progress. Every plain code sample here compiles today. Anything that doesn't yet sits under a **Planned** label, and that part of the design is decided but not built, so don't expect it to run.

## Hello

```fennec
def main(stdout: Stdout):
    stdout.print("Hello, Fennec")
```

`main` receives what the program is allowed to touch, here standard output. There's no global `print`: code can only write output if it's given somewhere to write it. More on that in [capabilities](#capabilities).

## Names

A name is immutable unless you declare it `mut`. There's no `let` or `const`, because those two forms already say everything.

```fennec
name = "Ash"
mut health = 100
health -= 10
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

A literal default gives the parameter its type: an `int`, `float`, `str` or `bool`, or an enum case like `mode=Mode.fast`. Writing `limit: int = 8` is an error whose fix drops the `: int`. The type is written when the default is `None`, an empty `[]` or `{}`, or isn't the literal's own type.

**Planned:** hard errors for the positional calls that are easy to get backwards: a bare `True` or `False`, neighboring parameters of the same type, and three or more positional arguments unless no two neighbors could be swapped and still compile. The first argument and `self` are exempt.

## Built-in types

| Type | Example |
| --- | --- |
| `int`, `float` | `42`, `3.14` (64-bit) |
| Sized numbers | `i32`, `u8`, `f32` when you want control over size |
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
from std.game import Id, IdMap

struct Unit:
    health = 100
    target: Id[Unit]? = None

def main(stdout: Stdout):
    mut units = IdMap[Unit]()
    ash = units.insert(Unit())
    vex = units.insert(Unit(target=ash))
    units[ash].health -= 10
    stdout.print(units[ash].health, units[vex].target == ash)

    units.remove(ash)
    stdout.print(units.get(ash))      # None: a stale Id finds nothing
```

An `Id` is a small plain value with a generation in it, so it stays safe after its slot is reused, and a cycle of them can't leak. `Tree`, in `std.collections`, does the same bookkeeping for parents and children.

Assigning or passing a value never lets anyone change yours. This doesn't mean everything gets copied: the compiler passes values by reference when it's safe, moves them when the original isn't used again, and shares large data until one side changes it. You write simple value code, and the compiler does the rest.

Nested structs are stored inline, so editing one is a single write to memory:

```fennec
struct Stats:
    health = 100

struct Unit:
    stats: Stats

def main(stdout: Stdout):
    mut units = [Unit(stats=Stats())]
    units[0].stats.health -= 10
    stdout.print(units[0].stats.health)
```

`mut x = place` names the real place, so writing through it edits the original, as you'd expect from Python:

```fennec planned
mut unit = world.units[id]
unit.stats.health -= 10          # edits the unit in the table

best = unit                      # replacing a name makes a new value
mut backup = copy(unit)          # a separate value, on purpose
```

Writing through a name and replacing it on the same name is a compile error. So is reading what the name came from while the name is still in use. The name can't be stored, returned or captured, so there are no lifetimes to write.

Planned: `mut a, b = xs.pair(i, j)` for two elements of one list, and a compile error for a copy that can quietly go stale:

```fennec planned
world.selected = world.units[i]          # error: edits to one won't reach the other
world.selected = copy(world.units[i])    # fine: a separate copy, on purpose
```

## Mutation is visible

Parameters are read-only. A function that changes its argument says `mut` in its signature, and every call to it says `mut` too.

```fennec
struct Player:
    health = 100

def apply_damage(mut player: Player, amount: int):
    player.health -= amount

def main(stdout: Stdout):
    mut player = Player()
    apply_damage(mut player, 10)   # you can see player changes here
    stdout.print(player)           # and that it doesn't change here
```

Reading any call, you know whether it can modify your data without opening the function. Methods and loops say it the same way:

```fennec
struct Enemy:
    health: int

    def take_damage(mut self, amount: int):
        self.health -= amount

def main(stdout: Stdout):
    mut enemies = [Enemy(health=3)]
    for mut enemy in enemies:
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
health = find_target(enemies).health else 1     # 1 if there's no target
score = scores.get(team).get(player) else 0
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

**Planned: errors in the signature.** Today the errors a function can fail with are inferred. In the locked design, a function says so with `fails Name`, where one `error` can list several cases, so a signature stays short. `fennec fix` will write it for you.

```fennec planned
error LoadError:
    NotFound(path: str)
    BadRow(line: int)

def load(path: str) -> float fails LoadError:
    ...
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

**Planned:** a compile error for a key struct with its own `def ==`, suggesting you key by a field instead; `SortedSet`, `Grid` and `Packed`; and `sum(wallet, start=Money(cents=0))`, which replaces today's positional start.

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

Using one twice fails loudly with `this generator has already been used`, rather than quietly giving you nothing, which is Python's trap. `yield` is allowed inside `with`, and what it holds is closed when the generator's owner goes away. A failure comes out when the item that fails is asked for, and acts like a failure from any call.

**Planned:** a compile error for a second use, wherever that can be proved. `map` and `filter` could come later, though `(f(x) for x in xs if ok(x))` already says both.

## A lifetime is a block

`with` is how Fennec spells ownership and lifetime. A thing opened by `with` lasts for the block, and any way out of the block closes it, inner blocks first. You can read the order from the indentation, and that's the whole lifetime system: there's nothing to annotate.

```fennec
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

`parallel` is the one word for using more cores. Reading through it compiles today, but it still runs in order on one core:

```fennec
def main(stdout: Stdout):
    bullets = [1, 2, 3]
    total = sum(b for b in parallel(bullets))
    hits = [b * 2 for b in parallel(bullets)]
    stdout.print(total, hits)
```

**Planned:** actually using your cores, `for mut` through it, streams and `limit=`. Everything it starts ends when its loop ends, and there's no `async`, no `await` and no thread pool to manage.

```fennec planned
for mut particle in parallel(particles):
    particle.position += particle.velocity * dt

hits = [check(b) for b in parallel(bullets, limit=8)]

with net.listen(8080) as server:
    for conn in parallel(server):
        handle(conn, app)
```

Each iteration can change only its own element, so data can't race. There'll be one scheduler, with I/O multiplexed by the OS, so a task that's waiting parks and frees its core while its code still reads as if it blocks. A bad index in one server task is meant to stay in that task; that gets designed when servers are built.

## Launch and channels

**Planned.** A long-running task is started with `launch`, which makes a channel, passes one end to your function, runs it, and hands you the other end. The block's end stops it, so a task can't leak.

```fennec planned
def physics_loop(inbox: Channel[Cmd]):
    mut sim = Sim()
    while sim.running:
        for cmd in inbox.pending():      # what's waiting, without blocking
            apply(mut sim, cmd)
        step(mut sim)

with launch(physics_loop) as physics:
    physics.send(Cmd.Push(id, force))
```

Channels are bounded ring buffers with `send`, `pending()` and `for x in ch`. You choose what a full one does when you make it: `.Wait`, the default, `.DropOldest` or `.Fail`. The rule of thumb is that a long-running loop should never wait on another, since deadlock needs someone waiting.

## Sharing

**Planned.** Most programs share nothing. For the few things that really are shared, like a cache or a server's stats, `shared Name:` is Fennec's one reference type. Its methods lock automatically: `mut self` writes and `self` reads.

```fennec planned
shared Cache:
    _pages = dict[str, Page]()

    def get(self, key: str) -> Page?:
        self._pages.get(key)

    def set(mut self, key: str, page: Page):
        self._pages[key] = page
```

Calling `cache.set(key, page)` takes the lock for that call. To change several handles together, `with` locks them all, always in the same order, so two transfers can't deadlock:

```fennec planned
shared Account:
    total = 0

def transfer(a: Account, b: Account, amount: int):
    with a, b as mut x, mut y:               # both locked for the block
        x.total -= amount
        y.total += amount
```

It holds plain values and `Id`s, never another `shared`, so handles can't form a cycle. Lock bodies can't do I/O, and there's no module-level `shared`: a handle in a signature acts like a capability, so a function's parameters show what it can reach. And a search for `shared` finds every shared thing in your program.

## Visibility

`pub` marks what other kits can use, and it goes on top-level things only. Inside a kit, every file sees everything. A big program that wants walls between its parts splits into local kits.

```fennec
pub struct Score:
    points = 0

pub def best(scores: list[Score]) -> int:
    max(score.points for score in scores)
```

**Planned:** `_name` hiding a field from everything but its type. Today a `_name` field is reachable like any other, and fields can still be marked `pub` one at a time.

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

For a collection of different kinds of things, use an `enum`.

## Compile-time loops and serialization

**Planned.** Instead of macros or `derive`, a generic function can loop over a type at compile time, and that's all. `fields(T)` gives a struct's fields, `cases(T)` an enum's cases, and `*Cs` a list of types. Each set of types gets its own specialized copy, and the checker sees ordinary code. There's no I/O at compile time and no generating code from text, so a kit can't run anything on your machine while it compiles.

```fennec planned
def write[T, W: Writer](x: T, mut out: W):
    for f in fields(T):
        out.field(f.name)
        write(f.get(x), mut out)
```

That's what serialization is built on. Each format implements a small `Writer` and `Reader`, one generic function breaks any type down, and a type overrides one format with a method named after it, leaving the others alone:

```fennec planned
struct Color:
    r: int
    g: int
    b: int

    def json(self, mut out: JsonWriter):
        out.str(f"#{self.r:02x}{self.g:02x}{self.b:02x}")
```

`std.pack` will be the default binary format: no field names on the wire, `where` ranges shrink values, a schema hash, delta encoding and opt-in quantization, all in plain Fennec. Annotations like `@json(name="user_id")` are data read at compile time, and they never change what they're on.

JSON and TOML already work today, including typed reading:

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
    [.Damage(i, 50) for i, health in enumerate(healths) if health > 0]

def main(stdout: Stdout):
    mut healths = [100, 0]
    for command in on_explosion(healths):
        match command:
            case .Damage(target, amount):
                healths[target] -= amount
    stdout.print(healths)
```

Every change happens in one visible place, which also makes replays and networking simpler.

## Capabilities

A function can only touch the outside world through what it's given. Without an `Fs` parameter, it can't read files. Without a `Clock`, it can't read the time.

```fennec
def load_save(fs: Fs, clock: Clock, slot: str) -> str:
    fs.read_text(slot)?                 # can read files and the time

def parse_save(data: str) -> list[str]:
    data.split()                        # pure: no I/O possible
```

Signatures show what code can do. `Stdout`, `Fs`, `Clock`, `Args` and `Stdin` work today. `Net`, `Env` and `Process` are planned.

## Tests

Tests live next to the code they test.

```fennec
def add(a: int, b: int) -> int:
    a + b

test "adding works":
    assert add(2, 3) == 5

test "adding zero changes nothing" for n: int:
    assert add(n, 0) == n
```

The second test is a property test: Fennec generates inputs and shrinks a failure to the smallest example. `fennec run` runs the tests a change affects before it runs your program, and `fennec test` skips the ones whose code hasn't changed.

A test names the capabilities it needs, like `main`, and gets fakes: an in-memory `Fs`, a fixed `Clock` and captured `Stdout`, fresh for each test.

```fennec
test "saves the score"(fs: Fs, stdout: Stdout):
    save_score(fs, stdout, 120)?
    assert fs.read_text("score.txt")? == "120"
    assert stdout.captured() == "saved\n"
```

Sleeping on the fixed clock moves it on at once, and `stdout.captured()` works only in a test's own body. `fennec test --real` gives tests the real capabilities. A scripted `Net` is planned, for when `Net` exists.

## Performance controls

**Planned.** `@hot` on a function means prove it or don't compile. The compiler has to show the function and everything it calls does no allocation, makes no copies and has no leftover count checks, or it's a compile error naming the line.

```fennec planned
@hot
def update(mut particles: list[Particle], dt: f32):
    for mut particle in particles:
        particle.position += particle.velocity * dt
```

`@fast_math` opts one function out of Fennec's deterministic floats. Without it, there are no fused operations, so results stay the same on every machine. Floats will also have one equality, order and hash: NaN equals itself and sorts last, `0.0` equals `-0.0`, and `a.approx(b, within=...)` compares computed values with a tolerance.

## C and unsafe code

**Planned.** Fennec is safe by default, and libraries are written in Fennec wherever possible. Calling C is for what can't be: the operating system, GPU drivers and platform SDKs. It's the one place unsafe code is allowed, and it's fenced in:

```fennec planned
extern "C":
    struct Sqlite3:
        _opaque: u8

    def sqlite3_close(db: owned ptr[Sqlite3]) -> i32

pub struct Database:
    handle: owned ptr[Sqlite3]

    def del(self):
        unsafe("handle came from sqlite3_open and is closed only here"):
            sqlite3_close(self.handle)
```

- Pointers and `unsafe` are only allowed in files with an `extern` block, and every call into C states its reason.
- `pub` functions can't expose pointers, so callers only ever see a safe API.
- A C callback is a plain top-level function that gets an `Id` through user data, so C never holds anything that can go wrong.
- For now, only the Fennec project publishes native kits. Your own app can still use `extern` in its own code.
