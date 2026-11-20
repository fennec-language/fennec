//! Moving every span of a declaration by one amount. A declaration an edit moved without changing
//! it, set where it is now, equals what the program declares there now: that's how a check after
//! an edit tells a declaration that only moved from one that changed (`check_to_build_keeping`).
//!
//! Each struct is named in full, so a span added to one later can't be left where it was.

use fennec_syntax::{
    Annotation, Arg, Case, Expr, Extension, Field, FnDef, Generic, Import, Interface, Item, Param, Span,
    Stmt, StmtKind, TypeDef, TypeExpr,
};

/// `item` with every span in it moved through `moved`. A function's or a test's is left as it is:
/// they're compared as they are.
pub(crate) fn moved_item(item: &Item, moved: &mut dyn FnMut(Span) -> Span) -> Item {
    let mut item = item.clone();
    match &mut item {
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => type_def(def, moved),
        Item::Interface(interface) => {
            let Interface { name: _, methods, span } = interface;
            for method in methods.iter_mut() {
                fn_def(method, moved);
            }
            *span = moved(*span);
        }
        Item::Extend(extension) => {
            let Extension { name: _, generics, methods, span } = extension;
            for generic in generics.iter_mut() {
                self::generic(generic, moved);
            }
            for method in methods.iter_mut() {
                fn_def(method, moved);
            }
            *span = moved(*span);
        }
        Item::TypeAlias { name: _, ty, span } => {
            type_expr(ty, moved);
            *span = moved(*span);
        }
        Item::Const { name: _, ty, value, span } => {
            for ty in ty.iter_mut() {
                type_expr(ty, moved);
            }
            expr(value, moved);
            *span = moved(*span);
        }
        Item::Import(import) => {
            let Import { module: _, names: _, aliases: _, alias: _, span } = import;
            *span = moved(*span);
        }
        Item::Function(_) | Item::Test(_) | Item::Stmt(_) => {}
    }
    item
}

fn type_def(def: &mut TypeDef, moved: &mut dyn FnMut(Span) -> Span) {
    let TypeDef { name: _, generics, fields, cases, methods, shared: _, span } = def;
    for generic in generics.iter_mut() {
        self::generic(generic, moved);
    }
    for field in fields.iter_mut() {
        self::field(field, moved);
    }
    for case in cases {
        let Case { name: _, fields, span } = case;
        for field in fields.iter_mut() {
            self::field(field, moved);
        }
        *span = moved(*span);
    }
    for method in methods.iter_mut() {
        fn_def(method, moved);
    }
    *span = moved(*span);
}

fn field(field: &mut Field, moved: &mut dyn FnMut(Span) -> Span) {
    let Field { name: _, ty, default, rule, annotations, span } = field;
    for ty in ty.iter_mut() {
        type_expr(ty, moved);
    }
    for value in default.iter_mut().chain(rule.iter_mut()) {
        expr(value, moved);
    }
    for annotation in annotations.iter_mut() {
        self::annotation(annotation, moved);
    }
    *span = moved(*span);
}

fn annotation(annotation: &mut Annotation, moved: &mut dyn FnMut(Span) -> Span) {
    let Annotation { name: _, args, span } = annotation;
    for arg in args {
        let Arg { name: _, is_mut: _, value } = arg;
        expr(value, moved);
    }
    *span = moved(*span);
}

fn fn_def(def: &mut FnDef, moved: &mut dyn FnMut(Span) -> Span) {
    let FnDef { name: _, generics, params, ret, arrow, fails, body, annotations, span } = def;
    for generic in generics.iter_mut() {
        self::generic(generic, moved);
    }
    for param in params {
        let Param { name: _, ty, is_mut: _, is_self: _, default, span } = param;
        for ty in ty.iter_mut() {
            type_expr(ty, moved);
        }
        for value in default.iter_mut() {
            expr(value, moved);
        }
        *span = moved(*span);
    }
    for ty in ret.iter_mut().chain(fails.iter_mut()) {
        type_expr(ty, moved);
    }
    if let Some(arrow) = arrow {
        *arrow = moved(*arrow);
    }
    crate::lend_lowering::every_span(body, moved);
    for annotation in annotations.iter_mut() {
        self::annotation(annotation, moved);
    }
    *span = moved(*span);
}

fn generic(generic: &mut Generic, moved: &mut dyn FnMut(Span) -> Span) {
    let Generic { name: _, bounds, span } = generic;
    for bound in bounds.iter_mut() {
        type_expr(bound, moved);
    }
    *span = moved(*span);
}

fn type_expr(ty: &mut TypeExpr, moved: &mut dyn FnMut(Span) -> Span) {
    let TypeExpr { name: _, args, optional: _, span } = ty;
    for arg in args.iter_mut() {
        type_expr(arg, moved);
    }
    *span = moved(*span);
}

/// An expression's spans, and those of the statements and types written inside it, as a copy's
/// are moved (`lend_lowering::every_span`).
fn expr(value: &mut Expr, moved: &mut dyn FnMut(Span) -> Span) {
    let mut probe = [Stmt { kind: StmtKind::Expr(value.clone()), span: value.span }];
    crate::lend_lowering::every_span(&mut probe, moved);
    if let [Stmt { kind: StmtKind::Expr(moved_value), .. }] = probe {
        *value = moved_value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A declaration moved by an edit above it equals its new self once moved; one that changed
    /// doesn't.
    #[test]
    fn a_moved_declaration_equals_itself_once_moved_and_a_changed_one_doesnt() {
        let before = "struct Dot:\n    x: int = 1 + 2\n    y = list[int]()\n\nLIMIT: int = 3\n";
        let item = |source: &str, at: usize| {
            fennec_syntax::parse(source).ok().and_then(|module| module.items.get(at).cloned())
        };
        let after = format!("# a line\n{before}");
        let by = 9;
        for at in 0..2 {
            let (was, is) = (item(before, at), item(&after, at));
            let shifted = was
                .as_ref()
                .map(|was| moved_item(was, &mut |span| Span { start: span.start + by, end: span.end + by }));
            assert_eq!(shifted, is, "item {at}");
        }
        let changed = format!("# a line\n{}", before.replace("x: int", "x: str"));
        let was = item(before, 0)
            .map(|was| moved_item(&was, &mut |span| Span { start: span.start + by, end: span.end + by }));
        assert_ne!(was, item(&changed, 0));
    }
}
