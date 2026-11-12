---
layout: ../layouts/Doc.astro
title: A tour of Fennec
description: A walk through the whole of Fennec, from values and tables to failures, parallel work, sharing and tasks, with what's built and what's planned marked as you go.
---

# A tour of Fennec

<p class="lede">If you can read Python, you can read Fennec. What's different is underneath: everything is a plain value, types are checked before anything runs, failures can't be ignored, and there are no references, lifetimes or <code>unsafe</code> to manage.</p>

> **What you can trust on this page.** Fennec is in progress. Every plain code sample here compiles today, and a test checks that it does. Anything that doesn't yet sits under a **Planned** label: that part of the design is decided but not built, so don't expect it to run.

To build a real program, read [the guide](https://github.com/fennec-language/fennec/blob/main/docs/guide.md), which writes a small source control tool from start to finish.

## Hello

```fennec
def main(stdout: Stdout):
    stdout.print("Hello, Fennec")
```

`main` receives what the program is allowed to touch, here standard output. There's no global `print`: code can only write output if it's given somewhere to write it. More on that in [capabilities](#capabilities).

## Four rules

Most of Fennec follows from four rules:

- A name holds its own value.
- `mut` lets a name change.
- To change something in a list, write to it there.
- `ref` lets a loop or a function change yours.

```fennec
struct Unit:
    name: str
    health = 100

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

A plain name never quietly links to something else. **Planned:** a local `ref target = units[1]`, for several changes to one place in a row. It lasts until its last use, and while it's alive nothing else reaches `units`.

## Names

A name can't change unless you declare it `mut`. There's no `let` or `const`, because those two forms already say everything.

```fennec
def main(stdout: Stdout):
    name = "Ash"
    mut health = 100
    health -= 10
    stdout.print(name, health)
```

Writing `name = "Vex"` next would be a compile error, which tells you to declare it `mut name = "Ash"`.

A name that can't change is set once, by a single expression. When the value depends on a condition, use a conditional expression, or `match` for more than two choices:

```fennec
enum Kind:
    Runner
    Walker
    Turret

def speed_of(kind: Kind) -> float:
    match kind:
        case .Runner: return 8.0
        case .Walker: return 3.0
        case .Turret: return 0.0

def label_of(health: int) -> str:
    return "boss" if health > 500 else "minion"
```

Names belong to the block they're made in. A name made inside an `if`, `for` or `with` doesn't exist after it, so nothing leaks into the rest of the function.

A module can hold constants, worked out while compiling. There's no `mut` at the top level, so there's no global state.

```fennec
KEYWORDS = {"def", "if", "else"}

def is_keyword(word: str) -> bool:
    return word in KEYWORDS
```

## Functions

Parameters and return types are written, and everything inside a function is inferred. A function gives its value with `return`.

```fennec
from std.math import PI

def area(radius: float) -> float:
    return PI * radius * radius

def factorial(n: int) -> int:
    return 1 if n <= 1 else n * factorial(n - 1)
```

- `return` can leave early too. A function with no `->` returns nothing. If you leave a return type out, the error's fix writes in the one the compiler inferred.
- Short functions are passed with an arrow: `enemies.sort(key=enemy => enemy.health)`. An arrow is one expression, and anything longer is a nested `def`.
- Functions used as values never change anything around them. A nested function that needs to change something takes a `ref` parameter.

```fennec
def counts(words: list[str]) -> dict[str, int]:
    def count(word: str, ref seen: dict[str, int]):
        seen[word] = (seen.get(word) else 0) + 1

    mut seen = dict[str, int]()
    for word in words:
        count(word, ref seen)
    return seen
```

## Named arguments

A parameter with a default is passed by name only, and a literal default gives it its type. Defaults are plain values worked out at compile time, so Python's shared-mutable-default surprise can't happen.

```fennec
def scan(rows: list[int], limit=8) -> list[int]:
    return rows[:limit]

def main(stdout: Stdout):
    stdout.print(scan([1, 2, 3]), scan([1, 2, 3], limit=1))
```

- A literal default is an `int`, `float`, `str` or `bool`, or an enum case. Writing `limit: int = 8` is an error whose fix drops the `: int`. The type is written when the default is `None`, as in `target: Unit? = None`.
- An empty collection's default is its constructor: `items = list[str]()`. Inside a function body, `items = []` is fine.
- Positional calls that are easy to get backwards are compile errors: a bare `True` or `False`, and neighboring arguments of the same type, which could be swapped and still compile. The error says which one to name. The first argument and `self` are exempt.

```fennec
struct Account:
    total = 0

def transfer(source: Account, destination: Account, amount: int) -> list[Account]:
    return [copy(source, total=source.total - amount), copy(destination, total=destination.total + amount)]

def main(stdout: Stdout):
    savings = Account(total=10)
    checking = Account()
    stdout.print(transfer(savings, destination=checking, amount=3))
```

## Built-in types

| Type | Example |
| --- | --- |
| `int`, `float` | `42`, `3.14` (64-bit) |
| Sized numbers | `pixel: u8 = 255`, `low: i8 = -128`, `speed: f32 = 1.5`, from `i8` to `u64` and `f32`, checked against their size |
| `bool` | `True`, `False` |
| `str` | `"fennec"`, `f"{count} foxes"`, `r"\d+"` |
| `bytes` | `bytes([137, 80])`, `"hi".encode()` |
| `list`, `dict`, `set` | `[1, 2]`, `{"hp": 10}`, `{1, 2}` |
| `tuple` | `(3, 4)` |
| `array[T, N]` | `array[float, 16]()`, held inline |
| Durations | `30s`, `200ms`, `1h30min` |

Built-in types are lowercase. Everything you define or import is capitalized. Integer overflow is checked everywhere, release builds included.

## Structs

A struct is a set of named fields, like a Python dataclass. You get a constructor automatically, and fields can have defaults. A field with a default takes its type from it, so only fields without one need a type. Fields with defaults come after the ones without, as parameters do.

```fennec
struct Config:
    name: str
    volume = 0.8
    fullscreen = False

def main(stdout: Stdout):
    config = Config(name="game", volume=0.5)
    stdout.print(config)
```

A method without `self` is a named constructor, for anything the automatic one can't do:

```fennec
from std.math import Vec3

struct Enemy:
    position: Vec3
    health = 100

    def at(position: Vec3) -> Enemy:
        return Enemy(position=position)

def main(stdout: Stdout):
    boss = Enemy.at(Vec3(x=1.0, y=2.0, z=3.0))
    stdout.print(boss.health)
```

Rules about a field's value go right next to it with `where`. They're checked on every write, and look only at their own field. An invariant across several fields goes in a named constructor or a method.

```fennec
struct Player:
    level: int where level >= 1

def main(stdout: Stdout):
    player = Player(level=1)
    stdout.print(player)
```

Equality, hashing and printing work automatically for any type whose fields support them. There's nothing to derive. `extend` adds methods to one of your kit's types from another file:

```fennec
struct Square:
    side: float

extend Square:
    def area(self) -> float:
        return self.side * self.side
```

## Copies

`copy(x, field=...)` makes a changed copy, and runs the `where` rules again. A plain `=` covers everything else, and the compiler skips the real copy when nobody can tell.

```fennec
struct Person:
    name: str

def main(stdout: Stdout):
    people = [Person(name="ana"), Person(name="bo")]
    shouting = [copy(person, name=person.name.upper()) for person in people]
    stdout.print(shouting)
```

A plain parameter can't change, so passing something huge costs nothing.

## Fixed-size arrays

`array[T, N]` holds exactly `N` items where the array is, with no block of its own. It's for math types and small buffers inside structs.

```fennec
struct Item:
    name: str

struct Mat4:
    m: array[float, 16]

struct Bag:
    # every slot starts as None
    slots = array[Item?, 20]()

def main(stdout: Stdout):
    identity: array[float, 4] = [1.0, 0.0, 0.0, 1.0]
    bag = Bag()
    stdout.print(len(identity), identity[3], bag.slots[0])
```

Write one as a list of exactly `N` items, or as `array[T, N]()`, which fills it with the item's zero. It indexes, loops with `for` and `for ref`, and has `len`, `in` and `==`. Its size never changes, and `list(arr)` makes a list from it. A wrong count, and a position known to be outside it, are compile errors.

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

    _ = units.remove(ash)
    # None: a stale Id finds nothing
    stdout.print(units.get(ash))
```

An `Id` is an index, a generation and a tag for its table, so it finds the right thing or nothing. `units[id]` stops the program on a dead `Id`, and `units.get(id)` gives an optional, for when the item may be gone. `Tree`, in `std.collections`, does the same bookkeeping for parents and children. There are no classes, so this is how graphs and trees are done.

Assigning or passing a value never lets anyone change yours. That doesn't mean everything gets copied: the compiler passes values without copying when it's safe, and shares large data until one side changes it. Nested structs are stored inline, so editing one is a single write to memory.

```fennec
struct Stats:
    health = 100

struct Unit:
    stats: Stats

def main(stdout: Stdout):
    mut units = [Unit(stats=Stats())]
    # edits the unit in the list
    units[0].stats.health -= 10

    # your own copy
    mut backup = units[0]
    backup.stats.health = 0
    stdout.print(units[0].stats.health, backup.stats.health)
```

**Planned:** a lint for a copy that can quietly go stale, asking whether a `ref` was meant:

```fennec planned
# lint: edits to one won't reach the other; a `ref` changes the original
world.selected = world.units[i]
```

## Mistakes the compiler stops

Each of these is a compile error that says what to write instead:

```text
target = units[target_id]
target.health -= 10
# error: `target` isn't mut
#        to change `units`: write to `units[target_id].health`

mut target = units[target_id]
target.health -= 10
# error: this changes your own copy of what's in `units`, and nothing reads it

for unit in units:
    units[0].health -= 1
# error: you can't change `units` while looping over it
#        use `for ref`, or loop over indexes
```

- A copy made on every round of a loop is an error too, naming the line and why the value is shared. A copy made once, on the first write, is only a hint.
- `dict[str, int?]` is an error: leave the key out, since not being in a dict already means missing. The same goes for optional keys and set items.
- One silent mistake remains: changing a `mut` copy and then reading it, believing the list changed. A rule gets added for that only if testing shows real bugs.

## Mutation is visible

Parameters are read-only. A function that changes its argument says `ref` in its signature, and every call to it says `ref` too.

```fennec
struct Player:
    health = 100

def apply_damage(ref player: Player, amount: int):
    player.health -= amount

def main(stdout: Stdout):
    mut player = Player()
    # you can see player change here
    apply_damage(ref player, 10)
    stdout.print(player)
```

Reading any call, you know whether it can change your data without opening the function. A `ref` needs a `mut` name or another `ref` to start from, and it can't be stored, returned or captured, so there are no lifetimes to write. Two ways to reach the same thing at once, like `f(ref x, ref x)`, are refused. Methods and loops say it the same way:

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

An `enum` is one of several cases, each with its own data. `match` must handle every case, and `case` takes or-patterns and guards, as in Python.

```fennec
from std.math import PI

enum Shape:
    Circle(radius: float)
    Rect(width: float, height: float)
    Dot

def area(shape: Shape) -> float:
    match shape:
        case .Circle(radius): return PI * radius * radius
        case .Rect(width, height): return width * height
        case .Dot: return 0.0

def kind(shape: Shape) -> str:
    match shape:
        case .Circle(radius) if radius > 10.0: return "a big circle"
        case .Circle(_) | .Dot: return "round"
        case .Rect(_, _): return "square-ish"
```

The leading dot marks an enum case, and it's required when the type is known. You only write the enum's name if the type isn't known where you write it.

## Unions

A union mixes types that already exist, where an `enum` defines new cases. Each value is held in place with its member's tag, never boxed.

```fennec
from std.math import PI

struct Circle:
    radius: float

    def area(self) -> float:
        return PI * self.radius * self.radius

struct Square:
    side: float

    def area(self) -> float:
        return self.side * self.side

type Shape = Circle | Square

def report(shapes: list[Shape], stdout: Stdout):
    # every member has area(), so Shape does
    stdout.print(sum(shape.area() for shape in shapes))
    for shape in shapes:
        if shape is Circle:
            # a Circle here
            stdout.print(shape.radius)
```

- `if x is Circle:` narrows `x` inside the block, and `match` handles each member, checked for exhaustiveness. A method every member has is the union's too.
- `None` is never a member: `str | None` is an error whose fix writes `str?`, and an optional union is `(A | B)?`.
- Failures stay separate: a function that can fail returns `-> T fails E`, never `-> T | E`.

## Missing values

A value that might be missing has a `?` on its type: `Enemy?` holds either an `Enemy` or `None`. The compiler won't let you use it until you've checked.

```fennec
struct Enemy:
    health = 3

def find_target(enemies: list[Enemy]) -> Enemy?:
    if len(enemies) == 0: return None
    return enemies[0]

def main(stdout: Stdout):
    target = find_target([Enemy()])
    if target is None:
        return
    # the compiler knows it's an Enemy here
    stdout.print(target.health)
```

A chain that ends in `else` can read through something that might be missing. It stops at the first missing step and gives the default, or leaves, with `else return`, `else continue`, `else break` or `else fail`.

```fennec
struct Enemy:
    health = 3

def find_target(enemies: list[Enemy]) -> Enemy?:
    if len(enemies) == 0: return None
    return enemies[0]

def main(stdout: Stdout):
    # 1 if there's no target
    health = find_target([]).health else 1
    scores = {"red": {"ash": 12}}
    score = scores.get("red").get("vex") else 0
    stdout.print(health, score)
```

`else` covers only the chain right before it, so a larger expression needs brackets: `(find_target(enemies).health else 1) + 1`. Without the `else`, it's a compile error that suggests one or an `is None` check.

Taking from an empty collection gives `None`, so draining a list reads naturally:

```fennec
def main(stdout: Stdout):
    mut todo = [1, 2, 3]
    while True:
        next = todo.pop() else break
        stdout.print(next)
```

## Failures

There are no exceptions. A failure is a value a function returns, and you can't use the result without dealing with it. A function that can fail says so with `fails Name`, where one `error` can list several cases.

```fennec
struct Item:
    name: str

error SellError:
    NotFound(item: Item)
    OutOfStock(item: Item)

struct Shop:
    _items = dict[Item, int]()

    def sell(ref self, item: Item) -> Item fails SellError:
        count = self._items.get(item) else fail .NotFound(item)
        if count == 0:
            fail .OutOfStock(item)
        self._items[item] = count - 1
        return item
```

`fail` returns a failure, and `?` passes one up to your caller, so you can see every line a function can leave from. There are a few ways to deal with a call that can fail:

```fennec
error NotFound:
    path: str

def load_score(path: str) -> int fails NotFound:
    if path == "":
        fail NotFound(path)
    return 10

def doubled(path: str) -> int fails NotFound:
    # pass the failure up
    return load_score(path)? * 2

def main(stdout: Stdout):
    # use a default if it failed
    fallback = load_score("") except NotFound(): 0
    stdout.print(fallback)

    # or name the failure in a block that leaves with return, fail, continue or break
    score = load_score("save") except why:
        stdout.print(f"no file at {why.path}")
        return
    stdout.print(score)
```

A failure is handled at the call. `?` passes it up, and `except` handles it: one case on the line, `except why:` with a block, or `case` arms under `except why:`, with `case _:` for the rest. `why` must be used on every way out, by reading it or by `fail why`. There's no `Ok`, `Err` or `Result` to store and forget, and `else` is only for a missing value.

An `except why:` block after `x =` can also give `x` a value as its last line, rather than leaving. **Planned:** the old `Ok`, `Err` and `Result` removed.

- The value after `except` is only worked out if it's needed. Failing is a plain return, with no stack unwinding.
- `fails A | B` combines failures from different places, and `?` passes any of them up as it is. A function type can say it too: `(str) -> int fails ParseError | RangeError`.
- `result.expect("why")` says a call can't fail, and needs a reason. **Planned:** `ignore` goes, and `_ = f()` on a call that can fail is a compile error, so a failure must always be handled.
- A run-time bug is different from a failure. `xs[i]` out of range stops the program with a clear message, because there's nothing sensible to handle.

- Ignoring a returned value is a compile error, and `_ = call()` throws one away on purpose.
- `xs.get(i)` gives an optional, for an index that might not be there.
- There's no `exit()`. A failure out of `main` prints to stderr and exits with code 1, and `def main(...) -> int:` exits with the `int` it gives back.


## Collections

Comprehensions build collections, and the brackets tell you something new is being made. Without brackets, a generator expression is lazy.

```fennec
struct Enemy:
    name: str
    health = 3

def main(stdout: Stdout):
    enemies = [Enemy(name="Ash"), Enemy(name="Vex", health=0)]
    # a new list
    alive = [enemy for enemy in enemies if enemy.health > 0]
    # a new dict
    by_name = {enemy.name: enemy for enemy in alive}
    # just a loop
    total = sum(enemy.health for enemy in enemies)
    strongest = max(enemies, key=enemy => enemy.health)
    stdout.print(by_name, total, strongest)
```

- Python's looping builtins come along: `sum`, `min`, `max`, `any`, `all`, `sorted`, `enumerate`, `zip`, `range`. `len(xs)` and `abs(x)` are builtins too, and other questions about one value are methods.
- A `dict` keeps insertion order, so looping over it gives the same order every time.
- `d.get(key)` returns an optional, and a fallback reads the way it does anywhere else: `d.get(key) else 0`.
- A struct works as a dict key with nothing to write. It hashes all its fields.
- A loop can change items, but not how many there are. Adding and removing happen outside it.

```fennec
from std.collections import SortedMap

struct Tile:
    x: int
    y: int

def main(stdout: Stdout):
    seen = {Tile(x=1, y=2): 1}
    stdout.print(seen.get(Tile(x=1, y=2)) else 0)

    mut scores = SortedMap[str, int]()
    scores["vex"] = 7
    scores["ash"] = 12
    stdout.print(scores.keys(), scores["ash"])
```

`std.collections` also has `SortedSet`, `Grid`, `Packed`, `Deque`, `Heap`, `PriorityQueue`, `Counter` and `Tree`, and a sum can start somewhere other than zero, as in `sum(prices, start=1.5)`.

A `Graph` holds nodes and the edges between them by `Id`, built on a `Table`, so there are no references to tangle:

```fennec
from std.collections import Graph

def main(stdout: Stdout):
    mut imports = Graph[str, str]()
    main_id = imports.add_node("main")
    api = imports.add_node("api")
    json = imports.add_node("json")
    imports.add_edge(from_id=main_id, to_id=api, edge="respond")
    imports.add_edge(from_id=api, to_id=json, edge="dumps")
    order = imports.topological_order() else []
    stdout.print([imports[id] for id in order])
```

## Strings

`str` is UTF-8 text that works like Python's: f-strings with format specs, slicing, `split`, `strip`, `join`, `startswith`, `in` and the rest.

```fennec
def main(stdout: Stdout):
    name = "Ash"
    coins = 1200
    stdout.print(f"Hello, {name}! You have {coins:,} coins and {3.5:.1f} HP")

    # a one-character str
    first = name[0]
    words = "fire ice storm".split()
    clean = "  Fox ".strip().lower()
    stdout.print(first, words, clean)
```

`bytes` is a separate type for binary data, and text read from outside is checked once, when it's decoded. **Planned:** `{x=}`, which prints `x=` and the value.

A t-string, `t"..."`, makes a `Template`, as in Python 3.14. It keeps the text you wrote apart from the values, so a value can never become part of a query, a page or a command:

```fennec
type SqlValue = int | str

def placeholders(query: Template[SqlValue]) -> str:
    mut sent = ""
    for at, text in enumerate(query.strings):
        sent += text
        if at < len(query.values):
            sent += "?"
    return sent

def main(stdout: Stdout):
    # a name someone else chose is a value, never part of the text
    name = "ash'; drop table players; --"
    query: Template[SqlValue] = t"select score from players where name = {name}"
    stdout.print(placeholders(query), query.values)
```

- A `str` is never a `Template`, so an f-string where one is wanted is a compile error.
- `LiteralStr` is text written in the source: a literal, literals joined with `+`, or a choice between them. It goes anywhere a `str` goes, but text read while the program runs is never one.

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

Using one twice is a compile error, `this generator has already been used`, rather than quietly giving you nothing, which is Python's trap. A second use the compiler can't see, like one in a later round of a loop, stops the program with the same message. A failure comes out when the item that fails is asked for, and acts like a failure from any call.

A type is loopable when it has `def iter(self) -> Iterator[T]`, written with `yield`, and `for ref` works on a type with `def len` and the `ref` form of `def []`.

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
    total = sum(bullet for bullet in parallel(bullets))
    hits = [bullet * 2 for bullet in parallel(bullets)]
    stdout.print(particles, total, hits)
```

- Each round can change only its own item, so data can't race.
- A sum through `parallel` combines its parts in a fixed order, so a float sum gives the same bits however many cores it ran on.
- A list comprehension through `parallel` keeps input order. A stream, `(f(x) for x in parallel(xs))`, arrives in input order too, while later items still run.
- A generator works as a source: it's read in order, and the work on its items runs in parallel.

A server gives each connection its own task the same way. Calls look blocking, and a task that waits frees its core:

```fennec
from std.net import Connection, NetError

def echo(conn: Connection) fails NetError:
    for line in conn.lines()?:
        conn.write(f"{line}\n")?

def main(net: Net, stdout: Stdout):
    with net.listen(9000)? as server:
        for conn in parallel(server):
            echo(conn) except error:
                stdout.print(f"{conn.peer} dropped: {error}")
                continue
            stdout.print(f"{conn.peer} left")
```

Each connection closes when its round ends, however it ends. A failure that passes up out of a round stops the server and cuts the others short, as `?` does in any loop, so handle it in the body, as `except` does here. `break` stops taking connections and lets the running rounds finish, and a crash ends only its own round.

**Planned:** `limit=` on `parallel`.

## Sharing, resources and tasks

Most programs share nothing. A typical game's main loop owns the world and passes `ref world` down.

It answers four questions, each with its own words:

| Question | Words |
| --- | --- |
| Can it change? | `mut`, `ref` |
| Who's touching it? | `shared`, `lock` |
| When does it go back? | `with` |
| Who talks to whom? | `launch`, channels |

Every borrow lives in a block you can see, so there are no lifetimes to write. A `shared Name:` declaration, and a `with` on a `shared` value, are refused, and `fennec fix` rewrites what's mechanical of them.

### `shared` and `lock`

`shared T` is a type, so it works on any type and goes in fields and collections too, like `dict[str, shared Room]`. It's reached only inside `lock`:

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
    return 0

def main(stdout: Stdout):
    shared scores = Scores()
    record(scores, "Ana", points=10)
    stdout.print(best_of(scores, "Ana"))
```

- A `shared` value is made in a function, usually `main`, and passed to whoever needs it, like a capability. There's no top-level shared state, so a function's signature says what shared state it touches.
- Any way out of a `lock` block releases it. Leaving with `return` keeps the changes made so far, and today so does `fail`. **Planned:** a `fail` or `?` leaving `lock ... as ref` rolls back the block's changes, as a crash does, so a write lock either finishes or has no effect.
- A crash inside `lock x as ref s:` rolls back: every value the block changed is put back before the lock is released, so nobody ever sees a half-change. There's nothing to write, and a lock is never left poisoned.
- Two values locked at once go in one statement, `lock source, target as ref payer, ref payee:`, and a `lock` inside another is refused for now.
- A `shared` value never holds one of its own type and never forms a cycle, so a tree uses a `Table` and `Id`s. Its memory is freed when the last holder lets go, with no cleanup code.
- **Planned:** lock order checked across the whole program, and a compile error for waiting inside a lock, on a channel or the network.

### `with` and `lend`

`with` lends you something for the length of a block, then takes it back. Files are opened this way:

```fennec
def save(fs: Fs, path: str, scores: list[int]) fails IoError:
    # the new file takes the path's place only when the block ends, synced to the disk
    with fs.create(path)? as file:
        for score in scores:
            file.write_line(str(score))?

def load(fs: Fs, path: str) -> list[int] fails IoError:
    mut found = list[int]()
    with fs.open(path)? as file:
        for line in file.lines()?:
            number = int.parse(line) except InvalidValue(): 0
            found.append(number)
    return found
```

- The end of the block is where the file finishes. The `?` on the `with` line covers both opening it and finishing it, so there's no `close()` to forget.
- A block that fails leaves the old file as it was, so there's never half a file. `durable=False` skips waiting for the disk, and `file.sync()?` puts what's written there at any point.
- `fs.open` reads in `lines()` or `chunks(size)`, so a file bigger than memory is fine.
- What a `with` lends can't escape the block, and a struct holding something borrowed is borrowed too.

Writing your own takes `-> with T` and one `lend`, with setup above it, cleanup below, and `lend x except:` for when the caller's block fails:

```fennec
struct Ledger:
    lines = list[str]()

def entry(ref ledger: Ledger, name: str) -> with str:
    ledger.lines.append(f"open {name}")
    # the caller's block runs here
    lend name.upper() except:
        # the caller's block failed
        ledger.lines.append(f"undo {name}")
    ledger.lines.append(f"close {name}")

def main(stdout: Stdout):
    mut ledger = Ledger()
    with entry(ref ledger, "ana") as label:
        stdout.print(label)
    stdout.print(ledger.lines)
```

Code after `lend` runs when the caller's block succeeds, and if it fails, that's the `with` statement's failure. An early `return` still runs it.

### `launch` and channels

`launch` runs a function beside you until its `with` block ends, so no task outlives the code that started it. Built natively.

```fennec
from std.concurrent import Inbox, Crashed, Closed, Timeout

enum Sound:
    Play(name: str, volume: float)
    StopAll

def audio(inbox: Inbox[Sound], stdout: Stdout):
    for sound in inbox:
        match sound:
            case .Play(name, volume):
                stdout.print(f"playing {name} at {volume}")
            case .StopAll:
                stdout.print("quiet")

def main(stdout: Stdout) fails Crashed | Closed | Timeout:
    with launch((inbox) => audio(inbox, stdout), stop_after=5s)? as sounds:
        sounds.send(.Play(name="jump", volume=0.8))?
        sounds.send(.StopAll)?
```

- A parameter typed `Inbox[T]` makes `launch` create the channel: the task gets the receiving end and you get the sending end. The enum is the protocol, so you can only send real cases and the task must handle each one.
- The `?` on the `with launch` line covers the task: its own failures, and `Crashed` if it crashes, come out where the block ends. A crash ends only its own task.
- Channels are always bounded, and `send` waits when one is full. `send` and `receive` take `timeout=`. A channel of your own is `with channel[Job](size=64) as jobs:`, from `std.concurrent`, a plain function as Python's `open()` is. `to=.Every` gives every listener every message.
- **Planned:** pools, like a database's connections, each lent to one block at a time by its keeper, so nothing can leak or be used twice.

Not in the language at all: `del`, destructors, moves, lifetimes, `unsafe` and global state.

## Visibility

`export` marks what other kits can use, and it goes on top-level types and functions only. Inside a kit, every file sees everything. A big program that wants walls between its parts splits into local kits.

```fennec
export struct Score:
    points = 0
    # seen only by Score's own methods
    _best = 0

export def best(scores: list[Score]) -> int:
    return max(score.points for score in scores)
```

- A `_name` field or method is hidden from everything but its type's own methods. Tests get no special access.
- A top-level name never starts with `_`, since it's already private to its kit.
- Other kits match an exported type's fields by name, as in `case Point(x=a, y=_)`, so a kit can add a field with a default to a type it exports without breaking anyone. That's built for the standard library's types so far.

## Interfaces and generics

A type fits an interface by having its methods. There's nothing to declare.

```fennec
from std.math import PI

interface Shape:
    def area(self) -> float

struct Circle:
    radius: float

    # Circle is a Shape because it has area()
    def area(self) -> float:
        return PI * self.radius * self.radius

def total_area[T: Shape](shapes: list[T]) -> float:
    return sum(shape.area() for shape in shapes)

def describe(shape: Shape) -> str:
    return f"area {shape.area()}"
```

A parameter typed with an interface is resolved at compile time, like `[T: Shape]`, with no hidden dispatch or boxing. For a collection of different kinds of things, use a union or an `enum`.

## Saving data

JSON, TOML and `std.pack`, the default binary format, save any type whose fields can all be saved, with nothing to derive:

```fennec
from std import json

struct Settings:
    volume = 0.8

def main(stdout: Stdout):
    text = json.dumps(Settings())
    loaded = json.loads[Settings](text) except ParseError(): Settings()
    stdout.print(text, loaded)
```

- A `Table` saves as a list of its items in order, and an `Id` as where its item is in that list. Loading makes a new `Table` whose `Id`s find the same items.
- A union saves tagged with its member's type name, as in `{"Circle": {"radius": 1.0}}`.
- Case style is an option on the call, `names=.CamelCase`. A field may be named any word, keywords included, and `@serialize(name="uid")` renames one.
- `where` rules check a value as it loads, and a key the type doesn't have is an error unless the call says `extra=.Ignore`.

```fennec
from std import json

struct Account:
    type: str
    display_name: str

def main(stdout: Stdout):
    # {"type": "admin", "displayName": "Ash"}
    stdout.print(json.dumps(Account(type="admin", display_name="Ash"), names=.CamelCase))
```

**Planned:** compile-time loops over a type, which serialization will be built on, instead of macros or `derive`. `fields(T)` gives a struct's fields and `cases(T)` an enum's cases. There's no I/O at compile time, so a kit can't run anything on your machine while it compiles.

```fennec planned
def write[T, S: Serializer](x: T, ref out: S):
    for f in fields(T):
        out.field(f.name)
        write(f.get(x), ref out)
```

## Events and callbacks

With no references, a callback can't hold on to the world and change it later. Instead, handlers return what should happen, and the main loop applies it:

```fennec
enum Command:
    Damage(target: int, amount: int)

def on_explosion(healths: list[int]) -> list[Command]:
    return [.Damage(target=i, amount=50) for i, health in enumerate(healths) if health > 0]

def main(stdout: Stdout):
    mut healths = [100, 0]
    for command in on_explosion(healths):
        match command:
            case .Damage(target, amount):
                healths[target] -= amount
    stdout.print(healths)
```

Every change happens in one visible place, which also makes replays and networking simpler. Functions are still values you can pass and store, as callbacks, dicts of handlers or routes. The whole program is compiled, so every function such a value could hold is known at compile time. There's no code the compiler hasn't seen.

## Capabilities

A function can only touch the outside world through what it's given. Without an `Fs` parameter, it can't read files. Without a `Clock`, it can't read the time.

```fennec
def load_save(fs: Fs, slot: str) -> str fails IoError:
    return fs.read_text(slot)?

# pure: no I/O possible
def parse_save(data: str) -> list[str]:
    return data.split()
```

Calling `print`, or using `stdout` in a function that wasn't given it, is an error that says how to pass it in. `Stdout`, `Stdin`, `Args`, `Fs`, `Clock`, `Env`, `Entropy`, `Process` and `Net` work today. `clock.now()` reads a monotonic clock, for measuring, and `clock.time()` reads the wall clock as a `Time` in UTC.

## Tests

Tests live next to the code they test.

```fennec
def clamped(health: int) -> int:
    return max(health, 0)

test "clamping stops at zero":
    assert clamped(-5) == 0

test "clamping never goes below zero" for n: int:
    assert clamped(n) >= 0
```

The second test is a property test: Fennec makes up inputs and shrinks a failure to the smallest example. `fennec run` runs the tests a change affects before it runs your program, and `fennec test` skips the ones whose code hasn't changed.

A test names the capabilities it needs, like `main`, and gets fakes: an in-memory `Fs`, a fixed `Clock`, captured `Stdout`, and a `Net` whose servers and clients reach each other in memory.

```fennec
def save_score(fs: Fs, stdout: Stdout, score: int) fails IoError:
    fs.write_text("score.txt", str(score))?
    stdout.print("saved")

test "saves the score"(fs: Fs, stdout: Stdout):
    save_score(fs, stdout, 120)?
    assert fs.read_text("score.txt")? == "120"
    assert stdout.captured() == "saved\n"
```

`fennec test --real` gives tests the real capabilities. `bench "name":` blocks sit beside the code they measure, and `fennec bench` runs them.

## Performance controls

`@hot` on a function means prove it or don't compile. Anything that may allocate or copy in a `@hot` function, or in what it calls, is a compile error that says why, and so is any check native code keeps there, like an index it can't place inside its list. An overflow check the compiler can't prove away stays, as the safety net it is everywhere.

```fennec
struct Particle:
    position: float
    velocity: float

@hot
def update(ref particles: list[Particle], dt: float):
    for ref particle in particles:
        particle.position += particle.velocity * dt
```

Floats have one equality, order and hash: NaN equals itself and sorts last, and `0.0` equals `-0.0`. There are no fused operations, and `sin`, `cos`, `exp`, `log` and `**` come from one library on every platform, so a float result is the same bits on every machine. There's no `@fast_math` to opt out, since replays and lockstep games depend on those bits, and speed comes from checks the compiler proves it can remove instead. **Planned:** `a.approx(b, within=...)`, which compares computed values with a tolerance.

## C libraries

**Planned.** Libraries are written in Fennec wherever possible. For what can't be, like windows, input, audio and graphics drivers, the Fennec project wraps the C library behind a safe API, in a first-party kit. Your code calls it like any other Fennec code, with capabilities in its signatures. There's no `unsafe` for you to write, now or later.

## Building

- `fennec run` runs the tests a change affects, then the program, built through LLVM a function at a time and cached. Where LLVM can't build something yet, it says why, and `fennec test` fails the tests that reach it.
- `fennec build` makes a native program through LLVM, and `fennec build --release` optimizes the whole program as one. On Linux, a release build is one static program that needs nothing from the machine.

## Decided lately

- **Failing out of `main`** prints to stderr and exits with code 1, and `main` can return an exit code.
- **Floats have one order,** with NaN equal to itself and sorted last.
- **`x / 0.0` stops the program,** as Python's `ZeroDivisionError` does.
- **A broken `where` rule** stops the program in code, and fails cleanly when data is loaded.
- **`?` inside `parallel`** passes a failure up out of the loop, as anywhere else.
- **There's no scoped `ref`.** Write directly, `units[target_id].health -= 1`, or call a method.
