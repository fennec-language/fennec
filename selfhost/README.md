# Fennec in Fennec

The start of a self-hosted Fennec: the front end written in Fennec. It mirrors the Rust
front end in `crates/syntax` and the checker in `crates/check` part for part, and prints the
same dumps:

| Fennec           | Rust                                      |
| ---------------- | ----------------------------------------- |
| `src/span.fen`   | `crates/syntax/src/span.rs`, `diagnostic.rs` |
| `src/token.fen`  | `crates/syntax/src/token.rs`              |
| `src/lexer.fen`  | `crates/syntax/src/lexer.rs`              |
| `src/ast.fen`    | `crates/syntax/src/ast.rs`                |
| `src/parser.fen` | `crates/syntax/src/parser.rs` and `parser/{stmt,expr,pattern}.rs` |
| `src/check/types.fen` | `crates/check/src/types.rs` |
| `src/check/program.fen` | `crates/check/src/program.rs` |
| `src/check/checker.fen` | `crates/check/src/checker.rs`, starting with the declarations |
| `src/dump.fen`   | `crates/syntax/src/dump.rs` and `dump/tree.rs`, which `fennec dev tokens <file>` and `fennec dev tree <file>` print, and the checker's diagnostics, which `fennec dev check <file>` prints |

`crates/fennec/tests/selfhost.rs` checks that both front ends print exactly the same dump for
every example, every error example, every file of the example kits, the standard library's
Fennec sources and this kit's own source, with the interpreter and with the JIT. For the
checker it compares the kinds of diagnostics ported so far (`CHECK_KINDS`), so a program the
Rust checker accepts must get no complaint from the self-hosted one either.

## Running it

Fennec programs can't read their command-line arguments yet, so this one reads its work from
`inputs.txt` in the folder it runs in, one `tokens <path>`, `tree <path>` or `check <path>`
line per file. `check` checks against the standard library sources named by `std <path>`
lines before it:

```bash
printf "tokens examples/hello.fen\ntree examples/hello.fen\ncheck examples/hello.fen\n" > inputs.txt
fennec run selfhost
```

Compare with `fennec dev tokens examples/hello.fen`, `fennec dev tree examples/hello.fen` and
`fennec dev check examples/hello.fen`.

`NOTES.md` lists what was awkward to write in Fennec. It feeds the language's design.
