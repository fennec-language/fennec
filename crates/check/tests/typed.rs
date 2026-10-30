//! The typed tree: every expression and every declared name in a program that checks has a
//! known type, found by the span of what it is or what declares it.

use std::fs;
use std::path::Path;

use fennec_check::{Checked, Type};
use fennec_syntax::{Expr, ExprKind, FStrPart, Item, Module, Pattern, Span, Stmt, StmtKind};

use crate::{dedent, list, named, optional};

/// Every example program, by file name.
fn examples() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut found: Vec<(String, String)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "fen"))
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            Some((name, fs::read_to_string(&path).ok()?))
        })
        .collect();
    found.sort();
    found
}

/// A program's syntax tree and what checking it works out, or its problems.
fn checked(source: &str) -> Result<(Module, Checked), Vec<String>> {
    let module = fennec_syntax::parse(source).map_err(|problem| vec![problem.message])?;
    let checked = fennec_check::check_typed(&module)
        .map_err(|problems| problems.into_iter().map(|problem| problem.message).collect::<Vec<_>>())?;
    Ok((module, checked))
}

/// The source text a span covers.
fn text(source: &str, span: Span) -> &str {
    source.get(span.start as usize..span.end as usize).unwrap_or("?")
}

/// The code of every function, method, nested function and test, by its key in the typed
/// tree, and the field defaults and rules of every type.
fn bodies(module: &Module) -> Vec<(String, Vec<Stmt>, Vec<Expr>)> {
    let mut found = Vec::new();
    for item in &module.items {
        match item {
            Item::Function(def) => nested(&def.name, &def.body, &mut found),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                for method in &def.methods {
                    nested(&format!("{}.{}", def.name, method.name), &method.body, &mut found);
                }
                let fields = def.fields.iter().flat_map(|field| field.default.iter().chain(&field.rule));
                found.push((format!("fields of {}", def.name), Vec::new(), fields.cloned().collect()));
            }
            Item::Test(test) => {
                found.push((format!("test \"{}\"", test.name), test.body.clone(), Vec::new()));
            }
            _ => {}
        }
    }
    found
}

fn nested(key: &str, body: &[Stmt], found: &mut Vec<(String, Vec<Stmt>, Vec<Expr>)>) {
    found.push((key.to_owned(), body.to_vec(), Vec::new()));
    for stmt in body {
        if let StmtKind::Def(def) = &stmt.kind {
            nested(&format!("{key}.{}", def.name), &def.body, found);
        }
    }
}

/// Every expression that's a value, in statements, leaving out nested functions.
fn values_in(stmts: &[Stmt], found: &mut Vec<Expr>) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Expr(expr) | StmtKind::Fail(expr) | StmtKind::Yield(expr) | StmtKind::Assert(expr) => {
                values(expr, found);
            }
            StmtKind::Return(value) => value.iter().for_each(|value| values(value, found)),
            StmtKind::Assign { target, value, .. } => {
                match &target.kind {
                    ExprKind::Tuple(names) => found.extend(names.iter().cloned()),
                    _ => values(target, found),
                }
                values(value, found);
            }
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    values(cond, found);
                    values_in(body, found);
                }
                values_in(otherwise.as_deref().unwrap_or_default(), found);
            }
            StmtKind::While { cond: expr, body }
            | StmtKind::For { iter: expr, body, .. }
            | StmtKind::With { value: expr, body, .. } => {
                values(expr, found);
                values_in(body, found);
            }
            StmtKind::Def(_) | StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
        }
    }
}

/// Whether an expression names a type, like `Vec3` in `Vec3.zero()`, or a module, like
/// `json` in `json.dumps(...)`.
fn is_type_name(expr: &Expr) -> bool {
    matches!(&expr.kind, ExprKind::Name(name) if name.starts_with(char::is_uppercase) || ["json", "toml", "path"].contains(&name.as_str()))
}

fn values(expr: &Expr, found: &mut Vec<Expr>) {
    found.push(expr.clone());
    let mut inner: Vec<&Expr> = Vec::new();
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            match &callee.kind {
                ExprKind::Attr { object, .. } if !is_type_name(object) => inner.push(object),
                ExprKind::Lambda { .. } => inner.push(callee),
                _ => {}
            }
            inner.extend(args.iter().map(|arg| &arg.value));
        }
        ExprKind::Attr { object, .. } if !is_type_name(object) => inner.push(object),
        ExprKind::Match { subject, arms } => {
            inner.push(subject);
            for arm in arms {
                if let Pattern::Literal(literal) = &arm.pattern {
                    inner.push(literal);
                }
                values_in(&arm.body, found);
            }
        }
        ExprKind::Lambda { body, .. } => inner.push(body),
        ExprKind::FStr(parts) => inner.extend(parts.iter().filter_map(|part| match part {
            FStrPart::Expr { expr, .. } => Some(expr),
            FStrPart::Text(_) => None,
        })),
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => inner.extend(items),
        ExprKind::Dict(pairs) => inner.extend(pairs.iter().flat_map(|(key, value)| [key, value])),
        ExprKind::Comprehension { elem, value, iter, cond, .. } => {
            inner.extend(
                [Some(elem.as_ref()), value.as_deref(), Some(iter.as_ref()), cond.as_deref()]
                    .into_iter()
                    .flatten(),
            );
        }
        ExprKind::Unary { expr, .. } => inner.push(expr),
        ExprKind::Binary { lhs: a, rhs: b, .. }
        | ExprKind::Else { value: a, default: b }
        | ExprKind::Index { object: a, index: b } => inner.extend([a.as_ref(), b.as_ref()]),
        ExprKind::Cond { then, cond, otherwise } => inner.extend([then.as_ref(), cond, otherwise]),
        ExprKind::Try { value, context } => {
            inner.extend(std::iter::once(value.as_ref()).chain(context.as_deref()));
        }
        ExprKind::Slice { object, start, end } => {
            inner.extend(std::iter::once(object.as_ref()).chain(start.as_deref()).chain(end.as_deref()));
        }
        _ => {}
    }
    for expr in inner {
        values(expr, found);
    }
}

#[test]
fn every_example_is_fully_typed() {
    let examples = examples();
    assert!(examples.len() > 10, "the examples weren't found");
    for (file, source) in examples {
        let (module, checked) = checked(&source).unwrap();
        for (name, signature) in checked.signatures.iter() {
            let types = signature.params.iter().map(|param| &param.ty).chain([&signature.returns]);
            for ty in types {
                assert!(ty.is_known(), "{file}: `{name}` has {ty:?} in its signature");
            }
        }
        for (key, body) in checked.types.iter() {
            for (span, ty) in body.exprs() {
                assert!(ty.is_known(), "{file}, {key}: `{}` is {ty:?}", text(&source, span));
            }
            for (_, name, ty) in body.names() {
                assert!(ty.is_known(), "{file}, {key}: `{name}` is {ty:?}");
            }
        }
        for (key, stmts, exprs) in bodies(&module) {
            let body = checked.types.body(&key).unwrap_or_else(|| panic!("{file}: nothing typed in {key}"));
            let mut found = Vec::new();
            values_in(&stmts, &mut found);
            for expr in &exprs {
                values(expr, &mut found);
            }
            for expr in found {
                assert!(
                    body.expr(expr.span).is_some(),
                    "{file}, {key}: `{}` has no type",
                    text(&source, expr.span)
                );
            }
        }
    }
}

#[test]
fn the_standard_library_is_fully_typed() {
    let (_, checked) = checked("def main(stdout: Stdout):\n    pass\n").unwrap();
    assert!(checked.types.body("Vec3.length").unwrap().exprs().count() > 0);
    for (key, body) in checked.types.iter() {
        for (_, ty) in body.exprs() {
            assert!(ty.is_known(), "{key}: {ty:?}");
        }
    }
}

/// The type of the first expression written as `text` in `function`.
fn type_of(source: &str, function: &str, text: &str) -> Option<Type> {
    let (_, checked) = checked(source).ok()?;
    let start = u32::try_from(source.find(text)?).ok()?;
    let end = start + u32::try_from(text.len()).ok()?;
    checked.types.body(function)?.expr(Span { start, end }).cloned()
}

#[test]
fn expressions_are_typed_where_they_are() {
    let source = dedent(
        "
        def main(stdout: Stdout):
            mut queue = []
            queue.append('first')
            mut target = None
            target = 3
            count = len(queue)
            stdout.print(queue, target is None, count)
            stdout.print(sorted(queue, key=name => len(name)))
        ",
    );
    assert_eq!(type_of(&source, "main", "[]"), Some(list(Type::Str)));
    assert_eq!(type_of(&source, "main", "queue.append(\"first\")"), Some(Type::Unit));
    assert_eq!(type_of(&source, "main", "len(queue)"), Some(Type::Int));
    let arrow = Type::Arrow(vec![Type::Str], Box::new(Type::Int));
    assert_eq!(type_of(&source, "main", "name => len(name)"), Some(arrow));
    assert_eq!(type_of(&source, "main", "queue"), Some(list(Type::Str)));
    assert_eq!(type_of(&source, "main", "target is None"), Some(Type::Bool));
}

#[test]
fn names_are_typed_where_they_are_declared() {
    let source = dedent(
        "
        def main(stdout: Stdout):
            mut target = None
            target = 3
            for i, word in enumerate(['a']):
                stdout.print(i, word)
        ",
    );
    let (_, checked) = checked(&source).unwrap();
    let body = checked.types.body("main").unwrap();
    let names: Vec<(&str, Type)> = body.names().map(|(_, name, ty)| (name, ty.clone())).collect();
    assert_eq!(
        names,
        [("stdout", named("Stdout")), ("target", optional(Type::Int)), ("i", Type::Int), ("word", Type::Str)]
    );
    let start = u32::try_from(source.find("target").unwrap()).unwrap();
    assert_eq!(body.name(Span { start, end: start + 6 }, "target"), Some(&optional(Type::Int)));
}

#[test]
fn generic_calls_are_typed_with_their_type_filled_in() {
    let source = dedent(
        "
        def first[T](items: list[T]) -> T:
            items[0]

        def main(stdout: Stdout):
            stdout.print(first(['a']), first([1.5]))
        ",
    );
    assert_eq!(type_of(&source, "main", "first([\"a\"])"), Some(Type::Str));
    assert_eq!(type_of(&source, "main", "first([1.5])"), Some(Type::Float));
    assert_eq!(type_of(&source, "first", "items[0]"), Some(Type::Param("T".to_owned())));
}
