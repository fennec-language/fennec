# How `fennec fmt` formats code

`fennec fmt` has one style and no options. The rules below are decided; the summary is in
`docs/decisions.md` under Tooling. The principle: the output depends only on the code, never
on how it was laid out, so the same syntax tree always formats to the same text.

## What it never changes

The formatter works on tokens and changes only the space and line breaks between them. It
adds or removes nothing that changes the syntax tree:

- a trailing comma on a broken list,
- parentheses around a long expression,
- the order of imports, which can't change what a program means.

Parentheses written in the source, number spellings like `1_000`, and string escapes stay as
written. Comments keep their text.

Before it writes a file, it parses the result and checks that the syntax tree is the same as
before (positions aside, imports sorted) and that every comment is still there. If either
check fails, the file is left alone and the command reports a bug.

## Lines and brackets

- Lines are at most 100 characters.
- A bracketed list that fits goes on one line, without a trailing comma.
- A list that doesn't fit puts each item on its own line, one indent deeper, with a trailing
  comma. The outermost list breaks first. A trailing comma in the source doesn't keep a list
  broken; only a one-item tuple keeps its comma on one line, `(x,)`.
- A list of plain literals (numbers, `True`, `False`, `None`, strings up to 20 characters)
  fills its lines instead of one item per line.
- A lone list argument hugs its call: `"\n".join([` opens both, and `])` closes both.
- No comma is added where the parser wouldn't take one: after a comprehension, inside indexes,
  type arguments and generic parameters, or after arrow-function parameters.
- Inside brackets, an item that doesn't fit breaks before each of its loosest operators, with
  the operator starting the line:

  ```python
  if (
      enemy.health > 0
      and enemy.visible
      and not world.paused
  ):
  ```

- A statement that's still too long with nothing to break gets its expression wrapped in
  parentheses: an assignment's value, what follows `return`, `assert`, `fail` or `yield`, or
  the condition of an `if`, `elif` or `while`. Expressions with `if`/`else`, a comprehension,
  an arrow function or a `match` at the top aren't split at operators.
- Trailing comments don't count toward the width.

## Spaces

- One space around binary operators, including `**`, and around `->`, `=>` and `=`.
- No space after unary minus, before `(` or `[` in calls and indexes, or around `.`.
- Keyword arguments hug their `=`: `Vec3(x=1.0)`. Defaults in declarations don't: `x = 0.0`
  in a struct, `Wait(turns = 1)` in an enum.
- One space after `,` and `:` in types, dicts and blocks; none before.
- Slices have no spaces around their colon: `xs[1:3]`.
- Operator methods are written tight: `def +(self, other: Vec3)`, `def []=(mut self, ...)`.

## Blank lines

- Top-level functions, types, interfaces and tests get one blank line before and after. A
  comment directly above one moves with it.
- Methods in a struct, enum or error get one blank line around them. Interface signatures
  don't.
- Everywhere else, blank lines are kept, but never more than one in a row.
- No blank lines at the start of a block or at the end of a file. A file ends with one line
  feed, and every line ends with LF.

## Blocks

- Each block is indented 4 spaces more than the line ending in `:`.
- A body goes on its own lines, `case` arms included, however it was written.
- A guard clause stays on one line when it fits: an `if` with no `elif` or `else` whose body is
  a single `return`, `fail`, `break` or `continue`. `if hp <= 0: return`. Written as a block,
  it's joined onto one line when it fits; when it doesn't fit, it's split.

## Imports

- A run of imports at the top level is sorted: `std` first, then everything else, with a blank
  line between the groups, and alphabetical within each. The names in `from x import a, b`
  are sorted too.
- Once kits can depend on other kits, those imports will get a group of their own between
  `std` and the kit's own modules.
- A comment on its own line above an import moves with it, except above the first import,
  where it stays: it's usually about the whole file.

## Comments

- A trailing comment has two spaces before its `#`. Comments on neighbouring lines aren't
  lined up.
- A comment gets one space after its `#` or `##`: `#x` becomes `# x`. A `#!` line is left as
  it is. Otherwise its text is kept.
- A comment on its own line between blocks takes the indentation of the block its column falls
  in, so a comment at the end of a block stays in it.

## The command

- `fennec fmt [paths]` rewrites files in place. A folder means every `.fen` file under it,
  skipping hidden folders, `target` and `node_modules`.
- With no paths, it formats the whole kit around the current folder: the nearest folder at or
  above it with a `kit.toml`. Outside a kit, it formats the current folder.
- `fennec fmt --check [paths]` lists the files that aren't formatted and fails if there are
  any. CI runs `fennec fmt --check examples crates/syntax/src/stdlib`.
- A file that doesn't parse is skipped with a note, and doesn't fail the command: the compiler
  reports syntax errors.
