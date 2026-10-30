# Fennec in Fennec

The start of a self-hosted Fennec: the front end written in Fennec. It mirrors the Rust
front end in `crates/syntax` part for part, and prints the same dumps:

| Fennec                     | Rust                                   |
| -------------------------- | -------------------------------------- |
| the lexer in `src/main.fen` | `crates/syntax/src/lexer.rs`, `token.rs` |
| `dump_tokens`              | `fennec dev tokens <file>`             |

`crates/fennec/tests/selfhost.rs` checks that both front ends print exactly the same dump for
every example, every error example, the standard library's Fennec sources and this kit's own
source, with the interpreter and with the JIT.

## Running it

Fennec programs can't read their command-line arguments yet, so this one reads its work from
`inputs.txt` in the folder it runs in, one `tokens <path>` line per file:

```bash
echo "tokens examples/hello.fen" > inputs.txt
fennec run selfhost/src/main.fen
```

Compare with `fennec dev tokens examples/hello.fen`.

`NOTES.md` lists what was awkward to write in Fennec. It feeds the language's design.
