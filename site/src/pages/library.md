---
layout: ../layouts/Doc.astro
title: Standard library
description: Fennec's batteries-included standard library, from files and JSON to HTTP, SQL and game math.
---

# Standard library

<p class="lede">Fennec comes with batteries included, like Python. Where Python has a module for something, Fennec's usually has the same name.</p>

> **Partly built.** `math`, `game`, `layout`, `path`, `json`, `toml`, `collections`, `random`, `time` and `re` work today, along with `Fs`, `Clock`, `Args` and `Stdin`. Everything else below is planned, and the built modules don't have everything listed yet. See the [roadmap](/roadmap/).

## What's in it

| Area | Modules |
| --- | --- |
| Basics | `math`, `random`, `time`, `datetime`, `string`, `re`, `unicode` |
| Collections | `collections`: `Deque`, `Counter`, `Heap`, `PriorityQueue`, `Tree`, `SortedMap`, `SortedSet`, `Grid`, `Packed` |
| Files and the OS | `fs`, `path`, `env`, `process`, `cli` |
| Data formats | `json`, `toml`, `pack`, `csv`, `base64`, `compress` |
| Networking | `net`: TCP, UDP, TLS, an HTTP client and server, and WebSocket; `url` |
| Security | `crypto`: hashes and HMAC; `uuid`. Secure random bytes come from the `Entropy` capability |
| Storage | `sql`: one interface, with drivers as first-party kits |
| Diagnostics | `log`, `profile`, `test` |
| Games | `Vec2`, `Vec3`, `Vec4`, `Quat`, `Mat4`, `IdMap`, `simd`, `layout`; later `audio` and `gpu` |

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

This works for JSON and TOML today. CSV reads and writes rows of text with `csv.loads` and `csv.dumps`, and reading it into structs is planned.

**Planned:** one `Writer` and `Reader` interface that every format implements, so one generic function breaks any struct down, and `std.pack`, the default binary format: no field names on the wire, `where` ranges that shrink values, a schema hash, delta encoding and opt-in quantization, written in plain Fennec.

## Built the same way as your code

- **I/O goes through capabilities.** `fs.read_text(path)` needs an `Fs`, so even the standard library can't reach the outside world unexpectedly. `Net` and test fakes are planned.
- **Errors are typed,** with the same `error`, `fail` and `?` as everywhere else.
- **You only pay for what you use.** Only what your program reaches gets compiled.

## Stable and next

**Planned:** new modules start in `std.next`, where they can still change, and graduate once they've settled. That way a module never gets stuck with an early mistake.

The standard library is written in Fennec wherever possible, and only its OS layer calls C. Windows, input, image formats, machine learning and database drivers come as first-party kits rather than `std`.
