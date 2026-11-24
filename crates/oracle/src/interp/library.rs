//! The parts of the standard library the oracle models on its own: `std.math`'s constants, the
//! functions whose result IEEE 754 fixes exactly, and those `docs/language.md` says the `libm`
//! crate computes, with that crate, `std.json`'s `dumps` and `loads`, in `json.rs`, `std.path`'s
//! `Path`, in `paths.rs`, and `std.fs`, in `files.rs`. The rest of `std.math`, its vectors and
//! matrices, is run from its own Fennec source (`RUN_FROM_SOURCE` in `load.rs`). The rest of the
//! library is unsupported.

use fennec_syntax::Span;

use super::{Run, crash, unsupported};
use crate::value::{Held, Text, Value};

/// A constant of the standard library, by its whole name, like `std.math.PI`, or a case of one
/// of `std.json`'s option enums, like `std.json.JsonNames.CamelCase`.
pub(crate) fn constant(path: &str) -> Option<Value> {
    if let Some((ty, case)) = path.strip_prefix("std.json.").and_then(|rest| rest.split_once('.')) {
        return super::json::option_case(ty, case);
    }
    Some(Value::Float(match path {
        "std.math.PI" => std::f64::consts::PI,
        "std.math.INFINITY" => f64::INFINITY,
        "std.math.NAN" => f64::NAN,
        _ => return None,
    }))
}

/// Whether the oracle models a function of the standard library, by its whole name.
pub(crate) fn has_function(path: &str) -> bool {
    matches!(
        path,
        "std.math.sqrt"
            | "std.math.sin"
            | "std.math.cos"
            | "std.math.tan"
            | "std.math.acos"
            | "std.math.exp"
            | "std.math.log"
            | "std.math.floor"
            | "std.math.ceil"
            | "std.math.lerp"
            | "std.json.dumps"
            | "std.json.loads"
            | "std.path.Path"
            | "std.intrinsics.text_byte"
            | "std.intrinsics.text_size"
            | "std.intrinsics.text_part"
            | "std.intrinsics.text_of_code"
            | "std.intrinsics.bytes_text"
            | "std.intrinsics.channel_new"
            | "std.intrinsics.channel_new_every"
            | "std.intrinsics.channel_listen"
            | "std.intrinsics.channel_send"
            | "std.intrinsics.channel_receive"
            | "std.intrinsics.channel_close"
            | "std.intrinsics.channel_free"
            | "std.intrinsics.id_tag"
            | "std.intrinsics.table_tag"
            | "std.intrinsics.id_key"
            | "std.intrinsics.text_plain_end"
            | "std.intrinsics.text_escape_at"
            | "std.intrinsics.text_same_at"
            | "std.intrinsics.stop"
            | "std.intrinsics.bytes_builder"
            | "std.intrinsics.bytes_push"
            | "std.intrinsics.bytes_push_word"
            | "std.intrinsics.bytes_push_varint"
            | "std.intrinsics.bytes_push_text"
            | "std.intrinsics.bytes_push_bytes"
            | "std.intrinsics.bytes_set"
            | "std.intrinsics.float_bits"
            | "std.intrinsics.float_of_bits"
            | "std.intrinsics.pack_lock"
            | "std.intrinsics.new_tag"
            | "std.random.random_int"
            | "std.random.random_float"
            | "std.random.random_choice"
            | "std.crypto.equal"
    )
}

/// Calls a function of the standard library the oracle models.
pub(crate) fn call(path: &str, args: &[Value], span: Span) -> Run<Value> {
    // `std.math`'s functions take floats, and an int given one is taken as the float it is, as
    // `sqrt(16)` is 4.0.
    let floats: Vec<Value>;
    let args = if path.starts_with("std.math.") && args.iter().any(|arg| matches!(arg, Value::Int(_))) {
        floats = args
            .iter()
            .map(|arg| match arg {
                Value::Int(n) => Value::Float(crate::text::int_as_float(*n)),
                other => other.clone(),
            })
            .collect();
        floats.as_slice()
    } else {
        args
    };
    if let Some(name) = path.strip_prefix("std.intrinsics.") {
        return intrinsic(name, args, span);
    }
    if let Some(name) = path.strip_prefix("std.random.") {
        return random(name, args, span);
    }
    // `crypto.equal`: whether two texts, or two runs of bytes, are the same. It takes a time that
    // depends only on their lengths, which a program can't see.
    if path == "std.crypto.equal" {
        return match args {
            [Value::Str(a), Value::Str(b)] => Ok(Value::Bool(a == b)),
            [Value::Bytes(a), Value::Bytes(b)] => Ok(Value::Bool(a == b)),
            _ => unsupported("`crypto.equal` of something other than two texts or two bytes"),
        };
    }
    match (path, args) {
        ("std.math.sqrt", [Value::Float(x)]) => {
            if *x < 0.0 {
                return crash("can't take the square root of a negative number", span);
            }
            Ok(Value::Float(x.sqrt()))
        }
        // As `docs/language.md` says, `libm` computes these, the same bits on every platform.
        // Past -1 or 1 `acos` is NaN, and so is `log` below 0: neither stops the program.
        ("std.math.sin", [Value::Float(x)]) => Ok(Value::Float(libm::sin(*x))),
        // An `f32` is computed in `f32`, by `libm`'s own, as native code does.
        ("std.math.sqrt", [Value::F32(x)]) => {
            if *x < 0.0 {
                return crash("can't take the square root of a negative number", span);
            }
            Ok(Value::F32(libm::sqrtf(*x)))
        }
        ("std.math.sin", [Value::F32(x)]) => Ok(Value::F32(libm::sinf(*x))),
        ("std.math.cos", [Value::F32(x)]) => Ok(Value::F32(libm::cosf(*x))),
        ("std.math.acos", [Value::F32(x)]) => Ok(Value::F32(libm::acosf(*x))),
        ("std.math.cos", [Value::Float(x)]) => Ok(Value::Float(libm::cos(*x))),
        ("std.math.tan", [Value::Float(x)]) => Ok(Value::Float(libm::tan(*x))),
        ("std.math.acos", [Value::Float(x)]) => Ok(Value::Float(libm::acos(*x))),
        ("std.math.exp", [Value::Float(x)]) => Ok(Value::Float(libm::exp(*x))),
        ("std.math.log", [Value::Float(x)]) => Ok(Value::Float(libm::log(*x))),
        ("std.math.floor", [Value::Float(x)]) => {
            super::builtins::float_to_int(x.floor(), span).map(Value::Int)
        }
        ("std.math.ceil", [Value::Float(x)]) => super::builtins::float_to_int(x.ceil(), span).map(Value::Int),
        ("std.math.lerp", [Value::Float(start), Value::Float(end), Value::Float(amount)]) => {
            Ok(Value::Float(start + (end - start) * amount))
        }
        ("std.path.Path", [Value::Str(text)]) => Ok(super::files::path_value(text.as_str().to_owned())),
        _ => unsupported(path.to_owned()),
    }
}

/// An intrinsic the standard library's own code calls, each as `intrinsics.fen` says it works.
/// A `Table`'s tag as the int a program reads it as.
fn tag_as_int(tag: u64) -> Run<i64> {
    i64::try_from(tag).map_or_else(|_| unsupported("a `Table` tag past the largest int"), Ok)
}

fn intrinsic(name: &str, args: &[Value], span: Span) -> Run<Value> {
    match (name, args) {
        ("text_byte", [Value::Str(text), Value::Int(at)]) => Ok(Value::Int(
            usize::try_from(*at)
                .ok()
                .and_then(|at| text.as_bytes().get(at))
                .map_or(-1, |byte| i64::from(*byte)),
        )),
        ("text_size", [Value::Str(text)]) => Ok(Value::Int(super::as_int(text.len()))),
        ("text_part", [Value::Str(text), Value::Int(start), Value::Int(end)]) => {
            let part = usize::try_from(*start)
                .ok()
                .zip(usize::try_from(*end).ok())
                .and_then(|(start, end)| text.get(start..end));
            match part {
                Some(part) => Ok(Value::Str(Text::from(part.to_owned()))),
                None => unsupported("`text_part` at places that aren't where characters start"),
            }
        }
        ("text_of_code", [Value::Int(code)]) => match u32::try_from(*code).ok().and_then(char::from_u32) {
            Some(c) => Ok(Value::Str(Text::from(c.to_string()))),
            None => unsupported("`text_of_code` of a code no character has"),
        },
        ("bytes_text", [Value::Bytes(data), Value::Int(start), Value::Int(end)]) => {
            let part = usize::try_from(*start)
                .ok()
                .zip(usize::try_from(*end).ok())
                .and_then(|(start, end)| data.get(start..end));
            match part {
                Some(part) => Ok(Value::Str(Text::from(
                    std::str::from_utf8(part).map(str::to_owned).unwrap_or_default(),
                ))),
                None => unsupported("`bytes_text` at places outside the bytes"),
            }
        }
        // From byte `at` on, the first byte a JSON reader looks closer at, `"`, `\` or a control
        // character, or the first JSON writes escaped or that starts a character it might, which
        // adds DEL and 194, the first byte of U+0080 to U+00BF. Or the text's size.
        ("text_plain_end" | "text_escape_at", [Value::Str(text), Value::Int(at)]) => {
            let Some(rest) = usize::try_from(*at).ok().and_then(|at| text.as_bytes().get(at..)) else {
                return unsupported(format!("`{name}` from past the text's end"));
            };
            let escaped = |byte: u8| byte == b'"' || byte == b'\\' || byte < 0x20;
            let found = rest.iter().position(|&byte| {
                escaped(byte) || (name == "text_escape_at" && (byte == 0x7f || byte == 194))
            });
            let size = text.len();
            Ok(Value::Int(super::as_int(found.map_or(size, |found| size - rest.len() + found))))
        }
        // Whether the text's bytes from `start` begin with all of `word`'s: false from a negative
        // start, or where `word` would run past the end.
        ("text_same_at", [Value::Str(text), Value::Int(start), Value::Str(word)]) => Ok(Value::Bool(
            usize::try_from(*start)
                .ok()
                .and_then(|start| text.as_bytes().get(start..))
                .is_some_and(|rest| rest.starts_with(word.as_bytes())),
        )),
        // The tag of the `Table` an `Id` is from, and a `Table`'s own, which its `Id`s have.
        ("id_tag", [Value::Id(id)]) => Ok(Value::Int(tag_as_int(id.tag)?)),
        ("table_tag", [Value::Table(table)]) => Ok(Value::Int(tag_as_int(table.tag)?)),
        // An `Id`'s slot and generation in one number, the slot above the generation's 32 bits,
        // as `id_of_key` takes it back.
        ("id_key", [Value::Id(id)]) => match i64::try_from(id.slot) {
            Ok(slot) if slot < 1 << 31 => Ok(Value::Int(slot << 32 | i64::from(id.generation))),
            _ => unsupported("an `Id` whose slot doesn't fit in 31 bits"),
        },
        ("stop", [Value::Str(message)]) => crash(message.clone(), span),
        // An empty byte builder, which the writing intrinsics, in `builtins.rs`, write into.
        ("bytes_builder", []) => Ok(Value::Bytes(Vec::new())),
        // A float's bits as an int, and back, as `intrinsics.fen` says.
        ("float_bits", [Value::Float(x)]) => Ok(Value::Int(x.to_bits().cast_signed())),
        ("float_of_bits", [Value::Int(bits)]) => Ok(Value::Float(f64::from_bits(bits.cast_unsigned()))),
        // A program the oracle runs has no `pack.lock`: a kit with one is refused as it loads
        // (`kit.rs`), and a single file has none.
        ("pack_lock", []) => Ok(Value::Str(Text::from(String::new()))),
        _ => unsupported(format!("std.intrinsics.{name}")),
    }
}

/// `Random`'s draws, as `docs/language.md` says: the `n`th number of a seed's `SplitMix64`
/// sequence, and a range by Lemire's method, drawing again for the few that would bias it.
fn random(name: &str, args: &[Value], span: Span) -> Run<Value> {
    match (name, args) {
        ("random_int", [Value::Int(seed), Value::Int(drawn), Value::Int(low), Value::Int(high)]) => {
            if low > high {
                return crash(
                    format!("can't pick a number from {low} to {high}: `low` is bigger than `high`"),
                    span,
                );
            }
            let count = high.wrapping_sub(*low).wrapping_add(1).cast_unsigned();
            let (offset, after) = below(*seed, *drawn, count);
            Ok(Value::Tuple(Held::new(vec![
                Value::Int(low.wrapping_add(offset.cast_signed())),
                Value::Int(after),
            ])))
        }
        ("random_float", [Value::Int(seed), Value::Int(drawn)]) => {
            // The top 53 bits, over 2 to the 53rd: from 0.0 up to, but not including, 1.0.
            #[expect(clippy::cast_precision_loss, reason = "53 bits fit a float's mantissa exactly")]
            let fraction = (splitmix(*seed, *drawn) >> 11) as f64 / 9_007_199_254_740_992.0;
            Ok(Value::Tuple(Held::new(vec![Value::Float(fraction), Value::Int(drawn.saturating_add(1))])))
        }
        ("random_choice", [Value::Int(seed), Value::Int(drawn), Value::List(items)]) => {
            if items.is_empty() {
                return crash("can't choose from an empty list", span);
            }
            let (at, after) = below(*seed, *drawn, u64::try_from(items.len()).unwrap_or(u64::MAX));
            let item = usize::try_from(at).ok().and_then(|at| items.get(at)).cloned().unwrap_or(Value::None);
            Ok(Value::Tuple(Held::new(vec![item, Value::Int(after)])))
        }
        _ => unsupported(format!("std.random.{name}")),
    }
}

/// The `n`th 64-bit number of `seed`'s `SplitMix64` sequence, which needs no state.
fn splitmix(seed: i64, n: i64) -> u64 {
    let mut z = seed
        .cast_unsigned()
        .wrapping_add(n.cast_unsigned().wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A number from 0 up to `count`, or any 64-bit one for a `count` of 0, the whole range, drawn
/// from `seed`'s sequence from place `drawn` by Lemire's method, and the place after the last
/// number it drew: a draw whose low half is under `2^64 mod count` would bias it, and is drawn
/// again.
fn below(seed: i64, drawn: i64, count: u64) -> (u64, i64) {
    let threshold = if count == 0 { 0 } else { count.wrapping_neg() % count };
    let mut at = drawn;
    loop {
        let bits = splitmix(seed, at);
        at = at.saturating_add(1);
        if count == 0 {
            return (bits, at);
        }
        let wide = u128::from(bits) * u128::from(count);
        let low = u64::try_from(wide & u128::from(u64::MAX)).unwrap_or(0);
        if low >= threshold {
            return (u64::try_from(wide >> 64).unwrap_or(0), at);
        }
    }
}
