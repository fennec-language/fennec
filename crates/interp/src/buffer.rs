//! `Buffer[T]` as the interpreter holds it: its items, and how many it has room for. The room
//! follows the runtime's rule for a list's block exactly, so `capacity()` gives the same
//! number whichever way a program runs.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use crate::call::CallArg;
use crate::value::Value;

/// The fewest items a new buffer or list has room for.
pub(crate) const MIN_ROOM: usize = 4;

/// The room a buffer or list grows to when `more` items don't fit after the `count` it has:
/// twice what it held, as Rust's `Vec` grows, or what it needs if that's more. From the least
/// room, a list one item at a time doubles through powers of two.
pub(crate) fn grown_room(count: usize, more: usize) -> usize {
    (2 * count).max(count + more)
}

#[derive(Clone, Debug)]
pub(crate) struct Buffer {
    pub(crate) items: Vec<Value>,
    pub(crate) room: usize,
}

/// Buffers are equal when their items are: how much room each has doesn't matter.
impl PartialEq for Buffer {
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items
    }
}

impl Buffer {
    /// An empty buffer with room for at least `room` items.
    pub(crate) fn with_room(room: usize) -> Buffer {
        Buffer { items: Vec::new(), room: room.max(MIN_ROOM) }
    }

    /// Makes room for `more` items after the ones it has, as the runtime does.
    fn grow(&mut self, more: usize) {
        if self.items.len() + more > self.room {
            self.room = grown_room(self.items.len(), more);
        }
    }

    /// Fills the next slot, growing first if every slot is filled.
    pub(crate) fn push(&mut self, item: Value) {
        self.grow(1);
        self.items.push(item);
    }

    /// Adds `more` items after its own, growing once for all of them.
    pub(crate) fn extend(&mut self, more: &[Value]) {
        self.grow(more.len());
        self.items.extend_from_slice(more);
    }

    /// Makes room for at least `room` items in all.
    pub(crate) fn reserve(&mut self, room: usize) {
        self.room = self.room.max(room);
    }
}

/// A method that changes a buffer.
pub(crate) fn change(
    buffer: &mut Buffer,
    method: &str,
    args: &[Value],
    span: Span,
) -> Result<Value, Diagnostic> {
    // A negative count or room is taken as none, as the runtime takes it.
    let count = |n: &i64| usize::try_from(*n).unwrap_or(0);
    match (method, args) {
        ("push", [item]) => buffer.push(item.clone()),
        ("pop", []) => {
            return buffer.items.pop().ok_or_else(|| Diagnostic::new("can't pop from an empty Buffer", span));
        }
        ("reserve", [Value::Int(room)]) => buffer.reserve(count(room)),
        ("extend", [Value::Buffer(other)]) => buffer.extend(&other.items),
        ("insert", [Value::Int(at), item]) => {
            let length = buffer.items.len();
            let at = usize::try_from(*at).ok().filter(|at| *at <= length).ok_or_else(|| {
                Diagnostic::new(format!("can't insert at {at} in a Buffer of length {length}"), span)
            })?;
            buffer.grow(1);
            buffer.items.insert(at, item.clone());
        }
        ("remove", [Value::Int(at)]) => {
            let at = crate::ops::position(*at, buffer.items.len(), span)?;
            return Ok(buffer.items.remove(at));
        }
        ("truncate", [Value::Int(length)]) => buffer.items.truncate(count(length)),
        _ => return Err(Diagnostic::new(format!("`{method}` was called with the wrong arguments"), span)),
    }
    Ok(Value::Unit)
}

/// A method that reads a buffer: how many items it has room for, and whether nothing else
/// holds it. That's only ever true when nothing does, and may be false when unsure, so a
/// program can use it to pick a faster way but its output can't depend on it.
pub(crate) fn read(
    buffer: &Rc<Buffer>,
    method: &str,
    args: &[Value],
    span: Span,
) -> Result<Value, Diagnostic> {
    match (method, args) {
        ("capacity", []) => Ok(Value::Int(i64::try_from(buffer.room).unwrap_or(i64::MAX))),
        ("is_unique", []) => Ok(Value::Bool(Rc::strong_count(buffer) == 1)),
        _ => Err(Diagnostic::new(format!("a Buffer has no method `{method}`"), span)),
    }
}

/// `Buffer[T]()` or `Buffer[T](capacity=n)`.
pub(crate) fn make(args: &[CallArg], span: Span) -> Result<Value, Diagnostic> {
    let room = match args {
        [] => 0,
        [CallArg { name: Some(name), value: Value::Int(room), .. }] if name == "capacity" => {
            usize::try_from(*room).unwrap_or(0)
        }
        _ => return Err(Diagnostic::new("`Buffer` was called with the wrong arguments", span)),
    };
    Ok(Value::Buffer(Rc::new(Buffer::with_room(room))))
}
