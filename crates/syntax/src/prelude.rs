//! The part of the standard library written in Fennec itself.

/// The standard library written in Fennec, loaded before every program by both the
/// interpreter and the type checker.
pub const PRELUDE: [&str; 7] = [ERRORS, MATH, PATH, COLLECTIONS, RANDOM, TIME, RE];

const ERRORS: &str = r"
error IoError:
    reason: str

error InvalidValue:
    message: str

error ParseError:
    message: str
    line: int
    column: int
";

const MATH: &str = r"
from std.math import sqrt

struct Vec3:
    x = 0.0
    y = 0.0
    z = 0.0

    def zero():
        Vec3()

    def +(self, other: Vec3):
        Vec3(x=self.x + other.x, y=self.y + other.y, z=self.z + other.z)

    def -(self, other: Vec3):
        Vec3(x=self.x - other.x, y=self.y - other.y, z=self.z - other.z)

    def *(self, scale: float):
        Vec3(x=self.x * scale, y=self.y * scale, z=self.z * scale)

    def dot(self, other: Vec3):
        self.x * other.x + self.y * other.y + self.z * other.z

    def length(self):
        sqrt(self.dot(self))

    def distance_to(self, other: Vec3):
        (self - other).length()
";

const PATH: &str = r#"
## A file path, like `saves/slot1.json`. Its parts are separated by `/`. Every method only
## looks at the text; none of them touch the file system.
struct Path:
    text: str

    ## `Path("saves") / "slot1.json"` is the same as `.join`.
    def /(self, part: str) -> Path:
        self.join(part)

    ## This path with `part` added. A `part` that starts with `/` replaces the path.
    def join(self, part: str) -> Path:
        if part.startswith("/") or self.text == "":
            return Path(part)
        if self.text.endswith("/"):
            return Path(self.text + part)
        Path(self.text + "/" + part)

    ## Whether the path starts at the root, `/`.
    def is_absolute(self) -> bool:
        self.text.startswith("/")

    ## The names between the `/`s, leaving out empty ones and `.`.
    def parts(self) -> list[str]:
        [part for part in self.text.split("/") if part != "" and part != "."]

    ## The last part: `fox.png` for `assets/fox.png`.
    def name(self) -> str:
        parts = self.parts()
        if len(parts) == 0:
            return ""
        parts[-1]

    ## What follows the name's last `.`, without the dot: `png` for `fox.png`. Empty for
    ## names like `notes` and `.hidden`.
    def extension(self) -> str:
        pieces = self.name().split(".")
        if len(pieces) < 2 or (len(pieces) == 2 and pieces[0] == ""):
            return ""
        pieces[-1]

    ## The name without its extension: `fox` for `fox.png`.
    def stem(self) -> str:
        name = self.name()
        extension = self.extension()
        if extension == "":
            return name
        name[:len(name) - len(extension) - 1]

    ## The path without its last part: `assets` for `assets/fox.png`, and `.` for `fox.png`.
    def parent(self) -> Path:
        parts = self.parts()
        if len(parts) == 0:
            return self.rebuild([])
        self.rebuild(parts[:len(parts) - 1])

    ## The same path written simply: `a/./b/../c` becomes `a/c`. A `..` that would go above
    ## the start stays in a relative path and is dropped from an absolute one.
    def normalize(self) -> Path:
        mut kept = []
        for part in self.parts():
            if part != "..":
                kept.append(part)
            elif len(kept) > 0 and kept[-1] != "..":
                kept.pop()
            elif not self.is_absolute():
                kept.append("..")
        self.rebuild(kept)

    ## `parts` joined by `/`, starting at the root when this path does.
    def rebuild(self, parts: list[str]) -> Path:
        text = "/".join(parts)
        if self.is_absolute():
            return Path("/" + text)
        if text == "":
            return Path(".")
        Path(text)
"#;

const COLLECTIONS: &str = r"
## A double-ended queue: adding or removing at either end is quick however long it gets.
## For breadth-first searches, event queues and undo histories.
struct Deque[T]:
    ## The items sit in a ring that starts at `head`. Slots outside it hold stale items.
    slots: list[T] = []
    head = 0
    count = 0

    ## A deque holding `items`, first to last.
    def of(items: list[T]) -> Deque[T]:
        Deque(slots=items, count=len(items))

    def len(self) -> int:
        self.count

    ## Adds an item at the back.
    def append(mut self, item: T):
        self.make_room(item)
        self.slots[(self.head + self.count) % len(self.slots)] = item
        self.count += 1

    ## Adds an item at the front.
    def appendleft(mut self, item: T):
        self.make_room(item)
        self.head = (self.head - 1) % len(self.slots)
        self.slots[self.head] = item
        self.count += 1

    ## Removes the item at the back and gives it back, or None when the deque is empty.
    def pop(mut self) -> T?:
        if self.count == 0:
            return None
        self.count -= 1
        self.slots[(self.head + self.count) % len(self.slots)]

    ## Removes the item at the front and gives it back, or None when the deque is empty.
    def popleft(mut self) -> T?:
        if self.count == 0:
            return None
        item = self.slots[self.head]
        self.head = (self.head + 1) % len(self.slots)
        self.count -= 1
        item

    ## The item at the front, or None when the deque is empty.
    def first(self) -> T?:
        if self.count == 0:
            return None
        self.slots[self.head]

    ## The item at the back, or None when the deque is empty.
    def last(self) -> T?:
        if self.count == 0:
            return None
        self.slots[(self.head + self.count - 1) % len(self.slots)]

    def clear(mut self):
        self.slots = []
        self.head = 0
        self.count = 0

    ## The items, front to back.
    def items(self) -> list[T]:
        [self.slots[(self.head + at) % len(self.slots)] for at in range(self.count)]

    def iter(self) -> list[T]:
        self.items()

    ## Makes room for one more item, doubling the slots when they're full. `filler` fills
    ## the new slots.
    def make_room(mut self, filler: T):
        if self.count < len(self.slots):
            return
        mut slots = self.items()
        for _ in range(len(slots) + 1):
            slots.append(filler)
        self.slots = slots
        self.head = 0

## A binary min-heap: `pop` gives back the smallest item. Items are compared with `<`, so
## they can be numbers, strings, tuples, or structs with `def <`. For the largest first,
## push negated numbers, or use a `PriorityQueue` with negated priorities.
struct Heap[T: Ordered]:
    items: list[T] = []

    ## A heap holding `items`.
    def of(items: list[T]) -> Heap[T]:
        mut heap = Heap(items=items)
        for at in range(len(items) // 2 - 1, -1, -1):
            heap.sift_down(at)
        heap

    def len(self) -> int:
        len(self.items)

    def push(mut self, item: T):
        self.items.append(item)
        mut at = len(self.items) - 1
        while at > 0:
            parent = (at - 1) // 2
            if not self.items[at] < self.items[parent]:
                break
            self.swap(at, parent)
            at = parent

    ## Removes the smallest item and gives it back, or None when the heap is empty.
    def pop(mut self) -> T?:
        if len(self.items) == 0:
            return None
        smallest = self.items[0]
        last = self.items.pop()
        if len(self.items) > 0:
            self.items[0] = last
            self.sift_down(0)
        smallest

    ## The smallest item, or None when the heap is empty.
    def peek(self) -> T?:
        if len(self.items) == 0:
            return None
        self.items[0]

    def clear(mut self):
        self.items = []

    ## Moves the item at `start` down until it's no bigger than the items below it.
    def sift_down(mut self, start: int):
        mut at = start
        while True:
            left = 2 * at + 1
            if left >= len(self.items):
                break
            mut child = left
            if left + 1 < len(self.items) and self.items[left + 1] < self.items[left]:
                child = left + 1
            if not self.items[child] < self.items[at]:
                break
            self.swap(at, child)
            at = child

    def swap(mut self, a: int, b: int):
        item = self.items[a]
        self.items[a] = self.items[b]
        self.items[b] = item

## A queue where items come out lowest priority first, and items with the same priority
## come out in the order they went in. For highest first, negate the priorities.
struct PriorityQueue[T, P: Ordered]:
    ## Each item's priority and the order it was pushed in, smallest first. Items wait in
    ## `waiting` by that order, so they needn't have an order themselves.
    heap: Heap[(P, int)] = Heap()
    waiting: dict[int, T] = {}
    pushed = 0

    def len(self) -> int:
        self.heap.len()

    def push(mut self, item: T, priority: P):
        self.heap.push((priority, self.pushed))
        self.waiting[self.pushed] = item
        self.pushed += 1

    ## Removes the item with the lowest priority and gives it back, or None when the queue
    ## is empty.
    def pop(mut self) -> T?:
        entry = self.heap.pop()
        if entry is None:
            return None
        priority, order = entry
        self.waiting.pop(order)

    ## The item with the lowest priority, or None when the queue is empty.
    def peek(self) -> T?:
        entry = self.heap.peek()
        if entry is None:
            return None
        priority, order = entry
        self.waiting[order]

## Counts how many times each item appears. Items keep the order they were first counted in.
struct Counter[T: Hashable]:
    counts: dict[T, int] = {}

    ## A counter of `items`.
    def of(items: list[T]) -> Counter[T]:
        mut counter = Counter()
        counter.update(items)
        counter

    ## How many different items it has counted.
    def len(self) -> int:
        len(self.counts)

    ## Counts one more `item`.
    def add(mut self, item: T):
        self.counts[item] = self.counts.get(item, 0) + 1

    ## Counts each of `items`.
    def update(mut self, items: list[T]):
        for item in items:
            self.add(item)

    ## How many times `item` was counted: 0 if never.
    def [](self, item: T) -> int:
        self.counts.get(item, 0)

    def []=(mut self, item: T, count: int):
        self.counts[item] = count

    ## How many items were counted in all.
    def total(self) -> int:
        sum(self.counts.values())

    ## The `n` most common items and their counts, most common first. Ties keep the order
    ## the items were first counted in.
    def most_common(self, n: int) -> list[(T, int)]:
        pairs = self.counts.items()
        counts = self.counts.values()
        ranked = sorted(range(len(pairs)), key=at => -counts[at])
        [pairs[at] for at in ranked[:n]]

    ## Each item and its count, in the order they were first counted.
    def items(self) -> list[(T, int)]:
        self.counts.items()

    def iter(self) -> list[T]:
        self.counts.keys()
";

const RANDOM: &str = r"
from std.random import random_int, random_float, random_choice

## A seeded random number generator. The same seed gives the same numbers in the same order
## on every machine, so games replay and tests repeat exactly. It's SplitMix64: fast, and
## good enough for games, but not for secrets.
struct Random:
    seed: int
    ## How many numbers it has drawn so far.
    drawn = 0

    ## A whole number from `low` to `high`, both included.
    def randint(mut self, low: int, high: int) -> int:
        value, drawn = random_int(self.seed, self.drawn, low, high)
        self.drawn = drawn
        value

    ## A float from 0.0 up to, but not including, 1.0.
    def random(mut self) -> float:
        value, drawn = random_float(self.seed, self.drawn)
        self.drawn = drawn
        value

    ## A float from `low` up to, but not including, `high`.
    def uniform(mut self, low: float, high: float) -> float:
        low + (high - low) * self.random()

    ## One of `items`, picked at random.
    def choice[T](mut self, items: list[T]) -> T:
        item, drawn = random_choice(self.seed, self.drawn, items)
        self.drawn = drawn
        item

    ## Puts `items` in a random order.
    def shuffle[T](mut self, mut items: list[T]):
        for at in range(len(items) - 1, 0, -1):
            other = self.randint(0, at)
            item = items[at]
            items[at] = items[other]
            items[other] = item
";

const TIME: &str = r"
## A length of time, to the nanosecond. Made with `Duration.seconds(0.5)`,
## `Duration.millis(16)` or `Duration(nanos=...)`.
struct Duration:
    nanos = 0

    def seconds(seconds: float) -> Duration:
        Duration(nanos=round(seconds * 1_000_000_000.0))

    def millis(millis: int) -> Duration:
        Duration(nanos=millis * 1_000_000)

    def micros(micros: int) -> Duration:
        Duration(nanos=micros * 1_000)

    ## The duration in seconds, for moving things by `speed * dt.as_seconds()`.
    def as_seconds(self) -> float:
        float(self.nanos) / 1_000_000_000.0

    def as_millis(self) -> float:
        float(self.nanos) / 1_000_000.0

    def +(self, other: Duration) -> Duration:
        Duration(nanos=self.nanos + other.nanos)

    def -(self, other: Duration) -> Duration:
        Duration(nanos=self.nanos - other.nanos)

    def *(self, times: int) -> Duration:
        Duration(nanos=self.nanos * times)

    def <(self, other: Duration) -> bool:
        self.nanos < other.nanos

## A moment, read from a `Clock`. Subtract two to get the `Duration` between them.
struct Instant:
    ## Nanoseconds since the clock started.
    nanos = 0

    def -(self, earlier: Instant) -> Duration:
        Duration(nanos=self.nanos - earlier.nanos)

    def +(self, later: Duration) -> Instant:
        Instant(nanos=self.nanos + later.nanos)

    def <(self, other: Instant) -> bool:
        self.nanos < other.nanos
";

const RE: &str = r#"
from std.re import is_regex

## A regular expression. Matching takes time in proportion to the text's length whatever the
## pattern, since nothing backtracks. Patterns follow Python's `re`: `.`, classes like `[a-z]`
## and `\d`, anchors `^ $ \b`, repeats `* + ? {m,n}` (lazy with a `?` after), groups,
## `(?:...)` and `|`. Write them as raw strings, like `Regex(r"\d+")`.
##
## Its methods: `match(text)` at the start, `fullmatch(text)` for all of it, `search(text)`
## anywhere (each gives a `Match?`); `find_all(text)`; `replace(text, with)`, where `\1` in
## `with` is group 1; and `split(text)`.
struct Regex:
    pattern: str where is_regex(pattern)

## Where a `Regex` matched, and what its groups caught.
struct Match:
    text: str
    ## Where the match starts and ends, counting characters, as in `text[start:end]`.
    start: int
    end: int
    ## What each group caught, from group 1 on: None for a group that took no part.
    groups: list[str?]

    ## Group `n`'s text: 0 for the whole match.
    def [](self, n: int) -> str?:
        if n == 0:
            return self.text
        self.groups[n - 1]
"#;
