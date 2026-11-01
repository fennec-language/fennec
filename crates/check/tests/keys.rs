//! What can be a dict key or a set item: not a type with its own `def ==`, whose hash could
//! disagree with it, wherever that type is held.

use crate::problems;

fn in_main(body: &str) -> Vec<String> {
    let text = format!(
        "from std.collections import Deque, SortedMap, SortedSet\n\n\
         struct Box[T]:\n    item: T\n\n\
         extend Box[T: Eq]:\n    def ==(self, other: Box[T]) -> bool:\n        self.item == other.item\n\n\
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
