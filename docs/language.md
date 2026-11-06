# Fennec on one page

This page is the source of truth for the language. If `decisions.md` disagrees with it, this page wins. Most of the syntax here isn't built yet.

Fennec reads at a glance, builds almost instantly, and is built to run as fast as hand-written systems code in release builds. Everything is a plain value by default, and the compiler does the hard work.

## Four rules

- A name holds its own value.
- `mut` lets a name change.
- To change something in a list, write to it there.
- `ref` lets a loop or a function change yours.

```fennec
def heal_all(ref units: list[Unit]):
    for ref unit in units:
        unit.health += 10

def main(stdout: Stdout):
    mut units = [Unit(name="Ana", health=50), Unit(name="Bo", health=70)]
    units[0].health -= 10
    heal_all(ref units)

    target = units[1]              # your own value
    updated_health = target.health + 1
```

There's no inline `ref`.

## What you can declare

```fennec
struct Unit:               # a value
    name: str
    health = 100 where health >= 0

enum Action:               # a value with cases
    Wait
    Attack(target_id: Id[Unit])

error SellError:           # a failure with cases
    NotFound(item: Item)
    OutOfStock(item: Item)

shared Game:               # one thing, with a lock, no cycles
    players: list[Player]

interface Shape:           # what a type can do
    def area(self) -> float
```

There are no classes and no handle keyword. Nearly everything you write is a `struct`.

## Mistakes the compiler stops

```fennec
target = units[target_id]
target.health -= 10
# error: `target` isn't mut
#        to change the list: units[target_id].health -= 10

mut target = units[target_id]
target.health -= 10
# error: this changes your own copy, and nothing reads it

for unit in units:
    units[0].health -= 1
# error: you can't change a list while looping over it
#        use `for ref`, or loop over indexes
```

`dict[str, int?]` is an error too: leave the key out, since not being in a dict already means missing. The same goes for optional keys and set items.

One silent mistake remains: changing a `mut` copy and then reading it, believing the list changed. A rule gets added for that only if testing shows real bugs.

## Copies

```fennec
uppercased = [copy(person, name=person.name.upper()) for person in people]
```

`copy(x, field=...)` makes a changed copy and re-runs `where` rules. A plain `=` covers everything else, and the compiler skips the real copy when nobody can tell. A plain parameter can't change, so passing something huge costs nothing.

## Fixed-size arrays

```fennec
struct Mat4:
    m: array[float, 16]                # 16 floats, held inline

struct Bag:
    slots = array[Item?, 20]()         # every slot starts as None

identity: array[float, 4] = [1.0, 0.0, 0.0, 1.0]
```

`array[T, N]` holds exactly `N` items where the array is, with no block of its own. Write one as a list of exactly `N` items where an array goes, or as `array[T, N]()`, which fills it with the item's zero: `0`, `0.0`, `False`, `""`, `None`, or a struct made from its defaults. It indexes, loops with `for` and `for ref`, and has `len`, `in` and `==`. A plain `=` copies it. Its size never changes, and it doesn't go where a list goes: `list(arr)` makes one. A wrong count, and a position known to be outside it, are compile errors.

## Pointing at things: tables and Ids

```fennec
struct Unit:
    health: int
    target_id: Id[Unit]?

mut units = Table[Unit]()
unit_id = units.add(Unit(health=50))

target = units.get(enemy.target_id) else return    # may be gone
units[unit_id].health -= 1                         # sure: stops loudly if wrong
```

Tables are for large, flat data. An `Id` is an index, a generation and a tag for its table, so it finds the right thing or nothing.

## `shared`: the class-like option

```fennec
shared Game:
    players: list[Player]

    def add(ref self, name: str):
        self.players.append(Player(name=name))

    def top_scores(self) -> list[str]:
        [player.name for player in self.players if player.score > 50]

game = Game(players=[])     # no `mut` needed
same_game = game            # a second name for the same game
game.add("Chris")           # takes the lock for the call
```

- A `shared` value can always change, through its `ref self` methods, so its names don't need `mut`. `self` methods only read.
- Writing a field needs a `ref self` method or `with game as ref current:`, so the code shows how long it holds the lock.
- A value leaving a `shared` is your own copy, and one going in is the `shared` value's own. So `names = game.names` costs a copy, and using `current.names` inside `with` doesn't.
- It can hold another `shared` type, but never in a cycle. That's a compile error. Locks are taken outer type first, so nesting can't deadlock.
- `def del` is allowed here and refused on a `struct`. It runs when the last owner goes away.
- To use two together, take both at once: `with savings as ref source, checking as ref destination:`. Taking the same value twice stops the program, like an index out of range.
- `Connection`, files, channels and `Stdout` are `shared` types that ship with Fennec. They manage their own read and write sides, and `with` on one only closes it at the end of the block.
- You can't do I/O while holding a lock, so a `shared` pool hands out a connection and the sending happens outside its lock.

A typical game doesn't use `shared`: its main loop owns the world and passes `ref world` down.

## Failures

```fennec
def sell(ref self, item: Item) -> Item fails SellError:
    count = self._items.get(item) else fail .NotFound(item)
    if count == 0:
        fail .OutOfStock(item)
    self._items[item] = count - 1
    item

value = load(path)?              # pass the failure up
count = counts.get(word) else 0  # use a default
first = items[0]                 # must be there: a bug stops the program

stream = fs.open(filename) else:
    stdout.print(f"Couldn't open file: {filename}")
    return
```

- `?` is only for failures, `else` ends a chain and can leave, and ignoring a returned value is an error.
- `.Case` is required when the type is known.
- `fails A | B` combines failures from different places: `?` passes any of them up as it is, and a function type can say it too, as in `(str) -> int fails ParseError | RangeError`. Built.
- An `else:` block must leave: it ends with `return`, `fail`, `continue` or `break`. Built.

## Capabilities

```fennec
def main(stdout: Stdout, fs: Fs):
    stdout.print("hello")
```

A function reaches outside itself only through what's in its signature, so there's no global `print`. The error message has to teach this, since it's the first thing newcomers hit.

## A whole program

```fennec
struct LineProtocol:
    connection: Connection

    def messages(self) -> Iterator[Message] fails NetError:
        for line in self.connection.lines()?:
            message = parse_message(line) else continue
            yield message

def main(net: Net, stdout: Stdout):
    with net.listen(9000)? as server:
        for connection in parallel(server):
            protocol = LineProtocol(connection=connection)
            for message in protocol.messages()?:
                stdout.print(message.text)
```

Each connection gets its own task, and a failure ends only that task. The wrapper is a plain value, and connections close when their last owner goes away.

## Speed

- Everyday builds stay fast, and release builds can take as long as they need.
- `@hot` turns a hidden copy into a compile error, and the tracer shows where each copy happens and why.

## Still to decide

| Question | Current pick |
|---|---|
| Failing exit code | A clean way to print a friendly message and still exit with failure |
| Float order | Keep one order, and measure x86 before calling it settled |
| `x / 0.0` | Keep stopping the program |
| `where` | A broken rule always stops the program |
| Private fields | Allow them in a constructor inside the same kit; other kits use named constructors |
| A shared method calling another | Refuse calls into a `shared` passed as a parameter |
| `?` inside `parallel` | It ends only that task |
| Generators | One use only: a second loop is a compile error when the compiler can see it, and otherwise stops the program |
| A scoped `ref` | Possibly `with units[target_id] as ref target:` |
| Closing written files | Automatic close can't report failure, so writers get an explicit `close()?` |
