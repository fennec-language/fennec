//! Lists whose items are structs laid out in place that hold blocks, like a string field.

use std::rc::Rc;

use super::super::{Tables, finish, heap, start, with};
use crate::packed::Element;
use crate::value::{Instance, Value};

/// A list of `Unit { name: str, hp: int }`, laid out in place, two words an item.
fn units() -> Rc<Element> {
    let fields = vec![(Rc::from("name"), Element::Str), (Rc::from("hp"), Element::Int)];
    Rc::new(Element::Struct { name: Rc::from("Unit"), fields })
}

fn unit(name: &str, hp: i64) -> Value {
    let fields = vec![(Rc::from("name"), Value::str(name)), (Rc::from("hp"), Value::Int(hp))];
    Value::Instance(Rc::new(Instance {
        type_name: Rc::from("Unit"),
        case: None,
        fields,
        is_error: false,
        cause: None,
        notes: None,
    }))
}

fn begin() {
    let tables = Tables { shapes: vec![Rc::new(Element::Str)], elements: vec![units()], ..Tables::default() };
    start(tables, crate::Inputs::default());
}

/// A new list owned once of these units, numbered from 1, made the way native code would get one.
fn list(names: &[&str]) -> i64 {
    numbered(names, 1)
}

fn numbered(names: &[&str], first: i64) -> i64 {
    let items = names.iter().zip(first..).map(|(name, hp)| unit(name, hp)).collect();
    let value = with(|state| Some(state.push(Value::list(items)))).unwrap();
    super::from_value(value, 0, 0, 0)
}

fn read(list: i64) -> Value {
    with(|state| Some(heap::read_block(state, u64::try_from(list).unwrap()).unpacked())).unwrap()
}

fn live() -> usize {
    with(|state| state.heap.live())
}

#[test]
fn a_list_of_structs_holding_strings_owns_them() {
    begin();
    let before = live();
    let all = list(&["archer one", "medic two", "soldier three"]);
    assert_ne!(all, 0);
    assert_eq!(
        read(all),
        Value::list(vec![unit("archer one", 1), unit("medic two", 2), unit("soldier three", 3)])
    );
    // A slice holds its own owner of each name, so the names outlive the list sliced.
    let tail = super::slice(all, 1, i64::MIN);
    heap::free(all);
    assert_eq!(read(tail), Value::list(vec![unit("medic two", 2), unit("soldier three", 3)]));
    // Equal names in different blocks make equal lists.
    let again = numbered(&["medic two", "soldier three"], 2);
    assert_eq!(heap::equal(tail, again), 1);
    let other = numbered(&["medic two", "soldier four"], 2);
    assert_eq!(heap::equal(tail, other), 0);
    // Taking an item out lets go of its name, and clearing lets go of the rest.
    super::remove_at(tail, tail + 8 * i64::try_from(super::LIST_HEADER_WORDS).unwrap());
    assert_eq!(read(tail), Value::list(vec![unit("soldier three", 3)]));
    super::clear(tail);
    assert_eq!(read(tail), Value::list(Vec::new()));
    for done in [tail, again, other] {
        heap::free(done);
    }
    assert_eq!(live(), before);
    finish();
}

#[test]
fn a_shared_list_of_structs_holding_strings_is_copied_with_its_names() {
    begin();
    let before = live();
    let all = list(&["c", "a long name for a unit", "b"]);
    with(|state| heap::retain(state, u64::try_from(all).unwrap()));
    // Changing a shared list copies it first, and the copy holds the names too.
    let copy = super::unshare(all);
    assert_ne!(copy, all);
    heap::free(all);
    assert_eq!(read(copy), Value::list(vec![unit("c", 1), unit("a long name for a unit", 2), unit("b", 3)]));
    heap::free(copy);
    assert_eq!(live(), before);
    finish();
}

#[test]
fn items_holding_a_string_in_an_enum_case_own_it_only_in_that_case() {
    // `Tag { label: Option[str] }`, the optional laid out as an enum: only `Some` holds a block.
    let cases =
        vec![(Rc::from("None"), Vec::new()), (Rc::from("Some"), vec![(Rc::from("value"), Element::Str)])];
    let option = Element::Enum { name: Rc::from("Option[str]"), cases, words: 2 };
    let tag = Rc::new(Element::Struct { name: Rc::from("Tag"), fields: vec![(Rc::from("label"), option)] });
    let tables = Tables { shapes: vec![Rc::new(Element::Str)], elements: vec![tag], ..Tables::default() };
    start(tables, crate::Inputs::default());
    let tagged = |label: Value| {
        let fields = vec![(Rc::from("label"), label)];
        Value::Instance(Rc::new(Instance {
            type_name: Rc::from("Tag"),
            case: None,
            fields,
            is_error: false,
            cause: None,
            notes: None,
        }))
    };
    let before = live();
    let items =
        vec![tagged(Value::str("a label longer than a word")), tagged(Value::None), tagged(Value::str("b"))];
    let value = with(|state| Some(state.push(Value::list(items.clone())))).unwrap();
    let all = super::from_value(value, 0, 0, 0);
    assert_eq!(read(all), Value::list(items.clone()));
    let tail = super::slice(all, 1, i64::MIN);
    heap::free(all);
    assert_eq!(read(tail), Value::list(items.get(1..).unwrap().to_vec()));
    heap::free(tail);
    assert_eq!(live(), before);
    finish();
}

#[test]
fn a_struct_item_handed_over_as_a_block_is_found_by_its_fields() {
    let fields = vec![(Rc::from("name"), Element::Str), (Rc::from("hp"), Element::Int)];
    let shape = Rc::new(Element::Struct { name: Rc::from("Unit"), fields });
    let tables = Tables {
        shapes: vec![Rc::clone(&shape), Rc::new(Element::Str)],
        str_shape: 1,
        elements: vec![shape],
        ..Tables::default()
    };
    start(tables, crate::Inputs::default());
    let before = live();
    let all = list(&["archer", "medic", "soldier"]);
    // As native code hands one over: a block of the item's shape with equal fields.
    let wanted = with(|state| Some(state.push(unit("medic", 2)))).unwrap();
    let wanted = heap::from_value(wanted, 0, 0, 0);
    assert_eq!(super::contains(all, wanted), 1);
    super::remove(all, wanted, 0, 0);
    assert_eq!(read(all), Value::list(vec![unit("archer", 1), unit("soldier", 3)]));
    assert_eq!(super::contains(all, wanted), 0);
    heap::free(wanted);
    heap::free(all);
    assert_eq!(live(), before);
    finish();
}

#[test]
fn a_buffer_reserves_room_and_truncates_letting_go_of_what_it_drops() {
    begin();
    let before = live();
    let all = list(&["archer", "medic", "soldier"]);
    let room = |list: i64| with(|state| state.heap.block(u64::try_from(list).ok()?)?.get(3).copied());
    let reserved = super::reserve(all, 20);
    assert_eq!(room(reserved), Some(20));
    assert_eq!(super::reserve(reserved, 2), reserved);
    let cut = super::truncate(reserved, 1);
    assert_eq!(read(cut), Value::list(vec![unit("archer", 1)]));
    assert_eq!(room(cut), Some(20));
    heap::free(cut);
    assert_eq!(live(), before);
    finish();
}

#[test]
fn extending_a_buffer_holds_what_the_other_one_holds_too() {
    begin();
    let before = live();
    let first = list(&["archer"]);
    let second = numbered(&["medic", "soldier"], 2);
    let both = super::extend(first, second);
    heap::free(second);
    assert_eq!(read(both), Value::list(vec![unit("archer", 1), unit("medic", 2), unit("soldier", 3)]));
    assert_eq!(with(|state| state.heap.block(u64::try_from(both).ok()?)?.get(3).copied()), Some(4));
    let inserted = super::insert_buffer(both, 9, 0, 0);
    assert_eq!(inserted, 0);
    heap::free(both);
    assert_eq!(live(), before);
    let (_, failure, _) = finish();
    assert!(failure.is_some_and(|failure| failure.message == "can't insert at 9 in a Buffer of length 3"));
}

#[test]
fn a_list_of_items_with_no_fields_reads_back_with_every_item() {
    let empty = Rc::new(Element::Struct { name: Rc::from("Empty"), fields: Vec::new() });
    let tables = Tables { shapes: vec![Rc::new(Element::Str)], elements: vec![empty], ..Tables::default() };
    start(tables, crate::Inputs::default());
    let list = u64::try_from(super::new(0, 2)).unwrap();
    let read = with(|state| Some(heap::read_block(state, list))).unwrap();
    let shown = heap::shown::shown(list);
    finish();
    let Value::List(items) = read.unpacked() else { panic!("a list") };
    assert_eq!(items.len(), 2);
    assert_eq!(shown, "[Empty(), Empty()]");
}

/// The bases where `pow` has special cases, and some where it hasn't.
const BASES: [f64; 22] = [
    0.0,
    -0.0,
    f64::INFINITY,
    f64::NEG_INFINITY,
    f64::NAN,
    1.0,
    -1.0,
    2.0,
    -2.0,
    0.5,
    -0.5,
    3.0,
    0.1,
    -7.25,
    1e-310,
    -1e-310,
    f64::MIN_POSITIVE,
    1e154,
    1e155,
    -1e200,
    f64::MAX,
    f64::MIN,
];

#[test]
fn a_powers_fast_paths_give_what_pow_gives_at_its_special_cases() {
    for power in [2.0, 0.5, 1.0, 0.0, -1.0] {
        for base in BASES {
            let fast = super::float_power(base, power);
            let general = libm::pow(base, power);
            let same = fast.to_bits() == general.to_bits() || (fast.is_nan() && general.is_nan());
            assert!(same, "{base:?} ** {power:?}: the fast path gives {fast:?}, and pow {general:?}");
        }
    }
}

#[test]
fn a_powers_fast_paths_are_exact() {
    for base in [3.0_f64, 0.1, -7.25, 1.0 / 3.0, 12345.678, 1e-150] {
        assert_eq!(super::float_power(base, 2.0).to_bits(), (base * base).to_bits());
        assert_eq!(super::float_power(base.abs(), 0.5).to_bits(), base.abs().sqrt().to_bits());
        assert_eq!(super::float_power(base, -1.0).to_bits(), (1.0 / base).to_bits());
    }
    // Any other power is `pow`'s.
    assert_eq!(super::float_power(1.5, 2.7).to_bits(), libm::pow(1.5, 2.7).to_bits());
}

/// A list native code changes that this thread can't find stops the program as a bug, rather than
/// going on as if it were changed, which dropped what a task pushed to a constant it couldn't see.
#[test]
#[should_panic(expected = "a list native code changed isn't one this thread can find")]
fn a_list_this_thread_cant_find_stops_rather_than_drops_what_was_added() {
    begin();
    // No block of this thread's is at an address this odd. Asked through what `fennec_list_unshare`
    // and the rest call, as a panic can't leave an `extern "C"` function.
    let _ = with(|state| super::grow_in(state, 8 * 1_234_567 + 8, 0));
}
