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
}
