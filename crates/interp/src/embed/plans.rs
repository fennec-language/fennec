//! Where a shape's blocks hold the addresses of other blocks, worked out once per shape, so
//! freeing a block doesn't walk its shape's description every time.

use crate::packed::Element;

/// Which words of a block's fields hold addresses, counting from the first field.
pub(super) enum Plan {
    /// The same words in every block of the shape.
    Fixed(Box<[usize]>),
    /// An enum's: the words for each case, by the case's number in the first word.
    Cases(Box<[Box<[usize]>]>),
    /// Anything else, which is walked as before.
    Walk,
}

/// The words of a value laid out from `at` that hold addresses, if they're the same for every
/// value of the element: not for an enum inside it, whose case decides.
fn fixed_words(element: &Element, at: &mut usize, found: &mut Vec<usize>) -> Option<()> {
    match element {
        Element::Boxed(_) | Element::Str | Element::List(_) | Element::Object | Element::Maybe(_) => {
            found.push(*at);
            *at += 1;
        }
        Element::Struct { fields, .. } => {
            for (_, field) in fields {
                fixed_words(field, at, found)?;
            }
        }
        Element::Enum { .. } => return None,
        Element::Int | Element::Float | Element::Bool => *at += 1,
    }
    Some(())
}

/// How to find the addresses in blocks of a shape.
pub(super) fn plan(element: &Element) -> Plan {
    if let Element::Enum { cases, .. } = element {
        let mut plans = Vec::with_capacity(cases.len());
        for (_, fields) in cases {
            let (mut at, mut found) = (1, Vec::new());
            for (_, field) in fields {
                if fixed_words(field, &mut at, &mut found).is_none() {
                    return Plan::Walk;
                }
            }
            plans.push(found.into_boxed_slice());
        }
        return Plan::Cases(plans.into_boxed_slice());
    }
    let mut found = Vec::new();
    match fixed_words(element, &mut 0, &mut found) {
        Some(()) => Plan::Fixed(found.into_boxed_slice()),
        None => Plan::Walk,
    }
}

/// The non-zero addresses in a block's fields, if the plan says where they are without
/// walking. `None` means walk it.
pub(super) fn addresses(plan: &Plan, fields: &[u64], found: &mut Vec<u64>) -> Option<()> {
    let words = match plan {
        Plan::Fixed(words) => words,
        Plan::Cases(cases) => cases.get(usize::try_from(*fields.first()?).ok()?)?,
        Plan::Walk => return None,
    };
    found.extend(words.iter().filter_map(|at| fields.get(*at).copied()).filter(|address| *address != 0));
    Some(())
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::{Plan, addresses, plan};
    use crate::packed::Element;

    #[test]
    fn a_tree_node_s_children_are_found_by_its_case() {
        let tree = Element::Enum {
            name: Rc::from("Tree"),
            cases: vec![
                (Rc::from("Leaf"), Vec::new()),
                (
                    Rc::from("Node"),
                    vec![(Rc::from("left"), Element::Boxed(0)), (Rc::from("right"), Element::Boxed(0))],
                ),
            ],
            words: 3,
        };
        let plan = plan(&tree);
        assert!(matches!(plan, Plan::Cases(_)));
        let mut found = Vec::new();
        addresses(&plan, &[1, 40, 0], &mut found).unwrap();
        assert_eq!(found, [40]);
        found.clear();
        addresses(&plan, &[0, 0, 0], &mut found).unwrap();
        assert_eq!(found, Vec::<u64>::new());
    }

    #[test]
    fn a_struct_s_addresses_are_the_same_words_each_time() {
        let inner = Element::Struct {
            name: Rc::from("In"),
            fields: vec![(Rc::from("n"), Element::Int), (Rc::from("s"), Element::Str)],
        };
        let outer = Element::Struct {
            name: Rc::from("Out"),
            fields: vec![(Rc::from("a"), inner), (Rc::from("b"), Element::Boxed(1))],
        };
        let mut found = Vec::new();
        addresses(&plan(&outer), &[7, 16, 24], &mut found).unwrap();
        assert_eq!(found, [16, 24]);
    }
}
