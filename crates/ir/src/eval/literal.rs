//! A worked-out constant's value written back as a literal, which the program holds instead of
//! the code that made it.

use std::collections::HashMap;

use fennec_syntax::{Arg, Expr, ExprKind, Span, UnaryOp};

use super::value::{Instance, Value};
use crate::Program;

/// Where the literals point: each unique, so the typed tree tells them apart, and in the
/// prelude's range, past its own text and short of what `lend_lowering` copies, as code the
/// compiler writes is. A check of the program then holds them to nothing a person writes, as a
/// case named in full where its type is known, which is how a literal names every enum value.
const BAKED_SPANS: u32 = Span::PRELUDE + (1 << 29);

/// What writing literals needs: the program's own types, which it can name to build one, and
/// each one's field names.
pub struct Written<'a> {
    own: &'a [&'a str],
    fields: HashMap<String, Vec<String>>,
    next: u32,
}

impl<'a> Written<'a> {
    /// `own` names the module's own structs and enums.
    pub fn new(program: &Program, own: &'a [&'a str]) -> Self {
        let mut fields = HashMap::new();
        for def in &program.structs {
            fields.insert(def.name.clone(), def.fields.iter().map(|(name, _)| name.clone()).collect());
        }
        for def in &program.enums {
            for case in &def.cases {
                let names = case.fields.iter().map(|(name, _)| name.clone()).collect();
                fields.insert(format!("{}.{}", def.name, case.name), names);
            }
        }
        Written { own, fields, next: BAKED_SPANS }
    }

    fn expr(&mut self, kind: ExprKind) -> Expr {
        let span = Span { start: self.next, end: self.next + 1 };
        self.next += 2;
        Expr { kind, span }
    }

    fn all<'v>(&mut self, items: impl IntoIterator<Item = &'v Value>) -> Option<Vec<Expr>> {
        items.into_iter().map(|item| literal(item, self)).collect()
    }

    /// A struct or an enum case built by name, or a `Duration` as its literal. An error, a type
    /// the program can't name, and a struct with `_` fields, which only its own methods can
    /// set, have no literal.
    fn instance(&mut self, instance: &Instance) -> Option<Expr> {
        let type_name = instance.type_name.as_ref();
        if type_name == "Duration"
            && let [Value::Int(nanos)] = instance.fields.as_slice()
        {
            return Some(self.expr(ExprKind::Duration(*nanos)));
        }
        if !self.own.contains(&type_name) {
            return None;
        }
        let key = match &instance.case {
            Some(case) => format!("{type_name}.{case}"),
            None => type_name.to_owned(),
        };
        let names = self.fields.get(&key)?.clone();
        if names.len() != instance.fields.len() {
            return None;
        }
        let mut args = Vec::new();
        for (name, value) in names.into_iter().zip(&instance.fields) {
            if name.starts_with('_') {
                return None;
            }
            args.push(Arg { name: Some(name), is_mut: false, value: literal(value, self)? });
        }
        let owner = self.expr(ExprKind::Name(type_name.to_owned()));
        let callee = match &instance.case {
            Some(case) => {
                let attr =
                    ExprKind::Attr { object: Box::new(owner), name: case.to_string(), optional: false };
                if args.is_empty() {
                    return Some(self.expr(attr));
                }
                self.expr(attr)
            }
            None => owner,
        };
        Some(self.expr(ExprKind::Call { callee: Box::new(callee), args }))
    }
}

/// A value as a literal, or nothing when it has none: an empty collection, whose items' type a
/// literal can't say, a float that isn't finite, a result, a closure, or a value of a type the
/// program can't name or build.
pub fn literal(value: &Value, written: &mut Written<'_>) -> Option<Expr> {
    let kind = match value {
        Value::None => ExprKind::None,
        Value::Bool(value) => ExprKind::Bool(*value),
        Value::Int(value) if *value < 0 => {
            let inner = written.expr(ExprKind::Int(value.checked_neg()?));
            ExprKind::Unary { op: UnaryOp::Neg, expr: Box::new(inner) }
        }
        Value::Int(value) => ExprKind::Int(*value),
        Value::Float(value) if !value.is_finite() => return None,
        Value::Float(value) if value.is_sign_negative() => {
            let inner = written.expr(ExprKind::Float(-value));
            ExprKind::Unary { op: UnaryOp::Neg, expr: Box::new(inner) }
        }
        Value::Float(value) => ExprKind::Float(*value),
        Value::Str(text) => ExprKind::Str(text.to_string()),
        Value::Bytes(data) => ExprKind::Bytes(data.to_vec().into()),
        Value::List(items) if !items.is_empty() => ExprKind::List(written.all(items.iter())?),
        Value::Tuple(items) => ExprKind::Tuple(written.all(items.iter())?),
        Value::Set(items) if !items.is_empty() => ExprKind::Set(written.all(items.keys())?),
        Value::Dict(pairs) if !pairs.is_empty() => {
            let mut all = Vec::new();
            for (key, value) in &pairs.pairs {
                all.push((literal(key, written)?, literal(value, written)?));
            }
            ExprKind::Dict(all)
        }
        Value::Member(_, inner) => return literal(inner, written),
        Value::Instance(instance) => return written.instance(instance),
        _ => return None,
    };
    Some(written.expr(kind))
}
