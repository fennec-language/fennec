//! The part of the language the JIT compiles, and the order to compile functions in.
//! Until the type checker lands, the JIT works out types itself: parameters are annotated,
//! and a function's return type is learned while compiling it, so callees compile first.

use std::collections::{HashMap, HashSet};

use fennec_syntax::{
    Diagnostic, Expr, ExprKind, FStrPart, FnDef, Item, Module, Span, Stmt, StmtKind, TypeExpr,
};

/// The types the JIT handles. `Stdout` is a capability with nothing to pass at run time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ty {
    Int,
    Float,
    Bool,
    Unit,
    Stdout,
}

impl Ty {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Ty::Int => "int",
            Ty::Float => "float",
            Ty::Bool => "bool",
            Ty::Unit => "nothing",
            Ty::Stdout => "Stdout",
        }
    }
}

pub(crate) struct Function<'m> {
    pub(crate) def: &'m FnDef,
    pub(crate) params: Vec<Ty>,
    /// The declared return type, if there is one.
    pub(crate) ret: Option<Ty>,
}

pub(crate) struct Program<'m> {
    /// Functions in compile order: each after the unannotated functions it calls.
    pub(crate) functions: Vec<Function<'m>>,
    pub(crate) main_span: Span,
}

pub(crate) fn unsupported(what: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("the JIT can't compile {what} yet"), span)
        .with_help("run the program without `--jit` to use the interpreter")
}

impl<'m> Program<'m> {
    pub(crate) fn from_module(module: &'m Module) -> Result<Self, Diagnostic> {
        let mut functions = Vec::new();
        for item in &module.items {
            match item {
                Item::Function(def) => functions.push(function(def)?),
                Item::Import(_) | Item::TypeAlias { .. } | Item::Interface(_) | Item::Test(_) => {}
                Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                    return Err(unsupported("structs, enums and errors", def.span));
                }
                Item::Stmt(stmt) => return Err(unsupported("top-level statements", stmt.span)),
            }
        }
        let main = functions
            .iter()
            .find(|function| function.def.name == "main")
            .ok_or_else(|| Diagnostic::new("this program has no `main` function", Span::default()))?;
        let main_span = main.def.span;
        let functions = order(functions)?;
        Ok(Program { functions, main_span })
    }
}

fn function(def: &FnDef) -> Result<Function<'_>, Diagnostic> {
    let mut params = Vec::with_capacity(def.params.len());
    for param in &def.params {
        if param.is_mut || param.is_self {
            return Err(unsupported("`mut` parameters", param.span));
        }
        let ty = param.ty.as_ref().ok_or_else(|| unsupported("this parameter", param.span))?;
        params.push(ty_of(ty)?);
    }
    let ret = def.ret.as_ref().map(ty_of).transpose()?;
    Ok(Function { def, params, ret })
}

fn ty_of(ty: &TypeExpr) -> Result<Ty, Diagnostic> {
    if ty.optional || !ty.args.is_empty() {
        return Err(unsupported(&format!("the type `{}`", ty.name), ty.span));
    }
    Ok(match ty.name.as_str() {
        "int" | "i64" => Ty::Int,
        "float" | "f64" => Ty::Float,
        "bool" => Ty::Bool,
        "Stdout" => Ty::Stdout,
        "None" => Ty::Unit,
        other => return Err(unsupported(&format!("the type `{other}`"), ty.span)),
    })
}

/// Orders functions so each comes after the functions without a declared return type that
/// it calls. A cycle of such functions needs a declared return type to break it.
fn order(functions: Vec<Function<'_>>) -> Result<Vec<Function<'_>>, Diagnostic> {
    let unannotated: HashSet<String> = functions
        .iter()
        .filter(|function| function.ret.is_none())
        .map(|function| function.def.name.clone())
        .collect();
    let needs: HashMap<String, HashSet<String>> = functions
        .iter()
        .map(|function| {
            let mut called = HashSet::new();
            for stmt in &function.def.body {
                calls_in_stmt(stmt, &mut called);
            }
            called.retain(|name| unannotated.contains(name) && *name != function.def.name);
            (function.def.name.clone(), called)
        })
        .collect();
    let mut remaining = functions;
    let mut ordered = Vec::with_capacity(remaining.len());
    let mut done: HashSet<String> = HashSet::new();
    while !remaining.is_empty() {
        let before = remaining.len();
        let (ready, waiting): (Vec<_>, Vec<_>) = remaining.into_iter().partition(|function| {
            needs.get(&function.def.name).is_none_or(|called| called.iter().all(|name| done.contains(name)))
        });
        done.extend(ready.iter().map(|function| function.def.name.clone()));
        ordered.extend(ready);
        remaining = waiting;
        if remaining.len() == before {
            let stuck = remaining.first().map_or(Span::default(), |function| function.def.span);
            return Err(Diagnostic::new("functions that call each other need a declared return type", stuck)
                .with_help("add one, like `def fib(n: int) -> int:`"));
        }
    }
    Ok(ordered)
}

fn calls_in_stmt(stmt: &Stmt, called: &mut HashSet<String>) {
    match &stmt.kind {
        StmtKind::Expr(expr) | StmtKind::Return(Some(expr)) => calls_in(expr, called),
        StmtKind::Assign { value, .. } => calls_in(value, called),
        StmtKind::If { branches, otherwise } => {
            for (cond, body) in branches {
                calls_in(cond, called);
                for stmt in body {
                    calls_in_stmt(stmt, called);
                }
            }
            otherwise.iter().flatten().for_each(|stmt| calls_in_stmt(stmt, called));
        }
        StmtKind::While { cond: expr, body } | StmtKind::For { iter: expr, body, .. } => {
            calls_in(expr, called);
            for stmt in body {
                calls_in_stmt(stmt, called);
            }
        }
        _ => {}
    }
}

fn calls_in(expr: &Expr, called: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            if let ExprKind::Name(name) = &callee.kind {
                called.insert(name.clone());
            }
            calls_in(callee, called);
            for arg in args {
                calls_in(&arg.value, called);
            }
        }
        ExprKind::Unary { expr: inner, .. } | ExprKind::Attr { object: inner, .. } => calls_in(inner, called),
        ExprKind::Binary { lhs, rhs, .. } => {
            calls_in(lhs, called);
            calls_in(rhs, called);
        }
        ExprKind::Cond { then, cond, otherwise } => {
            for part in [then, cond, otherwise] {
                calls_in(part, called);
            }
        }
        ExprKind::FStr(parts) => {
            for part in parts {
                if let FStrPart::Expr { expr, .. } = part {
                    calls_in(expr, called);
                }
            }
        }
        _ => {}
    }
}
