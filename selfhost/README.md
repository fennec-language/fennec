# Fennec in Fennec

The start of a self-hosted Fennec: the front end written in Fennec. It mirrors the Rust
front end in `crates/syntax` part for part, and prints the same dumps:

| Fennec           | Rust                                      |
| ---------------- | ----------------------------------------- |
| `src/span.fen`   | `crates/syntax/src/span.rs`, `diagnostic.rs` |
| `src/token.fen`  | `crates/syntax/src/token.rs`              |
| `src/lexer.fen`  | `crates/syntax/src/lexer.rs`              |
| `src/ast.fen`    | `crates/syntax/src/ast.rs`                |
| `src/parser.fen` | `crates/syntax/src/parser.rs` and `parser/{stmt,expr,pattern}.rs` |
| `src/dump.fen`   | `crates/syntax/src/dump.rs` and `dump/tree.rs`, which `fennec dev tokens <file>` and `fennec dev tree <file>` print |

`crates/fennec/tests/selfhost.rs` checks that both front ends print exactly the same dump for
every example, every error example, the standard library's Fennec sources and this kit's own
source, with the interpreter and with the JIT.

## Running it

Fennec programs can't read their command-line arguments yet, so this one reads its work from
`inputs.txt` in the folder it runs in, one `tokens <path>` or `tree <path>` line per file:

```bash
printf "tokens examples/hello.fen\ntree examples/hello.fen\n" > inputs.txt
fennec run selfhost
```

Compare with `fennec dev tokens examples/hello.fen` and `fennec dev tree examples/hello.fen`.

`NOTES.md` lists what was awkward to write in Fennec. It feeds the language's design.
