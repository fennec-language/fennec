//! Reading a parsed value into the type a program asks for, as in `json.loads[Settings](text)`.
//! An `Id`, an `IdMap` and an enum case read back as `dump` wrote them, and a generic struct
//! reads its fields with its type arguments filled in.
//! A struct's field takes the type written for it, or else the shape of its default, which
//! the type checker makes sure shows the field's type. An int is read as a float where a
//! float is wanted, since JSON and TOML write whole floats without a decimal point.

use std::rc::Rc;

use fennec_syntax::{Field, Span, TypeExpr};

use crate::call::CallArg;
use crate::interp::{Interp, TypeKind};
use crate::ops::to_float;
use crate::value::{Id, IdMap, Slot, Value};

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

impl Interp<'_> {
    /// `value` as a value of type `ty`. `at` says where in the document it is, for messages.
    pub(crate) fn decode(
        &mut self,
        value: Value,
        ty: &TypeExpr,
        at: &str,
        span: Span,
    ) -> Result<Value, Misfit> {
        if ty.optional {
            if matches!(value, Value::None) {
                return Ok(Value::None);
            }
            return self.decode(value, &TypeExpr { optional: false, ..ty.clone() }, at, span);
        }
        let arg = |n: usize| ty.args.get(n).cloned().ok_or_else(|| format!("`{}` needs its types", ty.name));
        match (ty.name.as_str(), value) {
            ("int", value @ Value::Int(_))
            | ("float", value @ Value::Float(_))
            | ("str", value @ Value::Str(_))
            | ("bool", value @ Value::Bool(_))
            | ("None", value @ Value::None) => Ok(value),
            ("float", Value::Int(n)) => Ok(Value::Float(to_float(n))),
            ("list", Value::List(items)) => {
                let item = arg(0)?;
                let mut read = Vec::new();
                for (n, value) in items.iter().enumerate() {
                    read.push(self.decode(value.clone(), &item, &inside(at, &format!("[{n}]")), span)?);
                }
                Ok(Value::list(read))
            }
            ("dict", Value::Dict(pairs)) => {
                let item = arg(1)?;
                let mut read = Vec::new();
                for (key, value) in pairs.iter() {
                    let place = inside(at, &format!("[{}]", crate::format::repr(key)));
                    read.push((key.clone(), self.decode(value.clone(), &item, &place, span)?));
                }
                Ok(Value::Dict(Rc::new(read.into())))
            }
            ("tuple", Value::List(items)) if items.len() == ty.args.len() => {
                let mut read = Vec::new();
                for (n, (value, item)) in items.iter().zip(&ty.args).enumerate() {
                    read.push(self.decode(value.clone(), item, &inside(at, &format!("[{n}]")), span)?);
                }
                Ok(Value::Tuple(Rc::new(read)))
            }
            ("Id", Value::Dict(pairs)) => id_of(&pairs, at).map(Value::Id),
            ("IdMap", Value::Dict(pairs)) => self.decode_id_map(&pairs, &arg(0)?, at, span),
            (name, value) if self.is_enum(name) => self.decode_case(ty, value, at, span),
            (name, Value::Dict(pairs)) if self.is_struct(name) => self.decode_struct(ty, &pairs, at, span),
            (_, value) => Err(misfit(at, &written(ty), &value)),
        }
    }

    /// An `IdMap` from its slots: each one's generation, and its value unless it's free.
    fn decode_id_map(
        &mut self,
        pairs: &[(Value, Value)],
        item: &TypeExpr,
        at: &str,
        span: Span,
    ) -> Result<Value, Misfit> {
        let Some(Value::List(written)) = entry(pairs, "slots") else {
            return Err(format!("{} should be an IdMap, with its `slots`", place(at)));
        };
        let mut slots = Vec::new();
        for (n, slot) in written.iter().enumerate() {
            let here = inside(&inside(at, "slots"), &format!("[{n}]"));
            let Value::Dict(slot) = slot else {
                return Err(format!("{} should be a slot, with its `generation`", place(&here)));
            };
            let generation = number(entry(slot, "generation"), &inside(&here, "generation"))?;
            let value = match entry(slot, "value") {
                Some(value) => Some(self.decode(value.clone(), item, &inside(&here, "value"), span)?),
                None => None,
            };
            slots.push(Slot { generation, value });
        }
        Ok(Value::IdMap(Rc::new(IdMap { slots })))
    }

    fn is_struct(&self, name: &str) -> bool {
        self.type_info(name).is_some_and(|info| info.kind == TypeKind::Struct)
    }

    fn is_enum(&self, name: &str) -> bool {
        self.type_info(name).is_some_and(|info| info.kind == TypeKind::Enum)
    }

    /// An enum case: its name, or for one with fields, `{"Case": {fields}}`.
    fn decode_case(&mut self, ty: &TypeExpr, value: Value, at: &str, span: Span) -> Result<Value, Misfit> {
        let name = ty.name.as_str();
        let info = self.type_info(name).ok_or_else(|| format!("`{name}` isn't a type"))?;
        let (case, pairs) = match &value {
            Value::Str(case) => (case.to_string(), None),
            Value::Dict(pairs) if pairs.len() == 1 => match pairs.iter().next() {
                Some((Value::Str(case), Value::Dict(fields))) => (case.to_string(), Some(fields.clone())),
                _ => return Err(misfit(at, name, &value)),
            },
            other => return Err(misfit(at, name, other)),
        };
        let Some(found) = info.def.cases.iter().find(|each| each.name == case) else {
            return Err(format!("{} is {case:?}, which isn't a case of `{name}`", place(at)));
        };
        let given = given_types(&info.def.generics, ty);
        let args = match (pairs, found.fields.is_empty()) {
            (None, true) => Vec::new(),
            (Some(pairs), _) => {
                self.decode_fields(&found.fields, &given, &pairs, (&format!("`{name}.{case}`"), at), span)?
            }
            (None, false) => return Err(format!("{} is `{name}.{case}`, which needs its fields", place(at))),
        };
        self.construct_variant(Some(name), &case, args, span).map_err(|problem| problem.message)
    }

    /// A struct from a table of its fields. A field left out takes its default.
    fn decode_struct(
        &mut self,
        ty: &TypeExpr,
        pairs: &[(Value, Value)],
        at: &str,
        span: Span,
    ) -> Result<Value, Misfit> {
        let name = ty.name.as_str();
        let (fields, generics) = self
            .type_info(name)
            .map(|info| (info.def.fields.clone(), info.def.generics.clone()))
            .unwrap_or_default();
        let given = given_types(&generics, ty);
        let args = self.decode_fields(&fields, &given, pairs, (&format!("`{name}`"), at), span)?;
        match self.construct(name, args, span).map_err(|problem| problem.message)? {
            Value::Ok(built) => Ok(built.as_ref().clone()),
            Value::Err(_) => Err(format!("{} breaks a `where` rule of `{name}`", place(at))),
            built => Ok(built),
        }
    }

    /// The fields of a struct or case, read from a table of them. `owner` is the struct or
    /// case, as messages say it, and where it is in the document.
    fn decode_fields(
        &mut self,
        fields: &[Field],
        given: &[(String, TypeExpr)],
        pairs: &[(Value, Value)],
        (owner, at): (&str, &str),
        span: Span,
    ) -> Result<Vec<CallArg>, Misfit> {
        for (key, _) in pairs {
            let known = matches!(key, Value::Str(key) if fields.iter().any(|field| *field.name == **key));
            if !known {
                return Err(format!(
                    "{owner} has no field {}, but {} has one",
                    crate::format::repr(key),
                    place(at)
                ));
            }
        }
        let mut args = Vec::new();
        for field in fields {
            let found = pairs.iter().find(|(key, _)| matches!(key, Value::Str(key) if **key == *field.name));
            let place = inside(at, &field.name);
            let value = match (found, &field.ty, &field.default) {
                (Some((_, value)), Some(ty), _) => {
                    self.decode(value.clone(), &substituted(ty, given), &place, span)?
                }
                (Some((_, value)), None, Some(default)) => {
                    let like = self.eval(default).map_err(|problem| problem.message)?;
                    self.conform(value.clone(), &like, &place, span)?
                }
                (None, _, Some(_)) => continue,
                _ => return Err(format!("{} is missing", self::place(&place))),
            };
            args.push(CallArg { name: Some(field.name.clone()), ..CallArg::plain(value, span) });
        }
        Ok(args)
    }

    /// `value` in the shape of `like`, a field's default.
    fn conform(&mut self, value: Value, like: &Value, at: &str, span: Span) -> Result<Value, Misfit> {
        match (like, value) {
            (Value::Float(_), Value::Int(n)) => Ok(Value::Float(to_float(n))),
            (Value::Int(_), value @ Value::Int(_))
            | (Value::Float(_), value @ Value::Float(_))
            | (Value::Str(_), value @ Value::Str(_))
            | (Value::Bool(_), value @ Value::Bool(_)) => Ok(value),
            (Value::List(defaults), Value::List(items)) => {
                let mut read = Vec::new();
                for (n, value) in items.iter().enumerate() {
                    read.push(match defaults.first() {
                        Some(first) => {
                            self.conform(value.clone(), first, &inside(at, &format!("[{n}]")), span)?
                        }
                        None => value.clone(),
                    });
                }
                Ok(Value::list(read))
            }
            (Value::Dict(defaults), Value::Dict(pairs)) => {
                let mut read = Vec::new();
                for (key, value) in pairs.iter() {
                    let place = inside(at, &format!("[{}]", crate::format::repr(key)));
                    read.push((
                        key.clone(),
                        match defaults.first() {
                            Some((_, first)) => self.conform(value.clone(), first, &place, span)?,
                            None => value.clone(),
                        },
                    ));
                }
                Ok(Value::Dict(Rc::new(read.into())))
            }
            (Value::Tuple(defaults), Value::List(items)) if defaults.len() == items.len() => {
                let mut read = Vec::new();
                for (n, (value, first)) in items.iter().zip(defaults.iter()).enumerate() {
                    read.push(self.conform(value.clone(), first, &inside(at, &format!("[{n}]")), span)?);
                }
                Ok(Value::Tuple(Rc::new(read)))
            }
            (Value::Instance(instance), Value::Dict(pairs)) if instance.case.is_none() => {
                let ty = TypeExpr {
                    name: instance.type_name.to_string(),
                    args: Vec::new(),
                    optional: false,
                    span,
                };
                self.decode_struct(&ty, &pairs, at, span)
            }
            (Value::Instance(instance), value) => {
                let ty = TypeExpr {
                    name: instance.type_name.to_string(),
                    args: Vec::new(),
                    optional: false,
                    span,
                };
                self.decode_case(&ty, value, at, span)
            }
            (like, value) => Err(misfit(at, &like.type_name(), &value)),
        }
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

/// The value of `name` in an object, if it has one.
fn entry<'p>(pairs: &'p [(Value, Value)], name: &str) -> Option<&'p Value> {
    pairs.iter().find(|(key, _)| matches!(key, Value::Str(key) if **key == *name)).map(|(_, value)| value)
}

/// A slot's generation, or an `Id`'s, which is a whole number that isn't negative.
fn number<N: TryFrom<i64>>(value: Option<&Value>, at: &str) -> Result<N, Misfit> {
    match value {
        Some(Value::Int(n)) => N::try_from(*n).map_err(|_| format!("{} is out of range", place(at))),
        Some(other) => Err(misfit(at, "int", other)),
        None => Err(format!("{} is missing", place(at))),
    }
}

/// An `Id` from its slot and generation.
fn id_of(pairs: &[(Value, Value)], at: &str) -> Result<Id, Misfit> {
    for (key, _) in pairs {
        if !matches!(key, Value::Str(key) if **key == *"slot" || **key == *"generation") {
            return Err(format!(
                "an Id has only `slot` and `generation`, but {} has {}",
                place(at),
                crate::format::repr(key)
            ));
        }
    }
    let slot = number(entry(pairs, "slot"), &inside(at, "slot"))?;
    let generation = number(entry(pairs, "generation"), &inside(at, "generation"))?;
    Ok(Id { slot, generation })
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
    if name.starts_with(['a', 'e', 'i', 'o', 'u']) { format!("an {name}") } else { format!("a {name}") }
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
