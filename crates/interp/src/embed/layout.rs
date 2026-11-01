//! The layout of the runtime's blocks, for code generators that read them directly, and the
//! rules for doing that. Offsets are in bytes from a block's address.
//!
//! - **Owner counts aren't atomic.** A program runs on one thread.
//! - **Retaining** adds one to the count at [`COUNT`], and **releasing** subtracts one. Both can
//!   be done inline, without asking the runtime.
//! - **`fennec_free` is called only when a release reaches zero.** It releases what the block
//!   holds, then lets the block go.
//! - **A block counted [`super::heap::IMMORTAL`] is safe to do this to**: it's so high that no
//!   program gets it near zero, so it's never freed.
//! - **An address of zero holds nothing.** Check for it before retaining or releasing wherever
//!   a value can be missing.

/// A block's owner count.
pub const COUNT: i32 = 0;
/// A block's shape number.
pub const SHAPE: i32 = 8;
/// Where a boxed value's fields start.
pub const FIELDS: i32 = 16;

/// How many items a list has.
pub const LIST_LENGTH: i32 = 16;
/// How many items a list has room for.
pub const LIST_ROOM: i32 = 24;
/// Where a list's items start.
pub const LIST_ITEMS: i32 = 32;

/// A string's length in bytes.
pub const TEXT_BYTES: i32 = 16;
/// A string's length in characters.
pub const TEXT_CHARS: i32 = 24;
/// How many bytes a string has room for.
pub const TEXT_ROOM: i32 = 32;
/// Where a string's UTF-8 starts.
pub const TEXT: i32 = 40;

/// A generator's block, whose words come after its count and shape like a boxed value's. Every
/// generator's start the same way, so a loop can read one without knowing which it is. A
/// `Layout`'s field offsets count from [`FIELDS`], so add that to read one.
///
/// This is the address of the function that resumes it. Only generated code calls that, so
/// each backend can use its own calling convention.
pub const GEN_RESUME: i32 = 16;
/// Where it goes on from: 0 to start, positive from a `yield`, -1 finished, -2 running, -3 failed.
pub const GEN_PC: i32 = 24;
/// Whether anything has started using it.
pub const GEN_STARTED: i32 = 32;
/// What it failed with.
pub const GEN_FAILURE: i32 = 40;
/// The item it last gave.
pub const GEN_ITEM: i32 = 48;

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use fennec_syntax::Module;

    use super::super::{Tables, finish, heap, lists, start, strings, with};
    use super::{FIELDS, LIST_ITEMS, LIST_LENGTH, LIST_ROOM, TEXT};
    use crate::packed::Element;

    /// Reads a word of a block.
    fn word(address: i64, offset: i32) -> Option<u64> {
        with(|state| {
            let block = state.heap.block(u64::try_from(address).ok()?)?;
            block.get(usize::try_from(offset / 8).ok()?).copied()
        })
    }

    fn text_of(address: i64) -> String {
        with(|state| {
            u64::try_from(address)
                .ok()
                .and_then(|at| state.heap.block(at))
                .map_or_else(String::new, heap::block_text)
        })
    }

    #[test]
    fn a_list_with_room_is_empty_and_has_it() {
        let tables = Tables { elements: vec![Rc::new(Element::Int)], ..Tables::default() };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        let list = lists::with_room(0, 100);
        assert_eq!(word(list, LIST_LENGTH), Some(0));
        assert_eq!(word(list, LIST_ROOM), Some(100));
        finish();
    }

    #[test]
    fn numbers_format_without_a_temporary() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        let (two, comma) = with(|state| (heap::text_address(state, ".2f"), heap::text_address(state, ",")));
        assert_eq!(text_of(strings::float_spec_text(1.5, two, 0, 0)), "1.50");
        assert_eq!(text_of(strings::int_spec_text(1000, comma, 0, 0)), "1,000");
        finish();
    }

    /// These restate how the runtime lays out blocks, for code generators. They can't drift.
    #[test]
    fn the_offsets_match_the_runtime() {
        let words = |offset: i32| usize::try_from(offset / 8).unwrap_or(0);
        assert_eq!(words(FIELDS), heap::HEADER_WORDS);
        assert_eq!(words(LIST_ITEMS), lists::LIST_HEADER_WORDS);
        assert_eq!(words(TEXT), heap::TEXT_HEADER_WORDS);
    }

    fn set_word(address: i64, offset: i32, value: u64) {
        with(|state| {
            let at = u64::try_from(address).unwrap_or(0);
            let word = usize::try_from(offset / 8).unwrap_or(0);
            if let Some(slot) = state.heap.block_mut(at).and_then(|block| block.get_mut(word)) {
                *slot = value;
            }
        });
    }

    thread_local! {
        static CLOSED: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
    }

    /// Closes a generator, calling back into the runtime as program code would.
    extern "C" fn close(generator: i64) {
        assert!(word(generator, super::GEN_PC).is_some_and(|pc| pc > 0 && pc < 10));
        let text = strings::int_text(5_000);
        heap::free(text);
        CLOSED.set(CLOSED.get() + 1);
    }

    fn generator_shape() -> Element {
        let int = || (Rc::from("word"), Element::Int);
        Element::Struct { name: Rc::from("gen"), fields: vec![int(), int(), int(), int(), int()] }
    }

    #[test]
    fn a_generator_stopped_at_a_yield_is_closed_when_it_is_freed() {
        let shapes = vec![Rc::new(generator_shape()), Rc::new(Element::Str)];
        let tables = Tables { shapes, str_shape: 1, ..Tables::default() };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        heap::set_abandon(0, close);
        CLOSED.set(0);
        let not_started = heap::alloc(0);
        heap::free(not_started);
        assert_eq!(CLOSED.get(), 0);
        let stopped = heap::alloc(0);
        set_word(stopped, super::GEN_PC, 3);
        heap::free(stopped);
        assert_eq!(CLOSED.get(), 1);
        assert_eq!(word(stopped, 0), None);
        finish();
    }

    #[test]
    fn a_generator_inside_a_list_inside_a_struct_is_closed_too() {
        let items = Rc::new(Element::Boxed(0));
        let holder = Element::Struct {
            name: Rc::from("holder"),
            fields: vec![(Rc::from("items"), Element::List(Rc::clone(&items)))],
        };
        let shapes = vec![Rc::new(generator_shape()), Rc::new(holder), Rc::new(Element::Str)];
        let tables = Tables { shapes, str_shape: 2, elements: vec![items], ..Tables::default() };
        start(&Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        heap::set_abandon(0, close);
        CLOSED.set(0);
        let generator = heap::alloc(0);
        set_word(generator, super::GEN_PC, 1);
        let list = lists::new(0, 1);
        set_word(list, LIST_ITEMS, u64::try_from(generator).unwrap());
        let holder = heap::alloc(1);
        set_word(holder, FIELDS, u64::try_from(list).unwrap());
        heap::free(holder);
        assert_eq!(CLOSED.get(), 1);
        assert_eq!(word(generator, 0), None);
        finish();
    }
}
