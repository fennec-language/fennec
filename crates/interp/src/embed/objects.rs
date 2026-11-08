//! The numbers native code passes for operators.

use fennec_syntax::BinOp;


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

