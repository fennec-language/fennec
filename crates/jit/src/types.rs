//! What the JIT knows about a program's functions. Ints, floats and bools are native
//! machine values; every other value is an object the runtime holds, which native code
//! refers to by a handle. Return types come from the type checker.

use std::collections::{HashMap, HashSet};

use fennec_check::Signatures;
pub(crate) use fennec_interp::embed::dicts::Word;
use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Imported, Item, Module, Span, StmtKind, UnaryOp};

use crate::layout::Layouts;

/// How native code holds a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Ty {
    Int,
    Float,
    Bool,
    /// Nothing at all: what a function without a value returns.
    Unit,
    /// A capability, like `stdout`, which needs nothing at run time.
    Cap(Cap),
    /// Anything else, held by the runtime.
    Obj,
    /// A plain struct, in memory native code owns, which it refers to by address. The
    /// number picks its layout.
    Struct(u32),
    /// A boxed struct or enum: the address of a block the runtime owns, shared by every
    /// copy. The number picks its layout.
    Boxed(u32),
    /// A string: the address of a block the runtime owns, shared by every copy.
    Str,
    /// A dict with these keys and values: the address of a block the runtime owns, shared
    /// by every copy until one changes it.
    Dict(Word, Word),
    /// A list of numbers, bools or plain structs, packed one after another: the address of
    /// a block the runtime owns, shared by every copy until one changes it.
    List(ListItem),
    /// A function known while compiling, by its number there, which needs nothing at run
    /// time: calls to it are lowered right where they are.
    Known(u32),
}

/// What a native list's items are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ListItem {
    Int,
    Float,
    Bool,
    /// A plain struct or enum, by its layout's number.
    Struct(u32),
    /// The address of a string's block, which the list owns.
    Str,
    /// The address of a boxed value's block, which the list owns, by its layout's number.
    Boxed(u32),
}

impl ListItem {
    /// The kind of item a list of `ty` holds natively, if it can.
    pub(crate) fn of(ty: Ty) -> Option<ListItem> {
        Some(match ty {
            Ty::Int => ListItem::Int,
            Ty::Float => ListItem::Float,
            Ty::Bool => ListItem::Bool,
            Ty::Struct(id) => ListItem::Struct(id),
            Ty::Str => ListItem::Str,
            Ty::Boxed(id) => ListItem::Boxed(id),
            _ => return None,
        })
    }

    /// How native code holds one item.
    pub(crate) fn ty(self) -> Ty {
        match self {
            ListItem::Int => Ty::Int,
            ListItem::Float => Ty::Float,
            ListItem::Bool => Ty::Bool,
            ListItem::Struct(id) => Ty::Struct(id),
            ListItem::Str => Ty::Str,
            ListItem::Boxed(id) => Ty::Boxed(id),
        }
    }
}

impl Ty {
    /// Whether values of this kind are addresses of blocks with an owner count.
    pub(crate) fn counted(self) -> bool {
        matches!(self, Ty::Boxed(_) | Ty::Str | Ty::Dict(..) | Ty::List(_))
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Ty::Int => "int",
            Ty::Float => "float",
            Ty::Bool => "bool",
            Ty::Unit => "nothing",
            Ty::Cap(cap) => cap.name(),
            Ty::Obj => "value",
            Ty::Struct(_) => "struct",
            Ty::Boxed(_) => "boxed value",
            Ty::Str => "str",
            Ty::Dict(..) => "dict",
            Ty::List(_) => "list",
            Ty::Known(_) => "function",
        }
    }
}

/// The capabilities `main` can be given.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Cap {
    Stdout,
    Fs,
    Clock,
    Args,
    Stdin,
}

impl Cap {
    pub(crate) fn named(name: &str) -> Option<Cap> {
        match name {
            "Stdout" => Some(Cap::Stdout),
            "Fs" => Some(Cap::Fs),
            "Clock" => Some(Cap::Clock),
            "Args" => Some(Cap::Args),
            "Stdin" => Some(Cap::Stdin),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Cap::Stdout => "Stdout",
            Cap::Fs => "Fs",
            Cap::Clock => "Clock",
            Cap::Args => "Args",
            Cap::Stdin => "Stdin",
        }
    }

    /// How the runtime tells capabilities apart.
    pub(crate) fn kind(self) -> i64 {
        match self {
            Cap::Stdout => 0,
            Cap::Fs => 1,
            Cap::Clock => 2,
            Cap::Args => 3,
            Cap::Stdin => 4,
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
    /// Its name in the signature table: `name`, or `Type.name` for a method.
    pub(crate) key: String,
    /// Whether it's from the standard library rather than the program.
    pub(crate) prelude: bool,
    pub(crate) params: Vec<Param>,
    pub(crate) ret: Ty,
    /// Whether it gives back a result: it contains `fail` or `?`.
    pub(crate) fallible: bool,
}

pub(crate) struct Program<'m> {
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
        let modules = module
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
            .collect();
        let constants = prelude
            .iter()
            .chain(std::iter::once(module))
            .flat_map(|module| &module.items)
            .filter_map(|item| match item {
                Item::Const { name, ty: None, value, .. } if is_literal(value) => {
                    Some((name.clone(), value.clone()))
                }
                _ => None,
            })
            .collect();
        Ok(Program { functions, layouts, excluded, main_span, modules, constants })
    }
}

fn function<'m>(
    def: &'m FnDef,
    key: String,
    signatures: &Signatures,
    layouts: &Layouts,
) -> Option<Function<'m>> {
    let signature = signatures.get(&key)?;
    if signature.generator || def.body.iter().any(|stmt| matches!(stmt.kind, StmtKind::Def(_))) {
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
    Some(Function { def, key, prelude: false, params, ret, fallible: signature.can_fail })
}
