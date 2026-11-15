//! A value saved through a format's own `Serializer`, as `std.serialize` sets the calls out: what
//! `value.serialize(ref out)` calls on `out`, by the value's type, for a type with no
//! `def serialize` of its own. A type that writes its own runs it.
//!
//! - An `int`, `float`, `bool`, `str` or `bytes` is `out.int`, `out.float`, `out.bool`,
//!   `out.text` or `out.bytes` of it. A missing value is `out.none()`, and an optional's value
//!   that's there is `out.some(value)`.
//! - A list is `out.sequence(items)`, a set the same with its items in order, and a dict
//!   `out.map` of its pairs.
//! - A struct is `out.record(name, count)`, then each field it saves, those `@skip` leaves out
//!   aside, as `out.field(name, value)`, or `out.renamed(name, value)` under the name
//!   `@serialize(name=...)` gave it, then `out.end()`.
//! - An enum's case with no data is `out.enum_case(name, case, at)`, `at` where it is among the
//!   cases. One with data is `out.case_record(name, case, at, count)`, then its fields as a
//!   struct's, then `out.end()` for the case: its fields and the case each end.
//!
//! What each is called with is typed as the value's own parts are, so `out.sequence` of a
//! `list[int?]` is called with `T` an `int?`. A union, a tuple, a `Table` and an `Id` aren't
//! modeled here.

use fennec_check::Type;
use fennec_syntax::Span;

use super::calls::{ArgVal, Receiver, Target};
use super::json::type_name;
use super::{Interp, Place, Run, unsupported};
use crate::load::Kind;
use crate::value::Value;

impl Interp {
    /// `object.serialize(ref out)`, when it's modeled here: its type, as the checker typed
    /// `object`, is known and has no `def serialize` of its own.
    pub(crate) fn serialize_target(&mut self, object: &'static fennec_syntax::Expr) -> Run<Option<Target>> {
        let Some(ty) = self.static_type(object.span) else { return Ok(None) };
        if let Type::Named(name) | Type::Generic(name, _) = &ty {
            let name = fennec_syntax::rename::shown_type_name(name);
            if self.program.method(name, "serialize").is_some() || !self.program.types.contains_key(name) {
                return Ok(None);
            }
        }
        if !super::typed::settled(&ty) {
            return unsupported("saving a value whose type parameter isn't known where it's saved");
        }
        let value = self.eval(object)?;
        Ok(Some(Target::Serialize { value, ty }))
    }

    /// What `value.serialize(ref out)` calls on `out`, for a value of type `ty`.
    pub(crate) fn serialize_into(&mut self, value: &Value, ty: &Type, out: &Place, span: Span) -> Run<()> {
        self.tick()?;
        match (ty, value) {
            (Type::Int, Value::Int(_)) => self.tell(out, "int", vec![value.clone()], Vec::new(), span),
            (Type::Float, Value::Float(_)) => self.tell(out, "float", vec![value.clone()], Vec::new(), span),
            (Type::Float, Value::Int(n)) => {
                let float = Value::Float(crate::text::int_as_float(*n));
                self.tell(out, "float", vec![float], Vec::new(), span)
            }
            (Type::Bool, Value::Bool(_)) => self.tell(out, "bool", vec![value.clone()], Vec::new(), span),
            (Type::Str | Type::LiteralStr, Value::Str(_)) => {
                self.tell(out, "text", vec![value.clone()], Vec::new(), span)
            }
            (Type::Bytes, Value::Bytes(_)) => self.tell(out, "bytes", vec![value.clone()], Vec::new(), span),
            (Type::Optional(_) | Type::None, Value::None) => {
                self.tell(out, "none", Vec::new(), Vec::new(), span)
            }
            (Type::Optional(inner), held) => {
                self.tell(out, "some", vec![held.clone()], vec![(**inner).clone()], span)
            }
            (Type::List(inner), Value::List(_)) => {
                self.tell(out, "sequence", vec![value.clone()], vec![(**inner).clone()], span)
            }
            (Type::Set(inner), Value::Set(items)) => {
                self.tell(out, "sequence", vec![Value::List(items.clone())], vec![(**inner).clone()], span)
            }
            (Type::Dict(key, held), Value::Dict(pairs)) => {
                let pairs =
                    pairs.iter().map(|(key, value)| Value::Tuple(vec![key.clone(), value.clone()])).collect();
                let types = vec![(**key).clone(), (**held).clone()];
                self.tell(out, "map", vec![Value::List(pairs)], types, span)
            }
            (Type::Named(name), Value::Instance(_)) => self.serialize_instance(value, name, &[], out, span),
            (Type::Generic(name, args), Value::Instance(_)) => {
                self.serialize_instance(value, name, args, out, span)
            }
            (ty, held) => unsupported(format!(
                "saving a {} as a {} through a format's own `Serializer`",
                held.kind(),
                type_name(ty)
            )),
        }
    }

    /// What a struct, or an enum's case, calls on `out`: its type's own `def serialize`, if it
    /// has one, or its fields', as the module's notes say.
    fn serialize_instance(
        &mut self,
        value: &Value,
        name: &str,
        args: &[Type],
        out: &Place,
        span: Span,
    ) -> Run<()> {
        let name = fennec_syntax::rename::shown_type_name(name);
        let Some(declared) = self.program.types.get(name) else {
            return unsupported(format!("saving a {name} through a format's own `Serializer`"));
        };
        if declared.kind == Kind::Error || declared.def.shared {
            return unsupported(format!("saving a {name} through a format's own `Serializer`"));
        }
        let Value::Instance(instance) = value else { return unsupported("saving what isn't a struct") };
        if (declared.kind == Kind::Enum) != instance.case.is_some() {
            return unsupported("saving a value of another type than its own");
        }
        let fields: Vec<_> = self
            .saved_fields(name, instance.case.as_deref(), args)?
            .into_iter()
            .filter(|field| !field.skipped)
            .collect();
        let count = Value::Int(i64::try_from(fields.len()).unwrap_or(i64::MAX));
        let shown = Value::Str(name.to_owned());
        let mut ends = 1;
        match &instance.case {
            None => self.tell(out, "record", vec![shown, count], Vec::new(), span)?,
            Some(case) => {
                let at = declared.def.cases.iter().position(|known| known.name == *case).unwrap_or_default();
                let at = Value::Int(i64::try_from(at).unwrap_or(i64::MAX));
                let case_name = Value::Str(case.clone());
                if fields.is_empty() {
                    return self.tell(out, "enum_case", vec![shown, case_name, at], Vec::new(), span);
                }
                self.tell(out, "case_record", vec![shown, case_name, at, count], Vec::new(), span)?;
                ends = 2;
            }
        }
        for field in fields {
            let Some(held) = instance.field(&field.name).cloned() else {
                return unsupported(format!("a {name} without its field `{}`", field.name));
            };
            // Under the name `@serialize(name=...)` gave it, spelled just so, or its own.
            let (method, shown) = match field.renamed {
                Some(renamed) => ("renamed", renamed),
                None => ("field", field.name),
            };
            self.tell(out, method, vec![Value::Str(shown), held], vec![field.ty], span)?;
        }
        for _ in 0..ends {
            self.tell(out, "end", Vec::new(), Vec::new(), span)?;
        }
        Ok(())
    }

    /// Calls the method `name` of the format at `out`, with `given`, and `types` for its type
    /// parameters in the order it lists them.
    fn tell(&mut self, out: &Place, name: &str, given: Vec<Value>, types: Vec<Type>, span: Span) -> Run<()> {
        let Some(Target::Def { def, .. }) = self.method_target(Receiver::Place(out.clone()), name, span)?
        else {
            return unsupported(format!("a `Serializer` the oracle doesn't model, with its `{name}`"));
        };
        if def.generics.len() != types.len() {
            return unsupported(format!("a `Serializer` whose `{name}` takes other type parameters"));
        }
        let bound = def.generics.iter().map(|generic| generic.name.clone()).zip(types).collect();
        let given = given.into_iter().map(|value| ArgVal::Value(None, value)).collect();
        self.call_def_bound((def, bound), None, Some(Receiver::Place(out.clone())), given, span)?;
        Ok(())
    }
}
