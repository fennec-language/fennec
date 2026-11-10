//! Writes `shared T` and `lock` in as what every engine runs today, once a program checks:
//! each `shared T` is given a `shared` declaration holding one field, `value: T`, which the
//! written `shared T` then means, so every message still says what was written, and
//! `lock x as ref s:` becomes `with x as ref s__lock:` with each `s` read as `s__lock.value`.
//! `shared s = e` builds one around `e`. It's the only place that knows how they're run, so a
//! native `shared T`, and the rollback a crash in a lock takes, can replace it alone.

use std::collections::BTreeMap;

use fennec_syntax::{
    Arg, Expr, ExprKind, Field, FnDef, Item, Module, Span, Stmt, StmtKind, TypeDef, TypeExpr,
};

use crate::lock::SHARED;
use crate::types::Type;

/// What a lowering found to write in: the type each `shared` type it made holds, by its name.
struct Lowering<'d> {
    /// The type each `shared name = value` holds, by the value's span.
    declared: &'d BTreeMap<(u32, u32), Type>,
    made: BTreeMap<String, TypeExpr>,
    /// Whether anything was written in.
    changed: bool,
}

/// What ends the name a lowered `lock` gives what it holds: `s__lock` for `lock x as ref s:`.
pub(crate) const HELD: &str = "__lock";

/// Lowers every `shared T`, `shared s = e` and `lock` in `module`, saying whether there were any.
pub(crate) fn lower(module: &mut Module, declared: &BTreeMap<(u32, u32), Type>) -> bool {
    let mut lowering = Lowering { declared, made: BTreeMap::new(), changed: false };
    for item in &mut module.items {
        match item {
            Item::Function(def) => lowering.function(def),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => lowering.type_def(def),
            Item::Extend(extension) => extension.methods.iter_mut().for_each(|def| lowering.function(def)),
            Item::Interface(interface) => interface.methods.iter_mut().for_each(|def| lowering.function(def)),
            Item::Test(test) => {
                for param in test.capabilities.iter_mut().chain(&mut test.inputs) {
                    lowering.written(param.ty.as_mut());
                }
                lowering.stmts(&mut test.body);
            }
            Item::TypeAlias { ty, .. } => lowering.written(Some(ty)),
            Item::Const { ty, .. } => lowering.written(ty.as_mut()),
            Item::Stmt(stmt) => lowering.stmts(std::slice::from_mut(stmt)),
            Item::Import(_) => {}
        }
    }
    // A declaration made by an earlier lowering of the same program is there already.
    let declared: Vec<String> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Struct(def) => Some(def.name.clone()),
            _ => None,
        })
        .collect();
    lowering.made.retain(|name, _| !declared.contains(name));
    let changed = lowering.changed || !lowering.made.is_empty();
    for (name, held) in lowering.made {
        let value = Field {
            name: "value".to_owned(),
            ty: Some(held),
            default: None,
            rule: None,
            annotations: Vec::new(),
            span: Span::default(),
        };
        module.items.push(Item::Struct(TypeDef {
            name,
            generics: Vec::new(),
            fields: vec![value],
            cases: Vec::new(),
            methods: Vec::new(),
            shared: true,
            span: Span::default(),
        }));
    }
    changed
}

/// The name of the `shared` declaration that holds a `held`, made from how it's written.
pub(crate) fn made_name(held: &str) -> String {
    let letters: String = held.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    format!("Shared__{letters}")
}

/// A type written as its text says, like `dict[str, int]`.
/// A program's own type named like one of the standard library's is checked under a hidden name,
/// `Log'`, which isn't written that way, so it's parsed as a name and given back.
fn type_from(text: &str) -> Option<TypeExpr> {
    const STANDS_IN: &str = "hidden__mark__";
    let written = text.replace(fennec_syntax::visit::HIDDEN, STANDS_IN);
    let module = fennec_syntax::parse(&format!("def held(value: {written}):\n    pass\n")).ok()?;
    let Some(Item::Function(def)) = module.items.into_iter().next() else { return None };
    let mut ty = def.params.into_iter().next().and_then(|param| param.ty)?;
    hidden_again(&mut ty, STANDS_IN);
    Some(ty)
}

fn hidden_again(ty: &mut TypeExpr, stands_in: &str) {
    ty.name = ty.name.replace(stands_in, &fennec_syntax::visit::HIDDEN.to_string());
    for arg in &mut ty.args {
        hidden_again(arg, stands_in);
    }
}

impl Lowering<'_> {
    /// The `shared` declaration holding a `held`, made the first time it's asked for.
    fn made(&mut self, held: TypeExpr) -> String {
        let name = made_name(&crate::program::written(&held));
        self.made.entry(name.clone()).or_insert(held);
        name
    }

    fn type_def(&mut self, def: &mut TypeDef) {
        for field in def.fields.iter_mut().chain(def.cases.iter_mut().flat_map(|case| &mut case.fields)) {
            self.written(field.ty.as_mut());
        }
        for method in &mut def.methods {
            self.function(method);
        }
    }

    fn function(&mut self, def: &mut FnDef) {
        for param in &mut def.params {
            self.written(param.ty.as_mut());
        }
        self.written(def.ret.as_mut());
        self.written(def.fails.as_mut());
        self.stmts(&mut def.body);
    }

    /// The declaration for each `shared T`, here or anywhere inside a written type. What's
    /// written stays as it is, and means that declaration once it exists.
    fn written(&mut self, ty: Option<&mut TypeExpr>) {
        let Some(ty) = ty else { return };
        for arg in &mut ty.args {
            self.written(Some(arg));
        }
        if ty.name == SHARED
            && let [held] = ty.args.as_slice()
        {
            self.made(held.clone());
        }
    }

    fn stmts(&mut self, body: &mut [Stmt]) {
        for stmt in body {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &mut Stmt) {
        match &mut stmt.kind {
            StmtKind::Assign { target, value, ty, .. } => {
                let shared = ty.as_ref().is_some_and(|ty| ty.name == SHARED && ty.args.is_empty());
                if shared {
                    let held = self.declared.get(&(value.span.start, value.span.end)).and_then(Type::source);
                    if let Some(held) = held.as_deref().and_then(type_from) {
                        let name = self.made(held);
                        // Types are found by span, so what's made has spans of its own: the call
                        // runs from the name to the value's end, and its callee is empty.
                        let span = value.span;
                        let given = std::mem::replace(value, Expr { kind: ExprKind::None, span });
                        let at = Span { start: target.span.start, end: target.span.start };
                        let callee = Box::new(Expr { kind: ExprKind::Name(name), span: at });
                        let arg = Arg { name: Some("value".to_owned()), is_mut: false, value: given };
                        let whole = Span { start: target.span.start, end: span.end };
                        *value = Expr { kind: ExprKind::Call { callee, args: vec![arg] }, span: whole };
                        *ty = None;
                    }
                } else {
                    self.written(ty.as_mut());
                }
                self.exprs(value);
            }
            StmtKind::With { value, names, body, lock } => {
                self.exprs(value);
                if std::mem::take(lock) {
                    self.changed = true;
                    let renamed: Vec<(String, String)> = names
                        .iter()
                        .map(|held| (held.name.clone(), format!("{}{HELD}", held.name)))
                        .collect();
                    for (held, (_, handle)) in names.iter_mut().zip(&renamed) {
                        held.name.clone_from(handle);
                    }
                    fennec_syntax::visit::in_stmts(body, &mut |expr| {
                        if let ExprKind::Name(name) = &expr.kind
                            && let Some((_, handle)) = renamed.iter().find(|(held, _)| held == name)
                        {
                            let at = handle_span(expr.span);
                            let object = Box::new(Expr { kind: ExprKind::Name(handle.clone()), span: at });
                            expr.kind = ExprKind::Attr { object, name: "value".to_owned(), optional: false };
                        }
                    });
                }
                self.stmts(body);
            }
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    self.exprs(cond);
                    self.stmts(body);
                }
                if let Some(otherwise) = otherwise {
                    self.stmts(otherwise);
                }
            }
            StmtKind::While { cond, body } => {
                self.exprs(cond);
                self.stmts(body);
            }
            StmtKind::For { iter, body, .. } => {
                self.exprs(iter);
                self.stmts(body);
            }
            StmtKind::OnFailure { cleanup, body, .. } => {
                self.stmts(cleanup);
                self.stmts(body);
            }
            StmtKind::Def(def) => self.function(def),
            StmtKind::Expr(value)
            | StmtKind::Fail(value)
            | StmtKind::Yield(value)
            | StmtKind::Assert(value)
            | StmtKind::Return(Some(value)) => self.exprs(value),
            StmtKind::Lend { value, otherwise } => {
                self.exprs(value);
                self.stmts(otherwise);
            }
            StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
        }
    }

    /// The statements in a `match`'s arms, anywhere in `value`.
    fn exprs(&mut self, value: &mut Expr) {
        fennec_syntax::visit::in_expr(value, &mut |expr| {
            if let ExprKind::Match { arms, .. } = &mut expr.kind {
                for arm in arms {
                    self.stmts(&mut arm.body);
                }
            }
        });
    }
}

/// Where a lowered lock's handle is, read in place of the name at `named`: empty at the name's
/// start, so its type, found by span, isn't the value's. A name another lowering wrote in, like a
/// local `ref`'s place, is empty already, so its handle gets a span of its own.
fn handle_span(named: Span) -> Span {
    if named.start == named.end {
        crate::lend_lowering::here(named)
    } else {
        Span { start: named.start, end: named.start }
    }
}
