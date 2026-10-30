//! What the JIT knows about a program's functions. Ints, floats and bools are native
//! machine values; every other value is an object the runtime holds, which native code
//! refers to by a handle. Return types come from the type checker.

use std::collections::HashSet;

use fennec_check::Signatures;
use fennec_syntax::{Diagnostic, FnDef, Imported, Item, Module, Span, StmtKind};

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
}

impl Ty {
    /// Whether values of this kind are addresses of blocks with an owner count.
    pub(crate) fn counted(self) -> bool {
        matches!(self, Ty::Boxed(_) | Ty::Str)
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
        }
    }
}

/// The capabilities `main` can be given.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Cap {
    Stdout,
    Fs,
    Clock,
}

impl Cap {
    pub(crate) fn named(name: &str) -> Option<Cap> {
        match name {
            "Stdout" => Some(Cap::Stdout),
            "Fs" => Some(Cap::Fs),
            "Clock" => Some(Cap::Clock),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Cap::Stdout => "Stdout",
            Cap::Fs => "Fs",
            Cap::Clock => "Clock",
        }
    }

    /// How the runtime tells capabilities apart.
    pub(crate) fn kind(self) -> i64 {
        match self {
            Cap::Stdout => 0,
            Cap::Fs => 1,
            Cap::Clock => 2,
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
}

pub(crate) fn unsupported(what: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("the JIT can't compile {what} yet"), span)
        .with_help("run the program without `--jit` to use the interpreter")
}

impl<'m> Program<'m> {
    /// The functions and methods the JIT could compile, the standard library's included.
    /// The rest run in the interpreter when native code calls them.
    pub(crate) fn from_module(
        module: &'m Module,
        prelude: &'m [Module],
        signatures: &Signatures,
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
        let main_span = main_span
            .ok_or_else(|| Diagnostic::new("this program has no `main` function", Span::default()))?;
        // Compile only what a run can reach from `main`.
        let modules: Vec<&Module> = prelude.iter().chain(std::iter::once(module)).collect();
        let (reached, mentions) = crate::reach::reachable(&functions, &modules);
        functions.retain(|function| reached.contains(&function.key));
        excluded.retain(|key| crate::reach::is_mentioned(key, &mentions));
        if !functions.iter().any(|function| function.def.name == "main") {
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
        Ok(Program { functions, layouts, excluded, main_span, modules })
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
