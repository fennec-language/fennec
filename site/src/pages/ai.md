---
layout: ../layouts/Doc.astro
title: Built for AI to write
description: How Fennec is designed so AI models write correct code on the first try, and catch their own mistakes when they don't.
---

# Built for AI to write

<p class="lede">A growing share of new code is written with AI help. Fennec is designed so a model's first attempt is likely to be right, and so its mistakes are caught by the compiler instead of by you.</p>

> **Some of this is planned.** Most of it works today. Not built yet: `todo()`, a required finish on types like `Transaction`, the check for weakened tests, the task suite, `fennec context` and the MCP server. Each is marked where it comes up.

## A shape models already know

Fennec reads like typed Python: indentation, `def`, comprehensions, f-strings, `and`, `or` and `not`, `True`, `False` and `None`. Python is the language models have written more of than any other, so they write Fennec fluently from a short guide rather than a learning curve.

What Fennec drops is the part of Python that causes bugs at scale: types that are only checked at runtime, exceptions, truthiness, shared mutable objects, classes and inheritance, and `None` values you can forget to check.

## One way to do each thing

Models do best when there's one obvious answer. Comprehensions build collections, and builtins like `sum` and `max` consume them. `mut` is the only word for a name that changes, and `ref` for a loop or function that changes yours, and errors always go through `fail`, `?`, `else` and `match`. There's one string syntax, one way to declare a function, one formatter with no settings, and one tool that builds, tests and lints. There are no macros and no decorators to learn per project.

## Habits from other languages are caught, not guessed at

When a model reaches for something Fennec doesn't have, the compiler recognizes it and says exactly what to write instead.

| What a model writes | What Fennec says |
| --- | --- |
| `count = 0` then `count += 1` | `count` isn't mut. Declare it `mut count`. |
| `def area(shape):` | Parameters need types: `def area(shape: Shape):`. |
| `if items:` | There's no truthiness. Write `if items.len() > 0:`. |
| `raise NotFound(path)` | There are no exceptions. Write `fail NotFound(path)`, which returns the error. |
| `try: ... except NotFound:` | Pass errors up with `?`, give a default with `else`, or handle them with `match`. |
| `print(value)` | There's no global `print`. A function reaches outside itself only through its signature: write `stdout.print(...)`. |

**Planned:** the same kind of help for `lambda`, which is `x => x * 2` here, for `class`, which is a `struct` or an `enum`, for `def __add__`, which is `def +`, and for setting a name in both branches of an `if`, which is `label = "a" if ready else "b"`.

## Guardrails for the mistakes models actually make

| The mistake | What stops it |
| --- | --- |
| Forgetting a value might be missing | An `Enemy?` can't be used until it's checked for `None`. |
| Tangled shared state | Data is plain values in tables. `shared` is rare, and every change to one takes its lock. |
| Expecting a copy to change the original | A name always holds its own value, so changing a copy that nothing reads is a compile error whose fix writes to the original. Planned: a copy that can go stale is a compile error too. |
| Changing a list while looping over it | A compile error naming the loop, saying to use `for ref` or loop over indexes. |
| Ignoring an error | A call that can fail needs `?`, `else` or `match`. Discarding an error on purpose needs `ignore(result, "why")`. |
| Assuming something can't fail | Asserting success needs `expect("reason")`, and an empty reason is rejected. |
| Leaving a stub behind | Unused functions and types are lint errors. Planned: `todo()`, which compiles in development builds and is an error in release builds. |
| Forgetting the last step | Planned: a type can require that it's finished, so a `Transaction` that's never committed doesn't compile. |
| Silencing the compiler | No lint can be switched off inline. Exceptions go in project config, each with a written reason. |
| Quietly adding I/O | A function can only use the filesystem, network or clock if it's given one, and that shows in its parameters. |
| Writing slow code | A copy made on every round of a loop is a compile error, `fennec lint` hints at what may copy or allocate, and `@hot` proves a function does neither. |

## Signatures that tell the truth

A function's parameters say what it changes (`ref`) and what it can touch (capabilities like `Fs` and `Clock`). Its written return type says what it produces, and `fails Name` says how it can fail. A model, or a reviewer, can understand a call without reading the function behind it.

## Errors written to be fixed

Every compiler error names the problem and says what to write instead. `fennec fix` applies the fixes that can't change what a program does, and leaves the rest as hints for you or the agent to make. Errors are also available as JSON, with `--json`, so an agent can read them directly.

```text
error: this might be None
  --> src/combat.fen:14:5
   |
14 |     enemy.take_damage(10)
   |     ^^^^^
   |
   = help: end the chain with `else` and a default, like `x.name else ""`, or check with `if x is None:` first
```

## Testing with nothing to set up

Tests are part of the language. There's no framework to choose, inputs for property tests are made up for you, and tests get fake filesystems, clocks and networks automatically. When testing is as easy as writing a comment, models write tests every time.

**Planned:** the test runner will also report tests that were deleted and assertions that were removed or loosened since the last commit, so a model can't quietly make a failing test pass by weakening it.

## Measured, not guessed

Whether AI writes a language well is something you can measure. Alongside the compiler, Fennec will keep a suite of programming tasks that models solve in Fennec, tracking:

- how often their code compiles on the first try,
- how often it passes its tests,
- how fast it runs compared to hand-written code.

Syntax decisions get settled with this data. When two designs are close, the one models write correctly more often wins.

## Tools for agents

- Diagnostics as JSON, with the edits `fennec fix` would make.
- `fennec init`, which writes Fennec's guidance for agents into `AGENTS.md`.
- A language server, for hover, go to definition and fixes.
- **Planned:** `fennec context`, a kit's whole API sized for a prompt, and an MCP server so agents can build, test and look up types directly.
