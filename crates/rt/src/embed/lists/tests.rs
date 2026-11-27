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

/// What the `runtime` fuzz target found, cut down: an item's address before the list's own, as a
/// made-up one is, took the header for an item, moved its words down and took 1 from a length of
/// 0. A place partway into an item, or past the last, is no item either: each changes nothing.
#[test]
fn an_item_taken_out_of_a_list_at_a_place_no_item_starts_changes_nothing() {
    begin();
    let before = live();
    let all = list(&["archer one", "medic two"]);
    let word = 8 * i64::try_from(super::LIST_HEADER_WORDS).unwrap();
    for item in [all - 8, all, all + 8, all + word + 8, all + word + 32, all + word + 40] {
        super::remove_at(all, item);
        let both = Value::list(vec![unit("archer one", 1), unit("medic two", 2)]);
        assert_eq!(read(all), both, "at {}", item - all);
    }
    // The second item starts two words past the first, and taking it out still works.
    super::remove_at(all, all + word + 16);
    assert_eq!(read(all), Value::list(vec![unit("archer one", 1)]));
    heap::free(all);
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
    // One whose fields differ from every item's in one of them isn't found.
    let other = with(|state| Some(state.push(unit("medic", 9)))).unwrap();
    let other = heap::from_value(other, 0, 0, 0);
    assert_eq!(super::contains(all, other), 0);
    heap::free(other);
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
    // Room for 20 more than the 3 it holds, not for 20 in all.
    let reserved = super::reserve(all, 20);
    assert_eq!(room(reserved), Some(23));
    // It has that room already, so asking again for as much, or for less, changes nothing.
    assert_eq!(super::reserve(reserved, 20), reserved);
    assert_eq!(super::reserve(reserved, 2), reserved);
    // One more than it has room for beyond what it holds makes more.
    let more = super::reserve(reserved, 21);
    assert_eq!(room(more), Some(24));
    let cut = super::truncate(more, 1);
    assert_eq!(read(cut), Value::list(vec![unit("archer", 1)]));
    assert_eq!(room(cut), Some(24));
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

/// Clearing a list of ints keeps its block and its room, and only sets its length to 0: what it held
/// stays in the room, past the length, so the appends after it go into the same block, and growing
/// into the room zeroes the item it adds, whatever was there. Items that hold nothing are neither
/// copied out to be let go of nor zeroed.
#[test]
fn a_cleared_list_of_ints_keeps_its_room() {
    let tables = Tables { elements: vec![Rc::new(Element::Int)], ..Tables::default() };
    start(tables, crate::Inputs::default());
    let before = with(|state| state.heap.live());
    let numbers = super::new(0, 3);
    let words =
        |list: i64| with(|state| state.heap.block(list.cast_unsigned()).map(<[u64]>::to_vec)).unwrap();
    for (at, n) in [7, 8, 9].into_iter().enumerate() {
        let place = super::LIST_HEADER_WORDS + at;
        with(|state| state.heap.block_mut(numbers.cast_unsigned()).unwrap()[place] = n);
    }
    let room = words(numbers)[3];
    super::clear(numbers);
    let cleared = words(numbers);
    assert_eq!((cleared[2], cleared[3]), (0, room), "no items, and the same room");
    assert_eq!(
        &cleared[super::LIST_HEADER_WORDS..super::LIST_HEADER_WORDS + 3],
        [7, 8, 9],
        "left in the room"
    );
    assert_eq!(super::grow(numbers), numbers, "an append after it goes into the same block");
    let grown = words(numbers);
    assert_eq!(grown[2], 1);
    assert_eq!(grown[super::LIST_HEADER_WORDS], 0, "the item an append adds starts at 0");
    heap::free(numbers);
    assert_eq!(with(|state| state.heap.live()), before);
    finish();
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

/// A list of a kind of item the program never described, made on one thread and read on another,
/// reads the same there: the runtime numbers such a kind once for every thread. Each thread
/// numbered its own before, so another thread read the list as nothing, or as another kind, as an
/// error's notes made on `main` and changed in a task were lost (#1843).
#[test]
fn a_list_of_a_kind_the_runtime_numbered_reads_the_same_on_another_thread() {
    let kind = |name: &str| {
        let fields = vec![(Rc::from("only"), Element::Int)];
        Rc::new(Element::Struct { name: Rc::from(name), fields })
    };
    let item = |value: i64| {
        Value::Instance(Rc::new(Instance {
            type_name: Rc::from("NumberedOnAnotherThread"),
            case: None,
            fields: vec![(Rc::from("only"), Value::Int(value))],
            is_error: false,
            cause: None,
            notes: None,
        }))
    };
    // The two threads run at once, as a run's threads do, and the maker ends only once the list is read.
    let (sent, made) = std::sync::mpsc::channel::<Vec<u64>>();
    let (read_it, done) = std::sync::mpsc::channel::<()>();
    let maker = std::thread::spawn(move || {
        begin();
        // Another kind first, which this thread numbers and the reading thread never meets, so the
        // two threads' own counts of kinds differ.
        let other = kind("NumberedHereOnly");
        with(|state| super::write_list(state, &other, &Value::list(vec![]))).unwrap();
        let list = with(|state| {
            super::write_list(state, &kind("NumberedOnAnotherThread"), &Value::list(vec![item(7)]))
        })
        .unwrap();
        let block: Vec<u64> =
            with(|state| state.heap.block(u64::try_from(list).ok()?).map(<[u64]>::to_vec)).unwrap();
        sent.send(block).unwrap();
        done.recv().unwrap();
        finish();
    });
    begin();
    let made = made.recv().unwrap();
    let read = with(|state| {
        let mut block = made.into_boxed_slice();
        // Adopted as this thread's own, as a block a task hands over is.
        if let Some(count) = block.first_mut() {
            *count = 1;
        }
        let address = state.heap.insert(block);
        Some(heap::read_block(state, u64::try_from(address).ok()?).unpacked())
    })
    .unwrap();
    read_it.send(()).unwrap();
    maker.join().unwrap();
    assert_eq!(read, Value::list(vec![item(7)]));
    finish();
}

/// A thread's runtime whose program described `described` kinds, as each thread of one run has.
fn begin_described(described: usize) {
    let elements = (0..described)
        .map(|at| Rc::new(Element::Struct { name: Rc::from(format!("Described{at}")), fields: vec![] }))
        .collect();
    let tables = Tables { shapes: vec![Rc::new(Element::Str)], elements, ..Tables::default() };
    start(tables, crate::Inputs::default());
}

/// A kind no program describes, one for each `at`.
fn undescribed(tag: &str, at: usize) -> Rc<Element> {
    let fields = vec![(Rc::from("count"), Element::Int), (Rc::from("name"), Element::Str)];
    Rc::new(Element::Struct { name: Rc::from(format!("{tag}{at}")), fields })
}

/// Sixteen threads of one run number the same kinds at once, each in its own order and each only
/// some of them, then read every number back: each kind has one number on every thread, and reads
/// back as itself there, numbered there or not. From the review of #1870.
#[test]
fn kinds_numbered_at_once_on_many_threads_get_one_number_each() {
    const THREADS: usize = 16;
    const KINDS: usize = 60;
    let together = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
    let threads: Vec<_> = (0..THREADS)
        .map(|thread| {
            let together = std::sync::Arc::clone(&together);
            std::thread::spawn(move || {
                begin_described(7);
                together.wait();
                for step in 0..KINDS {
                    let at = (step * 7 + thread * 11) % KINDS;
                    if !(at + thread).is_multiple_of(3) {
                        with(|state| Some(super::number_of_element(state, &undescribed("AtOnce", at))))
                            .unwrap();
                    }
                }
                together.wait();
                let read: Vec<(usize, bool)> = (0..KINDS)
                    .map(|at| {
                        let kind = undescribed("AtOnce", at);
                        let number = with(|state| Some(super::number_of_element(state, &kind))).unwrap();
                        let same = with(|state| {
                            Some(state.tables.element(number).is_some_and(|found| *found == kind))
                        });
                        (number, same.unwrap_or(false))
                    })
                    .collect();
                finish();
                read
            })
        })
        .collect();
    let read: Vec<Vec<(usize, bool)>> = threads.into_iter().map(|thread| thread.join().unwrap()).collect();
    for thread in &read {
        assert_eq!(thread, &read[0], "two threads gave a kind different numbers, or misread one");
    }
    let mut numbers: Vec<usize> = read[0].iter().map(|(number, _)| *number).collect();
    numbers.sort_unstable();
    numbers.dedup();
    assert_eq!(numbers.len(), KINDS);
    assert!(numbers.iter().all(|number| *number >= 7));
    assert!(read[0].iter().all(|(_, same)| *same));
}

/// Runs one after another on one thread, as the fuzzer and these tests run them in one process,
/// each numbering 1024 kinds of its own: the most kinds is counted for each run, as a run lets its
/// program's go when it ends. With one list for the process, the fifth stopped as a bug.
#[test]
fn the_most_kinds_the_runtime_numbers_is_counted_for_each_run() {
    for run in 0..6 {
        begin_described(2);
        for at in 0..1024 {
            with(|state| Some(super::number_of_element(state, &undescribed(&format!("Run{run}Kind"), at))))
                .unwrap();
        }
        finish();
    }
}

/// A list of a kind of item this thread knows nothing of stops as a bug when it's read, rather than
/// reading as nothing: as nothing, an error's notes were lost without a word (#1843).
#[test]
#[should_panic(expected = "which this thread knows nothing of")]
fn a_list_of_a_kind_this_thread_knows_nothing_of_stops_when_read() {
    begin();
    let address = with(|state| {
        let shape = super::first_list_shape(state) + 5;
        Some(state.heap.insert(vec![1, shape, 0, 0].into_boxed_slice()))
    })
    .unwrap();
    let _ = read(address);
}
