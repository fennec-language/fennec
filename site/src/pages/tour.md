---
layout: ../layouts/Doc.astro
title: A tour of Fennec
description: A walk through Fennec's proposed syntax and semantics, from values and mutation to errors, collections, tests and C interop.
---

# A tour of Fennec

<p class="lede">If you can read Python, you can read Fennec. What's different is underneath: values instead of shared objects, types checked before anything runs, errors you can't ignore, and costs you can see.</p>

> **Fennec is in early design.** Everything on this page is proposed syntax, and it will change as real programs push on it.

## Hello

```fennec
def main(stdout: Stdout):
    stdout.print("Hello, Fennec")
```

`main` receives what the program is allowed to touch, here standard output. There's no global `print`: code can only write output if it's given somewhere to write it. More on that in [capabilities](#capabilities).

## Bindings

There are two ways to name a value. `const` can never change; `let` can.

```fennec
const name = "Ash"
let health = 100
health -= 10
```

Prefer `const`. The compiler flags any `let` that never changes. `const` is deep: nothing inside a `const` value can change either, not even through a function you pass it to.

## Functions

Parameters have types. Return types are inferred, and the last expression in a function is its value.

```fennec
def area(radius: float):
    PI * radius * radius                  # returns float

def label(enemy: Enemy):
    f"{enemy.name} ({enemy.health} HP)"   # returns str

def factorial(n: int) -> int:             # recursive functions state their return type
    1 if n <= 1 else n * factorial(n - 1)
```

- Use `return` to leave early. You can write a return type anywhere you want it as documentation; your editor shows the inferred one.
- Short functions can be passed inline with an arrow: `enemies.sort(key=enemy => enemy.health)`.
- Anything longer is a nested function, which also gives the logic a name:

```fennec
def visible_enemies(world: World):
    def in_view(enemy: Enemy):
        const distance = (enemy.position - world.camera).length()
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

Built-in types are lowercase. Everything you define or import is capitalized.

## Values

A `struct` is a value. Assigning or passing it gives the receiver its own independent copy, so nothing can change your data behind your back.

```fennec
struct Vec3:
    x: f32
    y: f32
    z: f32

let a = Vec3(x=1.0, y=2.0, z=3.0)
let b = a
b.x = 10.0   # a.x is still 1.0
```

Copies are cheap or free: the compiler moves a value instead of copying it when the original isn't used again. Copying anything that owns heap memory, like a list or a string, is written out as `.copy()`, so expensive copies never happen by accident.

Equality, hashing and printing work automatically for any type whose fields support them. There's nothing to derive.

## Mutation is visible

Parameters are read-only. A function that changes its argument says `mut` in its signature, and every call to it says `mut` too.

```fennec
def apply_damage(mut player: Player, amount: int):
    player.health -= amount

let player = Player.spawn()
apply_damage(mut player, 10)   # you can see player changes here
describe(player)               # and that it doesn't change here
```

Reading any call, you know whether it can modify your data without opening the function. Loops say it too: `for mut enemy in enemies:` changes each enemy in place.

## Methods

Methods live inside the struct, as in Python. `self` is read-only unless the method says `mut self`.

```fennec
struct Enemy:
    const id: Id[Enemy]          # const fields never change after creation
    position: Vec3
    health: int

    def is_alive(self):
        self.health > 0

    def take_damage(mut self, amount: int):
        self.health -= amount
```

## Enums and match

An `enum` is one of several cases, each with its own data. `match` must handle every case.

```fennec
enum Shape:
    Circle(radius: f32)
    Rect(width: f32, height: f32)

def area(shape: Shape):
    match shape:
        case .Circle(radius): PI * radius * radius
        case .Rect(width, height): width * height
```

The leading dot marks an enum case. You only write the enum's name if two enums in scope share a case name.

## Errors

There are no exceptions. A function that can fail returns `Result`, you can't use the value without dealing with the error, and `?` passes an error up to your caller.

```fennec
enum ConfigError:
    NotFound(path: Path)
    Parse(line: int, message: str)
    Io(IoError)

def load_config(fs: Fs, path: Path) -> Result[Config, ConfigError]:
    const text = fs.read_text(path)?      # an IoError becomes ConfigError.Io
    parse_config(text)
```

Any enum used as an error gets display and source chaining automatically, and `?` converts errors into any case that wraps them. To assert that something can't fail, you write `expect("why it can't fail")`.

## Optional values

A value that might be missing has a `?` on its type: `Enemy?` holds either an `Enemy` or `None`. The compiler won't let you use it until you've checked.

```fennec
def find_target(world: World) -> Enemy?:
    ...

const target = find_target(world)
if target is None:
    return
target.take_damage(10)                  # the compiler knows target is an Enemy here

const damage = player.weapon?.damage ?? 1
```

There's no wrapper to unpack. An `Enemy?` takes no more space than a plain pointer would.

## Collections

Comprehensions build collections, and the brackets tell you something new is being allocated. Without brackets, a generator expression is lazy and allocates nothing.

```fennec
const alive = [enemy for enemy in enemies if enemy.is_alive()]   # a new list
const by_id = {enemy.id: enemy for enemy in alive}               # a new dict
const total = sum(hit.damage for hit in hits)                    # no allocation
const strongest = max(alive, key=enemy => enemy.health)
```

- Python's builtins come along: `len`, `sum`, `min`, `max`, `any`, `all`, `enumerate`, `zip`, `range`.
- A `dict` keeps insertion order, so iterating it is always deterministic.
- `IdMap` stores things that come and go, like enemies or bullets. Inserting returns an `Id`, and a stale `Id` safely finds nothing, even after its slot is reused.

```fennec
const id = world.enemies.insert(Enemy.spawn())
world.enemies.remove(id)
world.enemies.get(id)   # None
```

## Immutable state

`const` makes any value immutable, so there's no need for separate frozen types. A few more tools for working with immutable data:

```fennec
const point = (3, 4)                            # tuples are always immutable
const (quotient, remainder) = divmod(17, 5)     # and unpack naturally

const hurt = replace(enemy, health=enemy.health - 10)

const items: frozen[dict[str, Item]] = freeze(load_items(fs)?)
```

- `replace()` makes a copy with some fields changed. When the original isn't used again, the compiler updates it in place, so immutable code runs at mutation speed.
- `frozen[...]` is for large data that many places hold at once. It can never change, copying it is nearly free, and it's safe to share across threads.

## Operators

Your own types support operators through Python's special methods.

```fennec
struct Vec3:
    x: f32
    y: f32
    z: f32

    def __add__(self, other: Vec3):
        Vec3(x=self.x + other.x, y=self.y + other.y, z=self.z + other.z)

position += velocity * dt
```

Operators can't perform I/O, and the test runner checks that equality, ordering and hashing follow their rules.

## Interfaces and generics

A type matches an interface by having its methods. There's nothing to declare.

```fennec
interface Shape:
    def area(self) -> f32

struct Circle:
    radius: f32

    def area(self):
        PI * self.radius * self.radius     # Circle is a Shape because it has area()

def total_area[T: Shape](shapes: list[T]):
    sum(shape.area() for shape in shapes)
```

Writing `struct Circle(Shape):` turns that into a promise the compiler checks. Generics are resolved at compile time, so calls go straight to the right function. For a collection of different kinds of things, use an `enum`.

## Capabilities

A function can only touch the outside world through what it's given. Without an `Fs` parameter, it can't read files. Without a `Clock`, it can't read the time.

```fennec
def load_save(fs: Fs, clock: Clock, slot: int):
    ...   # can read files and the time

def parse_save(data: bytes):
    ...   # pure: no I/O possible
```

Signatures show what code can do, and tests pass in fakes like an in-memory filesystem or a fixed clock with no setup.

## Tests

Tests live next to the code they test.

```fennec
test "a unit circle's area is pi":
    assert area(.Circle(radius=1.0)) == PI

test "reversing twice gives the original" for items: list[int]:
    assert reversed(reversed(items)) == items
```

The second test is a property test: Fennec generates inputs for any type, shrinks a failure to the smallest example, and saves its seed so it replays exactly. Tests only get fake capabilities, so they can't touch the real network or the wall clock by accident. When an assertion fails, it shows the value of every part of the expression.

## Performance controls

```fennec
from std.layout import SoaList

struct Particle:
    position: Vec3
    velocity: Vec3
    life: f32

noalloc def update(mut particles: SoaList[Particle], dt: f32):
    for mut particle in particles:
        particle.position += particle.velocity * dt
        particle.life -= dt
```

- `noalloc` is checked through every call. If anything inside allocates, it's a compile error that names the call.
- `SoaList` stores each field in its own array, which is faster for code that loops over one field at a time. You still write `particle.position`.

## Shared objects

Most data should be plain values. When something really needs a shared identity, such as an asset cache or a network connection, use a `class`. Classes are reference counted, there's no garbage collector, and the compiler rejects reference cycles that would leak.

```fennec
class AssetCache:
    let textures: dict[str, Texture]
    weak owner: Game
```

## C and unsafe code

Fennec is safe by default. Calling C is the one place unsafe code is allowed, and it's fenced in:

```fennec
extern "C":
    def sqlite3_open(path: *u8, db: **Sqlite3) -> i32
    def sqlite3_close(db: *Sqlite3) -> i32

export struct Database:
    handle: *Sqlite3

    def __del__(self):
        unsafe("handle came from sqlite3_open and is closed exactly once"):
            sqlite3_close(self.handle)
```

- `unsafe` blocks are only allowed in modules that declare `extern` functions, and each one states its reason.
- Exported functions can't expose raw pointers, so callers only ever see a safe API.
- A type with `__del__` is cleaned up exactly once, when its owner is done with it, and can't be copied or leaked.
- A package that uses `unsafe` is marked, and you have to allow it by name before your project can depend on it.
