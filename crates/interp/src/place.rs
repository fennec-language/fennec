//! Places: a name, or a path into one, like `world.player.health` or `scores[2]`. Changing a
//! place takes its value out, changes it, and puts it back, so a value nothing else shares is
//! changed without being copied. A step into a type's own `def []` reads the item with it,
//! and putting a value back writes the item with its `def []=`.

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
        let root = binding.value.clone();
        if let Some(own) = self.own_index(&root, place) {
            drop(root);
            let item = self.read_item(place, &own)?;
            self.frame.push();
            self.frame.declare(ITEM, item, true);
            let inner = Place { root: ITEM.to_owned(), steps: own.after.to_vec(), span: place.span };
            let taken = self.take(&inner);
            self.frame.pop();
            return taken;
        }
        drop(root);
        let binding = self
            .frame
            .get_mut(&place.root)
            .ok_or_else(|| Diagnostic::new(format!("`{}` doesn't exist", place.root), place.span))?;
        take_in(&mut binding.value, &place.steps, place.span)
    }

    pub(crate) fn put(&mut self, place: &Place, value: Value) -> Result<(), Diagnostic> {
        let binding = self
            .frame
            .get_mut(&place.root)
            .ok_or_else(|| Diagnostic::new(format!("`{}` doesn't exist", place.root), place.span))?;
        unpack_in_place(&mut binding.value);
        let root = binding.value.clone();
        if let Some(own) = self.own_index(&root, place) {
            drop(root);
            return self.put_through(place, &own, value);
        }
        drop(root);
        let binding = self
            .frame
            .get_mut(&place.root)
            .ok_or_else(|| Diagnostic::new(format!("`{}` doesn't exist", place.root), place.span))?;
        put_in(&mut binding.value, &place.steps, value, place.span)
    }

    /// The first step of a place into a type's own `def []`.
    fn own_index<'p>(&self, root: &Value, place: &'p Place) -> Option<OwnIndex<'p>> {
        let mut value = root;
        for (at, step) in place.steps.iter().enumerate() {
            if let (Value::Instance(instance), Step::Index(key)) = (value, step)
                && self.type_info(&instance.type_name).is_some_and(|info| info.methods.contains_key("[]"))
            {
                return Some(OwnIndex {
                    before: place.steps.get(..at)?,
                    key,
                    after: place.steps.get(at + 1..)?,
                });
            }
            value = step_into(value, step)?;
        }
        None
    }

    /// What a type's own `def []` gives for a step of a place.
    fn read_item(&mut self, place: &Place, own: &OwnIndex<'_>) -> Result<Value, Diagnostic> {
        let binding = self
            .frame
            .get(&place.root)
            .ok_or_else(|| Diagnostic::new(format!("`{}` doesn't exist", place.root), place.span))?;
        let mut holder = &binding.value;
        for step in own.before {
            holder = step_into(holder, step)
                .ok_or_else(|| Diagnostic::new("this can't be changed", place.span))?;
        }
        let holder = holder.clone();
        self.index(holder, own.key.clone(), place.span)
    }

    /// Puts a value in a place that goes through a type's own `def []`: changes the item, then
    /// writes it with the type's `def []=`.
    fn put_through(&mut self, place: &Place, own: &OwnIndex<'_>, value: Value) -> Result<(), Diagnostic> {
        let item = if own.after.is_empty() {
            value
        } else {
            let item = self.read_item(place, own)?;
            self.frame.push();
            self.frame.declare(ITEM, item, true);
            let inner = Place { root: ITEM.to_owned(), steps: own.after.to_vec(), span: place.span };
            let changed =
                self.put(&inner, value).and_then(|()| self.take(&Place { steps: Vec::new(), ..inner }));
            self.frame.pop();
            changed?
        };
        let holder_place = Place { steps: own.before.to_vec(), ..place.clone() };
        let holder = self.take(&holder_place)?;
        self.frame.push();
        self.frame.declare(HOLDER, holder, true);
        self.frame.declare(KEY, own.key.clone(), false);
        self.frame.declare(ITEM, item, false);
        let name = |name: &str| Expr { kind: ExprKind::Name(name.to_owned()), span: place.span };
        let object = name(HOLDER);
        let written = self
            .call_mut_method(&object, "[]=", &[name(KEY), name(ITEM)], place.span)
            .and_then(|_| self.take(&Place { root: HOLDER.to_owned(), steps: Vec::new(), span: place.span }));
        self.frame.pop();
        self.put(&holder_place, written?)
    }
}

/// The names that hold the parts of a place while a type's own `def []=` runs.
const ITEM: &str = "$item";
const HOLDER: &str = "$holder";
const KEY: &str = "$key";

/// A step of a place into a type's own `def []`, and the steps around it.
struct OwnIndex<'p> {
    before: &'p [Step],
    key: &'p Value,
    after: &'p [Step],
}

/// One step into a value, to read it.
fn step_into<'v>(value: &'v Value, step: &Step) -> Option<&'v Value> {
    match (value, step) {
        (Value::Instance(instance), Step::Field(name)) => instance.field(name),
        (Value::List(items), Step::Index(Value::Int(at))) => {
            items.get(ops::position(*at, items.len(), Span::default()).ok()?)
        }
        (Value::Dict(pairs), Step::Index(key)) => pairs.get(key),
        (Value::IdMap(map), Step::Index(Value::Id(id))) => map.get(*id),
        _ => None,
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
