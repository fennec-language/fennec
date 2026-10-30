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
- **There's no way to get a character's code or its UTF-8 length.** Spans count bytes, but
  strings index by character, so the lexer needs each character's byte width. `utf8_width`
  compares against the first character of each width (`ࠀ` is U+0800, `𐀀` is U+10000). It
  can't name U+0080 at all, so it treats everything after `~` as two bytes or more, and gets
  DEL (U+007F) wrong. `ord`, or a `str.utf8_len()`, would make this one line. (#65)
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
  syntax tree, but adding `ast.fen` to the kit made its `.Name(word)` ambiguous. (#62)
- **A case can't be called `None`**, so the `None` literal in the syntax tree is
  `ExprKind.Nothing`.
- **Keywords can't be field names**, so a case holding a function is `Function(function)`,
  not `Def(def)`, and one holding an import is `Import(imported)`. (#69)

## Functions

- **A recursive function must write its return type, and there's no way to write "returns
  nothing".** The checker suggests `-> nothing`, which isn't a type it knows: it passes only
  because unknown types aren't checked. `-> None` fails as soon as the body ends in a call.
  The tree dump's recursive methods say `-> nothing` for now. Which method of a recursive
  group needs it depends on the order they're checked in, so renaming a method can move the
  error to another one. This one feels like the checker being wrong. (#63)
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
  it. (#60)
- **Calling a `mut self` method copied the receiver's lists.** Each call to a method that
  appends to a field copied the whole list, so lexing and parsing were quadratic in the size
  of the file. The interpreter now moves arguments into the callee's frame. The JIT still
  copies. (#61)
