//! Expressions: literals, names, operators, `match`, comprehensions, and the `else` and
//! `except` that handle a missing value or a failure.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use fennec_check::{Num, Type};
use fennec_syntax::{
    BinOp, CompKind, Expr, ExprKind, FStrPart, MatchArm, Pattern, Span, Stmt, StmtKind, UnaryOp,
};

use super::{Flow, Interp, MOST_ITEMS, Run, crash, position, unsupported};
use crate::load::Kind;
use crate::text::{self, Number};
use crate::value::{Func, Value};

impl Interp {
    #[expect(clippy::too_many_lines, reason = "one arm for each kind of expression")]
    pub(crate) fn eval(&mut self, expr: &'static Expr) -> Run<Value> {
        self.tick()?;
        match &expr.kind {
            ExprKind::Int(n) => self.int_literal(*n, expr.span),
            ExprKind::Float(x) => match self.program.type_at(expr.span) {
                Some(Type::Sized(_)) => unsupported("f32"),
                _ => Ok(Value::Float(*x)),
            },
            ExprKind::Duration(_) => unsupported("durations"),
            ExprKind::Bytes(_) | ExprKind::Folder(_) => unsupported("embedded files"),
            ExprKind::Str(text) => Ok(Value::Str(text.clone())),
            ExprKind::FStr(parts) => self.fstring(parts),
            ExprKind::Bool(truth) => Ok(Value::Bool(*truth)),
            ExprKind::None => Ok(Value::None),
            ExprKind::Name(name) => self.name_value(name, expr.span),
            ExprKind::Variant(case) => self.construct_case_at(case, expr, Vec::new()),
            ExprKind::List(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval(item)?);
                }
                Ok(Value::List(values))
            }
            ExprKind::Tuple(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval(item)?);
                }
                Ok(Value::Tuple(values))
            }
            ExprKind::Dict(pairs) => {
                let mut dict = Vec::with_capacity(pairs.len());
                for (key, value) in pairs {
                    let key = self.eval(key)?;
                    let value = self.eval(value)?;
                    dict_insert(&mut dict, key, value)?;
                }
                Ok(Value::Dict(dict))
            }
            ExprKind::Set(items) => {
                let mut set = Vec::with_capacity(items.len());
                for item in items {
                    let item = self.eval(item)?;
                    set_insert(&mut set, item)?;
                }
                Ok(Value::Set(set))
            }
            ExprKind::Comprehension { kind: CompKind::Generator, .. } => self.generator_of(expr),
            ExprKind::Comprehension { kind, elem, value, names, iter, cond } => {
                self.comprehension(*kind, (elem, value.as_deref()), names, iter, cond.as_deref())
            }
            ExprKind::Unary { op, expr: inner } => {
                let value = self.eval(inner)?;
                unary(*op, value, expr.span)
            }
            ExprKind::Binary { op: BinOp::And, lhs, rhs } => {
                if self.truth(lhs)? {
                    self.eval(rhs)
                } else {
                    Ok(Value::Bool(false))
                }
            }
            ExprKind::Binary { op: BinOp::Or, lhs, rhs } => {
                if self.truth(lhs)? {
                    Ok(Value::Bool(true))
                } else {
                    self.eval(rhs)
                }
            }
            ExprKind::Binary { op: op @ (BinOp::Is | BinOp::IsNot), lhs, rhs } => {
                let left = self.eval(lhs)?;
                let is = self.is_test(&left, rhs)?;
                Ok(Value::Bool(is == (*op == BinOp::Is)))
            }
            ExprKind::Binary { op: op @ (BinOp::In | BinOp::NotIn), lhs, rhs } => {
                let left = self.eval(lhs)?;
                if !matches!(left, Value::Instance(_))
                    && let Some(place) = self.place_of(rhs)?
                {
                    let found = self.peek_place(&place, expr.span, |held| plain_contains(held, &left))??;
                    let found = if let Some(found) = found {
                        found
                    } else {
                        let right = self.read_place(&place, expr.span)?;
                        self.contains(&right, &left)?
                    };
                    return Ok(Value::Bool(found == (*op == BinOp::In)));
                }
                let right = self.eval(rhs)?;
                self.binary_values(*op, left, right, expr.span)
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let left = self.eval(lhs)?;
                let right = self.eval(rhs)?;
                self.binary_values(*op, left, right, expr.span)
            }
            ExprKind::Cond { then, cond, otherwise } => {
                if self.truth(cond)? {
                    self.eval(then)
                } else {
                    self.eval(otherwise)
                }
            }
            ExprKind::Else { value, default, except } => self.else_expr(value, default, *except),
            ExprKind::Try { value, context: None } => self.eval(value),
            ExprKind::Try { context: Some(_), .. } => unsupported("`? from`"),
            ExprKind::Call { callee, args } => self.call(expr, callee, args),
            ExprKind::Attr { object, name, optional } => {
                if *optional {
                    return unsupported("`?.`");
                }
                self.attr(object, name, expr.span)
            }
            ExprKind::Index { object, index } => {
                if let Some(place) = self.place_of(object)? {
                    let at = self.eval(index)?;
                    let own =
                        self.peek_place(&place, expr.span, |held| matches!(held, Value::Instance(_)))?;
                    if !own {
                        return self.read_place(&place.then(super::Step::At(at)), expr.span);
                    }
                    let object_value = self.read_place(&place, expr.span)?;
                    return self.index_value(object_value, at, expr.span);
                }
                let object_value = self.eval(object)?;
                let at = self.eval(index)?;
                self.index_value(object_value, at, expr.span)
            }
            ExprKind::Slice { object, start, end } => {
                let object = self.eval(object)?;
                let start = match start {
                    Some(start) => Some(self.eval(start)?),
                    None => None,
                };
                let end = match end {
                    Some(end) => Some(self.eval(end)?),
                    None => None,
                };
                slice(object, start, end)
            }
            ExprKind::Lambda { params, body } => {
                let captured = self.captured(&[body], params)?;
                Ok(Value::Func(Box::new(Func::Lambda { params: params.clone(), body, captured })))
            }
            ExprKind::Match { subject, arms } => {
                let subject = self.eval(subject)?;
                self.match_value(&subject, arms)
            }
        }
    }

    /// An int written out, which takes the type it goes into: an `int`, or another size.
    fn int_literal(&self, n: i64, span: Span) -> Run<Value> {
        let mut ty = self.program.type_at(span);
        while let Some(Type::Optional(inner)) = ty {
            ty = Some(inner);
        }
        match ty {
            Some(Type::Sized(num)) if num.is_float() => unsupported("f32"),
            Some(Type::Sized(num)) => sized(*num, i128::from(n), span),
            Some(Type::Float) => Ok(Value::Float(text::int_as_float(n))),
            _ => Ok(Value::Int(n)),
        }
    }

    /// What a name means where it's read: a local, a constant, a function, or a builtin.
    pub(crate) fn name_value(&mut self, name: &str, span: Span) -> Run<Value> {
        if let Some(value) = self.lookup(name, span)? {
            return Ok(value);
        }
        if let Some(value) = self.constant(name)? {
            return Ok(value);
        }
        if self.program.functions.contains_key(name) {
            return Ok(Value::Func(Box::new(Func::Named(name.to_owned()))));
        }
        if matches!(name, "len" | "str" | "abs" | "int" | "float") {
            return Ok(Value::Func(Box::new(Func::Builtin(name.to_owned()))));
        }
        if let Some(from) = self.program.imported.get(name) {
            return Self::library_value(from);
        }
        unsupported(format!("the name `{name}`"))
    }

    /// A name from the standard library, as a value: a constant, or a function the oracle models.
    fn library_value(path: &str) -> Run<Value> {
        if let Some(value) = super::library::constant(path) {
            return Ok(value);
        }
        if super::library::has_function(path) {
            return Ok(Value::Func(Box::new(Func::Builtin(path.to_owned()))));
        }
        unsupported(path.to_owned())
    }

    fn fstring(&mut self, parts: &'static [FStrPart]) -> Run<Value> {
        let mut out = String::new();
        for part in parts {
            match part {
                FStrPart::Text(text) => out.push_str(text),
                FStrPart::Expr { expr, spec } => {
                    let value = self.eval(expr)?;
                    match spec.as_deref() {
                        None | Some("") => out.push_str(&self.display(&value)?),
                        Some(spec) => {
                            let number = match value {
                                Value::Int(n) => Number::Int(n),
                                Value::Float(x) => Number::Float(x),
                                Value::Sized(num, n) if !num.is_float() => match i64::try_from(n) {
                                    Ok(n) => Number::Int(n),
                                    Err(_) => {
                                        return unsupported("a format spec on a u64 past the largest int");
                                    }
                                },
                                other => {
                                    return unsupported(format!(
                                        "the format spec `{spec}` on a {}",
                                        other.kind()
                                    ));
                                }
                            };
                            match text::with_spec(&number, spec) {
                                Some(shown) => out.push_str(&shown),
                                None => return unsupported(format!("the format spec `{spec}`")),
                            }
                        }
                    }
                }
            }
        }
        Ok(Value::Str(out))
    }

    /// `value else default` and `value except ...`: the default for a missing value, or the
    /// handler for a failure of the call `value` is.
    fn else_expr(&mut self, value: &'static Expr, default: &'static Expr, except: bool) -> Run<Value> {
        if !except {
            return match self.chain(value)? {
                None | Some(Value::None) => self.default_of(default, None),
                Some(found) => Ok(found),
            };
        }
        let ran = match &value.kind {
            ExprKind::Call { callee, args } => {
                let (target, given) = self.prepare(value, callee, args)?;
                self.invoke(target, given, value)
            }
            _ => self.eval(value),
        };
        match ran {
            Err(Flow::Fail(error, _)) => self.default_of(default, Some(error)),
            other => other,
        }
    }

    /// The value of a chain an `else` ends, or `None` at its first step that's missing or that
    /// fails: `find(name).level else 0` gives 0 when `find` fails, and when it gives `None`.
    fn chain(&mut self, expr: &'static Expr) -> Run<Option<Value>> {
        match &expr.kind {
            ExprKind::Attr { object, name, optional: false } if !rooted_in_a_name(object) => {
                let Some(held) = self.chain(object)? else { return Ok(None) };
                match held {
                    Value::None => Ok(None),
                    Value::Instance(instance) => match instance.field(name) {
                        Some(field) => Ok(Some(field.clone())),
                        None => unsupported(format!("the field `{name}` of {}", instance.ty)),
                    },
                    other => unsupported(format!("the field `{name}` of a {}", other.kind())),
                }
            }
            ExprKind::Call { callee, args } => {
                if let ExprKind::Attr { object, name, optional: false } = &callee.kind
                    && !rooted_in_a_name(object)
                    && name != "expect"
                {
                    let Some(held) = self.chain(object)? else { return Ok(None) };
                    if matches!(held, Value::None) {
                        return Ok(None);
                    }
                    let receiver = super::calls::Receiver::Value(held);
                    let Some(target) = self.method_target(receiver, name, object.span)? else {
                        return Ok(None);
                    };
                    let mut given = Vec::with_capacity(args.len());
                    for arg in args {
                        given.push(self.arg_value(arg, false)?);
                    }
                    return match self.invoke(target, given, expr) {
                        Err(Flow::Fail(..)) => Ok(None),
                        other => other.map(Some),
                    };
                }
                let (target, given) = self.prepare(expr, callee, args)?;
                match self.invoke(target, given, expr) {
                    Err(Flow::Fail(..)) => Ok(None),
                    other => other.map(Some),
                }
            }
            _ => self.eval(expr).map(Some),
        }
    }

    /// What an `else` or `except` gives instead: a plain value, or a block that leaves or ends
    /// with its value, with the failure named for it.
    fn default_of(&mut self, default: &'static Expr, failure: Option<Value>) -> Run<Value> {
        if let Some((name, body)) = default.else_block() {
            let mark = self.mark();
            let value = match (name, failure) {
                (Some(name), Some(failure)) => self.bind(name, failure).and_then(|()| self.block_value(body)),
                _ => self.block_value(body),
            };
            self.restore(mark);
            return value;
        }
        self.eval(default)
    }

    // Operators.

    /// `left op right` on two values already worked out.
    pub(crate) fn binary_values(&mut self, op: BinOp, left: Value, right: Value, span: Span) -> Run<Value> {
        match op {
            BinOp::Eq => self.equal(&left, &right).map(Value::Bool),
            BinOp::NotEq => self.equal(&left, &right).map(|same| Value::Bool(!same)),
            BinOp::Lt => self.compare(&left, &right).map(|order| Value::Bool(order == Ordering::Less)),
            BinOp::LtEq => self.compare(&left, &right).map(|order| Value::Bool(order != Ordering::Greater)),
            BinOp::Gt => self.compare(&left, &right).map(|order| Value::Bool(order == Ordering::Greater)),
            BinOp::GtEq => self.compare(&left, &right).map(|order| Value::Bool(order != Ordering::Less)),
            BinOp::In => self.contains(&right, &left).map(Value::Bool),
            BinOp::NotIn => self.contains(&right, &left).map(|found| Value::Bool(!found)),
            BinOp::And | BinOp::Or | BinOp::Is | BinOp::IsNot => unsupported("a logical operator on values"),
            _ => self.arithmetic(op, left, right, span),
        }
    }

    fn arithmetic(&mut self, op: BinOp, left: Value, right: Value, span: Span) -> Run<Value> {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => int_arithmetic(op, a, b, span),
            (Value::Sized(num, a), Value::Sized(other, b)) if num == other => {
                sized_arithmetic(op, num, a, b, span)
            }
            (Value::Float(a), Value::Float(b)) => float_arithmetic(op, a, b, span),
            (Value::Str(a), Value::Str(b)) if op == BinOp::Add => Ok(Value::Str(a + &b)),
            (Value::Str(a), Value::Int(times)) if op == BinOp::Mul => {
                let times = usize::try_from(times).unwrap_or(0);
                if a.len().saturating_mul(times) > MOST_ITEMS {
                    return unsupported("ran too long: a string too long");
                }
                Ok(Value::Str(a.repeat(times)))
            }
            (Value::List(mut a), Value::List(b)) if op == BinOp::Add => {
                a.extend(b);
                Ok(Value::List(a))
            }
            (Value::Instance(instance), right) => {
                let symbol = op.symbol();
                match self.program.method(&instance.ty, symbol) {
                    Some(def) => {
                        let receiver = super::calls::Receiver::Value(Value::Instance(instance));
                        self.call_def(
                            def,
                            None,
                            Some(receiver),
                            vec![super::calls::ArgVal::Value(None, right)],
                            span,
                        )
                    }
                    None if op == BinOp::Div && instance.ty == "Path" && self.is_std_type("Path") => {
                        match instance.field("text") {
                            Some(Value::Str(text)) => super::paths::method(text, "/", &[right], span),
                            _ => unsupported("a Path without its text"),
                        }
                    }
                    None => unsupported(format!("`{symbol}` on a {}", instance.ty)),
                }
            }
            (left, right) => {
                unsupported(format!("`{}` on a {} and a {}", op.symbol(), left.kind(), right.kind()))
            }
        }
    }

    /// `value is T`: whether it's `None`, or holds a value of type `T`.
    fn is_test(&mut self, value: &Value, ty: &'static Expr) -> Run<bool> {
        match &ty.kind {
            ExprKind::None => Ok(matches!(value, Value::None)),
            ExprKind::Name(name) => self.has_type(value, name),
            _ => unsupported("`is` with a type in brackets"),
        }
    }

    pub(crate) fn has_type(&self, value: &Value, name: &str) -> Run<bool> {
        if let Value::Unknown(why) = value {
            return unsupported(why.clone());
        }
        Ok(match (name, value) {
            ("int", Value::Int(_))
            | ("float", Value::Float(_))
            | ("str", Value::Str(_))
            | ("bool", Value::Bool(_))
            | ("list", Value::List(_))
            | ("dict", Value::Dict(_))
            | ("set", Value::Set(_)) => true,
            (_, Value::Sized(num, _)) => num.name() == name,
            (_, Value::Instance(instance)) => {
                if let Some(members) = self.program.aliases.get(name) {
                    let mut any = false;
                    for member in members {
                        any = any || self.has_type(value, member)?;
                    }
                    any
                } else {
                    instance.ty == name
                }
            }
            ("int" | "float" | "str" | "bool" | "list" | "dict" | "set", _) => false,
            _ if self.program.types.contains_key(name) => false,
            _ => return unsupported(format!("`is {name}`")),
        })
    }

    /// `item in container`.
    pub(crate) fn contains(&mut self, container: &Value, item: &Value) -> Run<bool> {
        match container {
            Value::List(items) | Value::Tuple(items) | Value::Set(items) => {
                for held in items {
                    if self.equal(held, item)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Value::Dict(pairs) => Ok(super::find_key(pairs, item)?.is_some()),
            Value::Table(table) => match item {
                Value::Id(id) => Ok(table.find(*id).is_some()),
                other => unsupported(format!("looking for a {} in a Table", other.kind())),
            },
            Value::Str(text) => match item {
                Value::Str(part) => Ok(text.contains(part.as_str())),
                other => unsupported(format!("looking for a {} in a str", other.kind())),
            },
            Value::Instance(instance) => match self.program.method(&instance.ty, "in") {
                Some(def) => {
                    let receiver = super::calls::Receiver::Value(container.clone());
                    let given = vec![super::calls::ArgVal::Value(None, item.clone())];
                    match self.call_def(def, None, Some(receiver), given, def.span)? {
                        Value::Bool(found) => Ok(found),
                        _ => unsupported("a `def in` that doesn't give a bool"),
                    }
                }
                None => unsupported(format!("`in` on a {}", instance.ty)),
            },
            other => unsupported(format!("`in` on a {}", other.kind())),
        }
    }

    // Reading parts.

    fn attr(&mut self, object: &'static Expr, name: &str, span: Span) -> Run<Value> {
        if let ExprKind::Name(owner) = &object.kind
            && !self.is_bound(owner)
            && !self.program.constants.contains_key(owner.as_str())
        {
            let owner = &self.type_named(owner).into_owned();
            if let Some(declared) = self.program.types.get(owner.as_str()) {
                if declared.kind != Kind::Struct && declared.def.cases.iter().any(|case| case.name == name) {
                    return self.construct(owner, Some(name), Vec::new(), span);
                }
                // A type's function that takes nothing is read as a value, as `Vec3.zero` is.
                if let Some(def) = self.program.method(owner, name)
                    && def.params.is_empty()
                {
                    return self.call_def(def, None, None, Vec::new(), span);
                }
                return unsupported(format!("`{owner}.{name}`"));
            }
            if let Some(from) = self.program.imported.get(owner.as_str()) {
                return Self::library_value(&format!("{from}.{name}"));
            }
            return unsupported(format!("`{owner}.{name}`"));
        }
        if let Some(place) = self.place_of(object)? {
            let found = self.peek_place(&place, span, |held| match held {
                Value::None => Some(Value::None),
                Value::Instance(instance) => instance.field(name).cloned(),
                _ => None,
            })?;
            return match found {
                Some(found) => Ok(found),
                None => unsupported(format!("the field `{name}`")),
            };
        }
        let value = self.eval(object)?;
        match value {
            // A step along a chain an `else` ends, past a value that's missing.
            Value::None => Ok(Value::None),
            Value::Instance(instance) => match instance.field(name) {
                Some(field) => Ok(field.clone()),
                None => unsupported(format!("the field `{name}` of {}", instance.ty)),
            },
            other => unsupported(format!("the field `{name}` of a {}", other.kind())),
        }
    }

    pub(crate) fn index_value(&mut self, object: Value, at: Value, span: Span) -> Run<Value> {
        match (object, at) {
            (Value::List(items) | Value::Tuple(items), Value::Int(at)) => {
                let at = position(at, items.len(), span)?;
                items.into_iter().nth(at).map_or_else(|| crash("index out of range", span), Ok)
            }
            (Value::Str(text), Value::Int(at)) => {
                let count = text.chars().count();
                let at = position(at, count, span)?;
                Ok(Value::Str(text.chars().nth(at).map(String::from).unwrap_or_default()))
            }
            (Value::Dict(pairs), key) => match super::find_key(&pairs, &key)? {
                Some(at) => pairs
                    .into_iter()
                    .nth(at)
                    .map(|(_, value)| value)
                    .map_or_else(|| crash("no key", span), Ok),
                None => crash(format!("{} isn't a key here", self.repr(&key)?), span),
            },
            (Value::Instance(instance), key) => match self.program.method(&instance.ty, "[]") {
                Some(def) => {
                    let receiver = super::calls::Receiver::Value(Value::Instance(instance));
                    self.call_def(
                        def,
                        None,
                        Some(receiver),
                        vec![super::calls::ArgVal::Value(None, key)],
                        span,
                    )
                }
                None => unsupported(format!("`[]` on a {}", instance.ty)),
            },
            (Value::Cap(crate::value::Cap::Args), Value::Int(at)) => {
                position(at, 0, span)?;
                unsupported("an argument")
            }
            (Value::Table(mut table), Value::Id(id)) => {
                match table.find(id).and_then(|at| table.slots.get_mut(at)).and_then(|slot| slot.item.take())
                {
                    Some(item) => Ok(item),
                    None => crash(table.missing(id), span),
                }
            }
            (object, at) => unsupported(format!("indexing a {} with a {}", object.kind(), at.kind())),
        }
    }

    // Arrow functions and generators read the names around them as they were when made.

    /// Copies of the names `exprs` read that are bound here, leaving out `own`.
    pub(crate) fn captured(&self, exprs: &[&'static Expr], own: &[String]) -> Run<Vec<(String, Value)>> {
        let mut names = BTreeSet::new();
        for expr in exprs {
            names_in_expr(expr, &mut names);
        }
        let mut captured = Vec::new();
        for name in names {
            if own.contains(&name) {
                continue;
            }
            if let Some(value) = self.lookup(&name, Span::default())? {
                captured.push((name, value));
            }
        }
        Ok(captured)
    }

    // Comprehensions.

    fn comprehension(
        &mut self,
        kind: CompKind,
        (elem, value): (&'static Expr, Option<&'static Expr>),
        names: &'static [String],
        iter: &'static Expr,
        cond: Option<&'static Expr>,
    ) -> Run<Value> {
        let over = self.eval(iter)?;
        let mut items = self.items_of(over, iter.span)?;
        let mark = self.frame()?.vars.len();
        let mut made = Vec::new();
        let mut pairs = Vec::new();
        let ran = (|| {
            while let Some(item) = self.next_item(&mut items)? {
                self.bind_fresh(names, item)?;
                if let Some(cond) = cond
                    && !self.truth(cond)?
                {
                    continue;
                }
                let made_item = self.eval(elem)?;
                match (kind, value) {
                    (CompKind::Dict, Some(value)) => {
                        let made_value = self.eval(value)?;
                        dict_insert(&mut pairs, made_item, made_value)?;
                    }
                    (CompKind::Set, _) => set_insert(&mut made, made_item)?,
                    _ => made.push(made_item),
                }
                if made.len() + pairs.len() > MOST_ITEMS {
                    return unsupported("ran too long: a collection too big");
                }
            }
            Ok(())
        })();
        if let Ok(frame) = self.frame_mut() {
            frame.vars.truncate(mark);
        }
        ran?;
        Ok(match kind {
            CompKind::Dict => Value::Dict(pairs),
            CompKind::Set => Value::Set(made),
            CompKind::List | CompKind::Generator => Value::List(made),
        })
    }

    /// Gives a comprehension's names an item, as new names that hide any outside it.
    pub(crate) fn bind_fresh(&mut self, names: &[String], item: Value) -> Run<()> {
        if let [name] = names {
            return self.bind(name, item);
        }
        let parts = match item {
            Value::Tuple(parts) | Value::List(parts) if parts.len() == names.len() => parts,
            other => return unsupported(format!("taking a {} apart", other.kind())),
        };
        for (name, part) in names.iter().zip(parts) {
            self.bind(name, part)?;
        }
        Ok(())
    }

    // Matching.

    /// Runs the first arm whose pattern fits and whose guard holds, giving its block's value.
    pub(crate) fn match_value(&mut self, subject: &Value, arms: &'static [MatchArm]) -> Run<Value> {
        for arm in arms {
            let mut bound = Vec::new();
            if !self.fits(&arm.pattern, subject, &mut bound)? {
                continue;
            }
            let mark = self.mark();
            let ran = self.arm(bound, arm);
            self.restore(mark);
            if let Some(value) = ran? {
                return Ok(value);
            }
        }
        unsupported("a `match` no case fits")
    }

    /// An arm whose pattern fits: its value, or `None` when its guard doesn't hold.
    fn arm(&mut self, bound: Vec<(String, Value)>, arm: &'static MatchArm) -> Run<Option<Value>> {
        for (name, value) in bound {
            self.bind(&name, value)?;
        }
        if let Some(guard) = &arm.guard
            && !self.truth(guard)?
        {
            return Ok(None);
        }
        self.block_value(&arm.body).map(Some)
    }

    /// Whether a value fits a pattern, with the names it binds.
    fn fits(
        &mut self,
        pattern: &'static Pattern,
        value: &Value,
        bound: &mut Vec<(String, Value)>,
    ) -> Run<bool> {
        match pattern {
            Pattern::Wildcard => Ok(true),
            Pattern::Bind(name) => {
                bound.push((name.clone(), value.clone()));
                Ok(true)
            }
            Pattern::RefBind(_) => unsupported("`case` with `ref`"),
            Pattern::Literal(expr) => {
                let literal = self.eval(expr)?;
                if matches!(literal, Value::None) {
                    return Ok(matches!(value, Value::None));
                }
                if matches!(value, Value::None) {
                    return Ok(false);
                }
                self.equal(&literal, value)
            }
            Pattern::Tuple(parts) => match value {
                Value::Tuple(items) if items.len() == parts.len() => {
                    for (part, item) in parts.iter().zip(items) {
                        if !self.fits(part, item, bound)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                }
                _ => Ok(false),
            },
            Pattern::Or(choices) => {
                for choice in choices {
                    let mut tried = Vec::new();
                    if self.fits(choice, value, &mut tried)? {
                        bound.extend(tried);
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Pattern::Variant { name, args, fields, .. } => {
                let Value::Instance(instance) = value else {
                    return match value {
                        Value::None => Ok(false),
                        _ if self.has_type(value, name)? => Ok(args.is_empty()),
                        _ => Ok(false),
                    };
                };
                let fits_case = instance.case.as_deref() == Some(name.as_str());
                let fits_type = instance.ty == *name;
                if !fits_case && !fits_type {
                    return Ok(false);
                }
                if fits_type && instance.case.is_some() && !fits_case {
                    // A type-wide pattern, as `IoError()`, fits every case.
                    return if args.is_empty() {
                        Ok(true)
                    } else {
                        unsupported("a type-wide pattern with fields")
                    };
                }
                for (at, part) in args.iter().enumerate() {
                    let field = match fields.get(at) {
                        Some(field_name) => instance.field(field_name),
                        None => instance.fields.get(at).map(|(_, value)| value),
                    };
                    let Some(field) = field else {
                        return unsupported("a pattern with more fields than its case");
                    };
                    if !self.fits(part, field, bound)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
        }
    }
}

/// Whether an expression is a name, or fields and positions of one, with no call along it.
fn rooted_in_a_name(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Name(_) => true,
        ExprKind::Attr { object, .. } | ExprKind::Index { object, .. } => rooted_in_a_name(object),
        _ => false,
    }
}

/// A sized integer, which has to fit its size.
pub(crate) fn sized(num: Num, n: i128, span: Span) -> Run<Value> {
    let Some((least, most)) = num.range() else { return unsupported("f32") };
    if (least..=most).contains(&n) {
        Ok(Value::Sized(num, n))
    } else {
        crash(format!("this calculation overflows a {}", num.name()), span)
    }
}

fn overflow<T>(span: Span) -> Run<T> {
    crash("this calculation overflows an int", span)
}

fn unary(op: UnaryOp, value: Value, span: Span) -> Run<Value> {
    match (op, value) {
        (UnaryOp::Neg, Value::Int(n)) => {
            n.checked_neg().map_or_else(|| overflow(span), |n| Ok(Value::Int(n)))
        }
        (UnaryOp::Neg, Value::Float(x)) => Ok(Value::Float(-x)),
        (UnaryOp::Neg, Value::Sized(num, n)) => sized(num, -n, span),
        (UnaryOp::Not, Value::Bool(truth)) => Ok(Value::Bool(!truth)),
        (UnaryOp::BitNot, Value::Int(n)) => Ok(Value::Int(!n)),
        (UnaryOp::BitNot, Value::Sized(num, n)) => {
            let Some((_, most)) = num.range() else { return unsupported("f32") };
            if num.is_unsigned() { Ok(Value::Sized(num, most - n)) } else { Ok(Value::Sized(num, !n)) }
        }
        (op, value) => unsupported(format!("{op:?} on a {}", value.kind())),
    }
}

fn int_arithmetic(op: BinOp, a: i64, b: i64, span: Span) -> Run<Value> {
    if b == 0 && matches!(op, BinOp::Div | BinOp::FloorDiv | BinOp::Mod) {
        return crash("division by zero", span);
    }
    let result = match op {
        BinOp::BitAnd => return Ok(Value::Int(a & b)),
        BinOp::BitOr => return Ok(Value::Int(a | b)),
        BinOp::BitXor => return Ok(Value::Int(a ^ b)),
        BinOp::ShiftLeft | BinOp::ShiftRight => {
            let Ok(count) = u32::try_from(b) else {
                return crash("can't shift by a count outside 0 to 63", span);
            };
            if count >= 64 {
                return crash("can't shift by a count outside 0 to 63", span);
            }
            return Ok(Value::Int(if op == BinOp::ShiftLeft { a.wrapping_shl(count) } else { a >> count }));
        }
        BinOp::Add => a.checked_add(b),
        BinOp::Sub => a.checked_sub(b),
        BinOp::Mul => a.checked_mul(b),
        BinOp::Div => return Ok(Value::Float(text::int_as_float(a) / text::int_as_float(b))),
        BinOp::FloorDiv => {
            a.checked_div_euclid(b).map(|q| if b < 0 && a.rem_euclid(b) != 0 { q - 1 } else { q })
        }
        BinOp::Mod => a.checked_rem_euclid(b).map(|r| if b < 0 && r != 0 { r + b } else { r }),
        BinOp::Pow => {
            let Ok(exponent) = u32::try_from(b) else {
                return crash("an int can only be raised to a non-negative power", span);
            };
            a.checked_pow(exponent)
        }
        _ => return unsupported(format!("`{}` on ints", op.symbol())),
    };
    result.map_or_else(|| overflow(span), |n| Ok(Value::Int(n)))
}

fn sized_arithmetic(op: BinOp, num: Num, a: i128, b: i128, span: Span) -> Run<Value> {
    if num.is_float() {
        return unsupported("f32");
    }
    if b == 0 && matches!(op, BinOp::Div | BinOp::FloorDiv | BinOp::Mod) {
        return crash("division by zero", span);
    }
    let bits = num.bits();
    let wrap = |n: i128| -> Value {
        let modulus = 1_i128 << bits;
        let mut wrapped = n.rem_euclid(modulus);
        if !num.is_unsigned() && wrapped >= modulus / 2 {
            wrapped -= modulus;
        }
        Value::Sized(num, wrapped)
    };
    match op {
        BinOp::Add => sized(num, a + b, span),
        BinOp::Sub => sized(num, a - b, span),
        BinOp::Mul => a.checked_mul(b).map_or_else(|| overflow(span), |n| sized(num, n, span)),
        BinOp::Div => Ok(Value::Float(int128_as_float(a) / int128_as_float(b))),
        BinOp::FloorDiv => sized(num, a.div_euclid(b) - i128::from(b < 0 && a.rem_euclid(b) != 0), span),
        BinOp::Mod => {
            let r = a.rem_euclid(b);
            sized(num, if b < 0 && r != 0 { r + b } else { r }, span)
        }
        BinOp::Pow => {
            let Ok(exponent) = u32::try_from(b) else {
                return crash("a number can only be raised to a non-negative power", span);
            };
            a.checked_pow(exponent).map_or_else(|| overflow(span), |n| sized(num, n, span))
        }
        BinOp::BitAnd => Ok(Value::Sized(num, a & b)),
        BinOp::BitOr => Ok(Value::Sized(num, a | b)),
        BinOp::BitXor => Ok(wrap(a ^ b)),
        BinOp::ShiftLeft | BinOp::ShiftRight => {
            let count = u32::try_from(b).ok().filter(|count| *count < bits);
            let Some(count) = count else {
                return crash("can't shift by a count past the type's width", span);
            };
            if op == BinOp::ShiftLeft { Ok(wrap(a << count)) } else { Ok(Value::Sized(num, a >> count)) }
        }
        _ => unsupported(format!("`{}` on {}", op.symbol(), num.name())),
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a sized number becomes the nearest float, as the language says"
)]
pub(crate) fn int128_as_float(n: i128) -> f64 {
    n as f64
}

fn float_arithmetic(op: BinOp, a: f64, b: f64, span: Span) -> Run<Value> {
    if b == 0.0 && matches!(op, BinOp::Div | BinOp::FloorDiv | BinOp::Mod) {
        return crash("division by zero", span);
    }
    Ok(Value::Float(match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div => a / b,
        BinOp::FloorDiv => (a / b).floor(),
        BinOp::Mod => a - b * (a / b).floor(),
        BinOp::Pow => float_power(a, b)?,
        _ => return unsupported(format!("`{}` on floats", op.symbol())),
    }))
}

/// `a ** b` on floats, for the powers the runtime works out without `libm`: any other one is
/// `libm`'s, which the oracle doesn't have.
fn float_power(a: f64, b: f64) -> Run<f64> {
    if b.to_bits() == 2.0_f64.to_bits() {
        Ok(a * a)
    } else if b.to_bits() == 0.5_f64.to_bits() {
        Ok(if a == 0.0 {
            0.0
        } else if a == f64::NEG_INFINITY {
            f64::INFINITY
        } else {
            a.sqrt()
        })
    } else if b.to_bits() == 1.0_f64.to_bits() {
        Ok(a)
    } else if b == 0.0 {
        Ok(1.0)
    } else if b.to_bits() == (-1.0_f64).to_bits() {
        Ok(1.0 / a)
    } else {
        unsupported("`**` on floats, which libm works out")
    }
}

/// `object[start:end]` on a list, a tuple or a string, each end counting from the end when it's
/// negative and held to the ends, as in Python.
fn slice(object: Value, start: Option<Value>, end: Option<Value>) -> Run<Value> {
    let bound = |value: Option<Value>, len: usize, default: usize| -> Run<usize> {
        match value {
            None => Ok(default),
            Some(Value::Int(at)) => {
                let signed = super::as_int(len);
                let resolved = if at < 0 { (at + signed).max(0) } else { at.min(signed) };
                Ok(usize::try_from(resolved).unwrap_or(0))
            }
            Some(other) => unsupported(format!("slicing with a {}", other.kind())),
        }
    };
    match object {
        Value::List(items) => {
            let from = bound(start, items.len(), 0)?;
            let to = bound(end, items.len(), items.len())?;
            Ok(Value::List(items.into_iter().skip(from).take(to.saturating_sub(from)).collect()))
        }
        Value::Tuple(items) => {
            let from = bound(start, items.len(), 0)?;
            let to = bound(end, items.len(), items.len())?;
            Ok(Value::Tuple(items.into_iter().skip(from).take(to.saturating_sub(from)).collect()))
        }
        Value::Str(text) => {
            let count = text.chars().count();
            let from = bound(start, count, 0)?;
            let to = bound(end, count, count)?;
            Ok(Value::Str(text.chars().skip(from).take(to.saturating_sub(from)).collect()))
        }
        other => unsupported(format!("slicing a {}", other.kind())),
    }
}

/// `item in container` with no type's own `==`, or `None` for a container that needs one.
fn plain_contains(container: &Value, item: &Value) -> Run<Option<bool>> {
    Ok(Some(match (container, item) {
        (Value::Dict(pairs), key) => super::find_key(pairs, key)?.is_some(),
        (Value::Table(table), Value::Id(id)) => table.find(*id).is_some(),
        (Value::Str(text), Value::Str(part)) => text.contains(part.as_str()),
        (Value::List(items) | Value::Tuple(items) | Value::Set(items), item) => {
            let mut found = false;
            for held in items {
                if matches!(held, Value::Instance(_)) {
                    return Ok(None);
                }
                if super::show::plain_equal(held, item)? {
                    found = true;
                    break;
                }
            }
            found
        }
        _ => return Ok(None),
    }))
}

/// Adds a pair to a dict, replacing the value of a key it has, where the key is.
pub(crate) fn dict_insert(pairs: &mut Vec<(Value, Value)>, key: Value, value: Value) -> Run<()> {
    match super::find_key(pairs, &key)? {
        Some(at) => {
            if let Some(pair) = pairs.get_mut(at) {
                pair.1 = value;
            }
        }
        None => pairs.push((key, value)),
    }
    Ok(())
}

/// Adds an item to a set, unless it's there.
pub(crate) fn set_insert(items: &mut Vec<Value>, item: Value) -> Run<()> {
    for held in items.iter() {
        if super::show::plain_equal(held, &item)? {
            return Ok(());
        }
    }
    items.push(item);
    Ok(())
}

/// Every name an expression reads, and the names inside it that it binds: more than it needs,
/// which only means copying a name it doesn't read.
pub(crate) fn names_in_expr(expr: &Expr, names: &mut BTreeSet<String>) {
    match &expr.kind {
        ExprKind::Name(name) => {
            names.insert(name.clone());
        }
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => {
            for item in items {
                names_in_expr(item, names);
            }
        }
        ExprKind::Dict(pairs) => {
            for (key, value) in pairs {
                names_in_expr(key, names);
                names_in_expr(value, names);
            }
        }
        ExprKind::FStr(parts) => {
            for part in parts {
                if let FStrPart::Expr { expr, .. } = part {
                    names_in_expr(expr, names);
                }
            }
        }
        ExprKind::Comprehension { elem, value, iter, cond, .. } => {
            names_in_expr(elem, names);
            names_in_expr(iter, names);
            for part in [value, cond].into_iter().flatten() {
                names_in_expr(part, names);
            }
        }
        ExprKind::Unary { expr, .. } => names_in_expr(expr, names),
        ExprKind::Binary { lhs, rhs, .. } => {
            names_in_expr(lhs, names);
            names_in_expr(rhs, names);
        }
        ExprKind::Cond { then, cond, otherwise } => {
            for part in [then, cond, otherwise] {
                names_in_expr(part, names);
            }
        }
        ExprKind::Else { value, default, .. } => {
            names_in_expr(value, names);
            names_in_expr(default, names);
        }
        ExprKind::Try { value, context } => {
            names_in_expr(value, names);
            if let Some(context) = context {
                names_in_expr(context, names);
            }
        }
        ExprKind::Call { callee, args } => {
            names_in_expr(callee, names);
            for arg in args {
                names_in_expr(&arg.value, names);
            }
        }
        ExprKind::Attr { object, .. } => names_in_expr(object, names),
        ExprKind::Index { object, index } => {
            names_in_expr(object, names);
            names_in_expr(index, names);
        }
        ExprKind::Slice { object, start, end } => {
            names_in_expr(object, names);
            for part in [start, end].into_iter().flatten() {
                names_in_expr(part, names);
            }
        }
        ExprKind::Lambda { body, .. } => names_in_expr(body, names),
        ExprKind::Match { subject, arms } => {
            names_in_expr(subject, names);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    names_in_expr(guard, names);
                }
                names_in_stmts(&arm.body, names);
            }
        }
        _ => {}
    }
}

fn names_in_stmts(body: &[Stmt], names: &mut BTreeSet<String>) {
    for stmt in body {
        match &stmt.kind {
            StmtKind::Expr(expr)
            | StmtKind::Return(Some(expr))
            | StmtKind::Fail(expr)
            | StmtKind::Yield(expr) => {
                names_in_expr(expr, names);
            }
            StmtKind::Assign { target, value, .. } => {
                names_in_expr(target, names);
                names_in_expr(value, names);
            }
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    names_in_expr(cond, names);
                    names_in_stmts(body, names);
                }
                if let Some(body) = otherwise {
                    names_in_stmts(body, names);
                }
            }
            StmtKind::While { cond, body } => {
                names_in_expr(cond, names);
                names_in_stmts(body, names);
            }
            StmtKind::For { iter, body, .. } => {
                names_in_expr(iter, names);
                names_in_stmts(body, names);
            }
            _ => {}
        }
    }
}
