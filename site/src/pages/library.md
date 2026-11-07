---
layout: ../layouts/Doc.astro
title: Standard library
description: Fennec's batteries-included standard library, from files and JSON to HTTP, SQL and game math.
---

# Standard library

<p class="lede">Fennec comes with batteries included, like Python. Where Python has a module for something, Fennec's usually has the same name.</p>

> **Partly built.** `math`, `game`, `layout`, `path`, `json`, `toml`, `pack`, `csv`, `base64`, `collections`, `random`, `time`, `re`, `log`, `cli`, `net`, `process`, `crypto` and `uuid` work today, along with `Fs`, `Clock`, `Args`, `Stdin`, `Env`, `Entropy` and `Net`. Everything else below is planned, and the built modules don't have everything listed yet. See the [roadmap](/roadmap/).

## What's in it

| Area | Modules |
| --- | --- |
| Basics | `math`, `random`, `time`, `datetime`, `string`, `re`, `unicode` |
| Collections | `collections`: `Deque`, `Counter`, `Heap`, `PriorityQueue`, `Tree`, `SortedMap`, `SortedSet`, `Grid`, `Packed`, `Table` and its `Id`s; later `Graph` |
| Files and the OS | `fs`, `path`, `env`, `process`, `cli` |
| Data formats | `json`, `toml`, `pack`, `csv`, `base64`, `compress` |
| Networking | `net`: TCP, UDP, TLS, and an HTTP client and server today; WebSocket and `url` planned |
| Security | `crypto`: BLAKE3 and SHA-256 hashes, HMAC and comparing secrets; `uuid`. Secure random bytes come from the `Entropy` capability |
| Storage | `sql`: one interface, with drivers as first-party kits |
| Diagnostics | `log`, `profile`, `test` |
| Games | `Vec2`, `Vec3`, `Vec4`, `Quat`, `Mat4`, `simd`, `layout`; later `audio` and `gpu` |

## Importing what you use

A module's names exist only once it's imported, as in Python, so a file's imports show everything it uses:

```fennec
from std.math import Vec3, sqrt   # Vec3 and sqrt by name
from std import json             # the whole module, as json.loads(...)
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

This works for JSON, TOML and `std.pack` today. A format is a type with a `Serializer`'s methods, so one `serialize` serves every format. A `Table` is saved as a list of its items in order, with each `Id` as where its item is in that list, and loading makes a new `Table` whose `Id`s find the same items. CSV reads and writes rows of text with `csv.loads` and `csv.dumps`, and reading it into structs is planned.

`std.pack` is the default binary format, written in plain Fennec: no field names on the wire, and a number for the layout's shape, so loading into a type of another shape fails at once. **Planned:** `where` ranges that shrink values, delta encoding and opt-in quantization.

## Built the same way as your code

- **I/O goes through capabilities.** `fs.read_text(path)` needs an `Fs`, so even the standard library can't reach the outside world unexpectedly. `Net` works the same way, and tests get fakes of each.
- **Errors are typed,** with the same `error`, `fail` and `?` as everywhere else.
- **You only pay for what you use.** Only what your program reaches gets compiled.

## Stable and next

**Planned:** new modules start in `std.next`, where they can still change, and graduate once they've settled. That way a module never gets stuck with an early mistake.

The standard library is written in Fennec wherever possible, and only its OS layer calls C. Windows, input, image formats, machine learning and database drivers come as first-party kits rather than `std`.
