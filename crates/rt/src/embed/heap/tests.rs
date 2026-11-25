//! Values crossing to native blocks and back: id maps, generic structs, tuples and strings.

use std::rc::Rc;

use super::super::{Tables, finish, heap, start, strings, with};
use super::{TASK_MARKS, number_to_mark, task_mark};
use crate::packed::Element;
use crate::value::Value;

fn text_of(address: i64) -> String {
    with(|state| {
        let block = u64::try_from(address).ok().and_then(|at| state.heap.block(at));
        block.map_or_else(String::new, heap::block_text)
    })
}

fn temp(at: i64) -> Value {
    with(|state| Some(state.temp(at))).unwrap()
}

#[test]
fn an_id_map_of_optional_ids_crosses_and_comes_back_the_same() {
    use crate::value::{Id, Instance, Slot, Table};
    let named = |name: &str, fields: Vec<(&str, Element)>| {
        let fields = fields.into_iter().map(|(field, element)| (Rc::from(field), element)).collect();
        Element::Struct { name: Rc::from(name), fields }
    };
    let id = named("Id", vec![("key", Element::Int), ("tag", Element::Int)]);
    let none: Vec<(Rc<str>, Element)> = Vec::new();
    let some = vec![(Rc::from("value"), id.clone())];
    let option = Element::Enum {
        name: Rc::from("Option[Id]"),
        cases: vec![(Rc::from("None"), none), (Rc::from("Some"), some)],
        words: 3,
    };
    let unit = named("Unit", vec![("name", Element::Str), ("target", option)]);
    let items = Element::List(Rc::new(unit));
    let generations = Element::List(Rc::new(Element::Int));
    let map = named(
        "Table[Unit]",
        vec![("items", items), ("slots", generations), ("live", Element::Int), ("free_from", Element::Int)],
    );
    let tables =
        Tables { shapes: vec![Rc::new(map), Rc::new(Element::Str)], str_shape: 1, ..Tables::default() };
    start(tables, crate::Inputs::default());
    let unit = |name: &str, target: Value| {
        let fields = vec![(Rc::from("name"), Value::str(name)), (Rc::from("target"), target)];
        Value::Instance(Rc::new(Instance {
            type_name: Rc::from("Unit"),
            case: None,
            fields,
            is_error: false,
            cause: None,
            notes: None,
        }))
    };
    let tag = crate::value::new_tag();
    let slots = vec![
        Slot { tag, value: Some(unit("a", Value::None)) },
        Slot { tag: 0, value: None },
        Slot {
            tag: crate::value::new_tag(),
            value: Some(unit("b", Value::Id(Id { slot: 0, generation: 0, tag }))),
        },
    ];
    let original = Value::Table(Rc::new(Table::new(slots)));
    let value = with(|state| Some(state.push(original.clone()))).unwrap();
    let block = heap::from_value(value, 0, 0, 0);
    assert_ne!(block, 0);
    assert_eq!(temp(heap::to_value(block)), original);
    finish();
}

#[test]
fn a_generic_struct_s_block_holds_an_instance_named_after_its_type() {
    use crate::value::Instance;
    let fields = vec![(Rc::from("value"), Element::Int)];
    let layout = Element::Struct { name: Rc::from("Holder[int]"), fields };
    let tables =
        Tables { shapes: vec![Rc::new(layout), Rc::new(Element::Str)], str_shape: 1, ..Tables::default() };
    start(tables, crate::Inputs::default());
    let fields = vec![(Rc::from("value"), Value::Int(4))];
    let held = Instance {
        type_name: Rc::from("Holder"),
        case: None,
        fields,
        is_error: false,
        cause: None,
        notes: None,
    };
    let original = Value::Instance(Rc::new(held));
    let value = with(|state| Some(state.push(original.clone()))).unwrap();
    let block = heap::from_value(value, 0, 0, 0);
    assert_ne!(block, 0);
    assert_eq!(temp(heap::to_value(block)), original);
    finish();
}

#[test]
fn a_list_of_tuples_reads_back_as_tuples() {
    let pair = Element::Struct {
        name: Rc::from("(int, int)"),
        fields: vec![(Rc::from("_0"), Element::Int), (Rc::from("_1"), Element::Int)],
    };
    let tables =
        Tables { shapes: vec![Rc::new(Element::Str)], elements: vec![Rc::new(pair)], ..Tables::default() };
    start(tables, crate::Inputs::default());
    let list = super::super::lists::new(0, 2);
    let path = Value::list(vec![
        Value::tuple(vec![Value::Int(13), Value::Int(5)]),
        Value::tuple(vec![Value::Int(1), Value::Int(2)]),
    ]);
    for (at, word) in [13, 5, 1, 2].into_iter().enumerate() {
        with(|state| {
            let block = state.heap.block_mut(u64::try_from(list).ok()?)?;
            *block.get_mut(super::super::lists::LIST_HEADER_WORDS + at)? = word;
            Some(())
        });
    }
    assert_eq!(temp(heap::to_value(list)), path);
    finish();
}

/// A copy of a block that holds many others lets go of the room it listed them in once it's
/// done, where a short one's is kept for the next: kept, one copy of a list of 20,000 strings
/// held 160 KB of it on its thread for the rest of the run.
#[test]
fn copying_a_big_list_lets_its_scratch_room_go() {
    let str = Rc::new(Element::Str);
    let tables = Tables { shapes: vec![Rc::clone(&str)], elements: vec![str], ..Tables::default() };
    start(tables, crate::Inputs::default());
    let room_after_copying = |count: usize| {
        let texts: Vec<String> =
            (0..count).map(|number| format!("item {number} of a list, long enough for a block")).collect();
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let list = with(|state| super::super::lists::list_of_strings(state, 0, &texts));
        // Owned twice, so the copy is made.
        with(|state| super::retain(state, list.cast_unsigned()));
        assert_ne!(heap::unshare(list), 0);
        with(|state| state.heap.scratch.capacity())
    };
    assert!(room_after_copying(100) >= 100, "a short list's room is kept");
    let room = room_after_copying(20_000);
    assert!(room <= super::SCRATCH_KEPT_WORDS, "{room} words kept after copying 20,000");
    finish();
}

/// Freeing a block that holds many others lets go of the room it listed them in, where a short
/// one's is kept for the next: kept, one list of 20,000 strings freed held 160 KB of it on its
/// thread for the rest of the run.
#[test]
fn freeing_a_big_list_lets_its_room_for_children_go() {
    let str = Rc::new(Element::Str);
    let tables = Tables { shapes: vec![Rc::clone(&str)], elements: vec![str], ..Tables::default() };
    start(tables, crate::Inputs::default());
    let room_after_freeing = |count: usize| {
        let texts: Vec<String> =
            (0..count).map(|number| format!("item {number} of a list, long enough for a block")).collect();
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let list = with(|state| super::super::lists::list_of_strings(state, 0, &texts));
        heap::free(list);
        with(|state| state.heap.found.capacity())
    };
    assert!(room_after_freeing(100) >= 100, "a short list's room is kept");
    let room = room_after_freeing(20_000);
    assert!(room <= super::SCRATCH_KEPT_WORDS, "{room} words kept after freeing 20,000");
    let (_, failure, _) = finish();
    assert!(failure.is_none(), "{failure:?}");
}

#[test]
fn strings_have_their_methods() {
    let str = Rc::new(Element::Str);
    let tables = Tables { shapes: vec![Rc::clone(&str)], elements: vec![str], ..Tables::default() };
    start(tables, crate::Inputs::default());
    let string = |text: &str| with(|state| heap::text_address(state, text));
    let fox = string("The quick Fox");
    assert_eq!(text_of(strings::upper(fox)), "THE QUICK FOX");
    assert_eq!(text_of(strings::lower(fox)), "the quick fox");
    assert_eq!(text_of(strings::replace(fox, string("quick"), string("slow"))), "The slow Fox");
    assert_eq!(strings::find(fox, string("Fox")), 10);
    assert_eq!(strings::find(fox, string("cat")), -1);
    let words = strings::split(fox, 0, 0, 0);
    let list = Element::List(Rc::new(Element::Str));
    let words = u64::try_from(words).unwrap();
    let parts = with(|state| Some(heap::read_word(state, &list, words))).unwrap();
    assert_eq!(parts, Value::list(["The", "quick", "Fox"].into_iter().map(Value::str).collect()));
    assert_eq!(strings::split(fox, string(""), 0, 0), 0);
    let (_, failure, _) = finish();
    assert!(failure.is_some());
}

/// A string that's never freed, as a literal is, has its text kept apart from those of strings that
/// can be, read once and found again after: a free has nothing to forget while only such texts
/// are held. A string that can be freed has its text forgotten when it is.
#[test]
fn a_never_freed_strings_text_is_kept_apart_from_those_a_free_forgets() {
    let str = Rc::new(Element::Str);
    let tables = Tables { shapes: vec![Rc::clone(&str)], elements: vec![str], ..Tables::default() };
    start(tables, crate::Inputs::default());
    let string = |text: &str| with(|state| heap::text_address(state, text));
    let read = |at: i64| with(|state| Some(heap::read_block(state, at.cast_unsigned()))).unwrap();
    let message = "NewUser.age breaks its `where` rule";
    let literal = string(message);
    heap::constant_kept(literal);
    let (first, again) = (read(literal), read(literal));
    assert_eq!(first, Value::str(message));
    let (Value::Str(first), Value::Str(again)) = (first, again) else { panic!("not strings") };
    assert!(Rc::ptr_eq(&first, &again), "read again, its text is made anew");
    with(|state| {
        assert!(state.heap.texts.borrow().is_empty(), "a literal's text is among those a free forgets")
    });
    let made = string("a string made as the program runs, long enough for a block of its own");
    assert_eq!(
        read(made),
        Value::str("a string made as the program runs, long enough for a block of its own")
    );
    with(|state| assert_eq!(state.heap.texts.borrow().len(), 1));
    heap::free(made);
    with(|state| assert!(state.heap.texts.borrow().is_empty(), "a freed string's text is kept"));
    assert_eq!(read(literal), Value::str(message));
    let (_, failure, _) = finish();
    assert!(failure.is_none(), "{failure:?}");
}

/// A task's mark is its own number's until the marks have come round, which is 16 million tasks on
/// one thread. After that a task whose number's mark is held by one still alive takes the next
/// that's free, so two tasks alive on a thread never share a mark: a long task's end finds its
/// blocks by its mark, and would take the other's with them.
#[test]
fn a_task_never_takes_the_mark_of_one_still_alive_on_its_thread() {
    // Before the marks come round nothing's asked, as no number's mark has been given twice.
    assert_eq!(number_to_mark(7, |_| panic!("asked before the marks came round")), 7);
    let round = TASK_MARKS;
    // A task alive since number 7 has the mark that number 7 a round later would get.
    assert_eq!(task_mark(7), task_mark(round + 7));
    let alive = |number: u64| task_mark(number) == task_mark(7);
    assert_eq!(number_to_mark(round + 7, alive), round + 8);
    assert_ne!(task_mark(number_to_mark(round + 7, alive)), task_mark(7));
    // With the next two held as well, it takes the one after.
    let three = |number: u64| [7, 8, 9].iter().any(|held| task_mark(number) == task_mark(*held));
    assert_eq!(number_to_mark(round + 7, three), round + 10);
    // One whose own mark is free keeps it.
    assert_eq!(number_to_mark(round + 20, alive), round + 20);
}

/// A `Table` laid out with each slot's tag (#2424) crosses to its block and back with each slot's
/// own, 0 for one taken out, and an `Id` finds its item only under its slot's tag.
#[test]
fn a_table_s_slots_keep_their_own_tags_crossing_to_a_block_and_back() {
    use crate::value::{Id, Slot, Table};
    let fields = vec![
        (Rc::from("items"), Element::List(Rc::new(Element::Int))),
        (Rc::from("slots"), Element::List(Rc::new(Element::Int))),
        (Rc::from("live"), Element::Int),
        (Rc::from("free_from"), Element::Int),
    ];
    let layout = Element::Struct { name: Rc::from("Table[int]"), fields };
    let tables = Tables {
        shapes: vec![Rc::new(layout), Rc::new(Element::Str)],
        elements: vec![Rc::new(Element::Int)],
        str_shape: 1,
        ..Tables::default()
    };
    start(tables, crate::Inputs::default());
    let tags = [crate::value::new_tag(), crate::value::new_tag()];
    let slots = vec![
        Slot { tag: tags[0], value: Some(Value::Int(10)) },
        Slot { tag: 0, value: None },
        Slot { tag: tags[1], value: Some(Value::Int(30)) },
    ];
    let original = Table::new(slots);
    let value = with(|state| Some(state.push(Value::Table(Rc::new(original.clone()))))).unwrap();
    let block = heap::from_value(value, 0, 0, 0);
    assert_ne!(block, 0);
    let Value::Table(back) = temp(heap::to_value(block)) else { panic!("not a Table") };
    assert_eq!(*back, original);
    assert_eq!(back.slots, original.slots, "each slot's tag came back too");
    assert_eq!(back.get(Id::new(2, 0, tags[1])), Some(&Value::Int(30)));
    assert_eq!(back.get(Id::new(2, 0, tags[0])), None, "the same slot under another tag");
    assert_eq!(back.get(Id::new(1, 0, 0)), None, "a slot taken out finds nothing");
    let ids: Vec<u64> = back.entries().map(|(id, _)| id.tag).collect();
    assert_eq!(ids, vec![tags[0], tags[1]]);
    finish();
}

/// `fennec_tag_count` gives the place of the count `fennec_new_tag` takes from, which never moves,
/// so native code taking tags from it and the runtime never give the same one.
#[test]
fn the_tag_count_is_where_new_tag_takes_from_and_stays_there() {
    let at = heap::tag_count();
    assert_ne!(at, 0);
    let first = heap::new_tag();
    let second = heap::new_tag();
    assert!(second > first);
    assert_eq!(heap::tag_count(), at);
}

/// `fennec_tag_run` gives each thread a place of its own for its run of tags, which stays put.
#[test]
fn each_thread_s_run_of_tags_has_a_place_of_its_own_that_stays_put() {
    let here = heap::tag_run();
    assert_ne!(here, 0);
    assert_eq!(heap::tag_run(), here);
    let there = std::thread::spawn(|| heap::tag_run()).join().unwrap_or(0);
    assert_ne!(there, 0);
    assert_ne!(there, here);
}
