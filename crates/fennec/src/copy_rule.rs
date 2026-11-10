//! What a program costs that it must not: anything that may allocate or copy in a `@hot`
//! function or what it calls, and any check native code keeps there apart from overflow. Changing
//! a value something else still holds copies it first; anywhere else, that's a hint, since every
//! copy is a snapshot as the program says, but a hot path refuses it.

use fennec_syntax::{Diagnostic, Module, Sources};

/// Anything a resolved module may allocate or copy in a `@hot` function or what it calls, or any check native code keeps there
/// apart from overflow, and each call whose positional arguments are easy to get backwards.
pub(crate) fn implicit_copies(
    module: &Module,
    signatures: &fennec_check::Signatures,
    sources: &Sources,
) -> Vec<Diagnostic> {
    let mut problems = Vec::new();
    problems.extend(positional_arguments(module, sources));
    let hot = hot_functions(module);
    // What's lowered here is only for `@hot` functions and what they call, so a program with
    // none isn't lowered for it: a build lowers it anyway.
    if hot.is_empty() {
        return problems;
    }
    let lowered = fennec_ir::lower(module, signatures);
    let findings = fennec_ir::costs(&lowered);
    let keys: Vec<String> = hot.iter().map(|(key, _)| key.clone()).collect();
    let reach = fennec_ir::reached_from_hot(&lowered, &keys);
    problems.extend(fennec_lint::hot_costs(&findings, &reach));
    problems.extend(checks_kept(&fennec_llvm::checks_left(&lowered), &reach));
    for (key, span) in hot {
        let base = |name: &str| name.split_once('[').map_or(name, |(base, _)| base).to_owned();
        if let Some(missing) = lowered.not_lowered.iter().find(|missing| base(&missing.function) == key) {
            let message = format!(
                "`@hot` `{key}` can't be checked yet, since it isn't built natively: {}",
                missing.why
            );
            problems.push(Diagnostic::new(message, span).with_help("take `@hot` off until it is"));
        }
    }
    problems
}

/// Each check native code keeps in a `@hot` function or what it calls: one the compiler couldn't
/// show can't fail, like an index it can't place inside its list. An overflow check it couldn't
/// prove away stays, as the safety net it is everywhere else, and so does a float divide check.
fn checks_kept(checks: &[fennec_llvm::CheckLeft], reach: &fennec_ir::HotReach) -> Vec<Diagnostic> {
    use fennec_llvm::CheckKind;
    let key = |name: &str| name.split_once('[').map_or(name, |(key, _)| key).to_owned();
    checks
        .iter()
        .filter(|check| {
            !check.span.in_prelude()
                && !matches!(check.what, CheckKind::Overflow | CheckKind::FloatDivideByZero)
        })
        .filter_map(|check| {
            let (_, root) = reach.reached.iter().find(|(name, _)| *name == check.function)?;
            let (what, help) = match &check.what {
                CheckKind::Overflow | CheckKind::FloatDivideByZero => return None,
                CheckKind::Bounds => (
                    "this index may be outside what it indexes, so it keeps its bounds check".to_owned(),
                    "index with a loop's count over the length, as in `for i in range(items.len())`",
                ),
                CheckKind::MissingKey => (
                    "this key may be missing, so it keeps its check".to_owned(),
                    "look it up with `.get(key) else ...`, before the hot path if it can be",
                ),
                CheckKind::DivideByZero => (
                    "this may divide by zero, so it keeps its check".to_owned(),
                    "divide by something the compiler can see isn't zero, like a number written out",
                ),
                CheckKind::ShiftRange => (
                    "this shift may be outside 0 to 63, so it keeps its check".to_owned(),
                    "shift by a number written out, or one the compiler can see is in range",
                ),
                CheckKind::Builtin(name) => (
                    format!("`{name}` can fail here, so it keeps its check"),
                    "do what can fail before the hot path, and pass in what it gives",
                ),
            };
            let whose = if key(&check.function) == *root {
                format!("`@hot` `{root}`")
            } else {
                format!("`{}`, which `@hot` `{root}` calls", key(&check.function))
            };
            let message = format!("{what} in {whose}");
            Some(Diagnostic::new(message, check.span).with_help(format!("{help}, or take `@hot` off")))
        })
        .collect()
}

/// Each of the program's own calls whose positional arguments are easy to get backwards.
fn positional_arguments(module: &Module, sources: &Sources) -> Vec<Diagnostic> {
    fennec_lint::positional_arguments(module, sources)
        .into_iter()
        .filter(|lint| !lint.diagnostic.span.in_prelude())
        .map(|lint| Diagnostic { message: lint.message().to_owned(), ..lint.diagnostic })
        .collect()
}

/// Where a function says it's `@hot`, if it does.
fn hot(def: &fennec_syntax::FnDef) -> Option<&fennec_syntax::Annotation> {
    def.annotations.iter().find(|annotation| annotation.name == "hot")
}

/// The program's own `@hot` functions and methods, by key, with where `@hot` is written.
fn hot_functions(module: &Module) -> Vec<(String, fennec_syntax::Span)> {
    let mut found = Vec::new();
    for item in &module.items {
        let (owner, methods) = match item {
            fennec_syntax::Item::Function(def) => {
                found.extend(
                    hot(def).filter(|_| !def.span.in_prelude()).map(|at| (def.name.clone(), at.span)),
                );
                continue;
            }
            fennec_syntax::Item::Struct(def)
            | fennec_syntax::Item::Enum(def)
            | fennec_syntax::Item::Error(def) => (&def.name, &def.methods),
            fennec_syntax::Item::Extend(extension) => (&extension.name, &extension.methods),
            _ => continue,
        };
        for method in methods.iter().filter(|method| !method.span.in_prelude()) {
            found.extend(hot(method).map(|at| (format!("{owner}.{}", method.name), at.span)));
        }
    }
    found
}

/// Checks a program to run, with what its `@hot` code may not cost as errors, and its
/// constants worked out while compiling.
pub(crate) fn check_to_run(
    module: &mut Module,
    sources: &Sources,
) -> Result<fennec_check::Signatures, Vec<Diagnostic>> {
    let signatures = baked(module, fennec_check::check_to_run)?;
    let copies = implicit_copies(module, &signatures, sources);
    if copies.is_empty() { Ok(signatures) } else { Err(copies) }
}

/// Like `check_to_run`, without working out the constants: what a check that only asks
/// whether the program is sound needs.
pub(crate) fn check_rules(
    module: &mut Module,
    sources: &Sources,
) -> Result<fennec_check::Signatures, Vec<Diagnostic>> {
    let signatures = fennec_check::check_to_run(module)?;
    let copies = implicit_copies(module, &signatures, sources);
    if copies.is_empty() { Ok(signatures) } else { Err(copies) }
}

/// Like `check_rules`, checking all of a module, as a build does, which works constants out when
/// the program starts.
pub(crate) fn check_all_rules(
    module: &mut Module,
    sources: &Sources,
) -> Result<fennec_check::Signatures, Vec<Diagnostic>> {
    let signatures = fennec_check::check_and_resolve(module)?;
    let copies = implicit_copies(module, &signatures, sources);
    if copies.is_empty() { Ok(signatures) } else { Err(copies) }
}

/// The steps a constant may take to be worked out while compiling. One that takes more is
/// worked out when the program starts, with a hint.
const CONSTANT_STEPS: u64 = 1_000_000;

type Check = fn(&mut Module) -> Result<fennec_check::Signatures, Vec<Diagnostic>>;

/// Checks a module with `check`, after working out its top-level constants while compiling.
fn baked(module: &mut Module, check: Check) -> Result<fennec_check::Signatures, Vec<Diagnostic>> {
    let mut resolved = module.clone();
    let signatures = check(&mut resolved)?;
    if bake_resolved(module, &resolved, &signatures)? {
        return check(module);
    }
    *module = resolved;
    Ok(signatures)
}

/// Works out a module's top-level constants while compiling: each that isn't a literal already
/// becomes the literal of its value, in the module as written, which is still to be checked.
pub(crate) fn bake(module: &mut Module) -> Result<(), Vec<Diagnostic>> {
    let mut resolved = module.clone();
    let signatures = fennec_check::check_to_run(&mut resolved)?;
    bake_resolved(module, &resolved, &signatures).map(|_| ())
}

/// What working out one constant while compiling gave.
enum Baked {
    Literal(fennec_syntax::Expr),
    TooLong,
    /// It's worked out when the program starts: its value has no literal, like a `Regex`'s, or
    /// it reaches what the compiler's evaluator doesn't do.
    AtStart,
    Failed(Diagnostic),
}

/// Whether a constant's value is a literal already: a number, string, bool or `None`.
fn is_literal(value: &fennec_syntax::Expr) -> bool {
    use fennec_syntax::ExprKind;
    match &value.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Duration(_)
        | ExprKind::Str(_)
        | ExprKind::Bytes(_)
        | ExprKind::Folder(_)
        | ExprKind::Bool(_)
        | ExprKind::None => true,
        ExprKind::Unary { op: fennec_syntax::UnaryOp::Neg, expr } => {
            matches!(expr.kind, ExprKind::Int(_) | ExprKind::Float(_))
        }
        _ => false,
    }
}

/// Works out each of a module's own constants that isn't a literal already, through the IR, as
/// `resolved`, its checked copy, has them.
fn worked_out(resolved: &Module, signatures: &fennec_check::Signatures) -> Vec<(String, Baked)> {
    let own: Vec<&str> = resolved
        .items
        .iter()
        .filter_map(|item| match item {
            fennec_syntax::Item::Const { name, value, .. } if !is_literal(value) => Some(name.as_str()),
            _ => None,
        })
        .collect();
    if own.is_empty() {
        return Vec::new();
    }
    // Lowering from the constants leaves out the module's functions they call, so those are
    // added until nothing they reach is left out.
    let mut wanted: Vec<String> = own.iter().map(|name| (*name).to_owned()).collect();
    let program = loop {
        let program =
            fennec_ir::lower_some(resolved, signatures, &|name| wanted.iter().any(|want| want == name));
        let more: Vec<String> =
            program.external.iter().filter(|name| !wanted.contains(name)).cloned().collect();
        if more.is_empty() {
            break program;
        }
        wanted.extend(more);
    };
    let types: Vec<&str> = resolved
        .items
        .iter()
        .filter_map(|item| match item {
            fennec_syntax::Item::Struct(def) | fennec_syntax::Item::Enum(def) => Some(def.name.as_str()),
            _ => None,
        })
        .collect();
    let mut written = fennec_ir::eval::Written::new(&program, &types);
    let mut baked = Vec::new();
    for (name, worked) in fennec_ir::eval::work_out(&program, CONSTANT_STEPS) {
        if !own.contains(&name.as_str()) {
            continue;
        }
        let outcome = match worked {
            fennec_ir::eval::Worked::Value(value) => {
                fennec_ir::eval::literal(&value, &mut written).map_or(Baked::AtStart, Baked::Literal)
            }
            fennec_ir::eval::Worked::TooLong => Baked::TooLong,
            fennec_ir::eval::Worked::NotHere(_) => Baked::AtStart,
            fennec_ir::eval::Worked::Failed(problem) => Baked::Failed(problem),
        };
        baked.push((name, outcome));
    }
    baked
}

/// Writes into `module` the literal of each constant `resolved`, its checked copy, works out
/// within the budget, and says whether it wrote any. One that fails to work out is a compile
/// error, since it would fail every time the program started.
fn bake_resolved(
    module: &mut Module,
    resolved: &Module,
    signatures: &fennec_check::Signatures,
) -> Result<bool, Vec<Diagnostic>> {
    let mut literals = Vec::new();
    for (name, baked) in worked_out(resolved, signatures) {
        match baked {
            Baked::Literal(value) => literals.push((name, value)),
            Baked::TooLong => eprintln!(
                "hint: `{name}` takes more than {CONSTANT_STEPS} steps to work out while compiling, so it's worked out when the program starts"
            ),
            Baked::AtStart => {}
            Baked::Failed(problem) => return Err(vec![problem]),
        }
    }
    for item in &mut module.items {
        if let fennec_syntax::Item::Const { name, value, .. } = item
            && let Some((_, literal)) = literals.iter().find(|(baked, _)| baked == name)
        {
            *value = literal.clone();
        }
    }
    Ok(!literals.is_empty())
}
