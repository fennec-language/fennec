//! `std.crypto`: hashes of text or bytes as hex strings, HMAC, and comparing secrets in time
//! that doesn't depend on where they differ.

use super::{Function, Module, no_checks, no_methods, takes};
use crate::function::FnCx;
use crate::types::{Type, a};
use fennec_syntax::{Diagnostic, Span};

pub(crate) const MODULE: Module = Module {
    functions: &FUNCTIONS,
    result,
    method: no_methods,
    construct: no_checks,
};

const FUNCTIONS: [Function; 6] = [
    takes("crypto.blake3", (1, 1)),
    takes("crypto.derive_key", (2, 2)),
    takes("crypto.equal", (2, 2)),
    takes("crypto.hmac_sha256", (2, 2)),
    takes("crypto.keyed_blake3", (2, 2)),
    takes("crypto.sha256", (1, 1)),
];

/// What each argument is, for messages, or nothing for what's hashed or signed. A BLAKE3 key
/// and a derive context are text; the rest can be text, taken as its UTF-8 bytes, or bytes.
fn arguments(name: &str) -> [(Option<&'static str>, bool); 2] {
    match name {
        "crypto.keyed_blake3" => [(Some("key"), false), (None, true)],
        "crypto.derive_key" => [(Some("context"), false), (Some("key material"), true)],
        "crypto.hmac_sha256" => [(Some("key"), true), (None, true)],
        _ => [(None, true), (None, true)],
    }
}

fn result(cx: &mut FnCx<'_>, name: &str, plain: &[(Type, Span)], _: Span) -> Type {
    for ((ty, at), (what, bytes_too)) in plain.iter().zip(arguments(name)) {
        let ty = cx.checker.settled(ty);
        let fits = matches!(ty, Type::Str | Type::Unknown | Type::Var(_)) || (bytes_too && ty == Type::Bytes);
        if !fits {
            let wanted = if bytes_too { "a str or bytes" } else { "a str" };
            let short = name.trim_start_matches("crypto.");
            let message = match what {
                Some(what) => format!("`{short}`'s {what} should be {wanted}, but this is {}", a(&ty)),
                None => format!("`{short}` takes {wanted}, but this is {}", a(&ty)),
            };
            cx.error(Diagnostic::new(message, *at));
        }
    }
    if name == "crypto.equal" { Type::Bool } else { Type::Str }
}
