<p align="center">
  <img src="site/public/brand/fennec-logo-512.png" alt="Fennec logo" width="160" />
</p>

<h1 align="center">Fennec</h1>

<p align="center"><strong>Fast to write. Fast to build. Fast to run.</strong></p>

Fennec is a safe, compiled language for games and tools, designed so that the easy way to write something is also the readable way and the fast way, for people and for the AI they work with.

> **Status: early design.** A first interpreter runs simple programs; most of the language described on the website isn't implemented yet, and the syntax will change.

**Read more at [seashell86.github.io/fennec](https://seashell86.github.io/fennec/):**

- [Why Fennec](https://seashell86.github.io/fennec/why/): what it's for and the principles behind it
- [A tour](https://seashell86.github.io/fennec/tour/): the proposed syntax and semantics
- [Built for AI to write](https://seashell86.github.io/fennec/ai/)
- [Two speeds](https://seashell86.github.io/fennec/speed/): instant development builds, full-speed release builds
- [Roadmap](https://seashell86.github.io/fennec/roadmap/)

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

The first slice of the compiler runs real programs: functions, `if`/`elif`/`else`, loops, lists, comprehensions, f-strings, `mut`, and Fennec's error messages. It's a simple interpreter for now; the typed bytecode interpreter and native compilers come next.

```bash
cargo run -q -p fennec -- run examples/fizzbuzz.fen
```

Every program in [`examples/`](examples) is a test: it must print exactly its `.out` file, and every program in [`examples/errors/`](examples/errors) must fail with exactly its `.err` file.

## Repository layout

- `crates/syntax`: the lexer (with Python-style indentation), parser and syntax tree.
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

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
