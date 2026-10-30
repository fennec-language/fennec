---
layout: ../layouts/Doc.astro
title: Why Fennec
description: Why Fennec is being built, what it's for, and the principles behind every decision.
---

# Why Fennec

<p class="lede">Software should be pleasant to build and fast to run. Too often it's neither. Fennec exists to fix that for the software we make: games, tools and the services behind them.</p>

Builds that take minutes break your focus. Build setups that need pages of configuration take time away from the actual work. Code that's hard to read is hard to trust, and code that's slow for reasons nobody can see is hard to fix. None of that is necessary.

## What Fennec is for

**Fast iteration while you work.** When you change a line, you should see the result in moments. Development builds in Fennec are designed for that alone: changed code runs in an interpreter right away, and everything you didn't touch comes from a cache.

**Full speed when you ship.** Release builds take their time and produce code that runs as fast as hand-tuned native code, with no garbage collector and no hidden allocation.

**A gap between the two is fine.** Developing and shipping are different jobs. A development build optimizes for your time; a release build optimizes for the player's or user's time. Fennec doesn't pretend one build can do both well.

**Code that reads like Python.** If you can read Python, you can read Fennec: indentation, `def`, comprehensions and f-strings, with types checked before anything runs.

**Code you can trust.** If it compiles, it's memory-safe, its errors are handled, and anything that mutates your data says so where it happens.

**Nothing to configure.** One tool builds, tests, formats, lints and manages dependencies. Strong defaults are built in, so a new project starts out strict without a page of settings.

**Built for working with AI.** More and more code is written with AI help. Fennec is designed so a model's first attempt is likely to be right, and its mistakes are caught by the compiler instead of by your users.

## Principles

These decide every design question.

1. **The easy way is the readable way and the fast way.** If the obvious code is slow or unclear, that's a bug in the language.
2. **If it compiles, it's safe.** Memory safety isn't optional, and escape hatches exist only at the boundary with C.
3. **Every guardrail is free or visible.** Checks cost nothing at runtime, and every exception to a rule carries a written reason you can search for.
4. **Readable by people, writable by AI.** A familiar syntax with one way to do each thing, and compiler errors that explain how to fix them.
5. **Occam's razor.** Fewer concepts, each doing one job well. Every feature has to earn its place with real programs that need it.

## Small and decisive

Fennec is small and opinionated, and it moves quickly. Decisions are made by the people building with it, based on real programs and measurements. Until 1.0, nothing is frozen: if something turns out to be wrong, it changes.

## Why the name?

Fennec foxes are cool. That's the whole reason.
