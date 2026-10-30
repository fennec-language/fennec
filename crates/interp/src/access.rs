//! Reading fields (`enemy.health`, `player.weapon?.damage`) and indexing (`scores[0]`,
//! `world.enemies[id]`, and types with their own `def []`).

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use crate::eval::unhandled_result;
use crate::interp::{Interp, TypeKind};
use crate::ops;
use crate::value::Value;

impl Interp<'_> {
    pub(crate) fn attr(
        &mut self,
        value: Value,
        name: &str,
        optional: bool,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        match value {
            Value::None if optional => Ok(Value::None),
            Value::None => Err(might_be_none(span)),
            Value::Ok(_) | Value::Err(_) => Err(unhandled_result(span)),
            Value::Instance(instance) => instance.field(name).cloned().ok_or_else(|| {
                Diagnostic::new(format!("`{}` has no field `{name}`", instance.type_name), span)
                    .with_help("call a method with parentheses, like `x.length()`")
            }),
            Value::Type(type_name) => self.type_attr(&type_name, name, span),
            other => Err(Diagnostic::new(format!("a {} has no field `{name}`", other.type_name()), span)),
        }
    }

    /// `Shape.Empty`, or a named constructor that takes nothing, like `Vec3.zero`.
    fn type_attr(&mut self, type_name: &str, name: &str, span: Span) -> Result<Value, Diagnostic> {
        let info = self.type_info(type_name);
        if let Some(info) = &info
            && info.kind == TypeKind::Enum
            && info.def.cases.iter().any(|case| case.name == name)
        {
            return self.construct_variant(Some(type_name), name, Vec::new(), span);
        }
        let constructor =
            info.and_then(|info| info.methods.get(name).cloned()).filter(|method| !method.def.takes_self());
        match constructor {
            Some(constructor) if constructor.def.params.is_empty() => {
                self.call_with_values(&constructor, Vec::new(), span)
            }
            _ => Err(Diagnostic::new(format!("`{type_name}` has nothing called `{name}`"), span)),
        }
    }

    pub(crate) fn index(&mut self, value: Value, at: Value, span: Span) -> Result<Value, Diagnostic> {
        match (&value, &at) {
            (Value::Type(_), _) => Ok(value),
            (Value::Ok(_) | Value::Err(_), _) => Err(unhandled_result(span)),
            (Value::None, _) => Err(might_be_none(span)),
            (Value::IdMap(map), Value::Id(id)) => map.get(*id).cloned().ok_or_else(|| {
                Diagnostic::new("this Id refers to something that's been removed", span)
                    .with_help("use `.get(id)`, which gives None instead")
            }),
            (Value::Dict(pairs), key) => {
                pairs.iter().find(|(existing, _)| existing == key).map(|(_, value)| value.clone()).ok_or_else(
                    || {
                        Diagnostic::new(format!("the dict has no key {}", crate::format::repr(key)), span)
                            .with_help("use `.get(key)`, which gives None instead")
                    },
                )
            }
            (Value::Instance(instance), _) => {
                let method = self
                    .type_info(&instance.type_name)
                    .and_then(|info| info.methods.get("[]").cloned())
                    .ok_or_else(|| {
                        Diagnostic::new(format!("`{}` can't be indexed", instance.type_name), span)
                            .with_help("define `def [](self, index: ...)` to index it")
                    })?;
                self.call_with_values(&method, vec![value.clone(), at], span)
            }
            _ => ops::index(&value, &at, span),
        }
    }

    /// The items a `for` loop, comprehension or builtin walks through.
    pub(crate) fn iterate(&mut self, value: Value, span: Span) -> Result<Vec<Value>, Diagnostic> {
        Ok(self.entries(value, span)?.into_iter().map(|(_, item)| item).collect())
    }

    /// Like `iterate`, with the key each item can be changed through, for `for mut`.
    pub(crate) fn entries(
        &mut self,
        value: Value,
        span: Span,
    ) -> Result<Vec<(Option<Value>, Value)>, Diagnostic> {
        let keyless = |items: &[Value]| items.iter().map(|item| (None, item.clone())).collect();
        Ok(match value {
            Value::List(items) => {
                items.iter().zip(0_i64..).map(|(item, at)| (Some(Value::Int(at)), item.clone())).collect()
            }
            Value::Tuple(items) | Value::Set(items) => keyless(&items),
            Value::Dict(pairs) => pairs.iter().map(|(key, _)| (None, key.clone())).collect(),
            Value::Str(text) => {
                text.chars().map(|c| (None, Value::str(c.encode_utf8(&mut [0; 4])))).collect()
            }
            Value::IdMap(map) => {
                map.entries().map(|(id, item)| (Some(Value::Id(id)), item.clone())).collect()
            }
            Value::Instance(instance) => {
                let method = self
                    .type_info(&instance.type_name)
                    .and_then(|info| info.methods.get("iter").cloned())
                    .ok_or_else(|| {
                        Diagnostic::new(format!("can't loop over a `{}`", instance.type_name), span)
                            .with_help("define `def iter(self)` that yields its items")
                    })?;
                let items =
                    self.call_with_values(&method, vec![Value::Instance(Rc::clone(&instance))], span)?;
                return self
                    .entries(items, span)
                    .map(|entries| entries.into_iter().map(|(_, item)| (None, item)).collect());
            }
            Value::Ok(_) | Value::Err(_) => return Err(unhandled_result(span)),
            Value::None => return Err(might_be_none(span)),
            other => return Err(Diagnostic::new(format!("can't loop over a {}", other.type_name()), span)),
        })
    }
}

pub(crate) fn might_be_none(span: Span) -> Diagnostic {
    Diagnostic::new("this might be None", span)
        .with_help("check with `if x is None:` first, use `?.`, or give a default with `else`")
}
