---
layout: ../layouts/Doc.astro
title: Language reference
description: Fennec as it runs today, written from examples that are all tested.
---

# Language reference

<p class="lede">Every program here runs as part of Fennec's tests, and prints what's shown. So this is what the language does today, not what it's meant to do.</p>

> Written by `fennec dev reference` from `examples/`. Change the examples, not this page.

## Values and memory

**Indexing gives a place, not a copy.**

### `list_loops`

```fennec
# Loops over lists that only index them read and change items in place, and copies made
# before the loop never change.

struct Particle:
    x: float
    speed: float

    def nudge(mut self, by: float):
        self.x += by

def first_negative(xs: list[int]) -> int:
    mut i = 0
    while i < xs.len():
        if xs[i] < 0: return i
        i += 1
    -1

def main(stdout: Stdout):
    mut xs = [1, 2, 3, 4]
    copy = xs
    for i in range(xs.len()):
        xs[i] *= 10
    stdout.print(xs, copy)

    mut grid = [0 for _ in range(12)]
    for row in range(3):
        for col in range(4):
            grid[row * 4 + col] = row * col
    stdout.print(grid, first_negative([3, 1, -4, 1, -5]), first_negative([1]))

    mut particles = [Particle(x=float(i), speed=0.5) for i in range(3)]
    for step in range(2):
        for i in range(particles.len()):
            particles[i].x += particles[i].speed
    for i in range(particles.len()):
        particles[i].nudge(100.0)
    stdout.print(particles[0].x, particles[2].x)

    lists = [[1, 2], [3, 4]]
    mut total = 0
    for xs in lists:
        total += xs[0] * xs[1]
    stdout.print(total, xs)
```

It prints:

```text
[10, 20, 30, 40] [1, 2, 3, 4]
[0, 0, 0, 0, 0, 1, 2, 3, 0, 2, 4, 6] 2 -1
101.0 103.0
14 [10, 20, 30, 40]
```

**`IdMap` and `Id[T]` are the way to point at things.**

### `game`

```fennec
from std.game import Id, IdMap
from std.math import Vec3

type EnemyId = Id[Enemy]

struct Enemy:
    position: Vec3
    velocity = Vec3.zero
    health = 100

    def take_damage(mut self, amount: int):
        self.health -= amount

struct World:
    enemies = IdMap[Enemy]()
    player = Vec3.zero

error TargetGone:
    target: EnemyId

error OutOfRange:
    distance: float

noalloc def step(mut world: World, dt: float):
    for mut enemy in world.enemies:
        enemy.position += enemy.velocity * dt

export def attack(mut world: World, target: EnemyId):
    mut enemy = world.enemies.get(target)
    if enemy is None: fail TargetGone(target)
    distance = (enemy.position - world.player).length()
    if distance > 5.0: fail OutOfRange(distance)
    enemy.take_damage(10)
    world.enemies[target] = enemy

def main(stdout: Stdout):
    mut world = World()
    near = world.enemies.insert(Enemy(position=Vec3(x=3.0, y=4.0), velocity=Vec3(x=-1.0)))
    far = world.enemies.insert(Enemy(position=Vec3(x=10.0)))

    stdout.print(attack(mut world, near))
    stdout.print(attack(mut world, far))
    stdout.print(world.enemies[near].health)

    step(mut world, 2.0)
    stdout.print(world.enemies[near].position)

    world.enemies.modify(far, e => e.take_damage(25))
    stdout.print(world.enemies[far].health)

    world.enemies.remove(near)
    stdout.print(attack(mut world, near))
    stdout.print(world.enemies.len(), world.enemies.get(near))

    weakest = min(world.enemies.values(), key=enemy => enemy.health)
    stdout.print(weakest.health)

test "attacking a distant enemy fails":
    mut world = World()
    far = world.enemies.insert(Enemy(position=Vec3(x=10.0, y=0.0, z=0.0)))
    assert attack(mut world, far) == Err(OutOfRange(distance=10.0))

test "attacking a removed enemy fails":
    mut world = World()
    id = world.enemies.insert(Enemy(position=Vec3.zero))
    world.enemies.remove(id)
    assert attack(mut world, id) == Err(TargetGone(id))
```

It prints:

```text
Ok()
Err(OutOfRange(distance=10.0))
90
Vec3(x=1.0, y=4.0, z=0.0)
75
Err(TargetGone(target=Id(0)))
1 None
75
```

## Collections

**Collections are values.**

### `list_values`

```fennec
## Lists are values: a copy keeps its items when the original changes, and a `mut`
## parameter changes the caller's list.

struct Point:
    x: float
    y: float

def grow(mut xs: list[int], n: int):
    for i in range(n):
        xs.append(i * 10)

def doubled(xs: list[int]) -> list[int]:
    mut out: list[int] = []
    for x in xs:
        out.append(x * 2)
    return out

def nudge(mut points: list[Point]):
    for i in range(points.len()):
        points[i].x += 1.0

def both(xs: list[int], mut ys: list[int]) -> int:
    ys.append(9)
    ys[0] = 7
    return xs.len() * 10 + xs[0]

def spread(
    mut a: list[int],
    mut b: list[float],
    mut c: list[int],
    mut d: list[bool],
    n: int,
) -> float:
    for i in range(n):
        a.append(i)
        b.append(float(i) / 2.0)
        c[0] += i
        d.append(i % 2 == 0)
    return b[-1] * 2.0

def main(stdout: Stdout):
    mut xs = [1, 2, 3]
    copy = xs
    xs[0] = 100
    xs.append(4)
    stdout.print(xs, copy, xs.len(), copy.len())

    mut shared = [7]
    alias = shared
    shared.append(8)
    stdout.print(shared, alias)

    grow(mut xs, 3)
    stdout.print(xs, doubled(xs), xs == copy, copy == [1, 2, 3])

    mut many: list[int] = []
    for i in range(1_000):
        many.append(i)
    mut total = 0
    for v in many:
        total += v
    stdout.print(many.len(), total, many[-1], many[500])

    mut points = [Point(x=0.0, y=1.0), Point(x=2.0, y=3.0)]
    before = points
    nudge(mut points)
    points.append(Point(x=9.0, y=9.0))
    stdout.print(points, before)

    mut sorted = [3, 1, 2]
    sorted.sort()
    last = sorted.pop()
    stdout.print(sorted, last, sorted + [5], xs[1:3])

    mut walked = [1, 2, 3]
    for w in walked:
        walked.append(w)
    stdout.print(walked)

    # Passed both ways, the argument without `mut` keeps its items.
    mut twice = [1, 2]
    stdout.print(both(twice, mut twice), twice)

    # A call can give back more than fits in a machine's return registers.
    mut ints = [1]
    mut floats = [0.5]
    mut counts = [10]
    mut flags = [False]
    stdout.print(
        spread(mut ints, mut floats, mut counts, mut flags, 100),
        ints.len(),
        floats[50],
        counts,
        flags[-1],
    )
```

It prints:

```text
[100, 2, 3, 4] [1, 2, 3] 4 3
[7, 8] [7]
[100, 2, 3, 4, 0, 10, 20] [200, 4, 6, 8, 0, 20, 40] False True
1000 499500 999 500
[Point(x=1.0, y=1.0), Point(x=3.0, y=3.0), Point(x=9.0, y=9.0)] [Point(x=0.0, y=1.0), Point(x=2.0, y=3.0)]
[1, 2] 3 [1, 2, 5] [2, 3]
[1, 2, 3, 1, 2, 3]
21 [7, 2, 9]
99.0 101 24.5 [4960] False
```

## Determinism

**Dicts and sets keep insertion order.**

### `sets_native`

```fennec
## Sets of ints and strings, native, keeping the order items were first added.

struct Seen:
    numbers: set[int]
    words: set[str]

def distinct(items: list[int]) -> int:
    {item % 10 for item in items}.len()

def main(stdout: Stdout):
    mut numbers = {3, 1, 2, 3}
    stdout.print(numbers, numbers.len(), 2 in numbers, 9 in numbers, 9 not in numbers)
    numbers.add(9)
    numbers.add(1)
    numbers.discard(3)
    numbers.discard(77)
    numbers.remove(2)
    stdout.print(numbers, numbers.len())
    for n in numbers:
        stdout.print(n)
    numbers.add(3)
    stdout.print(numbers, sum(n for n in numbers))

    copy = numbers
    numbers.add(100)
    numbers.remove(1)
    stdout.print(copy, numbers, copy == numbers, copy == {1, 9, 3}, {1, 2} == {2, 1})

    mut words = {"a", "b"}
    words.add("c")
    words.add("a")
    words.remove("b")
    stdout.print(words, "a" in words, "b" in words, sorted(words), [w + "!" for w in words])
    words.clear()
    words.add("z")
    numbers.clear()
    stdout.print(words, numbers, numbers.len())

    stdout.print(
        {n % 3 for n in range(10)},
        {f"k{n % 2}" for n in range(5)},
        distinct([1, 11, 21, 2]),
    )
    stdout.print({n for n in range(10) if n % 2 == 0}.len())
    stdout.print({x * 2 for x in [1, 2]} == {2, 4})

    mut seen = Seen(numbers={1}, words={"x"})
    seen.numbers.add(2)
    seen.words.add("y")
    seen.words.discard("x")
    stdout.print(seen, 2 in seen.numbers, "x" in seen.words, seen.numbers.len())
    for word in seen.words:
        stdout.print(word)
    floats = {1.5, 2.5}
    stdout.print(floats, 1.5 in floats)

    mut large = {0}
    for i in range(2000):
        large.add(i * 7 % 500)
        if i % 3 == 0:
            large.discard(i % 500)
    stdout.print(large.len(), 7 in large)
```

It prints:

```text
{3, 1, 2} 3 True False True
{1, 9} 2
1
9
{1, 9, 3} 13
{1, 9, 3} {9, 3, 100} False True True
{"a", "c"} True False ["a", "c"] ["a!", "c!"]
{"z"} set() 0
{0, 1, 2} {"k0", "k1"} 2
5
True
Seen(numbers={1, 2}, words={"y"}) True False 2
y
{1.5, 2.5} True
417 True
```

**`Clock` is a capability,**

### `std_time`

```fennec
from std.time import Clock, Duration, Instant

struct Ball:
    y = 10.0
    speed = 0.0

    ## Falls for one step of the simulation.
    def fall(mut self, dt: Duration):
        self.speed -= 9.8 * dt.as_seconds()
        self.y += self.speed * dt.as_seconds()

## How many fixed steps fit in the time since the last frame, carrying the rest over. The
## simulation only ever sees `step`, so it runs the same however fast the machine is.
def steps_due(carried: Duration, frame: Duration, step: Duration) -> (int, Duration):
    mut left = carried + frame
    mut steps = 0
    while not left < step:
        left = left - step
        steps += 1
    (steps, left)

test "frame times add up exactly" for millis: list[int]:
    total = Duration.millis(sum(millis))
    mut added = Duration()
    for ms in millis:
        added = added + Duration.millis(ms)
    assert added == total

def main(stdout: Stdout, clock: Clock):
    step = Duration.seconds(1.0 / 60.0)
    stdout.print(step, f"{step.as_millis():.3f} ms")
    stdout.print(Duration.millis(16) < step, step * 60, Duration.micros(1_500).as_millis())

    # Frame times as a real loop would read them, here made up so the run repeats.
    frames = [
        Instant(nanos=0),
        Instant(nanos=20_000_000),
        Instant(nanos=31_000_000),
        Instant(nanos=70_000_000),
    ]
    mut ball = Ball()
    mut carried = Duration()
    for at in range(1, frames.len()):
        due, carried = steps_due(carried, frames[at] - frames[at - 1], step)
        for _ in range(due):
            ball.fall(step)
        stdout.print(f"frame {at}: {due} steps, y = {ball.y:.4f}")

    # The real clock only ever moves forward.
    started = clock.now()
    clock.sleep(Duration.millis(2))
    waited = clock.now() - started
    stdout.print(not waited < Duration.millis(2), started < clock.now())
```

It prints:

```text
Duration(nanos=16666667) 16.667 ms
True Duration(nanos=1000000020) 1.5
frame 1: 1 steps, y = 9.9973
frame 2: 0 steps, y = 9.9973
frame 3: 3 steps, y = 9.9728
True True
```

**`Random` is a seeded value.**

### `std_random`

```fennec
from std.random import Random, random_int

struct Enemy:
    name: str
    health: int

## Rolls `count` dice with `sides` sides each.
def roll(mut rng: Random, count: int, sides: int) -> int:
    sum(rng.randint(1, sides) for _ in range(count))

test "the same seed gives the same numbers" for seed: int:
    mut a = Random(seed=seed)
    mut b = Random(seed=seed)
    assert [a.randint(0, 99) for _ in range(5)] == [b.randint(0, 99) for _ in range(5)]

test "randint stays in its range" for seed: int, low: int:
    mut rng = Random(seed=seed)
    value = rng.randint(low, low + 5)
    assert low <= value <= low + 5

test "shuffling keeps every item" for seed: int, items: list[int]:
    mut rng = Random(seed=seed)
    mut shuffled = items
    rng.shuffle(mut shuffled)
    assert sorted(shuffled) == sorted(items)

def main(stdout: Stdout):
    mut rng = Random(seed=42)
    stdout.print([rng.randint(1, 6) for _ in range(10)])
    stdout.print(roll(mut rng, 3, 6), rng.random(), rng.uniform(-1.0, 1.0))

    enemies = [
        Enemy(name="goblin", health=7),
        Enemy(name="orc", health=15),
        Enemy(name="troll", health=30),
    ]
    stdout.print(rng.choice(enemies).name)

    mut deck = ["ace", "king", "queen", "jack", "ten"]
    rng.shuffle(mut deck)
    stdout.print(deck)
    stdout.print(rng)

    # A replay starts again from the same seed and sees the same numbers.
    mut replay = Random(seed=42)
    stdout.print([replay.randint(1, 6) for _ in range(10)])

    # Without keeping a generator: the 3rd number of seed 7's sequence, from 0 to 9.
    value, drawn = random_int(7, 3, 0, 9)
    stdout.print(value, drawn)
    mut wide = Random(seed=-1)
    stdout.print(wide.randint(-9_223_372_036_854_775_807, 9_223_372_036_854_775_807))
```

It prints:

```text
[5, 1, 2, 3, 1, 6, 2, 5, 3, 4]
9 0.5200132996032402 0.3303188215994022
goblin
["queen", "jack", "ten", "king", "ace"]
Random(seed=42, drawn=20)
[5, 1, 2, 3, 1, 6, 2, 5, 3, 4]
5 4
7266964230113668128
```

## Functions and types

**A leading-dot case resolves from the type expected where it's written.**

### `enums`

```fennec
from std.math import PI

enum Shape:
    Circle(radius: float)
    Rect(width: float, height: float)
    Point

def area(shape: Shape) -> float:
    match shape:
        case .Circle(radius):
            PI * radius * radius
        case .Rect(width, height):
            width * height
        case .Point:
            0.0

def describe(shape: Shape) -> str:
    kind = match shape:
        case .Circle(_):
            "round"
        case .Rect(width, height):
            if width == height:
                "square"
            else:
                "rectangle"
        case _:
            "tiny"
    f"a {kind} shape with area {area(shape):.2f}"

def main(stdout: Stdout):
    shapes = [.Circle(radius=1.0), Shape.Rect(2.0, 3.0), .Rect(width=2.0, height=2.0), .Point]
    for shape in shapes:
        stdout.print(describe(shape))
    stdout.print(sum(area(shape) for shape in shapes) > 13.0)
    stdout.print(shapes[1])
```

It prints:

```text
a round shape with area 3.14
a rectangle shape with area 6.00
a square shape with area 4.00
a tiny shape with area 0.00
True
Shape.Rect(width=2.0, height=3.0)
```

**`extend` adds methods to a type from another file of the same kit.**

### `extend`

```fennec
# `extend` adds methods to a type outside its declaration, usually from another file of the
# same kit. The methods are the type's own: they're called, and checked, like any other.

struct Money:
    cents: int

    def show(self) -> str:
        f"${self.cents / 100:.2f}"

extend Money:
    def +(self, other: Money) -> Money:
        Money(cents=self.cents + other.cents)

    def split(self, ways: int) -> list[Money]:
        [Money(cents=self.cents // ways) for _ in range(ways)]

def main(stdout: Stdout):
    total = Money(cents=1250) + Money(cents=375)
    stdout.print(total.show(), [part.show() for part in total.split(3)])
```

It prints:

```text
$16.25 ["$5.41", "$5.41", "$5.41"]
```

**They're called interfaces, not protocols.**

### `interfaces`

```fennec
from std.math import PI

interface Shape:
    def area(self) -> float
    def name(self) -> str

interface Marker:
    pass

struct Circle:
    radius: float

    def area(self) -> float:
        PI * self.radius * self.radius  # Circle is a Shape because it has area() and name()

    def name(self) -> str:
        "circle"

struct Square:
    side: float

    def area(self) -> float:
        self.side * self.side

    def name(self) -> str:
        "square"

def total_area[T: Shape](shapes: list[T]) -> float:
    sum(shape.area() for shape in shapes)

def first[T, U](pairs: list[(T, U)]) -> T:
    item, _ = pairs[0]
    item

def main(stdout: Stdout):
    stdout.print(f"{total_area([Circle(radius=1.0), Circle(radius=2.0)]):.2f}")
    stdout.print(total_area([Square(side=2.0), Square(side=3.0)]))
    stdout.print([shape.name() for shape in [Square(side=1.0)]], first([(1, "one"), (2, "two")]))
    numbers = [1, 2]
    stdout.print(numbers)
```

It prints:

```text
15.71
13.0
["square"] 1
[1, 2]
```

**`Self` in an interface's method means the type that fits it.**

### `self_type`

```fennec
# `Self` in an interface's method means the type that fits the interface. `min` and `max`
# compare items with a type's `def <`.

interface Addable:
    def +(self, other: Self) -> Self

interface Signed:
    def abs(self) -> Self

struct Money:
    cents: int

    def +(self, other: Money) -> Money:
        Money(cents=self.cents + other.cents)

    def <(self, other: Money) -> bool:
        self.cents < other.cents

    def abs(self) -> Money:
        Money(cents=self.cents.abs())

def twice[T: Addable](x: T) -> T:
    x + x

def size[T: Signed](x: T) -> T:
    x.abs()

def main(stdout: Stdout):
    stdout.print(twice(Money(cents=5)), twice(2), twice(1.5), twice("ab"))
    stdout.print(size(Money(cents=-5)), size(-2), size(-1.5))
    wallet = [Money(cents=5), Money(cents=2)]
    stdout.print(min(wallet), max(wallet), max(wallet[0], wallet[1]))
```

It prints:

```text
Money(cents=10) 4 3.0 abab
Money(cents=5) 2 1.5
Money(cents=2) Money(cents=5) Money(cents=5)
```

**A nested `def` that's only called directly can change the enclosing function's `mut` names.**

### `nested_functions_native`

```fennec
## Functions written inside functions, native: each call is the body, right there.

def scaled(items: list[int], factor: int) -> list[int]:
    def times(n: int) -> int:
        n * factor
    [times(item) for item in items]

def labelled(name: str, n: int) -> str:
    def part(i: int) -> str:
        prefix = f"{name}-"
        return prefix + str(i)
    def both(a: int, b: int) -> str:
        part(a) + "/" + part(b)
    both(n, n + 1)

def by_branch(n: int) -> int:
    def pick(x: int) -> int:
        if x > 10: return x - 10
        x + 1
    pick(n) + pick(n + 10)

def kept(start: int) -> list[int]:
    def seen(n: int) -> int:
        n + start
    mut found = []
    for n in range(3):
        found.append(seen(n))
    found

def rolling(items: list[int]) -> list[int]:
    mut found = []
    for at, item in enumerate(items):
        found.append(item + items[0] + (items[at - 1] if at > 0 else 0))
    for first, second in zip(items, items):
        found.append(first + second + items.len())
    found

def main(stdout: Stdout):
    stdout.print(scaled([1, 2, 3], 4), labelled("kit", 2), by_branch(3), by_branch(30), kept(100))
    def shout(word: str) -> str:
        word + "!"
    stdout.print(shout("hi"), shout(shout("again")))
    total = 5
    def plus(n: int) -> int:
        n + total
    stdout.print(plus(1), [plus(n) for n in range(2)], sum(plus(n) for n in range(3)))
    stdout.print(rolling([1, 2, 3]))
    mut changing = [1, 2, 3]
    for at, item in enumerate(changing):
        if at == 0:
            changing.append(item + 9)
        stdout.print(at, item, changing.len())
```

It prints:

```text
[4, 8, 12] kit-2/kit-3 7 50 [100, 101, 102]
hi! again!!
6 [5, 6] 18
[2, 4, 6, 5, 7, 9]
0 1 4
1 2 4
2 3 4
```

## Standard library

**Command-line arguments and standard input are capabilities.**

### `args_and_stdin`

```fennec
# `main` names `args` to read its command-line arguments and `stdin` to read its input. A
# program that doesn't name them can't see them. Try
# `printf "one\ntwo\n" | fennec run examples/args_and_stdin.fen fire ice`.

def shout(words: list[str]) -> str:
    " ".join([word.upper() for word in words])

def main(stdout: Stdout, args: Args, stdin: Stdin):
    stdout.print(args.len(), "arguments:", shout([arg for arg in args]))
    if args.len() > 0:
        stdout.print("the first is", args[0])
    lines = stdin.lines()
    stdout.print(lines.len(), "lines of input")
    for line in lines:
        stdout.print(">", line)
```

It prints:

```text
0 arguments: 
0 lines of input
```

**`int(text)` and `float(text)` fail with `InvalidValue`**

### `builtins`

```fennec
from std.math import PI, ceil, cos, floor, sin, sqrt

def square(n: int) -> int:
    n * n

def checked(n: int):
    if n < 0: fail IoError("it's negative")

def main(stdout: Stdout):
    # Converting between numbers. `int` truncates toward zero.
    stdout.print(float(2), float(0.5), int(2.9), int(-2.9), int(-9223372036854775808.0), int(7))
    # Text might not be a number, so reading one can fail with an `InvalidValue`.
    stdout.print(float("2.5"), float(" 1e3 ") else 0.0, int(" 42 ") else 0, int("4x2") else -1)
    match int("twelve"):
        case Ok(n):
            stdout.print(n)
        case InvalidValue(message):
            stdout.print(message)

    # Rounding goes to the even neighbour on a tie, as in Python.
    stdout.print((-3).abs(), (-2.5).abs(), 4.abs())
    stdout.print(round(2.5), round(3.5), round(-2.5), round(3.14159, 2), round(1234.5, -2))
    stdout.print(floor(2.7), floor(-2.5), ceil(2.1), ceil(-2.5))
    stdout.print(sqrt(16), sqrt(2.25), sqrt(0), sin(0), cos(0), sin(PI / 2.0), cos(PI))

    # Walking through collections.
    stdout.print(reversed("fennec"), reversed([1, 2, 3]), reversed((1, 2)))
    no_numbers: set[int] = set()
    stdout.print(no_numbers, set([3, 1, 3, 2, 1]), set("hello").len())
    stdout.print(
        range(3),
        range(2, 5),
        range(10, 0, -3),
        range(6, 0, -2),
        range(0, 10, 4),
        range(5, 5),
        range(5, 0),
    )
    stdout.print(sorted([3, 1, 2]), min(3, 1, 2), max([3, 1, 2]), sum([1, 2, 3]), sum([0.5, 1.5]))
    stdout.print(list("abc"), enumerate(["a", "b"]), zip([1, 2], ["a", "b"]))
    stdout.print(any([False, True]), all([True, False]), all([]), any([]))

    # Results.
    three: Result[int, str] = Ok(3)
    no: Result[int, str] = Err("no")
    stdout.print(checked(1), three, no)
    ignore(three, "only checking that ignore accepts a reason")

    # Calls that return don't count toward the depth limit.
    stdout.print(sum(square(n) for n in range(2000)))
```

It prints:

```text
2.0 0.5 2 -2 -9223372036854775808 7
Ok(2.5) 1000.0 42 -1
"twelve" isn't an int
3 2.5 4
2 4 -2 3.14 1200.0
2 -3 3 -2
4.0 1.5 0.0 0.0 1.0 1.0 -1.0
cennef [3, 2, 1] [2, 1]
set() {3, 1, 2} 4
[0, 1, 2] [2, 3, 4] [10, 7, 4, 1] [6, 4, 2] [0, 4, 8] [] []
[1, 2, 3] 1 3 6 2.0
["a", "b", "c"] [(0, "a"), (1, "b")] [(1, "a"), (2, "b")]
True False True False
Ok() Ok(3) Err("no")
2664667000
```

**`bytes` is binary data, a run of ints from 0 to 255.**

### `bytes`

```fennec
# `bytes` is binary data: a run of ints from 0 to 255. Text becomes bytes with `encode`,
# as UTF-8, and comes back with `decode`, which fails on bytes that aren't text.

def checksum(data: bytes) -> int:
    sum(byte for byte in data) % 256

def main(stdout: Stdout):
    data = "héllo".encode()
    stdout.print(data, data.len(), "bytes, but", "héllo".len(), "characters")
    stdout.print(data[0], data[-1], data[1:3], 104 in data)
    stdout.print(checksum(data), [byte for byte in data[:2]])

    header = bytes([137, 80, 78, 71])
    stdout.print(header + bytes([13, 10]), header == bytes([137, 80, 78, 71]), bytes() < header)

    match data.decode():
        case Ok(text):
            stdout.print("decoded", text)
        case InvalidValue(message):
            stdout.print(message)
    match header.decode():
        case Ok(text):
            stdout.print("decoded", text)
        case InvalidValue(_):
            stdout.print("the header isn't text")
```

It prints:

```text
bytes([104, 195, 169, 108, 108, 111]) 6 bytes, but 5 characters
104 111 bytes([195, 169]) True
27 [104, 195]
bytes([137, 80, 78, 71, 13, 10]) True True
decoded héllo
the header isn't text
```

**Imports bind names exactly as in Python.**

### `imports`

```fennec
## The three ways to import, as in Python.

# `import std.collections` binds `std`: write `std.collections.Deque`.
# `from std import random` binds `random`: write `random.Random`.
from std import random
import std.collections
# `from std.re import Regex` binds `Regex` itself.
from std.re import Regex

def first_word(text: str) -> str:
    found = Regex(r"\w+").search(text)
    found?.text else ""

def main(stdout: Stdout):
    mut queue = std.collections.Deque()
    queue.append("ash")
    queue.append("kit")
    stdout.print(queue.popleft(), queue.len())

    mut rng = random.Random(seed=7)
    stdout.print(rng.randint(1, 6))

    stdout.print(first_word("  hello world"))
```

It prints:

```text
ash 1
3
hello
```

**`as` renames an import, in both of Python's forms.**

### `import_as`

```fennec
## `as` binds an import under another name, so two modules with the same function names,
## like `std.json` and `std.toml`, can both be used in one file.

from std import json as data
from std.json import loads as json_loads
import std.math as geometry
from std.toml import loads as toml_loads

struct Settings:
    speed: float

def main(stdout: Stdout):
    numbers = json_loads[list[int]]("[1, 2, 3]") else []
    settings = toml_loads[Settings]("speed = 2.5") else Settings(1.0)
    stdout.print(numbers, settings.speed)
    stdout.print(geometry.sqrt(16.0), data.dumps(numbers))
```

It prints:

```text
[1, 2, 3] 2.5
4.0 [1, 2, 3]
```

**Two modules can have functions with the same name.**

### `import_as`

```fennec
## `as` binds an import under another name, so two modules with the same function names,
## like `std.json` and `std.toml`, can both be used in one file.

from std import json as data
from std.json import loads as json_loads
import std.math as geometry
from std.toml import loads as toml_loads

struct Settings:
    speed: float

def main(stdout: Stdout):
    numbers = json_loads[list[int]]("[1, 2, 3]") else []
    settings = toml_loads[Settings]("speed = 2.5") else Settings(1.0)
    stdout.print(numbers, settings.speed)
    stdout.print(geometry.sqrt(16.0), data.dumps(numbers))
```

It prints:

```text
[1, 2, 3] 2.5
4.0 [1, 2, 3]
```

**JSON and TOML read into plain values.**

### `json`

```fennec
from std import json

struct Settings:
    volume = 0.8
    fullscreen = False
    keys = ["w", "a", "s", "d"]

struct Player:
    name: str
    level: int
    speed: float
    items: list[str]
    pet: str?
    alive: bool

def describe(text: str) -> str:
    match json.loads[list[float]](text):
        case Ok(numbers):
            f"read {json.dumps(numbers)}"
        case ParseError(message):
            f"couldn't read it: {message}"

def main(stdout: Stdout):
    text = "\n".join([
        "{",
        "  \"name\": \"Ash\",",
        "  \"level\": 7,",
        "  \"speed\": 2.5,",
        "  \"items\": [\"torch\", \"rope\"],",
        "  \"pet\": null,",
        "  \"alive\": true",
        "}",
    ])
    player = json.loads[Player](text)?
    stdout.print(player.name, "is level", player.level + 1, "and moves at", player.speed)
    for item in player.items:
        stdout.print("carries", item)
    stdout.print(player.pet is None, player.alive)

    stdout.print(json.dumps(player))
    stdout.print(json.dumps(Settings(), indent=2))
    stdout.print(json.dumps((1, "two", 3.0, None, (4, 5), {"6": [6], "7": []})))
    stdout.print(json.dumps("quotes \" and \\ and\nnew lines"))
    stdout.print(json.loads[str]("\"\\u00e9t\\u00e9\"")?)
    settings = json.loads[Settings](json.dumps(Settings()))?
    stdout.print(settings == json.loads[Settings](json.dumps(Settings(), indent=4))?)
    loud = json.loads[Settings]("{\"volume\": 1}")?
    stdout.print(loud.volume, loud.keys)

    stdout.print(describe("[1, 2.5e3, -0.5]"))
    stdout.print(describe("[1, \"two\"]"))
    stdout.print(describe("[1, 2,]"))
    stdout.print(describe("{\"a\": 1,\n \"b\" 2}"))
    stdout.print(describe("\"never ends"))
    stdout.print(describe("tru"))
    match json.loads[list[int]]("[\n  1,\n  oops\n]"):
        case ParseError(_, line, column):
            stdout.print(f"the problem is on line {line}, column {column}")
        case Ok(value):
            stdout.print(value)
```

It prints:

```text
Ash is level 8 and moves at 2.5
carries torch
carries rope
True True
{"name": "Ash", "level": 7, "speed": 2.5, "items": ["torch", "rope"], "pet": null, "alive": true}
{
  "volume": 0.8,
  "fullscreen": false,
  "keys": [
    "w",
    "a",
    "s",
    "d"
  ]
}
[1, "two", 3.0, null, [4, 5], {"6": [6], "7": []}]
"quotes \" and \\ and\nnew lines"
été
True
1.0 ["w", "a", "s", "d"]
read [1.0, 2500.0, -0.5]
couldn't read it: `[1]` should be a float, but it's a str
couldn't read it: expected a value, but found `]`, at line 1, column 7
couldn't read it: expected `:` after the key, but found `2`, at line 2, column 6
couldn't read it: this string never ends, at line 1, column 1
couldn't read it: `tru` isn't a JSON value; the words JSON knows are true, false and null, at line 1, column 1
the problem is on line 3, column 3
```

### `toml`

```fennec
from std import toml

struct Kit:
    name: str
    version: str
    fennec: str
    description: str
    authors: list[str]
    license: str
    capabilities: list[str]
    released: str

struct Dependency:
    version: str
    path = ""

struct Bin:
    name: str
    main: str

struct Release:
    profile: bool
    budget_mb: int
    scale: float

struct Build:
    release: Release

## A `kit.toml`.
struct Manifest:
    kit: Kit
    dependencies: dict[str, Dependency]
    bin: list[Bin]
    build: Build

## The kinds of value TOML knows.
struct Sample:
    speed = 0
    angle = 0.0
    at = ""
    empty: list[int] = []

struct Window:
    title = "Fox Run"
    width = 1280
    height = 720
    vsync = True

def kit_toml() -> str:
    "\n".join([
        "# The kit a Fennec project describes itself with.",
        "[kit]",
        "name = \"fox-run\"",
        "version = \"0.3.1\"",
        "fennec = \"0.1\"",
        "description = \"\"\"",
        "A small game about a fennec fox \\",
        "    who runs very fast.\"\"\"",
        "authors = [\"Ash <ash@example.com>\", 'Kit']",
        "license = 'MIT OR Apache-2.0'",
        "capabilities = [\"Stdout\", \"Fs\"]",
        "released = 2026-10-30",
        "",
        "[dependencies]",
        "vecmath = { version = \"1.2\" }",
        "sprites = { version = \"0.4\", path = \"../sprites\" }",
        "",
        "[[bin]]",
        "name = \"game\"",
        "main = \"src/main.fen\"",
        "",
        "[[bin]]",
        "name = \"editor\"",
        "main = \"tools/editor.fen\"",
        "",
        "[build.release]",
        "profile = true",
        "budget_mb = 1_024",
        "scale = 1.5e0",
    ])

def describe(text: str) -> str:
    match toml.loads[Sample](text):
        case Ok(sample):
            f"read {sample}"
        case ParseError(message):
            f"couldn't read it: {message}"

def main(stdout: Stdout):
    manifest = toml.loads[Manifest](kit_toml())?
    stdout.print(manifest.kit.name, manifest.kit.version, "for Fennec", manifest.kit.fennec)
    stdout.print(manifest.kit.description)
    stdout.print(manifest.kit.authors, manifest.kit.released)
    for name, dependency in manifest.dependencies.items():
        stdout.print("needs", name, dependency.version)
        if dependency.path != "":
            stdout.print("  from", dependency.path)
    for bin in manifest.bin:
        stdout.print(bin.name, "starts at", bin.main)
    stdout.print(manifest.build.release)

    stdout.write(toml.dumps(manifest))
    stdout.print("---")
    stdout.write(toml.dumps(Window()))
    stdout.print(toml.loads[Manifest](toml.dumps(manifest))? == manifest)

    stdout.print(describe("speed = 0x10\nangle = -0.5\nat = 07:30:00\nempty = []"))
    stdout.print(describe("name = \"fox\"\nname = \"kit\""))
    stdout.print(describe("[kit]\nversion = 1.2.3"))
    stdout.print(describe("[kit\nname = \"fox\""))
    stdout.print(describe("title = \"never ends\nname = \"fox\""))
    stdout.print(describe("[kit]\nname = \"fox\"\n[kit]"))
```

It prints:

```text
fox-run 0.3.1 for Fennec 0.1
A small game about a fennec fox who runs very fast.
["Ash <ash@example.com>", "Kit"] 2026-10-30
needs vecmath 1.2
needs sprites 0.4
  from ../sprites
game starts at src/main.fen
editor starts at tools/editor.fen
Release(profile=True, budget_mb=1024, scale=1.5)
[kit]
name = "fox-run"
version = "0.3.1"
fennec = "0.1"
description = "A small game about a fennec fox who runs very fast."
authors = ["Ash <ash@example.com>", "Kit"]
license = "MIT OR Apache-2.0"
capabilities = ["Stdout", "Fs"]
released = "2026-10-30"

[dependencies.vecmath]
version = "1.2"
path = ""

[dependencies.sprites]
version = "0.4"
path = "../sprites"

[[bin]]
name = "game"
main = "src/main.fen"

[[bin]]
name = "editor"
main = "tools/editor.fen"

[build.release]
profile = true
budget_mb = 1024
scale = 1.5
---
title = "Fox Run"
width = 1280
height = 720
vsync = true
True
read Sample(speed=16, angle=-0.5, at="07:30:00", empty=[])
couldn't read it: `name` is defined twice, at line 2, column 1
couldn't read it: `1.2.3` isn't a number TOML knows, at line 2, column 11
couldn't read it: expected `]` to end the header, but found the end of the line, at line 1, column 5
couldn't read it: this string never ends on its line; use `"""` for text over several lines, at line 1, column 9
couldn't read it: the table `[kit]` is defined twice, at line 3, column 1
```

**Bad text fails with `ParseError`.**

### `json`

```fennec
from std import json

struct Settings:
    volume = 0.8
    fullscreen = False
    keys = ["w", "a", "s", "d"]

struct Player:
    name: str
    level: int
    speed: float
    items: list[str]
    pet: str?
    alive: bool

def describe(text: str) -> str:
    match json.loads[list[float]](text):
        case Ok(numbers):
            f"read {json.dumps(numbers)}"
        case ParseError(message):
            f"couldn't read it: {message}"

def main(stdout: Stdout):
    text = "\n".join([
        "{",
        "  \"name\": \"Ash\",",
        "  \"level\": 7,",
        "  \"speed\": 2.5,",
        "  \"items\": [\"torch\", \"rope\"],",
        "  \"pet\": null,",
        "  \"alive\": true",
        "}",
    ])
    player = json.loads[Player](text)?
    stdout.print(player.name, "is level", player.level + 1, "and moves at", player.speed)
    for item in player.items:
        stdout.print("carries", item)
    stdout.print(player.pet is None, player.alive)

    stdout.print(json.dumps(player))
    stdout.print(json.dumps(Settings(), indent=2))
    stdout.print(json.dumps((1, "two", 3.0, None, (4, 5), {"6": [6], "7": []})))
    stdout.print(json.dumps("quotes \" and \\ and\nnew lines"))
    stdout.print(json.loads[str]("\"\\u00e9t\\u00e9\"")?)
    settings = json.loads[Settings](json.dumps(Settings()))?
    stdout.print(settings == json.loads[Settings](json.dumps(Settings(), indent=4))?)
    loud = json.loads[Settings]("{\"volume\": 1}")?
    stdout.print(loud.volume, loud.keys)

    stdout.print(describe("[1, 2.5e3, -0.5]"))
    stdout.print(describe("[1, \"two\"]"))
    stdout.print(describe("[1, 2,]"))
    stdout.print(describe("{\"a\": 1,\n \"b\" 2}"))
    stdout.print(describe("\"never ends"))
    stdout.print(describe("tru"))
    match json.loads[list[int]]("[\n  1,\n  oops\n]"):
        case ParseError(_, line, column):
            stdout.print(f"the problem is on line {line}, column {column}")
        case Ok(value):
            stdout.print(value)
```

It prints:

```text
Ash is level 8 and moves at 2.5
carries torch
carries rope
True True
{"name": "Ash", "level": 7, "speed": 2.5, "items": ["torch", "rope"], "pet": null, "alive": true}
{
  "volume": 0.8,
  "fullscreen": false,
  "keys": [
    "w",
    "a",
    "s",
    "d"
  ]
}
[1, "two", 3.0, null, [4, 5], {"6": [6], "7": []}]
"quotes \" and \\ and\nnew lines"
été
True
1.0 ["w", "a", "s", "d"]
read [1.0, 2500.0, -0.5]
couldn't read it: `[1]` should be a float, but it's a str
couldn't read it: expected a value, but found `]`, at line 1, column 7
couldn't read it: expected `:` after the key, but found `2`, at line 2, column 6
couldn't read it: this string never ends, at line 1, column 1
couldn't read it: `tru` isn't a JSON value; the words JSON knows are true, false and null, at line 1, column 1
the problem is on line 3, column 3
```

### `toml`

```fennec
from std import toml

struct Kit:
    name: str
    version: str
    fennec: str
    description: str
    authors: list[str]
    license: str
    capabilities: list[str]
    released: str

struct Dependency:
    version: str
    path = ""

struct Bin:
    name: str
    main: str

struct Release:
    profile: bool
    budget_mb: int
    scale: float

struct Build:
    release: Release

## A `kit.toml`.
struct Manifest:
    kit: Kit
    dependencies: dict[str, Dependency]
    bin: list[Bin]
    build: Build

## The kinds of value TOML knows.
struct Sample:
    speed = 0
    angle = 0.0
    at = ""
    empty: list[int] = []

struct Window:
    title = "Fox Run"
    width = 1280
    height = 720
    vsync = True

def kit_toml() -> str:
    "\n".join([
        "# The kit a Fennec project describes itself with.",
        "[kit]",
        "name = \"fox-run\"",
        "version = \"0.3.1\"",
        "fennec = \"0.1\"",
        "description = \"\"\"",
        "A small game about a fennec fox \\",
        "    who runs very fast.\"\"\"",
        "authors = [\"Ash <ash@example.com>\", 'Kit']",
        "license = 'MIT OR Apache-2.0'",
        "capabilities = [\"Stdout\", \"Fs\"]",
        "released = 2026-10-30",
        "",
        "[dependencies]",
        "vecmath = { version = \"1.2\" }",
        "sprites = { version = \"0.4\", path = \"../sprites\" }",
        "",
        "[[bin]]",
        "name = \"game\"",
        "main = \"src/main.fen\"",
        "",
        "[[bin]]",
        "name = \"editor\"",
        "main = \"tools/editor.fen\"",
        "",
        "[build.release]",
        "profile = true",
        "budget_mb = 1_024",
        "scale = 1.5e0",
    ])

def describe(text: str) -> str:
    match toml.loads[Sample](text):
        case Ok(sample):
            f"read {sample}"
        case ParseError(message):
            f"couldn't read it: {message}"

def main(stdout: Stdout):
    manifest = toml.loads[Manifest](kit_toml())?
    stdout.print(manifest.kit.name, manifest.kit.version, "for Fennec", manifest.kit.fennec)
    stdout.print(manifest.kit.description)
    stdout.print(manifest.kit.authors, manifest.kit.released)
    for name, dependency in manifest.dependencies.items():
        stdout.print("needs", name, dependency.version)
        if dependency.path != "":
            stdout.print("  from", dependency.path)
    for bin in manifest.bin:
        stdout.print(bin.name, "starts at", bin.main)
    stdout.print(manifest.build.release)

    stdout.write(toml.dumps(manifest))
    stdout.print("---")
    stdout.write(toml.dumps(Window()))
    stdout.print(toml.loads[Manifest](toml.dumps(manifest))? == manifest)

    stdout.print(describe("speed = 0x10\nangle = -0.5\nat = 07:30:00\nempty = []"))
    stdout.print(describe("name = \"fox\"\nname = \"kit\""))
    stdout.print(describe("[kit]\nversion = 1.2.3"))
    stdout.print(describe("[kit\nname = \"fox\""))
    stdout.print(describe("title = \"never ends\nname = \"fox\""))
    stdout.print(describe("[kit]\nname = \"fox\"\n[kit]"))
```

It prints:

```text
fox-run 0.3.1 for Fennec 0.1
A small game about a fennec fox who runs very fast.
["Ash <ash@example.com>", "Kit"] 2026-10-30
needs vecmath 1.2
needs sprites 0.4
  from ../sprites
game starts at src/main.fen
editor starts at tools/editor.fen
Release(profile=True, budget_mb=1024, scale=1.5)
[kit]
name = "fox-run"
version = "0.3.1"
fennec = "0.1"
description = "A small game about a fennec fox who runs very fast."
authors = ["Ash <ash@example.com>", "Kit"]
license = "MIT OR Apache-2.0"
capabilities = ["Stdout", "Fs"]
released = "2026-10-30"

[dependencies.vecmath]
version = "1.2"
path = ""

[dependencies.sprites]
version = "0.4"
path = "../sprites"

[[bin]]
name = "game"
main = "src/main.fen"

[[bin]]
name = "editor"
main = "tools/editor.fen"

[build.release]
profile = true
budget_mb = 1024
scale = 1.5
---
title = "Fox Run"
width = 1280
height = 720
vsync = true
True
read Sample(speed=16, angle=-0.5, at="07:30:00", empty=[])
couldn't read it: `name` is defined twice, at line 2, column 1
couldn't read it: `1.2.3` isn't a number TOML knows, at line 2, column 11
couldn't read it: expected `]` to end the header, but found the end of the line, at line 1, column 5
couldn't read it: this string never ends on its line; use `"""` for text over several lines, at line 1, column 9
couldn't read it: the table `[kit]` is defined twice, at line 3, column 1
```

**`Path` is a struct written in Fennec.**

### `path`

```fennec
from std.path import Path

def main(stdout: Stdout, fs: Fs):
    saves = Path("saves")
    slot = saves / "slot1.json"
    stdout.print(slot.text, slot.name(), slot.stem(), slot.extension())
    stdout.print(slot.parent().text, slot.parent().parent().text, Path("/").parent().text)

    sprite = Path("assets/sprites/../art/./fox.tar.gz")
    stdout.print(sprite.name(), sprite.stem(), sprite.extension())
    stdout.print(sprite.normalize().text, sprite.parts())
    stdout.print(Path("../../levels/./one").normalize().text, Path("/a/../../b").normalize().text)
    stdout.print(
        Path("a/..").normalize().text,
        Path(".hidden").extension() == "",
        Path("notes.").stem(),
    )

    stdout.print(
        Path("/home/ash").join("games").join("fox").text,
        Path("logs/").join("today.txt").text,
    )
    stdout.print(
        Path("ignored").join("/etc/hosts").text,
        Path("/etc").is_absolute(),
        slot.is_absolute(),
    )
    stdout.print(slot == Path("saves/slot1.json"), slot)
    stdout.print(fs.exists(Path("/")), fs.exists(Path("/").join("no such place")))
```

It prints:

```text
saves/slot1.json slot1.json slot1 json
saves . /
fox.tar.gz fox.tar gz
assets/art/fox.tar.gz ["assets", "sprites", "..", "art", "fox.tar.gz"]
../../levels/one /b
. True notes.
/home/ash/games/fox logs/today.txt
/etc/hosts True False
True Path(text="saves/slot1.json")
True False
```

**Files and folders go through `fs`.**

### `fs`

```fennec
# Files and folders go through the `Fs` capability that `main` receives, so code can only
# touch the disk when it's handed `fs`. A method that can fail gives an `IoError`.
from std.fs import Metadata

def describe(found: Metadata) -> str:
    if found.is_dir: return "a folder"
    "a file" if found.is_file else "something else"

def main(stdout: Stdout, fs: Fs):
    stdout.print("the root is", describe(fs.metadata("/")?))

    # These paths don't exist, so each call fails without changing anything.
    match fs.list_dir("no/such/folder"):
        case Ok(paths):
            stdout.print(paths.len(), "paths")
        case IoError(_):
            stdout.print("there's no folder to list")
    match fs.remove("no/such/folder/save.txt"):
        case Ok(_):
            stdout.print("removed it")
        case IoError(_):
            stdout.print("there's nothing to remove")
    match fs.metadata("no/such/folder/save.txt"):
        case Ok(found):
            stdout.print(found.size)
        case IoError(_):
            stdout.print("there's nothing to look at")
```

It prints:

```text
the root is a folder
there's no folder to list
there's nothing to remove
there's nothing to look at
```

**Taking from an empty `Deque`, `Heap` or `PriorityQueue` gives `None`.**

### `std_collections`

```fennec
from std.collections import Counter, Deque, Heap, PriorityQueue

## Steps from `start` to every square of a grid, by breadth-first search. `#` is a wall.
def distances(grid: list[str], start: (int, int)) -> dict[(int, int), int]:
    mut steps = {start: 0}
    mut frontier = Deque()
    frontier.append(start)
    while frontier.len() > 0:
        here = frontier.popleft()
        if here is None: break
        x, y = here
        for dx, dy in [(1, 0), (-1, 0), (0, 1), (0, -1)]:
            next = (x + dx, y + dy)
            nx, ny = next
            open = 0 <= ny < grid.len() and 0 <= nx < grid[ny].len() and grid[ny][nx] != "#"
            if open and next not in steps:
                steps[next] = steps[here] + 1
                frontier.append(next)
    steps

## A heap's items, smallest first. The heap passed in doesn't change: this pops a copy.
def drain(heap: Heap[int]) -> list[int]:
    mut left = heap
    mut items = []
    while True:
        item = left.pop()
        if item is None: return items
        items.append(item)

test "a heap gives back its items in order" for items: list[int]:
    assert drain(Heap.of(items)) == sorted(items)

test "a deque used from both ends keeps its order" for items: list[int]:
    mut deque = Deque()
    for item in items:
        deque.appendleft(item)
    assert deque.items() == reversed(items)
    assert deque.len() == items.len()

def main(stdout: Stdout):
    mut history = Deque.of(["move", "jump"])
    history.append("attack")
    history.appendleft("spawn")
    stdout.print(history.items(), history.len())
    stdout.print(history.pop(), history.popleft(), history.first(), history.last())
    for action in history:
        stdout.print(action)

    grid = ["..#.", "..#.", "...."]
    steps = distances(grid, (0, 0))
    stdout.print(steps[(3, 0)], steps.len())

    mut scores = Heap.of([40, 10, 30])
    scores.push(20)
    stdout.print(scores.peek(), drain(scores), scores.len())

    mut turns = PriorityQueue()
    turns.push("goblin", 12)
    turns.push("wizard", 18)
    turns.push("knight", 12)
    turns.push("dragon", 3)
    mut order = []
    while turns.len() > 0:
        order.append(turns.pop())
    stdout.print(order)

    mut loot = Counter.of(["gold", "gem", "gold", "potion", "gold", "gem"])
    loot.add("potion")
    loot["key"] += 1
    stdout.print(loot["gold"], loot["sword"], loot.total(), loot.len())
    stdout.print(loot.most_common(2))
    for item, count in loot.items():
        stdout.print(f"{item}: {count}")
```

It prints:

```text
["spawn", "move", "jump", "attack"] 4
attack spawn move jump
move
jump
7 10
10 [10, 20, 30, 40] 4
["dragon", "goblin", "knight", "wizard"]
3 0 8 4
[("gold", 3), ("gem", 2)]
gold: 3
gem: 2
potion: 2
key: 1
```

## Syntax

**A module can hold constants.**

### `constants_native`

```fennec
## Constants at the top level, written out where they're used.

NONE = -1
LIMIT = 10
ALSO = LIMIT
HALF = 0.5
NEGATIVE = -2.5
READY = True
NAME = "fennec"

TYPED: float = 1.5

WORDS = ["a", "b"]
DOUBLE = LIMIT * 2

def around(n: int) -> int:
    return n + NONE

def shadow(LIMIT: int) -> int:
    return LIMIT + 1

def main(stdout: Stdout):
    stdout.print(NONE, LIMIT, ALSO, HALF, NEGATIVE, READY, NAME, TYPED, WORDS, DOUBLE)
    stdout.print(around(LIMIT), shadow(1), LIMIT * LIMIT, HALF + NEGATIVE, not READY)
    stdout.print(NAME.len(), NAME + "!", NAME[0], f"{NAME} {LIMIT}")
    mut at = NONE
    while at != LIMIT:
        at += 1
    stdout.print(at, TYPED + HALF, ALSO == LIMIT)
```

It prints:

```text
-1 10 10 0.5 -2.5 True fennec 1.5 ["a", "b"] 20
9 2 100 -2.0 False
6 fennec! f fennec 10
10 2.0 True
```

**A generator is a lazy stream, used once.**

### `generators`

```fennec
# A generator gives its items one at a time, as they're asked for. It can be used once.

error TooBig:
    at: int

struct Door:
    out: Stdout
    name: str

    def del(self):
        self.out.print(f"closing {self.name}")

# Nothing here runs until the first item is asked for.
def countdown(out: Stdout, start: int) -> Iterator[int]:
    out.print("counting")
    mut n = start
    while n > 0:
        yield n
        n -= 1
    out.print("liftoff")

# It can go on forever, if whatever uses it stops.
def naturals() -> Iterator[int]:
    mut n = 0
    while True:
        yield n
        n += 1

# Each `yield` keeps its place, however deep it is.
def words(numbers: list[int]) -> Iterator[str]:
    for n in numbers:
        if n % 2 == 0:
            yield f"even {n}"
        else:
            match n:
                case 1:
                    yield "one"
                    yield "uno"
                case _:
                    yield f"odd {n}"
        yield f"after {n}"

# A generator can take another.
def doubled(numbers: Iterator[int]) -> Iterator[int]:
    for n in numbers:
        yield n * 2

# One left partway closes what it's holding when its owner goes away.
def holding(out: Stdout) -> Iterator[int]:
    with Door(out=out, name="outer") as outer:
        with Door(out=out, name="inner") as inner:
            yield 1
            yield 2
        out.print("past the inner door")

def leaves_early(out: Stdout):
    for n in holding(out):
        out.print("got", n)
        break
    out.print("left the loop")

# A failure comes out when the item that fails is asked for.
def checked(limit: int) -> Iterator[int]:
    for n in range(5):
        if n >= limit: fail TooBig(at=n)
        yield n

def total_up_to(limit: int) -> int:
    mut total = 0
    for n in checked(limit)?:
        total += n
    total

def bump(mut count: int) -> int:
    count += 1
    count

def main(stdout: Stdout):
    # The two take turns.
    for n in countdown(stdout, 2):
        stdout.print("got", n)

    stdout.print(list(words([1, 2, 3])))

    for n in naturals():
        if n == 2: break
        stdout.print(n)

    # Printing one doesn't use it.
    made = countdown(stdout, 1)
    stdout.print(made)
    stdout.print(sum(made))

    # What it sees is fixed when it's made.
    mut factor = 3
    mut numbers = [1, 2, 3]
    scaled = (n * factor for n in numbers if n != 2)
    factor = 100
    numbers = []
    stdout.print(scaled)
    stdout.print(list(scaled))

    # Stages can be chained, each kept in a name.
    lines = (line.strip() for line in ["  a", "   ", "bc  "])
    filled = (line for line in lines if line != "")
    stdout.print(sum(line.len() for line in filled))
    stdout.print(list(doubled(doubled(n + 1 for n in range(3)))))

    # What stops early stops asking.
    stdout.print(any(n > 1 for n in naturals()), all(n < 1 for n in naturals()))
    from_zero = naturals()
    stdout.print(any(n > 1 for n in from_zero))
    for at, n in enumerate(naturals()):
        if at == 2:
            stdout.print(at, n)
            break
    for letter, n in zip(["p", "q"], naturals()):
        stdout.print(letter, n)
    stdout.print(list(enumerate(n * 2 for n in range(2))))

    # Written right inside what uses it, it can change the names around it.
    mut count = 0
    stdout.print(sum(bump(mut count) for _ in range(3)), count)

    leaves_early(stdout)
    for n in holding(stdout):
        stdout.print("got", n)

    match total_up_to(5):
        case Ok(total):
            stdout.print(total)
        case Err(failure):
            stdout.print("failed", failure)
    match total_up_to(2):
        case Ok(total):
            stdout.print(total)
        case Err(failure):
            stdout.print("failed", failure)
```

It prints:

```text
counting
got 2
got 1
liftoff
["one", "uno", "after 1", "even 2", "after 2", "odd 3", "after 3"]
0
1
<Iterator[int]>
counting
liftoff
1
<Iterator[int]>
[3, 9]
3
[4, 8, 12]
True False
True
2 2
p 0
q 1
[(0, 0), (1, 2)]
6 3
got 1
closing inner
closing outer
left the loop
got 1
got 2
closing inner
past the inner door
closing outer
10
failed TooBig(at=2)
```

**`case` takes or-patterns and guards, as in Python.**

### `case_guards`

```fennec
## `case` takes several patterns separated by `|`, and an `if` guard that has to be true
## for the case to be taken. A guarded case never counts toward handling every case.

enum Tok:
    Int(value: int)
    Float(value: float)
    Name(word: str)
    Eof

def describe(tok: Tok) -> str:
    match tok:
        case .Int(_) | .Float(_):
            "a number"
        case .Name(word) if word in ["def", "if", "match"]:
            "the keyword " + word
        case .Name(word):
            "the name " + word
        case .Eof:
            "the end"

def size(n: int) -> str:
    match n:
        case 0:
            "none"
        case 1 | 2 | 3:
            "a few"
        case many if many < 0:
            "less than none"
        case _:
            "lots"

def first_word(found: str?) -> str:
    match found:
        case text if text is not None and text.len() > 0:
            text.split()[0]
        case _:
            "(nothing)"

def main(stdout: Stdout):
    for tok in [Tok.Int(1), Tok.Float(2.5), Tok.Name("match"), Tok.Name("fox"), Tok.Eof]:
        stdout.print(describe(tok))
    for n in [0, 2, -4, 40]:
        stdout.print(n, size(n))
    stdout.print(first_word("quick brown fox"), first_word(""), first_word(None))
```

It prints:

```text
a number
a number
the keyword match
the name fox
the end
0 none
2 a few
-4 less than none
40 lots
quick (nothing) (nothing)
```

## More examples

### `arithmetic`

```fennec
def main(stdout: Stdout):
    # `//` rounds down and `%` takes the divisor's sign, as in Python, whatever the signs.
    stdout.print(7 // 2, 7 // -2, -7 // 2, -7 // -2, 6 // -3)
    stdout.print(7 % 2, 7 % -2, -7 % 2, -7 % -2, 6 % -3)
    stdout.print(7.0 // -2.0, 7.0 % -2.0, -7.5 // 2.0, -7.5 % 2.0, 7.5 / 2.0, 7.5 - 2.0, 2.0 ** 0.5)

    # `+` joins strings and lists.
    stdout.print("fen" + "nec", [1, 2] + [3])

    # Comparisons.
    stdout.print(
        1 != 2,
        2 != 2,
        "a" != "b",
        True > False,
        False < True,
        (1, 2) < (1, 3),
        [1] < [1, 0],
    )

    # Changing a name in place.
    mut n = 6
    n *= 7
    n -= 2
    mut x = 1.0
    x /= 4.0
    stdout.print(n, x)
```

It prints:

```text
3 -4 -4 3 -2
1 -1 1 -1 0
-4.0 -1.0 -4.0 0.5 3.75 5.5 1.4142135623730951
fennec [1, 2, 3]
True False True True True True True
40 0.25
```

### `bench_blocks`

```fennec
def total(count: int) -> int:
    mut sum_so_far = 0
    for at in range(count):
        sum_so_far += at
    sum_so_far

test "a total adds everything":
    assert total(4) == 6

bench "adding up a thousand numbers":
    assert total(1000) == 499500

def main(stdout: Stdout):
    stdout.print(total(10))
```

It prints:

```text
45
```

### `bits`

```fennec
# Bit operators work on ints. A shift by a count outside 0 to 63 stops the program.

def flags(a: int, b: int) -> list[int]:
    [a & b, a | b, a ^ b, ~a, a << 3, a >> 1]

def hash_step(start: int, byte: int) -> int:
    mut hash = start
    hash ^= byte
    hash <<= 5
    hash |= byte & 15
    hash >>= 1
    hash &= 1048575
    hash

def main(stdout: Stdout):
    stdout.print(flags(12, 10))
    stdout.print(flags(-12, 10))
    # Tighter than comparisons and looser than `+`, as in Python.
    stdout.print(1 | 2 ^ 3 & 4, 1 + 2 << 3, 6 & 3 == 2, ~1 + 1)
    # Bits shifted past the top are lost: `<<` doesn't overflow.
    stdout.print(1 << 63, 3 << 63, -1 >> 63, -8 >> 1)
    mut hash = 7
    for byte in [1, 200, 33]:
        hash = hash_step(hash, byte)
    stdout.print(hash)
    # A mask of the low 8 bits.
    mask = (1 << 8) - 1
    stdout.print(1000 & mask, 1000 >> 8)
```

It prints:

```text
[8, 14, 6, -13, 96, 6]
[0, -2, -2, 11, -96, -6]
3 24 True -1
-9223372036854775808 -9223372036854775808 -1 -4
43600
232 3
```

### `calculator`

```fennec
# A calculator over syntax trees, compiled to plain machine code: the trees are boxed,
# `match` is a switch, and `==` compares trees and plain values natively.

struct Range:
    low: int
    high: int

enum Sign:
    Plus
    Minus

enum Calc:
    Lit(value: int)
    Arg
    Op(sign: Sign, left: Calc, right: Calc)
    Times(left: Calc, right: Calc)
    Clamp(range: Range, inner: Calc)

def run(calc: Calc, arg: int) -> int:
    match calc:
        case .Lit(n):
            n
        case .Arg:
            arg
        case .Op(.Plus, a, b):
            run(a, arg) + run(b, arg)
        case .Op(_, a, b):
            run(a, arg) - run(b, arg)
        case .Times(a, b):
            run(a, arg) * run(b, arg)
        case .Clamp(Range(-50, high), inner):
            min(max(run(inner, arg), -50), high)
        case .Clamp(range, inner):
            min(max(run(inner, arg), range.low), range.high)

def fold(calc: Calc) -> Calc:
    match calc:
        case .Times(.Lit(0), _):
            .Lit(0)
        case .Times(.Lit(1), b):
            fold(b)
        case .Op(.Plus, .Lit(0), b):
            fold(b)
        case .Op(sign, a, b):
            .Op(sign, fold(a), fold(b))
        case .Times(a, b):
            .Times(fold(a), fold(b))
        case .Clamp(range, inner):
            .Clamp(range, fold(inner))
        case other:
            other

def grow(depth: int, seed: int) -> Calc:
    if depth == 0: return Calc.Arg if seed % 3 == 0 else .Lit(seed % 7)
    a = grow(depth - 1, seed * 5 + 1)
    b = grow(depth - 1, seed * 3 + 2)
    match seed % 6:
        case 0:
            .Op(.Plus, a, b)
        case 1:
            .Op(.Minus, .Lit(0), b)
        case 2:
            .Times(.Lit(1), a)
        case 3:
            .Clamp(Range(low=-50, high=50), .Times(a, b))
        case 4:
            .Times(.Lit(0), a)
        case _:
            .Op(.Minus, a, b)

def main(stdout: Stdout):
    calc = grow(10, 1)
    folded = fold(calc)
    mut same = 0
    for arg in range(-5, 6):
        if run(calc, arg) == run(folded, arg):
            same += 1
    stdout.print(same, run(calc, 3), run(folded, 3))
    stdout.print(fold(folded) == folded, calc == folded, grow(10, 1) == calc, calc != grow(10, 2))
    stdout.print(Range(low=1, high=2) == Range(low=1, high=2), Sign.Plus == .Minus)
```

It prints:

```text
11 65 65
True False True True
True False
```

### `collections`

```fennec
def main(stdout: Stdout):
    point = (3, 4)
    x, y = point
    stdout.print(point, x + y)

    quotient, remainder = divmod(17, 5)
    stdout.print(quotient, remainder)

    hp = {"ash": 70, "kit": 40}
    mut roster = hp
    roster["vex"] = 90
    roster["kit"] += 5
    stdout.print(hp)
    stdout.print(roster, roster.len(), "vex" in roster)
    stdout.print(roster.get("nobody"), roster.get("nobody", 0))

    for name, health in roster.items():
        stdout.print(f"{name}: {health}")

    strong = {name: health for name, health in roster.items() if health > 50}
    stdout.print(strong)

    mut seen = {"fire", "ice"}
    seen.add("fire")
    seen.add("storm")
    stdout.print(seen.len(), "ice" in seen)
    stdout.print({n % 3 for n in range(10)})

    letters = "fennec"
    stdout.print(letters[1:4], letters[-3:], [1, 2, 3, 4][:2])
    for at, name in enumerate(["a", "b"]):
        stdout.print(at, name)
    stdout.print(list(zip([1, 2], ["one", "two"])))

def divmod(a: int, b: int) -> (int, int):
    (a // b, a % b)
```

It prints:

```text
(3, 4) 7
3 2
{"ash": 70, "kit": 40}
{"ash": 70, "kit": 45, "vex": 90} 3 True
None 0
ash: 70
kit: 45
vex: 90
{"ash": 70, "vex": 90}
3 True
{0, 1, 2}
enn nec [1, 2]
0 a
1 b
[(1, "one"), (2, "two")]
```

### `collections_more`

```fennec
from std.game import IdMap

def describe(result: Result[str, IoError]) -> str:
    match result:
        case Ok(text):
            f"read {text.len()} characters"
        case Err(_):
            "couldn't do it"

def main(stdout: Stdout, fs: Fs):
    # Lists.
    mut items = [3, 1, 4, 1, 5]
    stdout.print(items.index(4), items.index(9), items.count(1), items.count(7))
    stdout.print(items.pop(0), items)
    stdout.print(items[0:2], (1, 2, 3)[1:], (1, 2, 3)[-2:-1])
    items.extend([9, 2])
    items.remove(1)
    stdout.print(items)
    items.reverse()
    stdout.print(items)
    items.sort()
    stdout.print(items)
    items.sort(key=n => -n)
    stdout.print(items)
    items.remove_where(n => n > 4)
    stdout.print(items)
    items.clear()
    stdout.print(items)

    # Dicts.
    mut hp = {"ash": 70, "kit": 40, "vex": 90}
    stdout.print(hp["kit"], hp.get("vex"), hp.keys(), hp.values())
    stdout.print(hp.pop("kit"), hp, "vex" in hp, "kit" in hp)
    hp.clear()
    stdout.print(hp, hp.get("ash"))

    # Sets.
    mut seen = {"fire", "ice"}
    seen.add("ice")
    seen.add("storm")
    seen.discard("ice")
    seen.discard("mud")
    seen.remove("fire")
    stdout.print(seen)
    seen.clear()
    stdout.print(seen)

    # IdMap.
    mut enemies = IdMap[str]()
    first = enemies.insert("ash")
    enemies.insert("kit")
    enemies.insert("vex")
    for id, name in enemies.items():
        stdout.print(name, id == first)
    stdout.print(enemies.ids().len(), enemies[first])
    enemies.remove_where(name => name.startswith("k"))
    stdout.print(enemies.values())
    enemies.clear()
    stdout.print(enemies.values(), enemies.get(first))

    # Strings.
    text = "  fire ice  "
    stdout.print(f"[{text.lstrip()}]", f"[{text.rstrip()}]")
    stdout.print("one\ntwo".lines(), "a,b,,c".split(","), "fennec".startswith("fen"))
    stdout.print(
        "fennec".replace("n", "N"),
        "fennec".find("nec"),
        "fennec".find("fox"),
        "äbc".find("c"),
    )
    stdout.print("nec" in "fennec", "fox" in "fennec", "fox" not in "fennec")
    stdout.write("no newline, ")
    stdout.write("then one\n")

    # Files. Paths in a folder that doesn't exist fail without touching the disk.
    stdout.print(fs.exists("no/such/folder/save.txt"))
    stdout.print(describe(fs.read_text("no/such/folder/save.txt")))
    match fs.write_text("no/such/folder/save.txt", "saved"):
        case Ok(_):
            stdout.print("wrote it")
        case Err(_):
            stdout.print("couldn't write it")
```

It prints:

```text
2 None 2 0
3 [1, 4, 1, 5]
[1, 4] (2, 3) (2,)
[4, 1, 5, 9, 2]
[2, 9, 5, 1, 4]
[1, 2, 4, 5, 9]
[9, 5, 4, 2, 1]
[4, 2, 1]
[]
40 90 ["ash", "kit", "vex"] [70, 40, 90]
40 {"ash": 70, "vex": 90} True False
{} None
{"storm"}
set()
ash True
kit False
vex False
3 ash
["ash", "vex"]
[] None
[fire ice  ] [  fire ice]
["one", "two"] ["a", "b", "", "c"] True
feNNec 3 None 2
True False True
no newline, then one
False
couldn't do it
couldn't write it
```

### `comprehensions_native`

```fennec
## Comprehensions and sums in the middle of expressions, over everything they can go over.

struct Point:
    x: int
    y: float

def pick(values: list[int], limit: int) -> list[int]:
    return [v for v in values if v < limit]

def main(stdout: Stdout):
    numbers = [i * 3 for i in range(10)]
    evens = [n for n in numbers if n % 2 == 0]
    stdout.print(numbers, evens, sum(evens), sum(n for n in numbers if n > 10))
    stdout.print(
        sorted(numbers),
        sum([1, 2, 3]),
        sum([0.5, 1.5]),
        sum(float(n) for n in evens),
        sum([], 7),
    )
    stdout.print(
        [p.x for p in [Point(x=1, y=2.0), Point(x=3, y=4.0)]],
        [Point(x=n, y=0.5) for n in range(2)],
    )
    stdout.print(
        {n % 3: n for n in numbers},
        {str(n): n > 3 for n in range(5)},
        {n: float(n) for n in [1, 2]},
    )
    stdout.print(
        [c + "!" for c in "héllo"],
        [k for k in {"a": 1, "b": 2}],
        [v * 2 for v in {"a": 1, "b": 2}.values()],
    )
    stdout.print(pick(numbers, 7), [[j for j in range(i)] for i in range(3)])
    n = 100
    stdout.print([n for n in range(3)], n, sum(n * 2 for n in range(4)), n)
    stdout.print(2 ** 10, (-3) ** 3, 7 ** 0, 2.0 ** 0.5, [2 ** k for k in range(5)])
    mut grows = [1, 2]
    copies = [g * 10 for g in grows]
    grows.append(3)
    stdout.print(grows, copies, 2 ** 62)

    letters = [1.5, 2.5, 3.5]
    for i, x in enumerate(letters):
        stdout.print(i, x)
    for a, b in zip(letters, numbers):
        stdout.print(a, b)
    stdout.print(
        sum(a * float(b) for a, b in zip(letters, numbers)),
        [i * 2 for i, _ in enumerate(letters)],
    )
    points = [Point(x=5, y=0.5), Point(x=6, y=1.5)]
    for at, point in enumerate(points):
        stdout.print(at, point.x, point.y)
    for short, long in zip([1], [1, 2]):
        stdout.print(short, long)
    i = 9
    for i, x in enumerate(letters):
        stdout.print(i, x)
    stdout.print(i)
```

It prints:

```text
[0, 3, 6, 9, 12, 15, 18, 21, 24, 27] [0, 6, 12, 18, 24] 60 117
[0, 3, 6, 9, 12, 15, 18, 21, 24, 27] 6 2.0 60.0 7
[1, 3] [Point(x=0, y=0.5), Point(x=1, y=0.5)]
{0: 27} {"0": False, "1": False, "2": False, "3": False, "4": True} {1: 1.0, 2: 2.0}
["h!", "é!", "l!", "l!", "o!"] ["a", "b"] [2, 4]
[0, 3, 6] [[], [0], [0, 1]]
[0, 1, 2] 100 12 100
1024 -27 1 1.4142135623730951 [1, 2, 4, 8, 16]
[1, 2, 3] [10, 20] 4611686018427387904
0 1.5
1 2.5
2 3.5
1.5 0
2.5 3
3.5 6
28.5 [0, 2, 4]
0 5 0.5
1 6 1.5
1 1
0 1.5
1 2.5
2 3.5
9
```

### `default_params`

```fennec
# A parameter can have a default: a plain value, passed by name if at all.

def repeat(text: str, times: int = 2, between: str = "") -> str:
    mut out = ""
    for at in range(times):
        if at > 0:
            out += between
        out += text
    out

def offset(by: int = -1, scale: float = -0.5, label: str? = None) -> str:
    shown = label else "none"
    f"{by} {scale} {shown}"

struct Tally:
    count: int

    def bump(mut self, by: int = 1, again: bool = False):
        self.count += by
        if again:
            self.count += by

def main(stdout: Stdout):
    stdout.print(repeat("ab"))
    stdout.print(repeat("ab", times=3))
    stdout.print(repeat("ab", between="-", times=3))
    stdout.print(repeat(text="cd", between="+"))
    stdout.print(offset(), offset(label="x"), offset(scale=2.0, by=4))
    mut tally = Tally(count=0)
    tally.bump()
    tally.bump(by=5)
    tally.bump(again=True, by=10)
    stdout.print(tally.count)
```

It prints:

```text
abab
ababab
ab-ab-ab
cd+cd
-1 -0.5 none -1 -0.5 x 4 2.0 none
26
```

### `dicts_large`

```fennec
## Dicts of many keys that native code holds as objects, found through a hash table.

struct Cell:
    x: int
    y: int

def main(stdout: Stdout):
    mut seen: dict[Cell, int] = {}
    for i in range(500):
        cell = Cell(x=i % 31, y=i % 17)
        seen[cell] = seen.get(cell, 0) + 1
    stdout.print(
        seen.len(),
        seen[Cell(x=0, y=0)],
        Cell(x=99, y=0) in seen,
        seen.get(Cell(x=99, y=0), -1),
    )

    mut by_pair: dict[(int, str), list[int]] = {}
    for i in range(200):
        key = (i % 20, f"k{i % 3}")
        if key not in by_pair:
            by_pair[key] = []
        by_pair[key].append(i)
    stdout.print(by_pair.len(), by_pair[(1, "k1")], (19, "k9") in by_pair)
    popped = by_pair.pop((1, "k1"))
    stdout.print(popped.len(), (1, "k1") in by_pair, by_pair.len())
    by_pair[(1, "k1")] = [7]
    stdout.print(by_pair[(1, "k1")], by_pair.keys()[by_pair.len() - 1])

    copy = by_pair
    by_pair[(2, "new")] = [1]
    stdout.print(copy.len(), by_pair.len(), copy == by_pair, copy == copy, (2, "new") in copy)

    mut floats: dict[float, int] = {}
    for i in range(40):
        floats[float(i) / 2.0] = i
    stdout.print(floats[1.5], floats.len(), floats.get(99.0, 0), floats.get(-0.0, 5))
    built = {i % 25: i for i in range(100)}
    stdout.print(built.len(), built[3], list(built.keys())[:3])
    by_pair.clear()
    stdout.print(by_pair.len(), (1, "k1") in by_pair)
```

It prints:

```text
500 1 False -1
60 [1, 61, 121, 181] False
4 False 59
[7] (1, "k1")
60 61 False True False
3 40 0 5
25 78 [0, 1, 2]
0 False
```

### `early_returns`

```fennec
## Small functions that return early, called directly and recursively.

def down(n: int) -> int:
    if n <= 0: return 0
    return down(n - 1) + 1

def fib(n: int) -> int:
    if n < 2: return n
    fib(n - 1) + fib(n - 2)

def sign(n: int) -> int:
    if n > 0:
        return 1
    elif n < 0:
        return -1
    return 0

def clamp(x: float) -> float:
    if x > 1.0: return 1.0
    if x < 0.0: return 0.0
    x

def tell(n: int, stdout: Stdout):
    if n > 2:
        stdout.print("big")
        return
    stdout.print("small")

def narrowed(n: int) -> int:
    if not (n > 5 and n < 10): return n - 9_223_372_036_854_775_807
    n + 9_223_372_036_854_775_797

def main(stdout: Stdout):
    stdout.print(down(50), fib(15), sign(-4), sign(4), sign(0))
    stdout.print(clamp(2.5), clamp(-1.0), clamp(0.25))
    tell(3, stdout)
    tell(1, stdout)
    stdout.print(narrowed(7), narrowed(9), narrowed(-1), narrowed(10))
```

It prints:

```text
50 610 -1 1 0
1.0 0.0 0.25
big
small
9223372036854775804 9223372036854775806 -9223372036854775808 -9223372036854775797
```

### `enum_methods`

```fennec
# Enums with methods, enums that come back from the runtime, inside structs and dicts, and
# changed in place in a list. `match` also takes apart structs and bools natively.

struct Cell:
    row: int
    col: int

enum Light:
    Red
    Amber(blinking: bool)
    Green(seconds: float)

    def next(self) -> Light:
        match self:
            case .Red:
                .Green(30.0)
            case .Green(_):
                .Amber(False)
            case .Amber(True):
                .Red
            case .Amber(_):
                .Amber(True)

    def wait(self) -> float:
        match self:
            case .Green(seconds):
                seconds
            case _:
                1.5

    def cycle(mut self):
        self = self.next()

struct Junction:
    at: Cell
    light: Light

def main(stdout: Stdout):
    mut light = Light.Red
    for i in range(5):
        stdout.print(light, light.wait())
        light.cycle()
    lights = {"a": Light.Green(2.5), "b": .Amber(True)}
    got = lights["a"]
    stdout.print(got, got.wait(), got == .Green(2.5), got.next())
    junctions = {"j": Junction(at=Cell(1, 2), light=.Amber(False))}
    j = junctions["j"]
    stdout.print(j.light.next(), j.at.row, j)
    mut all = [Light.Red, .Green(1.0), .Amber(False)]
    for mut l in all:
        l = l.next()
    stdout.print(all)
    all.append(.Green(-0.0))
    stdout.print(all[-1] == .Green(0.0), all[-1] == .Green(-0.0), all.len())
    cell = Cell(3, 4)
    picked = match cell:
        case Cell(0, c):
            c
        case Cell(r, 4):
            r * 100
        case _:
            -1
    stdout.print(picked)
    flag = cell.row > 2
    said = match flag:
        case True:
            "yes"
        case False:
            "no"
    stdout.print(said)
    stdout.print(Junction(at=cell, light=.Red) == Junction(at=Cell(3, 4), light=Light.Red))
```

It prints:

```text
Light.Red 1.5
Light.Green(seconds=30.0) 30.0
Light.Amber(blinking=False) 1.5
Light.Amber(blinking=True) 1.5
Light.Red 1.5
Light.Green(seconds=2.5) 2.5 True Light.Amber(blinking=False)
Light.Amber(blinking=True) 1 Junction(at=Cell(row=1, col=2), light=Light.Amber(blinking=False))
[Light.Green(seconds=30.0), Light.Amber(blinking=False), Light.Amber(blinking=True)]
False True 4
300
yes
True
```

### `enum_values`

```fennec
# Enums whose cases hold numbers, bools and plain structs are values like any other: a
# copy never changes when the original does, and `match` takes them apart by case and by
# what's inside.

struct Cell:
    row: int
    col: int

enum Dir:
    North
    East
    South
    West

enum Move:
    Step(dir: Dir, count: int)
    Jump(to: Cell)
    Wait(turns = 1)
    Quit

enum Token:
    Number(value: int)
    Decimal(value: float)
    Flag(on: bool)
    Plus
    Minus
    End

struct Robot:
    at: Cell
    facing: Dir
    last: Move

def turn(dir: Dir) -> Dir:
    match dir:
        case .North:
            .East
        case .East:
            .South
        case .South:
            .West
        case .West:
            .North

def step(at: Cell, dir: Dir, count: int) -> Cell:
    match dir:
        case .North:
            Cell(row=at.row - count, col=at.col)
        case .South:
            Cell(row=at.row + count, col=at.col)
        case .East:
            Cell(row=at.row, col=at.col + count)
        case .West:
            Cell(row=at.row, col=at.col - count)

def apply(robot: Robot, move: Move) -> Robot:
    match move:
        case .Step(dir, count):
            Robot(at=step(robot.at, dir, count), facing=dir, last=move)
        case .Jump(Cell(row, col)):
            Robot(at=Cell(row=row, col=col), facing=robot.facing, last=move)
        case _:
            Robot(at=robot.at, facing=turn(robot.facing), last=move)

def describe(move: Move) -> str:
    match move:
        case .Step(.North, 1):
            "one step north"
        case .Step(.North, count):
            f"{count} steps north"
        case .Step(_, 0):
            "standing still"
        case .Step(dir, count):
            f"{count} steps {dir}"
        case .Jump(Cell(0, 0)):
            "back home"
        case .Jump(to):
            f"jump to {to.row},{to.col}"
        case .Wait(turns):
            f"wait {turns}"
        case .Quit:
            "quit"

def score(tokens: list[Token]) -> int:
    mut total = 0
    mut sign = 1
    for token in tokens:
        match token:
            case .Number(value):
                total += sign * value
            case .Decimal(value):
                total += sign * int(value * 10.0)
            case .Flag(True):
                total *= 2
            case .Flag(_):
                pass
            case .Plus:
                sign = 1
            case .Minus:
                sign = -1
            case .End:
                break
    total

def main(stdout: Stdout):
    moves = [
        .Step(.North, 1),
        .Step(.North, 3),
        Move.Step(dir=.East, count=0),
        .Step(.West, 2),
        .Jump(Cell(0, 0)),
        .Jump(to=Cell(row=4, col=-1)),
        .Wait(),
        .Wait(turns=3),
        Move.Quit,
    ]
    for move in moves:
        stdout.print(describe(move))

    mut robot = Robot(at=Cell(row=0, col=0), facing=.North, last=.Quit)
    start = robot
    for move in moves:
        robot = apply(robot, move)
    stdout.print(robot)
    stdout.print(start)

    # Copies are independent, and `==` compares case and contents.
    mut a = Move.Step(.South, 2)
    b = a
    a = .Step(.South, 3)
    stdout.print(a == b, a != b, b == .Step(.South, 2), b == Move.Quit, Move.Wait() == .Wait(1))
    stdout.print(Token.Decimal(0.0) == .Decimal(-0.0), Token.Decimal(1.5) == .Decimal(1.5))

    tokens = [
        .Number(4),
        .Plus,
        .Decimal(2.5),
        .Flag(True),
        .Minus,
        .Number(3),
        .Flag(False),
        .End,
        .Number(100),
    ]
    stdout.print(score(tokens), tokens.len(), tokens[2], tokens[-1])

    mut path = [Dir.North]
    for i in range(6):
        path.append(turn(path[-1]))
    stdout.print(path)
    stdout.print(path[0] == path[4], path[1] == path[2])
```

It prints:

```text
one step north
3 steps north
standing still
2 steps Dir.West
back home
jump to 4,-1
wait 1
wait 3
quit
Robot(at=Cell(row=4, col=-1), facing=Dir.South, last=Move.Quit)
Robot(at=Cell(row=0, col=0), facing=Dir.North, last=Move.Quit)
False True True False True
False True
55 9 Token.Decimal(value=2.5) Token.Number(value=100)
[Dir.North, Dir.East, Dir.South, Dir.West, Dir.North, Dir.East, Dir.South]
True False
```

### `equality`

```fennec
from std.game import IdMap

enum Light:
    Red
    Green

enum Coin:
    Heads
    Tails

def square(n: int) -> int:
    n * n

def cube(n: int) -> int:
    n * n * n

def main(stdout: Stdout):
    # Sets and dicts are equal when they hold the same things, in any order.
    stdout.print({1, 2} == {2, 1}, {1, 2} == {1, 3}, {1} == {1, 2})
    stdout.print(
        {"a": 1, "b": 2} == {"b": 2, "a": 1},
        {"a": 1} == {"a": 2},
        {"a": 1} == {"a": 1, "b": 2},
    )

    mut left = IdMap[str]()
    mut right = IdMap[str]()
    left.insert("ash")
    right.insert("ash")
    stdout.print(left == right)
    right.insert("kit")
    stdout.print(left == right)

    # Functions are equal only to themselves. Types and builtins aren't values, and arrow
    # functions are only passed straight to calls, so neither is ever compared.
    stdout.print(square == square, square == cube)
```

It prints:

```text
True False False
True False False
True
False
True False
```

### `errors_and_none`

```fennec
error NotFound:
    name: str

error TooWeak:
    level: int

struct Player:
    name: str
    level = 1

def find(players: list[Player], name: str) -> Player?:
    for player in players:
        if player.name == name: return player
    None

def require(players: list[Player], name: str) -> Player:
    player = find(players, name)
    if player is None: fail NotFound(name)
    player

def challenge(players: list[Player], name: str) -> str:
    player = require(players, name)?
    if player.level < 5: fail TooWeak(player.level)
    f"{player.name} accepts the challenge"

def main(stdout: Stdout):
    players = [Player(name="Ash", level=7), Player(name="Kit")]

    stdout.print(find(players, "Kit")?.level)
    stdout.print(find(players, "Vex")?.level)
    stdout.print(find(players, "Vex")?.level else 0)

    for name in ["Ash", "Kit", "Vex"]:
        match challenge(players, name):
            case Ok(message):
                stdout.print(message)
            case TooWeak(level):
                stdout.print(f"{name} is only level {level}")
            case Err(error):
                stdout.print(f"no luck: {error}")

    nobody = Player(name="nobody", level=-1)
    level = (require(players, "Vex") else nobody).level
    stdout.print(level)
    found = require(players, "Ash").expect("Ash was added above")
    stdout.print(found.name)
    stdout.print(challenge(players, "Kit") == Err(TooWeak(level=1)))
```

It prints:

```text
1
None
0
Ash accepts the challenge
Kit is only level 1
no luck: NotFound(name="Vex")
-1
Ash
True
```

### `fallible_blocks`

```fennec
## Fallible functions that give back strings, lists and structs, used every way a result can be.

error TooBig:
    limit: int

error Wrapped:
    why: str

struct Node:
    value: int
    next: Node?
    names: list[str]

def word(n: int) -> str:
    if n > 2: fail TooBig(limit=2)
    f"w{n}"

def words(n: int) -> list[str]:
    mut all = []
    for i in range(n):
        all.append(word(i)?)
    all

def node(n: int) -> Node:
    if n > 3: fail TooBig(limit=3)
    if n == 0: return Node(value=0, next=None, names=[])
    inner = node(n - 1)?
    Node(value=n, next=inner, names=words(n)?)

def wrapped(n: int) -> str:
    text = word(n)? from Wrapped(why="word failed")
    text + "!"

struct Maker:
    made = 0

    def make(mut self, n: int) -> str:
        self.made += 1
        if n > 2: fail TooBig(limit=n)
        word(n)?

    def twice(mut self, n: int) -> list[str]:
        [self.make(n)?, self.make(n + 1)?]

def main(stdout: Stdout):
    stdout.print(word(1) else "none", word(9) else "none")
    stdout.print(words(2) else [], words(9) else ["failed"])
    stdout.print(
        (node(2) else Node(value=-1, next=None, names=[])).value,
        (node(9) else Node(value=-1, next=None, names=[])).value,
    )
    stdout.print(wrapped(1) else "failed", wrapped(9) else "failed")

    match word(1):
        case Ok(found):
            stdout.print("ok", found)
        case Err(problem):
            stdout.print("err", problem)
    match word(5):
        case Ok(found):
            stdout.print("ok", found)
        case Err(problem):
            stdout.print("err", problem)
    match wrapped(5):
        case Ok(found):
            stdout.print("ok", found)
        case Err(problem):
            stdout.print("err", problem)

    result = words(1)
    stdout.print(result)
    stdout.print(words(9))

    mut maker = Maker()
    stdout.print(maker.twice(1), maker.made)
    stdout.print(maker.twice(2), maker.made)
    stdout.print(maker.twice(5), maker.made)
```

It prints:

```text
w1 none
["w0", "w1"] ["failed"]
2 -1
w1! failed
ok w1
err TooBig(limit=2)
err Wrapped(why="word failed")
Ok(["w0"])
Err(TooBig(limit=2))
Ok(["w1", "w2"]) 2
Err(TooBig(limit=3)) 4
Err(TooBig(limit=5)) 5
```

### `fields_native`

```fennec
## Structs that hold lists and dicts, read and changed where they are, native.

struct Point:
    x: int
    y: float

struct Item:
    id: int
    tags: list[str]
    near: list[int]

struct Bag:
    items: list[Item]
    points: list[Point]
    counts: dict[str, int]
    numbers: list[int]
    spot: Point
    name: str
    total = 0

    def add(mut self, id: int, tag: str) -> int:
        self.items.append(Item(id=id, tags=[tag], near=[]))
        self.total += id
        self.items.len() - 1

    def tag_of(self, at: int) -> str:
        self.items[at].tags[0]

    def bump(mut self) -> int:
        self.total += 1
        self.total

    def link(mut self, a: int, b: int):
        self.items[a].near.append(b)
        self.items[b].near.append(a)

    def first_big(mut self) -> int:
        for mut point in self.points:
            if point.x > 3:
                point.x = 0
                return point.x + 1
        -1

    def mean_near(mut self) -> int:
        for mut point in self.points:
            point.x += self.numbers.len() + self.points.len()
        self.numbers.remove_where((n) => n == self.numbers.len())
        self.items[0].near.remove_where((n) => n > self.items.len())
        self.points[0].x

enum Shape:
    Dot
    Line(length: int)
    Group(parts: list[Shape], names: list[str])

def describe(shape: Shape) -> int:
    match shape:
        case .Dot:
            1
        case .Line(length):
            length * 10
        case .Group(parts, names):
            mut total = names.len()
            for part in parts:
                total += describe(part)
            total

def kind_of(shape: Shape) -> str:
    match shape:
        case .Dot | .Line(_):
            "small"
        case _:
            "big"

def fresh() -> Bag:
    Bag(
        items=[],
        points=[Point(x=1, y=0.5)],
        counts={},
        numbers=[3, 1, 2],
        spot=Point(x=0, y=0.0),
        name="bag",
    )

def total_near(bag: Bag) -> int:
    mut sum_of = 0
    for item in bag.items:
        sum_of += item.near.len()
    sum_of

def main(stdout: Stdout):
    mut bag = fresh()
    stdout.print(bag.add(5, "five"), bag.add(7, "seven"), bag.total, bag.tag_of(1))
    bag.link(0, 1)
    stdout.print(total_near(bag), bag.items[0].near, bag.items[1].tags)

    bag.points[0].x = 4
    bag.points[0].y += 1.5
    bag.points.append(Point(x=9, y=9.0))
    bag.numbers[0] = 8
    bag.numbers[1] += 10
    bag.counts["a"] = 1
    bag.counts["a"] += 1
    bag.counts["b"] = bag.counts["a"] * 3
    bag.spot.x = 6
    bag.spot.y = 2.5
    bag.name = bag.name + "!"
    bag.items[0].id = 50
    bag.items[1].tags[0] = "VII"
    bag.items[1].tags.append("more")
    bag.items[bag.bump() - bag.total].id += 1
    stdout.print(bag.points[0].x, bag.points[0].y, bag.points.len(), bag.numbers, bag.counts)
    stdout.print(bag.spot.x, bag.spot.y, bag.name, bag.items[0].id, bag.items[1].tags, bag.total)

    copy = bag
    bag.items[0].near.append(100)
    bag.numbers.append(4)
    bag.counts["c"] = 1
    bag.items[1].id = -1
    stdout.print(copy.items[0].near, bag.items[0].near, copy.numbers, bag.numbers)
    stdout.print(
        copy.counts.len(),
        bag.counts.len(),
        copy.items[1].id,
        bag.items[1].id,
        copy == bag,
    )

    stdout.print(bag.numbers.pop(), bag.numbers, bag.items.pop().id, bag.items.len())
    bag.numbers.insert(0, 42)
    bag.numbers.remove(8)
    bag.numbers.sort()
    stdout.print(bag.numbers, sorted(bag.numbers), 42 in bag.numbers, "a" in bag.counts)
    bag.numbers.remove_where((n) => n > 40)
    stdout.print(bag.numbers, bag.counts.get("zz", 0), bag.counts.get("a", 0))
    for key in bag.counts:
        stdout.print(key)
    for point in bag.points:
        stdout.print(point.x)
    for mut point in bag.points:
        point.x += 1
    stdout.print(bag.points[1].x, sum(bag.numbers), [n * 2 for n in bag.numbers])
    floats = [0.0, 1.5]
    stdout.print(0.0 in floats, -0.0 in floats, 1.5 in floats, 2.5 not in floats)
    bag.numbers.clear()
    stdout.print(bag.numbers, bag)
    stdout.print(fresh().numbers[2], fresh().name.len())
    mut more = fresh()
    more.numbers.append(4)
    more.points.append(Point(x=7, y=1.0))
    more.add(1, "one")
    more.items[0].near.append(50)
    stdout.print(more.first_big(), more.points, more.points.len())
    stdout.print(more.mean_near(), more.numbers, more.items[0].near, more.points)
    more.points.pop()
    stdout.print(more.points, more.first_big())
    stdout.print(
        describe(Shape.Group(parts=[Shape.Dot, Shape.Line(length=2)], names=["a"])),
        describe(Shape.Dot),
    )
    stdout.print(
        kind_of(Shape.Dot),
        kind_of(Shape.Line(length=1)),
        kind_of(Shape.Group(parts=[], names=[])),
    )
```

It prints:

```text
0 1 12 seven
2 [1] ["seven"]
4 2.0 2 [8, 11, 2] {"a": 2, "b": 6}
6 2.5 bag! 51 ["VII", "more"] 13
[1] [1, 100] [8, 11, 2] [8, 11, 2, 4]
2 3 7 -1 False
4 [8, 11, 2] -1 1
[2, 11, 42] [2, 11, 42] True True
[2, 11] 0 2
a
b
c
4
9
10 13 [4, 22]
True False True True
[] Bag(items=[Item(id=51, tags=["five"], near=[1, 100])], points=[Point(x=5, y=2.0), Point(x=10, y=9.0)], counts={"a": 2, "b": 6, "c": 1}, numbers=[], spot=Point(x=6, y=2.5), name="bag!", total=13)
2 3
1 [Point(x=1, y=0.5), Point(x=0, y=1.0)] 2
7 [3, 1, 2] [] [Point(x=7, y=0.5), Point(x=6, y=1.0)]
[Point(x=7, y=0.5)] 1
22 1
small small big
```

### `fields_of_any_type`

```fennec
## Structs and enums holding fields native code can't hold itself: optionals, tuples,
## nested lists. The rest of the struct stays native.

struct Inner:
    label: str
    note: str?

struct Holder:
    count: int
    maybe: Inner?
    pair: (int, str)
    grid: list[list[int]]
    words: list[str?]
    total = 0

    def bump(mut self) -> int:
        self.count += 1
        self.total += self.count
        self.total

    def label(self) -> str:
        self.maybe?.label else "none"

    def set_inner(mut self, label: str):
        self.maybe = Inner(label=label, note=None)

enum Shape:
    Dot
    Named(name: str?, parts: list[Shape])
    Pair(at: (int, int))

def name_of(shape: Shape) -> str:
    match shape:
        case .Dot:
            "dot"
        case .Named(name, parts):
            shown = name else "anon"
            f"{shown}:{parts.len()}"
        case .Pair(at):
            x, y = at
            f"{x},{y}"

def maybe(holder: Holder, given: bool) -> Holder?:
    if given: return holder
    None

def fresh() -> Holder:
    Holder(count=1, maybe=None, pair=(1, "one"), grid=[[1, 2], [3]], words=["a", None])

def main(stdout: Stdout):
    mut holder = fresh()
    stdout.print(holder.count, holder.maybe, holder.pair, holder.grid, holder.words, holder.label())
    stdout.print(holder.bump(), holder.bump(), holder.total)
    holder.set_inner("kit")
    stdout.print(holder.label(), holder.maybe?.note, holder.maybe)

    copy = holder
    holder.pair = (2, "two")
    holder.grid[0][1] = 9
    holder.grid.append([7])
    holder.words.append("b")
    holder.count = 100
    stdout.print(copy.pair, holder.pair, copy.grid, holder.grid, copy.words, holder.words)
    stdout.print(copy.count, holder.count, copy == holder, copy == copy)
    x, y = holder.pair
    stdout.print(x, y, holder.grid[1], holder.grid[0][1], holder.words[2])
    for row in holder.grid:
        stdout.print(row.len())
    holder.maybe = None
    stdout.print(holder.label(), holder)

    for shape in [
        Shape.Dot,
        Shape.Named(name=None, parts=[]),
        Shape.Named(name="n", parts=[Shape.Dot]),
        Shape.Pair(at=(1, 2)),
    ]:
        stdout.print(name_of(shape), shape)
    mut holders = [fresh(), fresh()]
    holders[0].pair = (5, "five")
    holders[1].bump()
    stdout.print(holders[0].pair, holders[1].pair, holders[1].total)

    # A value handed over as an object stays as it was, whatever native code does next.
    mut original = fresh()
    original.grid = [[1]]
    held = maybe(original, True)
    kept = (original, 5)
    original.count = 77
    original.grid.append([2])
    original.words.append("later")
    stdout.print(held?.count, held?.grid, original.count, original.grid, kept)
    stdout.print(held is None, held is not None, held == original, held?.pair)
    missing = maybe(original, False)
    stdout.print(missing is None, missing?.count)
```

It prints:

```text
1 None (1, "one") [[1, 2], [3]] ["a", None] none
2 5 5
kit None Inner(label="kit", note=None)
(1, "one") (2, "two") [[1, 2], [3]] [[1, 9], [3], [7]] ["a", None] ["a", None, "b"]
3 100 False True
2 two [3] 9 b
2
1
1
none Holder(count=100, maybe=None, pair=(2, "two"), grid=[[1, 9], [3], [7]], words=["a", None, "b"], total=5)
dot Shape.Dot
anon:0 Shape.Named(name=None, parts=[])
n:1 Shape.Named(name="n", parts=[Shape.Dot])
1,2 Shape.Pair(at=(1, 2))
(5, "five") (1, "one") 2
1 [[1]] 77 [[1], [2]] (Holder(count=1, maybe=None, pair=(1, "one"), grid=[[1]], words=["a", None], total=0), 5)
False True False (1, "one")
True None
```

### `fizzbuzz`

```fennec
def fizzbuzz(n: int) -> str:
    if n % 15 == 0:
        "FizzBuzz"
    elif n % 3 == 0:
        "Fizz"
    elif n % 5 == 0:
        "Buzz"
    else:
        str(n)

def main(stdout: Stdout):
    for n in range(1, 16):
        stdout.print(fizzbuzz(n))
```

It prints:

```text
1
2
Fizz
4
Buzz
Fizz
7
8
Fizz
Buzz
11
Fizz
13
14
FizzBuzz
```

### `formatting`

```fennec
def main(stdout: Stdout):
    # Floats print as Python prints them: whole ones keep `.0`, and very large or very
    # small ones use an exponent.
    big = 1e308 * 10.0
    stdout.print(big, -big, big - big)
    stdout.print(1e15, 1e16, -1.5e17, 1e-7, 0.0001, 0.1 + 0.2, -0.0)

    # Collections show their items as the REPL would echo them.
    no_numbers: set[int] = set()
    stdout.print((1,), (1, "two"), (), no_numbers, {1}, [None, True, False])

    # Format specs: `,` groups thousands, `.N` sets the places, `f` and `d` name the kind.
    stdout.print(f"{1234567:,} {-1234567:,} {123:,} {1234.5:,.2f} {-0.5:,}")
    stdout.print(f"{7:.2f} {7:d} {2.5:f} {2.0} {[1, 2]}")

    # Brackets and colons inside `{}` belong to the expression, not the spec.
    stdout.print(f"{ {1: 2}[1] } {({3: 4, 5: 6}).len():d} {max([1, 5], key=n => -n)}")
```

It prints:

```text
inf -inf nan
1000000000000000.0 1e+16 -1.5e+17 1e-07 0.0001 0.30000000000000004 -0.0
(1,) (1, "two") () set() {1} [None, True, False]
1,234,567 -1,234,567 123 1,234.50 -0.5
7.00 7 2.5 2.0 [1, 2]
2 2 1
```

### `functions`

```fennec
def factorial(n: int) -> int:
    1 if n <= 1 else n * factorial(n - 1)

def fibonacci(n: int) -> int:
    if n < 2: return n
    fibonacci(n - 1) + fibonacci(n - 2)

def apply_damage(mut health: int, amount: int):
    health -= amount

def describe(name: str, health: int) -> str:
    status = "healthy" if health > 50 else "hurt"
    f"{name} is {status} ({health} HP)"

def main(stdout: Stdout):
    stdout.print(factorial(10))
    stdout.print([fibonacci(n) for n in range(10)])
    mut health = 100
    apply_damage(mut health, 30)
    stdout.print(describe("Ash", health))
    apply_damage(mut health, 45)
    stdout.print(describe(health=health, name="Ash"))
```

It prints:

```text
3628800
[0, 1, 1, 2, 3, 5, 8, 13, 21, 34]
Ash is healthy (70 HP)
Ash is hurt (25 HP)
```

### `functions_more`

```fennec
error ConfigError:
    message: str

error SettingsUnreadable:
    path: str

struct Score:
    value: int where value >= 0

def parse_score(text: str) -> Score:
    if not text.isdigit(): fail ConfigError(f"\"{text}\" isn't a number")
    Score(value=int(text)?)?

def load_scores(lines: list[str]) -> list[Score]:
    [parse_score(line)? for line in lines]

def read_settings(fs: Fs, path: str) -> str:
    text = fs.read_text(path)? from SettingsUnreadable(path)
    text.strip()

def counter_from(start: int) -> list[int]:
    def next_after(n: int) -> int:
        n + start
    [next_after(n) for n in range(3)]

def countdown(n: int) -> Iterator[int]:
    mut at = n
    while at > 0:
        yield at
        at -= 1

def main(stdout: Stdout, fs: Fs):
    stdout.print(load_scores(["3", "14", "15"]))
    stdout.print(load_scores(["3", "x", "15"]))
    stdout.print(Score(value=5))
    stdout.print(counter_from(10))
    stdout.print(list(countdown(3)), sum(countdown(4)))

    match read_settings(fs, "/no/such/fennec/settings.toml"):
        case Ok(text):
            stdout.print(text)
        case SettingsUnreadable(path):
            stdout.print(f"couldn't read {path}")

    by_length = sorted(["bramble", "kit", "ash"], key=(name) => (name.len(), name))
    stdout.print(by_length)
```

It prints:

```text
Ok([Score(value=3), Score(value=14), Score(value=15)])
Err(ConfigError(message="\"x\" isn't a number"))
Score(value=5)
[10, 11, 12]
[3, 2, 1] 10
couldn't read /no/such/fennec/settings.toml
["ash", "kit", "bramble"]
```

### `functions_passed`

```fennec
## Functions taking arrows and generic values, lowered for what they're given.

struct Circle:
    radius: float

    def area(self) -> float:
        3.0 * self.radius * self.radius

struct Square:
    side: float

    def area(self) -> float:
        self.side * self.side

interface Shape:
    def area(self) -> float

def total[T: Shape](shapes: list[T]) -> float:
    mut sum_of = 0.0
    for shape in shapes:
        sum_of += shape.area()
    sum_of

def largest[T](items: list[T], better: (T, T) -> bool) -> T:
    mut best = items[0]
    for item in items:
        if better(item, best):
            best = item
    best

def apply(value: int, change: (int) -> int) -> int:
    change(value)

def twice(value: int, change: (int) -> int) -> int:
    change(change(value))

def pass_on(value: int, change: (int) -> int) -> int:
    twice(value, change)

def keep(change: (int) -> int) -> (int) -> int:
    change

def map_all(values: list[int], change: (int) -> int) -> list[int]:
    [change(value) for value in values]

def first_big(values: list[int], limit: int, big: (int, int) -> bool) -> int:
    mut found = -1
    for value in values:
        if found < 0 and big(value, limit):
            found = value
    found

def identity[T](value: T) -> T:
    value

def shadow(value: int, change: (int) -> int) -> int:
    offset = 100
    change(value) + offset

def countdown(n: int, change: (int) -> int) -> int:
    if n <= 0: return 0
    change(n) + countdown(n - 1, change)

def main(stdout: Stdout):
    offset = 7
    stdout.print(
        apply(1, (n) => n + offset),
        twice(1, (n) => n * 2 + offset),
        pass_on(2, (n) => n + 1),
    )
    stdout.print(total([Circle(radius=1.0), Circle(radius=2.0)]), total([Square(side=3.0)]))
    circles = [Circle(radius=float(i % 4)) for i in range(9)]
    stdout.print(
        largest(circles, (a, b) => a.radius > b.radius),
        largest([3, 9, 2], (a, b) => a > b),
    )
    stdout.print(
        largest(["fox", "den"], (a, b) => a < b),
        identity(4),
        identity("s"),
        identity(2.5),
        identity(circles[1]),
    )
    stdout.print(map_all([1, 2, 3], (n) => n * n), first_big([1, 5, 9], 4, (v, l) => v > l))
    stdout.print(
        keep((n) => n + offset)(1),
        shadow(1, (n) => n + offset),
        countdown(3, (n) => n * offset),
    )
    stdout.print(twice(1, (n) => twice(n, (m) => m * 3 + offset)))
    words = ["a", "bb"]
    stdout.print(
        apply(words[1].len(), (n) => n + words.len()),
        [apply(i, (n) => n + i) for i in range(3)],
    )
    for i in range(2):
        stdout.print(apply(i, (n) => n * 10 + i))
```

It prints:

```text
8 25 4
15.0 9.0
Circle(radius=3.0) 9
den 4 s 2.5 Circle(radius=1.0)
[1, 4, 9] 5
8 108 42
361
4 [0, 2, 4]
0
11
```

### `hello`

```fennec
def main(stdout: Stdout):
    stdout.print("Hello, Fennec")
```

It prints:

```text
Hello, Fennec
```

### `list_building`

```fennec
## Lists made with comprehensions over ranges, then indexed from either end.

def main(stdout: Stdout):
    n = 5
    squares = [x * x for x in range(n)]
    mut halves = [float(x) / 2.0 for x in range(2, 7)]
    halves[0] = 9.5
    halves[-1] += 1.0
    odd = [x % 2 == 1 for x in range(4)]
    none = [0 for _ in range(-3)]
    zeros = [0.0 for _ in range(3)]
    backwards = [x for x in range(10, 3)]
    stdout.print(squares, halves, odd, none, zeros, backwards)
    stdout.print(squares[-1], squares[-5], halves[-2], squares.len())

    mut composite = [0 for _ in range(50)]
    mut primes = 0
    for k in range(2, 50):
        if composite[k] == 0:
            primes += 1
            mut multiple = k * k
            while multiple < 50:
                composite[multiple] = 1
                multiple += k
    stdout.print(primes)

    for x in range(-9, 10, 3):
        stdout.print(x, x // 2, x % 2, x // 8, x % 8)
```

It prints:

```text
[0, 1, 4, 9, 16] [9.5, 1.5, 2.0, 2.5, 4.0] [False, True, False, True] [] [0.0, 0.0, 0.0] []
16 0 2.5 5
15
-9 -5 1 -2 7
-6 -3 0 -1 2
-3 -2 1 -1 5
0 0 0 0 0
3 1 1 0 3
6 3 0 0 6
9 4 1 1 1
```

### `lists`

```fennec
def main(stdout: Stdout):
    scores = [12, 7, 31, 18]
    stdout.print([score * 2 for score in scores])
    stdout.print([score for score in scores if score > 10])
    stdout.print(
        f"total {sum(scores)}, best {max(scores)}, worst {min(scores)}, count {scores.len()}",
    )
    stdout.print(sum(score * score for score in scores))
    stdout.print(any(score > 30 for score in scores), all(score > 5 for score in scores))

    mut queue = []
    queue.append("fire")
    queue.append("ice")
    queue.insert(0, "earth")
    stdout.print(queue, queue.len())
    stdout.print(queue.pop(), queue)

    names = ["vex", "ash", "kit", "bramble"]
    stdout.print(sorted(names))
    stdout.print(sorted(names, key=name => name.len()))
    stdout.print(max(names, key=name => name.len()))
    stdout.print(names[0], names[-1], "kit" in names, "fox" not in names)

    mut grid = [0, 0, 0]
    grid[1] = 5
    grid[2] += 2
    stdout.print(grid)
```

It prints:

```text
[24, 14, 62, 36]
[12, 31, 18]
total 68, best 31, worst 7, count 4
1478
True True
["earth", "fire", "ice"] 3
ice ["earth", "fire"]
["ash", "bramble", "kit", "vex"]
["vex", "ash", "kit", "bramble"]
bramble
vex bramble True True
[0, 5, 2]
```

### `lists_of_strings`

```fennec
## Lists of strings and of boxed values, native, each owning what it holds.

enum Tree:
    Leaf
    Node(left: Tree, value: int, right: Tree)

def depth(tree: Tree) -> int:
    match tree:
        case .Leaf:
            0
        case .Node(left, _, right):
            1 + max(depth(left), depth(right))

def join(parts: list[str]) -> str:
    mut all = ""
    for part in parts:
        all = all + part
    return all

def named(n: int) -> list[str]:
    return [f"n{i}" for i in range(n)]

def main(stdout: Stdout):
    mut words = ["pear", "fig", "apple"]
    stdout.print(words, words.len(), words[0], words[-1], words[1].len())
    words.append("kiwi")
    words[0] = "plum"
    stdout.print(words, join(words))

    copy = words
    words[1] = "date"
    words.append("lime")
    stdout.print(copy, words, copy == words, copy == copy)

    stdout.print(sorted(words), sorted(words, key=(w) => w.len()), sorted(words, key=(w) => w[-1]))
    stdout.print(sorted(["b", "a", "ab", "", "aa", "abcdefghi", "abcdefgh", "abcdefgz", "é", "z"]))
    for word in sorted(words):
        stdout.print(word, word.len())
    stdout.print(sorted(words)[0], named(3)[2], named(2))
    stdout.print([w + "!" for w in words if w.len() > 3])
    stdout.print("kiwi" in words, words.len())
    stdout.print(["a", "b"] == ["a", "b"], ["a"] == ["b"])

    parts = "a,b,c".split(",")
    stdout.print(parts, parts[1], sorted(parts, key=(p) => p == "b"))

    trees = [Tree.Leaf, Tree.Node(left=Tree.Leaf, value=1, right=Tree.Leaf)]
    stdout.print(depth(trees[0]), depth(trees[1]), trees.len())
    mut grown = trees
    grown.append(Tree.Node(left=trees[1], value=2, right=trees[1]))
    grown[0] = grown[2]
    total = sum(depth(tree) for tree in grown)
    stdout.print(total, depth(trees[0]))
    for tree in sorted(grown, key=(t) => -depth(t)):
        stdout.print(depth(tree))

    mut many = [""]
    for i in range(1000):
        many.append(f"item {i}")
        many[0] = many[i]
    stdout.print(many.len(), many[0], many[1000])
    for word, count in zip(["x", "y"], [1, 2]):
        stdout.print(word, count)
    for at, word in enumerate(["p", "q"]):
        stdout.print(at, word)
```

It prints:

```text
["pear", "fig", "apple"] 3 pear apple 3
["plum", "fig", "apple", "kiwi"] plumfigapplekiwi
["plum", "fig", "apple", "kiwi"] ["plum", "date", "apple", "kiwi", "lime"] False True
["apple", "date", "kiwi", "lime", "plum"] ["plum", "date", "kiwi", "lime", "apple"] ["date", "apple", "lime", "kiwi", "plum"]
["", "a", "aa", "ab", "abcdefgh", "abcdefghi", "abcdefgz", "b", "z", "é"]
apple 5
date 4
kiwi 4
lime 4
plum 4
apple n2 ["n0", "n1"]
["plum!", "date!", "apple!", "kiwi!", "lime!"]
True 5
True False
["a", "b", "c"] b ["a", "c", "b"]
0 1 2
5 0
2
2
1
1001 item 998 item 999
x 1
y 2
0 p
1 q
```

### `loop_flags`

```fennec
## Tests that can move out of a loop, characters, and division with nothing negative.

def pick(flag: bool, n: int) -> int:
    mut total = 0
    for i in range(n):
        total += i if flag else -i
        if not flag:
            total += 1
        elif i == 2:
            break
    return total

def both(flag: bool, other: bool) -> str:
    mut out = ""
    for i in range(3):
        out += "y" if flag else "n"
        if other:
            out += "o"
    return out

def shadow(flag: bool) -> int:
    mut n = 0
    for i in range(3):
        n += 1 if flag else 2
        flag2 = i > 0
        if flag2:
            n += 10
    return n

def early(flag: bool) -> int:
    for i in range(5):
        if flag: return i
    return -1

def chars(text: str) -> int:
    mut n = 0
    for c in text:
        if c == "a":
            n += 1
        elif c == "é":
            n += 10
        elif c != "b":
            n += 100
    return n

def main(stdout: Stdout):
    stdout.print(
        pick(True, 5),
        pick(False, 5),
        both(True, False),
        both(False, True),
        shadow(True),
        early(True),
        early(False),
    )
    stdout.print(chars("abécé"), chars("aaa"), chars(""))
    mut kept = ""
    for c in "héllo wörld":
        kept += c
        d = c
        e = d + "!"
        stdout.print(d, e, d == "é", c == kept[-1])
    for i in range(0, 12, 5):
        stdout.print(i % 3, i // 3, i % 4, i // 4)
    a = "x" if len0() > 0 else "y"
    stdout.print(a, a == "y", a + "z")
    mut b = "p" if a == "y" else "q"
    b += "r"
    stdout.print(b)
    for n in range(-3, 3):
        stdout.print(n % 3, n // 3)

def len0() -> int:
    return "".len()
```

It prints:

```text
3 -5 yyy nonono 23 0 -1
121 3 0
h h! False True
é é! True True
l l! False True
l l! False True
o o! False True
   ! False True
w w! False True
ö ö! False True
r r! False True
l l! False True
d d! False True
0 0 0 0
2 1 1 1
1 3 2 2
y True yz
pr
0 -1
1 -1
2 -1
0 0
1 0
2 0
```

### `loops`

```fennec
def first_square_over(limit: int) -> int:
    mut n = 1
    while True:
        if n * n > limit: return n
        n += 1

def main(stdout: Stdout):
    stdout.print(first_square_over(50))
    mut odds = []
    for n in range(20):
        if n % 2 == 0: continue
        if n > 9: break
        odds.append(n)
    stdout.print(odds)
    for countdown in range(3, 0, -1):
        stdout.print(f"{countdown}...")
    stdout.print("liftoff")
```

It prints:

```text
8
[1, 3, 5, 7, 9]
3...
2...
1...
liftoff
```

### `matching`

```fennec
enum Shape:
    Circle(radius: float)
    Rect(width: float, height: float)
    Point

def name_int(n: int) -> str:
    match n:
        case 0:
            "zero"
        case -1:
            "minus one"
        case _:
            "something else"

def name_float(x: float) -> str:
    match x:
        case 0.5:
            "a half"
        case -0.5:
            "minus a half"
        case _:
            "something else"

def name_str(text: str) -> str:
    match text:
        case "fox":
            "a fox"
        case _:
            "not a fox"

def name_bool(b: bool) -> str:
    match b:
        case True:
            "yes"
        case False:
            "no"

def name_maybe(n: int?) -> str:
    match n:
        case None:
            "nothing"
        case _:
            "something"

def outcome(result: Result[int, str]) -> str:
    match result:
        case Ok(0):
            "nothing"
        case Ok(_):
            "something"
        case Err():
            "failed"

def width(shape: Shape) -> str:
    match shape:
        # A pattern can name fewer fields than the case has.
        case .Rect(width):
            f"{width} wide"
        case .Point:
            "a point"
        case _:
            "no width"

def main(stdout: Stdout):
    stdout.print(
        name_int(0),
        name_int(-1),
        name_int(1),
        name_float(0.5),
        name_float(-0.5),
        name_float(1.5),
    )
    stdout.print(name_maybe(None), name_maybe(0))
    stdout.print(name_str("fox"), name_str("cat"), name_bool(True), name_bool(False))
    stdout.print(outcome(Ok(0)), outcome(Ok(5)), outcome(Err("no")))
    stdout.print(width(.Rect(width=2.0, height=1.0)), width(.Point), width(.Circle(radius=1.0)))

    # A tuple pattern has as many parts as the tuple it matches.
    pair = (0, 4)
    match pair:
        case (0, y):
            stdout.print(f"on the y axis at {y}")
        case (x, y):
            stdout.print(f"at {x}, {y}")
    triple = (2, 3, 1)
    match triple:
        case (0, y, _):
            stdout.print(f"on the y axis at {y}")
        case _:
            stdout.print("three numbers")
    empty = ()
    match empty:
        case ():
            stdout.print("no numbers")

    # A plain name never matches an error, so a forgotten error can't pass as a value.
    broken: Result[int, str] = Err("broken")
    match broken:
        case value:
            stdout.print(f"got {value}")
        case _:
            stdout.print("an error")

    # A dotted case never looks inside an error.
    pointless: Result[Shape, Shape] = Err(Shape.Point)
    match pointless:
        case .Point:
            stdout.print("a point")
        case _:
            stdout.print("an error")
```

It prints:

```text
zero minus one something else a half minus a half something else
nothing something
a fox not a fox yes no
nothing something failed
2.0 wide a point no width
on the y axis at 4
three numbers
no numbers
an error
an error
```

### `method_chains`

```fennec
## Methods called on what other calls give back, with the type known from the checker.

struct V:
    x: int

    def add(self, o: V) -> V:
        V(x=self.x + o.x)

    def bump(mut self) -> int:
        self.x += 1
        self.x

enum E:
    A(n: int)
    B

    def n(self) -> int:
        match self:
            case .A(n):
                n
            case .B:
                0

struct Tree:
    left: Tree?
    v: int

    def me(self) -> Tree:
        self

    def val(self) -> int:
        self.v

def make() -> V:
    V(x=1)

def tree() -> Tree:
    Tree(left=None, v=7)

def main(stdout: Stdout):
    stdout.print(make().add(make()).add(V(x=5)).x, make().x, E.A(n=3).n(), E.B.n())
    stdout.print(tree().me().val(), tree().me().me().v)
    mut v = make()
    stdout.print(v.bump(), v.add(v).x, v.x, {1: 2}.get(1, 5), {"a": 1}.len())
```

It prints:

```text
7 1 3 0
7 7
2 4 2 2 1
```

### `numbers`

```fennec
# Ints, floats and bools: the core that compiles straight to machine code.

def fib(n: int) -> int:
    if n < 2: return n
    return fib(n - 1) + fib(n - 2)

def collatz_steps(start: int) -> int:
    mut n = start
    mut steps = 0
    while n != 1:
        n = n // 2 if n % 2 == 0 else 3 * n + 1
        steps += 1
    steps

def is_prime(n: int) -> bool:
    if n < 2: return False
    mut d = 2
    while d * d <= n:
        if n % d == 0: return False
        d += 1
    True

def lerp(a: float, b: float, t: float) -> float:
    a + (b - a) * t

def main(stdout: Stdout):
    stdout.print(fib(20), collatz_steps(27))
    mut primes = 0
    for n in range(100):
        if is_prime(n):
            primes += 1
    stdout.print(f"{primes} primes below 100")

    # Division rounds toward negative infinity, and % follows it.
    stdout.print(7 // 2, -7 // 2, 7 % 3, -7 % 3, 7 % -3)
    stdout.print(7 / 2, -7.5 // 2.0, 7.5 % 2.0, 2.0 * 3.0)
    stdout.print((-4).abs(), (-2.5).abs(), min(3, 9), max(3.5, 1.0), float(3), int(2.9))
    stdout.print(lerp(0.0, 10.0, 0.25), 0.1 + 0.2, 1.0 / 3.0)

    mut evens = 0
    for i in range(10, 0, -2):
        evens += i
    stdout.print(evens, not True, 3 < 4 and 4 < 3, 3 < 4 or 4 < 3)

    mut total = 0
    mut i = 0
    while True:
        i += 1
        if i > 20: break
        if i % 3 != 0: continue
        total += i
    stdout.print(f"multiples of 3 up to 20 add up to {total}")

    # Right up to the limits of a 64-bit int, without going over.
    biggest = 9223372036854775806 + 1
    smallest = -biggest - 1
    stdout.print(biggest, smallest, biggest - 1, smallest + 1, smallest - -5)
```

It prints:

```text
6765 111
25 primes below 100
3 -4 1 2 -2
3.5 -4.0 1.5 6.0
4 2.5 3 3.5 3.0 2
2.5 0.30000000000000004 0.3333333333333333
30 False False True
multiples of 3 up to 20 add up to 63
9223372036854775807 -9223372036854775808 9223372036854775806 -9223372036854775807 -9223372036854775803
```

### `object_receivers`

```fennec
## Lists, dicts and strings held as objects, read and changed where they are.

struct Shelf:
    items: list[int]
    counts: dict[str, int]
    name: str

def first(shelf: Shelf) -> int:
    return shelf.items[0]

def main(stdout: Stdout):
    mut shelf = Shelf(items=[3, 1, 2], counts={"a": 1}, name="oak")
    stdout.print(shelf.items[1], shelf.items.len(), shelf.items[-1])
    stdout.print(shelf.counts["a"], shelf.counts.len(), shelf.counts.get("b", 7))
    stdout.print(shelf.name.len(), shelf.name[0], shelf.name.upper())

    shelf.items.append(9)
    shelf.items[0] = 4
    shelf.counts["b"] = 2
    stdout.print(shelf.items, shelf.counts, first(shelf))

    kept = shelf.items
    shelf.items.append(5)
    stdout.print(kept, shelf.items)

    grid = [[1, 2], [3, 4]]
    stdout.print(grid[1][0], grid[0].len())
    for row in grid:
        stdout.print(row[1], row.len())

    dicts = [{"x": 1}, {"y": 2}]
    stdout.print(dicts[1].get("y", 0), dicts[0].get("y", 8), dicts[0]["x"], dicts[1].len())
    for key in dicts[0]:
        stdout.print(key)
    words = ["ab", "cde"]
    stdout.print(words[1].len(), words[1][2])
```

It prints:

```text
1 3 2
1 1 7
3 o OAK
[4, 1, 2, 9] {"a": 1, "b": 2} 4
[4, 1, 2, 9] [4, 1, 2, 9, 5]
3 2
2 2
4 2
2 8 1 1
x
3 e
```

### `operators`

```fennec
struct Money:
    cents: int

    def +(self, other: Money) -> Money:
        Money(cents=self.cents + other.cents)

    def <(self, other: Money) -> bool:
        self.cents < other.cents

struct Grid:
    width: int
    cells: list[int]

    def [](self, at: (int, int)) -> int:
        x, y = at
        self.cells[y * self.width + x]

    def []=(mut self, at: (int, int), value: int):
        x, y = at
        self.cells[y * self.width + x] = value

    def iter(self) -> Iterator[int]:
        for cell in self.cells:
            yield cell

struct File:
    name: str

    def del(self):
        pass

def main(stdout: Stdout):
    total = Money(cents=150) + Money(cents=275)
    stdout.print(total, total > Money(cents=400), max([Money(cents=5), Money(cents=9)]))

    mut grid = Grid(width=2, cells=[0, 0, 0, 0])
    grid[(1, 1)] = 7
    stdout.print(grid[(1, 1)], sum(cell for cell in grid))

    with File(name="save.dat") as file:
        stdout.print(f"writing {file.name}")
```

It prints:

```text
Money(cents=425) True Money(cents=9)
7 7
writing save.dat
```

### `own_index_places`

```fennec
# A type's own `def []` and `def []=` act as places: changing part of an item reads it,
# changes it, and writes it back, however deep it's nested.

from std.game import IdMap

struct Part:
    name: str
    x = 0.0

    def shift(mut self, by: float):
        self.x += by

struct Shelf:
    parts: list[Part]
    writes = 0

    def [](self, at: int) -> Part:
        self.parts[at]

    def []=(mut self, at: int, part: Part):
        self.parts[at] = part
        self.writes += 1

struct Numbers:
    values: list[int]

    def [](self, at: int) -> int:
        self.values[at]

    def []=(mut self, at: int, value: int):
        self.values[at] = value

struct Room:
    shelf: Shelf

    def [](self, name: str) -> Shelf:
        self.shelf

    def []=(mut self, name: str, shelf: Shelf):
        self.shelf = shelf

struct Store:
    rooms: list[Room]

def main(stdout: Stdout):
    mut shelf = Shelf(parts=[Part(name="bolt")])
    shelf[0].x = 1.5
    shelf[0].x += 1.0
    shelf[0].name += "!"
    shelf[0].shift(0.5)
    stdout.print(shelf.parts, shelf.writes)

    # Compound assignment reads with `def []`, then writes with `def []=`.
    mut numbers = Numbers(values=[1, 2])
    numbers[0] += 5
    numbers[1] *= 3
    stdout.print(numbers.values)

    # One inside another, and inside other places.
    mut room = Room(shelf=shelf)
    room["back"][0].x = 9.0
    stdout.print(room.shelf.parts, room.shelf.writes, shelf.parts)

    mut store = Store(rooms=[room])
    store.rooms[0]["back"][0].shift(1.0)
    stdout.print(store.rooms[0].shelf.parts, room.shelf.parts)

    # Reached through a dict or an IdMap.
    mut by_name = {"main": shelf}
    by_name["main"][0].x = 20.0
    stdout.print(by_name["main"].parts, shelf.parts)

    mut shelves = IdMap[Shelf]()
    id = shelves.insert(shelf)
    shelves[id][0].x = 30.0
    stdout.print(shelves[id].parts, shelf.parts)
```

It prints:

```text
[Part(name="bolt!", x=3.0)] 4
[6, 6]
[Part(name="bolt!", x=9.0)] 5 [Part(name="bolt!", x=3.0)]
[Part(name="bolt!", x=10.0)] [Part(name="bolt!", x=9.0)]
[Part(name="bolt!", x=20.0)] [Part(name="bolt!", x=3.0)]
[Part(name="bolt!", x=30.0)] [Part(name="bolt!", x=3.0)]
```

### `packed_lists`

```fennec
# Lists of numbers and plain structs are stored packed, and changed in place, but they're
# still values: a copy is independent of what it was copied from.

struct Point:
    x = 0.0
    y = 0.0

    def nudge(mut self, dx: float):
        self.x += dx

def total(values: list[float]) -> float:
    mut sum = 0.0
    for value in values:
        sum += value
    return sum

def main(stdout: Stdout):
    mut scores = [3, 1, 4]
    scores.append(1)
    scores.append(5)
    copy = scores
    scores[0] = 9
    scores[-1] += 10
    stdout.print(scores, copy, scores.len(), scores[-2])

    mut points = [Point(x=float(i), y=float(i) * 2.0) for i in range(3)]
    for mut p in points:
        p.nudge(0.5)
        p.y = -p.y
    points[1].x = 100.0
    points.append(Point(y=7.0))
    stdout.print(points[0], points[1].x, points[-1], points.len())

    mut weights = [0.5, 1.5, 2.0]
    stdout.print(total(weights))
    # This loop names its own list, so it goes through the runtime's way.
    for w in weights:
        if w > 1.0:
            weights.append(w * 10.0)
    stdout.print(weights)

    mut flags = [True, False]
    flags[1] = not flags[0]
    stdout.print(flags, [p.x for p in points])
```

It prints:

```text
[9, 1, 4, 1, 15] [3, 1, 4, 1, 5] 5 1
Point(x=0.5, y=-0.0) 100.0 Point(x=0.0, y=7.0) 4
4.0
[0.5, 1.5, 2.0, 15.0, 20.0]
[True, False] [0.5, 100.0, 2.5, 0.0]
```

### `particles`

```fennec
# Structs whose methods change them, `for mut` loops that change every item, and an
# operator defined on a struct.

struct Particle:
    x = 0.0
    vx = 1.0
    hits = 0

    def step(mut self, dt: float):
        self.x += self.vx * dt
        if self.x > 10.0:
            self.vx = -self.vx
            self.hits += 1

    def +(self, other: Particle) -> Particle:
        Particle(x=self.x + other.x, vx=self.vx + other.vx, hits=self.hits + other.hits)

struct Swarm:
    particles: list[Particle]

    def run(mut self, frames: int):
        for frame in range(frames):
            for mut p in self.particles:
                p.step(0.5)

    # Stops at the first particle that has bounced, after nudging it.
    def nudge_first_bounced(mut self) -> bool:
        for mut p in self.particles:
            if p.hits > 0:
                p.x = 0.0
                return True
        False

def main(stdout: Stdout):
    mut swarm = Swarm(particles=[Particle(vx=float(i)) for i in range(4)])
    swarm.run(12)
    for p in swarm.particles:
        stdout.print(f"{p.x:.1f} {p.vx} {p.hits}")
    stdout.print(swarm.nudge_first_bounced(), swarm.particles[2].x)

    total = swarm.particles[0] + swarm.particles[3]
    stdout.print(total)

    mut counts = [1, 2, 3, 4]
    for mut n in counts:
        if n == 3: break
        n *= 10
    stdout.print(counts)
```

It prints:

```text
0.0 0.0 0
6.0 1.0 0
10.0 -2.0 1
3.0 -3.0 1
True 0.0
Particle(x=3.0, vx=-3.0, hits=1)
[10, 20, 3, 4]
```

### `robot`

```fennec
# A robot that follows moves: enums and structs held in registers, taken apart by `match`.
# It all compiles to plain machine code.

struct Cell:
    row: int
    col: int

enum Dir:
    North
    East
    South
    West

enum Move:
    Step(dir: Dir, count: int)
    Jump(to: Cell)
    Turn
    Wait(turns: int)

struct Robot:
    at: Cell
    facing: Dir
    waited: int

def turn(dir: Dir) -> Dir:
    match dir:
        case .North:
            .East
        case .East:
            .South
        case .South:
            .West
        case .West:
            .North

def step(at: Cell, dir: Dir, count: int) -> Cell:
    match dir:
        case .North:
            Cell(row=at.row - count, col=at.col)
        case .South:
            Cell(row=at.row + count, col=at.col)
        case .East:
            Cell(row=at.row, col=at.col + count)
        case .West:
            Cell(row=at.row, col=at.col - count)

def next_move(i: int, facing: Dir) -> Move:
    match i % 7:
        case 0:
            .Turn
        case 1:
            .Step(facing, i % 5)
        case 2:
            .Jump(Cell(row=i % 13, col=i % 11))
        case 3:
            .Wait(i % 3)
        case _:
            .Step(turn(facing), 1)

def apply(robot: Robot, move: Move) -> Robot:
    match move:
        case .Step(dir, 0):
            Robot(at=robot.at, facing=dir, waited=robot.waited + 1)
        case .Step(dir, count):
            Robot(at=step(robot.at, dir, count), facing=dir, waited=robot.waited)
        case .Jump(to):
            Robot(at=to, facing=robot.facing, waited=robot.waited)
        case .Turn:
            Robot(at=robot.at, facing=turn(robot.facing), waited=robot.waited)
        case .Wait(turns):
            Robot(at=robot.at, facing=robot.facing, waited=robot.waited + turns)

def main(stdout: Stdout):
    mut robot = Robot(at=Cell(row=0, col=0), facing=.North, waited=0)
    mut checksum = 0
    for i in range(1000):
        move = next_move(i, robot.facing)
        robot = apply(robot, move)
        checksum += robot.at.row * 3 + robot.at.col
    stdout.print(robot.at.row, robot.at.col, robot.waited, checksum)
```

It prints:

```text
9 5 171 23506
```

### `sorted_native`

```fennec
## `sorted` on lists of numbers and by keys, which native code does itself.

struct Point:
    x: int
    y: float

def main(stdout: Stdout):
    ints = [5, -3, 9, 0, -3, 7]
    stdout.print(sorted(ints), ints)
    stdout.print(sorted([1.5, -0.0, 0.0, -2.5, 8.0]))
    stdout.print(sorted([True, False, True]))
    stdout.print(sorted(ints, key=(n) => -n))
    stdout.print(sorted(ints, key=(n) => n % 3))
    stdout.print(sorted(ints, key=(n) => n > 0))
    stdout.print(sorted(ints, key=(n) => float(n) / 2.0))

    shift = 4
    stdout.print(sorted(ints, key=(n) => (n + shift) % 5))

    points = [Point(x=2, y=1.5), Point(x=1, y=1.5), Point(x=3, y=-1.0)]
    for point in sorted(points, key=(p) => p.y):
        stdout.print(point.x, point.y)
    stdout.print(sorted(points, key=(p) => p.x)[0].x)

    same_name = [3, 1, 2]
    stdout.print(sorted(same_name, key=(same_name) => -same_name))

    twice = sorted(sorted(ints), key=(n) => n * n)
    stdout.print(twice, sorted(twice).len())

    mut grown = sorted([3, 1, 2])
    grown.append(0)
    stdout.print(grown, sorted(grown))

    keep = [2, 1]
    copy = sorted(keep)
    stdout.print(keep, copy)
    stdout.print(sorted([n * 2 for n in range(4)], key=(n) => -n))
    stdout.print(sorted(range(3), key=(n) => -n))
    stdout.print(sorted("cab"))
```

It prints:

```text
[-3, -3, 0, 5, 7, 9] [5, -3, 9, 0, -3, 7]
[-2.5, -0.0, 0.0, 1.5, 8.0]
[False, True, True]
[9, 7, 5, 0, -3, -3]
[-3, 9, 0, -3, 7, 5]
[-3, 0, -3, 5, 9, 7]
[-3, -3, 0, 5, 7, 9]
[-3, -3, 7, 9, 5, 0]
3 -1.0
2 1.5
1 1.5
1
[3, 2, 1]
[0, -3, -3, 5, 7, 9] 6
[1, 2, 3, 0] [0, 1, 2, 3]
[2, 1] [1, 2]
[6, 4, 2, 0]
[2, 1, 0]
["a", "b", "c"]
```

### `std_hash`

```fennec
from std import hash
from std.hash import blake3, derive_key, keyed_blake3

## A save file with a tag that shows it wasn't edited by hand.
struct Save:
    level: int
    tag: str

def sign(key: str, level: int) -> Save:
    Save(level=level, tag=keyed_blake3(key, f"level {level}"))

def untouched(key: str, save: Save) -> bool:
    keyed_blake3(key, f"level {save.level}") == save.tag

test "the same text always has the same hash" for text: str:
    assert blake3(text) == hash.blake3(text)
    assert blake3(text).len() == 64

def main(stdout: Stdout):
    # The same text gives the same hash on every machine, so a hash names its content.
    stdout.print(blake3(""))
    stdout.print(blake3("fennec"))
    stdout.print(blake3("fennec") == blake3("fennec"), blake3("fennec") == blake3("Fennec"))

    # A key derived from a secret, for one purpose only.
    key = derive_key("fennec example 2026-10-30 save files", "the player's secret")
    save = sign(key, 3)
    stdout.print(untouched(key, save))
    stdout.print(untouched(key, Save(level=99, tag=save.tag)))
```

It prints:

```text
af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262
9648138d424426d20bcaf645c2782f9f4881ebe809b72f82490521d841c55b16
True False
True
False
```

### `std_re`

```fennec
from std.re import Match, Regex

struct Command:
    verb: str
    target: str
    count: int

## Reads a typed command like "take 3 arrows" or "open door".
def parse_command(line: str) -> Command?:
    found = Regex(r"^\s*(\w+)\s+(?:(\d+)\s+)?(\w+)\s*$").match(line)
    if found is None: return None
    Command(verb=found[1] else "", target=found[3] else "", count=int(found[2] else "1") else 1)

test "names with letters only are found whole" for word: str:
    names = Regex(r"[a-z]+").find_all(word)
    assert all(name.text.len() > 0 for name in names)

def main(stdout: Stdout):
    stdout.print(parse_command("take 3 arrows"))
    stdout.print(parse_command("  open door "))
    stdout.print(parse_command("dance"))

    log = "hp=30 mp=7 xp=1250 gold=99"
    stat = Regex(r"(\w+)=(\d+)")
    for found in stat.find_all(log):
        stdout.print(found[1], int(found[2] else "0") else 0, found.start, found.end)

    stdout.print(stat.replace(log, r"\1: \2"))
    stdout.print(Regex(r"\s*[,;]\s*").split("sword, shield;bow ,  arrows"))
    stdout.print(Regex(r"colou?r").search("What colour is it?"))
    stdout.print([m.start for m in Regex(r"\bcat\b").find_all("cat concat cat.")])

    # Patterns written in the program are checked before it runs. One built while it runs
    # might not be valid, so making it gives a result.
    for pattern in ["[0-9]+", "[0-9+"]:
        match Regex(pattern):
            case Ok(regex):
                stdout.print(pattern, "finds", [m.text for m in regex.find_all("a1b22")])
            case Err(error):
                stdout.print(pattern, "->", error.message)

    # Matching never backtracks, so this takes time in proportion to the text.
    many = "".join(["a" for _ in range(5_000)])
    stdout.print(Regex(r"(a*)*b").search(many))
```

It prints:

```text
Command(verb="take", target="arrows", count=3)
Command(verb="open", target="door", count=1)
None
hp 30 0 5
mp 7 6 10
xp 1250 11 18
gold 99 19 26
hp: 30 mp: 7 xp: 1250 gold: 99
["sword", "shield", "bow", "arrows"]
Match(text="colour", start=5, end=11, groups=[])
[0, 11]
[0-9]+ finds ["1", "22"]
[0-9+ -> this pattern isn't valid: this `[` is never closed (at character 0)
None
```

### `std_tree`

```fennec
# `Tree` keeps parents and children in sync, on top of `IdMap`, with no references. A scene:
# what's attached to what, read and changed through IDs.
from std.collections import Tree, TreeCycle, TreeNode
from std.game import Id

struct Part:
    name: str
    x = 0.0

    def shift(mut self, by: float):
        self.x += by

# Writing the type of an ID is only needed where one is stored.
type PartId = Id[TreeNode[Part]]

struct Camera:
    follows: PartId

# Where a part really is: its own position plus everything it's attached to.
def world_x(scene: Tree[Part], id: PartId) -> float:
    mut x = scene[id].x
    for above in scene.ancestors(id):
        x += scene[above].x
    x

def names(scene: Tree[Part], ids: list[PartId]) -> list[str]:
    [scene[id].name for id in ids]

def main(stdout: Stdout):
    mut scene = Tree[Part]()
    car = scene.add(Part(name="car", x=10.0))
    trailer = scene.add(Part(name="trailer", x=-5.0))
    wheel = scene.add_child(car, Part(name="wheel", x=2.0))
    hub = scene.add_child(wheel, Part(name="hub", x=0.5))
    door = scene.add_child(car, Part(name="door", x=1.0))
    camera = Camera(follows=hub)
    stdout.print(scene.len(), names(scene, scene.roots()))

    # Values are changed through an ID.
    scene[car].x += 5.0
    scene[door].shift(0.5)
    scene[door].name += " (left)"
    stdout.print(scene[car], scene[door], world_x(scene, camera.follows))

    # Walking up and down.
    stdout.print(
        scene.parent(wheel) == car,
        scene.parent(car),
        scene.up(hub, 2) == car,
        scene.up(hub, 3),
    )
    stdout.print(names(scene, scene.children(car)), names(scene, scene.ancestors(hub)))
    stdout.print(names(scene, scene.descendants(car)))

    # A copy is independent, so undo is a saved value.
    saved = scene

    # Moving a subtree takes what's below it along.
    scene.move(wheel, trailer)?
    stdout.print(names(scene, scene.children(car)), names(scene, scene.descendants(trailer)))
    stdout.print(world_x(scene, hub), world_x(saved, hub))

    # A node can't end up below itself.
    match scene.move(trailer, hub):
        case Ok():
            stdout.print("moved")
        case Err(TreeCycle(message)):
            stdout.print(message)
    match scene.move(hub, hub):
        case Ok():
            stdout.print("moved")
        case Err(TreeCycle(message)):
            stdout.print(message)

    scene.make_root(wheel)
    stdout.print(names(scene, scene.roots()), scene.parent(wheel))

    # Removing a node removes what's below it, and old IDs say so.
    scene.remove(wheel)
    stdout.print(
        scene.len(),
        names(scene, scene.roots()),
        scene.get(hub),
        scene.get(car) == scene[car],
    )
    scene.remove(trailer)
    stdout.print(names(scene, scene.roots()), saved.len(), saved[hub].name)
    scene.remove(door)
    stdout.print(scene.children(car))
```

It prints:

```text
5 ["car", "trailer"]
Part(name="car", x=15.0) Part(name="door (left)", x=1.5) 17.5
True None True None
["wheel", "door (left)"] ["wheel", "car"]
["wheel", "hub", "door (left)"]
["door (left)"] ["wheel", "hub"]
-2.5 17.5
a node can't move under itself or its descendants
a node can't move under itself or its descendants
["car", "trailer", "wheel"] None
3 ["car", "trailer"] None True
["car"] 5 hub
[]
```

### `string_building`

```fennec
## Building strings a piece at a time, and walking their characters.

def shout(word: str) -> str:
    mut loud = word
    loud += "!"
    return loud

def find(text: str, want: str) -> int:
    mut at = 0
    for c in text:
        if c == want: return at
        at += 1
    return -1

def main(stdout: Stdout):
    mut text = "ab"
    text += text
    text += text
    stdout.print(text, text.len())

    # A copy keeps its value when the original grows.
    kept = text
    text += "é"
    stdout.print(kept, text, text.len())
    stdout.print(shout("hi"), shout(kept))
    mut long = "<"
    for i in range(3):
        long += kept + kept + kept + kept + kept
    stdout.print(long, long.len())

    mut big = ""
    for i in range(100_000):
        big += "x" if i % 2 == 0 else "y"
    stdout.print(big.len(), big[4:9], big[-1])

    # The loop goes over the text as it was when the loop started.
    mut fox = "wörld 🦊!"
    mut count = 0
    for c in fox:
        fox += c
        count += 1
    stdout.print(fox, count, fox.len())

    mut dotted = ""
    for c in "a🦊bc":
        if c == "b": continue
        if c == "c": break
        dotted += c + "."
    stdout.print(dotted)
    for c in "":
        stdout.print("never")
    stdout.print(find("fennec", "c"), find("ü🦊x", "x"), find("ab", "z"))
```

It prints:

```text
abababab 8
abababab ababababé 9
hi! abababab!
<abababababababababababababababababababababababababababababababababababababababababababababababababababababababababababab 121
100000 xyxyx y
wörld 🦊!wörld 🦊! 8 16
a.🦊.
5 2 -1
```

### `strings`

```fennec
def main(stdout: Stdout):
    name = "Fennec"
    coins = 1234567
    health = 87.456
    stdout.print(f"{name} has {coins:,} coins and {health:.1f} HP")
    stdout.print(f"{{literal braces}} and {name.len() * 2}")

    words = "fire ice  storm".split()
    stdout.print(words)
    stdout.print(", ".join(words).upper())
    stdout.print("ice" in words, "ICE".lower() == "ice")
    stdout.print(name[0], name[-1], name.len())
    stdout.print("  padded  ".strip() + "!")
    stdout.print("a-b-c".split("-"), "file.toml".endswith(".toml"))

    mut initials = ""
    for word in words:
        initials += word[0]
    stdout.print(initials)
    stdout.print(str(3.0), str(2.5), 7 / 2, 7 // 2, -7 // 2, -7 % 3, 2 ** 10)

    # `*` repeats text. It's `str`'s own `def *`, written in Fennec in the standard library.
    for depth in range(3):
        stdout.print("  " * depth + "- level", depth)
    stdout.print("=" * 12, "ab" * 0 == "")

    # A character's code, and back. Text is UTF-8, so its bytes can outnumber its characters.
    stdout.print(
        "A".code(),
        "é".code(),
        "Fox".codes(),
        str.from_code(97),
        str.from_code(-1) else "?",
    )
    stdout.print("héllo".len(), "héllo".encode().len(), "héllo"[1])
    stdout.print("\x41\u00e9\U0001f98a", "tab\there")
```

It prints:

```text
Fennec has 1,234,567 coins and 87.5 HP
{literal braces} and 12
["fire", "ice", "storm"]
FIRE, ICE, STORM
True True
F c 6
padded!
["a", "b", "c"] True
fis
3.0 2.5 3.5 3 -4 2 1024
- level 0
  - level 1
    - level 2
============ True
65 233 [70, 111, 120] Ok("a") ?
5 6 é
Aé🦊 tab	here
```

### `struct_keys_native`

```fennec
## Dicts and sets keyed by plain structs and enums, native.

struct Cell:
    x: int
    y: int

struct Tagged:
    at: Cell
    weight: float
    live: bool

enum Dir:
    North
    East
    Far(steps: int)

struct Grid:
    seen: dict[Cell, int]
    marks: set[Cell]

def visit(mut grid: Grid, cell: Cell):
    grid.seen[cell] = grid.seen.get(cell, 0) + 1
    grid.marks.add(cell)

def main(stdout: Stdout):
    mut seen: dict[Cell, int] = {}
    for i in range(1000):
        cell = Cell(x=i % 7, y=i % 5)
        seen[cell] = seen.get(cell, 0) + 1
    stdout.print(
        seen.len(),
        seen[Cell(x=0, y=0)],
        Cell(x=1, y=1) in seen,
        Cell(x=9, y=9) in seen,
        Cell(x=9, y=9) not in seen,
    )
    stdout.print(seen.get(Cell(x=9, y=9), -1), seen.get(Cell(x=2, y=3), -1))

    literal = {Cell(x=1, y=2): "a", Cell(x=3, y=4): "b", Cell(x=1, y=2): "c"}
    stdout.print(literal, literal[Cell(x=1, y=2)], literal.len())
    mut total = 0
    for key in literal:
        total += key.x + key.y
    for key, word in literal.items():
        stdout.print(key.x, key, word)
    for key in literal.keys():
        total += key.y
    stdout.print(total, [word for word in literal.values()])

    copy = seen
    seen[Cell(x=100, y=100)] = 1
    seen[Cell(x=0, y=0)] = 0
    stdout.print(
        copy.len(),
        seen.len(),
        copy[Cell(x=0, y=0)],
        seen[Cell(x=0, y=0)],
        copy == seen,
        copy == copy,
    )

    tagged = {Tagged(at=Cell(x=1, y=1), weight=0.5, live=True): 1}
    stdout.print(
        tagged[Tagged(at=Cell(x=1, y=1), weight=0.5, live=True)],
        Tagged(at=Cell(x=1, y=1), weight=0.5, live=False) in tagged,
        tagged,
    )
    stdout.print(Tagged(at=Cell(x=1, y=1), weight=0.25, live=True) in tagged)

    mut dirs: dict[Dir, str] = {}
    dirs[Dir.North] = "n"
    dirs[Dir.Far(steps=2)] = "far2"
    dirs[Dir.Far(steps=3)] = "far3"
    stdout.print(
        dirs[Dir.North],
        dirs[Dir.Far(steps=3)],
        Dir.East in dirs,
        Dir.Far(steps=4) in dirs,
        dirs.len(),
    )

    mut marks = {Cell(x=1, y=1), Cell(x=2, y=2), Cell(x=1, y=1)}
    marks.add(Cell(x=3, y=3))
    marks.discard(Cell(x=2, y=2))
    marks.discard(Cell(x=9, y=9))
    marks.remove(Cell(x=3, y=3))
    stdout.print(marks, marks.len(), Cell(x=1, y=1) in marks, Cell(x=3, y=3) in marks)
    made = {Cell(x=i % 3, y=0) for i in range(10)}
    stdout.print(made.len(), sorted([cell.x for cell in made]))

    mut grid = Grid(seen={}, marks={Cell(x=0, y=0)})
    visit(mut grid, Cell(x=1, y=0))
    visit(mut grid, Cell(x=1, y=0))
    grid.seen[Cell(x=5, y=5)] = 50
    grid.marks.remove(Cell(x=0, y=0))
    stdout.print(grid, grid.seen.get(Cell(x=1, y=0), 0))

    mut large: dict[Cell, int] = {}
    for i in range(5000):
        large[Cell(x=i, y=i * 2)] = i
    stdout.print(large.len(), large[Cell(x=4999, y=9998)], Cell(x=1, y=1) in large)
```

It prints:

```text
35 29 True False True
-1 28
{Cell(x=1, y=2): "c", Cell(x=3, y=4): "b"} c 2
1 Cell(x=1, y=2) c
3 Cell(x=3, y=4) b
16 ["c", "b"]
35 36 29 0 False True
1 False {Tagged(at=Cell(x=1, y=1), weight=0.5, live=True): 1}
False
n far3 False False 3
{Cell(x=1, y=1)} 1 True False
3 [0, 1, 2]
Grid(seen={Cell(x=1, y=0): 2, Cell(x=5, y=5): 50}, marks={Cell(x=1, y=0)}) 2
5000 4999 False
```

### `structs`

```fennec
from std.math import Vec3

struct Enemy:
    name: str
    position = Vec3.zero
    health = 100

    def at(name: str, position: Vec3) -> Enemy:
        Enemy(name=name, position=position)

    def is_alive(self) -> bool:
        self.health > 0

    def take_damage(mut self, amount: int):
        self.health -= amount

def main(stdout: Stdout):
    mut ash = Enemy.at("Ash", Vec3(x=3.0, y=4.0))
    stdout.print(ash)
    stdout.print(ash.position.length())

    mut copy = ash
    copy.take_damage(30)
    stdout.print(ash.health, copy.health)

    ash.take_damage(120)
    stdout.print(ash.is_alive(), ash.health)

    ash.position.x = 10.0
    stdout.print(ash.position)

    healed = replace(ash, health=50)
    stdout.print(healed.health, ash.health)

    velocity = Vec3(x=1.0, y=2.0, z=0.0)
    stdout.print(ash.position + velocity * 2.0)
    stdout.print(ash == copy, copy == replace(copy))
```

It prints:

```text
Enemy(name="Ash", position=Vec3(x=3.0, y=4.0, z=0.0), health=100)
5.0
100 70
False -20
Vec3(x=10.0, y=4.0, z=0.0)
50 -20
Vec3(x=12.0, y=8.0, z=0.0)
False True
```

### `sum`

```fennec
# `sum` adds with `+`. Numbers need nothing, and an empty list of floats gives `0.0`. Any
# other type passes a start, since only the program knows what its zero is.

interface Addable:
    def +(self, other: Self) -> Self

struct Money:
    cents: int

    def +(self, other: Money) -> Money:
        Money(cents=self.cents + other.cents)

struct Item:
    price: Money

def total[T: Addable](items: list[T], start: T) -> T:
    sum(items, start)

def main(stdout: Stdout):
    no_floats: list[float] = []
    no_ints: list[int] = []
    stdout.print(sum([1, 2, 3]), sum([1.5, 2.5]), sum(no_ints), sum(no_floats))
    stdout.print(sum([1, 2], 10), sum(no_floats, 0.5))

    # The items' type can be worked out after the call.
    mut later = []
    stdout.print(sum(later))
    later.append(1.5)

    wallet = [Money(cents=150), Money(cents=250)]
    stdout.print(sum(wallet, Money(cents=0)))
    stdout.print(sum(wallet, Money(cents=1000)))

    cart = [Item(price=Money(cents=5)), Item(price=Money(cents=7))]
    stdout.print(sum(item.price for item in cart, Money(cents=0)))

    stdout.print(sum(["a", "b"], ""), sum([[1], [2, 3]], []))
    stdout.print(total(wallet, Money(cents=0)), total([1.5], 0.0))
```

It prints:

```text
6 4.0 0 0.0
13 0.5
0.0
Money(cents=400)
Money(cents=1400)
Money(cents=12)
ab [1, 2, 3]
Money(cents=400) 1.5
```

### `testing`

```fennec
def reverse(items: list[int]) -> list[int]:
    [items[items.len() - 1 - at] for at in range(items.len())]

def clamp(value: int, low: int, high: int) -> int:
    max(low, min(value, high))

test "reversing twice gives the original" for items: list[int]:
    assert reverse(reverse(items)) == items

test "clamp stays in range" for value: int:
    assert 0 <= clamp(value, 0, 10) <= 10

test "reversing a known list":
    assert reverse([1, 2, 3]) == [3, 2, 1]

def main(stdout: Stdout):
    stdout.print(reverse([1, 2, 3]), clamp(42, 0, 10))
```

It prints:

```text
[3, 2, 1] 10
```

### `text_native`

```fennec
## Text native code makes itself: f-strings, `str`, `join`, `code` and slices of lists.

struct Point:
    x: int
    y: float

def label(n: int) -> str:
    f"n={n}"

def main(stdout: Stdout):
    name = "fox"
    stdout.print(f"plain", f"{name}", f"{name}!", f"<{name}>", f"")
    stdout.print(f"{1} {2.5} {True} {False} {-7} {name} {1 + 2}")
    stdout.print(f"{3.14159:.2f} {1234567:,} {name}", f"{{braces}} {2 ** 10}")
    stdout.print(f"{[1, 2]} {Point(x=1, y=2.0)} {(1, 2)} {{}}", f"{label(4)}{label(5)}")
    stdout.print(str(5), str(-5), str(2.5), str(True), str(name), str([1]), str(Point(x=0, y=0.5)))
    stdout.print(label(10).len(), str(12345).len(), f"é{name}é".len())

    mut built = ""
    for i in range(30):
        built = f"{built}{i % 10}"
    stdout.print(built)

    stdout.print("a".code(), "é".code(), "fox"[1].code(), "7".isdigit(), "x".isdigit())
    stdout.print(
        ",".join(["a", "b", "c"]),
        "".join(["x"]),
        "-".join([]),
        ", ".join([str(n) for n in range(3)]),
    )

    numbers = [0, 1, 2, 3, 4, 5]
    stdout.print(
        numbers[1:3],
        numbers[:2],
        numbers[4:],
        numbers[:],
        numbers[-2:],
        numbers[:-4],
        numbers[3:1],
        numbers[9:],
        numbers[:99],
    )
    words = ["a", "b", "c"]
    part = words[1:]
    stdout.print(part, words, part[0], words[1:][1].len())
    points = [Point(x=1, y=1.0), Point(x=2, y=2.0)]
    stdout.print(points[1:][0].x, points[:1])
    mut changing = numbers[2:]
    changing.append(9)
    changing[0] = 7
    stdout.print(changing, numbers)
    stdout.print(
        any(n > 4 for n in numbers),
        any(n > 9 for n in numbers),
        all(n >= 0 for n in numbers),
        all(n > 0 for n in numbers),
    )
    stdout.print(
        any(n > 4 for n in numbers if n < 3),
        all(n < 3 for n in numbers if n < 3),
        any(c == "o" for c in "fox"),
    )
    stdout.print(
        any(True for _ in numbers[:0]),
        all(False for _ in numbers[:0]),
        "".isdigit(),
        "12".isdigit(),
        "1x".isdigit(),
        "ab".startswith("a"),
        "ab".endswith("a"),
    )
    long = "abcdefghijklmnopqrstuvwxyz"
    stdout.print(
        long[0:9],
        long[3:11],
        long[7:8],
        long[1:17],
        long[:0],
        long[20:],
        long[-3:],
        long[9:9].len(),
        long[3:11].len(),
    )
    stdout.print(
        long[3:11] == "defghijk",
        long[0:8] + long[8:16],
        "é" + long[2:5],
        "aéb"[1:2],
        "aéb"[1:],
    )
    stdout.print({long[0:2]: 1, long[1:3]: 2}[long[1:3]], long[5:9] < long[6:10])
```

It prints:

```text
plain fox fox! <fox> 
1 2.5 True False -7 fox 3
3.14 1,234,567 fox {braces} 1024
[1, 2] Point(x=1, y=2.0) (1, 2) {} n=4n=5
5 -5 2.5 True fox [1] Point(x=0, y=0.5)
4 5 5
012345678901234567890123456789
97 233 111 True False
a,b,c x  0, 1, 2
[1, 2] [0, 1] [4, 5] [0, 1, 2, 3, 4, 5] [4, 5] [0, 1] [] [] [0, 1, 2, 3, 4, 5]
["b", "c"] ["a", "b", "c"] b 1
2 [Point(x=1, y=1.0)]
[7, 3, 4, 5, 9] [0, 1, 2, 3, 4, 5]
True False True False
False True True
False True False True False True False
abcdefghi defghijk h bcdefghijklmnopq  uvwxyz xyz 0 8
True abcdefghijklmnop écde é éb
2 True
```

### `tokenizer`

```fennec
# A tokenizer: strings indexed, sliced, compared and joined natively, and tokens that hold
# their text.

enum Tok:
    Name(text: str)
    Number(value: int)
    Op(text: str)
    Text(body: str)

struct Token:
    tok: Tok
    line: int

def is_digit(c: str) -> bool:
    c >= "0" and c <= "9"

def is_letter(c: str) -> bool:
    (c >= "a" and c <= "z") or (c >= "A" and c <= "Z") or c == "_"

def digit(c: str) -> int:
    match c:
        case "0":
            0
        case "1":
            1
        case "2":
            2
        case "3":
            3
        case "4":
            4
        case "5":
            5
        case "6":
            6
        case "7":
            7
        case "8":
            8
        case _:
            9

def describe(token: Token) -> str:
    match token.tok:
        case .Name("def"):
            "keyword def"
        case .Name(text):
            "name " + text
        case .Number(value):
            f"number {value}"
        case .Op(text):
            "op " + text
        case .Text(body):
            "text " + body + " (" + str(body.len()) + " chars)"

def tokenize(source: str) -> list[Token]:
    mut tokens = []
    mut at = 0
    mut line = 1
    while at < source.len():
        c = source[at]
        if c == "\n":
            line += 1
            at += 1
        elif c == " ":
            at += 1
        elif is_digit(c):
            mut value = 0
            while at < source.len() and is_digit(source[at]):
                value = value * 10 + digit(source[at])
                at += 1
            tokens.append(Token(tok=.Number(value), line=line))
        elif is_letter(c):
            start = at
            while at < source.len() and (is_letter(source[at]) or is_digit(source[at])):
                at += 1
            tokens.append(Token(tok=.Name(source[start:at]), line=line))
        elif c == "\"":
            start = at + 1
            at += 1
            while at < source.len() and source[at] != "\"":
                at += 1
            tokens.append(Token(tok=.Text(source[start:at]), line=line))
            at += 1
        else:
            two = source[at:at + 2]
            if two == "->" or two == "==" or two == "<=":
                tokens.append(Token(tok=.Op(two), line=line))
                at += 2
            else:
                tokens.append(Token(tok=.Op(c), line=line))
                at += 1
    tokens

def main(stdout: Stdout):
    source = "def area(r) -> float:\n    3 * r * r == 12 <= x_1\nprint(\"héllo wörld\", \"\")"
    tokens = tokenize(source)
    for token in tokens:
        stdout.print(token.line, describe(token))
    stdout.print(tokens.len(), source.len(), source[-3:], source[100:], source[-1], source[4])

    word = "naïve"
    stdout.print(word.len(), word[2], word[-1], word[1:4], word + "!", "ï" in word, "x" not in word)
    stdout.print(
        "abc" < "abd",
        "b" > "abc",
        "" < "a",
        "Z" < "a",
        "é" > "z",
        word == "naïve",
        word != "naive",
    )
    copy = word
    joined = copy + copy
    stdout.print(joined, joined.len(), joined[5:], copy == word)
```

It prints:

```text
1 keyword def
1 name area
1 op (
1 name r
1 op )
1 op ->
1 name float
1 op :
2 number 3
2 op *
2 name r
2 op *
2 name r
2 op ==
2 number 12
2 op <=
2 name x_1
3 name print
3 op (
3 text héllo wörld (11 chars)
3 op ,
3 text  (0 chars)
3 op )
23 73 "")  ) a
5 ï e aïv naïve! True True
True True True True True True True
naïvenaïve 10 naïve True
```

### `tree_values`

```fennec
# Boxed values going everywhere values go: through lists, loops that leave early, `match`
# on values made on the spot, and structs that hold them.

struct Point:
    x: int
    y: int

enum Shape:
    Dot(at: Point)
    Group(first: Shape, rest: Shape)
    Moved(by: Point, inner: Shape)

struct Layer:
    depth: int
    origin: Point
    shape: Shape

def count(shape: Shape) -> int:
    match shape:
        case .Dot(_):
            1
        case .Group(first, rest):
            count(first) + count(rest)
        case .Moved(_, inner):
            count(inner)

def shift(shape: Shape, dx: int) -> Shape:
    match shape:
        case .Dot(at):
            .Dot(Point(x=at.x + dx, y=at.y))
        case .Group(first, rest):
            .Group(shift(first, dx), shift(rest, dx))
        case moved:
            .Moved(Point(x=dx, y=0), moved)

def first_dot(shape: Shape) -> Point:
    mut at = shape
    while True:
        match at:
            case .Dot(point):
                return point
            case .Group(first, _):
                at = first
            case .Moved(_, inner):
                at = inner

def deepest(shapes: list[Shape], limit: int) -> int:
    mut best = 0
    for shape in shapes:
        n = count(shape)
        if n > limit: break
        if n > best:
            best = n
    best

def row(n: int) -> Shape:
    mut shape = Shape.Dot(Point(x=0, y=0))
    for i in range(1, n):
        shape = .Group(.Dot(Point(x=i, y=i * i)), shape)
    shape

def main(stdout: Stdout):
    shapes = [row(1), row(3), shift(row(4), 10), row(50)]
    stdout.print(deepest(shapes, 10), shapes.len())
    for shape in shapes:
        stdout.print(count(shape), first_dot(shape))
    stdout.print(shapes[2])

    match shift(row(2), 5):
        case .Group(.Dot(at), rest):
            stdout.print(at, count(rest))
        case other:
            stdout.print("other", other)

    mut layer = Layer(depth=1, origin=Point(x=0, y=0), shape=row(2))
    before = layer
    layer.depth += 1
    layer.origin.x = 7
    layer.shape = .Moved(layer.origin, layer.shape)
    stdout.print(layer)
    stdout.print(before)
    stdout.print(before == layer, before.shape == row(2), layer.shape != before.shape)
    stdout.print(f"{count(layer.shape)} dots, depth {layer.depth}")
```

It prints:

```text
4 4
1 Point(x=0, y=0)
3 Point(x=2, y=4)
4 Point(x=13, y=9)
50 Point(x=49, y=2401)
Shape.Group(first=Shape.Dot(at=Point(x=13, y=9)), rest=Shape.Group(first=Shape.Dot(at=Point(x=12, y=4)), rest=Shape.Group(first=Shape.Dot(at=Point(x=11, y=1)), rest=Shape.Dot(at=Point(x=10, y=0)))))
Point(x=6, y=1) 1
Layer(depth=2, origin=Point(x=7, y=0), shape=Shape.Moved(by=Point(x=7, y=0), inner=Shape.Group(first=Shape.Dot(at=Point(x=1, y=1)), rest=Shape.Dot(at=Point(x=0, y=0)))))
Layer(depth=1, origin=Point(x=0, y=0), shape=Shape.Group(first=Shape.Dot(at=Point(x=1, y=1)), rest=Shape.Dot(at=Point(x=0, y=0))))
False True True
2 dots, depth 2
```

### `trees`

```fennec
# Types that contain themselves, like syntax trees and linked lists. They're values like
# any other: copies share memory until one of them changes, and nothing can tell.

enum Expr:
    Num(value: int)
    Var(slot: int)
    Add(left: Expr, right: Expr)
    Mul(left: Expr, right: Expr)
    Neg(inner: Expr)
    Let(slot: int, value: Expr, body: Expr)

    def size(self) -> int:
        match self:
            case .Add(a, b):
                1 + a.size() + b.size()
            case .Mul(a, b):
                1 + a.size() + b.size()
            case .Neg(inner):
                1 + inner.size()
            case .Let(_, value, body):
                1 + value.size() + body.size()
            case _:
                1

struct Env:
    a: int
    b: int

def lookup(env: Env, slot: int) -> int:
    env.a if slot == 0 else env.b

def bind(env: Env, slot: int, value: int) -> Env:
    Env(a=value, b=env.b) if slot == 0 else Env(a=env.a, b=value)

def eval(e: Expr, env: Env) -> int:
    match e:
        case .Num(n):
            n
        case .Var(slot):
            lookup(env, slot)
        case .Add(a, b):
            eval(a, env) + eval(b, env)
        case .Mul(a, b):
            eval(a, env) * eval(b, env)
        case .Neg(inner):
            -eval(inner, env)
        case .Let(slot, value, body):
            eval(body, bind(env, slot, eval(value, env)))

def simplify(e: Expr) -> Expr:
    match e:
        case .Add(.Num(0), right):
            simplify(right)
        case .Add(left, .Num(0)):
            simplify(left)
        case .Mul(.Num(1), right):
            simplify(right)
        case .Mul(.Num(0), _):
            .Num(0)
        case .Neg(.Neg(inner)):
            simplify(inner)
        case .Add(a, b):
            .Add(simplify(a), simplify(b))
        case .Mul(a, b):
            .Mul(simplify(a), simplify(b))
        case .Neg(inner):
            .Neg(simplify(inner))
        case .Let(slot, value, body):
            .Let(slot, simplify(value), simplify(body))
        case other:
            other

def build(depth: int, seed: int) -> Expr:
    if depth == 0: return Expr.Var(seed % 2) if seed % 4 == 0 else .Num(seed % 10)
    left = build(depth - 1, seed * 3 + 1)
    right = build(depth - 1, seed * 7 + 2)
    match seed % 5:
        case 0:
            .Add(left, right)
        case 1:
            .Mul(.Num(1), .Add(left, .Num(0)))
        case 2:
            .Add(.Neg(.Neg(left)), right)
        case 3:
            .Let(seed % 2, left, right)
        case _:
            .Mul(left, .Neg(right))

enum Chain:
    End
    Link(value: int, rest: Chain)

def count_down(n: int) -> Chain:
    mut chain = Chain.End
    for i in range(n):
        chain = .Link(i, chain)
    chain

def total(chain: Chain) -> int:
    mut sum = 0
    mut at = chain
    while True:
        match at:
            case .Link(value, rest):
                sum += value
                at = rest
            case .End:
                break
    sum

struct Program:
    name: str
    body: Expr
    runs: int

def main(stdout: Stdout):
    env = Env(a=3, b=-2)
    small = Expr.Let(0, .Add(.Num(2), .Var(1)), .Mul(.Var(0), .Neg(.Num(5))))
    stdout.print(small)
    stdout.print(eval(small, env), small.size())

    tree = build(12, 1)
    simpler = simplify(tree)
    stdout.print(tree.size(), simpler.size(), eval(tree, env), eval(simpler, env))
    stdout.print(simplify(simpler) == simpler, tree == simpler, build(12, 1) == tree)

    # A copy shares the tree; changing one changes nothing else.
    mut program = Program(name="calc", body=small, runs=0)
    saved = program
    program.runs += 1
    program.body = .Add(program.body, .Num(1))
    stdout.print(program.runs, saved.runs, eval(program.body, env), eval(saved.body, env))
    stdout.print(saved.body == small, program.body == small)

    chain = count_down(1000)
    stdout.print(total(chain), total(.Link(5, chain)) - total(chain))
    match chain:
        case .Link(first, .Link(second, _)):
            stdout.print(first, second)
        case _:
            stdout.print("short")
```

It prints:

```text
Expr.Let(slot=0, value=Expr.Add(left=Expr.Num(value=2), right=Expr.Var(slot=1)), body=Expr.Mul(left=Expr.Var(slot=0), right=Expr.Neg(inner=Expr.Num(value=5))))
0 8
2543 1345 -1764 -1764
True False True
1 0 1 0
True False
499500 5
999 998
```

### `types_more`

```fennec
from std.game import IdMap
from std.layout import SoaList

enum Light:
    Red
    Green

    def default() -> Light:
        Light.Red

enum Coin:
    Heads
    Tails

struct Money:
    cents: int

    def <(self, other: Money) -> bool:
        self.cents < other.cents

    def /(self, parts: int) -> Money:
        Money(cents=self.cents // parts)

    def %(self, parts: int) -> Money:
        Money(cents=self.cents % parts)

struct Score:
    value: int where value >= 0

struct Door:
    out: Stdout

    def del(self):
        self.out.print("closing the door")

struct Player:
    name: str

    def greet(self) -> str:
        f"hi, {self.name}"

def find(name: str) -> Player?:
    if name == "Kit": return Player(name="Kit")
    None

def parse(text: str) -> Result[int, str]:
    if not text.isdigit(): return Err(f"{text} isn't a number")
    Ok(int(text) else 0)

def call(make: () -> int) -> int:
    make()

def combine(join: (int, int) -> int, a: int, b: int) -> int:
    join(a, b)

def main(stdout: Stdout):
    # A named constructor that takes nothing can be used without parentheses.
    stdout.print(Light.default(), Light.default, Light.Green)
    # A case name that only one enum has needs no type in front of it.
    stdout.print([.Heads, .Tails], .Green)

    # Types with their own `<` get the other comparisons from it.
    small = Money(cents=5)
    big = Money(cents=9)
    stdout.print(small <= big, big <= small, small >= big, big >= small, small <= small)
    stdout.print(big / 2, big % 2)

    # `where` rules: literals are checked as the program is read; anything else gives a result.
    for value in [5, -1]:
        stdout.print(Score(value=value))

    # `with` runs `del` when the block ends.
    with Door(out=stdout) as door:
        door.out.print("inside")
    stdout.print("outside")

    # `?.` skips a method call on None.
    stdout.print(
        find("Vex")?.greet(),
        find("Kit")?.greet(),
        find("Kit") is not None,
        find("Vex") is not None,
    )

    # Arrow functions can take no arguments, or several, when passed for a parameter with a
    # function type.
    stdout.print(
        call(() => 42),
        combine((a, b) => a + b, 2, 3),
        combine((first, second) => second, 1, 2),
    )

    # Collections can end with a comma.
    stdout.print({"a": 1}, {1, 2}, [1], (1,))

    # A function can return a result it made itself.
    stdout.print(parse("12"), parse("twelve"))

    # `and` and `or` stop as soon as they know the answer.
    stdout.print(True or 1 // 0 == 0, False and 1 // 0 == 0, True and False, False or True)

    # An Id stops working once its item is removed, even after its slot is reused.
    mut enemies = IdMap[str]()
    ash = enemies.insert("ash")
    enemies.remove(ash)
    kit = enemies.insert("kit")
    stdout.print(enemies.get(ash), enemies[kit])

    stdout.print(SoaList[int](), SoaList[int]().len())
```

It prints:

```text
Light.Red Light.Red Light.Green
[Coin.Heads, Coin.Tails] Light.Green
True False False True True
Money(cents=4) Money(cents=1)
Ok(Score(value=5))
Err(InvalidValue(message="Score.value breaks its `where` rule"))
inside
closing the door
outside
None hi, Kit True False
42 5 2
{"a": 1} {1, 2} [1] (1,)
Ok(12) Err("twelve isn't a number")
True False False True
None kit
[] 0
```

### `values`

```fennec
def add_bonus(mut scores: list[int], bonus: int):
    for mut score in scores:
        score += bonus

def main(stdout: Stdout):
    original = [1, 2, 3]
    mut copy = original
    copy.append(4)
    stdout.print(original)
    stdout.print(copy)

    mut scores = [10, 20, 30]
    add_bonus(mut scores, 5)
    stdout.print(scores)
```

It prints:

```text
[1, 2, 3]
[1, 2, 3, 4]
[15, 25, 35]
```

### `vectors`

```fennec
# Plain structs, made only of numbers, bools and other plain structs, are values: copying
# one copies it, and only a `mut` name or `mut self` changes one in place.
from std.math import Vec3, ceil, cos, floor, sin, sqrt

struct Body:
    position = Vec3.zero
    velocity = Vec3(x=1.0)
    mass = 1.0
    asleep = False

    def kick(mut self, push: Vec3):
        self.velocity = self.velocity + push * (1.0 / self.mass)
        self.asleep = False

    def moved(self, dt: float) -> Body:
        Body(position=self.position + self.velocity * dt, velocity=self.velocity, mass=self.mass)

def settle(mut body: Body):
    body.velocity = Vec3.zero()
    body.asleep = True

def heaviest(a: Body, b: Body) -> Body:
    a if a.mass >= b.mass else b

def main(stdout: Stdout):
    mut ball = Body(mass=2.0)
    copy = ball
    ball.kick(Vec3(y=4.0))
    stdout.print(ball.velocity, copy.velocity)

    later = ball.moved(0.5)
    stdout.print(later.position, ball.position)

    ball.position.x = 3.0
    ball.position.z += 1.5
    stdout.print(ball.position, ball.position.length())

    settle(mut ball)
    stdout.print(ball.velocity, ball.asleep)

    rock = Body(mass=5.0)
    stdout.print(heaviest(ball, rock).mass, ball == copy, copy == Body(mass=2.0))

    mut bodies = [Body(mass=float(n)) for n in range(1, 4)]
    for mut body in bodies:
        body.kick(Vec3(x=float(body.mass)))
    for body in bodies:
        stdout.print(body.velocity.x)
    stdout.print(sqrt(16.0), floor(2.7), ceil(2.1), round(sin(0.0)), cos(0.0))
```

It prints:

```text
Vec3(x=1.0, y=2.0, z=0.0) Vec3(x=1.0, y=0.0, z=0.0)
Vec3(x=0.5, y=1.0, z=0.0) Vec3(x=0.0, y=0.0, z=0.0)
Vec3(x=3.0, y=0.0, z=1.5) 3.3541019662496847
Vec3(x=0.0, y=0.0, z=0.0) True
5.0 False True
2.0
2.0
2.0
4.0 2 3 0 1.0
```

### `word_counts`

```fennec
## Dicts keyed by ints and strings: counting, looking up, changing and walking them.

def tally(words: list[str]) -> dict[str, int]:
    mut counts: dict[str, int] = {}
    for word in words:
        counts[word] = counts.get(word, 0) + 1
    return counts

def total(counts: dict[str, int]) -> int:
    mut sum = 0
    for word in counts:
        sum += counts[word]
    return sum

def bump(mut counts: dict[str, int], word: str):
    counts[word] = counts.get(word, 0) + 1

def main(stdout: Stdout):
    counts = tally("the fox and the hound and the fennec".split())
    stdout.print(counts, total(counts), counts.len())
    for word, count in counts.items():
        if count > 1:
            stdout.print(word, count)

    mut squares: dict[int, int] = {}
    for i in range(1_000):
        squares[i * 7 % 1_000] = i * i
    stdout.print(
        squares.len(),
        squares[7],
        squares[993],
        50 in squares,
        5_000 in squares,
        3 not in squares,
    )

    # A copy keeps its value when the original changes.
    copy = squares
    squares[7] = -1
    squares[7] += 10
    stdout.print(copy[7], squares[7], squares == copy)

    # The loop goes over the dict as it was when the loop started.
    mut grow: dict[int, int] = {1: 1}
    for key in grow:
        grow[key + 1] = key
    stdout.print(grow)

    mut names: dict[str, str] = {"a": "apple"}
    names["b"] = "banana"
    names["a"] += " pie"
    for name in names.values():
        stdout.print(name)
    stdout.print(names["a"], names.get("z", "none"), names.get("b", "none"))

    mut flags: dict[int, bool] = {}
    flags[1] = True
    flags[2] = False
    mut ratios: dict[str, float] = {"half": 0.5}
    ratios["third"] = 1.0 / 3.0
    stdout.print(flags, ratios, flags[2], ratios["half"] * 2.0)
    removed = squares.pop(7)
    stdout.print(removed, squares.len(), 7 in squares)
    mut seen: dict[str, int] = {}
    bump(mut seen, "fox")
    bump(mut seen, "fox")
    bump(mut seen, "den")
    stdout.print(seen)
```

It prints:

```text
{"the": 3, "fox": 1, "and": 2, "hound": 1, "fennec": 1} 8 5
the 3
and 2
1000 1 998001 True False False
1 9 False
{1: 1, 2: 1}
apple pie
banana
apple pie none banana
{1: True, 2: False} {"half": 0.5, "third": 0.3333333333333333} False 1.0
9 999 False
{"fox": 2, "den": 1}
```

## Planned

No example shows these yet, so don't rely on them. Some are partly built, and some aren't started. [The decisions](https://github.com/fennec-language/fennec/blob/main/docs/decisions.md) say why each was made.

### Values and memory

- Everything is a value unless the code says otherwise.
- Fennec has no object graphs. Data lives in tables.
- `mut x = place` edits a place, however deeply it's nested.
- A copy that can go stale is a compile error.
- Cut from the first version, and each can be added later without breaking anything:
- Named fallbacks, used only on evidence.
- Parameters are borrowed by default.
- A value is moved, not shared, when it isn't used again.
- Views can't escape.
- Copies are visible.

### Collections

- They're a small header over a shared buffer.
- Fixed-size arrays live entirely inline.
- Short strings live in the header.
- Indexing a string takes constant time when it's ASCII.

### Layout

- Structs inside structs are stored inline.
- Fields always have names.
- Recursive types need no annotation.
- The compiler chooses the layout.
- Structs in `extern` modules use C's layout exactly.

### Allocation

- The compiler and runtime choose allocation, and code never names an allocator.
- The compiler places short-lived values itself.
- `@hot` on a function means prove it or don't compile.
- `@arena` is cut for now,
- Debug builds report allocation.

### Determinism

- Evaluation is always left to right.
- Floats give the same results on every platform.
- Parallel reductions combine in a fixed order.
- Any run can be recorded and replayed exactly.

### Functions and types

- A function that can fail says so with `fails Name`, and every exit is visible.
- `where` rules run on every write to a guarded field,
- Every `def` writes its return type. Arrow functions never do. No `->` means the function returns nothing.
- Built-in types are ordinary structs in the standard library.
- `Buffer[T]` is the one public building block for memory.
- A function is global only when it works on any sequence using nothing but looping and operators; anything that asks a value about itself is a method.
- `sum` needs a start for anything but numbers.
- No global function serves only one type.
- The standard library uses only what any program can.
- A type is said by calling it with its type arguments:
- A generic type's extension names its type parameter, and a bound makes it conditional,
- Units of measure are types.
- An enum fits an interface when every case does.
- Type parameters are written the way Python 3.12 writes them.
- Generic code is checked once, and errors name the missing piece.
- Generics compile the way Swift's do.
- Mixed types are an `enum`; open sets are function values.
- `Eq`, `Hash`, `Ordered` and `Format` are built-in interfaces,
- `=>` is one expression; anything longer is a nested `def`.
- Functions used as values never change anything around them.

### Lifetimes

- A lifetime is a block.
- `with` is only for real lifecycles:
- Names don't leak out of blocks, and reusing a name still in scope is a compile error.
- `with` runs cleanup on every way out of its block:
- The value after `with` must be a plain value.
- `def del` can't fail.
- Finishing that can fail is an explicit method,
- A type can require its finish.

### Standard library

- A module is in `std` if most programs need it, it's security-sensitive, or it has to be consistent everywhere.
- `std.net` holds TCP, UDP, TLS, an HTTP client and server, and WebSocket, all through the `Net` capability.
- `std.sql` is one standard database interface,
- `Tree` is in `std`.
- `std.process`, `std.crypto`, `std.log`, `std.cli` and `std.csv` are in `std` too.
- `std.crypto` holds hashing, including BLAKE3, so there's no separate `std.hash`.
- First-party kits, not `std`:
- Names live in their modules.
- What the language itself needs is available everywhere.
- Network access is a capability, `Net`, like `Fs`.
- Environment variables and running other programs are capabilities too, `Env` and `Process`,
- A string's characters are reached by code and as bytes without copying.
- Failures inside the standard library point at the program's call.
- Regular expressions take linear time.

### Visibility

- `pub` marks what other kits can use, and goes on top-level things only.
- A kit's API is checked against its last published version,

### Files and kits

- A kit is a folder with a `kit.toml` and its code under `src`.
- A file uses another file's names only after importing them.
- A kit's files share one set of top-level names.
- Imports can't go in a circle.

### Concurrency

- `shared Name:` is the only reference type.
- Lock bodies are short and can't deadlock.
- `with launch(physics_loop) as physics:` runs a long-lived task.
- Channels are bounded ring buffers.
- A long-running loop should never wait on another.
- There's one scheduler, with I/O multiplexed by the OS.
- A failure in a server task is meant to stay in that task.
- Reference counts are biased for values that cross tasks.
- Tasks are scoped.
- `parallel` takes only collections and streams,
- Tasks change only separate pieces.
- A value crosses a channel like it goes anywhere else.

### Syntax

- String escapes are Python's:
- There's no walrus (`:=`).
- Raw strings are `r"..."`.
- Doc comments are `##`, above what they document.
- `"  " * depth` repeats a string.
- Keywords can't be names, including field names.
- f-strings have `{x=}` and no `!r`.

### SIMD and GPU

- `Simd[T, N]` is the one vector type.
- `f16` and `bf16` are number types.
- `.lanes[N]()` walks a buffer `N` elements at a time.
- GPU access is a capability, `Gpu`,
- `GpuBuffer[T]` is GPU memory.
- Kernels are ordinary functions, run with `gpu.run`.
- Kernels compile through two small in-house backends:
- GPU floats are the same on the same device, not across vendors.
- Later slices, in order:

### Native code

- If it can be Fennec, it is Fennec.
- C is declared in an `extern "C":` block.
- Every call into C is inside `unsafe("reason")`.
- A small, fixed set of types crosses the boundary:
- Pointers are as safe as the compiler can make them.
- Declarations are checked against the real headers.
- Native code is caught misbehaving early.
- When the program stops itself,
- No C is called inside `parallel` bodies.
- `fennec audit --unsafe` lists every `unsafe` block in the dependency tree,
- Libraries are named in `kit.toml`:
- In v1, raw C is allowed only in the standard library's OS layer, first-party `fennec/` kits and your own app.
- There's no C compiled to wasm in v1.
- Every first-party native kit has a plan to become pure Fennec,
- An app allows each native kit by name:
- The interpreter calls only the standard library's own OS layer, never a kit's `extern` functions.
