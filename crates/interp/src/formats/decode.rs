//! Reading a parsed value into the type a program asks for, as in `json.loads[Settings](text)`.
//! An `Id`, a `Table` and an enum case read back as `dump` wrote them, and a generic struct
//! reads its fields with its type arguments filled in.
//! A struct's field takes the type written for it, or else the shape of its default, which
//! the type checker makes sure shows the field's type. An int is read as a float where a
//! float is wanted, since JSON and TOML write whole floats without a decimal point.

use std::rc::Rc;

use fennec_syntax::{Field, Span, TypeExpr};

;
use crate::ops::to_float;
use crate::value::{Id, Slot, Table, Value};

/// Why a value read doesn't fit the type wanted.
type Misfit = String;

/// Where a part of the document is, like `keys[2]` or `window.title`, from where its
/// container is. The whole document is at `""`.
fn inside(at: &str, part: &str) -> String {
    match (at.is_empty(), part.starts_with('[')) {
        (true, _) | (false, true) => format!("{at}{part}"),
        (false, false) => format!("{at}.{part}"),
    }
}

/// A place for a message: `` `keys[2]` ``, or "the text".
fn place(at: &str) -> String {
    if at.is_empty() { "the text".to_owned() } else { format!("`{at}`") }
}

/// A key of an object as the dict's key type wants it: an int key is written as its digits.
/// `at` is where the dict is.
fn object_key(key: &Value, wanted: &TypeExpr, at: &str) -> Result<Value, Misfit> {
    match (wanted.name.as_str(), key) {
        ("int", Value::Str(text)) => text
            .parse()
            .map(Value::Int)
            .map_err(|_| format!("{} has the key {text:?}, which isn't an int", place(at))),
        _ => Ok(key.clone()),
    }
}

/// What each of a type's parameters stands for in `ty`: `T` is `Tile` in `Grid[Tile]`.
pub(super) fn given_types(generics: &[fennec_syntax::Generic], ty: &TypeExpr) -> Vec<(String, TypeExpr)> {
    generics.iter().map(|generic| generic.name.clone()).zip(ty.args.iter().cloned()).collect()
}

/// A field's type with its struct's type parameters filled in.
pub(super) fn substituted(ty: &TypeExpr, given: &[(String, TypeExpr)]) -> TypeExpr {
    if ty.args.is_empty()
        && let Some((_, filled)) = given.iter().find(|(name, _)| *name == ty.name)
    {
        return TypeExpr { optional: ty.optional || filled.optional, ..filled.clone() };
    }
    TypeExpr { args: ty.args.iter().map(|arg| substituted(arg, given)).collect(), ..ty.clone() }
}

fn misfit(at: &str, wanted: &str, value: &Value) -> Misfit {
    let found = match value {
        Value::Dict(_) => "a table".to_owned(),
        Value::None => "null".to_owned(),
        other => article(&other.type_name()),
    };
    format!("{} should be {}, but it's {found}", place(at), article(wanted))
}

fn article(name: &str) -> String {
    let vowel = name.starts_with(|c: char| "aeiouAEIOU".contains(c));
    if vowel { format!("an {name}") } else { format!("a {name}") }
}

/// A type as it's written, like `list[int]`.
pub(super) fn written(ty: &TypeExpr) -> String {
    let args: Vec<String> = ty.args.iter().map(written).collect();
    let base = match (ty.name.as_str(), args.is_empty()) {
        ("tuple", _) => format!("({})", args.join(", ")),
        (name, true) => name.to_owned(),
        (name, false) => format!("{name}[{}]", args.join(", ")),
    };
    if ty.optional { format!("{base}?") } else { base }
}
