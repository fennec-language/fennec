//! The types of a program's expressions, which `needless-conversion`, `double-lookup`,
//! `stale-copy` and `self-compare` read. Each lint asks only once it has found something to ask
//! about, and the program is checked the first time one asks, once for all of them: before, each
//! checked all of the program again for itself.

use std::cell::OnceCell;
use std::collections::HashMap;

use fennec_check::{Checked, Type};
use fennec_syntax::Module;

/// A module, and its typed check once a lint has asked for it.
pub(crate) struct Types<'m> {
    module: &'m Module,
    checked: OnceCell<Option<Checked>>,
    #[cfg(test)]
    checks: std::cell::Cell<usize>,
}

impl<'m> Types<'m> {
    pub(crate) fn new(module: &'m Module) -> Self {
        Self {
            module,
            checked: OnceCell::new(),
            #[cfg(test)]
            checks: std::cell::Cell::new(0),
        }
    }

    /// The type of every expression in the module, by where it's written, or nothing when the
    /// module doesn't check.
    pub(crate) fn by_span(&self) -> Option<HashMap<(u32, u32), &Type>> {
        let checked = self.checked.get_or_init(|| {
            #[cfg(test)]
            self.checks.set(self.checks.get() + 1);
            fennec_check::check_typed(self.module).ok()
        });
        let checked = checked.as_ref()?;
        Some(
            checked
                .types
                .iter()
                .flat_map(|(_, body)| body.exprs())
                .map(|(span, ty)| ((span.start, span.end), ty))
                .collect(),
        )
    }

    /// How many times the module has been checked, for the tests to count.
    #[cfg(test)]
    pub(crate) fn checks(&self) -> usize {
        self.checks.get()
    }
}
