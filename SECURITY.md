# Security policy

Fennec is in early design, and only the latest commit on `main` is supported.

## Our goal: near-zero CVEs

True zero isn't possible: the compiler, the runtime and the C that can't be ported can all have bugs. Fennec's goal is to remove the usual causes:

- **Memory bugs are impossible in safe code.** `unsafe` exists only in extern modules.
- **C is ported rather than linked,** so less C runs at all.
- **A batteries-included standard library** keeps dependency trees shallow.
- **Capabilities limit the blast radius** of any bug: code can reach only what it was handed.
- **Typed standard library APIs** stop injection.
- **The runtime is small, in-house and fuzzed continuously.**
- **`fennec audit` is backed by a public advisory database.**
- **Programs ship as one static binary in an empty container,** so there's no OS image full of other people's CVEs.

This is the goal Fennec is built toward; some of it isn't built yet. The reasoning is in [the design decisions](docs/decisions.md).

## Reporting a vulnerability

Please report security problems privately through GitHub: open the repository's **Security** tab and choose **Report a vulnerability**. Don't open a public issue.

Reports about the interpreter, the `fennec` command, the release archives, or the website are all welcome. You'll get a reply within a week.
