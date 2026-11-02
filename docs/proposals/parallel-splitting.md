# Draft: how a collection splits for `parallel`

Status: a draft for Chris to decide. Nothing here is built, and nothing in `docs/decisions.md` changes until it's approved. It builds on the companion draft on `for mut` write-back.

## The question

`parallel(units)` and `for mut unit in parallel(units)` need to split a collection's work across threads. Built-in lists can, but `std` types like `IdMap`, `Packed` and `Buffer`, and user types, need a way to say how. Splitting has to keep two existing rules: each task changes only its own elements, and a reduction's result depends on the data's size, never the thread count.

## Recommendation: split by position

A collection that has positions, `def len(self)` plus `def []` with an `int` (and optionally `def is_live`, as in the `for mut` draft), splits with nothing more to write.

- `parallel(xs)` splits `0..xs.len()` into ranges. The work-stealing pool halves a range when a thread runs out of work, down to a smallest chunk the runtime picks from the element size.
- Where a reduction's parts combine is fixed by the length alone, so `sum(u.health for u in parallel(units))` gives the same float on every machine and every thread count.
- `for mut unit in parallel(units)` gives each task a disjoint range. That's safe only if writing position `i` touches nothing but element `i`. The compiler checks it from the `mut` form of `def []`: it must return a place rooted in one field of `self`, indexed by `i` itself, like `return self._items[i]`. If it isn't, `for mut` over `parallel(...)` is a compile error naming that method, and the plain `for mut` still works.

That check is what lets a user type join without any `unsafe` and without a splitting interface to implement.

## The game-shaped case

A step over 100,000 units, then a count:

```fennec
def step(mut units: IdMap[Unit], dt: float):
    for mut unit in parallel(units):
        unit.pos += unit.vel * dt
    alive = sum(1 for unit in parallel(units) if unit.health > 0)
```

The user `Units` table from the `for mut` draft splits the same way, since its `mut` `def []` returns `self._items[i]`.

## The `std` types

- **`list` and `Buffer`**: contiguous ranges, which also vectorize.
- **`IdMap`**: its positions are its storage slots, and ranges are cut at its chunk boundaries. A task's writes then touch only its own chunks, which matches the rule that a write after a snapshot copies a chunk, not the map.
- **`Grid`**: row-major positions, cut at row boundaries.
- **`Packed`**: `parallel(packed)` gives each task whole lists, to read. Writing items in parallel goes through its flat items, and how it exposes them in place is open.
- **`dict`, `set`, `SortedMap` and `SortedSet`**: read-only in parallel, split by their own internal ranges. There's no `for mut` over them in parallel, as there's no positional `def []` to check.

## `yield`-based iteration stays sequential

A type that only has `def iter(self)` with `yield` can't be split: a generator gives one item when asked, and the next depends on the last. `parallel(xs)` over such a type treats it as a stream, as it already does for a server: one task pulls items in order and hands each to the pool, read-only. `for mut` over it in parallel is a compile error that says to add positions. Reductions over a stream still combine in a fixed order, by item number.

## The alternative

Work-stealing libraries elsewhere have each type implement a splittable producer: a method that cuts the work in two at an index, plus a sequential iterator for each piece. It's flexible, and it's also a second interface every collection writes, whose writes the compiler can't check without lifetimes. Splitting by position gets the same work-stealing with nothing new to write, and the one check above replaces trusting the producer.

## Why

The positions a type already has for `def []` and `for mut` are exactly what splitting needs, and a reduction's fixed order falls out of splitting by length. A user type gets real parallelism for free, and one that can't prove separate writes still runs, in order.

## Open

- How `Packed` exposes its flat items for writing in place.
- Whether a type should be able to suggest its own smallest chunk, or whether the runtime always decides.
