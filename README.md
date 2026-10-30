<p align="center">
  <img src="site/public/brand/fennec-logo-512.png" alt="Fennec logo" width="160" />
</p>

<h1 align="center">Fennec</h1>

<p align="center"><strong>Fast to write. Fast to build. Fast to run.</strong></p>

Fennec is a safe, compiled language for games and tools, designed so that the easy way to write something is also the readable way and the fast way, for people and for the AI they work with.

> **Status: early design.** There is no compiler yet. The website describes the language as it's currently designed, and the syntax will change.

**Read more at [seashell86.github.io/fennec](https://seashell86.github.io/fennec/):**

- [Why Fennec](https://seashell86.github.io/fennec/why/): what it's for and the principles behind it
- [A tour](https://seashell86.github.io/fennec/tour/): the proposed syntax and semantics
- [Built for AI to write](https://seashell86.github.io/fennec/ai/)
- [Two speeds](https://seashell86.github.io/fennec/speed/): instant development builds, full-speed release builds
- [Roadmap](https://seashell86.github.io/fennec/roadmap/)

## A taste

```python
export def attack(mut world: World, target: Id[Enemy]):
    const enemy = world.enemies.get(target)
    if enemy is None:
        return Err(.TargetGone(target))
    const distance = (enemy.position - world.player).length()
    if distance > 5.0:
        return Err(.OutOfRange(distance))
    world.enemies.modify(target, enemy => enemy.take_damage(10))
    Ok(None)

const alive = [enemy for enemy in world.enemies if enemy.is_alive()]
const total_health = sum(enemy.health for enemy in alive)
```

## Repository layout

- `site/`: the website, built with [Astro](https://astro.build) and deployed to GitHub Pages on every push to `main`.

To work on the site, from the repository root (Node's version is pinned in `.node-version` for fnm):

```bash
npm --prefix site install
```

```bash
npm --prefix site run dev
```

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
