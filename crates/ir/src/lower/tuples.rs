//! Saving and loading a tuple: `serialize` and `deserialize` on a tuple call the functions the
//! checker wrote for its type (see `fennec_check::tuple_function`), as generic code like
//! `JsonWriter.item[T]` reaches them once `T` is a tuple.

use fennec_check::Type;
use fennec_syntax::{Expr, ExprKind};

use super::Lowering;

impl Lowering<'_> {
    /// The function the checker wrote for what `callee` calls, when it's a tuple's `serialize`, as
    /// in `held.serialize(ref out)`, or `deserialize`, as in `T.deserialize(ref src)` with `T` a
    /// tuple: its key, and what it's called on, which it takes first.
    pub(super) fn tuple_saving<'e>(&self, callee: &'e Expr) -> Option<(String, Option<&'e Expr>)> {
        let ExprKind::Attr { object, name, optional: false } = &callee.kind else { return None };
        if !matches!(name.as_str(), "serialize" | "deserialize") {
            return None;
        }
        if let ExprKind::Name(param) = &object.kind
            && self.lookup(param).is_none()
            && let Some(tuple @ Type::Tuple(_)) = self.subst.get(param)
        {
            return Some((fennec_check::tuple_function(name, tuple), None));
        }
        let tuple = self.object_type(object).filter(|ty| matches!(ty, Type::Tuple(_)))?;
        Some((fennec_check::tuple_function(name, &tuple), Some(object.as_ref())))
    }
}
