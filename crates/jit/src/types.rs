//! What the JIT knows about a program's functions. Ints, floats and bools are native
//! machine values; every other value is an object the runtime holds, which native code
//! refers to by a handle. Return types come from the type checker.

use fennec_check::{Signatures, Type};
use fennec_syntax::{Diagnostic, FnDef, Item, Module, Span, StmtKind, TypeExpr};

/// How native code holds a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ty {
    Int,
    Float,
    Bool,
    /// Nothing at all: what a function without a value returns.
    Unit,
    /// The `stdout` capability, which needs nothing at run time.
    Stdout,
    /// The `fs` capability, which needs nothing at run time either.
    Fs,
    /// Anything else, held by the runtime.
    Obj,
}

impl Ty {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Ty::Int => "int",
            Ty::Float => "float",
            Ty::Bool => "bool",
            Ty::Unit => "nothing",
            Ty::Stdout => "Stdout",
            Ty::Fs => "Fs",
            Ty::Obj => "value",
        }
    }

    pub(crate) fn of(ty: &Type) -> Ty {
        match ty {
            Type::Int => Ty::Int,
            Type::Float => Ty::Float,
            Type::Bool => Ty::Bool,
            Type::Unit => Ty::Unit,
            Type::Named(name) if name == "Stdout" => Ty::Stdout,
            Type::Named(name) if name == "Fs" => Ty::Fs,
            _ => Ty::Obj,
        }
    }
}

pub(crate) struct Param {
    pub(crate) name: String,
    pub(crate) ty: Ty,
    pub(crate) is_mut: bool,
}

pub(crate) struct Function<'m> {
    pub(crate) def: &'m FnDef,
    pub(crate) params: Vec<Param>,
    pub(crate) ret: Ty,
    /// Whether it gives back a result: it contains `fail` or `?`.
    pub(crate) fallible: bool,
}

pub(crate) struct Program<'m> {
    pub(crate) functions: Vec<Function<'m>>,
    pub(crate) main_span: Span,
}

pub(crate) fn unsupported(what: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("the JIT can't compile {what} yet"), span)
        .with_help("run the program without `--jit` to use the interpreter")
}

impl<'m> Program<'m> {
    /// The program's top-level functions the JIT could compile. The rest, and every
    /// method, run in the interpreter when native code calls them.
    pub(crate) fn from_module(module: &'m Module, signatures: &Signatures) -> Result<Self, Diagnostic> {
        let mut functions = Vec::new();
        let mut main_span = None;
        for item in &module.items {
            match item {
                Item::Function(def) => {
                    if def.name == "main" {
                        main_span = Some(def.span);
                    }
                    if let Some(function) = function(def, signatures) {
                        functions.push(function);
                    }
                }
                Item::Stmt(stmt) => return Err(unsupported("top-level statements", stmt.span)),
                _ => {}
            }
        }
        let main_span = main_span
            .ok_or_else(|| Diagnostic::new("this program has no `main` function", Span::default()))?;
        if !functions.iter().any(|function| function.def.name == "main") {
            return Err(unsupported("this `main`", main_span));
        }
        Ok(Program { functions, main_span })
    }
}

fn function<'m>(def: &'m FnDef, signatures: &Signatures) -> Option<Function<'m>> {
    let signature = signatures.get(&def.name)?;
    if signature.generator || def.body.iter().any(|stmt| matches!(stmt.kind, StmtKind::Def(_))) {
        return None;
    }
    let mut params = Vec::with_capacity(def.params.len());
    for param in &def.params {
        let ty = param_ty(param.ty.as_ref()?);
        params.push(Param { name: param.name.clone(), ty, is_mut: param.is_mut });
    }
    let ret = if signature.can_fail { Ty::Obj } else { Ty::of(&signature.returns) };
    Some(Function { def, params, ret, fallible: signature.can_fail })
}

/// How a parameter of a written type is held.
fn param_ty(ty: &TypeExpr) -> Ty {
    if ty.optional || !ty.args.is_empty() {
        return Ty::Obj;
    }
    match ty.name.as_str() {
        "int" | "i64" => Ty::Int,
        "float" | "f64" => Ty::Float,
        "bool" => Ty::Bool,
        "Stdout" => Ty::Stdout,
        "Fs" => Ty::Fs,
        _ => Ty::Obj,
    }
}
