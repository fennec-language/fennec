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

/// Calls `change` on every expression in a module, with the names the function it's in binds
/// itself, which shadow anything of the same name outside it: its parameters, and every local,
/// loop, `with`, lambda, comprehension and pattern name in it. A test's are its own too, and
/// outside a function there are none.
pub fn exprs_in_scope_mut<S: std::hash::BuildHasher + Default>(
    module: &mut Module,
    change: &mut dyn FnMut(&mut Expr, &HashSet<String, S>),
) {
    let none = HashSet::default();
    for item in &mut module.items {
        match item {
            Item::Function(def) => scoped(def, change),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                let fields =
                    def.fields.iter_mut().chain(def.cases.iter_mut().flat_map(|case| case.fields.iter_mut()));
                for field in fields {
                    for value in field.default.iter_mut().chain(field.rule.iter_mut()) {
                        expr(value, &mut |found| change(found, &none));
                    }
                }
                def.methods.iter_mut().for_each(|def| scoped(def, change));
            }
            Item::Interface(interface) => interface.methods.iter_mut().for_each(|def| scoped(def, change)),
            Item::Extend(extension) => extension.methods.iter_mut().for_each(|def| scoped(def, change)),
            Item::Test(test) => {
                let mut bound: HashSet<String, S> =
                    test.capabilities.iter().chain(&test.inputs).map(|param| param.name.clone()).collect();
                bound_in(&test.body, &mut bound);
                stmts(&mut test.body, &mut |found| change(found, &bound));
            }
            Item::Const { value, .. } => expr(value, &mut |found| change(found, &none)),
            Item::Stmt(each) => stmt(each, &mut |found| change(found, &none)),
            Item::Import(_) | Item::TypeAlias { .. } => {}
        }
    }
}

/// A function's expressions, each with the names the function binds.
fn scoped<S: std::hash::BuildHasher + Default>(
    def: &mut FnDef,
    change: &mut dyn FnMut(&mut Expr, &HashSet<String, S>),
) {
    let mut bound: HashSet<String, S> = def.params.iter().map(|param| param.name.clone()).collect();
    bound_in(&def.body, &mut bound);
    function(def, &mut |found| change(found, &bound));
}

/// Every name some statements bind, in them and in what's inside them.
fn bound_in<S: std::hash::BuildHasher>(body: &[Stmt], bound: &mut HashSet<String, S>) {
    for each in body {
        match &each.kind {
            StmtKind::Assign { target, .. } => bound_target(target, bound),
            StmtKind::For { names, body, .. } => {
                bound.extend(names.iter().cloned());
                bound_in(body, bound);
            }
            StmtKind::With { names, body, .. } => {
                bound.extend(names.iter().map(|held| held.name.clone()));
                bound_in(body, bound);
            }
            StmtKind::OnFailure { name, cleanup, body } => {
                bound.insert(name.clone());
                bound_in(cleanup, bound);
                bound_in(body, bound);
            }
            StmtKind::While { body, .. } => bound_in(body, bound),
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    bound_in(body, bound);
                }
                if let Some(body) = otherwise {
                    bound_in(body, bound);
                }
            }
            StmtKind::Lend { otherwise, .. } => bound_in(otherwise, bound),
            StmtKind::Def(def) => {
                bound.insert(def.name.clone());
                bound.extend(def.params.iter().map(|param| param.name.clone()));
                bound_in(&def.body, bound);
            }
            _ => {}
        }
    }
    // What expressions bind: a lambda's parameters, a comprehension's names, a case's pattern.
    crate::read::in_stmts(body, &mut |found| match &found.kind {
        ExprKind::Lambda { params, .. } => bound.extend(params.iter().cloned()),
        ExprKind::Comprehension { names, .. } => bound.extend(names.iter().cloned()),
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                bound_pattern(&arm.pattern, bound);
                bound_in(&arm.body, bound);
            }
        }
        _ => {}
    });
}

fn bound_target<S: std::hash::BuildHasher>(target: &Expr, bound: &mut HashSet<String, S>) {
    match &target.kind {
        ExprKind::Name(name) => {
            bound.insert(name.clone());
        }
        ExprKind::Tuple(items) => items.iter().for_each(|item| bound_target(item, bound)),
        _ => {}
    }
}

fn bound_pattern<S: std::hash::BuildHasher>(pattern: &Pattern, bound: &mut HashSet<String, S>) {
    match pattern {
        Pattern::Bind(name) | Pattern::RefBind(name) => {
            bound.insert(name.clone());
        }
        Pattern::Variant { args: patterns, .. } | Pattern::Tuple(patterns) | Pattern::Or(patterns) => {
            for pattern in patterns {
                bound_pattern(pattern, bound);
            }
        }
        Pattern::Wildcard | Pattern::Literal(_) => {}
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

/// Changes every span in some statements through `moved`: each statement's, each expression's,
/// each `with` name's, each `match` case's, and each nested function's and its parameters', so a
/// copy of them can be given spans of its own. Each span is changed once.
pub fn respan_stmts(body: &mut [Stmt], moved: &mut dyn FnMut(crate::Span) -> crate::Span) {
    stmt_spans(body, moved);
    stmts(body, &mut |value| {
        value.span = moved(value.span);
        if let ExprKind::Match { arms, .. } = &mut value.kind {
            for arm in arms {
                arm.span = moved(arm.span);
                stmt_spans(&mut arm.body, moved);
            }
        }
    });
}

/// The spans of statements and what they declare, not their expressions': those inside a
/// `match` are reached through it.
fn stmt_spans(body: &mut [Stmt], moved: &mut dyn FnMut(crate::Span) -> crate::Span) {
    for each in body {
        each.span = moved(each.span);
        match &mut each.kind {
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    stmt_spans(body, moved);
                }
                stmt_spans(otherwise.as_deref_mut().unwrap_or_default(), moved);
            }
            StmtKind::With { names, body, .. } => {
                for name in names {
                    name.span = moved(name.span);
                }
                stmt_spans(body, moved);
            }
            StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::Lend { otherwise: body, .. } => {
                stmt_spans(body, moved);
            }
            StmtKind::OnFailure { cleanup, body, .. } => {
                stmt_spans(cleanup, moved);
                stmt_spans(body, moved);
            }
            StmtKind::Def(def) => {
                def.span = moved(def.span);
                for param in &mut def.params {
                    param.span = moved(param.span);
                }
                stmt_spans(&mut def.body, moved);
            }
            StmtKind::Expr(_)
            | StmtKind::Assign { .. }
            | StmtKind::Return(_)
            | StmtKind::Fail(_)
            | StmtKind::Yield(_)
            | StmtKind::Assert(_)
            | StmtKind::Break
            | StmtKind::Continue
            | StmtKind::Pass => {}
        }
    }
}

/// Writes out the type of each field whose default makes a type with its type arguments, like
/// `Table[Unit]()` or `list[str]()`, for what reads a field's type from what's written.
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

/// Makes `array[float, 16]()` into `array.new[float, 16](0.0)`: the builtin that fills an
/// array with copies of its element's zero. Nothing written can name it.
pub fn filled_array(made: &mut Expr, zero: &Expr) {
    let ExprKind::Call { callee, args } = &mut made.kind else { return };
    let ExprKind::Index { object, .. } = &mut callee.kind else { return };
    object.kind = ExprKind::Name("array.new".to_owned());
    args.push(crate::ast::Arg { name: None, is_mut: false, value: zero.clone() });
}

/// Makes `a != b` into `not (a == b)`, for a type whose `def ==` decides both.
pub fn not_of_equals(compare: &mut Expr) {
    let ExprKind::Binary { op: op @ BinOp::NotEq, .. } = &mut compare.kind else { return };
    *op = BinOp::Eq;
    let equals = Expr { kind: compare.kind.clone(), span: compare.span };
    compare.kind = ExprKind::Unary { op: UnaryOp::Not, expr: Box::new(equals) };
}

/// `value.step`, where the value can fail, in a chain an `else` ends: reads `(value else None).step`,
/// so the chain stops there. Nothing's written for what's added, so its span is empty.
pub fn none_on_failure(step: &mut Expr) {
    let ExprKind::Attr { object, .. } = &mut step.kind else { return };
    let at = crate::Span { start: object.span.end, end: object.span.end };
    let value = std::mem::replace(object.as_mut(), Expr { kind: ExprKind::None, span: at });
    let default = Box::new(Expr { kind: ExprKind::None, span: at });
    **object = Expr { kind: ExprKind::Else { value: Box::new(value), default }, span: at };
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

/// Writes the defaults a call leaves out into it, by name. Each keeps the span it has where its
/// parameter is declared, which the checker gives its type in the calling function: two defaults
/// can't share one span, or one's type would stand for both.
pub fn add_defaults(call: &mut Expr, defaults: &[(String, Expr)]) {
    if let ExprKind::Call { args, .. } = &mut call.kind {
        for (name, default) in defaults {
            // A source of the program's own, written in where it's used, keeps its text's places,
            // so a default found for the copy is found for the text it came from too, which has
            // it already.
            if args.iter().any(|arg| arg.name.as_deref() == Some(name.as_str())) {
                continue;
            }
            args.push(crate::ast::Arg { name: Some(name.clone()), is_mut: false, value: default.clone() });
        }
    }
}

/// Where a program first narrows a capability with `only`, if it does. Native code carries a
/// capability as its kind alone, so a narrowed one would quietly become the whole capability
/// again: until a capability carries what it allows natively, LLVM refuses such a
/// program. A method of the program's own called `only` counts
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

/// Writes `start=0` into a `sum` call, for a sum of integers of a size that's empty.
pub fn add_int_start(call: &mut Expr) {
    let at = crate::Span { start: call.span.end, end: call.span.end };
    if let ExprKind::Call { args, .. } = &mut call.kind {
        let value = Expr { kind: ExprKind::Int(0), span: at };
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
        StmtKind::Lend { value, otherwise } => {
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
            stmts(otherwise.as_deref_mut().unwrap_or_default(), change);
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

fn expr(value: &mut Expr, change: &mut dyn FnMut(&mut Expr)) {
    change(value);
    match &mut value.kind {
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
        Pattern::Wildcard | Pattern::Bind(_) | Pattern::RefBind(_) => {}
    }
}
