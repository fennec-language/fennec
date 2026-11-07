---
layout: ../layouts/Doc.astro
title: Why Fennec
description: Why Fennec is being built, what it's for, and the principles behind every decision.
---

# Why Fennec

<p class="lede">Software should be pleasant to build and fast to run. Too often it's neither. Fennec exists to fix that for the software we make: games, tools and the services behind them.</p>

Builds that take minutes break your focus. Build setups that need pages of configuration take time away from the actual work. Code that's hard to read is hard to trust, and code that's slow for reasons nobody can see is hard to fix. None of that is necessary.

## What Fennec is for

**Fast iteration while you work.** When you change a line, you should see the result in moments. Fennec compiles a function at a time and caches each one by its content, so a change costs only what it touches.

**What you run is what ships.** There's one optimizing backend, LLVM, so the program you try while you work is the one you ship, with no garbage collector. Whole-program optimization is the only extra step, and it comes with `--release`. In CI, release builds run at about the speed of hand-written native code, measured over the whole run, startup included.

**Code that reads like Python.** If you can read Python, you can read Fennec: indentation, `def`, comprehensions and f-strings, with types checked before anything runs.

**Code you can trust.** If it compiles, it's memory-safe, its failures are handled, and anything that changes your data says so where it happens. There's no `unsafe` to reach for.

**Nothing to configure.** One tool builds, tests, formats and lints, and it will manage dependencies too. Strong defaults are built in, so a new project starts out strict without a page of settings.

**Built for working with AI.** More and more code is written with AI help. Fennec is designed so a model's first attempt is likely to be right, and its mistakes are caught by the compiler instead of by your users.

## Principles

These decide every design question.

1. **The easy way is the readable way and the fast way.** If the obvious code is slow or unclear, that's a bug in the language.
2. **If it compiles, it's safe.** Memory safety isn't optional, and there's no escape hatch. Where a C library is needed, the Fennec project wraps it behind a safe API.
3. **Every guardrail is free or visible.** A check stays unless the compiler proves it unnecessary, and every exception to a rule carries a written reason you can search for.
4. **Readable by people, writable by AI.** A familiar syntax with one way to do each thing, and compiler errors that explain how to fix them.
5. **Occam's razor.** Fewer concepts, each doing one job well. Every feature has to earn its place with real programs that need it.

## Small and decisive

Fennec is small and opinionated, and it moves quickly. Decisions are made by the people building with it, based on real programs and measurements. Until 1.0, nothing is frozen: if something turns out to be wrong, it changes.

## Why the name?

Fennec foxes are cool. That's the whole reason.
