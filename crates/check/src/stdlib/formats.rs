//! What `std.json` and `std.toml` share.

use fennec_syntax::{Diagnostic, Span};

use crate::function::FnCx;
use crate::messages::mismatch;
use crate::types::{Type, accepts, article};

/// `std.json` and `std.toml`. Read text gives values of any type, so its type is unknown,
/// except that a TOML document is always a table.
pub(super) fn result(cx: &mut FnCx<'_>, name: &str, plain: &[(Type, Span)], span: Span) -> Type {
    let (first, at) = plain.first().cloned().unwrap_or((Type::Unknown, span));
    let first = &first;
    let (format, function) = name.split_once('.').unwrap_or_default();
    if function == "loads" {
        if !accepts(&Type::Str, first) {
            cx.error(mismatch("the text to read", &Type::Str, first, at));
        }
        let read = if format == "toml" {
            Type::Dict(Box::new(Type::Str), Box::new(Type::Unknown))
        } else {
            Type::Unknown
        };
        return Type::result(read, Type::named("ParseError"));
    }
    let format = format.to_uppercase();
    let is_table = matches!(first, Type::Dict(..) | Type::Unknown)
        || cx.checker.program.kind_of(first) == Some(crate::program::TypeKind::Struct);
    if format == "TOML" && !is_table {
        cx.error(Diagnostic::new(
            format!(
                "a TOML document is a table, so `toml.dumps` needs a dict or a struct, not {}",
                article(&first.kind())
            ),
            at,
        ));
    } else if matches!(
        first,
        Type::Set(_) | Type::Function(_) | Type::Id(_) | Type::IdMap(_) | Type::Result(..)
    ) {
        cx.error(mismatch_holds(&format, first, at));
    }
    Type::Str
}

fn mismatch_holds(format: &str, ty: &Type, span: Span) -> Diagnostic {
    if matches!(ty, Type::Result(..)) {
        return crate::messages::unhandled_result(span);
    }
    Diagnostic::new(format!("{format} can't hold {}", article(&ty.kind())), span)
}
