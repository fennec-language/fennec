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

## Changing the front end

`selfhost/` mirrors the Rust code, so a change to the lexer, the parser, the tree dump or the
checker has to be made in both. The test `crates/fennec/tests/selfhost.rs` fails when they
disagree. Its message names the file, the first line where the dumps differ, and the source
line that line is about.

1. **Find the twin.** Each Fennec file mirrors one Rust file, as the table above shows, and
   its functions mostly have the same names: `resolve_within` in `check/program.fen` is
   `resolve_within` in `crates/check/src/program.rs`. Make the same change there, in the same
   order, with the same messages and help text.
2. **New syntax** goes in `parser.fen` and `ast.fen`. If the Rust tree dump shows the new
   field, show it in the `Tree` in `dump.fen` too. New tokens go in `token.fen` and
   `lexer.fen`.
3. **New built-in types or interfaces** go in the lists at the top of `check/program.fen`,
   and in `builtin_type` there if they hold other types.
4. **A new kind of checker error** isn't compared until its message is in `CHECK_KINDS` in
   the test. Only the kinds the self-hosted checker reports so far are listed, so a change
   to an error that isn't listed needs nothing here.
5. **Run the test:** `cargo test -p fennec --test fennec selfhost`. It runs both front ends
   over every example, every error example, the example kits, the standard library and
   `selfhost/` itself, with the interpreter and with the JIT.

Fennec code here has to pass the type checker too, so a stricter checker can break `selfhost/`
itself: `fennec run selfhost` then fails before it prints anything, with the checker's own
errors. Fix the Fennec code. If a rule felt wrong, add it to `NOTES.md`.

If you're stuck, open an issue labeled `area: selfhost`.

`NOTES.md` lists what was awkward to write in Fennec. It feeds the language's design.
