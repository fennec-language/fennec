//! Constants: names at the top level of a file, like `KEYWORDS = {"def", "if"}`. Each one's
//! value is computed once, the first time something reads it, apart from any function's
//! names, and then kept.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Expr, Span};

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
