---
layout: ../layouts/Doc.astro
title: Standard library
description: Fennec's batteries-included standard library, from files and JSON to HTTP, SQL and game math.
---

# Standard library

<p class="lede">Fennec comes with batteries included, like Python. Where Python has a module for something, Fennec's usually has the same name.</p>

> **Planned.** The standard library grows alongside the compiler. See the [roadmap](/roadmap/) for what comes when.

## What's in it

| Area | Modules |
| --- | --- |
| Basics | `math`, `random`, `time`, `datetime`, `string`, `re`, `unicode` |
| Collections | `collections`: `Deque`, `Counter`, `SortedDict`, heaps, `IdMap`, `Tree`, ring buffers |
| Files and the OS | `fs`, `path`, `env`, `process`, `cli` |
| Data formats | `json`, `toml`, `csv`, `base64`, `compress` |
| Networking | `net`: TCP, UDP, TLS, an HTTP client and server, and WebSocket; `url` |
| Security | `crypto`: hashes, HMAC and secure random numbers; `uuid` |
| Storage | `sql`: one interface, with drivers as first-party kits |
| Diagnostics | `log`, `profile`, `test` |
| Games | `Vec2`, `Vec3`, `Vec4`, `Quat`, `Mat4`, `simd`, `layout`, `arena`; later `audio` and `gpu` |

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

text = json.dumps(settings)
loaded = json.loads[Settings](text)?
```

The same works for TOML and CSV.

## Built the same way as your code

- **I/O goes through capabilities.** `http.get(net, url)` takes a `Net`, so even the standard library can't reach the outside world unexpectedly, and every module works with test fakes.
- **Errors are typed,** with the same `error`, `fail` and `?` as everywhere else.
- **You only pay for what you use.** The library is prebuilt and cached, and only what you import ends up in your program.

## Stable and next

The stable core is reliable to build on. New modules start in `std.next`, where they can still change, and graduate once they've settled. That way a module never gets stuck with an early mistake.

The standard library is written in Fennec wherever possible, and only its OS layer calls C. Windows, input, image formats, machine learning and database drivers come as first-party kits rather than `std`.
