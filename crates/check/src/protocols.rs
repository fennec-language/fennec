//! The protocols a type joins by writing a method with a set name, like `def str`. A method
//! with that name has to take what the protocol passes, or printing it would fail when it runs.

use fennec_syntax::{Diagnostic, FnDef};

use crate::checker::Checker;

impl Checker {
    /// Checks each program type's protocol methods, as declared.
    pub(crate) fn check_protocols(&mut self) {
        let mut problems = Vec::new();
        for info in self.program.types.values().filter(|info| !info.prelude) {
            for method in info.def.methods.iter().filter(|method| method.name == "str") {
                problems.extend(str_problem(&info.def.name, method));
            }
        }
        self.diagnostics.extend(problems);
    }
}

/// What's wrong with a type's `def str`, if anything: it takes `self, mut out: StrWriter` and
/// gives back nothing.
fn str_problem(type_name: &str, method: &FnDef) -> Option<Diagnostic> {
    let writes = match method.params.as_slice() {
        [this, out] => {
            this.is_self
                && out.is_mut
                && out
                    .ty
                    .as_ref()
                    .is_some_and(|ty| ty.name == "StrWriter" && ty.args.is_empty() && !ty.optional)
        }
        _ => false,
    };
    (!writes || method.ret.is_some()).then(|| {
        Diagnostic::new(
            format!("`{type_name}`'s `def str` writes into `mut out: StrWriter` and gives back nothing"),
            method.span,
        )
        .with_help("write `def str(self, mut out: StrWriter)`, and add its text with `out.write(...)`")
    })
}
