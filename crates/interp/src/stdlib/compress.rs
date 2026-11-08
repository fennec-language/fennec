//! `std.compress`: DEFLATE, and zlib and gzip around it, which the interpreter does in Rust in
//! `crate::compress`. `level=` goes on a function's name, as native code has it:
//! `compress.gzip.level`.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use super::{Module, no_constants};
use crate::compress as codec;
use crate::collections::plain;
use crate::value::Value;

pub(crate) const MODULE: Module = Module {
    functions: &[
        "compress.deflate",
        "compress.gunzip",
        "compress.gzip",
        "compress.inflate",
        "compress.unzlib",
        "compress.zlib",
    ],
    call,
    constant: no_constants,
    methods: &[],
};

/// One of these functions, which need nothing of the interpreter.
pub(crate) fn run(name: &str, values: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let (function, level) = match name.strip_suffix(".level") {
        Some(function) => {
            let Some(Value::Int(level)) = values.get(1) else {
                return Err(Diagnostic::new(format!("`{function}`'s `level` is an int"), span));
            };
            let Ok(level) = u8::try_from(*level).map_err(drop).and_then(|level| if level <= 9 { Ok(level) } else { Err(()) })
            else {
                return Err(Diagnostic::new(format!("`level` is from 0 to 9, not {level}"), span)
                    .with_help("0 stores it as it is, 6 is the usual, and 9 tries hardest"));
            };
            (function, level)
        }
        None => (name, 6),
    };
    let data: &[u8] = match values.first() {
        Some(Value::Bytes(data)) => data,
        Some(Value::Str(text)) => text.as_bytes(),
        _ => return Err(Diagnostic::new(format!("`{function}` takes bytes"), span)),
    };
    let made = |out: Vec<u8>| Value::Bytes(Rc::from(out));
    let read = |found: Result<Vec<u8>, String>, what: &str| match found {
        Ok(out) => Value::Ok(Rc::new(made(out))),
        Err(why) => {
            let message = Value::str(&format!("this isn't {what} data: {why}"));
            Value::Err(Rc::new(Value::made_error("InvalidValue", vec![("message", message)])))
        }
    };
    Ok(match function {
        "compress.deflate" => made(codec::deflate(data, level)),
        "compress.zlib" => made(codec::zlib(data, level)),
        "compress.gzip" => made(codec::gzip(data, level)),
        "compress.inflate" => read(
            codec::inflate(data).and_then(|(out, used)| {
                if used == data.len() { Ok(out) } else { Err("there's more after its end".to_owned()) }
            }),
            "DEFLATE",
        ),
        "compress.unzlib" => read(codec::unzlib(data), "zlib"),
        "compress.gunzip" => read(codec::gunzip(data), "gzip"),
        _ => return Err(Diagnostic::new(format!("`{function}` isn't in std.compress"), span)),
    })
}
