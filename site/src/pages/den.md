---
layout: ../layouts/Doc.astro
title: The Den
description: A preview of the Den, Fennec's registry for kits. It isn't built yet.
---

# The Den

<p class="lede">The Den will be Fennec's registry for kits. <strong>It isn't built yet.</strong> This is the plan, and it may change.</p>

## What it's for

A kit is a Fennec package. The Den keeps every kit's source permanently, and installing one never runs any of its code.

## How much checking a kit gets

The audit scales with what a kit can touch:

- **Pure**, with no capabilities and no C: checked automatically.
- **Capability**, using `Fs`, `Net` and the like: checked automatically and reviewed by AI, with its capabilities shown on its page. Adding a capability takes the author's own passkey tap.
- **Native**, with C code: audited by a person, and first-party kits only for now.

## Publishing

`fennec publish` opens your browser, you tap Touch ID or Face ID, and the source is uploaded with its signed hash. The Den then runs the kit's formatting, lints and tests in a sandbox, and shows what changed in its capabilities and API.

Accounts use passkeys only, with no passwords and no recovery by email.

## Known gap

Until a public transparency log exists, a hacked Den could swap an author's key undetected. The log should be built before the Den gets popular.
