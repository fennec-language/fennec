//! The parts of a list, tuple, set, dict or struct: shared by every copy of the value until one
//! copy is changed, which then takes a copy of its own, one level deep. So copying a value takes
//! the same time however big it is, as the compiled program's copies do, where copying it part by
//! part made building a deep value in a loop take time in proportion to its depth squared.
//!
//! What's cloned when a copy is changed is counted against the run's steps (`copied`), so a run
//! that copies a lot is still stopped. Letting go of a value goes through its parts one at a time,
//! never one call inside another, so a value nested 200,000 deep, as `fennec-fuzz` writes, is let
//! go of in a little stack.

use std::cell::Cell;
use std::sync::Arc;

use super::{Instance, Value};

thread_local! {
    /// How many parts this thread has cloned since `copied` last took the count.
    static COPIED: Cell<u64> = const { Cell::new(0) };
}

/// How many parts this thread has cloned to change a shared value since the last call, for the
/// run's steps.
pub(crate) fn copied() -> u64 {
    COPIED.with(|copied| copied.replace(0))
}

/// What a value's parts are, for letting go of them one at a time.
pub(crate) trait Parts: Clone + Default {
    /// How many values it holds, for what cloning it costs.
    fn count(&self) -> usize;
    /// Takes the values it holds out into `into`.
    fn take_into(&mut self, into: &mut Vec<Value>);
}

impl Parts for Vec<Value> {
    fn count(&self) -> usize {
        self.len()
    }

    fn take_into(&mut self, into: &mut Vec<Value>) {
        into.append(self);
    }
}

impl Parts for Vec<(Value, Value)> {
    fn count(&self) -> usize {
        self.len() * 2
    }

    fn take_into(&mut self, into: &mut Vec<Value>) {
        for (key, value) in self.drain(..) {
            into.push(key);
            into.push(value);
        }
    }
}

impl Parts for Instance {
    fn count(&self) -> usize {
        self.fields.len()
    }

    fn take_into(&mut self, into: &mut Vec<Value>) {
        into.extend(self.fields.drain(..).map(|(_, value)| value));
    }
}

/// A value's parts, shared by each copy until it's changed.
pub(crate) struct Held<T: Parts>(Arc<T>);

impl<T: Parts> Held<T> {
    pub(crate) fn new(parts: T) -> Held<T> {
        Held(Arc::new(parts))
    }

    /// The parts, to change: this copy's own, cloned one level deep first when another copy
    /// shares them, which is counted against the run's steps.
    pub(crate) fn own(&mut self) -> &mut T {
        if Arc::get_mut(&mut self.0).is_none() {
            let count = u64::try_from(self.0.count()).unwrap_or(u64::MAX);
            COPIED.with(|copied| copied.set(copied.get().saturating_add(count)));
        }
        Arc::make_mut(&mut self.0)
    }

    /// The parts, taken out: cloned, one level deep, when another copy shares them, and this
    /// copy left empty.
    pub(crate) fn into_inner(mut self) -> T {
        std::mem::take(self.own())
    }
}

impl<T: Parts> Clone for Held<T> {
    fn clone(&self) -> Held<T> {
        Held(Arc::clone(&self.0))
    }
}

impl<T: Parts> std::ops::Deref for Held<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

/// Changing the parts goes through `own`, so a change never reaches another copy's.
impl<T: Parts> std::ops::DerefMut for Held<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.own()
    }
}

impl<'a, T: Parts> IntoIterator for &'a Held<T>
where
    &'a T: IntoIterator,
{
    type Item = <&'a T as IntoIterator>::Item;
    type IntoIter = <&'a T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        (&*self.0).into_iter()
    }
}

impl<T: Parts + IntoIterator> IntoIterator for Held<T> {
    type Item = T::Item;
    type IntoIter = T::IntoIter;

    fn into_iter(self) -> T::IntoIter {
        self.into_inner().into_iter()
    }
}

impl<A> FromIterator<A> for Held<Vec<A>>
where
    Vec<A>: Parts,
{
    fn from_iter<I: IntoIterator<Item = A>>(items: I) -> Held<Vec<A>> {
        Held::new(items.into_iter().collect())
    }
}

impl<T: Parts + std::fmt::Debug> std::fmt::Debug for Held<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&*self.0, f)
    }
}

impl<T: Parts> Default for Held<T> {
    fn default() -> Held<T> {
        Held::new(T::default())
    }
}

impl<T: Parts> From<T> for Held<T> {
    fn from(parts: T) -> Held<T> {
        Held::new(parts)
    }
}

impl<T: Parts> Drop for Held<T> {
    /// Lets go of the parts only this copy holds one at a time: each value nested in them is
    /// emptied before it goes, so none of them goes on to let go of what's inside it.
    fn drop(&mut self) {
        let Some(parts) = Arc::get_mut(&mut self.0) else { return };
        let mut going = Vec::new();
        parts.take_into(&mut going);
        while let Some(value) = going.pop() {
            empty_into(value, &mut going);
        }
    }
}

/// Takes what `value` holds, when nothing else shares it, out into `into`, and lets go of it
/// emptied.
fn empty_into(value: Value, into: &mut Vec<Value>) {
    match value {
        Value::List(mut held) | Value::Tuple(mut held) | Value::Set(mut held) => {
            if let Some(parts) = Arc::get_mut(&mut held.0) {
                parts.take_into(into);
            }
        }
        Value::Dict(mut held) => {
            if let Some(parts) = Arc::get_mut(&mut held.0) {
                parts.take_into(into);
            }
        }
        Value::Instance(mut held) => {
            if let Some(parts) = Arc::get_mut(&mut held.0) {
                parts.take_into(into);
            }
        }
        Value::Table(mut table) => into.extend(table.slots.drain(..).filter_map(|slot| slot.item)),
        _ => {}
    }
}
