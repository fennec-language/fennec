//! What `std.unicode` does: a character's general category, the four normalization forms of
//! UAX #15, and full case folding, from the tables `tools/unicode/tables.py` writes. Hangul
//! syllables are decomposed and composed by arithmetic, as Unicode's algorithm says.

use crate::unicode_tables::{
    CANONICAL, CANONICAL_DATA, CATEGORIES, CATEGORY_NAMES, COMBINING, COMPATIBLE, COMPATIBLE_DATA, COMPOSE,
    FOLDED, FOLDED_DATA,
};

pub(crate) use crate::unicode_tables::VERSION;

const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = L_COUNT * N_COUNT;

/// A character's general category, like `Lu` for an upper case letter.
pub(crate) fn category(c: char) -> &'static str {
    let code = u32::from(c);
    let at = CATEGORIES.partition_point(|&(start, _)| start <= code).saturating_sub(1);
    let index = CATEGORIES.get(at).map_or(0, |&(_, index)| usize::from(index));
    CATEGORY_NAMES.get(index).copied().unwrap_or("Cn")
}

/// A character's canonical combining class: 0 for a starter.
fn combining(code: u32) -> u8 {
    COMBINING
        .binary_search_by_key(&code, |&(c, _)| c)
        .map_or(0, |at| COMBINING.get(at).map_or(0, |&(_, k)| k))
}

/// What a table maps `code` to, if it's there.
fn mapped<'t>(table: &[(u32, u16, u8)], data: &'t [u32], code: u32) -> Option<&'t [u32]> {
    let at = table.binary_search_by_key(&code, |&(c, _, _)| c).ok()?;
    let &(_, start, count) = table.get(at)?;
    data.get(usize::from(start)..usize::from(start) + usize::from(count))
}

/// The text decomposed, canonically or for compatibility too, and put in canonical order.
fn decomposed(text: &str, compatible: bool) -> Vec<u32> {
    let mut out = Vec::with_capacity(text.len());
    for c in text.chars() {
        let code = u32::from(c);
        if (S_BASE..S_BASE + S_COUNT).contains(&code) {
            let index = code - S_BASE;
            out.push(L_BASE + index / N_COUNT);
            out.push(V_BASE + index % N_COUNT / T_COUNT);
            if !index.is_multiple_of(T_COUNT) {
                out.push(T_BASE + index % T_COUNT);
            }
            continue;
        }
        let found = if compatible { mapped(&COMPATIBLE, &COMPATIBLE_DATA, code) } else { None }
            .or_else(|| mapped(&CANONICAL, &CANONICAL_DATA, code));
        match found {
            Some(parts) => out.extend_from_slice(parts),
            None => out.push(code),
        }
    }
    reorder(&mut out);
    out
}

/// Puts each run of characters with a combining class other than 0 in order of their classes,
/// keeping the order of those with the same class.
fn reorder(codes: &mut [u32]) {
    let mut start = 0;
    while start < codes.len() {
        let mut end = start;
        while codes.get(end).is_some_and(|&code| combining(code) != 0) {
            end += 1;
        }
        if let Some(run) = codes.get_mut(start..end) {
            run.sort_by_key(|&code| combining(code));
        }
        start = end + 1;
    }
}

/// The character `first` and `second` compose to, if they do.
fn composed_pair(first: u32, second: u32) -> Option<u32> {
    if (L_BASE..L_BASE + L_COUNT).contains(&first) && (V_BASE..V_BASE + V_COUNT).contains(&second) {
        return Some(S_BASE + ((first - L_BASE) * V_COUNT + (second - V_BASE)) * T_COUNT);
    }
    if (S_BASE..S_BASE + S_COUNT).contains(&first)
        && (first - S_BASE).is_multiple_of(T_COUNT)
        && (T_BASE + 1..T_BASE + T_COUNT).contains(&second)
    {
        return Some(first + second - T_BASE);
    }
    let at = COMPOSE.binary_search_by_key(&(first, second), |&(a, b, _)| (a, b)).ok()?;
    COMPOSE.get(at).map(|&(_, _, composite)| composite)
}

/// Decomposed text composed again, each character joined to the starter before it where no
/// character between them blocks it.
fn composed(codes: Vec<u32>) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::with_capacity(codes.len());
    let mut starter: Option<usize> = None;
    let mut last_class = 0;
    for code in codes {
        let class = combining(code);
        if let Some(at) = starter {
            let next_to_it = out.len() == at + 1;
            if (next_to_it || (last_class != 0 && last_class < class))
                && let Some(joined) = out.get(at).and_then(|&first| composed_pair(first, code))
            {
                if let Some(slot) = out.get_mut(at) {
                    *slot = joined;
                }
                continue;
            }
        }
        if class == 0 {
            starter = Some(out.len());
        }
        last_class = class;
        out.push(code);
    }
    out
}

fn text_of(codes: &[u32]) -> String {
    codes.iter().filter_map(|&code| char::from_u32(code)).collect()
}

/// The text in normalization form NFC, NFD, NFKC or NFKD.
pub(crate) fn normalized(text: &str, compatible: bool, compose: bool) -> String {
    // Most text is ASCII, which every form leaves as it is.
    if text.is_ascii() {
        return text.to_owned();
    }
    let codes = decomposed(text, compatible);
    if compose { text_of(&composed(codes)) } else { text_of(&codes) }
}

/// The text with full case folding, for comparing text without its case.
pub(crate) fn casefolded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match mapped(&FOLDED, &FOLDED_DATA, u32::from(c)) {
            Some(parts) => out.extend(parts.iter().filter_map(|&code| char::from_u32(code))),
            None => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{casefolded, category, normalized};

    fn forms(text: &str) -> [String; 4] {
        [
            normalized(text, false, true),
            normalized(text, false, false),
            normalized(text, true, true),
            normalized(text, true, false),
        ]
    }

    #[test]
    fn the_four_forms_are_unicodes() {
        // Each from Python's `unicodedata.normalize`, as NFC, NFD, NFKC and NFKD.
        let cases = [
            ("é", ["é", "e\u{301}", "é", "e\u{301}"]),
            ("e\u{301}", ["é", "e\u{301}", "é", "e\u{301}"]),
            ("ﬁ", ["ﬁ", "ﬁ", "fi", "fi"]),
            ("한", ["한", "\u{1112}\u{1161}\u{11ab}", "한", "\u{1112}\u{1161}\u{11ab}"]),
            (
                "\u{1100}\u{1161}\u{11a8}",
                ["각", "\u{1100}\u{1161}\u{11a8}", "각", "\u{1100}\u{1161}\u{11a8}"],
            ),
            // Combining marks put in order, the dot below before the dot above.
            ("q\u{307}\u{323}", ["q\u{323}\u{307}", "q\u{323}\u{307}", "q\u{323}\u{307}", "q\u{323}\u{307}"]),
            ("ṩ", ["ṩ", "s\u{323}\u{307}", "ṩ", "s\u{323}\u{307}"]),
            // Excluded from composing, so NFC leaves it decomposed.
            ("\u{958}", ["\u{915}\u{93c}", "\u{915}\u{93c}", "\u{915}\u{93c}", "\u{915}\u{93c}"]),
            ("Å", ["Å", "A\u{30a}", "Å", "A\u{30a}"]),
            ("①", ["①", "①", "1", "1"]),
            ("fox", ["fox", "fox", "fox", "fox"]),
        ];
        for (text, want) in cases {
            assert_eq!(forms(text), want.map(str::to_owned), "{text:?}");
        }
    }

    #[test]
    fn categories_and_folding_are_unicodes() {
        for (c, want) in [
            ('A', "Lu"),
            ('a', "Ll"),
            ('5', "Nd"),
            (' ', "Zs"),
            ('é', "Ll"),
            ('😀', "So"),
            ('\u{301}', "Mn"),
            ('\u{e000}', "Co"),
            ('\u{378}', "Cn"),
        ] {
            assert_eq!(category(c), want, "{c:?}");
        }
        assert_eq!(casefolded("Straße ΣΑΣ İ"), "strasse σασ i\u{307}");
    }
}
