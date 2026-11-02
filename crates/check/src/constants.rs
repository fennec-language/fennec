//! Constants: names at the top level of a file, like `KEYWORDS = {"def", "if"}`. A constant's
//! type is its written type, or else its value's, which is checked once, apart from any
//! function. Nothing can change a constant.

use fennec_syntax::{Diagnostic, Expr, Span, TypeExpr};

use crate::checker::{Checker, forget_vars};
use crate::function::FnCx;
use crate::messages::mismatch;
use crate::types::Type;

impl Checker {
    /// The type of the constant `name`, if there's one, checking its value the first time
    /// it's needed.
    pub(crate) fn constant_type(&mut self, name: &str) -> Option<Type> {
        let entry = self.program.constants.get(name)?;
        let (written, value, span, prelude) =
            (entry.ty.clone(), entry.value.clone(), entry.span, entry.prelude);
        match self.const_types.get(name) {
            Some(Some(ty)) => return Some(ty.clone()),
            Some(None) => {
                if !prelude {
                    self.diagnostics.push(
                        Diagnostic::new(format!("`{name}` depends on itself"), span).with_help(
                            "a constant's value can't use the constant, even through other constants",
                        ),
                    );
                }
                return Some(Type::Unknown);
            }
            None => {}
        }
        self.const_types.insert(name.to_owned(), None);
        let ty = FnCx::check_constant(self, name, written.as_ref(), &value, !prelude);
        if !prelude && let Some(inner) = self.shared_inside(&ty, &mut Vec::new()) {
            let message = format!("`{name}` holds `{inner}`, which is `shared`, so it can't be a constant");
            self.diagnostics.push(Diagnostic::new(message, span).with_help(
                "there's no `shared` at the top of a file: make it in `main` and pass it to what uses it",
            ));
        }
        self.const_types.insert(name.to_owned(), Some(ty.clone()));
        Some(ty)
    }
}

impl FnCx<'_> {
    /// Checks a constant's value, apart from any function. Returns the constant's type.
    fn check_constant(
        checker: &mut Checker,
        name: &str,
        written: Option<&TypeExpr>,
        value: &Expr,
        report: bool,
    ) -> Type {
        let mut cx = FnCx::new(checker, &format!("const {name}"), name, report);
        let actual = cx.expr(value);
        let ty = match written {
            Some(written) => {
                let declared = cx.resolve(written);
                if !cx.checker.fits(&declared, &actual) {
                    cx.error(mismatch(&format!("`{name}`"), &declared, &actual, value.span));
                }
                declared
            }
            None => actual,
        };
        let ty = cx.checker.settled(&ty);
        cx.names.push((value.span, name.to_owned(), ty.clone()));
        cx.finish(std::slice::from_ref(&ty));
        forget_vars(&ty)
    }

    /// Reports a change to a constant, if `name` is one that isn't hidden by a local name.
    /// Returns whether it is.
    pub(crate) fn changes_constant(&mut self, name: &str, span: Span) -> bool {
        if self.lookup(name).is_some() || !self.checker.program.constants.contains_key(name) {
            return false;
        }
        self.error(
            Diagnostic::new(format!("`{name}` is a constant, so it can't change"), span)
                .with_help(format!("make a changeable copy with a new name, like `mut copy = {name}`")),
        );
        true
    }
}
