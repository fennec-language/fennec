//! What can be a dict key or a set item: not a type with its own `def ==`, whose hash could
//! disagree with it, wherever that type is held. A float field is fine, since floats hash as they
//! compare.

use crate::problems;

fn in_main(body: &str) -> Vec<String> {
    let text = format!(
        "from std.collections import Deque, SortedMap, SortedSet\n\n\
         struct Box[T]:\n    item: T\n\n\
         extend Box[T: Eq]:\n    def ==(self, other: Box[T]) -> bool:\n        return self.item == other.item\n\n\
         struct Holder:\n    set: SortedSet[int]\n\n\
         enum Held:\n    Some(set: SortedSet[int])\n    Nothing\n\n\
         def main(stdout: Stdout):\n    {body}\n"
    );
    problems(&text)
}

fn rejected(body: &str, name: &str) {
    let found = in_main(body);
    assert!(
        found.iter().any(|problem| problem.contains(&format!("`{name}` can't be a dict key or set item"))),
        "{body}: {found:?}"
    );
}

#[test]
fn a_generic_type_with_its_own_equality_cant_be_a_key() {
    rejected("stdout.print({Box(item=2): 1})", "Box[int]");
    rejected("stdout.print({Box(item=2)})", "Box[int]");
}

#[test]
fn std_collections_with_their_own_equality_cant_be_keys() {
    rejected("stdout.print({SortedSet.of([1]): 1})", "SortedSet[int]");
    rejected("stdout.print({SortedMap[int, int](): 1})", "SortedMap[int, int]");
    rejected("stdout.print({Deque[int](): 1})", "Deque[int]");
    rejected("stdout.print({SortedSet.of([1])})", "SortedSet[int]");
}

#[test]
fn a_struct_or_enum_holding_one_cant_be_a_key() {
    rejected("stdout.print({Holder(set=SortedSet.of([1])): 1})", "Holder");
    rejected("stdout.print({Held.Nothing: 1})", "Held");
    rejected("stdout.print({(1, SortedSet.of([1]))})", "(int, SortedSet[int])");
}

/// A struct or enum holding a float, `float` or `f32`, is a key: `-0.0` is `0.0` and NaN is NaN,
/// for `==` and hashing alike.
#[test]
fn a_struct_or_enum_holding_a_float_is_a_key() {
    let text = "struct Point:\n    x: float\n    y: f32\n\nenum Reading:\n    Level(value: float)\n    Off\n\n\
        def main(stdout: Stdout):\n    seen = {Point(x=0.0, y=f32(1.0)), Point(x=-0.0, y=f32(1.0))}\n    \
        named = {Reading.Level(value=-0.0): 1, Reading.Off: 2}\n    stdout.print(seen, named)\n";
    assert_eq!(problems(text), Vec::<String>::new());
}
