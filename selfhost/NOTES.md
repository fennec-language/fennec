# What was awkward

Writing Fennec's own front end in Fennec, these are the places where the language got in the
way, or where the code says something less directly than it should. Each one says what the
code does instead, and links the issue where it's being decided or fixed.

## Input and output

- **A program can't read its command-line arguments**, and there's no stdin. The front end
  reads its work from `inputs.txt` in the current folder instead. An `Args` capability (or
  `Env`, still open in `docs/decisions.md`) would fix it. (#64)

## Strings and characters

- **`str * int` isn't supported.** The tree dump keeps its indentation as a string it grows
  and shrinks, instead of writing `"  " * depth`. (#69)
- **An f-string's `{expression}` can't contain a string**, since the first `"` ends the
  f-string. The lexer copies this rule from the Rust lexer.
- **Settled: there was no way to get a character's UTF-8 length.** Spans count bytes, but
  strings index by character, so the lexer needs each character's byte width. It used to
  compare against the first character of each width (`ࠀ` is U+0800, `𐀀` is U+10000), which
  got DEL (U+007F) wrong. Since #93 it's `len(c.encode())`. (#65)
- **Indexing a string walks it from the start**, so `source[i]` in a loop is quadratic. The
  lexer turns the source into a list of one-character strings with `list(source)` and
  indexes that, and rebuilds text with `"".join(chars[start:end])`. (#65)
- **Escapes stop at `\n \t \r \0 \\ \"`.** There's no `\x7f` or `\u{80}`, so control
  characters and invisible ones can't be written in a string at all. (#65)

## Numbers

- **`int(text)` and `float(text)` stop the program when the text doesn't fit**, rather than
  failing with an error the caller can handle. The lexer checks for overflow digit by digit
  itself, and checks a float's exponent has digits before calling `float`. (#66)

## Declarations

- **There are no module-level constants.** The keyword set is a set literal inside
  `is_keyword`, so it's rebuilt on every call. A `KEYWORDS = {...}` at the top level is what
  anyone would write. (#67)
- **A local can't have a written type.** `counts: dict[str, int] = {}` is a parse error, so an
  empty collection's type comes from how it's used later. (#69)

## Enums

- **A leading-dot case ignores the type it's going into.** `.Int(value)` is an error whenever
  two enums have a case called `Int`, even as an argument to a parameter typed `ExprKind`, a
  field typed `ExprKind`, or the result of a function that returns `ExprKind`. The token and
  syntax-tree enums share `Int`, `Float`, `Str`, `FStr`, `Name`, `Text`, `Code`, `List`,
  `Set`, `Dict`, `Tuple` and `Variant`, so those are written `ExprKind.Int(value)`,
  `Tok.Name(word)` and so on. Patterns don't have this problem: `case .Int(value):` works.
  The search covers the whole kit, not just what a file imports: `lexer.fen` never sees the
  syntax tree, but adding `ast.fen` to the kit made its `.Name(word)` ambiguous. Adding the
  checker's `Type` enum did the same to the parser's `.Function(...)` and `.Bool(...)`. (#62)
- **A case can't be called `None`**, so the `None` literal in the syntax tree is
  `ExprKind.Nothing`.
- **Keywords can't be field names**, so a case holding a function is `Function(function)`,
  not `Def(def)`, and one holding an import is `Import(imported)`. (#69)

## Functions

- **Settled: a recursive function that returned nothing had to say `-> nothing`.** The
  checker wanted a return type on every recursive function, and rejected `-> None` when the
  body ended in a call. Since #108, a `def` with no arrow returns nothing, recursive or not,
  so those arrows are gone. (#63)
- **A type's methods must all sit in its declaration.** Rust spreads `impl Parser` over
  `parser.rs` and `parser/{stmt,expr,pattern}.rs`; here the parser is one ~1,000-line
  `struct Parser`, with comments marking where each Rust file's part begins. Something like
  Swift's extensions, or letting a later file add methods to a kit's own type, would let
  it split the same way. (#69)
- **There's no `while let`.** Loops that run while a lookup succeeds are `while True:`, a
  lookup, and `if op is None: break`. (#69)

## Matching

- **`case` has no or-patterns and no guards.** `case " " | "\r":` and `case c if is_digit(c):`
  would let the lexer's main loop be one `match` on the character. It's an `if`/`elif`
  chain instead. `describe` repeats arms that or-patterns would merge (`.Int(_)` and
  `.Float(_)` are both "a number"), and the parser's `match self.peek():` arms often hold an
  `if` that a guard would replace. (#68)

## Engines

These are bugs rather than language design, found because this is the largest Fennec
program so far.

- **The JIT mixed up methods that share a name across types.** With `field`, `type_def`,
  `expr` and others on both `Parser` and `Tree`, `--jit` called the wrong one and failed with
  "a nothing has no field `name`". The interpreter was right. The tree dump's methods are
  named `write_expr` and so on to avoid it. Small programs with the same shape don't trigger
  it. It came back when the checker added `Checker.run` beside `Lexer.run`, so that one is
  `check_all`. (#60)
- **Calling a `mut self` method copied the receiver's lists.** Each call to a method that
  appends to a field copied the whole list, so lexing and parsing were quadratic in the size
  of the file. The interpreter now moves arguments into the callee's frame. The JIT still
  copies. (#61)
