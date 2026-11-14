//! `std.json`'s `dumps` and `loads[T]`, modeled here on their own, from `docs/language.md`, the
//! examples and what the compiled program does, never from the standard library's code: what a
//! value is written as, field by field, and what text reads as a `T`, with the `ParseError` a
//! load that fails gives. A failure whose message the oracle can't be sure of is refused.
//!
//! What's saved is worked out from the type the checker gave the value, since a value alone
//! doesn't say which `Table` an `Id` is for, or whether it's held as an optional.

mod parse;
mod read;
mod write;

use fennec_check::Type;
use fennec_syntax::{Expr, ExprKind, Field};

use super::calls::ArgVal;
use super::{Interp, Run, unsupported};
use crate::load::Kind;
use crate::value::Value;

/// How field names are spelled in JSON, as `names=` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Names {
    Snake,
    Camel,
    Pascal,
    Kebab,
}

/// What a call's named arguments ask for.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Options {
    pub(crate) names: Names,
    /// Whether a key no field has is passed over, as `extra=.Ignore` says.
    pub(crate) ignore: bool,
    /// How far each level is indented, or -1 for one line.
    pub(crate) indent: i64,
}

/// A field as it's saved: its name, its type with the type's parameters filled in, the name
/// `@serialize(name=...)` gave it, and whether `@skip` leaves it out.
#[derive(Clone, Debug)]
pub(crate) struct SavedField {
    pub(crate) name: String,
    pub(crate) ty: Type,
    pub(crate) renamed: Option<String>,
    pub(crate) skipped: bool,
    pub(crate) field: &'static Field,
}

impl SavedField {
    /// The name it's saved under: the one `@serialize` gave it, or its own.
    pub(crate) fn saved_name(&self) -> &str {
        self.renamed.as_deref().unwrap_or(&self.name)
    }
}

/// The standard library's enums the oracle knows as values: `std.json`'s options.
const OPTION_ENUMS: &[(&str, &[&str])] = &[
    ("JsonNames", &["SnakeCase", "CamelCase", "PascalCase", "KebabCase"]),
    ("JsonExtra", &["Error", "Ignore"]),
];

/// A case of one of `std.json`'s option enums, by the enum's name and the case's, when it's one.
pub(crate) fn option_case(ty: &str, case: &str) -> Option<Value> {
    let (_, cases) = OPTION_ENUMS.iter().find(|(name, _)| *name == ty)?;
    cases.contains(&case).then(|| Value::instance(ty, Some(case), Vec::new()))
}

/// A leading-dot case, like `.CamelCase`, of one of `std.json`'s option enums, when the checker
/// typed it as one and the program declares no type of that name itself.
pub(crate) fn option_case_typed(ty: Option<&Type>, case: &str) -> Option<Value> {
    let mut ty = ty?;
    while let Type::Optional(inner) = ty {
        ty = inner;
    }
    match ty {
        Type::Named(name) => option_case(name, case),
        _ => None,
    }
}

impl Interp {
    /// `json.dumps(value)` and `json.loads[T](text)`, with their named arguments.
    pub(crate) fn json_call(&mut self, name: &str, given: Vec<ArgVal>, expr: &'static Expr) -> Run<Value> {
        if OPTION_ENUMS.iter().any(|(own, _)| self.program.types.contains_key(*own)) {
            return unsupported("a program's own type named as one of std.json's");
        }
        let ExprKind::Call { args, .. } = &expr.kind else { return unsupported(name.to_owned()) };
        let Some(first) = args.iter().find(|arg| arg.name.is_none()) else {
            return unsupported(format!("{name} without its value"));
        };
        let mut options = Options { names: Names::Snake, ignore: false, indent: -1 };
        let mut value = None;
        for arg in given {
            let named = arg.name().map(str::to_owned);
            let held = self.arg_as_value(arg)?;
            match (named.as_deref(), held) {
                (None, held) if value.is_none() => value = Some(held),
                (Some("names"), held) => options.names = names_of(&held)?,
                (Some("extra"), held) => options.ignore = ignores(&held)?,
                (Some("indent"), Value::Int(indent)) => options.indent = indent,
                _ => return unsupported(format!("{name} with these arguments")),
            }
        }
        let Some(value) = value else { return unsupported(format!("{name} without its value")) };
        match name {
            "std.json.dumps" => {
                let Some(ty) = self.program.type_at(first.value.span).cloned() else {
                    return unsupported("json.dumps of a value whose type isn't known");
                };
                if options.indent < -1 {
                    return unsupported("json.dumps with a negative indent");
                }
                self.dumps(&value, &ty, options, expr.span)
            }
            "std.json.loads" => {
                let ty = match self.program.type_at(expr.span) {
                    Some(Type::Result(loaded, _)) => (**loaded).clone(),
                    Some(loaded) => loaded.clone(),
                    None => return unsupported("json.loads of a type that isn't known"),
                };
                let Value::Str(text) = value else { return unsupported("json.loads of what isn't text") };
                self.loads(&text, &ty, options, expr.span)
            }
            _ => unsupported(name.to_owned()),
        }
    }

    /// A struct or enum the program declares, whose saving and loading are its fields': one with
    /// a `def serialize` or `def deserialize` of its own isn't modeled, nor is the standard
    /// library's.
    pub(crate) fn saved_type(&self, name: &str) -> Run<Kind> {
        let name = fennec_syntax::rename::shown_type_name(name);
        let Some(declared) = self.program.types.get(name) else {
            return unsupported(format!("saving a {name} as JSON"));
        };
        if declared.kind == Kind::Error || declared.def.shared {
            return unsupported(format!("saving a {name} as JSON"));
        }
        if self.program.method(name, "serialize").is_some()
            || self.program.method(name, "deserialize").is_some()
        {
            return unsupported("a type's own `serialize` or `deserialize`");
        }
        Ok(declared.kind)
    }

    /// The fields a struct, or one of an enum's cases, saves, each with its type, the type's
    /// parameters filled in with `args`.
    pub(crate) fn saved_fields(&self, name: &str, case: Option<&str>, args: &[Type]) -> Run<Vec<SavedField>> {
        let name = fennec_syntax::rename::shown_type_name(name);
        let Some(declared) = self.program.types.get(name) else {
            return unsupported(format!("saving a {name} as JSON"));
        };
        let fields: &'static [Field] = match case {
            Some(case) => match declared.def.cases.iter().find(|known| known.name == case) {
                Some(found) => &found.fields,
                None => return unsupported(format!("the case `{name}.{case}`")),
            },
            None => &declared.def.fields,
        };
        let Some(typed) = self.program.fields_typed(name, case) else {
            return unsupported(format!("the fields of a {name}"));
        };
        let params: Vec<(String, Type)> = declared
            .def
            .generics
            .iter()
            .map(|generic| generic.name.clone())
            .zip(args.iter().cloned())
            .collect();
        if params.len() != declared.def.generics.len() {
            return unsupported(format!("a {name} whose type parameters aren't known"));
        }
        let mut saved = Vec::with_capacity(fields.len());
        for field in fields {
            let Some((_, ty)) = typed.iter().find(|(typed_name, _)| *typed_name == field.name) else {
                return unsupported(format!("the type of `{name}.{}`", field.name));
            };
            saved.push(SavedField {
                name: field.name.clone(),
                ty: filled(ty, &params),
                renamed: renamed(field),
                skipped: field.is_skipped(),
                field,
            });
        }
        Ok(saved)
    }
}

/// The name `@serialize(name=...)` gives a field, if it gives one.
fn renamed(field: &Field) -> Option<String> {
    let annotation = field.annotations.iter().find(|annotation| annotation.name == "serialize")?;
    annotation.args.iter().find_map(|arg| match &arg.value.kind {
        ExprKind::Str(name) if arg.name.as_deref() == Some("name") => Some(name.clone()),
        _ => None,
    })
}

/// A type with each type parameter in `params` filled in.
fn filled(ty: &Type, params: &[(String, Type)]) -> Type {
    let each = |inner: &Type| Box::new(filled(inner, params));
    match ty {
        Type::Param(name) => params
            .iter()
            .find(|(param, _)| param == name)
            .map_or_else(|| ty.clone(), |(_, given)| given.clone()),
        Type::List(inner) => Type::List(each(inner)),
        Type::Set(inner) => Type::Set(each(inner)),
        Type::Optional(inner) => Type::Optional(each(inner)),
        Type::Id(inner) => Type::Id(each(inner)),
        Type::Table(inner) => Type::Table(each(inner)),
        Type::Dict(key, value) => Type::Dict(each(key), each(value)),
        Type::Tuple(items) => Type::Tuple(items.iter().map(|item| filled(item, params)).collect()),
        Type::Union(members) => Type::Union(members.iter().map(|member| filled(member, params)).collect()),
        Type::Generic(name, args) => {
            Type::Generic(name.clone(), args.iter().map(|arg| filled(arg, params)).collect())
        }
        other => other.clone(),
    }
}

/// A type as `std.json`'s messages name it, as in `list[Point]`.
pub(crate) fn type_name(ty: &Type) -> String {
    super::calls::shown_type(ty)
}

/// What `names=` was given.
fn names_of(value: &Value) -> Run<Names> {
    match value {
        Value::None => Ok(Names::Snake),
        Value::Instance(instance) if instance.ty == "JsonNames" => match instance.case.as_deref() {
            Some("SnakeCase") => Ok(Names::Snake),
            Some("CamelCase") => Ok(Names::Camel),
            Some("PascalCase") => Ok(Names::Pascal),
            Some("KebabCase") => Ok(Names::Kebab),
            _ => unsupported("a `names=` that isn't a JsonNames"),
        },
        _ => unsupported("a `names=` that isn't a JsonNames"),
    }
}

/// Whether `extra=` says to pass over keys no field has.
fn ignores(value: &Value) -> Run<bool> {
    match value {
        Value::None => Ok(false),
        Value::Instance(instance) if instance.ty == "JsonExtra" => match instance.case.as_deref() {
            Some("Error") => Ok(false),
            Some("Ignore") => Ok(true),
            _ => unsupported("an `extra=` that isn't a JsonExtra"),
        },
        _ => unsupported("an `extra=` that isn't a JsonExtra"),
    }
}

/// A field's name as `names` spells it: its words, split at each `_`, joined as the spelling
/// joins them, with any `_` it starts with kept as it is.
pub(crate) fn spelled_as(name: &str, names: Names) -> String {
    if names == Names::Snake {
        return name.to_owned();
    }
    let rest = name.trim_start_matches('_');
    let mut out = name.get(..name.len() - rest.len()).unwrap_or_default().to_owned();
    for (at, word) in rest.split('_').filter(|word| !word.is_empty()).enumerate() {
        match names {
            Names::Kebab => {
                if at > 0 {
                    out.push('-');
                }
                out.push_str(word);
            }
            Names::Camel if at == 0 => out.push_str(word),
            Names::Camel | Names::Pascal => out.push_str(&capital(word)),
            Names::Snake => {}
        }
    }
    out
}

/// A word with its first character upper case.
fn capital(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// "a" or "an", by the name's first letter, as `std.json`'s messages choose.
pub(crate) fn article(name: &str) -> String {
    if name.starts_with(['a', 'e', 'i', 'o', 'u', 'A', 'E', 'I', 'O', 'U']) {
        format!("an {name}")
    } else {
        format!("a {name}")
    }
}

/// A `ParseError` saying `message`, with the line and column it gives.
pub(crate) fn parse_error(message: String, line: i64, column: i64) -> Value {
    Value::instance(
        "ParseError",
        None,
        vec![
            ("message".to_owned(), Value::Str(message)),
            ("line".to_owned(), Value::Int(line)),
            ("column".to_owned(), Value::Int(column)),
        ],
    )
}
