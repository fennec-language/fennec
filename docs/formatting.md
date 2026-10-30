# How `fennec fmt` formats code

`fennec fmt` has one style and no options, like `gofmt`. This page describes that style. The
decisions doc settles only some of it: one style with no options, 4-space indentation, and
`##` doc comments above what they document. Everything marked *to confirm* is a choice the
formatter makes that the design still needs to agree to.

## What it never changes

The formatter works on tokens. It changes only the space and line breaks between them, so it
never adds, drops or reorders code. The one exception is a trailing comma, added when it
breaks a list across lines. Comments are kept word for word, apart from spaces at the end of the line.
Parentheses, number spellings like `1_000`, and string escapes stay as written.

Before it writes a file, it parses the result and checks the syntax tree is the same as
before, positions aside, and that every comment is still there. If either check fails, the
file is left alone and the command reports a bug.

## Lines and brackets

- Lines are at most 100 characters. *To confirm.*
- A bracketed list that doesn't fit puts each item on its own line, one indent deeper, and
  ends with a trailing comma. The closing bracket goes on its own line. The outermost list
  breaks first. *To confirm.*
- A list whose only item is another bracketed list hugs it: `"\n".join([` opens both, the
  inner list's items go one indent deeper, and `])` closes both. *To confirm.*
- A list whose last item ends with a comma stays broken, even if it would fit, so a list can
  be kept one item per line on purpose. A one-item tuple like `(x,)` doesn't count.
  *To confirm.*
- A list that fits on one line is joined, even if the source broke it.
- No comma is added where the parser wouldn't take one: after a comprehension, inside
  indexes and type arguments, in generic parameters, after arrow-function parameters, or
  inside parentheses around one expression.
- A line with no brackets to break stays long. Trailing comments don't count toward the
  width. *To confirm.*
- Blank lines inside brackets are removed. *To confirm.*

## Spaces

- One space around binary operators, including `**`, and around `->`, `=>` and `=`.
  *To confirm `**`; Python's style hugs it for simple operands.*
- No space after unary minus, before `(` or `[` in calls and indexes, or around `.`.
- Keyword arguments hug their `=`: `Vec3(x=1.0)`. Defaults in declarations don't:
  `x = 0.0` in a struct, `Wait(turns = 1)` in an enum.
- One space after `,` and after `:` in types, dicts and blocks; none before.
- Slices have no spaces around their colon: `xs[1:3]`, `xs[i + 1:]`. *To confirm.*
- Operator methods are written tight: `def +(self, other: Vec3)`, `def []=(mut self, ...)`.

## Blank lines

- Top-level functions, types, interfaces and tests get one blank line before and after. A
  comment directly above one moves with it. *To confirm: one blank line, not Python's two.*
- Methods in a struct, enum or error get one blank line around them. Interface signatures
  don't. *To confirm.*
- Everywhere else, blank lines are kept, but never more than one in a row. *To confirm.*
- No blank lines at the start of a block or at the end of a file. A file ends with one
  newline.

## Blocks

- Each block is indented 4 spaces more than the line ending in `:`.
- A body on the same line as its `:`, like `case .Point: 0.0`, stays there unless the line
  is too long; then it moves to its own line, one indent deeper. A body on its own lines
  stays there. *To confirm.*

## Comments

- A trailing comment has two spaces before its `#`. Trailing comments on neighbouring lines
  at the same indentation line up, as on the site. *To confirm.*
- A comment on its own line between blocks takes the indentation of the block its column
  falls in, so a comment at the end of a block stays in it.
- `#x` is not rewritten to `# x`. *To confirm.*

## The command

- `fennec fmt [paths]` rewrites files in place. A folder means every `.fen` file under it,
  skipping hidden folders, `target` and `node_modules`.
- With no paths, it formats the whole kit around the current folder: the nearest folder at
  or above it with a `kit.toml`, `src/` and all. Outside a kit, it formats the current
  folder.
- `fennec fmt --check [paths]` lists the files that aren't formatted and fails if there are
  any. CI runs `fennec fmt --check examples crates/syntax/src/stdlib`.
- A file that doesn't parse is skipped with a note, and doesn't fail the command: the
  compiler reports syntax errors. *To confirm.*
- A file keeps its line endings: CRLF stays CRLF.
