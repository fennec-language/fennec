//! Where a shape's blocks and a list's in-place items hold the addresses of other blocks, worked
//! out once for each, so freeing one doesn't walk its description every time.

use crate::packed::Element;

/// Where a value holds blocks, counting from its first word.
pub(super) struct Holds {
    /// Words holding a block's address, or zero.
    blocks: Box<[usize]>,
    /// Where a string's header starts, which holds a block or not by its string.
    texts: Box<[usize]>,
    /// The enums inside that hold blocks, whose case says which words do.
    enums: Box<[Inner]>,
}

/// An enum inside a value: where its case's number is, and what each case holds, counting from the
/// value's first word too.
struct Inner {
    at: usize,
    cases: Box<[Holds]>,
}

impl Holds {
    /// Whether the words holding blocks are the same in every value, with no enum inside to ask.
    fn fixed(&self) -> bool {
        self.enums.is_empty()
    }

    /// Every block a value's words hold, zero left out: in its words, its strings' headers and the
    /// enums inside it, by their cases.
    pub(super) fn each_block(&self, words: &[u64], found: &mut impl FnMut(u64)) {
        for block in self.blocks_in(words).chain(self.texts_in(words)) {
            if block != 0 {
                found(block);
            }
        }
        for inner in &self.enums {
            let case = words.get(inner.at).and_then(|case| usize::try_from(*case).ok());
            if let Some(holds) = case.and_then(|case| inner.cases.get(case)) {
                holds.each_block(words, found);
            }
        }
    }

    /// The addresses in a value's words, zero for no block, which most values have all of.
    fn blocks_in<'w>(&'w self, words: &'w [u64]) -> impl Iterator<Item = u64> + 'w {
        self.blocks.iter().filter_map(|at| words.get(*at).copied())
    }

    /// The blocks of the strings in a value's words, zero for an optional string that's `None`.
    fn texts_in<'w>(&'w self, words: &'w [u64]) -> impl Iterator<Item = u64> + 'w {
        self.texts.iter().filter_map(|at| {
            let header = <&[u64; 3]>::try_from(words.get(*at..*at + 3)?).ok()?;
            super::text::block_of(header)
        })
    }
}

/// Which words of a value hold blocks. What holds no strings, which is most of what's freed,
/// pays nothing for the strings something else might hold.
pub(super) enum Plan {
    /// The same words in every value of the kind.
    Fixed(Box<[usize]>),
    /// An enum's: the words for each case, by the case's number in the first word.
    Cases(Box<[Box<[usize]>]>),
    /// A struct with strings in it.
    FixedWithTexts(Box<Holds>),
    /// An enum with strings in it, by the case's number in the first word.
    CasesWithTexts(Box<[Holds]>),
    /// Anything else, which is walked as before.
    Walk,
}

impl Plan {
    /// Whether no value of the kind holds a block.
    pub(super) fn holds_nothing(&self) -> bool {
        match self {
            Plan::Fixed(words) => words.is_empty(),
            Plan::Cases(cases) => cases.iter().all(|words| words.is_empty()),
            Plan::FixedWithTexts(_) | Plan::CasesWithTexts(_) | Plan::Walk => false,
        }
    }
}

/// Adds where a value laid out from `at` holds blocks: the words, the strings' headers, and each enum
/// inside that holds any, with what each of its cases holds.
fn fixed_words(element: &Element, at: &mut usize, holds: &mut Found) -> Option<()> {
    match element {
        Element::Text => {
            holds.texts.push(*at);
            *at += 3;
        }
        element if element.is_optional_text() => {
            holds.texts.push(*at);
            *at += 3;
        }
        Element::Boxed(_)
        | Element::Str
        | Element::List(_)
        | Element::ListOf(_)
        | Element::Object
        | Element::Maybe(_) => {
            holds.blocks.push(*at);
            *at += 1;
        }
        Element::Struct { fields, .. } => {
            for (_, field) in fields {
                fixed_words(field, at, holds)?;
            }
        }
        Element::Enum { words, .. } if !super::heap::holds_blocks(element) => *at += words,
        Element::Enum { cases, words, .. } => {
            let cases: Option<Vec<Holds>> =
                cases.iter().map(|(_, fields)| holds_of(fields.iter().map(|(_, field)| field), *at + 1)).collect();
            holds.enums.push(Inner { at: *at, cases: cases?.into_boxed_slice() });
            *at += words;
        }
        Element::Int | Element::Float | Element::Bool | Element::U64 | Element::F32 => *at += 1,
    }
    Some(())
}

/// What `fixed_words` finds, as it goes.
#[derive(Default)]
struct Found {
    blocks: Vec<usize>,
    texts: Vec<usize>,
    enums: Vec<Inner>,
}

fn holds_of<'e>(fields: impl Iterator<Item = &'e Element>, mut at: usize) -> Option<Holds> {
    let mut found = Found::default();
    for field in fields {
        fixed_words(field, &mut at, &mut found)?;
    }
    Some(Holds {
        blocks: found.blocks.into_boxed_slice(),
        texts: found.texts.into_boxed_slice(),
        enums: found.enums.into_boxed_slice(),
    })
}

/// How to find the blocks a value of a kind holds.
pub(super) fn plan(element: &Element) -> Plan {
    if let Element::Enum { cases, .. } = element {
        let cases: Option<Vec<Holds>> =
            cases.iter().map(|(_, fields)| holds_of(fields.iter().map(|(_, field)| field), 1)).collect();
        let Some(cases) = cases else { return Plan::Walk };
        if cases.iter().any(|holds| !holds.texts.is_empty() || !holds.fixed()) {
            return Plan::CasesWithTexts(cases.into_boxed_slice());
        }
        return Plan::Cases(cases.into_iter().map(|holds| holds.blocks).collect());
    }
    match holds_of(std::iter::once(element), 0) {
        Some(holds) if holds.texts.is_empty() && holds.fixed() => Plan::Fixed(holds.blocks),
        Some(holds) => Plan::FixedWithTexts(Box::new(holds)),
        None => Plan::Walk,
    }
}

/// How to find the blocks an in-place item of a list holds: how many words an item takes, worked
/// out once, and where they are.
pub(super) struct ItemPlan {
    pub(super) stride: usize,
    pub(super) plan: Plan,
}

/// The plan for items of a kind.
pub(super) fn item_plan(element: &Element) -> ItemPlan {
    ItemPlan { stride: element.words().max(1), plan: plan(element) }
}

/// The case a value's first word says.
fn case_of<T>(cases: &[T], fields: &[u64]) -> Option<usize> {
    let case = usize::try_from(*fields.first()?).ok()?;
    (case < cases.len()).then_some(case)
}

/// The non-zero addresses in a block's fields, if the plan says where they are without
/// walking. `None` means walk it.
pub(super) fn addresses(plan: &Plan, fields: &[u64], found: &mut impl Extend<u64>) -> Option<()> {
    let words = match plan {
        Plan::Fixed(words) => words,
        Plan::Cases(cases) => cases.get(case_of(cases, fields)?)?,
        Plan::FixedWithTexts(holds) => {
            holds_addresses(holds, fields, found);
            return Some(());
        }
        Plan::CasesWithTexts(cases) => {
            holds_addresses(cases.get(case_of(cases, fields)?)?, fields, found);
            return Some(());
        }
        Plan::Walk => return None,
    };
    found.extend(words.iter().filter_map(|at| fields.get(*at).copied()).filter(|address| *address != 0));
    Some(())
}

/// Where among a struct's fields its blocks are, each `base` on from its first field: each word
/// holding a block's address that isn't zero, and the first word of each string held in a block.
/// `None` for an enum's plan, or one that's walked, which says nothing without the value.
pub(super) fn places(plan: &Plan, fields: &[u64], base: usize, found: &mut impl Extend<usize>) -> Option<()> {
    let holds_block = |at: &usize| fields.get(*at).is_some_and(|word| *word != 0);
    match plan {
        Plan::Fixed(words) => found.extend(words.iter().copied().filter(holds_block).map(|at| at + base)),
        // An enum inside decides by its case, which this doesn't follow.
        Plan::FixedWithTexts(holds) if !holds.fixed() => return None,
        Plan::FixedWithTexts(holds) => {
            found.extend(holds.blocks.iter().copied().filter(holds_block).map(|at| at + base));
            found.extend(
                holds
                    .texts
                    .iter()
                    .copied()
                    .filter(|at| {
                        let header =
                            fields.get(*at..*at + 3).and_then(|words| <&[u64; 3]>::try_from(words).ok());
                        header.and_then(super::text::block_of).is_some_and(|block| block != 0)
                    })
                    .map(|at| at + base),
            );
        }
        Plan::Cases(_) | Plan::CasesWithTexts(_) | Plan::Walk => return None,
    }
    Some(())
}

fn holds_addresses(holds: &Holds, fields: &[u64], found: &mut impl Extend<u64>) {
    holds.each_block(fields, &mut |address| found.extend(std::iter::once(address)));
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::{Plan, addresses, item_plan, plan};
    use crate::packed::Element;

    /// A header holding a block at `address`.
    fn in_block(address: u64) -> [u64; 3] {
        [address, 30, 0]
    }

    /// A header holding its text inline.
    const INLINE: [u64; 3] = [0, 0, 0x80 << 56];

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

    /// A string in a header holds a block only when it's too long to be inline, and an optional
    /// one is all zero for `None`.
    #[test]
    fn a_struct_s_strings_are_found_in_their_headers() {
        let unit = Element::Struct {
            name: Rc::from("Unit"),
            fields: vec![
                (Rc::from("name"), Element::Text),
                (Rc::from("hp"), Element::Int),
                (Rc::from("pet"), Element::Maybe(Rc::new(Element::Text))),
                (Rc::from("bag"), Element::Boxed(1)),
            ],
        };
        let plan = plan(&unit);
        assert!(matches!(plan, Plan::FixedWithTexts(_)));
        let mut fields = Vec::new();
        fields.extend(in_block(40));
        fields.push(7);
        fields.extend(in_block(48));
        fields.push(56);
        let mut found = Vec::new();
        addresses(&plan, &fields, &mut found).unwrap();
        assert_eq!(found, [56, 40, 48]);
        let mut fields = Vec::new();
        fields.extend(INLINE);
        fields.push(7);
        fields.extend([0, 0, 0]);
        fields.push(0);
        found.clear();
        addresses(&plan, &fields, &mut found).unwrap();
        assert_eq!(found, Vec::<u64>::new());
    }

    /// A list that names its items by number is one word holding a block, as any list is.
    #[test]
    fn a_list_named_by_number_is_found_as_a_block() {
        let holder = Element::Struct {
            name: Rc::from("Holder"),
            fields: vec![(Rc::from("n"), Element::Int), (Rc::from("items"), Element::ListOf(3))],
        };
        let mut found = Vec::new();
        addresses(&plan(&holder), &[7, 88], &mut found).unwrap();
        assert_eq!(found, [88]);
    }

    /// An enum with no data inside a value takes its words and holds nothing, and one with data is
    /// followed by its case, as an optional of an in-place struct is.
    #[test]
    fn an_enum_inside_is_stepped_over_when_it_holds_nothing_and_followed_by_its_case_otherwise() {
        let plain = Element::Enum {
            name: Rc::from("Kind"),
            cases: vec![(Rc::from("One"), Vec::new()), (Rc::from("Two"), Vec::new())],
            words: 1,
        };
        let holder = Element::Struct {
            name: Rc::from("Holder"),
            fields: vec![(Rc::from("kind"), plain), (Rc::from("name"), Element::Str)],
        };
        let mut found = Vec::new();
        addresses(&plan(&holder), &[1, 64], &mut found).unwrap();
        assert_eq!(found, [64]);
        let holding = Element::Enum {
            name: Rc::from("Order"),
            cases: vec![
                (Rc::from("Hold"), Vec::new()),
                (Rc::from("Say"), vec![(Rc::from("text"), Element::Text)]),
            ],
            words: 4,
        };
        let outer = Element::Struct {
            name: Rc::from("Outer"),
            fields: vec![
                (Rc::from("first"), Element::Str),
                (Rc::from("order"), holding.clone()),
                (Rc::from("last"), Element::Str),
            ],
        };
        let planned = plan(&outer);
        assert!(!matches!(planned, Plan::Walk));
        found.clear();
        let [a, b, c] = in_block(72);
        addresses(&planned, &[40, 1, a, b, c, 48], &mut found).unwrap();
        assert_eq!(found, [40, 48, 72]);
        found.clear();
        addresses(&planned, &[40, 0, 0, 0, 0, 48], &mut found).unwrap();
        assert_eq!(found, [40, 48]);
        found.clear();
        // An item of a list goes the same way, item after item.
        let items = item_plan(&outer);
        assert_eq!(items.stride, 6);
        for item in [[0, 1, a, b, c, 0], [56, 0, 0, 0, 0, 0]] {
            addresses(&items.plan, &item, &mut found).unwrap();
        }
        assert_eq!(found, [72, 56]);
        found.clear();
        let order = plan(&holding);
        found.clear();
        addresses(&order, &[1, a, b, c], &mut found).unwrap();
        assert_eq!(found, [72]);
        found.clear();
        addresses(&order, &[0, 0, 0, 0], &mut found).unwrap();
        assert_eq!(found, Vec::<u64>::new());
        assert!(addresses(&order, &[9, 0, 0, 0], &mut found).is_none());
    }

    /// An item with no fields takes no words, and is still a step of one to walk a list by.
    #[test]
    fn an_item_plan_says_how_far_apart_items_are_and_what_they_hold() {
        let empty = Element::Struct { name: Rc::from("Empty"), fields: Vec::new() };
        let plan = item_plan(&empty);
        assert_eq!(plan.stride, 1);
        assert!(plan.plan.holds_nothing());
        let plan = item_plan(&Element::Text);
        assert_eq!(plan.stride, 3);
        assert!(!plan.plan.holds_nothing());
        let numbers = Element::Struct {
            name: Rc::from("At"),
            fields: vec![(Rc::from("x"), Element::Int), (Rc::from("y"), Element::Float)],
        };
        let plan = item_plan(&numbers);
        assert_eq!(plan.stride, 2);
        assert!(plan.plan.holds_nothing());
        let say = Element::Enum {
            name: Rc::from("Order"),
            cases: vec![
                (Rc::from("Hold"), Vec::new()),
                (Rc::from("Say"), vec![(Rc::from("text"), Element::Text)]),
            ],
            words: 4,
        };
        assert!(!item_plan(&say).plan.holds_nothing());
        let unknown = Element::Struct { name: Rc::from("Outer"), fields: vec![(Rc::from("order"), say)] };
        assert!(!item_plan(&unknown).plan.holds_nothing());
    }
}
