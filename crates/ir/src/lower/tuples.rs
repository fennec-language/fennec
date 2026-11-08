//! Saving and loading a tuple: `serialize` and `deserialize` on a tuple call the functions the
//! checker wrote for its type (see `fennec_check::tuple_function`), as generic code like
//! `JsonWriter.item[T]` reaches them once `T` is a tuple.

use fennec_check::Type;
use fennec_syntax::{Expr, ExprKind};

use super::Lowering;

impl Lowering<'_> {
    /// The function the checker wrote for what `callee` calls, when it's a tuple's `serialize`, as
    /// in `held.serialize(ref out)`, or `deserialize`, as in `T.deserialize(ref src)` with `T` a
    /// tuple: its key, what it's called on, which it takes first, and the tuple's type.
    pub(super) fn tuple_saving<'e>(&self, callee: &'e Expr) -> Option<(String, Option<&'e Expr>, Type)> {
        let ExprKind::Attr { object, name, optional: false } = &callee.kind else { return None };
        if !matches!(name.as_str(), "serialize" | "deserialize") {
            return None;
        }
        if let ExprKind::Name(param) = &object.kind
            && self.lookup(param).is_none()
            && let Some(tuple @ Type::Tuple(_)) = self.subst.get(param)
        {
            return Some((fennec_check::tuple_function(name, tuple), None, tuple.clone()));
        }
        let tuple = self.object_type(object).filter(|ty| matches!(ty, Type::Tuple(_)))?;
        Some((fennec_check::tuple_function(name, &tuple), Some(object.as_ref()), tuple))
    }

    /// `tuple_saving`'s function and what it's called on, or why there's none: the checker writes
    /// one for each tuple type reached from what the program saves.
    pub(super) fn tuple_saved<'e>(
        &self,
        callee: &'e Expr,
    ) -> Option<Result<(String, Option<&'e Expr>), String>> {
        let (key, object, tuple) = self.tuple_saving(callee)?;
        if self.context.signatures.get(&key).is_none() {
            return Some(Err(format!("there's no saving for the tuple `{tuple}`")));
        }
        Some(Ok((key, object)))
    }
}
