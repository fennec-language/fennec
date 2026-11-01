//! Walking every expression of a module to change it in place, as the kit loader and the
//! checker do when they work out which enum a leading-dot case like `.Int(value)` belongs to.

use crate::ast::{Expr, ExprKind, FStrPart, FnDef, Item, Module, Pattern, Stmt, StmtKind, TypeDef};

/// Calls `change` on every expression in `module`, outer ones before the ones inside them.
pub fn exprs_mut(module: &mut Module, change: &mut dyn FnMut(&mut Expr)) {
    for item in &mut module.items {
        match item {
            Item::Function(def) => function(def, change),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => type_def(def, change),
            Item::Interface(interface) => functions(&mut interface.methods, change),
            Item::Extend(extension) => functions(&mut extension.methods, change),
            Item::Test(test) => stmts(&mut test.body, change),
            Item::Const { value, .. } => expr(value, change),
            Item::Stmt(stmt) => self::stmt(stmt, change),
            Item::Import(_) | Item::TypeAlias { .. } => {}
        }
    }
}

/// Calls `edit` on every function and method in a module.
pub fn functions_mut(module: &mut Module, edit: &mut dyn FnMut(&mut FnDef)) {
    for item in &mut module.items {
        match item {
            Item::Function(def) => edit(def),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                def.methods.iter_mut().for_each(&mut *edit);
            }
            Item::Extend(extension) => extension.methods.iter_mut().for_each(&mut *edit),
            _ => {}
        }
    }
}

/// Calls `change` on an expression and every expression inside it.
pub fn in_expr(value: &mut Expr, change: &mut dyn FnMut(&mut Expr)) {
    expr(value, change);
}

/// Calls `change` on every expression in a function's body.
pub fn in_function(def: &mut FnDef, change: &mut dyn FnMut(&mut Expr)) {
    function(def, change);
}

/// Calls `change` on every expression in some statements.
pub fn in_stmts(body: &mut [Stmt], change: &mut dyn FnMut(&mut Expr)) {
    stmts(body, change);
}

/// Makes a leading-dot case name its enum: `.Int(value)` becomes `ExprKind.Int(value)`.
/// Anything but a leading-dot case is left as it is.
pub fn name_enum(case: &mut Expr, owner: &str) {
    let ExprKind::Variant(name) = &mut case.kind else { return };
    let name = std::mem::take(name);
    // The enum's name isn't written, so it covers no source: just where the dot is.
    let at = crate::Span { start: case.span.start, end: case.span.start };
    let object = Box::new(Expr { kind: ExprKind::Name(owner.to_owned()), span: at });
    case.kind = ExprKind::Attr { object, name, optional: false };
}

/// Writes the defaults a call leaves out into it, by name.
pub fn add_defaults(call: &mut Expr, defaults: &[(String, Expr)]) {
    if let ExprKind::Call { args, .. } = &mut call.kind {
        for (name, default) in defaults {
            let at = crate::Span { start: call.span.end, end: call.span.end };
            let value = Expr { kind: default.kind.clone(), span: at };
            args.push(crate::ast::Arg { name: Some(name.clone()), is_mut: false, value });
        }
    }
}

/// Gives a call to `sum` of floats its start: `sum(xs)` becomes `sum(xs, start=0.0)`.
pub fn add_float_start(call: &mut Expr) {
    let at = crate::Span { start: call.span.end, end: call.span.end };
    if let ExprKind::Call { args, .. } = &mut call.kind {
        let value = Expr { kind: ExprKind::Float(0.0), span: at };
        args.push(crate::ast::Arg { name: Some("start".to_owned()), is_mut: false, value });
    }
}

/// Gives a generator expression the type printing it shows, in the slot only dict
/// comprehensions otherwise use.
pub fn name_generator_type(comprehension: &mut Expr, shown: &str) {
    let span = comprehension.span;
    if let ExprKind::Comprehension { value, .. } = &mut comprehension.kind {
        *value = Some(Box::new(Expr { kind: ExprKind::Str(shown.to_owned()), span }));
    }
}

fn type_def(def: &mut TypeDef, change: &mut dyn FnMut(&mut Expr)) {
    let fields = def.fields.iter_mut().chain(def.cases.iter_mut().flat_map(|case| case.fields.iter_mut()));
    for field in fields {
        for value in field.default.iter_mut().chain(field.rule.iter_mut()) {
            expr(value, change);
        }
    }
    functions(&mut def.methods, change);
}

fn functions(defs: &mut [FnDef], change: &mut dyn FnMut(&mut Expr)) {
    for def in defs {
        function(def, change);
    }
}

fn function(def: &mut FnDef, change: &mut dyn FnMut(&mut Expr)) {
    stmts(&mut def.body, change);
}

fn stmts(body: &mut [Stmt], change: &mut dyn FnMut(&mut Expr)) {
    for stmt in body {
        self::stmt(stmt, change);
    }
}

fn stmt(stmt: &mut Stmt, change: &mut dyn FnMut(&mut Expr)) {
    match &mut stmt.kind {
        StmtKind::Expr(value)
        | StmtKind::Fail(value)
        | StmtKind::Yield(value)
        | StmtKind::Assert(value)
        | StmtKind::Return(Some(value)) => expr(value, change),
        StmtKind::Assign { target, value, .. } => {
            expr(target, change);
            expr(value, change);
        }
        StmtKind::If { branches, otherwise } => {
            for (cond, body) in branches {
                expr(cond, change);
                stmts(body, change);
            }
            stmts(otherwise.as_deref_mut().unwrap_or_default(), change);
        }
        StmtKind::While { cond: value, body }
        | StmtKind::For { iter: value, body, .. }
        | StmtKind::With { value, body, .. } => {
            expr(value, change);
            stmts(body, change);
        }
        StmtKind::Def(def) => function(def, change),
        StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
    }
}

fn expr(value: &mut Expr, change: &mut dyn FnMut(&mut Expr)) {
    change(value);
    match &mut value.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
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
        | ExprKind::Else { value: first, default: second }
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
                expr(&mut arg.value, change);
            }
        }
        ExprKind::Slice { object, start, end } => {
            expr(object, change);
            for inner in start.iter_mut().chain(end.iter_mut()) {
                expr(inner, change);
            }
        }
        ExprKind::Match { subject, arms } => {
            expr(subject, change);
            for arm in arms {
                pattern(&mut arm.pattern, change);
                if let Some(guard) = &mut arm.guard {
                    expr(guard, change);
                }
                stmts(&mut arm.body, change);
            }
        }
    }
}

fn pattern(pattern: &mut Pattern, change: &mut dyn FnMut(&mut Expr)) {
    match pattern {
        Pattern::Literal(value) => expr(value, change),
        Pattern::Variant { args, .. } | Pattern::Tuple(args) | Pattern::Or(args) => {
            for arg in args {
                self::pattern(arg, change);
            }
        }
        Pattern::Wildcard | Pattern::Bind(_) => {}
    }
}
