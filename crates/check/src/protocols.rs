//! The protocols a type joins by writing a method with a set name, like `def str`. A method
//! with that name has to take what the protocol passes, or printing it would fail when it runs.

use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Stmt, StmtKind};

use crate::checker::Checker;

impl Checker {
    /// Checks each program type's protocol methods, as declared.
    pub(crate) fn check_protocols(&mut self) {
        let mut problems = Vec::new();
        for info in self.program.types.values().filter(|info| !info.prelude) {
            for method in &info.def.methods {
                problems.extend(protocol_problem(&info.def.name, method));
            }
        }
        self.diagnostics.extend(problems);
    }
}

/// What's wrong with a protocol method of a type, if it is one and anything is, wherever it's
/// written: in the type's body or in an `extend`.
pub(crate) fn protocol_problem(type_name: &str, method: &FnDef) -> Option<Diagnostic> {
    match method.name.as_str() {
        "str" => str_problem(type_name, method),
        "[]mut" => mut_index_problem(type_name, method),
        "is_live" => is_live_problem(type_name, method),
        _ => None,
    }
}

/// What's wrong with a type's `def str`, if anything: it takes `self, mut out: StrWriter` and
/// gives back nothing.
fn str_problem(type_name: &str, method: &FnDef) -> Option<Diagnostic> {
    let writes = match method.params.as_slice() {
        [this, out] => {
            this.is_self
                && out.is_mut
                && out
                    .ty
                    .as_ref()
                    .is_some_and(|ty| ty.name == "StrWriter" && ty.args.is_empty() && !ty.optional)
        }
        _ => false,
    };
    (!writes || method.ret.is_some()).then(|| {
        Diagnostic::new(
            format!("`{type_name}`'s `def str` writes into `ref out: StrWriter` and gives back nothing"),
            method.span,
        )
        .with_help("write `def str(self, ref out: StrWriter)`, and add its text with `out.write(...)`")
    })
}

/// Whether a parameter is written as an `int`.
fn takes_int(method: &FnDef, at: usize) -> bool {
    method.params.get(at).is_some_and(|param| {
        param.ty.as_ref().is_some_and(|ty| ty.name == "int" && ty.args.is_empty() && !ty.optional)
    })
}

/// What's wrong with a type's `mut` form of `def []`, if anything: it takes `mut self, i: int`,
/// says what it gives back, and every way out returns a place in `self`.
fn mut_index_problem(type_name: &str, method: &FnDef) -> Option<Diagnostic> {
    if method.params.len() != 2 || !takes_int(method, 1) || method.ret.is_none() {
        let message =
            format!("`{type_name}`'s `ref` form of `def []` takes a position and gives back its item");
        let help = "write `def [](ref self, i: int) -> Item`";
        return Some(Diagnostic::new(message, method.span).with_help(help));
    }
    let mut returned = Vec::new();
    returns_in(&method.body, &mut returned);
    let ends_in_return = matches!(method.body.last().map(|stmt| &stmt.kind), Some(StmtKind::Return(Some(_))));
    let not_a_place = returned.iter().find(|value| !is_place_in_self(value)).map(|value| value.span);
    let at = not_a_place.or((!ends_in_return).then_some(method.span))?;
    let message = format!("`{type_name}`'s `ref` form of `def []` gives back a place in `self`");
    let help = "end it with `return` and a field or item of `self`, like `return self._items[i]`";
    Some(Diagnostic::new(message, at).with_help(help))
}

/// What's wrong with a type's `def is_live`, if anything: it says whether a position holds an item.
fn is_live_problem(type_name: &str, method: &FnDef) -> Option<Diagnostic> {
    let says = method.params.len() == 2
        && method.params.first().is_some_and(|this| this.is_self && !this.is_mut)
        && takes_int(method, 1)
        && method.ret.as_ref().is_some_and(|ty| ty.name == "bool" && !ty.optional);
    (!says).then(|| {
        Diagnostic::new(
            format!("`{type_name}`'s `def is_live` says whether a position holds an item"),
            method.span,
        )
        .with_help("write `def is_live(self, i: int) -> bool`")
    })
}

/// Every value a body returns, nested blocks included. A nested `def` returns on its own.
fn returns_in<'b>(body: &'b [Stmt], returned: &mut Vec<&'b Expr>) {
    for stmt in body {
        match &stmt.kind {
            StmtKind::Return(Some(value)) => returned.push(value),
            StmtKind::If { branches, otherwise } => {
                for (_, block) in branches {
                    returns_in(block, returned);
                }
                if let Some(block) = otherwise {
                    returns_in(block, returned);
                }
            }
            StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::With { body, .. }
            | StmtKind::OnFailure { body, .. } => {
                returns_in(body, returned);
            }
            _ => {}
        }
    }
}

/// Whether an expression names a field or item of `self`.
fn is_place_in_self(value: &Expr) -> bool {
    let (ExprKind::Attr { object, optional: false, .. } | ExprKind::Index { object, .. }) = &value.kind
    else {
        return false;
    };
    matches!(&object.kind, ExprKind::Name(name) if name == "self") || is_place_in_self(object)
}

/// Whether a `mut` form is `return self.field[i]` and nothing else, so writing position `i`
/// touches only element `i`, and tasks with separate positions can't meet.
pub(crate) fn writes_one_item(method: &FnDef) -> bool {
    let [Stmt { kind: StmtKind::Return(Some(value)), .. }] = method.body.as_slice() else { return false };
    let ExprKind::Index { object, index } = &value.kind else { return false };
    let ExprKind::Attr { object: holder, optional: false, .. } = &object.kind else { return false };
    let is_position = method
        .params
        .get(1)
        .is_some_and(|position| matches!(&index.kind, ExprKind::Name(name) if *name == position.name));
    is_position && matches!(&holder.kind, ExprKind::Name(name) if name == "self")
}
