//! Dicts of values in place changed under a lock's log, where the words that hold blocks differ
//! between what was there and what goes in, and where the dict has to move. Each is ended once by
//! a crash, which has to put everything back, and once normally, which keeps it. From the review
//! of the change that made dicts hold values in place.

use super::*;
use crate::embed::guarded::rollback::{begin, kept, save_ref, undo, undo_word};
use crate::embed::guarded::{shared_enter, shared_leave, shared_new};

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
    let shape = shape(1, Word::Int, Word::InPlace(element)).unwrap();
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
