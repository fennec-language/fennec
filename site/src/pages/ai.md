---
layout: ../layouts/Doc.astro
title: Built for AI to write
description: How Fennec is designed so AI models write correct code on the first try, and catch their own mistakes when they don't.
---

# Built for AI to write

<p class="lede">A growing share of new code is written with AI help. Fennec is designed so a model's first attempt is likely to be right, and so its mistakes are caught by the compiler instead of by you.</p>

## A shape models already know

Fennec's syntax is close to TypeScript: braces, `const` and `let`, `name: Type`, arrow functions, `import` and `export`. Models have seen a great deal of code shaped like this, so they write Fennec fluently with a short guide rather than a learning curve.

What Fennec drops is the part of TypeScript that causes bugs: `null` and `undefined`, `===`, `any`, implicit conversions, structural typing, and class inheritance.

## One way to do each thing

Models do best when there's one obvious answer. Fennec has one string syntax, one equality operator, one way to declare a function, one formatter with no settings, and one tool that builds, tests and lints. There are no macros and no custom operators to learn per project.

## Habits from other languages are caught, not guessed at

When a model reaches for something from another language, the compiler recognizes it and says exactly what to write instead.

| What a model writes | What Fennec says |
| --- | --- |
| `if (item == null)` | There's no `null`. `item` is an `Option`; compare it with `.None` or use `??`. |
| `try { ... } catch` | There are no exceptions. Return a `Result` and pass errors up with `?`. |
| `if (items)` | There's no truthiness. Write `if !items.isEmpty()`. |
| `x === y` | There's only `==`, and it never converts types. |
| `class Boss extends Enemy` | There's no inheritance. Use an `enum` case or an interface. |
| `var count = 0` | Use `let` for values that change and `const` for values that don't. |
| `console.log(value)` | There's no `console`. Write output through the `Stdout` that `main` receives; to inspect a value, use a test or the debugger. |

## Guardrails for the mistakes models actually make

| The mistake | What stops it |
| --- | --- |
| Ignoring an error | You can't use a `Result` without handling the failure. Discarding one needs `ignore(result, "why")`. |
| Assuming something can't fail | There's no `unwrap`. Asserting success needs `expect("reason")`, and an empty reason is rejected. |
| Leaving a stub behind | `todo` compiles in development builds and is an error in release builds. Unused values and dead code are errors. |
| Forgetting the last step | A type can require that its value is consumed. A `Transaction` that's never committed or rolled back doesn't compile. |
| Silencing the compiler | No lint can be switched off inline. Exceptions go in project config, each with a written reason. |
| Quietly adding I/O | A function can only use the filesystem, network or clock if it's given one, and that shows in its signature. |
| Writing slow code | Allocations and copies are visible, and lints catch quadratic patterns like `contains` inside a loop. |

## Signatures that tell the truth

A function's signature says what it changes (`inout`), what it can touch (capabilities like `Fs` and `Clock`), and how it can fail (`Result` and its error type). A model, or a reviewer, can understand a call without reading the function behind it.

## Errors written to be fixed

Every compiler error names the problem, explains why, and offers a fix that tools can apply automatically. Errors are also available as JSON, so an agent can read them directly.

```text
error: Fennec has no `null`
  --> src/inventory.fen:14:13
   |
14 |   if item == null {
   |              ^^^^ there is no null value
   |
   = fix: `item` is an `Option<Item>`. Write `if item == .None {` or use `item ?? fallback`.
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
- A short guide for models that ships with each compiler version.
- An MCP server so agents can build, test and look up types directly.
