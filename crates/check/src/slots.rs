//! Code the checker writes in for a program, as `with launch`'s source or a tuple's saving
//! functions, is the same text every time, read as the standard library's, with a slot named
//! where each type goes. The checked types are put in afterwards, as written types built straight
//! from them, never as text: a name the checker holds that no source can write, as `Delivery'` for
//! a program's own `Delivery` beside the library's, comes through as it is, and nothing a program
//! names is ever read again.

use fennec_syntax::{Expr, ExprKind, FnDef, TypeExpr};

use crate::types::Type;

/// Puts each slot's type in, wherever a slot is in `def`: in its signature as a written type, and
/// in its code, as in `Sender[Message__slot](...)`, as the expression that names that type. `None`
/// when a type has no written form there.
pub(crate) fn fill(def: &mut FnDef, slots: &[(&str, Type)]) -> Option<()> {
    let mut whole = true;
    for ty in def
        .ret
        .iter_mut()
        .chain(def.fails.iter_mut())
        .chain(def.params.iter_mut().filter_map(|param| param.ty.as_mut()))
    {
        whole &= fill_type(ty, slots).is_some();
    }
    // A local's declared type, as `mut read_x: Type0__slot = None`.
    fennec_syntax::visit::written_types_in(&mut def.body, &mut |ty| whole &= fill_type(ty, slots).is_some());
    // A tuple type alone in brackets, as in `src.item[Item0__slot]()`, is written as a tuple of one
    // item, the tuple, as `[(int, str),]` is: a tuple there is otherwise read as the brackets'
    // several types.
    fennec_syntax::visit::in_function(def, &mut |expr| {
        if let ExprKind::Index { index, .. } = &mut expr.kind
            && let ExprKind::Name(name) = &index.kind
            && let Some((_, ty @ Type::Tuple(_))) = slots.iter().find(|(slot, _)| slot == name)
        {
            let span = index.span;
            match ty.written(span).and_then(|written| written.as_expr(span)) {
                Some(tuple) => **index = Expr { kind: ExprKind::Tuple(vec![tuple]), span },
                None => whole = false,
            }
        }
    });
    fennec_syntax::visit::in_function(def, &mut |expr| {
        if let ExprKind::Name(name) = &expr.kind
            && let Some((_, ty)) = slots.iter().find(|(slot, _)| slot == name)
        {
            match ty.written(expr.span).and_then(|written| written.as_expr(expr.span)) {
                Some(named) => *expr = named,
                None => whole = false,
            }
        }
    });
    whole.then_some(())
}

/// Puts each slot's type in where a slot is in a written type, keeping a `?` written after it.
pub(crate) fn fill_type(ty: &mut TypeExpr, slots: &[(&str, Type)]) -> Option<()> {
    if let Some((_, filled)) = slots.iter().find(|(slot, _)| *slot == ty.name) {
        let optional = ty.optional;
        *ty = filled.written(ty.span)?;
        ty.optional |= optional;
        return Some(());
    }
    ty.args.iter_mut().try_for_each(|arg| fill_type(arg, slots))
}

/// Puts `text` in for each string in `def` that's `slot`, as a type's name shown in a message.
pub(crate) fn fill_text(def: &mut FnDef, slot: &str, text: &str) {
    fennec_syntax::visit::in_function(def, &mut |expr| {
        if matches!(&expr.kind, ExprKind::Str(written) if written == slot) {
            expr.kind = ExprKind::Str(text.to_owned());
        }
    });
}
