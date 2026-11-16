//! Dicts of values in place changed under a lock's log, where the words that hold blocks differ
//! between what was there and what goes in, and where the dict has to move. Each is ended once by
//! a crash, which has to put everything back, and once normally, which keeps it. From the review
//! of the change that made dicts hold values in place.

use super::*;
use crate::embed::guarded::rollback::{begin, kept, save_ref, to_change, undo, undo_word};
use crate::embed::guarded::{shared_enter, shared_leave, shared_new};
use crate::embed::lists::{self, LIST_HEADER_WORDS};

/// Where a `Holder`'s dict is among its words.
const TABLE_AT: usize = heap::HEADER_WORDS;

fn oops() -> Element {
    named("Oops", vec![("code", Element::Int), ("why", Element::Text)])
}

/// `Result[Point, Oops]` laid out as an enum is: the case, then that case's one field.
fn result_of() -> Element {
    Element::Enum {
        name: Rc::from("Result[Point, Oops]"),
        cases: vec![
            (Rc::from("Ok"), vec![(Rc::from("value"), point())]),
            (Rc::from("Err"), vec![(Rc::from("error"), oops())]),
        ],
        words: 6,
    }
}

/// A run whose shapes are a `Holder` of one dict, int keys and values in place of kind `element`,
/// and the string's: for a lock's cell. Kinds 0 to 3 are a `Point`, a tuple, an optional `Point`
/// and a result.
fn started_holding(element: u32) {
    let pair = named("(int, str)", vec![("0", Element::Int), ("1", Element::Text)]);
    let none: Vec<(Rc<str>, Element)> = Vec::new();
    let maybe = Element::Enum {
        name: Rc::from("Option[Point]"),
        cases: vec![(Rc::from("None"), none), (Rc::from("Some"), vec![(Rc::from("value"), point())])],
        words: 6,
    };
    let table = shape(1, Word::Int, Word::InPlace(element)).unwrap();
    let holder = named("Holder", vec![("table", Element::Boxed(usize::try_from(table).unwrap()))]);
    let tables = Tables {
        shapes: vec![Rc::new(holder), Rc::new(Element::Str)],
        elements: vec![Rc::new(point()), Rc::new(pair), Rc::new(maybe), Rc::new(result_of())],
        str_shape: 1,
        text_headers: true,
        ..Tables::default()
    };
    start(tables, crate::Inputs::default());
}

fn made_held(element: u32, value: &Value) -> u64 {
    made_held_past(1, element, value)
}

/// `made_held`, in a run whose string's shape is number `strings`.
fn made_held_past(strings: u64, element: u32, value: &Value) -> u64 {
    let shape = shape(strings, Word::Int, Word::InPlace(element)).unwrap();
    with(|state| super::super::write(state, shape, value).and_then(|at| u64::try_from(at).ok())).unwrap()
}

fn word(block: u64, at: usize) -> u64 {
    with(|state| state.heap.block(block).and_then(|words| words.get(at).copied())).unwrap_or(0)
}

fn set_word(block: u64, at: usize, value: u64) {
    with(|state| {
        if let Some(place) = state.heap.block_mut(block).and_then(|words| words.get_mut(at)) {
            *place = value;
        }
    });
}

fn blocks() -> usize {
    with(|state| state.heap.blocks.len())
}

/// A `shared` value holding a dict, locked to write with its log begun and the dict marked, as
/// native code's `lock x as ref s:` leaves it before `s.table[key] = value`. Gives its cell and the
/// dict as it's held inside.
fn locked(dict: u64) -> (u64, u64) {
    let cell = unsigned(heap::alloc(0));
    set_word(cell, TABLE_AT, dict);
    shared_new(signed(cell));
    shared_enter(signed(cell), 1);
    begin(signed(cell));
    let inside = word(cell, TABLE_AT);
    save_ref(signed(cell), i64::try_from(TABLE_AT).unwrap(), signed(inside));
    (cell, inside)
}

/// What native code does on a crash: the log's words go back, then the cell's.
fn crash(cell: u64) {
    undo();
    let back = undo_word(i64::try_from(TABLE_AT).unwrap(), signed(word(cell, TABLE_AT)), 1);
    set_word(cell, TABLE_AT, unsigned(back));
}

fn buffer_as(element: &Element, value: &Value) {
    with(|state| {
        let mut words = Vec::new();
        heap::write(state, element, value, &mut words)?;
        super::super::value_buffer_of(state, |buffer| {
            for (kept, word) in buffer.iter().zip(&words) {
                kept.set(*word);
            }
        });
        Some(())
    })
    .unwrap();
}

fn maybe_element() -> Element {
    with(|state| state.tables.elements.get(2).map(|element| element.as_ref().clone())).unwrap()
}

fn an_oops(code: i64, why: &str) -> Value {
    Value::Instance(Rc::new(Instance {
        type_name: Rc::from("Oops"),
        case: None,
        fields: vec![(Rc::from("code"), Value::Int(code)), (Rc::from("why"), Value::str(why))],
        is_error: false,
        cause: None,
        notes: None,
    }))
}

#[test]
fn an_optional_replaced_under_a_lock_by_its_other_case_goes_back_on_a_crash_and_stays_at_an_end() {
    // None, which holds nothing, replaced by Some, which holds two blocks; and the other way.
    for (crashes, first, second) in [
        (true, Value::None, a_point(1, LONG, &[1])),
        (false, Value::None, a_point(1, LONG, &[1])),
        (true, a_point(1, LONG, &[1]), Value::None),
        (false, a_point(1, LONG, &[1]), Value::None),
    ] {
        started_holding(2);
        let start_with = dict_of(vec![(Value::Int(5), first.clone())]);
        let (cell, inside) = locked(made_held(2, &start_with));
        let before = blocks();
        buffer_as(&maybe_element(), &second);
        assert_eq!(unsigned(super::super::set_in_place(signed(inside), 5)), inside);
        if crashes {
            crash(cell);
            assert_eq!(read(word(cell, TABLE_AT)), start_with, "crash {first:?} -> {second:?}");
            assert_eq!(blocks(), before, "crash {first:?} -> {second:?}");
        } else {
            kept();
            assert_eq!(read(word(cell, TABLE_AT)), dict_of(vec![(Value::Int(5), second.clone())]));
            let change: isize = if second == Value::None { -2 } else { 2 };
            assert_eq!(blocks().cast_signed(), before.cast_signed() + change, "kept {first:?} -> {second:?}");
        }
        shared_leave(signed(cell), 1);
        finish();
    }
}

#[test]
fn a_result_replaced_under_a_lock_by_its_other_case_goes_back_on_a_crash_and_stays_at_an_end() {
    // `Ok` holds a name's block where `Err` holds a short reason's own bytes, and a list where
    // `Err` holds nothing: the words that hold blocks differ between what's there and what goes in.
    let ok = || Value::Ok(Rc::new(a_point(1, LONG, &[1, 2])));
    let short = || Value::Err(Rc::new(an_oops(404, "short")));
    let long = || Value::Err(Rc::new(an_oops(500, LONG)));
    for crashes in [true, false] {
        for (first, second) in [(ok(), short()), (short(), ok()), (ok(), long()), (long(), ok())] {
            started_holding(3);
            let element = with(|state| state.tables.elements.get(3).map(|e| e.as_ref().clone())).unwrap();
            let start_with = dict_of(vec![(Value::Int(5), first.clone())]);
            let (cell, inside) = locked(made_held(3, &start_with));
            let before = blocks();
            buffer_as(&element, &second);
            assert_eq!(unsigned(super::super::set_in_place(signed(inside), 5)), inside);
            if crashes {
                crash(cell);
                assert_eq!(read(word(cell, TABLE_AT)), start_with, "crash {first:?} -> {second:?}");
                assert_eq!(blocks(), before, "crash {first:?} -> {second:?}");
            } else {
                kept();
                assert_eq!(
                    read(word(cell, TABLE_AT)),
                    dict_of(vec![(Value::Int(5), second.clone())]),
                    "kept {first:?} -> {second:?}"
                );
            }
            assert_eq!(word(word(cell, TABLE_AT), 0), 1);
            shared_leave(signed(cell), 1);
            finish();
        }
    }
}

#[test]
fn a_pop_then_a_set_of_the_same_key_under_one_lock_go_back_on_a_crash_and_stay_at_an_end() {
    for crashes in [true, false] {
        started_holding(0);
        let (cell, inside) = locked(made_held(0, &points()));
        let before = blocks();
        assert_eq!(unsigned(super::super::pop_in_place(signed(inside), 7, 0, 0)), inside);
        let_go_of_buffered(&point(), 5);
        let again = a_point(70, LONG, &[7]);
        buffer(&again);
        let after = unsigned(super::super::set_in_place(signed(inside), 7));
        set_word(cell, TABLE_AT, after);
        if crashes {
            crash(cell);
            assert_eq!(read(word(cell, TABLE_AT)), points());
            assert_eq!(blocks(), before);
        } else {
            kept();
            assert_eq!(
                read(word(cell, TABLE_AT)),
                dict_of(vec![
                    (Value::Int(-3), a_point(2, LONG, &[])),
                    (Value::Int(40), a_point(3, LONG, &[9])),
                    (Value::Int(7), again),
                ])
            );
        }
        shared_leave(signed(cell), 1);
        finish();
    }
}

#[test]
fn a_dict_that_has_to_grow_under_a_lock_goes_back_on_a_crash_and_stays_at_an_end() {
    for crashes in [true, false] {
        started_holding(0);
        let (cell, inside) = locked(made_held(0, &points()));
        let before = blocks();
        let mut now = inside;
        let mut wanted = vec![
            (Value::Int(7), a_point(1, "short", &[1, 2])),
            (Value::Int(-3), a_point(2, LONG, &[])),
            (Value::Int(40), a_point(3, LONG, &[9])),
        ];
        for key in 100..130_i64 {
            let value = a_point(key, if key % 2 == 0 { LONG } else { "s" }, &[key]);
            buffer(&value);
            let after = unsigned(super::super::set_in_place(signed(now), key));
            if after != now {
                // As native code does with what a set gives back: saved, then written to the field.
                save_ref(signed(cell), i64::try_from(TABLE_AT).unwrap(), signed(after));
                set_word(cell, TABLE_AT, after);
                now = after;
            }
            wanted.push((Value::Int(key), value));
        }
        assert_ne!(now, inside, "thirty more keys don't fit where three were");
        if crashes {
            crash(cell);
            assert_eq!(word(cell, TABLE_AT), inside);
            assert_eq!(read(inside), points());
            assert_eq!(blocks(), before, "every copy and every value made under the lock is gone");
        } else {
            kept();
            assert_eq!(read(word(cell, TABLE_AT)), dict_of(wanted));
        }
        assert_eq!(word(word(cell, TABLE_AT), 0), 1, "the dict is held once, by the value");
        shared_leave(signed(cell), 1);
        finish();
    }
}

/// The address of the entry `key` is at in `dict`, as native code holds it after a lookup, and
/// the word its value starts at.
fn entry_of(dict: u64, key: u64) -> (i64, usize) {
    with(|state| {
        let block = state.heap.block(dict)?;
        let layout = super::super::layout(state, block)?;
        let hash = super::super::hash_key(state, layout.key, &[key], super::super::Hashing::of(block));
        let super::super::Probe::Found(entry) = super::super::find(state, block, layout, &[key], hash) else {
            return None;
        };
        let mask = usize::try_from(*block.get(4)?).ok()?;
        let word = super::super::DICT_HEADER_WORDS + mask + 1 + entry * layout.width();
        Some((signed(dict + 8 * u64::try_from(word).ok()?), word + layout.value_at()))
    })
    .unwrap()
}

/// What native code does to a value it changes where it is: writes each new word, which hands the
/// entry the owners the new words hold, and lets go of what the old words held.
fn changed_in_place(dict: u64, place: usize, to: &Value) {
    with(|state| {
        let element = point();
        let mut new = Vec::new();
        heap::write(state, &element, to, &mut new)?;
        let old = state.heap.block(dict)?.get(place..place + new.len())?.to_vec();
        state.heap.block_mut(dict)?.get_mut(place..place + new.len())?.copy_from_slice(&new);
        let mut held = Vec::new();
        heap::held_addresses(&element, &old, &mut held);
        for address in held {
            heap::release(state, address);
        }
        Some(())
    })
    .unwrap();
}

/// A value held in the entry of a marked dict, changed where it is by native code: the entry is
/// saved first, with `fennec_lock_save_entry`. A crash puts the value back as it was, with what
/// it held, and a normal end keeps the change and lets what it held go. Saving the entry again,
/// as native code does at every take of the value, saves nothing more. An address that's no
/// entry of the dict, and a dict no log marked, save nothing.
#[test]
fn a_value_changed_in_its_entry_under_a_lock_goes_back_on_a_crash_and_stays_at_an_end() {
    for crashes in [true, false] {
        started_holding(0);
        let start_with = dict_of(vec![
            (Value::Int(5), a_point(1, LONG, &[1, 2])),
            (Value::Int(6), a_point(2, "short", &[])),
        ]);
        let after = a_point(99, "another name long enough for a block", &[7, 8, 9]);
        let then = a_point(100, "short", &[]);
        let (cell, inside) = locked(made_held(0, &start_with));
        let before = blocks();
        let (entry, place) = entry_of(inside, 5);
        // Neither of these is an entry to save: nothing changes, and nothing is held for them.
        super::super::save_entry(signed(inside), entry + 8);
        super::super::save_entry(signed(inside), 0);
        super::super::save_entry(signed(inside), entry);
        changed_in_place(inside, place, &after);
        // Taken again to change again: what's there now was made under the lock.
        super::super::save_entry(signed(inside), entry);
        changed_in_place(inside, place, &then);
        if crashes {
            crash(cell);
            assert_eq!(read(word(cell, TABLE_AT)), start_with);
            assert_eq!(blocks(), before);
        } else {
            kept();
            let changed =
                dict_of(vec![(Value::Int(5), then.clone()), (Value::Int(6), a_point(2, "short", &[]))]);
            assert_eq!(read(word(cell, TABLE_AT)), changed);
            // The long name and the list the value held at first are gone, and it holds an
            // empty list now.
            assert_eq!(blocks(), before - 1);
        }
        audited_behind(cell);
        shared_leave(signed(cell), 1);
        finish();
    }
}

/// Which of a `Point`'s words in place holds its list, `near`: after its number and its name's
/// header.
const NEAR_IN_A_POINT: usize = 4;

/// How many times a shared value has been copied so that one owner could change it.
fn copies() -> u64 {
    with(|state| state.heap.counts.copies)
}

/// Locks `cell` to write again, as `locked` leaves a new one, and gives the dict as it's held
/// inside.
fn locked_again(cell: u64) -> u64 {
    shared_enter(signed(cell), 1);
    begin(signed(cell));
    let inside = word(cell, TABLE_AT);
    save_ref(signed(cell), i64::try_from(TABLE_AT).unwrap(), signed(inside));
    inside
}

/// What native code does to add `item` to the list in the `Point` at `entry`: saves the entry,
/// takes the list out of it, asks the runtime for an item's room, fills it, and puts the list back
/// where it now is.
fn added_in_its_entry(dict: u64, entry: i64, place: usize, item: u64) {
    super::super::save_entry(signed(dict), entry);
    let list = word(dict, place + NEAR_IN_A_POINT);
    let grown = unsigned(lists::grow(signed(list)));
    let length = usize::try_from(word(grown, 2)).unwrap();
    set_word(grown, LIST_HEADER_WORDS + length - 1, item);
    set_word(dict, place + NEAR_IN_A_POINT, grown);
}

/// A list in a value held in its entry, added to under one lock after another, an item a lock.
/// The save marks the list, so each item goes on where the list is, which moves when it needs
/// room, and nothing is copied. A lock that adds a hundred more and crashes leaves it as it was
/// before that lock, though it moved under it.
///
/// The save once took an owner of the list for the log, so every lock's first append copied the
/// whole list: a copy a lock, each as long as the list had got.
#[test]
fn a_list_in_a_value_in_its_entry_is_added_to_where_it_is_under_lock_after_lock() {
    started_holding(0);
    let with_near = |near: &[i64]| {
        dict_of(vec![(Value::Int(5), a_point(1, LONG, near)), (Value::Int(6), a_point(2, "short", &[]))])
    };
    let (cell, mut inside) = locked(made_held(0, &with_near(&[1, 2])));
    let (before, copied) = (blocks(), copies());
    for item in 3..=40 {
        let (entry, place) = entry_of(inside, 5);
        added_in_its_entry(inside, entry, place, item);
        kept();
        shared_leave(signed(cell), 1);
        inside = locked_again(cell);
    }
    let all: Vec<i64> = (1..=40).collect();
    assert_eq!(copies(), copied, "a lock's append copied the list");
    assert_eq!(read(word(cell, TABLE_AT)), with_near(&all));
    assert_eq!(blocks(), before);
    let (entry, place) = entry_of(inside, 5);
    let list_before = word(inside, place + NEAR_IN_A_POINT);
    for item in 41..=140 {
        added_in_its_entry(inside, entry, place, item);
    }
    assert_ne!(word(inside, place + NEAR_IN_A_POINT), list_before, "the list didn't have to move");
    crash(cell);
    assert_eq!(read(word(cell, TABLE_AT)), with_near(&all));
    assert_eq!(copies(), copied);
    assert_eq!(blocks(), before);
    audited_behind(cell);
    shared_leave(signed(cell), 1);
    finish();
}

/// A run whose shapes are a `Holder` of one dict, a `Tally` and the string's. The dict has int
/// keys and holds `Kept`s in place, each a number and a `Tally` in a block of its own.
fn started_holding_a_tally() {
    let tally = named("Tally", vec![("count", Element::Int), ("seen", Element::List(Rc::new(Element::Int)))]);
    let kept_one = named("Kept", vec![("id", Element::Int), ("tally", Element::Boxed(1))]);
    let table = shape(2, Word::Int, Word::InPlace(0)).unwrap();
    let holder = named("Holder", vec![("table", Element::Boxed(usize::try_from(table).unwrap()))]);
    let tables = Tables {
        shapes: vec![Rc::new(holder), Rc::new(tally), Rc::new(Element::Str)],
        elements: vec![Rc::new(kept_one)],
        str_shape: 2,
        text_headers: true,
        ..Tables::default()
    };
    start(tables, crate::Inputs::default());
}

fn a_kept(id: i64, count: i64, seen: &[i64]) -> Value {
    let instance = |name: &str, fields: Vec<(Rc<str>, Value)>| {
        Value::Instance(Rc::new(Instance {
            type_name: Rc::from(name),
            case: None,
            fields,
            is_error: false,
            cause: None,
            notes: None,
        }))
    };
    let tally = instance(
        "Tally",
        vec![
            (Rc::from("count"), Value::Int(count)),
            (Rc::from("seen"), Value::list(seen.iter().map(|n| Value::Int(*n)).collect())),
        ],
    );
    instance("Kept", vec![(Rc::from("id"), Value::Int(id)), (Rc::from("tally"), tally)])
}

/// A struct held by a block of its own, in a value held in its entry. The save marks the block
/// and saves its fields, and marks the list it holds, so native code changes a field and adds to
/// the list where they are, with nothing copied. A crash puts both back, and a normal end keeps
/// them.
///
/// With an owner of the log's on the struct's block, native code's ask before the change copied it.
#[test]
fn a_struct_s_block_in_a_value_in_its_entry_is_changed_where_it_is_under_a_lock() {
    for crashes in [true, false] {
        started_holding_a_tally();
        let start_with = dict_of(vec![(Value::Int(5), a_kept(1, 10, &[1, 2]))]);
        let (cell, inside) = locked(made_held_past(2, 0, &start_with));
        let (before, copied) = (blocks(), copies());
        let (entry, place) = entry_of(inside, 5);
        super::super::save_entry(signed(inside), entry);
        // The tally's block is the value's second word. Native code asks before it changes a
        // struct's block it doesn't hold alone: a marked one stays where it is.
        let tally = word(inside, place + 1);
        assert_eq!(unsigned(to_change(signed(tally))), tally, "the struct's block was copied");
        set_word(tally, heap::HEADER_WORDS, 11);
        let seen = word(tally, heap::HEADER_WORDS + 1);
        let grown = unsigned(lists::grow(signed(seen)));
        set_word(grown, LIST_HEADER_WORDS + 2, 3);
        set_word(tally, heap::HEADER_WORDS + 1, grown);
        // Taken again to change again: nothing more is saved, and it's still changed in place.
        super::super::save_entry(signed(inside), entry);
        assert_eq!(unsigned(to_change(signed(tally))), tally);
        set_word(tally, heap::HEADER_WORDS, 12);
        assert_eq!(copies(), copied, "crashes: {crashes}");
        if crashes {
            crash(cell);
            assert_eq!(read(word(cell, TABLE_AT)), start_with);
        } else {
            kept();
            let changed = dict_of(vec![(Value::Int(5), a_kept(1, 12, &[1, 2, 3]))]);
            assert_eq!(read(word(cell, TABLE_AT)), changed);
        }
        assert_eq!(blocks(), before, "crashes: {crashes}");
        audited_behind(cell);
        shared_leave(signed(cell), 1);
        finish();
    }
}

/// Every block behind the lock is counted as many times as it's held, by the audit's own count.
/// The cell's own count is its lock's to keep, another way.
fn audited_behind(cell: u64) {
    let found = with(|state| heap::audit::audit(state, &[cell]));
    let wrong: Vec<_> = found.wrong.into_iter().filter(|finding| finding.address != cell).collect();
    assert_eq!(wrong, Vec::new());
}

/// A dict no log marked isn't saved: native code copies one that's shared before it changes it,
/// and one that's alone and unmarked is changed with nothing to put back.
#[test]
fn an_entry_of_a_dict_no_log_marked_is_not_saved() {
    started();
    let dict = made(Word::Int, 0, &points());
    let before = live();
    let (entry, _) = entry_of(dict, 7);
    super::super::save_entry(signed(dict), entry);
    assert_eq!(live(), before);
    assert_eq!(read(dict), points());
    audited(&[dict]);
    let_go(dict);
    finish();
}
