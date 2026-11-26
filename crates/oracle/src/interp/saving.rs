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
//! - A tuple is `out.items(name, count)`, `name` its type as written, then each item as
//!   `out.item(x)`, then `out.end_items()`.
//! - A union's value is `out.member(name, at)`, `name` the type of the member it holds as written
//!   and `at` where that member is among the union's, then `out.value(x)`, then
//!   `out.end_member()`. One more than one member could hold, as an empty list, is refused.
//!
//! What each is called with is typed as the value's own parts are, so `out.sequence` of a
//! `list[int?]` is called with `T` an `int?`. A `Table` and an `Id` aren't modeled here.

use fennec_check::Type;
use fennec_syntax::Span;

use super::calls::{ArgVal, Receiver, Target};
use super::json::type_name;
use super::{Flow, Interp, Place, Run, unsupported};
use crate::load::Kind;
use crate::value::{Held, Text, Value};

impl Interp {
    /// `object.serialize(ref out)`, when it's modeled here: its type, as the checker typed
    /// `object`, is known and has no `def serialize` of its own.
    pub(crate) fn serialize_target(&mut self, object: &'static fennec_syntax::Expr) -> Run<Option<Target>> {
        let Some(ty) = self.static_type(object.span) else { return Ok(None) };
        if let Type::Named(name) | Type::Generic(name, _) = &ty
            && (self.program.method(name, "serialize").is_some() || !self.program.types.contains_key(name))
        {
            return Ok(None);
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
            (Type::Sized(fennec_check::Num::F32), Value::F32(_)) => {
                self.tell(out, "f32", vec![value.clone()], Vec::new(), span)
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
                let pairs = pairs
                    .iter()
                    .map(|(key, value)| Value::Tuple(Held::new(vec![key.clone(), value.clone()])))
                    .collect();
                let types = vec![(**key).clone(), (**held).clone()];
                self.tell(out, "map", vec![Value::List(pairs)], types, span)
            }
            (Type::Tuple(types), Value::Tuple(items)) if types.len() == items.len() => {
                let Some(name) = ty.source() else {
                    return unsupported("a tuple whose type can't be written");
                };
                let count = Value::Int(i64::try_from(items.len()).unwrap_or(i64::MAX));
                self.tell(out, "items", vec![Value::Str(Text::from(name)), count], Vec::new(), span)?;
                for (item, item_type) in items.iter().zip(types) {
                    self.tell(out, "item", vec![item.clone()], vec![item_type.clone()], span)?;
                }
                self.tell(out, "end_items", Vec::new(), Vec::new(), span)
            }
            (Type::Union(members), held) => {
                let holding: Vec<(usize, &Type)> =
                    members.iter().enumerate().filter(|(_, member)| holds(member, held)).collect();
                let [(at, member)] = holding.as_slice() else {
                    return unsupported("saving a union's value that isn't one member's alone");
                };
                let Some(name) = member.source() else {
                    return unsupported("a union's member that can't be written");
                };
                let at = Value::Int(i64::try_from(*at).unwrap_or(i64::MAX));
                self.tell(out, "member", vec![Value::Str(Text::from(name)), at], Vec::new(), span)?;
                self.tell(out, "value", vec![held.clone()], vec![(*member).clone()], span)?;
                self.tell(out, "end_member", Vec::new(), Vec::new(), span)
            }
            (Type::Table(item), Value::Table(table)) => self.serialize_table(table, item, out, span),
            (Type::Id(item), Value::Id(id)) => {
                let kind = Value::Str(Text::from(type_name(item)));
                let given = vec![kind, Value::Int(id_key(*id)?), Value::Int(tag_int(id.tag)?)];
                self.tell(out, "id", given, Vec::new(), span)
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

    /// What a `Table` calls on `out`, as std's `serialize` for one does: its slots and the items in
    /// them where the format saves `Table`s slot for slot, and otherwise its items in order, each
    /// with its `Id`.
    fn serialize_table(
        &mut self,
        table: &crate::value::Table,
        item: &Type,
        out: &Place,
        span: Span,
    ) -> Run<()> {
        let kind = Value::Str(Text::from(type_name(item)));
        let items: Vec<Value> = table.live().map(|(_, held)| held.clone()).collect();
        let exact = matches!(self.ask(out, "exact", Vec::new(), Vec::new(), span)?, Value::Bool(true));
        if exact {
            // A slot in use is 0, and one taken out -1, as `table_slots` gives them.
            let slots =
                table.slots.iter().map(|slot| Value::Int(if slot.item.is_some() { 0 } else { -1 })).collect();
            // Each slot's tag, which says which `Id`s can be saved as their slot and generation.
            let tags = table
                .slots
                .iter()
                .map(|slot| tag_int(slot.tag).map(Value::Int))
                .collect::<Run<Vec<Value>>>()?;
            let given = vec![
                kind,
                Value::List(Held::new(tags)),
                Value::List(Held::new(slots)),
                Value::List(Held::new(items)),
            ];
            return self.tell(out, "slots", given, vec![item.clone()], span);
        }
        let ids = table.live().map(|(id, _)| Value::Id(id)).collect();
        let given = vec![kind, Value::List(Held::new(ids)), Value::List(Held::new(items))];
        self.tell(out, "table", given, vec![item.clone()], span)
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
        // Looked up by the name it's checked under, and saved by the name it's written with.
        let shown = fennec_syntax::rename::shown_type_name(name);
        let Some(declared) = self.program.types.get(name) else {
            return unsupported(format!("saving a {shown} through a format's own `Serializer`"));
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
        let shown = Value::Str(Text::from(shown.to_owned()));
        let mut ends = 1;
        match &instance.case {
            None => self.tell(out, "record", vec![shown, count], Vec::new(), span)?,
            Some(case) => {
                let at = declared.def.cases.iter().position(|known| known.name == *case).unwrap_or_default();
                let at = Value::Int(i64::try_from(at).unwrap_or(i64::MAX));
                let case_name = Value::Str(Text::from(case.clone()));
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
            self.tell(out, method, vec![Value::Str(Text::from(shown)), held], vec![field.ty], span)?;
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

impl Interp {
    /// `owner.deserialize(ref src)`, where `owner` is a type parameter, a built-in type, or a type
    /// the program declares: its type's own `def deserialize`, with its type's parameters bound,
    /// or the one the oracle models. `None` for any other name.
    pub(crate) fn deserialize_target(&mut self, owner: &str) -> Run<Option<Target>> {
        let ty = match self.resolved(&Type::Param(owner.to_owned())) {
            Type::Param(_) => match owner {
                "int" => Type::Int,
                "float" => Type::Float,
                "f32" => Type::Sized(fennec_check::Num::F32),
                "bool" => Type::Bool,
                "str" => Type::Str,
                "bytes" => Type::Bytes,
                _ if self.program.types.contains_key(owner) => Type::Named(owner.to_owned()),
                _ => return Ok(None),
            },
            ty => ty,
        };
        if !super::typed::settled(&ty) {
            return unsupported("loading a value whose type parameter isn't known where it's loaded");
        }
        if let Type::Named(name) | Type::Generic(name, _) = &ty
            && let Some(def) = self.program.method(name, "deserialize")
        {
            let args = match &ty {
                Type::Generic(_, args) => args.as_slice(),
                _ => &[],
            };
            let types = self.own_bound(def, name, args)?;
            return Ok(Some(Target::Def { def, parent: None, receiver: None, types }));
        }
        Ok(Some(Target::Deserialize(ty)))
    }

    /// What `T.deserialize(ref src)` reads through `src`, for a `T` of type `ty`: a plain value by
    /// its own method, an optional, a list, a set or a dict as the core's `extend`s read them,
    /// and a struct or an enum's case as the `def deserialize` the checker writes for it.
    pub(crate) fn deserialize_from(&mut self, ty: &Type, src: &Place, span: Span) -> Run<Value> {
        self.tick()?;
        match ty {
            Type::Int => self.ask(src, "int", Vec::new(), Vec::new(), span),
            Type::Float => self.ask(src, "float", Vec::new(), Vec::new(), span),
            Type::Sized(fennec_check::Num::F32) => self.ask(src, "f32", Vec::new(), Vec::new(), span),
            Type::Bool => self.ask(src, "bool", Vec::new(), Vec::new(), span),
            Type::Str | Type::LiteralStr => self.ask(src, "text", Vec::new(), Vec::new(), span),
            Type::Bytes => self.ask(src, "bytes", Vec::new(), Vec::new(), span),
            Type::Optional(inner) => {
                if self.asked_yes(src, "none", Vec::new(), span)? {
                    return Ok(Value::None);
                }
                self.ask(src, "value", Vec::new(), vec![(**inner).clone()], span)
            }
            Type::List(inner) => self.ask(src, "sequence", Vec::new(), vec![(**inner).clone()], span),
            Type::Set(inner) => {
                let Value::List(items) =
                    self.ask(src, "sequence", Vec::new(), vec![(**inner).clone()], span)?
                else {
                    return unsupported("a `sequence` that gave back what isn't a list");
                };
                let mut found: Vec<Value> = Vec::with_capacity(items.len());
                for item in items {
                    if !self.contains(&Value::Set(Held::new(found.clone())), &item)? {
                        found.push(item);
                    }
                }
                Ok(Value::Set(Held::new(found)))
            }
            Type::Dict(key, held) => self.dict_from(key, held, src, span),
            Type::Union(members) => self.member_from(members, src, span),
            Type::Table(item) => self.table_from(item, src, span),
            Type::Id(item) => self.id_from(item, src, span),
            Type::Named(name) => self.instance_from(name, &[], src, span),
            Type::Generic(name, args) => self.instance_from(name, args, src, span),
            other => {
                unsupported(format!("loading a {} through a format's own `Deserializer`", type_name(other)))
            }
        }
    }

    /// A `Table`, as std's `deserialize` for one reads it: slot for slot where the format saves
    /// them so, and otherwise its items, added in order to a new `Table` with the tag the read
    /// gives its kind.
    fn table_from(&mut self, item: &Type, src: &Place, span: Span) -> Run<Value> {
        let kind = Value::Str(Text::from(type_name(item)));
        if self.asked_yes(src, "exact", Vec::new(), span)? {
            let Value::List(slots) =
                self.ask(src, "value", Vec::new(), vec![Type::List(Box::new(Type::Int))], span)?
            else {
                return unsupported("slots read as what isn't a list");
            };
            let Value::List(items) =
                self.ask(src, "value", Vec::new(), vec![Type::List(Box::new(item.clone()))], span)?
            else {
                return unsupported("items read as what isn't a list");
            };
            let mut words = Vec::with_capacity(slots.len());
            for slot in &slots {
                let Value::Int(word) = slot else { return unsupported("a slot that isn't an int") };
                if *word > 4_294_967_295 || *word < -4_294_967_296 {
                    return self.failed_here(src, "has a slot past the last generation".to_owned(), span);
                }
                words.push(*word);
            }
            let live = words.iter().filter(|word| **word >= 0).count();
            if live != items.len() {
                let used = if live == 1 { "1 slot".to_owned() } else { format!("{live} slots") };
                return self.failed_here(src, format!("has {used} in use but {} items", items.len()), span);
            }
            let count = Value::Int(super::as_int(words.len()));
            let seen = Value::List(Held::new(words.iter().map(|word| Value::Int(*word)).collect()));
            self.ask(src, "saw_table", vec![kind.clone(), count, seen], Vec::new(), span)?;
            let tag = self.read_tag(src, kind, span)?;
            let mut items = items.into_inner().into_iter();
            let mut table = crate::value::Table::new(tag);
            // A slot in use gets the load's tag, which its `Id`s have, and one taken out none.
            for word in words {
                let held = if word >= 0 { items.next() } else { None };
                let slot_tag = if held.is_some() { tag } else { 0 };
                table.slots.push(crate::value::Slot { tag: slot_tag, item: held });
            }
            return Ok(Value::Table(Box::new(table)));
        }
        let Value::List(items) =
            self.ask(src, "value", Vec::new(), vec![Type::List(Box::new(item.clone()))], span)?
        else {
            return unsupported("items read as what isn't a list");
        };
        let count = items.len();
        let tag = self.read_tag(src, kind.clone(), span)?;
        let mut table = crate::value::Table::new(tag);
        for held in items.into_inner() {
            table.add(held, tag);
        }
        let seen = Value::List(Held::new(Vec::new()));
        self.ask(src, "saw_table", vec![kind, Value::Int(super::as_int(count)), seen], Vec::new(), span)?;
        Ok(Value::Table(Box::new(table)))
    }

    /// An `Id`, as std's `deserialize` for one reads it: its slot and generation where the format
    /// saves `Table`s slot for slot, and otherwise where its item is in its `Table`.
    fn id_from(&mut self, item: &Type, src: &Place, span: Span) -> Run<Value> {
        let kind = Value::Str(Text::from(type_name(item)));
        let int = |interp: &mut Interp| match interp.ask(src, "value", Vec::new(), vec![Type::Int], span)? {
            Value::Int(n) => Ok(n),
            other => unsupported(format!("an Id read as a {}", other.kind())),
        };
        if self.asked_yes(src, "exact", Vec::new(), span)? {
            let slot = int(self)?;
            let generation = int(self)?;
            if slot < 0 || !(0..=4_294_967_295).contains(&generation) {
                return self.failed_here(src, "is out of range".to_owned(), span);
            }
            self.ask(
                src,
                "saw_id",
                vec![kind.clone(), Value::Int(slot), Value::Int(generation)],
                Vec::new(),
                span,
            )?;
            let tag = self.read_tag(src, kind, span)?;
            let slot = usize::try_from(slot).unwrap_or(usize::MAX);
            let generation = u32::try_from(generation).unwrap_or(u32::MAX);
            return Ok(Value::Id(crate::value::Id { slot, generation, tag }));
        }
        let position = int(self)?;
        if position < 0 {
            return self.failed_here(src, "is out of range".to_owned(), span);
        }
        self.ask(src, "saw_id", vec![kind.clone(), Value::Int(position), Value::Int(-1)], Vec::new(), span)?;
        let tag = self.read_tag(src, kind, span)?;
        let slot = usize::try_from(position).unwrap_or(usize::MAX);
        Ok(Value::Id(crate::value::Id { slot, generation: 0, tag }))
    }

    /// The tag `src` gives the `Table`s of `kind` it reads, and their `Id`s.
    fn read_tag(&mut self, src: &Place, kind: Value, span: Span) -> Run<u64> {
        match self.ask(src, "tag", vec![kind], Vec::new(), span)? {
            Value::Int(tag) => u64::try_from(tag).map_or_else(|_| unsupported("a negative tag"), Ok),
            other => unsupported(format!("a tag that's a {}", other.kind())),
        }
    }

    /// Fails the load with `src.here(message)`, the error std's readers give for what was just
    /// read.
    fn failed_here<T>(&mut self, src: &Place, message: String, span: Span) -> Run<T> {
        let error = self.ask(src, "here", vec![Value::Str(Text::from(message))], Vec::new(), span)?;
        Err(Flow::Fail(error, span))
    }

    /// A union's value, as the `def deserialize` the checker writes for one reads it: `src.member`
    /// of its members' names says which comes next, `src.value` reads it, and `src.end_member`
    /// finishes it.
    fn member_from(&mut self, members: &[Type], src: &Place, span: Span) -> Run<Value> {
        let names = members
            .iter()
            .map(|member| Value::Str(Text::from(super::json::read::member_name(member))))
            .collect();
        let Value::Int(at) =
            self.ask(src, "member", vec![Value::List(Held::new(names))], Vec::new(), span)?
        else {
            return unsupported("a `member` that isn't an int");
        };
        let Some(member) = usize::try_from(at).ok().and_then(|at| members.get(at)) else {
            return unsupported("a `member` past the union's members");
        };
        let value = self.ask(src, "value", Vec::new(), vec![member.clone()], span)?;
        self.ask(src, "end_member", Vec::new(), Vec::new(), span)?;
        Ok(value)
    }

    /// A dict, as the core's `extend` of it reads one: its pairs counted first where the format
    /// says how many, or all at once, a key that comes twice failing, said where it is.
    fn dict_from(&mut self, key: &Type, held: &Type, src: &Place, span: Span) -> Run<Value> {
        let Value::Int(count) = self.ask(src, "map_count", Vec::new(), Vec::new(), span)? else {
            return unsupported("a `map_count` that isn't an int");
        };
        let mut found: Vec<(Value, Value)> = Vec::new();
        if count >= 0 {
            for _ in 0..count {
                let read = self.ask(src, "value", Vec::new(), vec![key.clone()], span)?;
                let value = self.ask(src, "value", Vec::new(), vec![held.clone()], span)?;
                self.put_once((read, value), &mut found, src, span)?;
            }
            self.ask(src, "end_map", Vec::new(), Vec::new(), span)?;
            return Ok(Value::Dict(Held::new(found)));
        }
        let Value::List(pairs) = self.ask(src, "map", Vec::new(), vec![key.clone(), held.clone()], span)?
        else {
            return unsupported("a `map` that gave back what isn't a list");
        };
        for pair in pairs {
            let Value::Tuple(both) = pair else { return unsupported("a pair that isn't one") };
            let Ok([read, value]) = <[Value; 2]>::try_from(both.into_inner()) else {
                return unsupported("a pair that isn't one");
            };
            self.put_once((read, value), &mut found, src, span)?;
        }
        Ok(Value::Dict(Held::new(found)))
    }

    /// Puts a pair read into a dict, failing as `src` says where a key comes twice.
    fn put_once(
        &mut self,
        (key, value): (Value, Value),
        found: &mut Vec<(Value, Value)>,
        src: &Place,
        span: Span,
    ) -> Run<()> {
        if super::find_key(found, &key)?.is_some() {
            let twice = super::json::read::twice_key(&self.display(&key)?);
            let error = self.ask(
                src,
                "here",
                vec![Value::Str(Text::from(format!("has {twice} twice")))],
                Vec::new(),
                span,
            )?;
            return Err(Flow::Fail(error, span));
        }
        found.push((key, value));
        Ok(())
    }

    /// A struct, or one of an enum's cases, as the `def deserialize` the checker writes reads it.
    fn instance_from(&mut self, name: &str, args: &[Type], src: &Place, span: Span) -> Run<Value> {
        // Looked up by the name it's checked under, and read by the name it's written with.
        let shown = fennec_syntax::rename::shown_type_name(name);
        let Some(declared) = self.program.types.get(name) else {
            return unsupported(format!("loading a {shown} through a format's own `Deserializer`"));
        };
        if declared.kind == Kind::Error || declared.def.shared {
            return unsupported(format!("loading a {name} through a format's own `Deserializer`"));
        }
        let def = declared.def;
        if declared.kind != Kind::Enum {
            let fields = self.saved_fields(name, None, args)?;
            return self.record_from((name, None), &fields, src, span);
        }
        let cases = def
            .cases
            .iter()
            .map(|case| {
                Value::Tuple(Held::new(vec![
                    Value::Str(Text::from(case.name.clone())),
                    Value::Bool(!case.fields.is_empty()),
                ]))
            })
            .collect();
        let asked = vec![Value::Str(Text::from(shown.to_owned())), Value::List(cases)];
        let Value::Int(at) = self.ask(src, "enum_case", asked, Vec::new(), span)? else {
            return unsupported("an `enum_case` that isn't an int");
        };
        let Some(case) = usize::try_from(at).ok().and_then(|at| def.cases.get(at)) else {
            let message = Value::Str(Text::from(format!("`{shown}` has no such case")));
            let error = self.ask(src, "rule", vec![message], Vec::new(), span)?;
            return Err(Flow::Fail(error, span));
        };
        if case.fields.is_empty() {
            return self.construct(name, Some(&case.name), Vec::new(), span);
        }
        let fields = self.saved_fields(name, Some(&case.name), args)?;
        self.record_from((name, Some(&case.name)), &fields, src, span)
    }

    /// The fields of a struct, or of a case, read: in order, when the format saves no names, or
    /// each by the name the format gives, those left out taking their defaults, a missing value
    /// read for a field with one taking it too, and one left out with none failing.
    fn record_from(
        &mut self,
        (name, case): (&str, Option<&str>),
        fields: &[super::json::SavedField],
        src: &Place,
        span: Span,
    ) -> Run<Value> {
        let shown =
            Value::Str(Text::from(case.unwrap_or(fennec_syntax::rename::shown_type_name(name)).to_owned()));
        let saved: Vec<&super::json::SavedField> = fields.iter().filter(|field| !field.skipped).collect();
        let mut given = Vec::with_capacity(saved.len());
        if self.asked_yes(src, "ordered", vec![shown.clone()], span)? {
            for field in &saved {
                let read = self.ask(src, "value", Vec::new(), vec![field.ty.clone()], span)?;
                given.push(ArgVal::Value(Some(field.name.clone()), read));
            }
            self.ask(src, "end_ordered", Vec::new(), Vec::new(), span)?;
            if case.is_some() {
                self.ask(src, "end_case", Vec::new(), Vec::new(), span)?;
            }
            return self.loaded_made((name, case), given, src, span);
        }
        self.ask(src, "record", vec![shown], Vec::new(), span)?;
        let listed = saved
            .iter()
            .map(|field| field.saved_name())
            .zip(saved.iter().map(|field| field.name.as_str()))
            .chain(fields.iter().filter(|field| field.skipped).map(|field| ("", field.name.as_str())))
            .map(|(as_saved, own)| {
                Value::Tuple(Held::new(vec![
                    Value::Str(Text::from(as_saved.to_owned())),
                    Value::Str(Text::from(own.to_owned())),
                ]))
            })
            .collect();
        let listed = Value::List(listed);
        // What was read for each field: nothing yet, or a value, which for an optional field may
        // be `None`.
        let mut read: Vec<Option<Value>> = vec![None; saved.len()];
        while self.asked_yes(src, "next_field", vec![listed.clone()], span)? {
            let Value::Int(at) = self.ask(src, "field_at", Vec::new(), Vec::new(), span)? else {
                return unsupported("a `field_at` that isn't an int");
            };
            let Some((field, slot)) =
                usize::try_from(at).ok().and_then(|at| saved.get(at).zip(read.get_mut(at)))
            else {
                continue;
            };
            let optional = matches!(field.ty, Type::Optional(_));
            // A format that can say a value isn't there may say so for any field, which then
            // takes its default; an optional field reads it as its own `None`.
            if !optional
                && self.asked_yes(src, "none_next", Vec::new(), span)?
                && self.asked_yes(src, "none", Vec::new(), span)?
            {
                *slot = None;
                continue;
            }
            let value = self.ask(src, "value", Vec::new(), vec![field.ty.clone()], span)?;
            if let Some(slot) = usize::try_from(at).ok().and_then(|at| read.get_mut(at)) {
                *slot = Some(value);
            }
        }
        for (field, value) in saved.iter().zip(read) {
            let optional = matches!(field.ty, Type::Optional(_));
            match value {
                Some(value) => given.push(ArgVal::Value(Some(field.name.clone()), value)),
                None if field.field.default.is_some() => {}
                None if optional => given.push(ArgVal::Value(Some(field.name.clone()), Value::None)),
                None => {
                    let missing = Value::Str(Text::from(field.saved_name().to_owned()));
                    let error = self.ask(src, "missing", vec![missing], Vec::new(), span)?;
                    return Err(Flow::Fail(error, span));
                }
            }
        }
        if case.is_some() {
            self.ask(src, "end_case", Vec::new(), Vec::new(), span)?;
        }
        self.loaded_made((name, case), given, src, span)
    }

    /// A value loaded, made from what was read: one that breaks a `where` rule fails the load
    /// through `src.rule`, with the words making it would say, as the `def deserialize` the
    /// checker writes does.
    fn loaded_made(
        &mut self,
        (name, case): (&str, Option<&str>),
        given: Vec<ArgVal>,
        src: &Place,
        span: Span,
    ) -> Run<Value> {
        let made = self.made_unruled(name, case, given)?;
        if let Value::Instance(instance) = &made
            && let Some(broken) = self.broken_rule(instance)?
        {
            let shown = fennec_syntax::rename::shown_type_name(name);
            let message = Value::Str(Text::from(format!("{shown}.{broken} breaks its `where` rule")));
            let error = self.ask(src, "rule", vec![message], Vec::new(), span)?;
            return Err(Flow::Fail(error, span));
        }
        Ok(made)
    }

    /// Calls the method `name` of the format at `src`, as `tell` does, and gives back what it
    /// gives.
    fn ask(
        &mut self,
        src: &Place,
        name: &str,
        given: Vec<Value>,
        types: Vec<Type>,
        span: Span,
    ) -> Run<Value> {
        let Some(Target::Def { def, .. }) = self.method_target(Receiver::Place(src.clone()), name, span)?
        else {
            return unsupported(format!("a `Deserializer` the oracle doesn't model, with its `{name}`"));
        };
        let generics: Vec<String> = def
            .generics
            .iter()
            .map(|generic| generic.name.clone())
            .filter(|generic| !generic.contains('\''))
            .collect();
        if generics.len() != types.len() {
            return unsupported(format!("a `Deserializer` whose `{name}` takes other type parameters"));
        }
        let bound = generics.into_iter().zip(types).collect();
        let given = given.into_iter().map(|value| ArgVal::Value(None, value)).collect();
        self.call_def_bound((def, bound), None, Some(Receiver::Place(src.clone())), given, span)
    }

    /// `ask`, for a method that says yes or no.
    fn asked_yes(&mut self, src: &Place, name: &str, given: Vec<Value>, span: Span) -> Run<bool> {
        match self.ask(src, name, given, Vec::new(), span)? {
            Value::Bool(yes) => Ok(yes),
            other => unsupported(format!("a `{name}` that gives a {}", other.kind())),
        }
    }
}

/// Whether a value could be one a union's member `member` holds, by what kind of value it is.
pub(crate) fn holds(member: &Type, value: &Value) -> bool {
    match (member, value) {
        (Type::Int, Value::Int(_))
        | (Type::Float, Value::Float(_))
        | (Type::Sized(fennec_check::Num::F32), Value::F32(_))
        | (Type::Bool, Value::Bool(_))
        | (Type::Str | Type::LiteralStr, Value::Str(_))
        | (Type::Bytes, Value::Bytes(_))
        | (Type::List(_), Value::List(_))
        | (Type::Set(_), Value::Set(_))
        | (Type::Dict(..), Value::Dict(_))
        | (Type::Tuple(_), Value::Tuple(_))
        | (Type::None | Type::Optional(_), Value::None) => true,
        (Type::Named(name) | Type::Generic(name, _), Value::Instance(instance)) => instance.ty == *name,
        _ => false,
    }
}

/// A `Table`'s tag as std's `table_tag` gives it.
fn tag_int(tag: u64) -> Run<i64> {
    i64::try_from(tag).map_or_else(|_| unsupported("a `Table` tag past the largest int"), Ok)
}

/// An `Id`'s slot and generation in one number, the slot above the generation's 32 bits, as
/// std's `id_key` gives it.
fn id_key(id: crate::value::Id) -> Run<i64> {
    match i64::try_from(id.slot) {
        Ok(slot) if slot < 1 << 31 => Ok(slot << 32 | i64::from(id.generation)),
        _ => unsupported("an `Id` whose slot doesn't fit in 31 bits"),
    }
}
