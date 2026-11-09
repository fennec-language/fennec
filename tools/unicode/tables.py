#!/usr/bin/env python3
"""Writes `crates/rt/src/unicode_tables.rs`, the tables `std.unicode` reads, from the
Unicode Character Database as Python's `unicodedata` holds it. Run it with the Python whose
Unicode version the tables should have, and commit what it writes:

    python3.14 tools/unicode/tables.py > crates/rt/src/unicode_tables.rs
    cargo fmt

Hangul syllables are left out of the decomposition and composition tables, since they're
worked out by arithmetic, as Unicode's own algorithm does.
"""

import sys
import unicodedata as ucd

LAST = 0x10FFFF
HANGUL = range(0xAC00, 0xD7A4)

def h(n):
    """A code point as hex, with `_` between groups of four digits once it needs more than four."""
    if n <= 0xFFFF:
        return hex(n)
    digits = f"{n:08x}"
    return f"0x{digits[:4]}_{digits[4:]}"

def chars():
    for code in range(LAST + 1):
        if 0xD800 <= code <= 0xDFFF:
            continue
        yield code, chr(code)

def mapping_table(name, pairs):
    """A sorted table of code point to code points, as offsets into one flat list."""
    data, rows = [], []
    for code, mapped in pairs:
        rows.append((code, len(data), len(mapped)))
        data.extend(ord(c) for c in mapped)
    out = [f"pub(crate) static {name}_DATA: [u32; {len(data)}] = ["]
    out += [f"    {', '.join(h(x) for x in data[i:i + 12])}," for i in range(0, len(data), 12)]
    out.append("];")
    out.append(f"pub(crate) static {name}: [(u32, u16, u8); {len(rows)}] = [")
    out += [f"    {' '.join(f'({h(c)}, {o}, {n}),' for c, o, n in rows[i:i + 6])}" for i in range(0, len(rows), 6)]
    out.append("];")
    return out

def main():
    categories = []
    names = []
    for code, c in chars():
        cat = ucd.category(c)
        if cat not in names:
            names.append(cat)
        if not categories or categories[-1][1] != cat:
            categories.append((code, cat))
    combining = [(code, ucd.combining(c)) for code, c in chars() if ucd.combining(c)]
    canonical = [
        (code, ucd.normalize("NFD", c))
        for code, c in chars()
        if code not in HANGUL and ucd.normalize("NFD", c) != c
    ]
    compatible = [
        (code, ucd.normalize("NFKD", c))
        for code, c in chars()
        if code not in HANGUL and ucd.normalize("NFKD", c) != ucd.normalize("NFD", c)
    ]
    compose = []
    for code, c in chars():
        if code in HANGUL:
            continue
        parts = ucd.decomposition(c)
        if not parts or parts.startswith("<"):
            continue
        parts = [int(p, 16) for p in parts.split()]
        # A primary composite: two parts, and NFC keeps it, so it isn't excluded.
        if len(parts) == 2 and ucd.normalize("NFC", c) == c:
            compose.append((parts[0], parts[1], code))
    compose.sort()
    folded = [(code, c.casefold()) for code, c in chars() if c.casefold() != c]

    out = [
        "// Written by `tools/unicode/tables.py` from the Unicode Character Database,",
        f"// version {ucd.unidata_version}, as Python {sys.version.split()[0]}'s `unicodedata` holds it.",
        "// Don't edit it by hand: run the script again.",
        "",
        f'pub(crate) const VERSION: &str = "{ucd.unidata_version}";',
        "",
        "/// The general categories, by the index `CATEGORIES` gives each run.",
        f"pub(crate) static CATEGORY_NAMES: [&str; {len(names)}] = [{', '.join(repr(n).replace(chr(39), chr(34)) for n in names)}];",
        "/// Where each run of one category starts, and its category.",
        f"pub(crate) static CATEGORIES: [(u32, u8); {len(categories)}] = [",
    ]
    out += [f"    {' '.join(f'({h(c)}, {names.index(k)}),' for c, k in categories[i:i + 8])}" for i in range(0, len(categories), 8)]
    out.append("];")
    out.append("/// Each character's canonical combining class, for those that have one other than 0.")
    out.append(f"pub(crate) static COMBINING: [(u32, u8); {len(combining)}] = [")
    out += [f"    {' '.join(f'({h(c)}, {k}),' for c, k in combining[i:i + 8])}" for i in range(0, len(combining), 8)]
    out.append("];")
    out.append("/// Each character's full canonical decomposition, its NFD.")
    out += mapping_table("CANONICAL", canonical)
    out.append("/// Each character's full compatibility decomposition, its NFKD, where that isn't its NFD.")
    out += mapping_table("COMPATIBLE", compatible)
    out.append("/// The primary composites: two characters, sorted, and the one they compose to.")
    out.append(f"pub(crate) static COMPOSE: [(u32, u32, u32); {len(compose)}] = [")
    out += [f"    {' '.join(f'({h(a)}, {h(b)}, {h(c)}),' for a, b, c in compose[i:i + 4])}" for i in range(0, len(compose), 4)]
    out.append("];")
    out.append("/// Each character's full case folding.")
    out += mapping_table("FOLDED", folded)
    print("\n".join(out))

main()
