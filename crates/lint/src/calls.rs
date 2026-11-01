//! `positional-arguments`: an argument whose meaning can't be read from the call has to be named.
//! That's a bare `True` or `False`, any argument past the first when there are three or more,
//! and an argument beside a parameter of the same type, which are easy to swap unnoticed. The
//! first argument and `self` are exempt. A call this can't be sure of the target of isn't flagged.

use std::collections::HashMap;

use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, Field, FnDef, Item, Module, Param, Span, TypeExpr};

use crate::Lint;

/// What a callee takes: each parameter's name and written type, without `self`.
type Takes<'m> = Vec<(&'m str, Option<&'m TypeExpr>)>;

fn takes_params(params: &[Param]) -> Takes<'_> {
    params
        .iter()
        .filter(|param| !param.is_self)
        .map(|param| (param.name.as_str(), param.ty.as_ref()))
        .collect()
}

fn takes_fields(fields: &[Field]) -> Takes<'_> {
    fields.iter().map(|field| (field.name.as_str(), field.ty.as_ref())).collect()
}

/// What the program's own functions, constructors and methods take.
#[derive(Default)]
struct Callees<'m> {
    named: HashMap<&'m str, Takes<'m>>,
    /// Every method with a name, on any type: the type isn't known here.
    methods: HashMap<&'m str, Vec<Takes<'m>>>,
}

impl<'m> Callees<'m> {
    fn of(module: &'m Module) -> Self {
        let mut callees = Callees::default();
        for item in &module.items {
            match item {
                Item::Function(def) => {
                    callees.named.insert(&def.name, takes_params(&def.params));
                }
                Item::Struct(def) | Item::Error(def) => {
                    callees.named.insert(&def.name, takes_fields(&def.fields));
                    callees.add_methods(&def.methods);
                }
                Item::Enum(def) => callees.add_methods(&def.methods),
                Item::Extend(extension) => callees.add_methods(&extension.methods),
                _ => {}
            }
        }
        callees
    }

    fn add_methods(&mut self, methods: &'m [FnDef]) {
        for method in methods {
            self.methods.entry(&method.name).or_default().push(takes_params(&method.params));
        }
    }

    /// What a call's target takes, if there's only one thing it can be.
    fn of_call(&self, callee: &Expr) -> Option<&Takes<'m>> {
        match &callee.kind {
            ExprKind::Name(name) => self.named.get(name.as_str()),
            ExprKind::Attr { name, .. } => {
                let methods = self.methods.get(name.as_str())?;
                let first = methods.first()?;
                methods.iter().all(|other| other == first).then_some(first)
            }
            _ => None,
        }
    }
}

fn same_type(a: Option<&TypeExpr>, b: Option<&TypeExpr>) -> bool {
    fn same(a: &TypeExpr, b: &TypeExpr) -> bool {
        a.name == b.name
            && a.optional == b.optional
            && a.args.len() == b.args.len()
            && a.args.iter().zip(&b.args).all(|(a, b)| same(a, b))
    }
    a.zip(b).is_some_and(|(a, b)| same(a, b))
}

/// Whether the positional argument at `at`, not the first, has to be named.
fn needs_name(args: &[&Arg], takes: &Takes<'_>, at: usize) -> bool {
    let ty = |at: usize| takes.get(at).and_then(|(_, ty)| *ty);
    let bare = args.get(at).is_some_and(|arg| matches!(arg.value.kind, ExprKind::Bool(_)));
    bare || args.len() >= 3 || same_type(ty(at), ty(at - 1)) || same_type(ty(at), ty(at + 1))
}

fn call(args: &[Arg], takes: &Takes<'_>) -> Option<Lint> {
    let positional: Vec<&Arg> = args.iter().take_while(|arg| arg.name.is_none()).collect();
    if positional.len() < 2 || positional.len() > takes.len() || positional.iter().any(|arg| arg.is_mut) {
        return None;
    }
    let first = (1..positional.len()).find(|at| needs_name(&positional, takes, *at))?;
    let (name, _) = takes.get(first)?;
    let at = positional.get(first)?.value.span;
    let mut diagnostic = Diagnostic::new(format!("name this argument: `{name}=`"), at)
        .with_help("arguments that are easy to mix up or can't be read from the call have names");
    // What follows a named argument has to be named too.
    for (arg, (name, _)) in positional.iter().zip(takes).skip(first) {
        let start = arg.value.span.start;
        diagnostic = diagnostic.with_fix(Span { start, end: start }, format!("{name}="));
    }
    Some(Lint::new("positional-arguments", diagnostic))
}

pub(crate) fn of(module: &Module) -> Vec<Lint> {
    let callees = Callees::of(module);
    let mut found = Vec::new();
    let mut walked = module.clone();
    fennec_syntax::visit::exprs_mut(&mut walked, &mut |expr| {
        if let ExprKind::Call { callee, args } = &expr.kind {
            found.extend(callees.of_call(callee).and_then(|takes| call(args, takes)));
        }
    });
    found
}
