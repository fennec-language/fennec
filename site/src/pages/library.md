---
layout: ../layouts/Doc.astro
title: Standard library
description: Fennec's batteries-included standard library, from files and JSON to HTTP, SQLite and game math.
---

# Standard library

<p class="lede">Fennec comes with batteries included, like Python. Where Python has a module for something, Fennec's usually has the same name.</p>

> **Planned.** The standard library grows alongside the compiler. See the [roadmap](/fennec/roadmap/) for what comes when.

## What's in it

| Area | Modules |
| --- | --- |
| Basics | `math`, `random`, `time`, `datetime`, `string`, `re`, `unicode` |
| Collections | `collections`: `Deque`, `Counter`, `SortedDict`, heaps, `IdMap`, ring buffers |
| Files and the OS | `fs`, `path`, `env`, `process`, `args` |
| Data formats | `json`, `toml`, `csv`, `base64`, `compress` |
| Networking | `http` (client and server), `url`, `net`, `tls` |
| Security | `hash`, `hmac`, `uuid`, secure random numbers |
| Storage | `sqlite` |
| Diagnostics | `log`, `profile`, `test` |
| Games | `Vec2`, `Vec3`, `Vec4`, `Quat`, `Mat4`, `simd`, `layout`, `arena`; later `window`, `input`, `audio` and `gpu` |

## Serialization with nothing to set up

The compiler knows every struct's fields, so reading and writing data formats needs no annotations:

```fennec
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

C-based pieces like SQLite and compression are part of the audited core, the one place besides `extern` modules where unsafe code is allowed. Packages you download stay free of unsafe code.
