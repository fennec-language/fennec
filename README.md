<p align="center">
  <img src="site/public/brand/fennec-logo-512.png" alt="Fennec logo" width="160" />
</p>

<h1 align="center">Fennec</h1>

<p align="center"><strong>Fast to write. Fast to build. Fast to run.</strong></p>

<p align="center">
  <a href="https://github.com/fennec-language/fennec/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/fennec-language/fennec/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/fennec-language/fennec/actions/workflows/codeql.yml"><img alt="CodeQL" src="https://github.com/fennec-language/fennec/actions/workflows/codeql.yml/badge.svg"></a>
  <a href="https://scorecard.dev/viewer/?uri=github.com/fennec-language/fennec"><img alt="OpenSSF Scorecard" src="https://api.scorecard.dev/projects/github.com/fennec-language/fennec/badge"></a>
</p>

Fennec is a safe, compiled language for games and tools, designed so that the easy way to write something is also the readable way and the fast way, for people and for the AI they work with.

> **Status: early.** `fennec run` type-checks a program and compiles it to machine code through LLVM, and `fennec build` makes a native program you can ship. Most of the language on the website is built, and the syntax will still change.

**Read more at [fennec-lang.dev](https://fennec-lang.dev/):**

- [Why Fennec](https://fennec-lang.dev/why/): what it's for and the principles behind it
- [A tour](https://fennec-lang.dev/tour/): the syntax and semantics, and what's still planned
- [Built for AI to write](https://fennec-lang.dev/ai/)
- [Two speeds](https://fennec-lang.dev/speed/): instant development builds, full-speed release builds
- [Roadmap](https://fennec-lang.dev/roadmap/)

## A taste

```python
from std.collections import Id, Table

struct Enemy:
    name: str
    health = 100 where health >= 0

struct World:
    enemies = Table[Enemy]()

error TargetGone:
    target: Id[Enemy]

## Hits an enemy for 10, or fails if it's gone.
export def attack(ref world: World, target: Id[Enemy]) fails TargetGone:
    enemy = world.enemies.get(target) else fail TargetGone(target)
    world.enemies[target] = copy(enemy, health=max(enemy.health - 10, 0))

def main(stdout: Stdout):
    mut world = World()
    fox = world.enemies.add(Enemy(name="fox"))
    _ = world.enemies.add(Enemy(name="owl", health=5))
    attack(ref world, fox)?

    alive = [enemy for enemy in world.enemies if enemy.health > 0]
    stdout.print(sum(enemy.health for enemy in alive))
```

## Try it

Once a release is out, this installs the newest one for macOS or Linux into `~/.fennec`, checked against the release's checksums. `FENNEC_VERSION=v0.1.0` in front of `sh` picks a version.

```bash
curl -fsSL https://fennec-lang.dev/install.sh | sh
```

Until then, install it from a checkout of this repository. This builds `fennec` for release with every runtime library it links programs with, from the same source, and LLVM's tools, into `~/.fennec/bin`. Run it again after pulling:

```bash
.github/scripts/install.sh
```

Or run it from the checkout with Cargo. Fennec runs most of the language on the website: structs, enums, unions, `match`, errors with `fail` and `?`, optional values, tables and Ids, comprehensions, generators, `parallel` loops, tests and property tests, and a standard library with JSON, HTTP and more. `fennec run` and `fennec test` type-check a program first and report every mistake they find. `fennec run` builds a program through LLVM a function at a time, and where LLVM can't build something yet, it says why.

```bash
cargo run -q -p fennec -- run examples/game.fen
```

```bash
cargo run -q -p fennec -- test examples/testing.fen
```

```bash
cargo run -q -p fennec -- build --release examples/game.fen -o game
```

A program can span several files as a kit: a folder with a `kit.toml` and its code under `src`. `fennec new my_game` starts one, `fennec init` sets up the current folder and adds Fennec's guidance for coding agents to its `AGENTS.md`, and `fennec run` and `fennec test` with no file run the kit around the current folder.

```bash
cargo run -q -p fennec -- run examples/projects/arena
```

Every program in [`examples/`](examples) is a test: it must print exactly its `.out` file and pass its own `test` blocks, and every program in [`examples/errors/`](examples/errors) must fail with exactly its `.err` message. The kits in [`examples/projects/`](examples/projects) work the same way.

## Repository layout

- `crates/syntax`: the lexer (with Python-style indentation), parser and syntax tree.
- `crates/check`: the type checker, which also infers every function's signature.
- `crates/ir`: the typed intermediate representation the backends compile.
- `crates/llvm`: the LLVM backend, behind `fennec run` and `fennec build`.
- `crates/rt`: the runtime's code, which `crates/runtime` builds into a library.
- `crates/runtime` and `crates/layout`: the runtime library compiled programs link against, and how values are laid out in memory.
- `crates/build`: content hashes, so a run rebuilds and retests only what changed.
- `crates/kit`: kits: `kit.toml`, and loading a kit's files as one program.
- `crates/fmt`, `crates/lint` and `crates/lsp`: the formatter, the linter and the language server.
- `crates/fennec`: the `fennec` command.
- `crates/bench` and `crates/count`: the benchmark suite, and counting the instructions a program runs.
- `crates/blake3`, `crates/re` and `crates/parked`: hashing, regular expressions and parked network tasks for the standard library.
- `crates/reference` and `crates/fuzz`: the language reference, written from the examples, and fuzzing.
- `examples/`: example programs, which double as the test suite.
- `site/`: the website, built with [Astro](https://astro.build) and deployed to Cloudflare on every push to `main`.

To work on the site, from the repository root (Node's version is pinned in `.node-version` for fnm; `dev` and `build` also write the language page from the examples, with cargo):

```bash
npm --prefix site install
```

```bash
npm --prefix site run dev
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Report security problems privately, as described in [SECURITY.md](SECURITY.md).

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.

The runtime library and the standard library also carry a [runtime library exception](RUNTIME-LIBRARY-EXCEPTION): programs compiled with Fennec don't need to credit Fennec for the parts of it built into them.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
