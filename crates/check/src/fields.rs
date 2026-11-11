//! `for f in fields(T):`, a loop over a type's fields that's unrolled when each copy of a generic
//! function is made, and checked once, as the generic function is. `f.name` is the field's name,
//! and `f.get(x)`, of an `x` of type `T`, is the field's value, of a type of its own that stands
//! for each field's: a type parameter with no bound, so it goes only where any type can.

use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, Span, Stmt, TypeExpr};

use crate::function::{FnCx, Origin};
use crate::types::Type;

/// The type a loop over `fields(T)` gives its name, holding `T` and the type of each field.
#[must_use]
pub(crate) fn field_type_name() -> String {
    format!("Field{}", fennec_syntax::visit::HIDDEN)
}

/// The name of the type that stands for each field's in the loop starting at `start`, the
/// start of what it loops over, which lowering gives each field's type in turn.
#[must_use]
pub fn each_field_type(start: u32) -> String {
    format!("FieldValue{}{start}", fennec_syntax::visit::HIDDEN)
}

/// What `for f in fields(T):` loops over: `T`, as it's written.
#[must_use]
pub fn fields_of(iter: &Expr) -> Option<&str> {
    let ExprKind::Call { callee, args } = &iter.kind else { return None };
    let ExprKind::Name(called) = &callee.kind else { return None };
    match args.as_slice() {
        [Arg { name: None, is_mut: false, value: Expr { kind: ExprKind::Name(of), .. } }]
            if called == "fields" =>
        {
            Some(of)
        }
        _ => None,
    }
}

/// What a field a loop over `fields(T)` gives has.
const FIELD_HAS: &str = "write `field.name` for its name, or `field.get(value)` for its value in `value`";

impl FnCx<'_> {
    /// `for f in fields(T):`, when `fields` is the language's own: `None` otherwise.
    pub(crate) fn fields_loop(
        &mut self,
        names: &[String],
        iter: &Expr,
        body: &[Stmt],
        span: Span,
    ) -> Option<()> {
        let of = fields_of(iter)?;
        if self.lookup("fields").is_some() || self.checker.program.functions.contains_key("fields") {
            return None;
        }
        let [name] = names else {
            let help = format!("name each field once, as `for field in fields({of}):`");
            let message = "a loop over `fields(...)` takes one name for each field";
            self.error(Diagnostic::new(message, span).with_help(help));
            return Some(());
        };
        let written = TypeExpr { name: of.to_owned(), args: Vec::new(), optional: false, span: iter.span };
        let Some(ty) = self.written_type(&written) else {
            self.error(crate::program::unknown_type(of, iter.span));
            return Some(());
        };
        let field = Type::Generic(field_type_name(), vec![ty, Type::Param(each_field_type(iter.span.start))]);
        let before = self.snapshot();
        self.push(false);
        self.declare_as(name, field, false, Origin::Loop, span);
        self.stmts(body, false);
        self.pop();
        let after = self.snapshot();
        self.merge(&[before, after]);
        Some(())
    }

    /// `f.name`, of a field a loop over `fields(T)` gives.
    pub(crate) fn field_attr(&mut self, ty: &Type, name: &str, span: Span) -> Option<Type> {
        let Type::Generic(owner, _) = ty else { return None };
        if *owner != field_type_name() {
            return None;
        }
        if name == "name" {
            return Some(Type::Str);
        }
        let message = format!("a field has a `name` and a `get(...)`, not `{name}`");
        self.error(Diagnostic::new(message, span).with_help(FIELD_HAS));
        Some(Type::Unknown)
    }

    /// `f.get(x)`, the value of the field a loop over `fields(T)` gives, in an `x` of type `T`.
    pub(crate) fn field_get(&mut self, ty: &Type, method: &str, args: &[Arg], span: Span) -> Option<Type> {
        let Type::Generic(owner, parts) = ty else { return None };
        if *owner != field_type_name() {
            return None;
        }
        let [of, each] = parts.as_slice() else { return None };
        let (of, each) = (of.clone(), each.clone());
        if method != "get" {
            let message = format!("a field has a `name` and a `get(...)`, not `{method}`");
            self.error(Diagnostic::new(message, span).with_help(FIELD_HAS));
            return Some(Type::Unknown);
        }
        let [Arg { name: None, is_mut: false, value }] = args else {
            let help = "pass the value whose field it reads, as `field.get(value)`";
            self.error(Diagnostic::new("`get` takes the value to read the field of", span).with_help(help));
            return Some(Type::Unknown);
        };
        let given = self.expr(value);
        if !self.checker.fits(&of, &given) {
            let message =
                format!("`get` reads a field of {}, not {}", crate::types::a(&of), crate::types::a(&given));
            let help = format!("pass {}, the type the loop is over", crate::types::a(&of));
            self.error(Diagnostic::new(message, value.span).with_help(help));
        }
        Some(each)
    }
}
