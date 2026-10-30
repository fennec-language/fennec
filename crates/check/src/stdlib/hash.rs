//! `std.hash`: BLAKE3 hashes of text, as hex strings.

use super::{Function, Module, no_checks, no_methods, takes};
use crate::function::FnCx;
use crate::messages::mismatch;
use crate::types::{Type, accepts};
use fennec_syntax::Span;

pub(crate) const MODULE: Module = Module {
    functions: &FUNCTIONS,
    result,
    method: no_methods,
    construct: no_checks,
};

const FUNCTIONS: [Function; 3] =
    [takes("hash.blake3", (1, 1)), takes("hash.derive_key", (2, 2)), takes("hash.keyed_blake3", (2, 2))];

/// Every argument is text, and every result is a hash written as hex.
fn result(cx: &mut FnCx<'_>, name: &str, plain: &[(Type, Span)], _: Span) -> Type {
    let what = match name {
        "hash.blake3" => ["the text to hash", ""],
        "hash.keyed_blake3" => ["the key", "the text to hash"],
        _ => ["the context", "the key material"],
    };
    for ((ty, at), what) in plain.iter().zip(what) {
        if !accepts(&Type::Str, ty) {
            cx.error(mismatch(what, &Type::Str, ty, *at));
        }
    }
    Type::Str
}
