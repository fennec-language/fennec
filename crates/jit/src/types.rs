//! What the JIT knows about a program's functions. Ints, floats and bools are native
//! machine values; every other value is an object the runtime holds, which native code
//! refers to by a handle. Return types come from the type checker.

use std::collections::{HashMap, HashSet};

use fennec_check::Signatures;
pub(crate) use fennec_layout::{Cap, ListItem, Ty, Word};
use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Imported, Item, Module, Span, UnaryOp};

use fennec_layout::Layouts;

pub(crate) struct Param {
    pub(crate) name: String,
    pub(crate) ty: Ty,
    pub(crate) is_mut: bool,
}

pub(crate) struct Function<'m> {
    pub(crate) def: &'m FnDef,
    /// Its name in the signature table: `name`, or `Type.name` for a method.
    pub(crate) key: String,
    /// Whether it's from the standard library rather than the program.
    pub(crate) prelude: bool,
    pub(crate) params: Vec<Param>,
    pub(crate) ret: Ty,
    /// Whether it gives back a result: it contains `fail` or `?`.
    pub(crate) fallible: bool,
    /// What it gives back directly when it's fallible and that's a counted block.
    pub(crate) direct: Option<Ty>,
}

pub(crate) struct Program<'m> {
    /// The typed IR of the functions it has, by name.
    pub(crate) ir: std::rc::Rc<HashMap<String, fennec_ir::Function>>,
    pub(crate) functions: Vec<Function<'m>>,
    pub(crate) layouts: Layouts,
    /// Functions and methods the JIT doesn't try to compile, like generators.
    pub(crate) excluded: Vec<String>,
    pub(crate) main_span: Span,
    /// The standard library modules the program imports, by the name it uses them under,
    /// like `math` in `math.sqrt(x)` or `std.math` in `std.math.sqrt(x)`.
    pub(crate) modules: HashSet<String>,
    /// The constants whose values native code can write out where they're used.
    pub(crate) constants: HashMap<String, Expr>,
}

/// The standard library modules a program imports, by the name it uses them under.
fn std_modules(module: &Module) -> HashSet<String> {
    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Import(import) => fennec_syntax::std_import(import).ok(),
            _ => None,
        })
        .flatten()
        .filter_map(|imported| match imported {
            Imported::Module { alias, .. } => Some(alias),
            Imported::Name { .. } => None,
        })
        .collect()
}

/// The constants native code can write out where they're used, by name.
fn literal_constants(prelude: &[Module], module: &Module) -> HashMap<String, Expr> {
    prelude
        .iter()
        .chain(std::iter::once(module))
        .flat_map(|module| &module.items)
        .filter_map(|item| match item {
            Item::Const { name, ty: None, value, .. } if is_literal(value) => {
                Some((name.clone(), value.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Whether a constant's value is a literal, or another constant, so reading it can't fail.
fn is_literal(value: &Expr) -> bool {
    match &value.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_) | ExprKind::Name(_) => {
            true
        }
        ExprKind::Unary { op: UnaryOp::Neg, expr } => {
            matches!(expr.kind, ExprKind::Int(_) | ExprKind::Float(_))
        }
        _ => false,
    }
}

pub(crate) fn unsupported(what: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("the JIT can't compile {what} yet"), span)
        .with_help("run the program with `--interp` to use the interpreter")
}

/// The typed IR of the functions the IR path can take, by name. It only takes functions of
/// numbers so far, so only those are worth lowering, which is most of the cost on a big program.
fn typed_ir(
    module: &Module,
    signatures: &Signatures,
    functions: &[Function<'_>],
) -> HashMap<String, fennec_ir::Function> {
    let numbers = |ty: Ty| matches!(ty, Ty::Int | Ty::Float | Ty::Bool);
    let wanted: HashSet<&str> = functions
        .iter()
        .filter(|function| {
            !function.fallible
                && (numbers(function.ret) || function.ret == Ty::Unit)
                && function.params.iter().all(|param| numbers(param.ty))
        })
        .map(|function| function.key.as_str())
        .collect();
    if wanted.is_empty() {
        return HashMap::new();
    }
    let lowered = fennec_ir::lower_some(module, signatures, &|key| wanted.contains(key)).functions;
    lowered.into_iter().map(|function| (function.name.clone(), function)).collect()
}

impl<'m> Program<'m> {
    /// The functions and methods the JIT could compile, the standard library's included.
    /// The rest run in the interpreter when native code calls them.
    pub(crate) fn from_module(
        module: &'m Module,
        prelude: &'m [Module],
        signatures: &Signatures,
    ) -> Result<Self, Diagnostic> {
        Self::build(module, prelude, signatures, &[])
    }

    /// Like `from_module`, for running `tests` rather than `main`: each is a function with
    /// its key, and the program is what they reach.
    pub(crate) fn for_tests(
        module: &'m Module,
        prelude: &'m [Module],
        signatures: &Signatures,
        tests: &'m [(String, FnDef)],
    ) -> Result<Self, Diagnostic> {
        Self::build(module, prelude, signatures, tests)
    }

    fn build(
        module: &'m Module,
        prelude: &'m [Module],
        signatures: &Signatures,
        tests: &'m [(String, FnDef)],
    ) -> Result<Self, Diagnostic> {
        let items = || prelude.iter().chain(std::iter::once(module)).flat_map(|module| &module.items);
        let enums = items().filter_map(|item| match item {
            Item::Enum(def) => Some(def),
            _ => None,
        });
        let mut layouts = Layouts::new(signatures, signatures.structures().map(str::to_owned), enums);
        for item in items() {
            if let Item::Struct(def) = item {
                layouts.add_defaults(def);
            }
        }
        let mut functions = Vec::new();
        let mut excluded = Vec::new();
        let mut main_span = None;
        let mut add = |def: &'m FnDef, key: String, from_prelude: bool| {
            if signatures.get(&key).is_none() {
                return;
            }
            match function(def, key.clone(), signatures, &layouts) {
                Some(function) => functions.push(Function { prelude: from_prelude, ..function }),
                None => excluded.push(key),
            }
        };
        let modules = prelude.iter().map(|module| (module, true)).chain(std::iter::once((module, false)));
        for (item, from_prelude) in modules
            .flat_map(|(module, from_prelude)| module.items.iter().map(move |item| (item, from_prelude)))
        {
            match item {
                Item::Function(def) => {
                    if def.name == "main" {
                        main_span = Some(def.span);
                    }
                    add(def, def.name.clone(), from_prelude);
                }
                Item::Struct(ty) | Item::Enum(ty) | Item::Error(ty) => {
                    for method in &ty.methods {
                        add(method, format!("{}.{}", ty.name, method.name), from_prelude);
                    }
                }
                Item::Extend(extension) => {
                    for method in &extension.methods {
                        add(method, format!("{}.{}", extension.name, method.name), from_prelude);
                    }
                }
                Item::Stmt(stmt) => return Err(unsupported("top-level statements", stmt.span)),
                _ => {}
            }
        }
        for (key, def) in tests {
            add(def, key.clone(), false);
            main_span = main_span.or(Some(def.span));
        }
        let main_span = main_span
            .ok_or_else(|| Diagnostic::new("this program has no `main` function", Span::default()))?;
        // Compile only what a run can reach from `main`, or from the tests it runs.
        let modules: Vec<&Module> = prelude.iter().chain(std::iter::once(module)).collect();
        let roots: Vec<String> = if tests.is_empty() {
            vec!["main".to_owned()]
        } else {
            tests.iter().map(|(key, _)| key.clone()).collect()
        };
        let (reached, mentions) = crate::reach::reachable(&functions, &modules, &roots);
        functions.retain(|function| reached.contains(&function.key));
        excluded.retain(|key| crate::reach::is_mentioned(key, &mentions));
        if tests.is_empty() && !functions.iter().any(|function| function.def.name == "main") {
            return Err(unsupported("this `main`", main_span));
        }
        let (modules, constants) = (std_modules(module), literal_constants(prelude, module));
        let ir = std::rc::Rc::new(typed_ir(module, signatures, &functions));
        Ok(Program { ir, functions, layouts, excluded, main_span, modules, constants })
    }
}

fn function<'m>(
    def: &'m FnDef,
    key: String,
    signatures: &Signatures,
    layouts: &Layouts,
) -> Option<Function<'m>> {
    let signature = signatures.get(&key)?;
    if signature.generator {
        return None;
    }
    let params = def
        .params
        .iter()
        .zip(&signature.params)
        .map(|(param, typed)| Param {
            name: param.name.clone(),
            ty: layouts.ty(&typed.ty),
            is_mut: param.is_mut,
        })
        .collect();
    let ret = if signature.can_fail { Ty::Obj } else { layouts.ty(&signature.returns) };
    // `main` hands its result to whatever started the program.
    let direct = Some(layouts.ty(&signature.returns))
        .filter(|ty| signature.can_fail && ty.counted() && def.name != "main");
    Some(Function { def, key, prelude: false, params, ret, fallible: signature.can_fail, direct })
}
