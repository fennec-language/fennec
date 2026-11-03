//! Places: a name, or a path into one, like `world.player.health` or `scores[2]`. Changing a
//! place takes its value out, changes it, and puts it back, so a value nothing else shares is
//! changed without being copied. A step into a type's own `def []` reads the item with it,
//! and putting a value back writes the item with its `def []=`.

use std::cell::RefCell;
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
        if let Some(cell) = self.cell_at(place) {
            return self.in_cell(&cell, place, Self::take);
        }
        if let Some(through) = self.through_positions(place)? {
            return self.take(&through);
        }
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
        if let Some(cell) = self.cell_at(place) {
            return self.in_cell(&cell, place, |interp, inner| interp.put(inner, value));
        }
        if let Some(through) = self.through_positions(place)? {
            return self.put(&through, value);
        }
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
        put_in(&mut binding.value, &place.steps, value, place.span)?;
        if place.steps.is_empty() {
            return Ok(());
        }
        self.check_rules_along(place)
    }

    /// The cell of the `shared` handle a path into starts at: `x` in `x.total`, in a `with`
    /// block that locked `x`.
    fn cell_at(&self, place: &Place) -> Option<Rc<RefCell<Value>>> {
        match self.frame.get(&place.root).map(|binding| &binding.value) {
            Some(Value::Shared(cell)) if !place.steps.is_empty() => Some(Rc::clone(cell)),
            _ => None,
        }
    }

    /// Changes what's in a `shared` handle's cell: holds it by a name of its own for the change.
    fn in_cell<T>(
        &mut self,
        cell: &RefCell<Value>,
        place: &Place,
        change: impl FnOnce(&mut Self, &Place) -> Result<T, Diagnostic>,
    ) -> Result<T, Diagnostic> {
        self.frame.push();
        self.frame.declare(CELL, cell.replace(Value::Unit), true);
        let changed = change(self, &Place { root: CELL.to_owned(), ..place.clone() });
        let held = self.frame.get_mut(CELL).map(|binding| std::mem::replace(&mut binding.value, Value::Unit));
        self.frame.pop();
        cell.replace(held.unwrap_or(Value::Unit));
        changed
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

    /// The `where` rule of a field of a struct, if it has one. Enum cases' fields can't have them.
    pub(crate) fn rule_of_field(&self, instance: &crate::value::Instance, field: &str) -> Option<Expr> {
        let fields = &self.type_info(&instance.type_name)?.def.fields;
        fields.iter().find(|written| written.name == field)?.rule.clone()
    }

    /// Whether a value's type has positions: the `mut` form of `def []`.
    pub(crate) fn has_positions(&self, value: &Value) -> bool {
        let Value::Instance(instance) = value else { return false };
        self.type_info(&instance.type_name).is_some_and(|info| info.methods.contains_key(MUT_INDEX))
    }

    /// The first step of a place into a position of a type with the `mut` form of `def []`.
    fn position_step(&self, place: &Place) -> Option<usize> {
        let mut value = &self.frame.get(&place.root)?.value;
        for (at, step) in place.steps.iter().enumerate() {
            if matches!(step, Step::Index(Value::Int(_))) && self.has_positions(value) {
                return Some(at);
            }
            value = step_into(value, step)?;
        }
        None
    }

    /// A place with its first step into a position replaced by the place that position is.
    fn through_positions(&mut self, place: &Place) -> Result<Option<Place>, Diagnostic> {
        let Some(at) = self.position_step(place) else { return Ok(None) };
        let (Some(before), Some(Step::Index(key)), Some(after)) =
            (place.steps.get(..at), place.steps.get(at), place.steps.get(at + 1..))
        else {
            return Ok(None);
        };
        let holder = Place { steps: before.to_vec(), ..place.clone() };
        let inside = self.position_steps(&holder, key, place.span)?;
        let steps = before.iter().chain(&inside).chain(after).cloned().collect();
        Ok(Some(Place { steps, ..place.clone() }))
    }

    /// Runs a holder's `mut` form of `def []` for a position, for the steps from the holder to
    /// that position's item. Anything else the method changes stays changed.
    fn position_steps(&mut self, holder: &Place, key: &Value, span: Span) -> Result<Vec<Step>, Diagnostic> {
        let held = self.take(holder)?;
        self.frame.push();
        self.frame.declare(HOLDER, held, true);
        self.frame.declare(KEY, key.clone(), false);
        let name = |name: &str| Expr { kind: ExprKind::Name(name.to_owned()), span };
        let outer = self.naming_place.replace(self.depth + 1);
        let called = self.call_mut_method(&name(HOLDER), MUT_INDEX, &[name(KEY)], span);
        self.naming_place = outer;
        let named = self.named_place.take();
        let held = self.take(&Place { root: HOLDER.to_owned(), steps: Vec::new(), span });
        self.frame.pop();
        self.put(holder, held?)?;
        called?;
        let named =
            named.ok_or_else(|| Diagnostic::new("the `mut` form of `def []` gave back no place", span))?;
        Ok(named.steps)
    }

    /// What's in a place, through any positions on the way.
    fn read_place(&mut self, place: &Place) -> Result<Value, Diagnostic> {
        if let Some(through) = self.through_positions(place)? {
            return self.read_place(&through);
        }
        let binding = self
            .frame
            .get(&place.root)
            .ok_or_else(|| Diagnostic::new(format!("`{}` doesn't exist", place.root), place.span))?;
        let mut value = &binding.value;
        for step in &place.steps {
            value = step_into(value, step)
                .ok_or_else(|| Diagnostic::new("the `mut` form of `def []` named no item", place.span))?;
        }
        Ok(value.clone())
    }

    /// `value[at]` on a type with only the `mut` form of `def []`.
    pub(crate) fn read_position(&mut self, value: Value, at: Value, span: Span) -> Result<Value, Diagnostic> {
        self.frame.push();
        self.frame.declare(POSITIONED, value, true);
        let place = Place { root: POSITIONED.to_owned(), steps: vec![Step::Index(at)], span };
        let read = self.read_place(&place);
        self.frame.pop();
        read
    }

    /// The live positions of a value, each with its item, for what lists a `for mut`'s items
    /// without the place they came from.
    pub(crate) fn position_entries(
        &mut self,
        value: Value,
        span: Span,
    ) -> Result<Vec<(Option<Value>, Value)>, Diagnostic> {
        self.frame.push();
        self.frame.declare(POSITIONED, value.clone(), true);
        let place = Place { root: POSITIONED.to_owned(), steps: Vec::new(), span };
        let items = self.position_items(value, &place, span);
        self.frame.pop();
        let mut entries = Vec::new();
        if let crate::generators::Items::Listed(items) = items? {
            entries.extend(items);
        }
        Ok(entries)
    }

    /// The live positions of what a `for mut` goes over, each with its item. `source` is dropped
    /// before any item is read, so reaching one doesn't copy what holds it.
    pub(crate) fn position_items(
        &mut self,
        source: Value,
        place: &Place,
        span: Span,
    ) -> Result<crate::generators::Items, Diagnostic> {
        let Value::Instance(instance) = &source else {
            return self.items_of(source, span);
        };
        let info = self.type_info(&instance.type_name);
        let method = |name: &str| info.as_ref().and_then(|info| info.methods.get(name).cloned());
        let (length, is_live) = (method("len"), method("is_live"));
        let length =
            length.ok_or_else(|| Diagnostic::new("`for mut` over this needs its `def len`", span))?;
        let Value::Int(count) = self.call_with_values(&length, vec![source.clone()], span)? else {
            return Err(Diagnostic::new("`def len` should give an int", span));
        };
        let mut live = Vec::new();
        for at in 0..count {
            let here = Value::Int(at);
            let keep = match &is_live {
                Some(is_live) => {
                    self.call_with_values(is_live, vec![source.clone(), here.clone()], span)?
                        == Value::Bool(true)
                }
                None => true,
            };
            if keep {
                live.push(here);
            }
        }
        drop(source);
        let mut items = Vec::with_capacity(live.len());
        for at in live {
            let item = self.read_place(&place.then(Step::Index(at.clone())))?;
            items.push((Some(at), item));
        }
        Ok(crate::generators::Items::Listed(items.into_iter()))
    }

    /// The first step of a place into a type with its own `def []=`.
    fn own_index<'p>(&self, root: &Value, place: &'p Place) -> Option<OwnIndex<'p>> {
        let mut value = root;
        for (at, step) in place.steps.iter().enumerate() {
            if let (Value::Instance(instance), Step::Index(key)) = (value, step)
                && self.type_info(&instance.type_name).is_some_and(|info| info.methods.contains_key("[]="))
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

/// The `mut` form of `def []`, under the name the parser gives it.
const MUT_INDEX: &str = "[]mut";
/// What holds a value read through its `mut` form of `def []`.
const POSITIONED: &str = "$positioned";

/// The names that hold the parts of a place while a type's own `def []=` runs.
const ITEM: &str = "$item";
const HOLDER: &str = "$holder";
const KEY: &str = "$key";
const CELL: &str = "$cell";

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
        (Value::Buffer(buffer), Step::Index(Value::Int(at))) => {
            buffer.items.get(ops::position(*at, buffer.items.len(), Span::default()).ok()?)
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
        (Value::Buffer(buffer), Step::Index(Value::Int(at))) => {
            let items = &mut Rc::make_mut(buffer).items;
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
