//! Reading a parsed value into the type a program asks for, as in `json.loads[Settings](text)`.
//! An `Id`, a `Table` and an enum case read back as `dump` wrote them, and a generic struct
//! reads its fields with its type arguments filled in.
//! A struct's field takes the type written for it, or else the shape of its default, which
//! the type checker makes sure shows the field's type. An int is read as a float where a
//! float is wanted, since JSON and TOML write whole floats without a decimal point.

use std::rc::Rc;

use fennec_syntax::{Field, Span, TypeExpr};

use crate::call::CallArg;
use crate::interp::{Interp, TypeKind};
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
            ("bytes", Value::Str(text)) => super::base64::decode(&text)
                .map(|data| Value::Bytes(data.into()))
                .ok_or_else(|| format!("{} should be bytes, written in base64", place(at))),
            ("list", Value::List(items)) => {
                let item = arg(0)?;
                let mut read = Vec::new();
                for (n, value) in items.iter().enumerate() {
                    read.push(self.decode(value.clone(), &item, &inside(at, &format!("[{n}]")), span)?);
                }
                Ok(Value::list(read))
            }
            ("dict", Value::Dict(pairs)) => {
                let (key_type, item) = (arg(0)?, arg(1)?);
                let mut read = Vec::new();
                for (key, value) in pairs.iter() {
                    let place = inside(at, &format!("[{}]", crate::format::repr(key)));
                    let key = object_key(key, &key_type, at)?;
                    read.push((key, self.decode(value.clone(), &item, &place, span)?));
                }
                Ok(Value::Dict(Rc::new(read.into())))
            }
            // A dict whose keys aren't text or ints is an array of `[key, value]` pairs.
            ("dict", Value::List(items)) => {
                let (key_type, item) = (arg(0)?, arg(1)?);
                let mut read = Vec::new();
                for (n, pair) in items.iter().enumerate() {
                    let place = inside(at, &format!("[{n}]"));
                    let Value::List(parts) = pair else {
                        return Err(misfit(&place, "[key, value] pair", pair));
                    };
                    let [key, value] = parts.as_slice() else {
                        return Err(format!("{} should be a [key, value] pair", self::place(&place)));
                    };
                    let key = self.decode(key.clone(), &key_type, &inside(&place, "[0]"), span)?;
                    read.push((key, self.decode(value.clone(), &item, &inside(&place, "[1]"), span)?));
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
            ("Id", Value::Int(position)) => {
                let tag = self.load_tag(&written(&arg(0)?));
                u32::try_from(position)
                    .map(|slot| Value::Id(Id { slot, generation: 0, tag }))
                    .map_err(|_| format!("{} is out of range", place(at)))
            }
            ("Table", Value::List(items)) => self.decode_table(&items, &arg(0)?, at, span),
            (name, value)
                if self.saves_plain(name)
                    && let Some(found) = self.read_plain(name, &value, at, span) =>
            {
                found
            }
            (name, value) if self.has_shape(name) => {
                let read = self.decode(value, &Self::shape_of(ty), at, span)?;
                self.build_shaped(name, read, at, span)
            }
            (name, value) if self.is_enum(name) => self.decode_case(ty, &value, at, span),
            (name, Value::Dict(pairs)) if self.is_struct(name) => self.decode_struct(ty, &pairs, at, span),
            (_, value) => Err(misfit(at, &written(ty), &value)),
        }
    }

    /// The tag a load gives the `Table` of `kind` it reads, and its `Id`s: new for each load.
    /// A new `Table` of `items`, read in order, with the tag this load gives its type.
    fn decode_table(
        &mut self,
        items: &[Value],
        item: &TypeExpr,
        at: &str,
        span: Span,
    ) -> Result<Value, Misfit> {
        let mut slots = Vec::new();
        for (n, held) in items.iter().enumerate() {
            let value = self.decode(held.clone(), item, &inside(at, &format!("[{n}]")), span)?;
            slots.push(Slot { generation: 0, value: Some(value) });
        }
        self.tables_read.push(at.to_owned());
        let tag = self.load_tag(&written(item));
        Ok(Value::Table(Rc::new(Table::tagged(slots, tag))))
    }

    fn load_tag(&mut self, kind: &str) -> u64 {
        *self.load_tags.entry(kind.to_owned()).or_insert_with(crate::value::new_tag)
    }

    fn is_struct(&self, name: &str) -> bool {
        self.type_info(name).is_some_and(|info| info.kind == TypeKind::Struct)
    }

    fn is_enum(&self, name: &str) -> bool {
        self.type_info(name).is_some_and(|info| info.kind == TypeKind::Enum)
    }

    /// An enum case: its name, or for one with fields, `{"Case": {fields}}`.
    fn decode_case(&mut self, ty: &TypeExpr, value: &Value, at: &str, span: Span) -> Result<Value, Misfit> {
        let name = ty.name.as_str();
        let info = self.type_info(name).ok_or_else(|| format!("`{name}` isn't a type"))?;
        let (case, pairs) = match value {
            Value::Str(case) => (case.to_string(), None),
            Value::Dict(pairs) if pairs.len() == 1 => match pairs.iter().next() {
                Some((Value::Str(case), Value::Dict(fields))) => (case.to_string(), Some(fields.clone())),
                _ => return Err(misfit(at, name, value)),
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
            Value::Err(error) => {
                let broken = match error.as_ref() {
                    Value::Instance(error) => match error.field("message") {
                        Some(Value::Str(message)) => message.to_string(),
                        _ => format!("a `where` rule of `{name}` is broken"),
                    },
                    _ => format!("a `where` rule of `{name}` is broken"),
                };
                Err(format!("in {}, {broken}", place(at)))
            }
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
        let names = self.reading.names;
        let ignore_extra = self.reading.extra == super::Extra::Ignore;
        // A field's name in the text: the one `@serialize` gives it, or its own as `names=` spells it.
        let wire = |field: &Field| field.saved_name().map_or_else(|| names.spell(&field.name), str::to_owned);
        let saved: Vec<&Field> = fields.iter().filter(|field| !field.is_skipped()).collect();
        for (key, _) in pairs {
            let known = matches!(key, Value::Str(key) if saved.iter().any(|field| wire(field) == **key));
            if known || ignore_extra {
                continue;
            }
            let shown = crate::format::repr(key);
            // A field the program has, but under another name or not saved at all.
            let own = fields
                .iter()
                .find(|field| matches!(key, Value::Str(key) if names.spell(&field.name) == **key));
            return Err(match own {
                Some(field) if field.is_skipped() => {
                    format!(
                        "{owner} doesn't save `{}`, which has `@skip`, but {} has it",
                        field.name,
                        place(at)
                    )
                }
                Some(field) => format!(
                    "{owner} saves `{}` as {}, but {} has {shown}",
                    field.name,
                    crate::format::repr(&Value::str(&wire(field))),
                    place(at)
                ),
                None => format!("{owner} has no field {shown}, but {} has one", place(at)),
            });
        }
        // A skipped field isn't in the text, so it takes its default.
        let mut args = Vec::new();
        for field in saved {
            let wire = wire(field);
            let found = pairs.iter().find(|(key, _)| matches!(key, Value::Str(key) if **key == *wire));
            let place = inside(at, &wire);
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
                self.decode_case(&ty, &value, at, span)
            }
            (like, value) => Err(misfit(at, &like.type_name(), &value)),
        }
    }
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
