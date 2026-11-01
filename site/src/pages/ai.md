---
layout: ../layouts/Doc.astro
title: Built for AI to write
description: How Fennec is designed so AI models write correct code on the first try, and catch their own mistakes when they don't.
---

# Built for AI to write

<p class="lede">A growing share of new code is written with AI help. Fennec is designed so a model's first attempt is likely to be right, and so its mistakes are caught by the compiler instead of by you.</p>

> **Some of this is planned.** Much of it works today. Not built yet: `ignore`, `expect` and `todo()`, a required finish on types like `Transaction`, the check for weakened tests, JSON diagnostics, the task suite and the agent tools at the end of this page.

## A shape models already know

Fennec reads like typed Python: indentation, `def`, comprehensions, f-strings, `and`, `or` and `not`, `True`, `False` and `None`. Python is the language models have written more of than any other, so they write Fennec fluently from a short guide rather than a learning curve.

What Fennec drops is the part of Python that causes bugs at scale: types that are only checked at runtime, exceptions, truthiness, shared mutable objects, classes and inheritance, and `None` values you can forget to check.

## One way to do each thing

Models do best when there's one obvious answer. Comprehensions build collections, and builtins like `sum` and `max` consume them. `mut` is the only word for mutability, and errors always go through `fail`, `?`, `else` and `match`. There's one string syntax, one way to declare a function, one formatter with no settings, and one tool that builds, tests and lints. There are no macros and no decorators to learn per project.

## Habits from other languages are caught, not guessed at

When a model reaches for something Fennec doesn't have, the compiler recognizes it and says exactly what to write instead.

| What a model writes | What Fennec says |
| --- | --- |
| `count = 0` then `count += 1` | `count` isn't mut. Declare it `mut count = 0`. |
| `def area(shape):` | Parameters need types: `def area(shape: Shape):`. |
| `if items:` | There's no truthiness. Write `if items.len() > 0:`. |
| `raise NotFound(path)` | There are no exceptions. Write `fail NotFound(path)`, which returns the error. |
| `try: ... except NotFound:` | Pass errors up with `?`, give a default with `else`, or handle them with `match`. |
| `lambda x: x * 2` | Write `x => x * 2`. |
| `print(value)` or `console.log(value)` | There's no global output. Write through the `Stdout` that `main` receives; to inspect a value, use a test or the debugger. |
| `class Boss(Enemy):` | There are no classes or inheritance. Use a `struct`, and an `enum` for different kinds of enemy. |
| `def __add__(self, other):` | Write `def +(self, other):`. |
| `if ready: label = "a"` / `else: label = "b"` | Set it once with an expression: `label = "a" if ready else "b"`. Names don't leak out of blocks. |

## Guardrails for the mistakes models actually make

| The mistake | What stops it |
| --- | --- |
| Forgetting a value might be missing | An `Enemy?` can't be used until it's checked for `None`. |
| Tangled shared state | Data is plain values in tables. The one reference type, `shared`, is planned, rare, and locks on every method. |
| Expecting a copy to change the original | `mut x = place` edits the real thing. Planned: a copy that can go stale is a compile error. |
| Changing a list while looping over it | A compile error, with `remove_where` suggested instead. |
| Ignoring an error | A call that can fail needs `?`, `else` or `match`. Discarding an error on purpose needs `ignore(result, "why")`. |
| Assuming something can't fail | Asserting success needs `expect("reason")`, and an empty reason is rejected. |
| Leaving a stub behind | `todo()` compiles in development builds and is an error in release builds. Unused values and dead code are errors. |
| Forgetting the last step | A type can require that its value is consumed. A `Transaction` that's never committed or rolled back doesn't compile. |
| Silencing the compiler | No lint can be switched off inline. Exceptions go in project config, each with a written reason. |
| Quietly adding I/O | A function can only use the filesystem, network or clock if it's given one, and that shows in its parameters. |
| Writing slow code | Allocation is visible, the compiler warns about hidden copies in loops, and lints catch quadratic patterns like `in` on a list inside a loop. |

## Signatures that tell the truth

A function's parameters say what it changes (`mut`) and what it can touch (capabilities like `Fs` and `Clock`). Its written return type says what it produces, and the errors it can fail with say how it can fail. Today those are inferred; `fails Name` will put them in the signature. A model, or a reviewer, can understand a call without reading the function behind it.

## Errors written to be fixed

Every compiler error names the problem, explains why, and offers a fix that tools can apply automatically. Errors are also available as JSON, so an agent can read them directly.

```text
error: `enemy` might be None
  --> src/combat.fen:14:5
   |
12 |     enemy = world.enemies.get(target)
   |             ------------------------- this can return None
   |
14 |     enemy.take_damage(10)
   |     ^^^^^ used without checking
   |
   = fix: add `if enemy is None: return` above it, or wrap the call in `if enemy is not None:`
```

## Testing with nothing to set up

Tests are part of the language. There's no framework to choose, inputs for property tests are generated for any type, and tests get fake filesystems and clocks automatically. When testing is as easy as writing a comment, models write tests every time.

The test runner also reports tests that were deleted and assertions that were removed or loosened since the last commit, so a model can't quietly make a failing test pass by weakening it.

## Measured, not guessed

Whether AI writes a language well is something you can measure. Alongside the compiler, Fennec will keep a suite of programming tasks that models solve in Fennec, tracking:

- how often their code compiles on the first try,
- how often it passes its tests,
- how fast it runs compared to hand-written code.

Syntax decisions get settled with this data. When two designs are close, the one models write correctly more often wins.

## Tools for agents

- Diagnostics as JSON, with automatic fixes.
- The inferred signature of any function, on request.
- A short guide for models that ships with each compiler version.
- An MCP server so agents can build, test and look up types directly.
