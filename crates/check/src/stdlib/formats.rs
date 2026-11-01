//! What `std.json` and `std.toml` share. Reading text needs the type to read it as, written
//! in brackets: `json.loads[Settings](text)`. Every type made of numbers, text, bools, lists,
//! dicts with text keys, tuples, optionals and structs of those can be read and written.

use fennec_syntax::{Arg, Diagnostic, Expr, Span};

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
        } else if let Err(problem) = self.holds(&format, first, call, &mut Vec::new()) {
            self.error(problem);
        } else if format == "TOML" && mentions_none(first, &self.checker.program) {
            self.error(Diagnostic::new("TOML has no None, so leave the key out instead", call));
        }
        Type::Str
    }

    /// `json.loads[T](text)`: the text read as a `T`, or a `ParseError`.
    pub(crate) fn typed_loads(&mut self, name: &str, index: &Expr, args: &[Arg], span: Span) -> Type {
        let types: Vec<Type> = args.iter().map(|arg| self.arg_value(arg, &[])).collect();
        match (args, types.as_slice()) {
            ([arg], [text]) if arg.name.is_none() && !arg.is_mut => {
                if !self.checker.fits(&Type::Str, text) {
                    self.error(mismatch("the text to read", &Type::Str, text, arg.value.span));
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
        } else if let Err(problem) = self.holds(&format, &ty, index.span, &mut Vec::new()) {
            self.error(problem);
        }
        Type::result(ty, Type::named("ParseError"))
    }

    /// The fields of a struct, or of every case of an enum, each with its case.
    fn fields_of(&self, name: &str) -> Vec<(Option<String>, String)> {
        let Some(info) = self.checker.program.type_info(name) else { return Vec::new() };
        let own = info.def.fields.iter().map(|field| (None, field.name.clone()));
        let cases = info.def.cases.iter().flat_map(|case| {
            case.fields.iter().map(move |field| (Some(case.name.clone()), field.name.clone()))
        });
        own.chain(cases).collect()
    }

    fn is_table(&self, ty: &Type) -> bool {
        matches!(ty, Type::Dict(..) | Type::Unknown)
            || self.checker.program.kind_of(ty) == Some(TypeKind::Struct)
    }

    /// Whether a format can hold values of a type. `seen` are the structs being looked
    /// at, so a struct that holds itself is looked at once.
    fn holds(
        &mut self,
        format: &str,
        ty: &Type,
        span: Span,
        seen: &mut Vec<String>,
    ) -> Result<(), Diagnostic> {
        let cannot =
            |what: &Type| Diagnostic::new(format!("{format} can't hold {}", article(&what.kind())), span);
        match ty {
            Type::Unknown | Type::Unit | Type::None | Type::Bool | Type::Int | Type::Float | Type::Str => {
                Ok(())
            }
            Type::List(item) | Type::Optional(item) | Type::IdMap(item) => {
                self.holds(format, item, span, seen)
            }
            // An `Id` is written as its slot and generation, whatever it's for.
            Type::Id(_) => Ok(()),
            Type::Dict(key, value) if **key == Type::Str => self.holds(format, value, span, seen),
            Type::Dict(..) => {
                Err(cannot(ty).with_help(format!("{format} tables have text keys: use a `dict[str, ...]`")))
            }
            Type::Tuple(items) => items.iter().try_for_each(|item| self.holds(format, item, span, seen)),
            Type::Result(..) => Err(unhandled_result(span)),
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
                for (case, field) in self.fields_of(name) {
                    let given = self.checker.program.type_args(ty);
                    let field_type =
                        self.checker.field_type(name, case.as_deref(), &field).substitute(&given);
                    self.holds(format, &field_type, span, seen)?;
                }
                Ok(())
            }
            other => Err(cannot(other)),
        }
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
