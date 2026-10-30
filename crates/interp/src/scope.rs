//! Names in a running function: one frame per call, one scope per block, so names created
//! inside an `if` or `for` don't outlive it.

use crate::value::Value;

pub(crate) struct Binding {
    pub(crate) name: String,
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
        self.scopes.iter().rev().flat_map(|scope| scope.iter().rev()).find(|binding| binding.name == name)
    }

    pub(crate) fn get_mut(&mut self, name: &str) -> Option<&mut Binding> {
        self.scopes
            .iter_mut()
            .rev()
            .flat_map(|scope| scope.iter_mut().rev())
            .find(|binding| binding.name == name)
    }

    pub(crate) fn declare(&mut self, name: &str, value: Value, is_mut: bool) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.push(Binding { name: name.to_owned(), value, is_mut });
        }
    }

    /// Every name in sight, for an arrow function to capture.
    pub(crate) fn visible(&self) -> Vec<(String, Value)> {
        self.scopes.iter().flatten().map(|binding| (binding.name.clone(), binding.value.clone())).collect()
    }
}
