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
    }))
}

fn begin() {
    let tables = Tables { shapes: vec![Rc::new(Element::Str)], elements: vec![units()], ..Tables::default() };
    start(&fennec_syntax::Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
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
