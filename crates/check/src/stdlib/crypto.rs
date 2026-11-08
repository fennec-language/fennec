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

const FUNCTIONS: [Function; 9] = [
    takes("crypto._add", (2, 2)),
    takes("crypto._end", (1, 1)),
    takes("crypto._start", (1, 1)),
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

/// What `crypto.fen` writes hashes of many chunks with: a hash started, given each chunk, then
/// ended in its hex.
fn pieces(name: &str) -> Type {
    match name {
        "crypto._start" => Type::Int,
        "crypto._add" => Type::Unit,
        _ => Type::Str,
    }
}

/// The function in `crypto.fen` that hashes `ty`'s chunks, when it's a list or an iterator of
/// text or bytes, like `crypto.blake3_of_bytes_chunks`.
fn of_chunks(cx: &mut FnCx<'_>, name: &str, ty: &Type) -> Option<String> {
    let (item, form) = match cx.checker.settled(ty) {
        Type::List(item) => (*item, "list"),
        Type::Iterator(item) => (*item, "chunks"),
        _ => return None,
    };
    let given = match cx.checker.settled(&item) {
        Type::Str => "str",
        Type::Bytes => "bytes",
        _ => return None,
    };
    Some(format!("{name}_of_{given}_{form}"))
}

fn result(cx: &mut FnCx<'_>, name: &str, plain: &[(Type, Span)], span: Span) -> Type {
    if name.starts_with("crypto._") {
        return pieces(name);
    }
    if matches!(name, "crypto.blake3" | "crypto.sha256")
        && let [(ty, _)] = plain
        && let Some(written) = of_chunks(cx, name, ty)
    {
        cx.checker.std_calls.insert((span.start, span.end), written);
        return Type::Str;
    }
    for ((ty, at), (what, bytes_too)) in plain.iter().zip(arguments(name)) {
        let ty = cx.checker.settled(ty);
        let fits = matches!(ty, Type::Str | Type::Unknown | Type::Var(_)) || (bytes_too && ty == Type::Bytes);
        if !fits {
            let short = name.trim_start_matches("crypto.");
            let wanted = match (bytes_too, short) {
                (true, "blake3" | "sha256") => "a str or bytes, or a list or iterator of them",
                (true, _) => "a str or bytes",
                (false, _) => "a str",
            };
            let message = match what {
                Some(what) => format!("`{short}`'s {what} should be {wanted}, but this is {}", a(&ty)),
                None => format!("`{short}` takes {wanted}, but this is {}", a(&ty)),
            };
            cx.error(Diagnostic::new(message, *at));
        }
    }
    if name == "crypto.equal" { Type::Bool } else { Type::Str }
}
