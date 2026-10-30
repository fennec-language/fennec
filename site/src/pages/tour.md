---
layout: ../layouts/Doc.astro
title: A tour of Fennec
description: A walk through Fennec's proposed syntax and semantics, from values and mutation to errors, collections, tests and C interop.
---

# A tour of Fennec

<p class="lede">If you've written TypeScript, most of Fennec will look familiar. What's different is underneath: values instead of shared objects, errors you can't ignore, and costs you can see.</p>

> **Fennec is in early design.** Everything on this page is proposed syntax, and it will change as real programs push on it.

## Hello

```fennec
fn main(console: Console) {
  console.print("Hello, Fennec")
}
```

`main` receives what the program is allowed to touch, here a console. More on that in [capabilities](#capabilities).

## Bindings

There are two ways to name a value. `const` can never change; `let` can.

```fennec
const name = "Ash"
let health = 100
health -= 10
```

Prefer `const`. The compiler flags any `let` that never changes. Unlike TypeScript, `const` is deep: nothing inside a `const` value can change either.

## Functions

```fennec
export fn area(radius: f32) -> f32 {
  PI * radius * radius
}

fn label(enemy: Enemy) {
  "${enemy.name} (${enemy.health} HP)"
}
```

- `fn` declares a function and `->` gives its return type.
- The last expression in a block is its value. Use `return` to leave early.
- Types are inferred inside functions. Exported functions always state their return type, so their signature is their documentation and changing a function's body never forces its callers to recompile.

## Values

A `struct` is a value. Assigning or passing it gives the receiver its own independent copy, so nothing can change your data behind your back.

```fennec
struct Vec3 {
  x: f32
  y: f32
  z: f32
}

let a = Vec3(x: 1.0, y: 2.0, z: 3.0)
let b = a
b.x = 10.0   // a.x is still 1.0
```

Copies are cheap or free: the compiler moves a value instead of copying it when the original isn't used again. Copying anything that owns heap memory, like a list or a string, is always written out as `.copy()`, so expensive copies never happen by accident.

## Mutation is visible

Parameters are read-only. A function that changes its argument says `inout` in its signature, and every call to it says `inout` too.

```fennec
fn applyDamage(inout player: Player, amount: i32) {
  player.health -= amount
}

let player = Player.spawn()
applyDamage(inout player, 10)   // you can see player changes here
describe(player)                // and that it doesn't change here
```

Reading any call, you know whether it can modify your data, without opening the function.

## Enums and match

An `enum` is one of several cases, each with its own data. `match` must handle every case.

```fennec
enum Shape {
  Circle(radius: f32)
  Rect(width: f32, height: f32)
}

fn area(shape: Shape) -> f32 {
  match shape {
    .Circle(radius) => PI * radius * radius
    .Rect(width, height) => width * height
  }
}
```

The leading dot marks an enum case. When the type is known, you write just the case.

## Errors

A function that can fail returns `Result`. You can't use the value without dealing with the error, and `?` passes an error up to your caller.

```fennec
error ConfigError {
  NotFound(path: Path)
  Parse(line: u32, message: String)
  Io(IoError)
}

fn loadConfig(fs: Fs, path: Path) -> Result<Config, ConfigError> {
  const text = fs.readToString(path)?   // an IoError becomes ConfigError.Io
  parseConfig(text)
}
```

`error` declares an error type with display and source chaining built in, and `?` converts errors into any case that wraps them. There are no exceptions and no `unwrap`: to assert that something can't fail, you write `expect("why it can't fail")`.

## Optional values

There's no `null`. A value that might be missing is an `Option`, and TypeScript's `?.` and `??` work on it.

```fennec
const damage = player.weapon?.damage ?? 1
const target = enemies.get(id) ?? return Err(.TargetGone(id))
```

## Collections and iterators

`List`, `Map` and `IdMap` cover most needs. Iterator chains don't allocate; they compile down to plain loops.

```fennec
const alive = enemies.filter(enemy => enemy.health > 0)
const strongest = alive.maxBy(enemy => enemy.health)
const totalDamage = hits.map(hit => hit.damage).sum()
const names = players.map(player => player.name.copy()).toList()   // this line allocates
```

- `Map` keeps insertion order, so iterating it is always deterministic.
- `IdMap` stores things that come and go, like enemies or bullets. Inserting returns an `Id`, and a stale `Id` safely finds nothing, even after its slot is reused.

```fennec
const id = world.enemies.insert(Enemy.spawn())
world.enemies.remove(id)
world.enemies.get(id)   // .None
```

## Operators

Your own types can use the standard operators by implementing their interfaces.

```fennec
impl Add for Vec3 {
  fn add(self, other: Vec3) -> Vec3 {
    Vec3(x: self.x + other.x, y: self.y + other.y, z: self.z + other.z)
  }
}

position += velocity * dt
```

The set of operators is fixed, so you can't invent new symbols. Operators can't perform I/O, and the test runner checks that `==`, ordering and hashing follow their rules.

## Interfaces and generics

```fennec
interface Shape {
  fn area(self) -> f32
}

fn totalArea<T: Shape>(shapes: List<T>) -> f32 {
  shapes.map(shape => shape.area()).sum()
}
```

Generics are resolved at compile time, so calls go straight to the right function. There's no runtime dispatch: for a collection of different kinds of things, use an `enum`.

## Capabilities

A function can only touch the outside world through what it's given. If a function has no `Fs` parameter, it can't read files. If it has no `Clock`, it can't read the time.

```fennec
fn loadSave(fs: Fs, clock: Clock, slot: u8) -> Result<SaveGame, SaveError> {
  // can read files and the time
}

fn parseSave(bytes: Bytes) -> Result<SaveGame, SaveError> {
  // pure: no I/O possible
}
```

Signatures show what code can do, and tests pass in fakes like an in-memory filesystem or a fixed clock with no setup.

## Tests

Tests live next to the code they test.

```fennec
test "a unit circle's area is pi" {
  assert area(.Circle(radius: 1.0)) == PI
}

test "reversing twice gives the original" for items: List<i32> {
  assert items.reversed().reversed() == items
}
```

The second test is a property test: Fennec generates inputs for any type, shrinks a failure to the smallest example, and saves its seed so it replays exactly. Tests only get fake capabilities, so they can't touch the real network or the wall clock by accident. When an assertion fails, it shows the value of every part of the expression.

## Performance controls

```fennec
struct Particle {
  position: Vec3
  velocity: Vec3
  life: f32
}

struct Effects {
  particles: List<Particle, layout: .Soa>
}

noalloc fn update(inout effects: Effects, dt: f32) {
  for particle in effects.particles.iterMut() {
    particle.position += particle.velocity * dt
    particle.life -= dt
  }
}
```

- `noalloc` is checked through every call. If anything inside allocates, it's a compile error that names the call.
- `layout: .Soa` stores each field in its own array, which is faster for code that loops over one field at a time. You still write `particle.position`.

## Shared objects

Most data should be plain values. When something really needs a shared identity, such as an asset cache or a network connection, use a `class`. Classes are reference counted, there's no garbage collector, and the compiler rejects reference cycles that would leak.

```fennec
class AssetCache {
  let textures: Map<String, Texture>
  weak owner: Game
}
```

## C and unsafe code

Fennec is safe by default. Calling C is the one place unsafe code is allowed, and it's fenced in:

```fennec
extern "C" {
  fn sqlite3_open(path: *u8, db: **Sqlite3) -> i32
  fn sqlite3_close(db: *Sqlite3) -> i32
}

export resource struct Database {
  handle: *Sqlite3

  drop(self) {
    unsafe("handle came from sqlite3_open and is closed exactly once") {
      sqlite3_close(self.handle)
    }
  }
}
```

- `unsafe` blocks are only allowed in modules that declare `extern` functions, and each one states its reason.
- Exported functions can't expose raw pointers, so callers only ever see a safe API.
- Anything holding a C resource is a `resource` and must say how it's freed.
- A package that uses `unsafe` is marked, and you have to allow it by name before your project can depend on it.
