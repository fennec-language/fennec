# Draft: how `for mut` writes back into a user type

Status: a draft for Chris to decide. Nothing here is built, and nothing in `docs/decisions.md` changes until it's approved. Until then, `for mut` over a user type is a compile error.

## The question

`for mut x in xs:` over a `list` writes each change straight into the list. A user collection, like a pool, a grid or an ECS table, needs a way to say the same thing. It has to keep values semantics, and it must never copy an element per iteration, since a copy on every iteration of a loop is already a compile error.

## Recommendation: positions plus the `mut` form of `def []`

A type supports `for mut` by defining two methods it mostly has already, and one optional one:

- `def len(self) -> int`: how many positions it has.
- The `mut` form of `def []` with an `int` position, which `docs/decisions.md` already promises: it gives the element as a place, not a copy. The syntax for that form isn't settled; this draft writes it `def [](mut self, i: int) -> T`.
- Optionally, `def is_live(self, i: int) -> bool`, for a type whose positions can be empty, like a pool with free slots. Empty positions are skipped.

`for mut x in xs:` then means:

```fennec
for i in range(xs.len()):
    if xs.is_live(i):        # only when the type defines it
        mut x = xs[i]        # a place, by the rule `mut x = place` already has
        ...
```

`xs` stays locked for the whole loop, as any path behind a `mut` name is, so the body can't add or remove items or reach other elements. Nothing is copied, nothing is counted, and `x` can't escape, all by rules that already exist. Reading with a plain `for` still uses `def iter(self)` with `yield`.

## The game-shaped case

An ECS table of 100,000 units, written by a user:

```fennec
struct Unit:
    pos: Vec2
    vel: Vec2
    health = 100

struct Units:
    _items = Buffer[Unit]()
    _live = Buffer[bool]()

    def len(self) -> int:
        return self._items.len()

    def is_live(self, i: int) -> bool:
        return self._live[i]

    def [](mut self, i: int) -> Unit:      # the `mut` form: returns a place in `_items`
        return self._items[i]

def step(mut units: Units, dt: float):
    for mut unit in units:
        unit.pos += unit.vel * dt
```

This compiles to one pass over `_items`, writing each `pos` in place, with a predictable branch on `_live`. `IdMap` works the same way: its positions are its storage slots, and dead ones are skipped.

## The alternatives

- **`def at(i)` and `def set(i, value)`**, the get-and-set pair of Python's `__getitem__` and `__setitem__` and C#'s indexers. Each iteration copies the element out and puts it back. With copy-on-write, an element that holds a collection, like a unit's inventory, is still held by the container when it's changed, so it's copied on every iteration. That breaks the rule this has to keep.
- **`yield mut`**, a generator that lends each place in turn, like Swift's `_modify` accessors and lending iterators elsewhere. It works, but it's the feature `docs/decisions.md` cut from the first version. A lent place would have to stay locked across each suspension of a generator, which is a handle that can be stored. It also can't be split for `parallel`. It can still come later without breaking anything.
- **Borrow-checked mutable iterators (`iter_mut`).** Each item is a unique reference with a lifetime. Fennec has no lifetimes to write, and a user type writing one usually needs unsafe code or split borrows.

Swift's `MutableCollection` is closest to the recommendation: an index type plus a subscript that writes in place, used as `for i in xs.indices { xs[i].x += 1 }`. The recommendation is that, with `for mut` as the spelling.

## Why

It adds no new concept: `len`, `def []` and `mut x = place` exist already. It never copies, by construction. And the same positions are what the companion draft on splitting for `parallel` needs, so one protocol serves both.

## Open

- The syntax of the `mut` form of `def []`.
- Whether positions other than `int`, like a grid's `(int, int)`, are worth allowing. This draft says no: a grid maps its `int` position to a cell itself.
