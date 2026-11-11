//! Inlay hints: what an editor writes into the code without it being there. The type a local
//! name holds where it's declared without one, each positional argument's parameter name, and
//! a mark where a value is copied inside a loop. They need a program that checks, since only
//! the typed tree knows types and which method is called.

use std::collections::HashMap;

use fennec_check::{Checked, Type};
use fennec_ir::{Cost, Finding};
use fennec_syntax::{Expr, ExprKind, FStrPart, FnDef, Item, Module, Sources, Span, Stmt, StmtKind};

use crate::analysis::Analysis;
use crate::completion::{Known, Written, owner};
use crate::definition::name_in;
use crate::json::Json;
use crate::text;

/// Each expression's type, by where it starts and ends.
type Typed<'a> = HashMap<(u32, u32), &'a Type>;

/// The protocol's numbers for the kinds of hint.
const TYPE: usize = 1;
const PARAMETER: usize = 2;

/// One hint, placed in its file's text.
struct Hint {
    at: usize,
    label: String,
    kind: Option<usize>,
    /// Whether it goes before what it's about, and so wants a space after it.
    before: bool,
    tooltip: Option<String>,
}

/// The hints for bytes `from..to` of the file at `path`, in the order they come.
pub(crate) fn hints(analysis: &Analysis, path: &str, from: usize, to: usize) -> Vec<Json> {
    let (Some(module), Some(checked)) = (&analysis.module, &analysis.checked) else { return Vec::new() };
    let Some((text, base)) = analysis.sources.by_path(path) else { return Vec::new() };
    let local = |span: Span| {
        let at = usize::try_from(span.start.checked_sub(base)?).ok()?;
        (analysis.sources.file_at(span)?.0 == path && (from..=to).contains(&at)).then_some(at)
    };
    let mut found = types(&analysis.sources, checked, &local);
    found.extend(parameters(module, &analysis.sources, checked, &local));
    found.extend(copies(&analysis.costs, &local));
    found.sort_by_key(|hint| (hint.at, !hint.before));
    found.iter().map(|hint| protocol(text, hint)).collect()
}

fn protocol(text: &str, hint: &Hint) -> Json {
    let mut fields = vec![
        ("position".to_owned(), text::position(text, hint.at)),
        ("label".to_owned(), Json::string(hint.label.as_str())),
    ];
    if let Some(kind) = hint.kind {
        fields.push(("kind".to_owned(), Json::number(kind)));
    }
    let padding = if hint.before { "paddingRight" } else { "paddingLeft" };
    fields.push((padding.to_owned(), Json::Bool(hint.before)));
    if let Some(tooltip) = &hint.tooltip {
        fields.push(("tooltip".to_owned(), Json::string(tooltip.as_str())));
    }
    Json::Object(fields)
}

/// Whether the name written at bytes `start..end` of `text` is given a type there: a `:`
/// after it with something on the line after that, unlike the `:` that ends `as name:`.
fn annotated(text: &str, end: usize) -> bool {
    let rest = text.get(end..).unwrap_or("").trim_start_matches(' ');
    rest.strip_prefix(':').is_some_and(|after| {
        let line = after.split('\n').next().unwrap_or("");
        !line.trim().is_empty() && !line.trim_start().starts_with('#')
    })
}

/// The type of each local name declared without one, after its name.
fn types(sources: &Sources, checked: &Checked, local: &dyn Fn(Span) -> Option<usize>) -> Vec<Hint> {
    let mut found = Vec::new();
    let mut seen = Vec::new();
    for (_, body) in checked.types.iter() {
        for (declared, name, ty) in body.names() {
            if name == "self" || name.starts_with('_') || matches!(ty, Type::Unknown) {
                continue;
            }
            let name_at = name_in(sources, declared, name);
            let Some(start) = local(name_at) else { continue };
            let end = start + name.len();
            let Some((_, text, _)) = sources.file_at(name_at) else { continue };
            // A name not written where it's declared, like one a desugaring makes, has no place.
            if text.get(start..end) != Some(name) || annotated(text, end) || seen.contains(&end) {
                continue;
            }
            seen.push(end);
            found.push(Hint {
                at: end,
                label: format!(": {ty}"),
                kind: Some(TYPE),
                before: false,
                tooltip: None,
            });
        }
    }
    found
}

/// A mark before each value a loop copies, as `fennec check` and `fennec lint` find them.
fn copies(costs: &[Finding], local: &dyn Fn(Span) -> Option<usize>) -> Vec<Hint> {
    let in_loop = |finding: &&Finding| finding.loops > 0 || finding.called_from_loop.is_some();
    let copied = costs.iter().filter(|finding| matches!(finding.cost, Cost::Copy { .. })).filter(in_loop);
    copied
        .filter_map(|finding| {
            let label = if finding.per_iteration() { "copies each pass" } else { "copies once" };
            Some(Hint {
                at: local(finding.span)?,
                label: label.to_owned(),
                kind: None,
                before: true,
                tooltip: Some(finding.message()),
            })
        })
        .collect()
}

/// The parameter name before each positional argument whose parameter is known, unless the
/// argument already says it.
fn parameters(
    module: &Module,
    sources: &Sources,
    checked: &Checked,
    local: &dyn Fn(Span) -> Option<usize>,
) -> Vec<Hint> {
    let typed: Typed<'_> = checked
        .types
        .iter()
        .flat_map(|(_, body)| body.exprs())
        .map(|(span, ty)| ((span.start, span.end), ty))
        .collect();
    let known = Known { module, sources };
    let mut found = Vec::new();
    walk(module, &mut |expr| {
        let ExprKind::Call { callee, args } = &expr.kind else { return };
        let names = params(&known, &typed, callee);
        let positional = args.iter().take_while(|arg| arg.name.is_none());
        for (arg, name) in positional.zip(&names) {
            let says = match &arg.value.kind {
                ExprKind::Name(written) | ExprKind::Attr { name: written, .. } => written == name,
                _ => false,
            };
            if says {
                continue;
            }
            if let Some(at) = local(arg.value.span) {
                let label = format!("{name}:");
                found.push(Hint { at, label, kind: Some(PARAMETER), before: true, tooltip: None });
            }
        }
    });
    found
}

/// The parameters of a function, without `self`.
fn of_function(def: &FnDef) -> Vec<String> {
    def.params.iter().filter(|param| !param.is_self).map(|param| param.name.clone()).collect()
}

/// The parameter names of what `callee` calls, when it's declared.
fn params(known: &Known<'_>, typed: &Typed<'_>, callee: &Expr) -> Vec<String> {
    let fields = |fields: &[fennec_syntax::Field]| fields.iter().map(|field| field.name.clone()).collect();
    match &callee.kind {
        ExprKind::Name(name) => {
            let own = known.items().filter(|(_, written)| matches!(written, Written::Program));
            for (item, _) in own {
                match item {
                    Item::Function(def) if def.name == *name => return of_function(def),
                    Item::Struct(def) | Item::Error(def) if def.name == *name => return fields(&def.fields),
                    _ => {}
                }
            }
            Vec::new()
        }
        ExprKind::Variant(name) => {
            let case = known.types().flat_map(|(def, _)| &def.cases).find(|case| case.name == *name);
            case.map(|case| fields(&case.fields)).unwrap_or_default()
        }
        ExprKind::Attr { object, name, .. } => {
            if let ExprKind::Name(owner) = &object.kind
                && !typed.contains_key(&(object.span.start, object.span.end))
            {
                let method =
                    known.methods(owner).into_iter().find(|(def, _)| def.name == *name && !def.takes_self());
                if let Some((def, _)) = method {
                    return of_function(def);
                }
                let case =
                    known.types().filter(|(def, _)| def.name == *owner).flat_map(|(def, _)| &def.cases);
                return case
                    .into_iter()
                    .find(|case| case.name == *name)
                    .map(|case| fields(&case.fields))
                    .unwrap_or_default();
            }
            let Some(owner) = typed.get(&(object.span.start, object.span.end)).and_then(|ty| owner(ty))
            else {
                return Vec::new();
            };
            let method =
                known.methods(owner).into_iter().find(|(def, _)| def.name == *name && def.takes_self());
            method.map(|(def, _)| of_function(def)).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// Calls `visit` on every expression in the module, outermost first.
fn walk(module: &Module, visit: &mut dyn FnMut(&Expr)) {
    for item in &module.items {
        match item {
            Item::Function(def) => block(&def.body, visit),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                for field in def.fields.iter().chain(def.cases.iter().flat_map(|case| &case.fields)) {
                    field.default.iter().chain(&field.rule).for_each(|value| walk_expr(value, visit));
                }
                def.methods.iter().for_each(|method| block(&method.body, visit));
            }
            Item::Extend(extension) => extension.methods.iter().for_each(|method| block(&method.body, visit)),
            Item::Interface(interface) => {
                interface.methods.iter().for_each(|method| block(&method.body, visit));
            }
            Item::Const { value, .. } => walk_expr(value, visit),
            Item::Test(test) => block(&test.body, visit),
            Item::Stmt(stmt) => statement(stmt, visit),
            Item::TypeAlias { .. } | Item::Import(_) => {}
        }
    }
}

fn block(body: &[Stmt], visit: &mut dyn FnMut(&Expr)) {
    for stmt in body {
        statement(stmt, visit);
    }
}

fn statement(stmt: &Stmt, visit: &mut dyn FnMut(&Expr)) {
    match &stmt.kind {
        StmtKind::Expr(value)
        | StmtKind::Fail(value)
        | StmtKind::Yield(value)
        | StmtKind::Assert(value)
        | StmtKind::Lend { value, .. } => {
            walk_expr(value, visit);
        }
        StmtKind::Return(value) => value.iter().for_each(|value| walk_expr(value, visit)),
        StmtKind::Assign { target, value, .. } => {
            walk_expr(target, visit);
            walk_expr(value, visit);
        }
        StmtKind::If { branches, otherwise } => {
            for (cond, body) in branches {
                walk_expr(cond, visit);
                block(body, visit);
            }
            if let Some(body) = otherwise {
                block(body, visit);
            }
        }
        StmtKind::While { cond, body } => {
            walk_expr(cond, visit);
            block(body, visit);
        }
        StmtKind::For { iter, body, .. } => {
            walk_expr(iter, visit);
            block(body, visit);
        }
        StmtKind::With { value, body, .. } => {
            walk_expr(value, visit);
            block(body, visit);
        }
        StmtKind::Def(def) => block(&def.body, visit),
        StmtKind::OnFailure { body, .. } => block(body, visit),
        StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
    }
}

fn walk_expr(value: &Expr, visit: &mut dyn FnMut(&Expr)) {
    visit(value);
    match &value.kind {
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => {
            for part in items {
                walk_expr(part, visit);
            }
        }
        ExprKind::Dict(pairs) => pairs.iter().for_each(|(key, value)| {
            walk_expr(key, visit);
            walk_expr(value, visit);
        }),
        ExprKind::FStr(parts) => parts.iter().for_each(|part| {
            if let FStrPart::Expr { expr, .. } = part {
                walk_expr(expr, visit);
            }
        }),
        ExprKind::Comprehension { elem, value, iter, cond, .. } => {
            [Some(iter), Some(elem), value.as_ref(), cond.as_ref()]
                .into_iter()
                .flatten()
                .for_each(|part| walk_expr(part, visit));
        }
        ExprKind::Unary { expr, .. } | ExprKind::Lambda { body: expr, .. } => walk_expr(expr, visit),
        ExprKind::Binary { lhs: first, rhs: second, .. }
        | ExprKind::Else { value: first, default: second, .. }
        | ExprKind::Index { object: first, index: second } => {
            walk_expr(first, visit);
            walk_expr(second, visit);
        }
        ExprKind::Cond { then, cond, otherwise } => {
            for part in [then, cond, otherwise] {
                walk_expr(part, visit);
            }
        }
        ExprKind::Try { value, context } => {
            walk_expr(value, visit);
            if let Some(context) = context {
                walk_expr(context, visit);
            }
        }
        ExprKind::Call { callee, args } => {
            walk_expr(callee, visit);
            for arg in args {
                walk_expr(&arg.value, visit);
            }
        }
        ExprKind::Attr { object, .. } => walk_expr(object, visit),
        ExprKind::Slice { object, start, end } => {
            walk_expr(object, visit);
            start.iter().chain(end).for_each(|part| walk_expr(part, visit));
        }
        ExprKind::Match { subject, arms } => {
            walk_expr(subject, visit);
            for arm in arms {
                arm.guard.iter().for_each(|guard| walk_expr(guard, visit));
                block(&arm.body, visit);
            }
        }
        ExprKind::Int(_)
        | ExprKind::Bytes(_)
        | ExprKind::Folder(_)
        | ExprKind::Duration(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::None
        | ExprKind::Name(_)
        | ExprKind::Variant(_) => {}
    }
}
