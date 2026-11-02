//! Constants: names at the top level of a file, like `KEYWORDS = {"def", "if"}`. Each one's
//! value is computed once, the first time something reads it, apart from any function's
//! names, and then kept.

use std::rc::Rc;

use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, Item, Module, Span, UnaryOp};

use crate::interp::Interp;
use crate::scope::Frame;
use crate::value::Value;

/// A constant, before and after its value is computed.
#[derive(Clone, Debug)]
pub(crate) enum Constant {
    Waiting(Rc<Expr>),
    /// Being computed now: reading it again means it depends on itself.
    Computing,
    Ready(Value),
}

impl Interp<'_> {
    /// The value of a name: one of the current call's, or else a constant, a function or a type.
    pub(crate) fn name(&mut self, name: &str, span: Span) -> Result<Value, Diagnostic> {
        if self.frame.get(name).is_none()
            && let Some(value) = self.constant(name, span)
        {
            return value;
        }
        self.lookup(name, span)
    }

    /// The value of the constant `name`, if there's one by that name, computed the first
    /// time it's read.
    pub(crate) fn constant(&mut self, name: &str, span: Span) -> Option<Result<Value, Diagnostic>> {
        let expr = match self.constants.get(name)? {
            Constant::Ready(value) => return Some(Ok(value.clone())),
            Constant::Computing => {
                let message = format!("`{name}` depends on itself");
                return Some(Err(Diagnostic::new(message, span)));
            }
            Constant::Waiting(expr) => Rc::clone(expr),
        };
        self.constants.insert(name.to_owned(), Constant::Computing);
        let caller = std::mem::replace(&mut self.frame, Frame::new());
        let value = self.eval(&expr);
        self.frame = caller;
        match &value {
            Ok(value) => self.constants.insert(name.to_owned(), Constant::Ready(value.clone())),
            Err(_) => self.constants.insert(name.to_owned(), Constant::Waiting(expr)),
        };
        Some(value)
    }
}

/// What working out a constant while compiling gave.
#[derive(Debug)]
pub enum Baked {
    /// Its value, written out as a literal the program can hold instead.
    Literal(Expr),
    /// It took more steps than the budget, so it's worked out when the program starts.
    TooLong,
    /// Its value has no literal form, like a `Regex`, so it's worked out when the program starts.
    Unwritable,
    /// Working it out failed, as it would every time the program started.
    Failed(Diagnostic),
}

/// Where the literals baked from constants point: past any source, and unique, so the typed
/// tree tells them apart.
const BAKED_SPANS: u32 = 1 << 30;

/// Works out each of a module's own constants that isn't a literal already, each within
/// `steps` steps, and gives back its value written out as a literal. A constant takes no
/// capabilities, so nothing it does can differ from one run to the next.
pub fn bake_constants(module: &Module, steps: u64) -> Result<Vec<(String, Baked)>, Diagnostic> {
    let module = module.clone();
    let worker = std::thread::Builder::new().stack_size(1 << 28).spawn(move || -> Result<_, Diagnostic> {
        let mut sink = std::io::sink();
        let mut interp = Interp::new(&mut sink)?;
        interp.load(&module)?;
        interp.limit_steps(steps);
        let own: Vec<&str> = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Struct(def) | Item::Enum(def) => Some(def.name.as_str()),
                _ => None,
            })
            .collect();
        let mut writer = Writer { own: &own, next: BAKED_SPANS };
        let mut baked = Vec::new();
        for item in &module.items {
            let Item::Const { name, value, .. } = item else { continue };
            if is_literal(value) {
                continue;
            }
            interp.refill_steps();
            let result = match interp.constant(name, value.span) {
                Some(Ok(found)) => writer.literal(&found).map_or(Baked::Unwritable, Baked::Literal),
                Some(Err(_)) if interp.steps_left == Some(0) => Baked::TooLong,
                Some(Err(problem)) => Baked::Failed(problem),
                None => continue,
            };
            baked.push((name.clone(), result));
        }
        Ok(baked)
    });
    let worker = worker.map_err(|problem| Diagnostic::new(problem.to_string(), Span::default()))?;
    worker
        .join()
        .unwrap_or_else(|_| Err(Diagnostic::new("working out the constants failed", Span::default())))
}

/// Whether a constant's value is a literal already: a number, string, bool or `None`.
fn is_literal(value: &Expr) -> bool {
    match &value.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Duration(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::None => true,
        ExprKind::Unary { op: UnaryOp::Neg, expr } => {
            matches!(expr.kind, ExprKind::Int(_) | ExprKind::Float(_))
        }
        _ => false,
    }
}

/// Writes values out as literal expressions.
struct Writer<'a> {
    /// The program's own types, which it can name to build one.
    own: &'a [&'a str],
    next: u32,
}

impl Writer<'_> {
    fn expr(&mut self, kind: ExprKind) -> Expr {
        let span = Span { start: self.next, end: self.next + 1 };
        self.next += 2;
        Expr { kind, span }
    }

    /// A value as a literal, or nothing when it has no literal form: an empty collection,
    /// whose items' type a literal can't say, a number that isn't finite, or a value of a type
    /// the program can't name or build.
    fn literal(&mut self, value: &Value) -> Option<Expr> {
        let kind = match value {
            Value::None => ExprKind::None,
            Value::Bool(value) => ExprKind::Bool(*value),
            Value::Int(value) if *value < 0 => {
                let inner = self.expr(ExprKind::Int(value.checked_neg()?));
                ExprKind::Unary { op: UnaryOp::Neg, expr: Box::new(inner) }
            }
            Value::Int(value) => ExprKind::Int(*value),
            Value::Float(value) if !value.is_finite() => return None,
            Value::Float(value) if value.is_sign_negative() => {
                let inner = self.expr(ExprKind::Float(-value));
                ExprKind::Unary { op: UnaryOp::Neg, expr: Box::new(inner) }
            }
            Value::Float(value) => ExprKind::Float(*value),
            Value::Str(text) => ExprKind::Str(text.to_string()),
            Value::List(items) if !items.is_empty() => ExprKind::List(self.all(items)?),
            Value::Packed(packed) => return self.literal(&packed.unpack()),
            Value::Tuple(items) => ExprKind::Tuple(self.all(items)?),
            Value::Set(items) if !items.is_empty() => ExprKind::Set(self.all(items)?),
            Value::Dict(pairs) if !pairs.is_empty() => {
                let mut written = Vec::new();
                for (key, value) in pairs.iter() {
                    written.push((self.literal(key)?, self.literal(value)?));
                }
                ExprKind::Dict(written)
            }
            Value::Instance(instance) => return self.instance(instance),
            _ => return None,
        };
        Some(self.expr(kind))
    }

    fn all(&mut self, items: &[Value]) -> Option<Vec<Expr>> {
        items.iter().map(|item| self.literal(item)).collect()
    }

    /// A struct or an enum case built by name, or a `Duration` as its literal. An error, a
    /// standard library type the program can't name, and a struct with `_` fields, which only
    /// its own methods can set, stay as they're written.
    fn instance(&mut self, instance: &crate::value::Instance) -> Option<Expr> {
        if instance.type_name.as_ref() == "Duration"
            && let [(_, Value::Int(nanos))] = instance.fields.as_slice()
        {
            return Some(self.expr(ExprKind::Duration(*nanos)));
        }
        let type_name = instance.type_name.as_ref();
        if instance.is_error || !self.own.contains(&type_name) {
            return None;
        }
        let mut args = Vec::new();
        for (name, value) in &instance.fields {
            if name.starts_with('_') {
                return None;
            }
            args.push(Arg { name: Some(name.to_string()), is_mut: false, value: self.literal(value)? });
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
