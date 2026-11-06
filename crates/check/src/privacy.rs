//! `_name` fields and methods: hidden from everything but their own type. Only the type's own
//! methods, and the functions nested in them, can read, write or call one. The one way in from
//! outside is the type's constructor, from code in the type's own kit: another kit, like a
//! program using the standard library, can't forge a value with a `_` field set to whatever it
//! likes, which capabilities like a network connection rely on.

use fennec_syntax::{Arg, Diagnostic, Field, Span};

use crate::function::FnCx;

impl FnCx<'_> {
    /// Whether the code being checked is inside `owner`: one of the methods written in its own
    /// declaration, or a function nested in one. A method an `extend` adds is outside it, or
    /// any program could extend a type to reach what it hides.
    fn inside(&self, owner: &str) -> bool {
        let Some(rest) = self.key.strip_prefix(owner).and_then(|rest| rest.strip_prefix('.')) else {
            return false;
        };
        let method = rest.split('.').next().unwrap_or(rest);
        // The methods generated to save a type save all of it, and no program can write them.
        let generated = self
            .checker
            .program
            .functions
            .get(&format!("{owner}.{method}"))
            .is_some_and(|entry| !entry.prelude && entry.def.span.in_prelude());
        generated
            || self
                .checker
                .program
                .type_info(owner)
                .is_some_and(|info| info.def.methods.iter().any(|def| def.name == method))
    }

    /// Reports a use of `owner`'s `_` field or method from outside it.
    pub(crate) fn private_member(&mut self, owner: &str, member: &str, span: Span) {
        if !member.starts_with('_') || self.inside(owner) {
            return;
        }
        let what = if self.checker.program.method(owner, member).is_some() { "call" } else { "reach" };
        self.error(Diagnostic::new(format!("`{member}` is private to `{owner}`"), span).with_help(format!(
            "only `{owner}`'s own methods can {what} `_` names: go through one of its other methods"
        )));
    }

    /// Reports arguments that set `owner`'s `_` fields when it's made from another kit, by name
    /// or by position. Code in its own kit can set them, as its methods can.
    pub(crate) fn private_fields_given(&mut self, owner: &str, fields: &[Field], args: &[Arg]) {
        let in_std = self.checker.program.type_info(owner).is_some_and(|info| info.prelude);
        // Until kits have dependencies, the only other kit is the standard library.
        let same_kit = |arg: &Arg| in_std == arg.value.span.in_prelude();
        let mut next = 0;
        for arg in args {
            let field = if let Some(name) = &arg.name {
                fields.iter().find(|field| &field.name == name)
            } else {
                next += 1;
                fields.get(next - 1)
            };
            if let Some(field) = field
                && !same_kit(arg)
            {
                self.private_member(owner, &field.name, arg.value.span);
            }
        }
    }
}
