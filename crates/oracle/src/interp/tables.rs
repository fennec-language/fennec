//! `std.collections.Table`'s methods, modeled from `docs/language.md` and what the compiled
//! program does: an item goes in the lowest slot that's free, a removed item's slot counts on to
//! its next generation, and the items are read and gone through in slot order.

use fennec_syntax::Span;

use super::calls::{ArgVal, Receiver, Target};
use super::{Interp, Place, Run, Step, unsupported};
use crate::value::{Id, Table, Value};

impl Interp {
    /// Whether `Table` names the standard library's, which the program doesn't declare one of its
    /// own in place of, or import from somewhere else.
    pub(crate) fn is_the_table(&self) -> bool {
        !self.program.types.contains_key("Table")
            && self.program.imported.get("Table").is_none_or(|from| from == "std.collections.Table")
    }

    /// `table.name(...)`.
    pub(crate) fn table_method(
        &mut self,
        receiver: Receiver,
        name: &str,
        args: Vec<Value>,
        span: Span,
    ) -> Run<Value> {
        match (name, args.as_slice()) {
            ("get", [Value::Id(id)]) => self.read_table(&receiver, span, |table| {
                let found = table.find(*id).and_then(|at| table.slots.get(at));
                found.and_then(|slot| slot.item.clone()).unwrap_or(Value::None)
            }),
            ("values", []) => self.read_table(&receiver, span, |table| {
                Value::List(table.live().map(|(_, item)| item.clone()).collect())
            }),
            ("ids", []) => self.read_table(&receiver, span, |table| {
                Value::List(table.live().map(|(id, _)| Value::Id(id)).collect())
            }),
            ("items", []) => self.read_table(&receiver, span, |table| {
                let pairs = table.live().map(|(id, item)| Value::Tuple(vec![Value::Id(id), item.clone()]));
                Value::List(pairs.collect())
            }),
            ("modify", [Value::Id(id), change]) => {
                let place = table_place(receiver, name)?;
                self.modify(&place, *id, change, span)
            }
            ("remove_where", [test]) => {
                let place = table_place(receiver, name)?;
                self.remove_where(&place, test, span)
            }
            _ => {
                let place = table_place(receiver, name)?;
                self.change_table(&place, name, args, span)
            }
        }
    }

    /// The methods that change a `Table` and need nothing but it: `add`, `remove`, `update` and
    /// `clear`.
    fn change_table(&mut self, place: &Place, name: &str, args: Vec<Value>, span: Span) -> Run<Value> {
        let mut args = args.into_iter();
        let (first, second, more) = (args.next(), args.next(), args.next());
        self.change_place(place, span, false, |held| {
            let Value::Table(table) = held else { return unsupported("a Table that isn't one") };
            match (name, first, second, more) {
                ("add", Some(item), None, None) => Ok(Value::Id(table.add(item))),
                ("remove", Some(Value::Id(id)), None, None) => Ok(table.remove(id).unwrap_or(Value::None)),
                ("clear", None, None, None) => {
                    for slot in &mut table.slots {
                        slot.item = None;
                    }
                    Ok(Value::Unit)
                }
                // It's written `if self.get(id) is None: return False`, so an item that's itself
                // missing isn't changed either.
                ("update", Some(Value::Id(id)), Some(value), None) => {
                    let found = table.find(id).and_then(|at| table.slots.get_mut(at));
                    match found.and_then(|slot| slot.item.as_mut()) {
                        Some(item) if !matches!(item, Value::None) => {
                            *item = value;
                            Ok(Value::Bool(true))
                        }
                        _ => Ok(Value::Bool(false)),
                    }
                }
                _ => unsupported(format!("the Table method `{name}`")),
            }
        })
    }

    /// Reads something of the `Table` a method is called on.
    fn read_table(
        &mut self,
        receiver: &Receiver,
        span: Span,
        read: impl FnOnce(&Table) -> Value,
    ) -> Run<Value> {
        let read = |held: &Value| match held {
            Value::Table(table) => Some(read(table)),
            _ => None,
        };
        let found = match receiver {
            Receiver::Place(place) => self.peek_place(place, span, read)?,
            Receiver::Value(value) => read(value),
        };
        let Some(found) = found else { return unsupported("a Table that isn't one") };
        self.charge(&found)?;
        Ok(found)
    }

    /// `table.modify(id, change)`: calls `change` on the item where it is, if the `Id` finds one,
    /// and says whether it did.
    fn modify(&mut self, place: &Place, id: Id, change: &Value, span: Span) -> Run<Value> {
        let found = self.peek_place(place, span, |held| match held {
            Value::Table(table) => Some(table.find(id).is_some()),
            _ => None,
        })?;
        match found {
            Some(true) => {
                let item = place.then(Step::At(Value::Id(id)));
                self.call_at(change, vec![ArgVal::Ref(None, item)], span)?;
                Ok(Value::Bool(true))
            }
            Some(false) => Ok(Value::Bool(false)),
            None => unsupported("a Table that isn't one"),
        }
    }

    /// `table.remove_where(test)`: takes out each item `test` gives `True` for, in slot order.
    fn remove_where(&mut self, place: &Place, test: &Value, span: Span) -> Run<Value> {
        let Value::Table(table) = self.read_place(place, span)? else {
            return unsupported("a Table that isn't one");
        };
        let mut gone = Vec::new();
        for (id, item) in table.live() {
            match self.call_at(test, vec![ArgVal::Value(None, item.clone())], span)? {
                Value::Bool(true) => gone.push(id),
                Value::Bool(false) => {}
                _ => return unsupported("a test for `remove_where` that doesn't give a bool"),
            }
        }
        self.change_place(place, span, false, |held| {
            let Value::Table(table) = held else { return unsupported("a Table that isn't one") };
            for id in gone {
                table.remove(id);
            }
            Ok(Value::Unit)
        })
    }

    /// Calls a function held as a value, written or an arrow function, with these arguments.
    fn call_at(&mut self, func: &Value, given: Vec<ArgVal>, span: Span) -> Run<Value> {
        match self.func_target(func.clone())? {
            Target::Def { def, parent, receiver } => self.call_def(def, parent, receiver, given, span),
            Target::Lambda { params, body, captured } => self.call_lambda(&params, body, captured, given),
            _ => unsupported("passing a builtin to a Table method"),
        }
    }
}

/// The place a method that changes a `Table` changes it in.
fn table_place(receiver: Receiver, name: &str) -> Run<Place> {
    match receiver {
        Receiver::Place(place) => Ok(place),
        Receiver::Value(_) => unsupported(format!("the Table method `{name}` on a value that isn't a place")),
    }
}
