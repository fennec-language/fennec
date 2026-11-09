---
layout: ../layouts/Doc.astro
title: Standard library
description: Fennec's batteries-included standard library, from files and JSON to HTTP, collections and game math.
---

# Standard library

<p class="lede">Fennec comes with batteries included, like Python. Where Python has a module for something, Fennec's usually has the same name.</p>

> **Partly built.** `math`, `layout`, `path`, `fs`, `json`, `toml`, `pack`, `csv`, `base64`, `compress`, `collections`, `random`, `time`, `string`, `re`, `unicode`, `log`, `cli`, `net`, `url`, `process`, `crypto`, `uuid`, `concurrent`, `sql`, `sqlite` and `postgres` work today, along with the `Stdout`, `Stdin`, `Args`, `Fs`, `Clock`, `Env`, `Entropy`, `Process` and `Net` capabilities. Anything marked planned below isn't built yet. See the [roadmap](/roadmap/).

## What's in it

| Area | Modules |
| --- | --- |
| Basics | `math`, `random`, `time`, `string`, `re`, `unicode` |
| Collections | `collections`: `Table` and its `Id`s, `Tree`, `SortedMap`, `SortedSet`, `Grid`, `Packed`, `Deque`, `Heap`, `PriorityQueue`, `Counter` and `Graph` |
| Files and the OS | `fs`, with files opened by `with`, `path`, `process`, `cli`, and environment variables through `Env` |
| Data formats | `json`, `toml`, `pack`, `csv`, `base64`, `compress` |
| Networking | `net`: TCP, UDP, an HTTP client with TLS, and an HTTP server. `url`. Planned: WebSocket |
| Security | `crypto`: BLAKE3 and SHA-256 hashes, HMAC, comparing secrets, and `Secret`, which can't be printed, logged or saved; `uuid`; `Template`, which every t-string makes. Secure random bytes come from the `Entropy` capability |
| Diagnostics | `log` |
| Games | `math`: `Vec2`, `Vec3`, `Vec4`, `Quat`, `Mat3` and `Mat4`, checked bit for bit against a widely used native math library. `layout` for lists stored a field at a time. Planned: `Simd`, and `audio` and `gpu` as kits |
| Tasks | `concurrent`: `Channel`, its ends `Inbox` and `Sender`, and the errors `Closed`, `Timeout` and `Crashed`, for `with launch`. Planned: `TaskGroup` and pools |
| Storage | `sql`, one interface for databases. `sqlite`, SQLite compiled in, which takes only t-strings, so SQL injection can't be written. `postgres`, written in Fennec over `net`: sign-in, queries and transactions, over TLS. Planned: connecting from a URL, as a pool |

## Importing what you use

A module's names exist only once it's imported, as in Python, so a file's imports show everything it uses:

```fennec
# Vec3 and sqrt by name
from std.math import Vec3, sqrt
# the whole module, as json.loads(...)
from std import json
```

Using a name without its import is an error that says which import to add. Capabilities like `Stdout`, `Fs` and `Clock` need no import, and nor do the errors standard operations fail with, like `IoError`.

## Serialization with nothing to set up

The compiler knows every struct's fields, so reading and writing data formats needs no annotations:

```fennec
from std import json

struct Settings:
    volume = 0.8
    fullscreen = False

def main(stdout: Stdout):
    text = json.dumps(Settings())
    loaded = json.loads[Settings](text) else Settings()
    stdout.print(loaded)
```

This works for JSON, TOML and `std.pack`. One `serialize` and `deserialize` serve every format. A `Table` is saved as a list of its items in order, with each `Id` as where its item is in that list, and loading makes a new `Table` whose `Id`s find the same items. CSV reads and writes rows of text with `csv.loads` and `csv.dumps`, and reading it into structs is planned.

`std.pack` is the default binary format, written in plain Fennec. Values are written by position, with their type's layout written once beside them, so data from an earlier version of a type loads by its names, and a new field takes its default. `pack.Packer` writes many values with each layout once, and `pack.Unpacker` reads them from a file or a stream. **Planned:** `where` ranges that shrink values, delta encoding and opt-in quantization.

## A web server is a function

`std.net`'s HTTP server answers each request with a plain function, and gives each connection its own task:

```fennec
from std.net import Request, Response

def respond(request: Request) -> Response:
    match request.path:
        case "/hello":
            name = request.query.get("name") else "you"
            Response(status=200, body=f"hello, {name}".encode())
        case _:
            Response(status=404, body="nothing here".encode())

def main(net: Net):
    with net.listen(8080)? as server:
        server.serve(respond)
```

Every call that waits takes `timeout=`, and connecting and HTTP requests stop after 30 seconds unless told otherwise.

## Built the same way as your code

- **I/O goes through capabilities.** `fs.read_text(path)` needs an `Fs`, so even the standard library can't reach the outside world unexpectedly. `Net` works the same way, and tests get fakes of each.
- **Failures are typed,** with the same `error`, `fail` and `?` as everywhere else.
- **You only pay for what you use.** Only what your program reaches gets compiled.
- **No special powers.** The standard library uses only what any program can, and it's written in Fennec wherever it can be.

## Stable and next

**Planned:** new modules start in `std.next`, where they can still change, and graduate once they've settled. That way a module never gets stuck with an early mistake.

Windows, input, audio, image formats, machine learning and database drivers come as first-party kits rather than `std`. Where one needs a C library, the Fennec project wraps it behind a safe API.
