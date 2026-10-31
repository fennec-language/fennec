//! Places: a name, or a path into one, like `world.player.health` or `scores[2]`. Changing a
//! place takes its value out, changes it, and puts it back, so a value nothing else shares is
//! changed without being copied.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Expr, ExprKind, Span};

use crate::exec::not_mut;
use crate::interp::Interp;
use crate::ops;
use crate::value::Value;

#[derive(Clone, Debug)]
pub(crate) enum Step {
    Field(String),
    Index(Value),
}

#[derive(Clone, Debug)]
pub(crate) struct Place {
    pub(crate) root: String,
    pub(crate) steps: Vec<Step>,
    pub(crate) span: Span,
}

impl Place {
    pub(crate) fn then(&self, step: Step) -> Place {
        let mut place = self.clone();
        place.steps.push(step);
        place
    }
}

impl Interp<'_> {
    /// The place an expression names, if it names one.
    pub(crate) fn place(&mut self, expr: &Expr) -> Result<Option<Place>, Diagnostic> {
        let step = match &expr.kind {
            ExprKind::Name(name) => {
                return Ok(Some(Place { root: name.clone(), steps: Vec::new(), span: expr.span }));
            }
            ExprKind::Attr { object, name, optional: false } => (object, Step::Field(name.clone())),
            ExprKind::Index { object, index } => (object, Step::Index(self.eval(index)?)),
            _ => return Ok(None),
        };
        let (object, step) = step;
        Ok(self.place(object)?.map(|place| Place { span: expr.span, ..place.then(step) }))
    }

    /// The place an expression names, which must start at a `mut` name since it's about to change.
    pub(crate) fn mut_place(&mut self, expr: &Expr, what: &str) -> Result<Place, Diagnostic> {
        let place = self.place(expr)?.ok_or_else(|| Diagnostic::new(what, expr.span))?;
        match self.frame.get(&place.root) {
            Some(binding) if binding.is_mut => Ok(place),
            Some(_) => Err(not_mut(&place.root, expr.span)),
            None => Err(Diagnostic::new(format!("`{}` doesn't exist", place.root), expr.span)),
        }
    }

    /// Takes the value out of a place, leaving a placeholder, so changing it doesn't copy it.
    pub(crate) fn take(&mut self, place: &Place) -> Result<Value, Diagnostic> {
        let binding = self
            .frame
            .get_mut(&place.root)
            .ok_or_else(|| Diagnostic::new(format!("`{}` doesn't exist", place.root), place.span))?;
        unpack_in_place(&mut binding.value);
        take_in(&mut binding.value, &place.steps, place.span)
    }

    pub(crate) fn put(&mut self, place: &Place, value: Value) -> Result<(), Diagnostic> {
        let binding = self
            .frame
            .get_mut(&place.root)
            .ok_or_else(|| Diagnostic::new(format!("`{}` doesn't exist", place.root), place.span))?;
        unpack_in_place(&mut binding.value);
        put_in(&mut binding.value, &place.steps, value, place.span)?;
        if place.steps.is_empty() {
            return Ok(());
        }
        self.check_rules_along(place)
    }

    /// Runs the `where` rule of every field a write to a place went through. A broken rule
    /// stops the program: a write has nowhere to give back a failure.
    fn check_rules_along(&mut self, place: &Place) -> Result<(), Diagnostic> {
        for (type_name, field, rule, value) in self.guarded_along(place) {
            self.frame.push();
            self.frame.declare(&field, value, false);
            let holds = self.truth(&rule, "where");
            self.frame.pop();
            if !holds? {
                return Err(crate::types::broken_rule(&type_name, &field, place.span));
            }
        }
        Ok(())
    }

    /// The fields with `where` rules a place goes through: each one's type, name, rule and value.
    fn guarded_along(&self, place: &Place) -> Vec<(String, String, Expr, Value)> {
        let mut found = Vec::new();
        let Some(mut at) = self.frame.get(&place.root).map(|binding| &binding.value) else { return found };
        for step in &place.steps {
            let next = match (step, at) {
                (Step::Field(name), Value::Instance(instance)) => {
                    let next = instance.field(name);
                    if let (Some(next), Some(rule)) = (next, self.rule_of_field(instance, name)) {
                        found.push((instance.type_name.to_string(), name.clone(), rule, next.clone()));
                    }
                    next
                }
                (Step::Index(Value::Int(index)), Value::List(items)) => {
                    usize::try_from(*index).ok().and_then(|index| items.get(index))
                }
                _ => None,
            };
            let Some(next) = next else { break };
            at = next;
        }
        found
    }

    /// The `where` rule of a field of a value, if it has one.
    pub(crate) fn rule_of_field(&self, instance: &crate::value::Instance, field: &str) -> Option<Expr> {
        let def = &self.type_info(&instance.type_name)?.def;
        let fields = match &instance.case {
            Some(case) => &def.cases.iter().find(|written| written.name == **case)?.fields,
            None => &def.fields,
        };
        fields.iter().find(|written| written.name == field)?.rule.clone()
    }
}

/// Changing part of a packed list goes through the interpreter's own kind of list.
fn unpack_in_place(value: &mut Value) {
    if let Value::Packed(packed) = value {
        *value = packed.unpack();
    }
}

fn take_in(value: &mut Value, steps: &[Step], span: Span) -> Result<Value, Diagnostic> {
    match steps.split_first() {
        None => Ok(std::mem::replace(value, Value::Unit)),
        Some((step, rest)) => take_in(slot(value, step, span)?, rest, span),
    }
}

fn put_in(value: &mut Value, steps: &[Step], new: Value, span: Span) -> Result<(), Diagnostic> {
    match steps.split_first() {
        None => {
            *value = new;
            Ok(())
        }
        Some((Step::Index(key), [])) if matches!(value, Value::Dict(_)) => {
            if let Value::Dict(pairs) = value {
                Rc::make_mut(pairs).set(key.clone(), new);
            }
            Ok(())
        }
        Some((step, rest)) => put_in(slot(value, step, span)?, rest, new, span),
    }
}

/// One step into a value, copying it first only if it's shared.
fn slot<'v>(value: &'v mut Value, step: &Step, span: Span) -> Result<&'v mut Value, Diagnostic> {
    match (value, step) {
        (Value::Instance(instance), Step::Field(name)) => {
            let type_name = instance.type_name.clone();
            Rc::make_mut(instance)
                .field_mut(name)
                .ok_or_else(|| Diagnostic::new(format!("`{type_name}` has no field `{name}`"), span))
        }
        (Value::List(items), Step::Index(Value::Int(at))) => {
            let items = Rc::make_mut(items);
            let at = ops::position(*at, items.len(), span)?;
            items.get_mut(at).ok_or_else(|| Diagnostic::new("index out of range", span))
        }
        (Value::Dict(pairs), Step::Index(key)) => Rc::make_mut(pairs).get_mut(key).ok_or_else(|| {
            Diagnostic::new(format!("the dict has no key {}", crate::format::repr(key)), span)
        }),
        (Value::IdMap(map), Step::Index(Value::Id(id))) => Rc::make_mut(map)
            .get_mut(*id)
            .ok_or_else(|| Diagnostic::new("this Id refers to something that's been removed", span)),
        (value, _) => Err(Diagnostic::new(format!("can't change part of a {}", value.type_name()), span)),
    }
}
