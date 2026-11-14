//! Dicts whose values are held in place, built straight in the heap: nothing native code makes
//! holds one yet, so these are the only ones there are.

use std::rc::Rc;

use super::super::{Tables, finish, heap, start, with};
use super::{Word, shape};
use crate::packed::Element;
use crate::value::{Instance, Value};

const LONG: &str = "a name long enough for a block of its own";

fn named(name: &str, fields: Vec<(&str, Element)>) -> Element {
    let fields = fields.into_iter().map(|(field, element)| (Rc::from(field), element)).collect();
    Element::Struct { name: Rc::from(name), fields }
}

/// A struct of five words: a number, a string's header and a list.
fn point() -> Element {
    named(
        "Point",
        vec![("x", Element::Int), ("name", Element::Text), ("near", Element::List(Rc::new(Element::Int)))],
    )
}

/// A result of a `Point` or a string, its `Err` case written first: number 3.
fn point_or_text() -> Element {
    Element::Enum {
        name: Rc::from("Result[Point, str]"),
        cases: vec![
            (Rc::from("Err"), vec![(Rc::from("error"), Element::Text)]),
            (Rc::from("Ok"), vec![(Rc::from("value"), point())]),
        ],
        words: 6,
    }
}

/// The kinds of item a run here has: a `Point`, a tuple of a number and a string, an optional
/// `Point`, and a result of a `Point` or a string: numbers 0 to 3.
fn elements() -> Vec<Rc<Element>> {
    let pair = named("(int, str)", vec![("0", Element::Int), ("1", Element::Text)]);
    let none: Vec<(Rc<str>, Element)> = Vec::new();
    let maybe = Element::Enum {
        name: Rc::from("Option[Point]"),
        cases: vec![(Rc::from("None"), none), (Rc::from("Some"), vec![(Rc::from("value"), point())])],
        words: 6,
    };
    vec![Rc::new(point()), Rc::new(pair), Rc::new(maybe), Rc::new(point_or_text())]
}

/// Starts a run with those kinds of item.
fn started() {
    let tables = Tables {
        shapes: vec![Rc::new(Element::Str)],
        elements: elements(),
        str_shape: 0,
        text_headers: true,
        ..Tables::default()
    };
    start(tables, crate::Inputs::default());
}

fn a_point(x: i64, name: &str, near: &[i64]) -> Value {
    let fields = vec![
        (Rc::from("x"), Value::Int(x)),
        (Rc::from("name"), Value::str(name)),
        (Rc::from("near"), Value::list(near.iter().map(|n| Value::Int(*n)).collect())),
    ];
    Value::Instance(Rc::new(Instance {
        type_name: Rc::from("Point"),
        case: None,
        fields,
        is_error: false,
        cause: None,
        notes: None,
    }))
}

fn dict_of(pairs: Vec<(Value, Value)>) -> Value {
    Value::Dict(Rc::new(pairs.into()))
}

/// A dict's block made from a `Value`, for keys of `key` and values in place of kind `element`.
fn made(key: Word, element: u32, value: &Value) -> u64 {
    let shape = shape(0, key, Word::InPlace(element)).unwrap();
    with(|state| super::write(state, shape, value).and_then(|at| u64::try_from(at).ok())).unwrap()
}

fn read(at: u64) -> Value {
    with(|state| Some(super::read(state, state.heap.block(at)?))).unwrap()
}

fn shown(at: u64) -> String {
    heap::shown::shown(at)
}

fn live() -> usize {
    with(|state| state.heap.live())
}

fn let_go(at: u64) {
    with(|state| heap::release(state, at));
}

fn points() -> Value {
    dict_of(vec![
        (Value::Int(7), a_point(1, "short", &[1, 2])),
        (Value::Int(-3), a_point(2, LONG, &[])),
        (Value::Int(40), a_point(3, LONG, &[9])),
    ])
}

#[test]
fn a_dict_of_structs_in_place_reads_back_as_it_was_written() {
    started();
    let before = live();
    let dict = made(Word::Int, 0, &points());
    assert_eq!(read(dict), points());
    assert_eq!(
        shown(dict),
        format!(
            "{{7: Point(x=1, name=\"short\", near=[1, 2]), -3: Point(x=2, name=\"{LONG}\", near=[]), 40: \
             Point(x=3, name=\"{LONG}\", near=[9])}}"
        )
    );
    // The dict, two long names and three lists.
    assert_eq!(live(), before + 6);
    let_go(dict);
    assert_eq!(live(), before);
    finish();
}

/// Every block the heap holds is counted as many times as it's held, by the audit's own count.
fn audited(held: &[u64]) {
    let found = with(|state| heap::audit::audit(state, held));
    assert_eq!(found.wrong, Vec::new());
}

#[test]
fn a_shape_says_its_values_are_in_place_and_which_kind() {
    for key in [Word::Int, Word::Str, Word::Text, Word::Plain(0), Word::Float] {
        for number in [0, 1, 77, (1 << 19) - 1] {
            let made = shape(5, key, Word::InPlace(number)).unwrap();
            assert_eq!(super::shape_kinds(5, made), Some((key, Word::InPlace(number))));
            // No dict of blocks, and none of one-word values, has that shape.
            assert_ne!(shape(5, key, Word::Block(number)), Some(made));
            assert!(made > super::BELOW_COUNTED_DOWN);
        }
    }
    assert_eq!(shape(5, Word::Int, Word::InPlace(1 << 19)), None);
    // The lowest shape of blocks is still one of blocks.
    let lowest = shape(5, Word::F32, Word::Block((1 << 20) - 1)).unwrap();
    assert_eq!(super::shape_kinds(5, lowest), Some((Word::F32, Word::Block((1 << 20) - 1))));
    // Nothing above the highest dict of blocks is a dict, and the shape below the lowest one
    // counted down is an ordinary one's, or none.
    assert_eq!(super::shape_kinds(5, super::TOP_BLOCK_DICT + 1), None);
    assert_eq!(super::shape_kinds(5, super::BELOW_COUNTED_DOWN), None);
    assert_eq!(super::shape_kinds(5, 5 + 1), Some((Word::Int, Word::Int)));
}

#[test]
fn an_entry_is_as_wide_as_its_key_and_its_values_kind() {
    started();
    let width = |key, element| {
        let shape = shape(0, key, Word::InPlace(element)).unwrap();
        with(|state| super::shape_layout(state, shape).map(|layout| (layout.width(), layout.value_at())))
    };
    // A hash, the key, then a `Point`'s five words, a tuple's four or an optional `Point`'s six.
    assert_eq!(width(Word::Int, 0), Some((7, 2)));
    assert_eq!(width(Word::Text, 0), Some((9, 4)));
    assert_eq!(width(Word::Int, 1), Some((6, 2)));
    assert_eq!(width(Word::Str, 2), Some((8, 2)));
    assert_eq!(width(Word::Int, 3), Some((8, 2)));
    // A kind the run doesn't have is no dict's.
    assert_eq!(width(Word::Int, 4), None);
    finish();
}

#[test]
fn dicts_of_values_in_place_are_equal_by_what_the_values_hold() {
    started();
    let before = live();
    let dict = made(Word::Int, 0, &points());
    // The same pairs added in another order, each value's blocks its own.
    let Value::Dict(pairs) = points() else { panic!() };
    let mut turned: Vec<(Value, Value)> = pairs.iter().cloned().collect();
    turned.reverse();
    let other = made(Word::Int, 0, &dict_of(turned.clone()));
    assert!(with(|state| heap::same(state, dict, other)));
    // One name differs, in a block: and one list.
    for changed in [a_point(2, "another name long enough for a block", &[]), a_point(2, LONG, &[5])] {
        let mut differing = turned.clone();
        differing[1].1 = changed;
        let third = made(Word::Int, 0, &dict_of(differing));
        assert!(!with(|state| heap::same(state, dict, third)));
        let_go(third);
    }
    audited(&[dict, other]);
    let_go(dict);
    let_go(other);
    assert_eq!(live(), before);
    finish();
}

#[test]
fn a_shared_dict_copied_to_change_counts_what_each_value_holds_again() {
    started();
    let before = live();
    let dict = made(
        Word::Text,
        0,
        &dict_of(vec![(Value::str(LONG), a_point(1, LONG, &[4])), (Value::str("k"), a_point(2, "n", &[]))]),
    );
    with(|state| heap::retain(state, dict));
    let copy = with(|state| super::unshared(state, dict)).unwrap();
    assert_ne!(copy, dict);
    assert_eq!(read(copy), read(dict));
    // Each holds the key's block, the name's and the two lists: counted twice now, and right.
    audited(&[dict, copy]);
    let_go(dict);
    audited(&[copy]);
    assert_eq!(
        shown(copy),
        format!(
            "{{\"{LONG}\": Point(x=1, name=\"{LONG}\", near=[4]), \"k\": Point(x=2, name=\"n\", near=[])}}"
        )
    );
    let_go(copy);
    assert_eq!(live(), before);
    finish();
}

#[test]
fn a_dict_of_values_in_place_grows_with_every_entry_whole() {
    started();
    let before = live();
    let pairs: Vec<(Value, Value)> = (0..40)
        .map(|at| (Value::Int(at * 3), a_point(at, if at % 2 == 0 { LONG } else { "odd" }, &[at])))
        .collect();
    let dict = made(Word::Int, 0, &dict_of(pairs.clone()));
    let (shape, width) = with(|state| {
        let block = state.heap.block(dict)?;
        Some((heap::shape_word(block)?, super::layout(state, block)?.width()))
    })
    .unwrap();
    let grown = with(|state| super::grown(state, dict, shape, width)).unwrap();
    assert_eq!(read(grown), dict_of(pairs));
    audited(&[grown]);
    let_go(grown);
    assert_eq!(live(), before);
    finish();
}

#[test]
fn a_value_in_place_taken_out_leaves_as_its_words_with_what_they_hold() {
    started();
    let before = live();
    let dict = made(Word::Int, 0, &points());
    let key = (-3_i64).cast_unsigned();
    assert!(matches!(
        with(|state| Some(super::take_key(state, dict, &[key]))),
        Some(super::Taken::Removed(0))
    ));
    let words = with(|state| super::buffered(state, 5)).unwrap();
    assert_eq!(words.len(), 5);
    let taken = with(|state| Some(heap::read_in_place(state, &point(), &words))).unwrap();
    assert_eq!(taken, a_point(2, LONG, &[]));
    assert_eq!(
        read(dict),
        dict_of(vec![
            (Value::Int(7), a_point(1, "short", &[1, 2])),
            (Value::Int(40), a_point(3, LONG, &[9]))
        ])
    );
    // The dict no longer holds the name and the list taken out: whoever took them does.
    let mut held = Vec::new();
    heap::held_addresses(&point(), &words, &mut held);
    assert_eq!(held.len(), 2);
    let mut all = vec![dict];
    all.extend(&held);
    audited(&all);
    // A key that isn't there takes nothing.
    assert!(matches!(with(|state| Some(super::take_key(state, dict, &[99]))), Some(super::Taken::Missing)));
    for address in held {
        let_go(address);
    }
    let_go(dict);
    assert_eq!(live(), before);
    finish();
}

#[test]
fn the_places_a_dict_holds_blocks_at_are_the_blocks_it_frees() {
    started();
    let dict = made(
        Word::Text,
        2,
        &dict_of(vec![
            (Value::str(LONG), a_point(1, LONG, &[4])),
            (Value::str("none"), Value::None),
            (Value::str("short"), a_point(2, "n", &[])),
        ]),
    );
    let (mut at_places, mut freed) = with(|state| {
        let block = state.heap.block(dict)?;
        let mut places = Vec::new();
        super::child_places(state, block, &mut places);
        let at_places: Vec<u64> = places.iter().filter_map(|place| block.get(*place).copied()).collect();
        Some((at_places, super::children(state, block)))
    })
    .unwrap();
    at_places.sort_unstable();
    freed.retain(|address| *address != 0);
    freed.sort_unstable();
    // The long key, a long name and two lists.
    assert_eq!(freed.len(), 4);
    assert_eq!(at_places, freed);
    assert_eq!(
        shown(dict),
        format!(
            "{{\"{LONG}\": Point(x=1, name=\"{LONG}\", near=[4]), \"none\": None, \"short\": Point(x=2, name=\"n\", near=[])}}"
        )
    );
    audited(&[dict]);
    let_go(dict);
    finish();
}

#[test]
fn a_tuple_in_place_is_a_tuple() {
    started();
    let before = live();
    let pairs = dict_of(vec![
        (Value::Int(1), Value::Tuple(Rc::new(vec![Value::Int(10), Value::str(LONG)]))),
        (Value::Int(2), Value::Tuple(Rc::new(vec![Value::Int(20), Value::str("b")]))),
    ]);
    let dict = made(Word::Int, 1, &pairs);
    assert_eq!(read(dict), pairs);
    assert_eq!(shown(dict), format!("{{1: (10, \"{LONG}\"), 2: (20, \"b\")}}"));
    audited(&[dict]);
    let_go(dict);
    assert_eq!(live(), before);
    finish();
}

#[test]
fn a_setter_of_one_word_changes_nothing_in_a_dict_of_values_in_place() {
    started();
    let dict = made(Word::Int, 0, &points());
    let was = read(dict);
    let signed = i64::try_from(dict).unwrap();
    assert_eq!(with(|state| super::set_words(state, signed, &[7], 5)), None);
    assert_eq!(with(|state| super::set_words(state, signed, &[8], 5)), None);
    assert_eq!(read(dict), was);
    audited(&[dict]);
    let_go(dict);
    finish();
}

#[test]
fn a_value_of_the_wrong_kind_makes_no_dict_and_leaves_nothing() {
    started();
    let before = live();
    let shape = shape(0, Word::Int, Word::InPlace(0)).unwrap();
    let wrong = dict_of(vec![(Value::Int(1), a_point(1, LONG, &[1])), (Value::Int(2), Value::Int(5))]);
    assert_eq!(with(|state| super::write(state, shape, &wrong)), None);
    assert_eq!(live(), before);
    // One that goes wrong partway, after a block was made for its name.
    let Value::Instance(whole) = a_point(3, LONG, &[]) else { panic!() };
    let mut fields = whole.fields.clone();
    fields[2].1 = Value::Int(5);
    let partly = Value::Instance(Rc::new(Instance { fields, ..(*whole).clone() }));
    let wrong = dict_of(vec![(Value::Int(1), a_point(1, LONG, &[1])), (Value::Int(2), partly)]);
    assert_eq!(with(|state| super::write(state, shape, &wrong)), None);
    assert_eq!(live(), before);
    finish();
}

/// Puts a `Point` in this thread's buffer as native code would: its words, with the owners they
/// hold.
fn buffer(value: &Value) {
    with(|state| {
        let mut words = Vec::new();
        heap::write(state, &point(), value, &mut words)?;
        super::value_buffer_of(state, |buffer| {
            for (kept, word) in buffer.iter().zip(&words) {
                kept.set(*word);
            }
        });
        Some(())
    })
    .unwrap();
}

fn signed(at: u64) -> i64 {
    i64::try_from(at).unwrap()
}

fn unsigned(at: i64) -> u64 {
    u64::try_from(at).unwrap()
}

#[test]
fn a_value_set_from_the_buffer_is_placed_replaced_and_grown_for() {
    started();
    let before = live();
    // The buffer is as long as the widest kind of item, an optional `Point`, and stays put.
    let at = super::value_buffer();
    assert_ne!(at, 0);
    assert_eq!(super::value_buffer(), at);
    assert_eq!(with(|state| super::buffered(state, 6).map(|words| words.len())), Some(6));
    assert_eq!(with(|state| super::buffered(state, 7)), None);
    let mut dict = made(Word::Int, 0, &dict_of(Vec::new()));
    let mut wanted = Vec::new();
    for key in 0..30_i64 {
        let value = a_point(key, if key % 3 == 0 { LONG } else { "short" }, &[key, key]);
        buffer(&value);
        dict = unsigned(super::set_in_place(signed(dict), key));
        wanted.push((Value::Int(key), value));
    }
    assert_eq!(read(dict), dict_of(wanted.clone()));
    audited(&[dict]);
    // One that's there is replaced, and what it held let go of.
    let replaced = a_point(-1, "another name long enough for a block", &[]);
    buffer(&replaced);
    assert_eq!(unsigned(super::set_in_place(signed(dict), 3)), dict);
    wanted[3].1 = replaced;
    assert_eq!(read(dict), dict_of(wanted.clone()));
    audited(&[dict]);
    // A dict something else holds too is copied, and the other is as it was.
    with(|state| heap::retain(state, dict));
    let added = a_point(100, LONG, &[1]);
    buffer(&added);
    let copy = unsigned(super::set_in_place(signed(dict), 100));
    assert_ne!(copy, dict);
    assert_eq!(read(dict), dict_of(wanted.clone()));
    wanted.push((Value::Int(100), added));
    assert_eq!(read(copy), dict_of(wanted));
    audited(&[dict, copy]);
    let_go(dict);
    let_go(copy);
    assert_eq!(live(), before);
    finish();
}

#[test]
fn a_value_set_by_a_strings_header_takes_the_key_as_the_dict_holds_it() {
    started();
    let before = live();
    let dict = made(Word::Text, 0, &dict_of(Vec::new()));
    let short = super::super::text::inline(b"short key");
    let long = with(|state| Some(super::super::text::header(state, LONG))).unwrap();
    buffer(&a_point(1, LONG, &[1]));
    let dict = unsigned(super::set_in_place_header(signed(dict), short[0], short[1], short[2]));
    buffer(&a_point(2, "n", &[]));
    let dict = unsigned(super::set_in_place_header(signed(dict), long[0], long[1], long[2]));
    assert_eq!(
        read(dict),
        dict_of(vec![
            (Value::str("short key"), a_point(1, LONG, &[1])),
            (Value::str(LONG), a_point(2, "n", &[]))
        ])
    );
    audited(&[dict]);
    // A dict of one-word values is given back unchanged.
    let plain = shape(0, Word::Int, Word::Int).unwrap();
    let other = unsigned(
        with(|state| super::write(state, plain, &dict_of(vec![(Value::Int(1), Value::Int(2))]))).unwrap(),
    );
    assert_eq!(unsigned(super::set_in_place(signed(other), 1)), other);
    assert_eq!(read(other), dict_of(vec![(Value::Int(1), Value::Int(2))]));
    let_go(other);
    let_go(dict);
    assert_eq!(live(), before);
    finish();
}

#[test]
fn a_value_popped_is_in_the_buffer_and_a_missing_key_fails_and_changes_nothing() {
    started();
    let before = live();
    let dict = made(Word::Int, 0, &points());
    assert_eq!(unsigned(super::pop_in_place(signed(dict), 40, 0, 0)), dict);
    let words = with(|state| super::buffered(state, 5)).unwrap();
    assert_eq!(
        with(|state| Some(heap::read_in_place(state, &point(), &words))),
        Some(a_point(3, LONG, &[9]))
    );
    // One that isn't there fails, gives the dict back and leaves the buffer as it was.
    assert_eq!(unsigned(super::pop_in_place(signed(dict), 41, 0, 0)), dict);
    assert!(super::super::failed());
    assert_eq!(with(|state| super::buffered(state, 5)), Some(words.clone()));
    let mut held = Vec::new();
    heap::held_addresses(&point(), &words, &mut held);
    for address in held {
        let_go(address);
    }
    let_go(dict);
    assert_eq!(live(), before);
    let (_, failure, _) = finish();
    assert!(failure.is_some_and(|failure| failure.message.contains("the dict has no key 41")));
}

#[test]
fn a_result_in_place_is_read_by_its_cases_names_whatever_their_order() {
    started();
    let before = live();
    let pairs = dict_of(vec![
        (Value::Int(1), Value::Ok(Rc::new(a_point(1, LONG, &[3])))),
        (Value::Int(2), Value::Err(Rc::new(Value::str(LONG)))),
        (Value::Int(3), Value::Err(Rc::new(Value::str("short")))),
    ]);
    let dict = made(Word::Int, 3, &pairs);
    assert_eq!(read(dict), pairs);
    // `Err` is case 0 here, as this element writes it first: the first word of each value says so.
    let cases: Vec<u64> = with(|state| {
        let block = state.heap.block(dict)?;
        let layout = super::layout(state, block)?;
        let held = super::entries(block, layout.width());
        Some(
            held.chunks_exact(layout.width())
                .filter_map(|entry| entry.get(layout.value_at()).copied())
                .collect(),
        )
    })
    .unwrap();
    assert_eq!(cases, [1, 0, 0]);
    // The dict, a `Point`'s name and list, and an error's text: the short one is in its header.
    assert_eq!(live(), before + 4);
    audited(&[dict]);
    // Equal to another made the same way, and not to one whose error differs.
    let same = made(Word::Int, 3, &pairs);
    assert!(with(|state| heap::same(state, dict, same)));
    let Value::Dict(held) = &pairs else { panic!() };
    let mut differing: Vec<(Value, Value)> = held.iter().cloned().collect();
    differing[2].1 = Value::Ok(Rc::new(a_point(1, LONG, &[3])));
    let other = made(Word::Int, 3, &dict_of(differing));
    assert!(!with(|state| heap::same(state, dict, other)));
    for dict in [dict, same, other] {
        let_go(dict);
    }
    assert_eq!(live(), before);
    finish();
}

/// The shape number of a struct whose one field is a dict, to hold one under a lock.
const HOLDER: i64 = 0;
/// Where a `Holder`'s dict is among its words.
const DICT_AT: usize = heap::HEADER_WORDS;

/// Starts a run as `started` does, with a `Holder` to put under a lock: struct shapes come before
/// the string's.
fn started_with_a_holder() {
    let dicts = usize::try_from(shape(1, Word::Int, Word::InPlace(0)).unwrap()).unwrap();
    let holder = named("Holder", vec![("by_key", Element::Boxed(dicts))]);
    let tables = Tables {
        shapes: vec![Rc::new(holder), Rc::new(Element::Str)],
        elements: elements(),
        str_shape: 1,
        text_headers: true,
        ..Tables::default()
    };
    start(tables, crate::Inputs::default());
}

fn word(block: i64, at: usize) -> i64 {
    let found = with(|state| state.heap.block(unsigned(block)).and_then(|words| words.get(at).copied()));
    i64::try_from(found.unwrap_or(0)).unwrap()
}

fn set_word(block: i64, at: usize, value: i64) {
    with(|state| {
        if let Some(place) = state.heap.block_mut(unsigned(block)).and_then(|words| words.get_mut(at)) {
            *place = unsigned(value);
        }
    });
}

/// How many blocks the current table has: the lock's, while it's held.
fn blocks() -> usize {
    with(|state| state.heap.blocks.len())
}

/// A `shared` value holding `points()` in a `Holder`, locked to write with its log begun, and the
/// dict marked as native code marks a value it's about to change where it is. Gives back the cell
/// and the dict.
fn locked() -> (i64, i64) {
    use super::super::guarded::{rollback, shared_enter, shared_new};
    let made = signed(made(Word::Int, 0, &points()));
    let cell = heap::alloc(HOLDER);
    set_word(cell, DICT_AT, made);
    shared_new(cell);
    shared_enter(cell, 1);
    rollback::begin(cell);
    // What a `shared` value holds is its own copy, in its lock's table.
    let dict = word(cell, DICT_AT);
    rollback::save_ref(cell, i64::try_from(DICT_AT).unwrap(), dict);
    let marked = i64::try_from(super::super::lists::MARKED).unwrap();
    assert_eq!(word(dict, 0), marked + 1, "the log marks the dict rather than hold it");
    (cell, dict)
}

/// Ends the lock's body as native code does: by a crash, which puts every saved word back, the
/// log's and then the cell's, or by keeping what changed. Then the lock is left.
fn ended(cell: i64, crashed: bool) {
    use super::super::guarded::{rollback, shared_leave};
    if crashed {
        rollback::undo();
        let at = i64::try_from(DICT_AT).unwrap();
        let back = rollback::undo_word(at, word(cell, DICT_AT), 1);
        set_word(cell, DICT_AT, back);
    } else {
        rollback::kept();
    }
    shared_leave(cell, 1);
}

/// What the dict a `Holder` holds reads as, and its count, with every block behind the lock
/// checked by the audit: the lock is taken to write, which makes its table the one that's read.
fn held_by(cell: i64) -> (Value, i64) {
    use super::super::guarded::{shared_enter, shared_leave};
    shared_enter(cell, 1);
    let dict = word(cell, DICT_AT);
    let read = (read(unsigned(dict)), word(dict, 0));
    // The cell's own count is its lock's to keep, another way. All it holds is counted as ever.
    let found = with(|state| heap::audit::audit(state, &[unsigned(cell)]));
    let wrong: Vec<_> = found.wrong.into_iter().filter(|finding| finding.address != unsigned(cell)).collect();
    assert_eq!(wrong, Vec::new());
    shared_leave(cell, 1);
    read
}

#[test]
fn values_set_in_place_under_a_locks_log_go_back_on_a_crash_and_stay_at_an_end() {
    started_with_a_holder();
    for crashed in [false, true] {
        let (cell, dict) = locked();
        let before = blocks();
        // One whose short name becomes a block, one whose block becomes a short name, and a new
        // key: each word noted in the log, where the dict lies.
        let puts =
            [(7, a_point(10, LONG, &[5])), (-3, a_point(20, "short", &[])), (99, a_point(30, LONG, &[1, 2]))];
        for (key, value) in &puts {
            buffer(value);
            assert_eq!(super::set_in_place(dict, *key), dict, "set where it lies, crashed: {crashed}");
        }
        let changed = dict_of(vec![
            (Value::Int(7), a_point(10, LONG, &[5])),
            (Value::Int(-3), a_point(20, "short", &[])),
            (Value::Int(40), a_point(3, LONG, &[9])),
            (Value::Int(99), a_point(30, LONG, &[1, 2])),
        ]);
        assert_eq!(read(unsigned(dict)), changed);
        // A name more for key 7, one fewer for -3, and a name and a list for 99: but the log holds
        // what was taken out until the body ends.
        assert!(blocks() > before);
        ended(cell, crashed);
        let (now, count) = held_by(cell);
        assert_eq!(now, if crashed { points() } else { changed }, "crashed: {crashed}");
        assert_eq!(count, 1, "crashed: {crashed}");
    }
    finish();
}

#[test]
fn a_value_taken_out_under_a_locks_log_goes_back_on_a_crash_and_stays_out_at_an_end() {
    started_with_a_holder();
    for crashed in [false, true] {
        let (cell, dict) = locked();
        assert_eq!(super::pop_in_place(dict, 40, 0, 0), dict, "taken out where it lies, crashed: {crashed}");
        let words = with(|state| super::buffered(state, 5)).unwrap();
        assert_eq!(
            with(|state| Some(heap::read_in_place(state, &point(), &words))),
            Some(a_point(3, LONG, &[9]))
        );
        assert_eq!(
            read(unsigned(dict)),
            dict_of(vec![
                (Value::Int(7), a_point(1, "short", &[1, 2])),
                (Value::Int(-3), a_point(2, LONG, &[]))
            ])
        );
        // Whoever took the value out lets go of what it holds, as native code does when its local
        // goes: the log has owners of its own, to put the value back with.
        let mut held = Vec::new();
        heap::held_addresses(&point(), &words, &mut held);
        assert_eq!(held.len(), 2);
        for address in held {
            let_go(address);
        }
        if crashed {
            // A key that isn't there fails, which is what leaves a lock's body by a crash: it gives
            // the dict back, and takes nothing out of it or of the buffer.
            assert_eq!(super::pop_in_place(dict, 41, 0, 0), dict);
            assert!(super::super::failed());
            assert_eq!(with(|state| super::buffered(state, 5)), Some(words.clone()));
        }
        ended(cell, crashed);
        let (now, count) = held_by(cell);
        let without = dict_of(vec![
            (Value::Int(7), a_point(1, "short", &[1, 2])),
            (Value::Int(-3), a_point(2, LONG, &[])),
        ]);
        assert_eq!(now, if crashed { points() } else { without }, "crashed: {crashed}");
        assert_eq!(count, 1, "crashed: {crashed}");
    }
    finish();
}
