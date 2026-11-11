//! Values crossing to native blocks and back: id maps, generic structs, tuples and strings.

use std::rc::Rc;

use super::super::{Tables, finish, heap, start, strings, with};
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
        vec![
            ("items", items),
            ("generations", generations),
            ("live", Element::Int),
            ("free_from", Element::Int),
            ("tag", Element::Int),
        ],
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
        Slot { generation: 0, value: Some(unit("a", Value::None)) },
        Slot { generation: 3, value: None },
        Slot { generation: 1, value: Some(unit("b", Value::Id(Id { slot: 0, generation: 0, tag }))) },
    ];
    let original = Value::Table(Rc::new(Table::tagged(slots, tag)));
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
