//! The interpreter's bodies for `std.intrinsics`: each does what the operator it stands for
//! does, failing the same way, through the same code.

use fennec_syntax::{BinOp, Diagnostic, Span, UnaryOp};

use crate::ops;
use crate::value::Value;

/// Runs the intrinsic named `name` on its arguments.
pub(crate) fn run(name: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let binary = |op: BinOp| match args {
        [a, b] => ops::binary(op, a.clone(), b.clone(), span),
        _ => Err(wrong(name, span)),
    };
    match (name, args) {
        ("int_add" | "float_add", _) => binary(BinOp::Add),
        ("int_sub" | "float_sub", _) => binary(BinOp::Sub),
        ("int_mul" | "float_mul", _) => binary(BinOp::Mul),
        ("int_floordiv", _) => binary(BinOp::FloorDiv),
        ("int_rem", _) => binary(BinOp::Mod),
        ("float_div", _) => binary(BinOp::Div),
        ("int_and", _) => binary(BinOp::BitAnd),
        ("int_or", _) => binary(BinOp::BitOr),
        ("int_xor", _) => binary(BinOp::BitXor),
        ("int_shl", _) => binary(BinOp::ShiftLeft),
        ("int_shr", _) => binary(BinOp::ShiftRight),
        ("int_neg" | "float_neg", [a]) => ops::unary(UnaryOp::Neg, a.clone(), span),
        ("int_not", [a]) => ops::unary(UnaryOp::BitNot, a.clone(), span),
        ("int_eq" | "float_eq", [a, b]) => Ok(Value::Bool(a == b)),
        ("int_lt", [Value::Int(a), Value::Int(b)]) => Ok(Value::Bool(a < b)),
        ("float_lt", [Value::Float(a), Value::Float(b)]) => Ok(Value::Bool(a.total_cmp(b).is_lt())),
        ("int_wrapping_add", [Value::Int(a), Value::Int(b)]) => Ok(Value::Int(a.wrapping_add(*b))),
        ("int_wrapping_sub", [Value::Int(a), Value::Int(b)]) => Ok(Value::Int(a.wrapping_sub(*b))),
        ("int_wrapping_mul", [Value::Int(a), Value::Int(b)]) => Ok(Value::Int(a.wrapping_mul(*b))),
        ("int_to_float", [Value::Int(a)]) => Ok(Value::Float(ops::to_float(*a))),
        ("int_hash", [Value::Int(a)]) => {
            Ok(Value::Int(crate::embed::dicts::hash_int(a.cast_unsigned()).cast_signed()))
        }
        ("float_to_int", [a @ Value::Float(_)]) => crate::builtins::to_int(a, span),
        ("float_bits", [Value::Float(a)]) => Ok(Value::Int(a.to_bits().cast_signed())),
        _ => Err(wrong(name, span)),
    }
}

fn wrong(name: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{name}` was called with the wrong arguments"), span)
}

#[cfg(test)]
mod tests {
    use fennec_syntax::Span;

    use super::run;
    use crate::value::Value;

    fn int(name: &str, args: &[i64]) -> Result<Value, String> {
        let args: Vec<Value> = args.iter().map(|n| Value::Int(*n)).collect();
        run(name, &args, Span::default()).map_err(|failure| failure.message)
    }

    #[test]
    fn int_intrinsics_do_what_the_operators_do_and_fail_the_same_way() {
        assert_eq!(int("int_add", &[2, 3]), Ok(Value::Int(5)));
        let overflow = crate::ops::binary(
            fennec_syntax::BinOp::Add,
            Value::Int(i64::MAX),
            Value::Int(1),
            Span::default(),
        )
        .map_err(|failure| failure.message);
        assert_eq!(int("int_add", &[i64::MAX, 1]), overflow);
        assert_eq!(int("int_floordiv", &[-7, 2]), Ok(Value::Int(-4)));
        assert_eq!(int("int_rem", &[-7, 2]), Ok(Value::Int(1)));
        assert!(int("int_floordiv", &[1, 0]).is_err());
        let negated = crate::ops::unary(fennec_syntax::UnaryOp::Neg, Value::Int(i64::MIN), Span::default());
        assert_eq!(int("int_neg", &[i64::MIN]), negated.map_err(|failure| failure.message));
        assert_eq!(int("int_wrapping_add", &[i64::MAX, 1]), Ok(Value::Int(i64::MIN)));
        assert_eq!(int("int_lt", &[1, 2]), Ok(Value::Bool(true)));
        assert_eq!(int("int_xor", &[6, 3]), Ok(Value::Int(5)));
        assert_eq!(int("int_not", &[0]), Ok(Value::Int(-1)));
        assert_eq!(int("int_shl", &[1, 3]), Ok(Value::Int(8)));
        assert_eq!(int("int_shr", &[-8, 1]), Ok(Value::Int(-4)));
        assert_eq!(int("int_to_float", &[3]), Ok(Value::Float(3.0)));
        let hash = crate::embed::dicts::hash_int(7).cast_signed();
        assert_eq!(int("int_hash", &[7]), Ok(Value::Int(hash)));
        assert!(int("int_add", &[1]).is_err());
    }

    #[test]
    fn float_intrinsics_do_what_the_operators_do() {
        let float = |name: &str, args: &[f64]| {
            let args: Vec<Value> = args.iter().map(|x| Value::Float(*x)).collect();
            run(name, &args, Span::default()).map_err(|failure| failure.message)
        };
        assert_eq!(float("float_div", &[1.0, 4.0]), Ok(Value::Float(0.25)));
        assert_eq!(float("float_eq", &[f64::NAN, f64::NAN]), Ok(Value::Bool(true)));
        assert_eq!(float("float_eq", &[-0.0, 0.0]), Ok(Value::Bool(true)));
        assert_eq!(float("float_lt", &[-0.0, 0.0]), Ok(Value::Bool(true)));
        assert_eq!(float("float_to_int", &[-2.7]), Ok(Value::Int(-2)));
        assert!(float("float_to_int", &[f64::NAN]).is_err());
        assert_eq!(float("float_bits", &[1.0]), Ok(Value::Int(1.0_f64.to_bits().cast_signed())));
    }
}
