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
    use super::super::{heap, lists};
    use super::{FIELDS, LIST_ITEMS, TEXT};

    /// These restate how the runtime lays out blocks, for code generators. They can't drift.
    #[test]
    fn the_offsets_match_the_runtime() {
        let words = |offset: i32| usize::try_from(offset / 8).unwrap_or(0);
        assert_eq!(words(FIELDS), heap::HEADER_WORDS);
        assert_eq!(words(LIST_ITEMS), lists::LIST_HEADER_WORDS);
        assert_eq!(words(TEXT), heap::TEXT_HEADER_WORDS);
    }
}
