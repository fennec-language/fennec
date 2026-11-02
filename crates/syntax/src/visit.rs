//! Walking every expression of a module to change it in place, as the kit loader and the
//! checker do when they work out which enum a leading-dot case like `.Int(value)` belongs to.

use std::collections::HashSet;

use crate::ast::{
    BinOp, Expr, ExprKind, FStrPart, FnDef, Generic, Item, Module, Pattern, Stmt, StmtKind, TypeDef, UnaryOp,
};

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

/// The interfaces every type that can fits automatically.
pub const BUILT_IN_INTERFACES: [&str; 4] = ["Eq", "Hashable", "Ordered", "Format"];

/// The interfaces a set of modules declares, with the built-in ones.
pub fn interface_names<'m>(modules: impl IntoIterator<Item = &'m Module>) -> HashSet<String> {
    let declared = modules.into_iter().flat_map(|module| &module.items).filter_map(|item| match item {
        Item::Interface(interface) => Some(interface.name.clone()),
        _ => None,
    });
    declared.chain(BUILT_IN_INTERFACES.map(str::to_owned)).collect()
}

/// Gives each parameter whose type is an interface a type parameter of its own, bound by that
/// interface: `def save(mut out: Serializer)` becomes `def save[Serializer'1: Serializer](mut out:
/// Serializer'1)`. A call is then copied for each type it's given, like any generic, and the
/// hidden name can't be written, so nothing else can use it.
pub fn interface_params<S: std::hash::BuildHasher>(module: &mut Module, interfaces: &HashSet<String, S>) {
    for item in &mut module.items {
        let (methods, owner) = match item {
            Item::Function(def) => (std::slice::from_mut(def), &[][..]),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                (&mut def.methods[..], &def.generics[..])
            }
            Item::Interface(interface) => (&mut interface.methods[..], &[][..]),
            Item::Extend(extension) => (&mut extension.methods[..], &extension.generics[..]),
            _ => continue,
        };
        for def in methods {
            hide_interface_params(def, owner, interfaces);
        }
    }
}

fn hide_interface_params<S: std::hash::BuildHasher>(
    def: &mut FnDef,
    owner: &[Generic],
    interfaces: &HashSet<String, S>,
) {
    let in_scope =
        |def: &FnDef, name: &str| def.generics.iter().chain(owner).any(|generic| generic.name == name);
    let mut made = 0;
    for at in 0..def.params.len() {
        let Some(ty) = def.params.get(at).and_then(|param| param.ty.clone()) else { continue };
        if !ty.args.is_empty() || ty.optional || !interfaces.contains(&ty.name) || in_scope(def, &ty.name) {
            continue;
        }
        made += 1;
        let hidden = format!("{}{HIDDEN}{made}", ty.name);
        def.generics.push(Generic { name: hidden.clone(), bounds: vec![ty], span: def.span });
        if let Some(written) = def.params.get_mut(at).and_then(|param| param.ty.as_mut()) {
            written.name = hidden;
        }
    }
}

/// What marks a type parameter the program didn't write, which can't be written in source.
pub const HIDDEN: char = '\'';

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

/// Writes out the type of each field whose default makes a type with its type arguments, like
/// `IdMap[Unit]()` or `list[str]()`, for what reads a field's type from what's written.
pub fn type_fields_from_defaults(module: &mut Module) {
    for item in &mut module.items {
        let (Item::Struct(def) | Item::Error(def)) = item else { continue };
        for field in def.fields.iter_mut().filter(|field| field.ty.is_none()) {
            let Some(default) = &field.default else { continue };
            if let ExprKind::Call { callee, .. } = &default.kind
                && matches!(callee.kind, ExprKind::Index { .. })
            {
                field.ty = callee.as_type();
            }
        }
    }
}

/// Makes `list[str]()`, `dict[str, int]()` or `set[int]()` the empty `[]`, `{}` or `set()`, whose
/// type the checker has already worked out.
pub fn empty_collection(made: &mut Expr) {
    let ExprKind::Call { callee, .. } = &made.kind else { return };
    let ExprKind::Index { object, .. } = &callee.kind else { return };
    let ExprKind::Name(name) = &object.kind else { return };
    made.kind = match name.as_str() {
        "list" => ExprKind::List(Vec::new()),
        "dict" => ExprKind::Dict(Vec::new()),
        _ => ExprKind::Call {
            callee: Box::new(Expr { kind: ExprKind::Name("set".to_owned()), span: object.span }),
            args: Vec::new(),
        },
    };
}

/// Makes `a != b` into `not (a == b)`, for a type whose `def ==` decides both.
pub fn not_of_equals(compare: &mut Expr) {
    let ExprKind::Binary { op: op @ BinOp::NotEq, .. } = &mut compare.kind else { return };
    *op = BinOp::Eq;
    let equals = Expr { kind: compare.kind.clone(), span: compare.span };
    compare.kind = ExprKind::Unary { op: UnaryOp::Not, expr: Box::new(equals) };
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

/// Where a program first narrows a capability with `only`, if it does. Native code carries a
/// capability as its kind alone, so a narrowed one would quietly become the whole capability
/// again: until a capability carries what it allows natively, both backends refuse such a
/// program and it runs in the interpreter. A method of the program's own called `only` counts
/// too, which only costs speed.
pub fn calls_only(module: &Module) -> Option<crate::Span> {
    let mut found = None;
    let mut copy = module.clone();
    exprs_mut(&mut copy, &mut |expr| {
        if let ExprKind::Call { callee, .. } = &expr.kind
            && let ExprKind::Attr { name, .. } = &callee.kind
            && name == "only"
        {
            found = found.or(Some(expr.span));
        }
    });
    found
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
        | ExprKind::Duration(_)
        | ExprKind::Bytes(_)
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
