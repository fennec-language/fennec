---
layout: ../layouts/Doc.astro
title: A tour of Fennec
description: A walk through Fennec's proposed syntax and semantics, from values and mutation to errors, collections, tests and C interop.
---

# A tour of Fennec

<p class="lede">If you can read Python, you can read Fennec. What's different is underneath: everything is a value, types are checked before anything runs, errors can't be ignored, and there are no references to manage.</p>

> **Fennec is in early design.** Most of this page already runs in the interpreter (`fennec run`, `fennec test` and `fennec repl`), and the type checker catches most of the mistakes it describes before a program runs; the rest are still caught as it runs. Concurrency, `noalloc` proofs and C interop come later. The syntax will still change as real programs push on it.

## Hello

```fennec
def main(stdout: Stdout):
    stdout.print("Hello, Fennec")
```

`main` receives what the program is allowed to touch, here standard output. There's no global `print`: code can only write output if it's given somewhere to write it. More on that in [capabilities](#capabilities).

## Names

A name is immutable unless you declare it `mut`.

```fennec
name = "Ash"
mut health = 100
health -= 10
name = "Vex"      # error: `name` isn't mut. Declare it `mut name = "Ash"`
```

`mut` is the one word for mutability everywhere: names, parameters, methods and loops. Immutability is deep: nothing inside an immutable value can change either, not even through a function you pass it to.

An immutable name is set once, by a single expression. When the value depends on a condition, use a conditional expression, or `match` for more than two choices:

```fennec
label = "boss" if enemy.health > 500 else "minion"

speed = match enemy.kind:
    case .Runner: 8.0
    case .Walker: 3.0
    case .Turret: 0.0
```

Names belong to the block they're created in. A name created inside an `if`, `for` or `with` doesn't exist after it, so nothing leaks into the rest of the function.

## Functions

Parameters and return types are written; everything inside a function is inferred. The last expression in a function is its value.

```fennec
from std.math import PI

def area(radius: float) -> float:
    PI * radius * radius

def label(enemy: Enemy) -> str:
    f"{enemy.name} ({enemy.health} HP)"

def factorial(n: int) -> int:
    1 if n <= 1 else n * factorial(n - 1)
```

- Use `return` to leave early. A function with no `->` returns nothing. If you leave a return type out, the error's fix writes in the one the compiler inferred.
- Short functions can be passed inline with an arrow: `enemies.sort(key=enemy => enemy.health)`.
- Closures read the names around them but can't change them. To build up a value, use a comprehension or a builtin like `sum`. A nested function that's only called directly, never passed around, can change the enclosing function's `mut` names.
- Anything longer is a nested function, which also gives the logic a name:

```fennec
def visible_enemies(world: World) -> list[Enemy]:
    def in_view(enemy: Enemy) -> bool:
        distance = (enemy.position - world.camera).length()
        distance < world.view_range

    [enemy for enemy in world.enemies if in_view(enemy)]
```

## Built-in types

| Type | Example |
| --- | --- |
| `int`, `float` | `42`, `3.14` (64-bit) |
| Sized numbers | `i32`, `u8`, `f32` when you want control over size |
| `bool` | `True`, `False` |
| `str` | `"fennec"`, `f"{count} foxes"` |
| `list`, `dict`, `set` | `[1, 2]`, `{"hp": 10}`, `{1, 2}` |
| `tuple` | `(3, 4)` |

Built-in types are lowercase. Everything you define or import is capitalized. Number literals take the type their context needs, so `distance > 5.0` works whether `distance` is a `float` or an `f32`.

## Structs

A struct is a set of named fields, like a Python dataclass. You get a constructor automatically, and fields can have defaults. A field with a default takes its type from it, so only fields without one need a type.

```fennec
struct Config:
    volume = 0.8
    fullscreen = False
    levels = ["intro"]

config = Config(volume=0.5)
defaults = Config()            # every field has a default
```

A method without `self` is a named constructor, for anything the automatic one can't do:

```fennec
from std.math import Vec3

struct Enemy:
    position: Vec3
    health = 100

    def at(position: Vec3) -> Enemy:
        Enemy(position=position)

boss = Enemy.at(spawn_point)
```

Rules about a field's value go right next to it with `where`. Creating a value that breaks one fails, like any other failure:

```fennec
struct Player:
    name: str where name.len() > 0
    level: int where level >= 1

player = Player(name=entered_name, level=1)?
```

When the values are known at compile time, the compiler checks the rules itself, so `Player(name="Ash", level=1)` needs no `?`.

Equality, hashing and printing work automatically for any type whose fields support them. There's nothing to derive.

## Values, not references

Everything in Fennec is a value. Assigning or passing something gives the receiver its own independent version, so nothing can change your data behind your back. There are no references, pointers or lifetimes to manage.

```fennec
from std.math import Vec3

a = Vec3(x=1.0, y=2.0, z=3.0)
mut b = a
b.x = 10.0   # a.x is still 1.0
```

This doesn't mean everything gets copied. The compiler passes values by reference when it's safe, moves them when the original isn't used again, and shares large data, like a big list, until one side changes it. You write simple value code; the compiler makes it fast.

Because a copy is independent, changing it never changes the original. The compiler catches the mistake that habit from other languages leads to:

```fennec
mut enemies = world.enemies
enemies.remove(id)    # error: you changed `enemies`, a copy of `world.enemies`,
                      # but never used it. To change the world, write world.enemies.remove(id)
```

## Mutation is visible

Parameters are read-only. A function that changes its argument says `mut` in its signature, and every call to it says `mut` too.

```fennec
def apply_damage(mut player: Player, amount: int):
    player.health -= amount

mut player = Player.spawn()
apply_damage(mut player, 10)   # you can see player changes here
describe(player)               # and that it doesn't change here
```

Reading any call, you know whether it can modify your data without opening the function. Methods and loops say it the same way:

```fennec
struct Enemy:
    health: int

    def is_alive(self) -> bool:
        self.health > 0

    def take_damage(mut self, amount: int):
        self.health -= amount

for mut enemy in enemies:
    enemy.take_damage(1)
```

## Enums and match

An `enum` is one of several cases, each with its own data. `match` must handle every case.

```fennec
from std.math import PI

enum Shape:
    Circle(radius: f32)
    Rect(width: f32, height: f32)

def area(shape: Shape) -> f32:
    match shape:
        case .Circle(radius): PI * radius * radius
        case .Rect(width, height): width * height
```

The leading dot marks an enum case. You only write the enum's name if two enums in scope share a case name.

## Missing values

A value that might be missing has a `?` on its type: `Enemy?` holds either an `Enemy` or `None`. The compiler won't let you use it until you've checked.

```fennec
def find_target(world: World) -> Enemy?:
    ...

target = find_target(world)
if target is None:
    return
target.take_damage(10)                  # the compiler knows target is an Enemy here

damage = player.weapon?.damage else 1   # use 1 if there's no weapon
```

## Errors

There are no exceptions. An error is a value a function returns when it fails, and you can't use the result without dealing with it.

```fennec
from std.path import Path

error NotFound:
    path: Path

error ConfigUnreadable:
    path: Path

def load_config(fs: Fs, path: Path) -> Config:
    if not fs.exists(path):
        fail NotFound(path)
    text = fs.read_text(path)? from ConfigUnreadable(path)
    parse(text)
```

- `error` declares an error type. `fail` returns one; anything else the function returns is success.
- `?` passes an error up to your caller. `from` adds context, keeping the original error as the cause.
- A function's possible errors are inferred, and your editor shows them.

There are three ways to deal with a call that can fail:

```fennec
config = load_config(fs, path)?                    # pass any error up

config = load_config(fs, path) else Config()       # use a default if it failed

match load_config(fs, path):                       # handle specific errors
    case Ok(config): run(config)
    case NotFound(path): create_default(fs, path)
    case Err(error): warn(f"couldn't load config: {error}")
```

The default after `else` is only evaluated if it's needed. `?` works inside comprehensions too: `[parse_score(line)? for line in lines]` gives every score, or the first error. Failing is a plain return, with no stack unwinding, so it costs nothing extra.

## Collections

Comprehensions build collections, and the brackets tell you something new is being allocated. Without brackets, a generator expression is lazy and allocates nothing.

```fennec
alive = [enemy for enemy in enemies if enemy.is_alive()]   # a new list
by_name = {enemy.name: enemy for enemy in alive}           # a new dict
total = sum(hit.damage for hit in hits)                    # no allocation
strongest = max(alive, key=enemy => enemy.health)
```

- Python's looping builtins come along: `sum`, `min`, `max`, `any`, `all`, `sorted`, `enumerate`, `zip`, `range`. Questions about one value are methods: `xs.len()`, `x.abs()`.
- A `dict` keeps insertion order, so iterating it is always deterministic.
- Changing a collection while looping over it is a compile error. Use `remove_where`, or collect what to change first:

```fennec
world.enemies.remove_where(enemy => enemy.health <= 0)
```

- `IdMap`, from `std.game`, stores things that come and go, like enemies or bullets. Inserting returns an `Id`, and a stale `Id` safely finds nothing, even after its slot is reused.

```fennec
from std.game import Id

type EnemyId = Id[Enemy]

id = world.enemies.insert(Enemy.spawn())
world.enemies.remove(id)
world.enemies.get(id)   # None
```

## Strings

`str` is UTF-8 text that works like Python's: f-strings with Python's format specs, slicing, `split`, `strip`, `join`, `startswith`, `in` and the rest.

```fennec
greeting = f"Hello, {name}! You have {coins:,} coins and {health:.1f} HP"

first = name[0]                    # a one-character str
words = "fire ice storm".split()   # ["fire", "ice", "storm"]
clean = raw.strip().lower()

mut log = ""
log += "spawned\n"                 # appends in place
```

- **Short strings don't allocate.** Up to about 23 bytes, like names and keys, are stored inline.
- **Indexing is fast.** It's constant-time for ASCII text, and cached for other text.
- **Slices share memory instead of copying,** so `for line in file.lines():` hands you each line without a copy.
- **`bytes` is a separate type for binary data.** Text read from outside is checked once: `fs.read_text` fails on invalid UTF-8, and `fs.read_bytes` gives raw bytes.

## Generators

A function with `yield` produces values one at a time.

```fennec
def visible(enemies: list[Enemy], camera: Camera) -> Iterator[Enemy]:
    for enemy in enemies:
        if camera.sees(enemy.position):
            yield enemy

for enemy in visible(enemies, camera):
    draw(enemy)
```

Generators compile to small state machines that live on the stack, so streaming through data allocates nothing and works inside `noalloc` code. A recursive generator is the one exception, since its memory grows with depth.

## Immutable data

```fennec
point = (3, 4)                              # tuples are always immutable
quotient, remainder = divmod(17, 5)         # and unpack naturally

hurt = replace(enemy, health=enemy.health - 10)
```

`replace()` makes a copy with some fields changed. When the original isn't used again, the compiler updates it in place, so immutable code runs at mutation speed.

## Context managers

`with` ties a value to a block. When the block ends, normally or through `?`, the value is cleaned up.

```fennec
with fs.open(path) as file:
    header = file.read_line()?
# the file is closed here

with db.transaction() as mut tx:
    tx.insert(player)?
    tx.insert(inventory)?
    tx.commit()?
# if anything failed before commit, the transaction rolls back
```

There's nothing special to implement: any type that defines cleanup with `def del` works with `with`.

## Operators

Your own types define operators by name.

```fennec
struct Vec3:
    x: f32
    y: f32
    z: f32

    def +(self, other: Vec3) -> Vec3:
        Vec3(x=self.x + other.x, y=self.y + other.y, z=self.z + other.z)

position += velocity * dt
```

`def []` defines indexing the same way. Operators can't perform I/O, and the test runner checks that equality, ordering and hashing follow their rules.

## Interfaces and generics

A type matches an interface by having its methods. There's nothing to declare.

```fennec
from std.math import PI

interface Shape:
    def area(self) -> f32

struct Circle:
    radius: f32

    def area(self) -> f32:
        PI * self.radius * self.radius     # Circle is a Shape because it has area()

def total_area[T: Shape](shapes: list[T]) -> f32:
    sum(shape.area() for shape in shapes)
```

Release builds compile generic code for each type it's used with, so calls go straight to the right function. For a collection of different kinds of things, use an `enum`.

## Events and callbacks

With no references, a callback can't hold on to the world and change it later. Instead, handlers return what should happen, and the main loop applies it:

```fennec
from std.math import Vec3

enum Command:
    Spawn(enemy: Enemy)
    Damage(target: EnemyId, amount: int)

def on_explosion(world: World, center: Vec3) -> list[Command]:
    [.Damage(id, 50) for id, enemy in world.enemies.items() if enemy.distance_to(center) < 3.0]

for command in on_explosion(world, blast):
    world.apply(command)
```

Every change to the world happens in one visible place, which also makes replays and networking simpler.

## Concurrency

> Planned after v0.1.

There's no `async` or `await`. Code that waits on disk or the network is written as normal functions, and the runtime does other work while it waits. Independent reads overlap automatically:

```fennec
def load_assets(fs: Fs, manifest: Manifest) -> Assets:
    textures = load_textures(fs, manifest)?    # these two reads run at the same time
    sounds = load_sounds(fs, manifest)?
    Assets(textures=textures, sounds=sounds)
```

Using several cores takes one word, and the compiler suggests where it's safe and worthwhile:

```fennec
for mut particle in parallel(particles):
    particle.position += particle.velocity * dt
```

Each element is changed by exactly one thread, and results are deterministic. Data races are impossible: values aren't shared, and changing shared state goes through a lock with `with stats.lock() as mut s:`.

## Capabilities

A function can only touch the outside world through what it's given. Without an `Fs` parameter, it can't read files. Without a `Clock`, it can't read the time.

```fennec
def load_save(fs: Fs, clock: Clock, slot: int) -> Save:
    ...   # can read files and the time

def parse_save(data: bytes) -> Save:
    ...   # pure: no I/O possible
```

Signatures show what code can do, and tests pass in fakes like an in-memory filesystem or a fixed clock with no setup.

## Tests

Tests live next to the code they test.

```fennec
from std.math import PI

test "a unit circle's area is pi":
    assert area(.Circle(radius=1.0)) == PI

test "reversing twice gives the original" for items: list[int]:
    assert reversed(reversed(items)) == items
```

The second test is a property test: Fennec generates inputs for any type, shrinks a failure to the smallest example, and saves its seed so it replays exactly. Tests only get fake capabilities, so they can't touch the real network or the wall clock by accident. When an assertion fails, it shows the value of every part of the expression.

## Performance controls

```fennec
from std.layout import SoaList
from std.math import Vec3

struct Particle:
    position: Vec3
    velocity: Vec3
    life: f32

noalloc def update(mut particles: SoaList[Particle], dt: f32):
    for mut particle in particles:
        particle.position += particle.velocity * dt
        particle.life -= dt
```

- `noalloc` is checked through every call. If anything inside allocates or might copy shared data, it's a compile error that names the line and says why.
- `SoaList` stores each field in its own array, which is faster for code that loops over one field at a time. You still write `particle.position`.

## C and unsafe code

Fennec is safe by default, and libraries are written in Fennec wherever possible. Calling C is for what can't be: the operating system, GPU drivers and platform SDKs. It's the one place unsafe code is allowed, and it's fenced in:

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

- Pointers (`ptr[...]`) and `unsafe` blocks are only allowed in files with an `extern` block, and every call into C states its reason.
- A pointer is never null unless its type says so (`ptr[T]?`). An `owned ptr` must be released exactly once, or the program doesn't compile.
- `extern` declarations are checked against the real C headers when building, and `fennec bindgen` writes them from the headers.
- `pub` functions can't expose pointers, so callers only ever see a safe API.
- A type with `def del` is cleaned up exactly once, when it's no longer used, and can't be duplicated.
- A kit that uses C is marked native, and you allow it by name before your project can depend on it. For now, only the Fennec project publishes these; your own app can still use `extern` in its own code.
