//! The interpreter's bodies for `std.intrinsics`: each does what the operator it stands for
//! does, failing the same way, through the same code. The text ones read a text by where its
//! bytes are.

use fennec_syntax::{BinOp, Diagnostic, Span, UnaryOp};

use crate::ops;
use crate::value::{Id, Slot, Table, Value};

/// Runs the intrinsic named `name` on its arguments.
pub(crate) fn run(name: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    // Imported by name into another std module, it keeps its module's name.
    let name = name.strip_prefix("intrinsics.").unwrap_or(name);
    let binary = |op: BinOp| match args {
        [a, b] => ops::binary(op, a.clone(), b.clone(), span),
        _ => Err(wrong(name, span)),
    };
    if let Some(done) = ids(name, args, span) {
        return done;
    }
    match (name, args) {
        // Channels run between tasks, which only native code has.
        ("channel_new" | "channel_send" | "channel_receive" | "channel_close" | "channel_free", _) => {
            Err(Diagnostic::new("the interpreter can't run channels", span)
                .with_help("build it natively: `fennec run` without `--interp`"))
        }
        // So do launched tasks.
        ("launch" | "launch_end", _) => {
            Err(Diagnostic::new("the interpreter can't run launched tasks", span)
                .with_help("build it natively: `fennec run` without `--interp`"))
        }
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
        ("float_lt", [Value::Float(a), Value::Float(b)]) => {
            Ok(Value::Bool(crate::value::float_order(*a, *b).is_lt()))
        }
        ("int_wrapping_add", [Value::Int(a), Value::Int(b)]) => Ok(Value::Int(a.wrapping_add(*b))),
        ("int_wrapping_sub", [Value::Int(a), Value::Int(b)]) => Ok(Value::Int(a.wrapping_sub(*b))),
        ("int_wrapping_mul", [Value::Int(a), Value::Int(b)]) => Ok(Value::Int(a.wrapping_mul(*b))),
        ("int_to_float", [Value::Int(a)]) => Ok(Value::Float(ops::to_float(*a))),
        ("int_hash", [Value::Int(a)]) => {
            Ok(Value::Int(crate::embed::dicts::hash_int(a.cast_unsigned()).cast_signed()))
        }
        ("float_to_int", [a @ Value::Float(_)]) => crate::builtins::to_int(a, span),
        ("float_bits", [Value::Float(a)]) => Ok(Value::Int(a.to_bits().cast_signed())),
        ("bytes_text", [Value::Bytes(data), Value::Int(start), Value::Int(end)]) => {
            let part = usize::try_from(*start).ok().zip(usize::try_from(*end).ok()).and_then(|(from, to)| data.get(from..to));
            let text = part.and_then(|part| std::str::from_utf8(part).ok()).unwrap_or("");
            Ok(Value::str(text))
        }
        ("text_plain_end" | "text_escape_at", [Value::Str(text), Value::Int(at)]) => {
            let writing = name == "text_escape_at";
            let from = usize::try_from(*at).unwrap_or(0);
            let found = text.bytes().enumerate().skip(from).find(|(_, b)| {
                *b < 32 || *b == 34 || *b == 92 || (writing && (*b == 127 || *b == 194))
            });
            Ok(Value::Int(i64::try_from(found.map_or(text.len(), |(at, _)| at)).unwrap_or(i64::MAX)))
        }
        ("float_of_bits", [Value::Int(bits)]) => Ok(Value::Float(f64::from_bits(bits.cast_unsigned()))),
        // What a type is, which only native code knows as it runs.
        ("shape_hash" | "layout" | "type_name", []) => Err(Diagnostic::new(
            "this saves or loads through a format written in Fennec, like `std.pack`, which needs native code",
            span,
        )
        .with_help("the interpreter has no types while it runs: run this without `--interp` or `--jit`")),
        ("text_byte", [Value::Str(text), Value::Int(at)]) => {
            let byte = usize::try_from(*at).ok().and_then(|at| text.as_bytes().get(at).copied());
            Ok(Value::Int(byte.map_or(-1, i64::from)))
        }
        ("text_of_code", [Value::Int(code)]) => {
            let found = u32::try_from(*code).ok().and_then(char::from_u32);
            found.map(|c| Value::str(c.encode_utf8(&mut [0; 4]))).ok_or_else(|| wrong(name, span))
        }
        ("text_size", [Value::Str(text)]) => Ok(Value::Int(i64::try_from(text.len()).unwrap_or(i64::MAX))),
        ("text_part", [Value::Str(text), Value::Int(start), Value::Int(end)]) => {
            let place = |at: &i64| usize::try_from(*at).ok();
            let part = place(start).zip(place(end)).and_then(|(start, end)| text.get(start..end));
            part.map(Value::str).ok_or_else(|| wrong(name, span))
        }
        ("stop", [Value::Str(message)]) => Err(Diagnostic::new(message.to_string(), span)),
        _ => Err(wrong(name, span)),
    }
}

/// The intrinsics into `Table`s and `Id`s, which std uses to save and load them, or `None` for
/// another.
fn ids(name: &str, args: &[Value], span: Span) -> Option<Result<Value, Diagnostic>> {
    let known = matches!(
        name,
        "id_key"
            | "id_at"
            | "id_tag"
            | "table_tag"
            | "new_tag"
            | "table_tagged"
            | "table_slots"
            | "table_of_slots"
            | "id_of_key"
    );
    known.then(|| id_intrinsic(name, args, span))
}

fn id_intrinsic(name: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    match (name, args) {
        ("id_key", [Value::Id(id)]) => Ok(Value::Int(id.key().cast_signed())),
        ("id_at", [Value::Int(position), Value::Int(tag)]) => {
            let slot = u32::try_from(*position).map_err(|_| wrong(name, span))?;
            Ok(Value::Id(Id { slot, generation: 0, tag: tag.cast_unsigned() }))
        }
        ("id_tag", [Value::Id(id)]) => Ok(Value::Int(id.tag.cast_signed())),
        ("table_tag", [Value::Table(table)]) => Ok(Value::Int(table.tag.cast_signed())),
        ("new_tag", []) => Ok(Value::Int(crate::value::new_tag().cast_signed())),
        ("table_tagged", [Value::Int(tag)]) => {
            Ok(Value::Table(std::rc::Rc::new(Table::tagged(Vec::new(), tag.cast_unsigned()))))
        }
        ("table_slots", [Value::Table(map)]) => Ok(Value::list(
            map.slots
                .iter()
                .map(|slot| {
                    let generation = i64::from(slot.generation);
                    Value::Int(if slot.value.is_some() { generation } else { -1 - generation })
                })
                .collect(),
        )),
        ("table_of_slots", [Value::Int(tag), Value::List(words), Value::List(items)]) => {
            let mut held = items.iter();
            let mut slots = Vec::with_capacity(words.len());
            for word in words.iter() {
                let Value::Int(word) = word else { return Err(wrong(name, span)) };
                let generation = if *word >= 0 { *word } else { -1 - *word };
                let generation = u32::try_from(generation).map_err(|_| wrong(name, span))?;
                let value = if *word >= 0 {
                    Some(held.next().cloned().ok_or_else(|| wrong(name, span))?)
                } else {
                    None
                };
                slots.push(Slot { generation, value });
            }
            Ok(Value::Table(std::rc::Rc::new(Table::tagged(slots, tag.cast_unsigned()))))
        }
        ("id_of_key", [Value::Int(key), Value::Int(tag)]) => {
            Ok(Value::Id(Id::from_key(key.cast_unsigned(), tag.cast_unsigned())))
        }
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
        assert_eq!(float("float_lt", &[-0.0, 0.0]), Ok(Value::Bool(false)));
        assert_eq!(float("float_lt", &[1.0, f64::NAN]), Ok(Value::Bool(true)));
        assert_eq!(float("float_lt", &[-f64::NAN, 1.0]), Ok(Value::Bool(false)));
        assert_eq!(float("float_to_int", &[-2.7]), Ok(Value::Int(-2)));
        assert!(float("float_to_int", &[f64::NAN]).is_err());
        assert_eq!(float("float_bits", &[1.0]), Ok(Value::Int(1.0_f64.to_bits().cast_signed())));
    }
}
