<p align="center">
  <img src="site/public/brand/fennec-logo-512.png" alt="Fennec logo" width="160" />
</p>

<h1 align="center">Fennec</h1>

<p align="center"><strong>Fast to write. Fast to build. Fast to run.</strong></p>

<p align="center">
  <a href="https://github.com/seashell86/fennec/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/seashell86/fennec/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/seashell86/fennec/actions/workflows/codeql.yml"><img alt="CodeQL" src="https://github.com/seashell86/fennec/actions/workflows/codeql.yml/badge.svg"></a>
  <a href="https://scorecard.dev/viewer/?uri=github.com/seashell86/fennec"><img alt="OpenSSF Scorecard" src="https://api.scorecard.dev/projects/github.com/seashell86/fennec/badge"></a>
</p>

Fennec is a safe, compiled language for games and tools, designed so that the easy way to write something is also the readable way and the fast way, for people and for the AI they work with.

> **Status: early design.** The interpreter runs most of the language described on the website, and a first type checker catches most mistakes before a program runs. Native compilers and concurrency come next, and the syntax will still change.

**Read more at [fennec-lang.dev](https://fennec-lang.dev/):**

- [Why Fennec](https://fennec-lang.dev/why/): what it's for and the principles behind it
- [A tour](https://fennec-lang.dev/tour/): the proposed syntax and semantics
- [Built for AI to write](https://fennec-lang.dev/ai/)
- [Two speeds](https://fennec-lang.dev/speed/): instant development builds, full-speed release builds
- [Roadmap](https://fennec-lang.dev/roadmap/)

## A taste

```python
error TargetGone:
    target: EnemyId

export def attack(mut world: World, target: EnemyId):
    enemy = world.enemies.get(target)
    if enemy is None:
        fail TargetGone(target)
    enemy_hit = replace(enemy, health=enemy.health - 10)
    world.enemies.set(target, enemy_hit)

alive = [enemy for enemy in world.enemies if enemy.health > 0]
total_health = sum(enemy.health for enemy in alive)
```

## Try it

The interpreter runs most of the language on the website: structs, enums, `match`, errors with `fail` and `?`, optional values, comprehensions, generators, `with`, operators, tests and property tests. `fennec run` and `fennec test` type-check a program first and report every mistake they find; the interpreter still checks what the type checker can't yet.

```bash
cargo run -q -p fennec -- run examples/game.fen
```

```bash
cargo run -q -p fennec -- test examples/testing.fen
```

```bash
cargo run -q -p fennec -- repl
```

Every program in [`examples/`](examples) is a test: it must print exactly its `.out` file and pass its own `test` blocks, and every program in [`examples/errors/`](examples/errors) must fail with exactly its `.err` message.

## Repository layout

- `crates/syntax`: the lexer (with Python-style indentation), parser and syntax tree.
- `crates/check`: the type checker, which also infers every function's signature.
- `crates/interp`: the interpreter.
- `crates/fennec`: the `fennec` command.
- `examples/`: example programs, which double as the test suite.
- `site/`: the website, built with [Astro](https://astro.build) and deployed to GitHub Pages on every push to `main`.

The compiler has no dependencies beyond Rust's standard library, so it builds in seconds. To work on the site, from the repository root (Node's version is pinned in `.node-version` for fnm):

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
