//! Walking every expression of a module to read it. `visit` has the walk that changes them; this
//! one is the same walk, for what only looks, so nothing has to copy a module first. A test holds
//! that both reach the same expressions in the same order.

use crate::ast::{Expr, ExprKind, FStrPart, FnDef, Item, Module, Pattern, Stmt, StmtKind, TypeDef};

/// Calls `look` on every expression in `module`, outer ones before the ones inside them.
pub fn exprs<'a>(module: &'a Module, look: &mut dyn FnMut(&'a Expr)) {
    for item in &module.items {
        match item {
            Item::Function(def) => function(def, look),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => type_def(def, look),
            Item::Interface(interface) => functions(&interface.methods, look),
            Item::Extend(extension) => functions(&extension.methods, look),
            Item::Test(test) => stmts(&test.body, look),
            Item::Const { value, .. } => expr(value, look),
            Item::Stmt(stmt) => self::stmt(stmt, look),
            Item::Import(_) | Item::TypeAlias { .. } => {}
        }
    }
}

/// Calls `look` on every expression in a function's body.
pub fn in_function<'a>(def: &'a FnDef, look: &mut dyn FnMut(&'a Expr)) {
    function(def, look);
}

/// Calls `look` on an expression and every expression inside it.
pub fn in_expr<'a>(value: &'a Expr, look: &mut dyn FnMut(&'a Expr)) {
    expr(value, look);
}

/// Calls `look` on every expression in some statements.
pub fn in_stmts<'a>(body: &'a [Stmt], look: &mut dyn FnMut(&'a Expr)) {
    stmts(body, look);
}

fn type_def<'a>(def: &'a TypeDef, change: &mut dyn FnMut(&'a Expr)) {
    let fields = def.fields.iter().chain(def.cases.iter().flat_map(|case| case.fields.iter()));
    for field in fields {
        for value in field.default.iter().chain(field.rule.iter()) {
            expr(value, change);
        }
    }
    functions(&def.methods, change);
}

fn functions<'a>(defs: &'a [FnDef], change: &mut dyn FnMut(&'a Expr)) {
    for def in defs {
        function(def, change);
    }
}

fn function<'a>(def: &'a FnDef, change: &mut dyn FnMut(&'a Expr)) {
    stmts(&def.body, change);
}

fn stmts<'a>(body: &'a [Stmt], change: &mut dyn FnMut(&'a Expr)) {
    for stmt in body {
        self::stmt(stmt, change);
    }
}

fn stmt<'a>(stmt: &'a Stmt, change: &mut dyn FnMut(&'a Expr)) {
    match &stmt.kind {
        StmtKind::Expr(value)
        | StmtKind::Fail(value)
        | StmtKind::Yield(value)
        | StmtKind::Assert(value)
        | StmtKind::Return(Some(value)) => expr(value, change),
        StmtKind::Lend { value, otherwise, .. } => {
            expr(value, change);
            stmts(otherwise, change);
        }
        StmtKind::Assign { target, value, .. } => {
            expr(target, change);
            expr(value, change);
        }
        StmtKind::If { branches, otherwise } => {
            for (cond, body) in branches {
                expr(cond, change);
                stmts(body, change);
            }
            stmts(otherwise.as_deref().unwrap_or_default(), change);
        }
        StmtKind::While { cond: value, body }
        | StmtKind::For { iter: value, body, .. }
        | StmtKind::With { value, body, .. } => {
            expr(value, change);
            stmts(body, change);
        }
        StmtKind::OnFailure { cleanup, body, .. } => {
            stmts(cleanup, change);
            stmts(body, change);
        }
        StmtKind::Def(def) => function(def, change),
        StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
    }
}

fn expr<'a>(value: &'a Expr, change: &mut dyn FnMut(&'a Expr)) {
    change(value);
    match &value.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Duration(_)
        | ExprKind::Bytes(_)
        | ExprKind::Folder(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::None
        | ExprKind::Name(_)
        | ExprKind::Variant(_) => {}
        ExprKind::FStr(parts) => {
            for part in parts {
                if let FStrPart::Expr { expr: inner, .. } = part {
                    expr(inner, change);
                }
            }
        }
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => {
            for item in items {
                expr(item, change);
            }
        }
        ExprKind::Dict(pairs) => {
            for (key, item) in pairs {
                expr(key, change);
                expr(item, change);
            }
        }
        ExprKind::Comprehension { elem, value: item, iter, cond, .. } => {
            for inner in [elem, iter].into_iter().chain(item).chain(cond) {
                expr(inner, change);
            }
        }
        ExprKind::Unary { expr: inner, .. }
        | ExprKind::Attr { object: inner, .. }
        | ExprKind::Lambda { body: inner, .. } => expr(inner, change),
        ExprKind::Binary { lhs: first, rhs: second, .. }
        | ExprKind::Else { value: first, default: second, .. }
        | ExprKind::Index { object: first, index: second } => {
            expr(first, change);
            expr(second, change);
        }
        ExprKind::Cond { then, cond, otherwise } => {
            for inner in [then, cond, otherwise] {
                expr(inner, change);
            }
        }
        ExprKind::Try { value: inner, context } => {
            expr(inner, change);
            if let Some(context) = context {
                expr(context, change);
            }
        }
        ExprKind::Call { callee, args } => {
            expr(callee, change);
            for arg in args {
                expr(&arg.value, change);
            }
        }
        ExprKind::Slice { object, start, end } => {
            expr(object, change);
            for inner in start.iter().chain(end.iter()) {
                expr(inner, change);
            }
        }
        ExprKind::Match { subject, arms } => {
            expr(subject, change);
            for arm in arms {
                pattern(&arm.pattern, change);
                if let Some(guard) = &arm.guard {
                    expr(guard, change);
                }
                stmts(&arm.body, change);
            }
        }
    }
}

fn pattern<'a>(pattern: &'a Pattern, change: &mut dyn FnMut(&'a Expr)) {
    match pattern {
        Pattern::Literal(value) => expr(value, change),
        Pattern::Variant { args, .. } | Pattern::Tuple(args) | Pattern::Or(args) => {
            for arg in args {
                self::pattern(arg, change);
            }
        }
        Pattern::Wildcard | Pattern::Bind(_) | Pattern::RefBind(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use crate::Span;

    /// Every expression of the standard library, and of code with one of everything the walks
    /// have to get into, is reached by both, in the same order.
    #[test]
    fn reading_reaches_what_changing_reaches() {
        let source = "struct S:\n    a: int = 1 + 2\n    def m(self) -> int:\n        return self.a\n\n\
            enum E:\n    A(x: int = 3)\n\n\
            interface I:\n    def go(self) -> int\n\n\
            extend S:\n    def n(self) -> int:\n        return 5\n\n\
            LIMIT: int = 6\n\n\
            test \"t\":\n    assert 7 == 7\n\n\
            def f(xs: list[int]) -> int fails E:\n    \
            def inner() -> int:\n        return 8\n    \
            y = [x for x in xs if x > 0]\n    \
            z = {1: 2}\n    \
            g = a => a + 1\n    \
            w = f\"{y} {z}\"\n    \
            if xs[0] > 1:\n        return len(xs[1:2])\n    elif not xs:\n        pass\n    else:\n        pass\n    \
            while xs:\n        break\n    \
            for x in xs:\n        continue\n    \
            with open(1) as h:\n        lend h except:\n            pass\n    \
            v = 1 if xs else 2\n    \
            t = (1, 2)\n    \
            u = g(3)? \n    \
            match u:\n        case 1 | 2 if u > 0:\n            fail .A(x=1)\n        case _:\n            yield 2\n    \
            q = xs.get(0) else 1\n    return q\n";
        let module = crate::parse(source).unwrap();
        let modules = crate::prelude_modules().unwrap().iter().chain([&module]);
        let mut reached = 0;
        for module in modules {
            let mut read: Vec<Span> = Vec::new();
            super::exprs(module, &mut |expr| read.push(expr.span));
            let mut changed: Vec<Span> = Vec::new();
            crate::visit::exprs_mut(&mut module.clone(), &mut |expr| changed.push(expr.span));
            assert_eq!(read, changed);
            reached += read.len();
        }
        assert!(reached > 1000, "{reached}");
    }
}
