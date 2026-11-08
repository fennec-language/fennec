//! Direct operations on objects: reading fields, operators, indexing, telling types apart for
//! method calls, and changing a part of a name in place. Each is one call, with no syntax to
//! build or walk.

use fennec_syntax::{BinOp, UnaryOp};

use super::{State, attempt, span, with};
;
use crate::value::Value;

/// The operators native code passes by number, in order.
const BINARY: [BinOp; 17] = [
    BinOp::Add,
    BinOp::Sub,
    BinOp::Mul,
    BinOp::Div,
    BinOp::FloorDiv,
    BinOp::Mod,
    BinOp::Pow,
    BinOp::Eq,
    BinOp::NotEq,
    BinOp::Lt,
    BinOp::LtEq,
    BinOp::Gt,
    BinOp::GtEq,
    BinOp::In,
    BinOp::NotIn,
    BinOp::Is,
    BinOp::IsNot,
];

/// The number native code passes for an operator, or `None` for `and` and `or`, which
/// native code always does itself.
pub fn binary_code(op: BinOp) -> Option<i64> {
    BINARY.iter().position(|known| *known == op).and_then(|at| i64::try_from(at).ok())
}

/// `-value` (`negate` 1) or `not value` (`negate` 0).
pub extern "C" fn unary(negate: i64, value: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let op = if negate != 0 { UnaryOp::Neg } else { UnaryOp::Not };
        let value = crate::ops::unary(op, state.take(value), span(start, end))?;
        Ok(state.push(value))
    })
}

/// The name of an object's type, by its number in the name table, or -1 when it isn't an
/// instance of a struct, enum or error, or its type's name isn't in the table.
pub extern "C" fn type_of(value: i64) -> i64 {
    with(|state| {
        let raw = state.raw(value);
        if let Some(name) = super::heap::lent_type(state, &raw) {
            return state.name_ids.get(name.as_ref()).copied().unwrap_or(-1);
        }
        match state.expanded(raw) {
            Value::Instance(instance) => {
                state.name_ids.get(instance.type_name.as_ref()).copied().unwrap_or(-1)
            }
            _ => -1,
        }
    })
}

/// Starts a path into a name, like `world.player`, for `put`.
pub extern "C" fn path(root: i64) {
    with(|state| {
        let root = state.name(root);
        state.path = Some(Place { root, steps: Vec::new(), span: fennec_syntax::Span::default() });
    });
}

/// Adds a field to the path.
pub extern "C" fn path_field(name: i64) {
    with(|state| {
        let name = state.name(name);
        if let Some(place) = &mut state.path {
            place.steps.push(Step::Field(name));
        }
    });
}

/// Adds an index to the path.
pub extern "C" fn path_index(at: i64) {
    with(|state| {
        let at = state.take(at);
        if let Some(place) = &mut state.path {
            place.steps.push(Step::Index(at));
        }
    });
}
