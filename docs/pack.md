# `std.pack`'s format

`std.pack` saves a value as bytes: a header naming its type's shape and layout, then its fields in
the order they're written, with no names. This is a wire format. Bytes saved by one version of a
program have to load in the next, so what's here is fixed: a change to it is a change to every
file saved, and goes in the release notes. `docs/language.md` says what `std.pack` does; this says
exactly what it writes.

## The header

- 4 bytes: `F`, `P`, `K`, then a mode. `2` has a layout. `1` has none, because the kit's `pack.lock`
  holds the type's layout as it is now. `X` and `E` are the same with `Table`s saved slot for slot
  (`exact=True`).
- 8 bytes: the type's shape hash, a signed 64-bit int, low byte first.
- With a layout: the layout text, saved as any text is, its length in bytes as a varint, 7 bits a
  byte, low bits first, with the top bit set on each byte but the last, then its UTF-8.

## The shape and its hash

A type's shape is text, made as follows:

- A plain type is its name: `int`, `float`, `f32`, `bool`, `str` and `bytes`.
- `list[T]`, `set[T]`, `dict[K, V]`, `Table[T]` and `Id[T]` are written so, with each part's shape
  in place of its letter, apart by `, `. An optional is its item's shape then `?`. A tuple is
  `(A, B)`, and a union is `(A | B)`, its members in the order the type lists them.
- A struct is its name, then `{`, each field it saves as `name: shape` apart by `, `, then `}`.
  Fields go in the order they're declared. A field `@skip` leaves out isn't there, and a field is
  named by its own name, not the one `@serialize(name=...)` gives it.
- An enum is its name, then `<`, each case apart by `, `, then `>`. A case is its name, and a case
  with fields has them after it as `(name: shape, ...)`.
- A generic struct or enum is its name, then its type arguments' shapes, each in full, in brackets,
  then its fields or cases with the arguments filled in: `Pair[int]{first: int, second: int}`.
- Inside its own shape, the same type with the same type arguments is written by its name and
  arguments alone: `Node{name: str, children: list[Node]}`, `Chain[int]{value: int, links:
  list[Chain[int]]}`. With other arguments it's another type, written in full. A type that comes
  up twice side by side, not inside itself, is written in full both times.
- A program's own type named like one in the standard library is written by its name as the
  program writes it.
- A type that writes its own `serialize` is shaped by what it saves, not by its fields, as its
  layout is. A standard library type that saves as another has that type's shape: a `Deque[T]`
  and a `SortedSet[T]` are `list[T]`, a `SortedMap[K, V]` is `dict[K, V]`, a `Packed[T]` and a
  `Grid[T]` are `list[list[T]]`, a `Uuid` and a `Time` are `str`, and a `Duration` is `int`.
- Any other type that writes its own `serialize`, a program's or a kit's, is its name and its type
  arguments' shapes in brackets, alone: `Money`, `Tagged[int]`. What it saves is its `serialize`'s
  to say, so its fields aren't in its shape, and changing them changes nothing.

The shape hash is 64-bit FNV-1a over the shape text's UTF-8 bytes: start at
`0xcbf29ce484222325`, and for each byte, xor it in and multiply by `0x100000001b3`, wrapping. The
result is read as a signed 64-bit int.

## The layout

The layout says what each value is, with every struct's field names and every enum's case names,
so bytes from another version of the type load by name. It's words apart by single spaces:

- `i`, `f`, `g`, `b`, `s` and `y`: an int, a float, an `f32`, a bool, text and bytes. An `f32` is its
  4 bytes, low first, and every `f32` NaN is the quiet NaN with no sign and no payload,
  `0x7FC00000`, as every float NaN is `0x7FF8000000000000`.
- `l` then the item: a list or a set. `m` then the key and the value: a dict. `o` then the item:
  an optional.
- `t`, the count of items, then each item: a tuple.
- `S`, the count of fields saved, then each field's saved name, the one `@serialize(name=...)`
  gives it if any, and what it holds: a struct.
- `E`, the count of cases, then each case's name, its count of fields, and each field's name and
  what it holds: an enum.
- `u`, the count of members, then each member's type as written with its spaces taken out, and
  what it holds: a union.
- `r` then a number: a struct or enum inside itself, the one open that many places in from the
  outermost, which is `0`.
- `T` then the item: a `Table`. `I`: an `Id`.
- The standard library's types that save as another are laid out as that type: a `Deque` and a
  `SortedSet` as a list of their items, a `SortedMap` as a dict, a `Packed` and a `Grid` as a list of
  lists, a `Uuid` and a `Time` as text, and a `Duration` as an int.
- `O` then the name: a type that saves itself its own way, read as it reads.
- `?`: what no format saves.

## Test vectors

Each row is what `pack.dumps` writes for a value of the type, in mode `2`: the shape, its hash as
the 8 bytes hold it, and the layout. `llvm::pack_headers_are_the_ones_docs_pack_md_pins` holds
the compiled program to this table, and each hash to its shape.

| Type | Shape | Hash, as written | Layout |
|---|---|---|---|
| `int` | `int` | `3ec8d42b19ff9f2b` | `i` |
| `str` | `str` | `f0600d5d19836e82` | `s` |
| `bool` | `bool` | `bd14b0c69bd42fcd` | `b` |
| `float` | `float` | `6501b242a9620aa0` | `f` |
| `f32` | `f32` | `443570ff185063dd` | `g` |
| `bytes` | `bytes` | `e44f1c4f47c02e2f` | `y` |
| `list[int]` | `list[int]` | `4675ffb2ccf2c473` | `l i` |
| `dict[str, int]` | `dict[str, int]` | `d1b23e33599e34ee` | `m s i` |
| `(int, str)` | `(int, str)` | `1e751c156c93397b` | `t 2 i s` |
| `Cell`, holding `int \| str` | `Cell{held: (int \| str)}` | `43ee6e1818b7c1fb` | `S 1 held u 2 int i str s` |
| `Point` | `Point{x: int, y: int}` | `7ebfb137ecdb8f58` | `S 2 x i y i` |
| `Note`, its `at` saved as `when`, `seen` skipped | `Note{text: str, at: int}` | `05c8c1838f24c980` | `S 2 text s when i` |
| `Shape`, `Dot` and `Circle(radius: float)` | `Shape<Dot, Circle(radius: float)>` | `5093b37191577e0d` | `E 2 Dot 0 Circle 1 radius f` |
| `Node`, holding `list[Node]` | `Node{name: str, children: list[Node]}` | `c2961eff3740fbc4` | `S 2 name s children l r 0` |
| `Pair[int]` | `Pair[int]{first: int, second: int}` | `047e06715d466f32` | `S 2 first i second i` |
| `Chain[int]`, holding `list[Chain[int]]` | `Chain[int]{value: int, links: list[Chain[int]]}` | `c8839d44f2c73115` | `S 2 value i links l r 0` |
| `Deque[int]` | `list[int]` | `4675ffb2ccf2c473` | `l i` |
| `Time` | `str` | `f0600d5d19836e82` | `s` |
| `Money`, writing its own `serialize` | `Money` | `6f4393e1f9b543b4` | `O Money` |
