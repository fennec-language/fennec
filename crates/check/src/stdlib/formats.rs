//! What `std.json` and `std.toml` share. Reading text needs the type to read it as, written
//! in brackets: `json.loads[Settings](text)`. Every type made of numbers, text, bytes, bools,
//! lists, dicts with text keys, tuples, optionals and structs of those can be read and written.

use std::collections::BTreeSet;

use fennec_syntax::{Arg, Diagnostic, Expr, Span};

use crate::checker::Checker;
use crate::function::FnCx;
use crate::messages::{mismatch, unhandled_result};
use crate::program::TypeKind;
use crate::types::{Type, a, article};

impl FnCx<'_> {
    /// `json.dumps(value)` and `toml.dumps(value)`, and `loads` called without a type.
    /// `spans` are the value's and the call's.
    pub(crate) fn format_result(&mut self, name: &str, first: &Type, spans: (Span, Span)) -> Type {
        let (at, call) = spans;
        let (format, function) = name.split_once('.').unwrap_or_default();
        if function == "loads" {
            self.error(
                Diagnostic::new(format!("`{name}` needs the type to read"), call)
                    .with_help(format!("write it in brackets, like `{name}[Settings](text)`")),
            );
            return Type::Unknown;
        }
        let format = format.to_uppercase();
        if format == "TOML" && !self.is_table(first) {
            self.error(Diagnostic::new(
                format!(
                    "a TOML document is a table, so `toml.dumps` needs a dict or a struct, not {}",
                    a(first)
                ),
                at,
            ));
        } else if let Err(problem) = self.holds(&format, first, (call, None, false), &mut Vec::new()) {
            self.error(problem);
        } else if format == "TOML" && mentions_none(first, &self.checker.program) {
            self.error(Diagnostic::new("TOML has no None, so leave the key out instead", call));
        } else if self.fits_bound(first, WRITES).is_ok() {
            self.checker
                .native_formats
                .insert((call.start, call.end), format!("{}.native_dumps", format.to_lowercase()));
        }
        Type::Str
    }

    /// `net.post(url, json=value)`: the value is sent as JSON, so it has to be something JSON
    /// holds. Native code writes it through its own `def serialize`, as `json.dumps` does.
    pub(crate) fn json_body(&mut self, ty: &Type, at: Span) {
        if let Err(problem) = self.holds("JSON", ty, (at, None, false), &mut Vec::new()) {
            self.error(problem);
        } else if self.fits_bound(ty, WRITES).is_ok() {
            self.checker.native_json_bodies.insert((at.start, at.end));
        }
    }

    /// `json.loads[T](text)`: the text read as a `T`, or a `ParseError`.
    pub(crate) fn typed_loads(&mut self, name: &str, index: &Expr, args: &[Arg], span: Span) -> Type {
        let mut plain = Vec::new();
        for arg in args {
            let ty = self.arg_value(arg, &[]);
            match arg.name.as_deref() {
                None if !arg.is_mut => plain.push((ty, arg.value.span)),
                Some(option @ ("names" | "extra")) if name == "json.loads" => {
                    self.json_option(option, &ty, arg.value.span);
                }
                Some(other) => self.error(Diagnostic::new(
                    format!("`{name}` has no parameter called `{other}`"),
                    arg.value.span,
                )),
                None => self.error(crate::messages::wrong_args(name, span)),
            }
        }
        match plain.as_slice() {
            [(text, at)] => {
                if !self.checker.fits(&Type::Str, text) {
                    self.error(mismatch("the text to read", &Type::Str, text, *at));
                }
            }
            _ => self.error(crate::messages::wrong_args(name, span)),
        }
        let Some(written) = index.as_type() else {
            let message = format!("write the type to read in brackets, like `{name}[Settings]`");
            self.error(Diagnostic::new(message, index.span));
            return Type::Unknown;
        };
        let ty = self.resolve(&written);
        let format = name.split_once('.').map_or(name, |(format, _)| format).to_uppercase();
        if format == "TOML" && !self.is_table(&ty) {
            let message =
                format!("a TOML document is a table, so it's read as a dict or a struct, not {}", a(&ty));
            self.error(Diagnostic::new(message, index.span));
        } else if let Err(problem) = self.holds(&format, &ty, (index.span, None, true), &mut Vec::new()) {
            self.error(problem);
        } else if self.fits_bound(&ty, READS).is_ok() {
            self.checker
                .native_formats
                .insert((span.start, span.end), format!("{}.native_loads", format.to_lowercase()));
        }
        Type::result(ty, Type::named("ParseError"))
    }

    /// A dict or `SortedMap`. Text and int keys make an object, an int written as its digits.
    /// In JSON, any other key makes an array of `[key, value]` pairs; TOML only has tables.
    fn holds_map(
        &mut self,
        format: &str,
        map: &Type,
        (key, value): (&Type, &Type),
        (span, field, loading): (Span, Option<&str>, bool),
        seen: &mut Vec<String>,
    ) -> Result<(), Diagnostic> {
        let as_object = matches!(key, Type::Str | Type::Int | Type::Unknown);
        if !as_object && format != "JSON" {
            return Err(Diagnostic::new(cant_hold(format, field, &article(&map.kind())), span)
                .with_help(format!("{format} tables have text keys: use `str` or `int` keys")));
        }
        if !as_object {
            self.holds(format, key, (span, field, loading), seen)?;
        }
        self.holds(format, value, (span, field, loading), seen)
    }

    /// Whether `name` is a standard library collection written in its natural shape.
    fn has_shape(&self, name: &str) -> bool {
        matches!(name, "SortedMap" | "SortedSet" | "Deque" | "Grid")
            && self.checker.program.type_info(name).is_some_and(|info| info.prelude)
    }

    /// The fields a format saves of a struct, or of every case of an enum, each with its case.
    /// A field with `@skip` isn't saved, so it can be of a type the format can't hold.
    fn fields_of(&self, name: &str) -> Vec<(Option<String>, String)> {
        let Some(info) = self.checker.program.type_info(name) else { return Vec::new() };
        let own = info
            .def
            .fields
            .iter()
            .filter(|field| !field.is_skipped())
            .map(|field| (None, field.name.clone()));
        let cases = info.def.cases.iter().flat_map(|case| {
            let saved = case.fields.iter().filter(|field| !field.is_skipped());
            saved.map(move |field| (Some(case.name.clone()), field.name.clone()))
        });
        own.chain(cases).collect()
    }

    fn is_table(&self, ty: &Type) -> bool {
        matches!(ty, Type::Dict(..) | Type::Unknown)
            || self.checker.program.kind_of(ty) == Some(TypeKind::Struct)
    }

    /// Whether a type parameter is bounded by the interface a value is written or read through.
    /// Each call's own type is checked where the call is, once every function is checked, so
    /// it's noted.
    fn saved_param(&mut self, name: &str, ty: &Type, (format, loading): (&str, bool)) -> bool {
        let interface = if loading { READS } else { WRITES };
        if !self.bounds_of(name).iter().any(|bound| bound == interface) {
            return false;
        }
        if self.report {
            let noted = (self.key.clone(), ty.clone(), format.to_owned(), interface.to_owned());
            self.checker.format_params.insert(noted);
        }
        true
    }

    /// Whether a format can hold values of a type. `field` is the struct field the value is
    /// in, if any, as `Type.field`, which a message names. `loading` says it's being read, where
    /// a `Secret` may go though it's never written. `seen` are the structs being looked at, so a
    /// struct that holds itself is looked at once.
    fn holds(
        &mut self,
        format: &str,
        ty: &Type,
        (span, field, loading): (Span, Option<&str>, bool),
        seen: &mut Vec<String>,
    ) -> Result<(), Diagnostic> {
        let cannot = |what: &Type| Diagnostic::new(cant_hold(format, field, &article(&what.kind())), span);
        if self.checker.program.is_shared(ty) {
            let what = format!("a `{}`", ty.source().unwrap_or_else(|| ty.kind()));
            return Err(Diagnostic::new(cant_hold(format, field, &what), span)
                .with_help("a `shared` value is reached inside a `lock`: save what it holds, read there"));
        }
        match ty {
            // `bytes` are written as base64 text, and an `Id` as its slot and generation, whatever
            // it's for.
            Type::Unknown
            | Type::Unit
            | Type::None
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Sized(crate::types::Num::F32)
            | Type::Str
            | Type::Bytes
            | Type::Id(_) => Ok(()),
            Type::Param(name) if self.saved_param(name, ty, (format, loading)) => Ok(()),
            // A union is saved tagged with its member, which a type's name alone has to say.
            Type::Union(members) if !members.iter().all(crate::saving::tested_by_name) => Err(cannot(ty)),
            Type::Dict(key, _) if crate::saving::holds_union(key) => Err(cannot(ty)),
            // `null` can't tell `None` from an optional holding `None`.
            Type::Optional(item) if matches!(**item, Type::Optional(_)) => Err(Diagnostic::new(
                cant_hold(format, field, "an optional of an optional"),
                span,
            )
            .with_help("`null` can't tell `None` from an optional holding `None`: save a struct or an enum that says which it is")),
            Type::List(item) | Type::Optional(item) | Type::Table(item) => {
                self.holds(format, item, (span, field, loading), seen)
            }
            Type::Dict(key, value) => self.holds_map(format, ty, (key, value), (span, field, loading), seen),
            Type::Tuple(items) | Type::Union(items) => {
                items.iter().try_for_each(|item| self.holds(format, item, (span, field, loading), seen))
            }
            Type::Result(..) => Err(unhandled_result(span)),
            // The standard library's collections are written in their natural shape.
            Type::Generic(name, args) if self.has_shape(name) => match (name.as_str(), args.as_slice()) {
                ("SortedMap", [key, value]) => {
                    self.holds_map(format, ty, (key, value), (span, field, loading), seen)
                }
                (_, args) => {
                    args.iter().try_for_each(|item| self.holds(format, item, (span, field, loading), seen))
                }
            },
            // A `Secret` loads as what it holds, though it's never saved.
            Type::Named(name) | Type::Generic(name, _) if loading && crate::saving::loaded_only(name) => ty
                .type_args()
                .iter()
                .try_for_each(|arg| self.holds(format, arg, (span, field, loading), seen)),
            Type::Named(name) | Type::Generic(name, _)
                if let Some((what, instead)) = crate::saving::never_saved(name) =>
            {
                Err(Diagnostic::new(cant_hold(format, field, what), span).with_help(instead))
            }
            Type::Named(name) | Type::Generic(name, _) => {
                let kind = self.checker.program.type_info(name).map(|info| info.kind);
                let fits = match kind {
                    Some(TypeKind::Struct) => true,
                    // A case is its name, or an object holding its fields, which TOML can't write.
                    Some(TypeKind::Enum) => format == "JSON",
                    _ => false,
                };
                if !fits {
                    return Err(cannot(ty));
                }
                // Each set of type arguments is looked at once.
                let key = ty.to_string();
                if seen.contains(&key) {
                    return Ok(());
                }
                seen.push(key);
                for (case, own) in self.fields_of(name) {
                    let given = self.checker.program.type_args(ty);
                    let field_type = self.checker.field_type(name, case.as_deref(), &own).substitute(&given);
                    let named = match &case {
                        Some(case) => format!("{name}.{case}.{own}"),
                        None => format!("{name}.{own}"),
                    };
                    self.holds(format, &field_type, (span, Some(&named), loading), seen)?;
                }
                Ok(())
            }
            other => Err(cannot(other)),
        }
    }
}

/// Checks, at each call of a generic function, what it writes or reads with a format as its type
/// parameter: the format has to hold the type the call gives it, as a value written out there
/// would. A call made with the caller's own type parameter passes the check on to the caller's
/// calls.
pub(crate) fn formats_at_calls(checker: &mut Checker) {
    let mut work: Vec<(String, Type, String, String)> = checker.format_params.iter().cloned().collect();
    let (mut seen, mut refused) = (BTreeSet::new(), BTreeSet::new());
    while let Some(noted) = work.pop() {
        if !seen.insert(noted.clone()) {
            continue;
        }
        let (function, ty, format, interface) = noted;
        let Some(entry) = checker.program.functions.get(&function) else { continue };
        let names: Vec<String> =
            checker.program.generics_of(entry).into_iter().map(|generic| generic.name).collect();
        let calls: Vec<((u32, u32), crate::generic_copies::Call)> = checker
            .generic_calls
            .iter()
            .filter(|(_, call)| call.callee == function)
            .map(|(at, call)| (*at, call.clone()))
            .collect();
        for (at, call) in calls {
            let types = call.types.iter().map(|ty| checker.settled(ty));
            let given: Vec<(String, Type)> = names.iter().cloned().zip(types).collect();
            let wanted = ty.substitute(&given);
            if wanted.has_params() {
                work.push((call.caller.clone(), wanted, format.clone(), interface.clone()));
                continue;
            }
            let span = Span { start: at.0, end: at.1 };
            let mut cx = FnCx::new(checker, &call.caller, "", true);
            let loading = interface == READS;
            if let Err(problem) = cx.holds(&format, &wanted, (span, None, loading), &mut Vec::new())
                && refused.insert(at)
            {
                let shown = fennec_syntax::without_kits(&function);
                let says = format!("`{shown}` writes or reads what it's given as {format}");
                let help = problem.help.clone().map_or(says.clone(), |help| format!("{says}. {help}"));
                cx.error(problem.with_help(help));
            }
        }
    }
}

/// The interface a value is written through, and the one it's read through.
const WRITES: &str = "Serializable";
const READS: &str = "Deserializable";

/// That a format can't hold `what`, naming the field it's in, if there is one.
fn cant_hold(format: &str, field: Option<&str>, what: &str) -> String {
    match field {
        Some(field) => format!("{format} can't hold `{field}`, which is {what}"),
        None => format!("{format} can't hold {what}"),
    }
}

/// Whether a value of this type could hold `None` somewhere inside.
fn mentions_none(ty: &Type, program: &crate::program::Program) -> bool {
    let mut seen = Vec::new();
    let mut pending = vec![ty.clone()];
    while let Some(ty) = pending.pop() {
        match &ty {
            Type::None | Type::Optional(_) => return true,
            Type::Named(name) if !seen.contains(name) => {
                seen.push(name.clone());
                let fields = program.type_info(name).map(|info| info.def.fields.clone()).unwrap_or_default();
                for field in fields {
                    if field.is_skipped() {
                        continue;
                    }
                    let written = field.ty.as_ref().and_then(|ty| program.resolve(ty).ok());
                    pending.extend(written);
                    if field
                        .default
                        .as_ref()
                        .is_some_and(|default| matches!(default.kind, fennec_syntax::ExprKind::None))
                    {
                        return true;
                    }
                }
            }
            other => pending.extend(other.children().into_iter().cloned()),
        }
    }
    false
}

/// `json.dumps(value)`, `toml.dumps(value)`, and `loads` called without a type.
pub(super) fn result(cx: &mut FnCx<'_>, name: &str, plain: &[(Type, Span)], span: Span) -> Type {
    let (first, at) = plain.first().cloned().unwrap_or((Type::Unknown, span));
    cx.format_result(name, &first, (at, span))
}
