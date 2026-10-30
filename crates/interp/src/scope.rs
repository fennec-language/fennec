//! Names in a running function: one frame per call, one scope per block, so names created
//! inside an `if` or `for` don't outlive it.

use std::rc::Rc;

use crate::value::Value;

pub(crate) struct Binding {
    pub(crate) name: Rc<str>,
    pub(crate) value: Value,
    pub(crate) is_mut: bool,
}

pub(crate) struct Frame {
    scopes: Vec<Vec<Binding>>,
}

impl Frame {
    pub(crate) fn new() -> Self {
        Frame { scopes: vec![Vec::new()] }
    }

    pub(crate) fn push(&mut self) {
        self.scopes.push(Vec::new());
    }

    pub(crate) fn pop(&mut self) {
        self.scopes.pop();
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Binding> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .find(|binding| same_name(&binding.name, name))
    }

    pub(crate) fn get_mut(&mut self, name: &str) -> Option<&mut Binding> {
        self.scopes
            .iter_mut()
            .rev()
            .flat_map(|scope| scope.iter_mut().rev())
            .find(|binding| same_name(&binding.name, name))
    }

    pub(crate) fn declare(&mut self, name: &str, value: Value, is_mut: bool) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.push(Binding { name: Rc::from(name), value, is_mut });
        }
    }

    /// Creates a name whose text is shared, so creating it doesn't copy the text.
    pub(crate) fn declare_shared(&mut self, name: Rc<str>, value: Value, is_mut: bool) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.push(Binding { name, value, is_mut });
        }
    }

    /// Empties the frame for another call, keeping its memory.
    pub(crate) fn reset(&mut self) {
        self.scopes.truncate(1);
        match self.scopes.first_mut() {
            Some(names) => names.clear(),
            None => self.scopes.push(Vec::new()),
        }
    }

    /// Every name in sight, for an arrow function to capture.
    pub(crate) fn visible(&self) -> Vec<(String, Value)> {
        self.scopes
            .iter()
            .flatten()
            .map(|binding| (binding.name.to_string(), binding.value.clone()))
            .collect()
    }
}

/// Whether two names are the same. A name the runtime shares is the same text, so its
/// address is checked first.
fn same_name(a: &str, b: &str) -> bool {
    std::ptr::eq(a, b) || a == b
}
