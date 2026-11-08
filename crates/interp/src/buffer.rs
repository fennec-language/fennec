//! `Buffer[T]` as the interpreter holds it: its items, and how many it has room for. The room
//! follows the runtime's rule for a list's block exactly, so `capacity()` gives the same
//! number whichever way a program runs.

/// The fewest items a new buffer or list has room for.
pub(crate) const MIN_ROOM: usize = 4;

/// The room a buffer or list grows to when `more` items don't fit after the `count` it has:
/// twice what it held, as Rust's `Vec` grows, or what it needs if that's more. From the least
/// room, a list one item at a time doubles through powers of two.
pub(crate) fn grown_room(count: usize, more: usize) -> usize {
    (2 * count).max(count + more)
}
