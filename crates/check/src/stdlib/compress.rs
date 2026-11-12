//! `std.compress`: DEFLATE, zlib and gzip. Compressing takes bytes or text and gives bytes;
//! reading back fails with an `InvalidValue` on data that isn't what it says, or that holds more
//! than `most=` bytes, 64 MB unless given.

use super::{Function, Module, no_checks, no_methods};
use crate::function::FnCx;
use crate::types::{Type, a};
use fennec_syntax::{Diagnostic, Span};

pub(crate) const MODULE: Module = Module {
    functions: &FUNCTIONS,
    result,
    method: no_methods,
    construct: no_checks,
};

const fn writes(name: &'static str) -> Function {
    Function { name, arity: (1, 1), named: &["level"], needs_values: true }
}

const fn reads(name: &'static str) -> Function {
    Function { name, arity: (1, 1), named: &["most"], needs_values: true }
}

const FUNCTIONS: [Function; 6] = [
    writes("compress.deflate"),
    reads("compress.gunzip"),
    writes("compress.gzip"),
    reads("compress.inflate"),
    reads("compress.unzlib"),
    writes("compress.zlib"),
];

fn result(cx: &mut FnCx<'_>, name: &str, plain: &[(Type, Span)], _: Span) -> Type {
    let short = name.trim_start_matches("compress.");
    let compressing = matches!(short, "deflate" | "zlib" | "gzip");
    for (ty, at) in plain.iter().take(1) {
        let ty = cx.checker.settled(ty);
        let fits = matches!(ty, Type::Bytes | Type::Unknown | Type::Var(_)) || (compressing && ty == Type::Str);
        if !fits {
            let wanted = if compressing { "bytes or a str" } else { "bytes" };
            cx.error(Diagnostic::new(format!("`{short}` takes {wanted}, but this is {}", a(&ty)), *at));
        }
    }
    if compressing { Type::Bytes } else { Type::result(Type::Bytes, Type::named("InvalidValue")) }
}
