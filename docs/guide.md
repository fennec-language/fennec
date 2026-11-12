# Building a program in Fennec

A practical guide to writing a real program today, using a small version of Hoard, a source control tool, as the example. Every sample here compiles and passes its tests on main. Anything that doesn't is marked **planned**, with what to write until it lands. `docs/language.md` is the full language on one page.

## Start a kit

```bash
fennec new hoard
```

That makes a folder:

```text
hoard/
  kit.toml
  AGENTS.md
  src/
    main.fen
    greeting.fen
```

`kit.toml` names the kit and the Fennec it's built with:

```toml
name = "hoard"
version = "0.1.0"
fennec = "0.0.1"

[dependencies]
```

- `src/main.fen` holds `main`. Every other file under `src` is a module: `src/store.fen` is `store`, and `src/net/sync.fen` is `net.sync`.
- A file uses another's names only after importing them: `from store import save`, or `import store` and then `store.save(...)`.
- Two files of a kit can't define the same top-level name, so a name means one thing everywhere in the kit.
- Without `export`, a name is private to its kit. Mark what other kits may use: `export def save(...)`.

### Dependencies

`[dependencies]` names other kits, from git pinned to a commit's whole hash, or from a folder on this machine:

```toml
[dependencies]
physics = { git = "https://git.example.com/studio/physics", commit = "3f9c2a1e7b4d3f9c2a1e7b4d3f9c2a1e7b4d3f9c" }
postgres = { git = "https://github.com/fennec-language/fennec", commit = "...", folder = "kits/postgres" }
tools = { path = "../tools" }
```

- `fennec update` fetches each kit from git that isn't fetched yet and pins it in `kit.lock` by the hash of its files. It says what each moved pin changes, file by file. Nothing else changes `kit.lock`, and a `kit.toml` that names another commit than `kit.lock` pins is an error until you run it.
- Fetching runs nothing: a kit has no install or build scripts, nothing is checked out, and only `kit.toml` and `src` are taken. A kit whose files come to more than 256 MiB, or with two names that differ only by case, is refused before any of it is read.
- Only `fennec update` and `fennec fetch` reach the network. `fennec fetch` gets each kit `kit.lock` pins that this machine hasn't got, as on a fresh clone. Checking, running, testing and the language server fetch nothing, and say to run `fennec fetch` when a kit is missing. `--offline` refuses to fetch at all.
- A fetched kit is kept in `~/.fennec/kits/`, and checked against its pin each time it's loaded: one whose files changed since, by hand or by a tool, isn't built, and `fennec fetch` gets it again whole.
- A dependency can't be named `std`, `fennec`, `main`, `lib` or `test`.
- A kit from a path isn't pinned, since it's work in progress. A kit from git can't name a path.

`import physics` reaches the kit's `src/lib.fen`, and `import physics.shapes` its `src/shapes.fen`, and `from physics import Body` works as it does for a kit's own files. A kit imports only what its own `kit.toml` names, so a dependency's dependencies stay its own.

Each kit keeps its own names: a program's `Circle` and a dependency's `Circle` are different types, and neither gets in the other's way. Another kit reaches only what's marked `export`. A dependency's tests and lints are its own, so `fennec test` and `fennec lint` leave them out, and a change to `kit.lock`, or to a kit from a path, is seen by the next run.

## `main` and capabilities

A function reaches outside itself only through what's in its signature. So `main` names everything the program may do, and hands each piece to the code that needs it:

```fennec
def main(stdout: Stdout, fs: Fs, args: Args):
    folder = args[0] if len(args) > 0 else "."
    stdout.print(f"saving {folder}")
```

There's no global `print`, and no way to reach the disk or the network without being handed `Fs` or `Net`. The others are `Clock`, `Entropy`, `Stdin`, `Env` and `Process`. Tests get fakes of each, below.

When a function deep in a program starts needing one, write it by its usual name, like `fs`, and run `fennec fix`. It gives that function an `fs: Fs` parameter, passes `fs` at each call, and does the same for each caller, up to the first one that already has an `Fs`, `main`, or a test. It changes nothing when a function in the chain is a method or is passed around as a value, and leaves the error for you.

`main` ends the program. Failing out of it prints the failure to stderr and exits with code 1, and `def main(...) -> int:` exits with the `int` it gives back. There's no `exit()`.

## Failures and `?`

A function that can fail says so in its signature, and every place it can leave is visible. This is `src/store.fen`, where Hoard keeps file contents under the hash of their bytes:

```fennec
## Where Hoard keeps file contents, each under the hash of its bytes.
from std.crypto import blake3

error StoreError:
    Missing(hash: str)
    Unreadable(path: str)

## The hash that names `data`: the same bytes always give the same name.
def name_of(data: bytes) -> str:
    return blake3(data)

## Saves `data` in the store under its hash, and gives back the hash.
def save(fs: Fs, data: bytes) -> str fails IoError:
    hash = name_of(data)
    fs.make_dir(".hoard/objects")?
    fs.write_bytes(f".hoard/objects/{hash}", data)?
    return hash

## Reads back what `save` stored under `hash`.
def load(fs: Fs, hash: str) -> bytes fails StoreError:
    path = f".hoard/objects/{hash}"
    there = fs.exists(path) except why:
        fail .Unreadable(path)
    if not there:
        fail .Missing(hash)
    data = fs.read_bytes(path) except why:
        fail .Unreadable(path)
    return data
```

- `error StoreError:` lists the ways it can fail, each with the data a caller needs.
- `?` passes a failure up to the caller. It's only for failures, and a function using it must say `fails`.
- `fail .Missing(hash)` leaves with a failure of your own.
- `except why:` handles a failure in a block, and `case` arms right under it handle each case. A `fail` inside it carries the failure it replaces along as a note, which counts as using `why`. There's no `Ok`, `Err` or `Result`: a failure is always handled at the call.
- `else` is only for a missing value: a default, or leaving with `else return`, `else continue` or `else fail ...`.

```fennec
data = load(fs, hash) except why:
    case .Missing(hash):
        stdout.print(f"nothing is saved under {hash}")
        return
    case .Unreadable(path):
        stdout.print(f"couldn't read {path}")
        return
stdout.print(len(data), "bytes")
```

Ignoring a returned value is a compile error. Write `_ = call()` when you mean to throw one away. On a call that can fail, `_ = f()` is a compile error, so handle the failure instead.

## Files

Everything on disk goes through `fs`:

| Call | What it does |
|---|---|
| `fs.read_text(path)?`, `fs.read_bytes(path)?` | Read a whole file |
| `fs.write_text(path, text)?`, `fs.write_bytes(path, data)?` | Write a whole file |
| `fs.exists(path)?` | Whether it's there, failing when it can't look |
| `fs.metadata(path)?` | Its `size`, `is_file` and `is_dir` |
| `fs.list_dir(path)?` | What's in a folder, sorted by name |
| `fs.walk(path)?` | Everything under a folder, at any depth |
| `fs.make_dir(path)?` | Make a folder and any missing parents |
| `fs.remove(path)?` | Remove a file, or an empty folder |

Each failure is an `IoError`.

To write a file a piece at a time, or read one too big for memory, open it with `with`:

```fennec
## Points HEAD at a snapshot's hash.
def set_head(fs: Fs, hash: str) fails IoError:
    # the new HEAD takes the old one's place only when the block ends, synced to the disk
    with fs.create(".hoard/HEAD")? as file:
        file.write_line(hash)?

## Each path listed in an ignore file, one a line.
def ignored(fs: Fs) -> list[str] fails IoError:
    mut found = list[str]()
    with fs.open(".hoardignore")? as file:
        for line in file.lines()?:
            if line != "": found.append(line)
    return found

## How many bytes a file holds, read a chunk at a time.
def size_of(fs: Fs, path: str) -> int fails IoError:
    mut total = 0
    with fs.open(path)? as file:
        for chunk in file.chunks(65536)?:
            total += len(chunk)
    return total

## A log line that doesn't matter if the power goes, written without waiting for the disk.
def note(fs: Fs, text: str) fails IoError:
    with fs.create(".hoard/last.log", durable=False)? as file:
        file.write_line(text)?
        # or put what's written so far on the disk now
        file.sync()?
```

- `fs.create` writes into a file of its own beside the path, which takes the path's place only when the block ends, synced to the disk. A block that fails leaves the old file as it was, so there's never half a file.
- `durable=False` skips waiting for the disk, for a file that doesn't matter if the power goes, and `file.sync()?` puts what's written on the disk at any point.
- `fs.open` reads with `lines()` or `chunks(size)`, and `read_text()` or `read_bytes()` for what's left. A file opened to read can't be written, and one made to write can't be read.
- The `?` on the `with` line covers both opening the file and finishing it at the block's end. There's no `close()`.

## Shared state with `shared` and `lock`

Most code passes plain values around: `=` gives you your own copy, and `ref` lets a function change yours. When many tasks have to reach the same value, make it `shared` in `main`, pass it down, and touch it inside `lock`. This is `src/index.fen`:

```fennec
## Which hash each tracked path had when it was last saved, shared by every task.
struct Entry:
    path: str
    hash: str

def record(index: shared dict[str, Entry], path: str, hash: str):
    # one task at a time can change it
    lock index as ref entries:
        entries[path] = Entry(path=path, hash=hash)

def hash_of(index: shared dict[str, Entry], path: str) -> str?:
    # many tasks can read it at once
    lock index as entries:
        found = entries.get(path) else return None
        return found.hash
    return None
```

- `lock x as s:` reads, and `lock x as ref s:` changes. Any way out of the block releases it.
- A crash inside `lock x as ref` puts back what the block changed, so nobody sees half a change.
- Don't wait inside a lock, on the network or a channel. Wait first, then lock.
- Two values locked together go in one `lock`: `lock source, target as ref payer, ref payee:`. A `lock` inside another is refused for now.
- There's no global state. A `shared` value is made in a function, usually `main`, as `shared index = dict[str, Entry]()`.

## `parallel` over data

`parallel(xs)` splits work across cores, with no threads or `async` to manage. This is `src/main.fen`:

```fennec
from index import Entry, record
from store import save

## Saves every file under `folder` into the store, hashing them in parallel.
def snapshot(fs: Fs, folder: str, index: shared dict[str, Entry]) -> int fails IoError:
    paths = [path.text for path in fs.walk(folder)? if not fs.metadata(path)?.is_dir]
    # each file is read and hashed in its own task, and the results stay in order
    hashes = [save(fs, fs.read_bytes(path)?)? for path in parallel(paths)]
    for path, hash in zip(paths, hashes):
        record(index, path=path, hash=hash)
    return len(paths)

def main(stdout: Stdout, fs: Fs, args: Args):
    folder = args[0] if len(args) > 0 else "."
    shared index = dict[str, Entry]()
    saved = snapshot(fs, folder, index) else:
        stdout.print(f"couldn't read {folder}")
        return
    stdout.print(f"saved {saved} files")
```

- `[f(x) for x in parallel(xs)]` runs in parallel, and gives its results in input order.
- `for ref x in parallel(xs):` changes each item where it is. A body changes only its own item.
- `sum(f(x) for x in parallel(xs))` adds up in a fixed order, so a float total is the same on every run.
- A failure in one round passes up out of the loop once the other rounds stop, as `?` does anywhere.

## Servers

An HTTP server is a function that answers each request, and `server.serve` runs it, with each connection in its own task:

```fennec
from std.net import Request, Response

## Answers `GET /objects/<hash>` with what the store holds under that hash.
def respond(fs: Fs, request: Request) -> Response:
    if request.method != "GET" or not request.path.startswith("/objects/"):
        return Response(status=404, body="nothing here".encode())
    hash = request.path[9:]
    data = fs.read_bytes(f".hoard/objects/{hash}") else:
        return Response(status=404, body=f"no object {hash}".encode())
    return Response(status=200, headers={"content-type": "application/octet-stream"}, body=data)

test "a saved object is served"(fs: Fs):
    fs.make_dir(".hoard/objects")?
    fs.write_bytes(".hoard/objects/abc", "hi".encode())?
    answer = respond(fs, Request(method="GET", path="/objects/abc"))
    assert answer.status == 200 and answer.text() == "hi"
    assert respond(fs, Request(method="GET", path="/objects/nope")).status == 404

def main(net: Net, fs: Fs, stdout: Stdout):
    with net.listen(8080)? as server:
        stdout.print(f"serving on http://localhost:{server.port}")
        # each connection gets its own task
        server.serve((request) => respond(fs, request))
```

`respond` is a plain function, so its tests call it directly.

For a protocol of your own, loop over the server's connections:

```fennec
from std.net import Connection, NetError

## Sends back each line a client sends, until it hangs up.
def echo(conn: Connection) fails NetError:
    for line in conn.lines()?:
        conn.write(f"{line}\n")?

def main(net: Net, stdout: Stdout):
    with net.listen(9000)? as server:
        # one task for each connection, which closes when its round ends
        for conn in parallel(server):
            # handle a failure here, or it would pass up and stop the server
            echo(conn) else:
                continue
```

A crash in one connection's task ends only that task, and the server keeps serving.

## A task beside you

`with launch(...)` runs a function beside the block until the block ends, so no task outlives the code that started it. A parameter typed `Inbox[T]` gives the task a channel, and the `as` name is its sending end. The `?` on the line covers the task's own failures and `Crashed`:

```fennec
from std.concurrent import Inbox, Crashed, Closed, Timeout

def total(inbox: Inbox[int], stdout: Stdout):
    stdout.print(sum(number for number in inbox))

def main(stdout: Stdout) fails Crashed | Closed | Timeout:
    with launch((inbox) => total(inbox, stdout), stop_after=1s)? as numbers:
        for number in range(5):
            numbers.send(number)?
    stdout.print("after")
```

When the block ends, the task's inbox closes, the task sums what it was sent and prints `10`, and only then does `after` print. Channels are always bounded, so `send` waits when the task falls behind. This builds natively.

## Hashing

`std.crypto` gives `blake3` and `sha256` as lowercase hex, for text or `bytes`, plus `keyed_blake3`, `hmac_sha256`, `derive_key` and `equal`, which compares secrets in constant time. The same bytes give the same hash on every machine, so a hash names its content, as `name_of` does above. A password or a key goes in a `Secret`, as `Secret.of(text)`: printing it, putting it in an f-string and saving it are compile errors, and `expose_secret()` is the one way to read it, so a search for that name finds every use. `==` on two secrets compares them in constant time, as `equal` does, so checking a password or a token is just `==`.

`blake3` and `sha256` also take a list or an iterator of text or `bytes`, and hash the chunks in turn, so a large file hashes as it's read, never in memory whole:

```fennec
from std.crypto import blake3

## A file's hash, read 64 KB at a time.
def hash_of(fs: Fs, path: str) -> str fails IoError:
    mut hash = ""
    with fs.open(path)? as file:
        hash = blake3(file.chunks(65536)?)
    return hash
```

## Templates and SQLite

A t-string, `t"..."`, makes a `Template`, as in Python 3.14. It keeps the text you wrote apart from the values, so a value can never become part of a query, a page or a command:

```fennec
type SqlValue = int | str

## The query as a database receives it: the text with a `?` for each value.
def placeholders(query: Template[SqlValue]) -> str:
    mut sent = ""
    for at, text in enumerate(query.strings):
        sent += text
        if at < len(query.values):
            sent += "?"
    return sent

## A column to sort by: the input chooses between texts written here, so it's a `LiteralStr`.
def order_column(by_size: bool) -> LiteralStr:
    return "size" if by_size else "path"

def main(stdout: Stdout):
    # a path someone else chose is a value, never part of the query's text
    path = "a.txt'; drop table entries; --"
    query: Template[SqlValue] = t"select hash from entries where path = {path}"
    stdout.print(placeholders(query))
    stdout.print(query.values)
    stdout.print(order_column(by_size=True))
```

- The values' type comes from where the template goes: here each `{}` has to be a `SqlValue`.
- A `str` is never a `Template`, so an f-string where one is wanted is a compile error.
- `LiteralStr` is text written in the source: a literal, literals joined with `+`, or a choice between literals. It goes anywhere a `str` goes, but text read while the program runs is never one, so input can only choose between texts you wrote.

`std.sqlite` takes these templates, and reads each row into a struct by its columns' names:

```fennec
from std import sqlite
from std.sql import SqlError

struct Entry:
    path: str
    hash: str

def record(fs: Fs, entries: list[Entry]) fails SqlError:
    with sqlite.open(fs, ".hoard/index.db")? as db:
        _ = db.execute(t"create table if not exists entries (path text primary key, hash text not null)")?
        # kept when the block ends, and none of it if the block fails
        with db.transaction()? as tx:
            for entry in entries:
                _ = tx.execute(t"insert or replace into entries values ({entry.path}, {entry.hash})")?

def hash_of(fs: Fs, path: str) -> str? fails SqlError:
    mut found = list[Entry]()
    with sqlite.open(fs, ".hoard/index.db")? as db:
        found = db.query[Entry](t"select path, hash from entries where path = {path}")?
    return found[0].hash if len(found) > 0 else None
```

- Each `{}` is bound as a parameter, never pasted into the query's text.
- A column that doesn't fit its field, or a field with no column, fails with `SqlError.Column`, naming it. `query[int]` or `query[str]` reads a single column.
- The database is in WAL mode, so reads go on while something writes. Tasks in a `parallel` loop can share one: each transaction gets a connection of its own and the database's one writer, and a task waiting for it pauses without holding a thread.
- In a test, each path is a database of the test's own, fresh for each test, in a temporary folder. It behaves as a file does, so what passes in a test passes for real. `fennec test --real` uses the paths themselves.

## Tests

A test sits beside the code it tests, and names the capabilities it needs, as `main` does. It gets fakes: an in-memory `Fs`, a fixed `Clock`, captured `Stdout` and a `Net` that works in memory. Each test starts with nothing.

```fennec
test "what's saved loads back"(fs: Fs):
    hash = save(fs, "hello".encode())?
    assert load(fs, hash)? == "hello".encode()

test "a hash nothing was saved under is missing"(fs: Fs):
    _ = load(fs, "nothing") except why:
        case .Missing(hash):
            assert hash == "nothing"
        case _:
            assert False
        return
    assert False
```

A property test takes its inputs after `for`. Fennec tries many random ones from a fixed seed, so a run repeats exactly, and shrinks a failing input to a small one:

```fennec
test "a hash names its content" for text: str:
    assert name_of(text.encode()) == name_of(text.encode())
```

`fennec test --real` gives tests the real disk and network instead.

## Commands

| Command | What it does |
|---|---|
| `fennec run` | Runs the tests a change affects, then the kit's `main`. Arguments after the path go to `main`: `fennec run . src` |
| `fennec run --no-test` | Runs it without the tests first |
| `fennec test` | Runs the kit's tests, skipping unchanged ones that passed. `--all` runs every one |
| `fennec build` | Makes a native program. `--release` optimizes the whole program as one, and `-o hoard` names it |
| `fennec check` | Type-checks without running anything |
| `fennec lint` | What the checker and the linter find. `--json` for tools |
| `fennec fix` | Makes the fixes that can't change what the program does |
| `fennec fmt` | Formats the kit in the one style |

Builds go through LLVM a function at a time and are cached, so an edit rebuilds only what changed. Where LLVM can't build something yet, `fennec run` and `fennec build` say why.

A build is the same bytes every time, wherever it's built:
- **A `--release` build** is the same bytes in any folder, on any machine, at any time, as a release you hand out has to be for anyone to check it by building it again. The files it shows a failure in are named from the kit, as `src/main.fen`, never from the folders you keep it in.
- **A dev build** is the same bytes built again in the same folder, at any time. It shows a failure at the path you gave, whole path and all, which is what you, or an agent reading the failure, want on your own machine.

## Not there yet

These are decided and planned, and `docs/language.md` says where each one stands:
- pools;
- `u64` and `f32`, and `wrapping_` and `saturating_` methods: `i8` to `i32` and `u8` to `u32` already run natively;
