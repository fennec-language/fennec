//! Each range form gives what slicing the text and then calling gives, the slice made the way
//! compiled code makes one (`fennec_str_slice`, then the header of what it made), over texts held
//! inline and in a block, ASCII and not, and ranges empty, reversed, at either end, negative,
//! missing, past the text and on every side of a character of several bytes.

use std::rc::Rc;

use super::super::super::{Tables, finish, heap, start, strings, with};
use super::{equals_range, find_range, split_range, starts_with_range};
use crate::packed::Element;
use crate::value::Value;

const MISSING: i64 = i64::MIN;

fn begin() {
    let text = Rc::new(Element::Text);
    let tables = Tables {
        shapes: vec![Rc::new(Element::Str)],
        elements: vec![text],
        text_headers: true,
        ..Tables::default()
    };
    start(tables, crate::Inputs::default());
}

fn header(text: &str) -> [u64; 3] {
    with(|state| Some(super::super::header(state, text))).unwrap()
}

fn text_of(words: [u64; 3]) -> String {
    with(|state| Some(super::super::text_of(state, &words))).unwrap()
}

fn live() -> usize {
    with(|state| state.heap.live())
}

/// Lets go of a header's block, if it has one.
fn let_go(words: [u64; 3]) {
    if let Some(block) = super::super::block_of(&words) {
        with(|state| heap::release(state, block));
    }
}

/// The header of `text[from:to]`, made as compiled code makes it: `fennec_str_slice` on the text's
/// block, then its header, inline when it fits.
fn sliced(text: &str, from: i64, to: i64) -> [u64; 3] {
    let block = with(|state| Some(heap::text_address(state, text))).unwrap();
    let part = strings::slice(block, from, to, 0, 0);
    with(|state| {
        heap::release(state, block.cast_unsigned());
        Some(())
    });
    super::super::of(part).words
}

/// A list of strings' texts, then let go of.
fn items(list: i64) -> Vec<String> {
    let read = with(|state| Some(heap::read_block(state, list.cast_unsigned()))).unwrap();
    with(|state| {
        heap::release(state, list.cast_unsigned());
        Some(())
    });
    let Value::List(items) = read else { panic!("not a list: {read:?}") };
    items
        .iter()
        .map(|item| match item {
            Value::Str(text) => text.to_string(),
            other => panic!("not a string: {other:?}"),
        })
        .collect()
}

const TEXTS: [&str; 6] = [
    "",
    "short",
    "naïve, ok",
    "a text long enough for a block, ASCII only",
    "naïve café, ünïcödé and more text: long enough for a block",
    "日本語のテキストも長くするとブロックになる",
];

// Parts of up to eight bytes are looked for where the text is, and longer ones in a copy.
const PARTS: [&str; 9] = ["", " ", "é", "a", "text", "ünï", "日本", "and more text", "テキストも"];

/// Every range worth trying on a text of `chars` characters.
fn ranges(chars: i64) -> Vec<(i64, i64)> {
    let mut ranges = vec![
        (MISSING, MISSING),
        (0, 0),
        (0, 1),
        (1, MISSING),
        (MISSING, -1),
        (-3, MISSING),
        (MISSING, -3),
        (3, 1),
        (-100, 100),
        (100, 200),
        (2, 7),
        (5, 5),
        (chars, MISSING),
        (MISSING, chars),
    ];
    // Each character boundary, as a start and as an end.
    for at in 0..=chars {
        ranges.push((at, MISSING));
        ranges.push((MISSING, at));
        ranges.push((at, (at + 2).min(chars)));
    }
    ranges
}

#[test]
fn a_range_reads_what_its_slice_holds_for_every_call() {
    begin();
    let before = live();
    for text in TEXTS {
        let whole = header(text);
        let chars = i64::try_from(text.chars().count()).unwrap();
        for (from, to) in ranges(chars) {
            let slice = sliced(text, from, to);
            let what = format!("{text:?}[{from}:{to}] = {:?}", text_of(slice));
            for part in PARTS {
                let other = header(part);
                assert_eq!(
                    find_range(whole[0], whole[1], whole[2], from, to, other[0], other[1], other[2]),
                    super::super::find_header(slice[0], slice[1], slice[2], other[0], other[1], other[2]),
                    "find {part:?} in {what}"
                );
                assert_eq!(
                    starts_with_range(whole[0], whole[1], whole[2], from, to, other[0], other[1], other[2]),
                    super::super::starts_with(slice[0], slice[1], slice[2], other[0], other[1], other[2]),
                    "startswith {part:?} of {what}"
                );
                assert_eq!(
                    equals_range(whole[0], whole[1], whole[2], from, to, other[0], other[1], other[2]) == 1,
                    text_of(slice) == part,
                    "{what} == {part:?}"
                );
                let_go(other);
            }
            // Equal to its own slice, which no part above may be.
            let itself = header(&text_of(slice));
            assert_eq!(
                equals_range(whole[0], whole[1], whole[2], from, to, itself[0], itself[1], itself[2]),
                1,
                "{what}"
            );
            let_go(itself);
            // None splits by whitespace, and one over eight bytes is split in a copy; the rest are split
            // where the text is.
            for separator in [None, Some(" "), Some(", "), Some("é"), Some("ünïcödé"), Some("ロック")]
            {
                let between = separator.map_or([0; 3], header);
                let ranged = split_range(
                    whole[0], whole[1], whole[2], from, to, between[0], between[1], between[2], 0, 0,
                );
                let after = strings::split_header(
                    slice[0], slice[1], slice[2], between[0], between[1], between[2], 0, 0,
                );
                assert_eq!(items(ranged), items(after), "split {separator:?} of {what}");
                let_go(between);
            }
            let_go(slice);
        }
        let_go(whole);
    }
    // Nothing a range read was made a string, and nothing is left.
    assert_eq!(live(), before);
    finish();
}

#[test]
fn a_range_made_no_string_for_its_slice() {
    begin();
    let whole = header("a text long enough for a block of its own, and then some");
    let space = header(" ");
    let before = live();
    assert_eq!(find_range(whole[0], whole[1], whole[2], 2, 30, space[0], space[1], space[2]), 4);
    assert_eq!(starts_with_range(whole[0], whole[1], whole[2], 2, 30, space[0], space[1], space[2]), 0);
    assert_eq!(live(), before, "a find or a startswith on a range makes nothing at all");
    let list = split_range(whole[0], whole[1], whole[2], 2, 30, space[0], space[1], space[2], 0, 0);
    assert_eq!(items(list), vec!["text", "long", "enough", "for", "a", "block"]);
    assert_eq!(live(), before, "a split makes only its list, whose short parts are inline");
    let_go(whole);
    finish();
}

/// A freed list of strings, as `split` makes, lets go of each long part it held, freeing those it
/// was the last owner of and leaving one held elsewhere. It's freed without walking it as any
/// list, by its strings' headers.
#[test]
fn a_freed_split_lets_go_of_its_long_parts() {
    begin();
    let kept = "a part long enough for a block of its own";
    let whole = header(&format!("{kept},short,another part long enough for one"));
    let comma = header(",");
    let before = live();
    let list = strings::split_header(whole[0], whole[1], whole[2], comma[0], comma[1], comma[2], 0, 0);
    // The list and its two long parts.
    assert_eq!(live(), before + 3);
    let read = with(|state| Some(heap::read_block(state, list.cast_unsigned()))).unwrap();
    let Value::List(parts) = read else { panic!("not a list: {read:?}") };
    assert_eq!(parts.len(), 3);
    // The first part is held elsewhere too, as a program keeping it would.
    let first = with(|state| {
        let block = state.heap.block(list.cast_unsigned())?;
        let header = <[u64; 3]>::try_from(block.get(4..7)?).ok()?;
        let first = super::super::block_of(&header)?;
        heap::retain(state, first);
        Some(first)
    })
    .unwrap();
    heap::free(list);
    assert_eq!(live(), before + 1, "the list and the part it alone held are freed");
    assert_eq!(text_of([first, u64::try_from(kept.len()).unwrap(), 0]), kept);
    with(|state| {
        heap::release(state, first);
        Some(())
    });
    assert_eq!(live(), before);
    let_go(whole);
    let_go(comma);
    let (_, failure, _) = finish();
    assert!(failure.is_none(), "{failure:?}");
}
