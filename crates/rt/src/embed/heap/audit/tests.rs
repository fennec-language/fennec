//! Heaps built wrong on purpose, a block at a time, and what the audit says of each.

use std::rc::Rc;

use super::super::super::{Tables, finish, start, with};
use super::super::IMMORTAL;
use super::{Audit, Wrong, audit};
use crate::packed::Element;

/// Starts a run whose only shape is `Node`, a struct holding another `Node` and a string.
fn started() {
    let fields = vec![(Rc::from("next"), Element::Boxed(0)), (Rc::from("name"), Element::Str)];
    let node = Element::Struct { name: Rc::from("Node"), fields };
    let tables =
        Tables { shapes: vec![Rc::new(node), Rc::new(Element::Str)], str_shape: 1, ..Tables::default() };
    start(tables, crate::Inputs::default());
}

fn made(words: Vec<u64>) -> u64 {
    with(|state| u64::try_from(state.heap.insert(words.into_boxed_slice())).unwrap_or(0))
}

/// A string's block with `count` owners.
fn text(count: u64) -> u64 {
    made(vec![count, 1, 8, 8, 8, 0, u64::from_le_bytes(*b"a string")])
}

/// A `Node`'s block with `count` owners, holding the blocks at `next` and `name`.
fn node(count: u64, next: u64, name: u64) -> u64 {
    made(vec![count, 0, next, name])
}

fn points(at: u64, next: u64) {
    with(|state| {
        let field = usize::try_from(at).ok().and_then(|at| state.heap.blocks.get_mut(at)?.get_mut(2));
        if let Some(field) = field {
            *field = next;
        }
    });
}

fn audited(held: &[u64]) -> Audit {
    with(|state| audit(state, held))
}

#[test]
fn a_heap_counted_right_has_nothing_wrong() {
    started();
    let name = text(2);
    let first = node(1, 0, name);
    let second = node(2, first, name);
    let found = audited(&[second, second]);
    assert_eq!(found, Audit { live: 3, wrong: Vec::new() });
    assert_eq!(found.said(), "");
    finish();
}

#[test]
fn a_block_nothing_holds_is_leaked_with_all_it_holds() {
    started();
    let name = text(1);
    let first = node(1, 0, name);
    let second = node(1, first, 0);
    let found = audited(&[]);
    assert_eq!(found.live, 3);
    let [leaked] = &found.wrong[..] else { panic!("{found:?}") };
    assert_eq!((leaked.wrong, leaked.address, leaked.kind.as_str()), (Wrong::Leaked, second, "Node"));
    assert_eq!((leaked.stored, leaked.found, leaked.under), (1, 0, 2));
    assert_eq!(
        found.said(),
        format!(
            "heap audit: 1 wrong among 3 live blocks\nheap audit: leaked: Node at {second:#x} is counted 1 and \
             nothing holds it, and 2 more blocks are held only through it\nheap audit: native blocks and blocks \
             behind a lock aren't checked\n"
        )
    );
    finish();
}

#[test]
fn a_count_above_its_holders_is_said_with_the_way_to_it() {
    started();
    let name = text(2);
    let inner = node(1, 0, name);
    let outer = node(1, inner, 0);
    let found = audited(&[outer]);
    let [high] = &found.wrong[..] else { panic!("{found:?}") };
    assert_eq!((high.wrong, high.address, high.kind.as_str()), (Wrong::CountedHigh, name, "str"));
    assert_eq!((high.stored, high.found), (2, 1));
    assert_eq!(high.way, vec![(outer, "Node".to_owned()), (inner, "Node".to_owned())]);
    assert!(found.said().contains(&format!(
        "counted too high: str at {name:#x} is counted 2 and held 1 times, by way of Node at {outer:#x} -> Node \
         at {inner:#x}\n"
    )));
    finish();
}

#[test]
fn a_count_below_its_holders_is_counted_too_low() {
    started();
    let name = text(1);
    let first = node(1, 0, name);
    let second = node(1, 0, name);
    let found = audited(&[first, second]);
    let [low] = &found.wrong[..] else { panic!("{found:?}") };
    assert_eq!((low.wrong, low.address, low.stored, low.found), (Wrong::CountedLow, name, 1, 2));
    assert_eq!(low.way.len(), 1);
    finish();
}

#[test]
fn blocks_that_hold_each_other_and_nothing_else_does_are_a_ring() {
    started();
    let first = node(1, 0, 0);
    let second = node(1, first, 0);
    points(first, second);
    let found = audited(&[]);
    let [ring] = &found.wrong[..] else { panic!("{found:?}") };
    assert_eq!((ring.wrong, ring.stored, ring.found, ring.under), (Wrong::Ring, 1, 1, 1));
    assert_eq!(ring.address, first.min(second));
    finish();
}

#[test]
fn an_address_held_with_no_block_there_is_gone() {
    started();
    let nowhere = 8 * 1_234_567;
    let holder = node(1, 0, nowhere);
    let found = audited(&[holder]);
    let [gone] = &found.wrong[..] else { panic!("{found:?}") };
    assert_eq!((gone.wrong, gone.address, gone.found), (Wrong::Gone, nowhere, 1));
    assert_eq!(gone.way, vec![(holder, "Node".to_owned())]);
    finish();
}

#[test]
fn a_block_thats_never_freed_is_a_root_and_is_never_compared() {
    started();
    let held_by_literal = text(1);
    let literal = node(IMMORTAL, 0, held_by_literal);
    // Its count is whatever retains and releases have left it at.
    let other = text(IMMORTAL + 5);
    let leaked = node(1, literal, other);
    let found = audited(&[]);
    assert_eq!(found.live, 2);
    let [only] = &found.wrong[..] else { panic!("{found:?}") };
    assert_eq!((only.wrong, only.address, only.under), (Wrong::Leaked, leaked, 0));
    finish();
}

#[test]
fn only_the_first_blocks_found_wrong_are_said_and_the_rest_counted() {
    started();
    for _ in 0..super::SAID + 3 {
        text(1);
    }
    let found = audited(&[]);
    assert_eq!(found.wrong.len(), super::SAID + 3);
    let said = found.said();
    assert_eq!(said.lines().count(), super::SAID + 3);
    assert!(said.contains("heap audit: and 3 more\n"), "{said}");
    finish();
}
