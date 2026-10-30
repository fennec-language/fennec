# What was awkward

Writing Fennec's own front end in Fennec, these are the places where the language got in the
way, or where the code says something less directly than it should. Each one says what the
code does instead.

## Input and output

- **A program can't read its command-line arguments**, and there's no stdin. The front end
  reads its work from `inputs.txt` in the current folder instead. An `Args` capability (or
  `Env`, still open in `docs/decisions.md`) would fix it.

## Strings and characters

- **There's no way to get a character's code or its UTF-8 length.** Spans count bytes, but
  strings index by character, so the lexer needs each character's byte width. `utf8_width`
  compares against the first character of each width (`ࠀ` is U+0800, `𐀀` is U+10000). It
  can't name U+0080 at all, so it treats everything after `~` as two bytes or more, and gets
  DEL (U+007F) wrong. `ord`, or a `str.utf8_len()`, would make this one line.
- **Indexing a string walks it from the start**, so `source[i]` in a loop is quadratic. The
  lexer turns the source into a list of one-character strings with `list(source)` and
  indexes that, and rebuilds text with `"".join(chars[start:end])`.
- **Escapes stop at `\n \t \r \0 \\ \"`.** There's no `\x7f` or `\u{80}`, so control
  characters and invisible ones can't be written in a string at all.

## Numbers

- **`int(text)` and `float(text)` stop the program when the text doesn't fit**, rather than
  failing with an error the caller can handle. The lexer checks for overflow digit by digit
  itself, and checks a float's exponent has digits before calling `float`.

## Declarations

- **There are no module-level constants.** The keyword set is a set literal inside
  `is_keyword`, so it's rebuilt on every call. A `KEYWORDS = {...}` at the top level is what
  anyone would write.
- **A local can't have a written type.** `counts: dict[str, int] = {}` is a parse error, so an
  empty collection's type comes from how it's used later.

## Matching

- **`case` has no or-patterns and no guards.** `case " " | "\r":` and `case c if is_digit(c):`
  would let the lexer's main loop be one `match` on the character. It's an `if`/`elif`
  chain instead.
