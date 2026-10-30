//! The signature table and the order functions are checked in. A function is checked the
//! first time a caller needs its signature, or after that in name order, so each body is
//! checked once. A function that's still being checked when it's needed again calls itself,
//! directly or through others, so it has to have its return type written; until it's done,
//! its written signature stands in.

use std::collections::BTreeMap;

use fennec_syntax::{Diagnostic, FnDef, Module};

use crate::effects::{self, Effects};
use crate::function::{Binding, FnCx};
use crate::infer::Slot;
use crate::program::{FnEntry, Program, TypeKind, written};
use crate::types::Type;
use crate::{Body, Checked, ParamType, Signature, Signatures, Types};

enum State {
    /// Being checked now. `recursive` is set when it's needed before it's done.
    Checking {
        recursive: bool,
    },
    Done(Signature),
}

pub(crate) struct Checker {
    pub(crate) program: Program,
    states: BTreeMap<String, State>,
    /// Field types, once worked out. `None` while a field's default is being looked at.
    fields: BTreeMap<String, Option<Type>>,
    /// The type of each name in each function, keyed `function/name`; `Unknown` for a name
    /// that holds different types in different places.
    locals: BTreeMap<String, Type>,
    /// Type variables, by number: what each was settled to, and where it was made.
    pub(crate) vars: Vec<Slot>,
    /// The typed tree, by function.
    pub(crate) typed: BTreeMap<String, Body>,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

impl Checker {
    pub(crate) fn new(module: &Module) -> Self {
        let mut program = Program::default();
        for source in fennec_syntax::PRELUDE {
            if let Ok(prelude) = fennec_syntax::parse(source) {
                program.add(&prelude, true);
            }
        }
        program.add(module, false);
        let diagnostics = std::mem::take(&mut program.import_problems);
        Checker {
            program,
            states: BTreeMap::new(),
            fields: BTreeMap::new(),
            locals: BTreeMap::new(),
            vars: Vec::new(),
            typed: BTreeMap::new(),
            diagnostics,
        }
    }

    pub(crate) fn run(mut self) -> Result<Checked, Vec<Diagnostic>> {
        self.check_declarations();
        let keys: Vec<String> = self.program.functions.keys().cloned().collect();
        for key in &keys {
            if !self.states.contains_key(key) {
                self.check_function(key);
            }
        }
        for test in self.program.tests.clone() {
            FnCx::check_test(&mut self, &test);
        }
        let user_types: Vec<String> = self
            .program
            .types
            .values()
            .filter(|info| !info.prelude)
            .map(|info| info.def.name.clone())
            .collect();
        for name in user_types {
            FnCx::check_fields(&mut self, &name);
        }
        let structs = self.structs();
        self.settle_typed();
        if self.diagnostics.is_empty() {
            self.unknowns_left();
        }
        if self.diagnostics.is_empty() {
            let locals = std::mem::take(&mut self.locals);
            let functions = self
                .states
                .into_iter()
                .filter_map(|(key, state)| match state {
                    State::Done(signature) => Some((key, signature)),
                    State::Checking { .. } => None,
                })
                .collect();
            let signatures = Signatures { functions, structs, locals };
            return Ok(Checked { signatures, types: Types { functions: self.typed } });
        }
        let mut diagnostics = self.diagnostics;
        diagnostics.sort_by(|a, b| (a.span.start, &a.message).cmp(&(b.span.start, &b.message)));
        diagnostics.dedup();
        Err(diagnostics)
    }

    /// Reports types written in signatures and interfaces that don't mean anything, and
    /// type parameters bound to something that isn't an interface.
    fn check_declarations(&mut self) {
        let mut problems = Vec::new();
        let functions =
            self.program.functions.values().filter(|entry| !entry.prelude).map(|entry| &entry.def);
        let methods = self.program.interfaces.values().flat_map(|interface| &interface.methods);
        for def in functions.chain(methods) {
            let generics: Vec<String> = def.generics.iter().map(|generic| generic.name.clone()).collect();
            for bound in def.generics.iter().filter_map(|generic| generic.bound.as_ref()) {
                if !self.program.interfaces.contains_key(&bound.name) {
                    problems.push(
                        Diagnostic::new(format!("`{}` isn't an interface", bound.name), bound.span)
                            .with_help("a type parameter can be bound to an interface, like `T: Shape`"),
                    );
                }
            }
            let written = def.params.iter().filter_map(|param| param.ty.as_ref()).chain(def.ret.as_ref());
            problems.extend(written.filter_map(|ty| self.program.resolve_in(ty, &generics).err()));
        }
        self.diagnostics.extend(problems);
    }

    /// A function's signature, checking its body first if that's the only way to know its
    /// return type.
    pub(crate) fn signature(&mut self, key: &str) -> Option<Signature> {
        let has_return_type = self.program.functions.get(key)?.def.ret.is_some();
        match self.states.get_mut(key) {
            Some(State::Done(signature)) => return Some(signature.clone()),
            Some(State::Checking { recursive }) => {
                *recursive |= !has_return_type;
                return self.declared(key);
            }
            None => self.check_function(key),
        }
        match self.states.get(key) {
            Some(State::Done(signature)) => Some(signature.clone()),
            _ => None,
        }
    }

    /// The return type written in a function's signature, or `Unknown` if none is.
    fn written_return(&self, key: &str) -> Type {
        let Some(entry) = self.program.functions.get(key) else { return Type::Unknown };
        let generics: Vec<String> = entry.def.generics.iter().map(|generic| generic.name.clone()).collect();
        let written = entry.def.ret.as_ref().map(|ret| self.program.resolve_in(ret, &generics));
        written.and_then(Result::ok).unwrap_or(Type::Unknown)
    }

    /// A signature from what's written: the return type is unknown if it isn't written.
    fn declared(&self, key: &str) -> Option<Signature> {
        let entry = self.program.functions.get(key)?;
        let returned = self.written_return(key);
        let effects = effects::of_body(&entry.def.body);
        Some(Signature {
            params: self.params(entry),
            returns: wrap(returned, Type::Unknown, effects),
            can_fail: effects.fails,
            generator: effects.yields,
        })
    }

    fn params(&self, entry: &FnEntry) -> Vec<ParamType> {
        let generics: Vec<String> = entry.def.generics.iter().map(|generic| generic.name.clone()).collect();
        entry
            .def
            .params
            .iter()
            .map(|param| ParamType {
                name: param.name.clone(),
                ty: match (&param.ty, &entry.owner) {
                    (Some(ty), _) => self.program.resolve_in(ty, &generics).unwrap_or(Type::Unknown),
                    (None, Some(owner)) => Type::named(owner),
                    (None, None) => Type::Unknown,
                },
                is_mut: param.is_mut,
            })
            .collect()
    }

    fn check_function(&mut self, key: &str) {
        self.check_with(key, Vec::new());
    }

    /// Checks a function defined inside another, which can read the names around it.
    pub(crate) fn check_nested(&mut self, key: &str, def: &FnDef, captured: Vec<Binding>, prelude: bool) {
        self.program.functions.insert(key.to_owned(), FnEntry { def: def.clone(), owner: None, prelude });
        self.check_with(key, captured);
    }

    fn check_with(&mut self, key: &str, captured: Vec<Binding>) {
        let Some(entry) = self.program.functions.get(key) else { return };
        let (def, prelude) = (entry.def.clone(), entry.prelude);
        let Some(mut signature) = self.declared(key) else { return };
        self.states.insert(key.to_owned(), State::Checking { recursive: false });
        let effects = effects::of_body(&def.body);
        let body = FnCx::check_function(self, key, &def, &signature.params, captured, !prelude);
        let value = match &def.ret {
            Some(_) => self.written_return(key),
            None => body.value,
        };
        // Anything still open in what it gives back has been reported, and callers
        // mustn't settle it.
        signature.returns = forget_vars(&wrap(value, body.error, effects));
        if let Some(State::Checking { recursive: true }) = self.states.get(key)
            && def.ret.is_none()
            && !prelude
        {
            self.diagnostics.push(needs_return_type(&def, &body.suggestion));
        }
        self.states.insert(key.to_owned(), State::Done(signature));
    }

    /// The type of a field of a struct, error or enum case, from its written type or default.
    pub(crate) fn field_type(&mut self, owner: &str, case: Option<&str>, field: &str) -> Type {
        let key = format!("{owner}.{}.{field}", case.unwrap_or(""));
        match self.fields.get(&key) {
            Some(Some(ty)) => return ty.clone(),
            Some(None) => return Type::Unknown,
            None => {}
        }
        let Some(info) = self.program.type_info(owner) else { return Type::Unknown };
        let fields = match case {
            Some(case) => info.def.cases.iter().find(|c| c.name == case).map(|c| c.fields.as_slice()),
            None => Some(info.def.fields.as_slice()),
        };
        let Some(found) = fields.and_then(|fields| fields.iter().find(|f| f.name == field)).cloned() else {
            return Type::Unknown;
        };
        self.fields.insert(key.clone(), None);
        let report = !info.prelude;
        let ty = match (&found.ty, &found.default) {
            (Some(ty), _) => match self.program.resolve(ty) {
                Ok(ty) => ty,
                Err(problem) => {
                    self.diagnostics.push(problem);
                    Type::Unknown
                }
            },
            (None, Some(default)) => {
                let ty = FnCx::type_of_default(self, default);
                if ((!ty.is_known() && ty != Type::Unknown) || ty == Type::None) && report {
                    self.diagnostics.push(field_needs_type(&found.name, &ty, default.span));
                }
                if ty == Type::None { Type::Unknown } else { forget_vars(&ty) }
            }
            (None, None) => Type::Unknown,
        };
        self.fields.insert(key, Some(ty.clone()));
        ty
    }

    /// Settles every type in the typed tree, now that every function has been checked.
    fn settle_typed(&mut self) {
        let mut typed = std::mem::take(&mut self.typed);
        for body in typed.values_mut() {
            for ty in body.exprs.values_mut().chain(body.names.values_mut()) {
                *ty = self.settled(ty);
            }
        }
        self.typed = typed;
        let locals = std::mem::take(&mut self.locals);
        self.locals = locals.into_iter().map(|(key, ty)| (key, self.settled(&ty))).collect();
    }

    /// The last line of defense: an expression in the program whose type still isn't known,
    /// though nothing was reported, is reported now. Only the innermost such expressions
    /// are, since what's around them is unknown because of them.
    fn unknowns_left(&mut self) {
        let prelude: Vec<String> = self
            .program
            .functions
            .iter()
            .filter(|(_, entry)| entry.prelude)
            .map(|(key, _)| key.clone())
            .collect();
        let mut unknown: Vec<(u32, u32)> = self
            .typed
            .iter()
            .filter(|(key, _)| !prelude.contains(key))
            .flat_map(|(_, body)| body.exprs.iter())
            .filter(|(_, ty)| !ty.is_known())
            .map(|(span, _)| *span)
            .collect();
        unknown.sort_unstable();
        unknown.dedup();
        let innermost: Vec<(u32, u32)> = unknown
            .iter()
            .filter(|outer| {
                !unknown.iter().any(|inner| inner != *outer && outer.0 <= inner.0 && inner.1 <= outer.1)
            })
            .copied()
            .collect();
        let unknown_signatures: Vec<Diagnostic> = self
            .states
            .iter()
            .filter_map(|(key, state)| match state {
                State::Done(signature) if !prelude.contains(key) => Some((key, signature)),
                _ => None,
            })
            .filter(|(_, signature)| {
                !signature.returns.is_known() || signature.params.iter().any(|param| !param.ty.is_known())
            })
            .filter_map(|(key, _)| self.program.functions.get(key))
            .map(|entry| {
                Diagnostic::new(format!("can't work out the types of `{}`", entry.def.name), entry.def.span)
                    .with_help("write its parameters' types and its return type")
            })
            .collect();
        self.diagnostics.extend(unknown_signatures);
        for (start, end) in innermost {
            self.diagnostics.push(
                Diagnostic::new("can't work out the type of this", fennec_syntax::Span { start, end })
                    .with_help("name it with its type written, like `value: int = ...`"),
            );
        }
    }

    /// Records the type a function's name holds, for compilers to use.
    pub(crate) fn note_local(&mut self, function: &str, name: &str, ty: &Type) {
        let key = format!("{function}/{name}");
        match self.locals.get(&key) {
            Some(known) if known != ty => {
                self.locals.insert(key, Type::Unknown);
            }
            Some(_) => {}
            None => {
                self.locals.insert(key, ty.clone());
            }
        }
    }

    /// Every struct's fields and their types, in the order they're written.
    fn structs(&mut self) -> BTreeMap<String, crate::Struct> {
        let structs: Vec<(String, Vec<String>, bool)> = self
            .program
            .types
            .values()
            .filter(|info| info.kind == TypeKind::Struct)
            .map(|info| {
                let names = info.def.fields.iter().map(|field| field.name.clone()).collect();
                let rules = info.def.fields.iter().any(|field| field.rule.is_some());
                (info.def.name.clone(), names, rules)
            })
            .collect();
        structs
            .into_iter()
            .map(|(name, fields, rules)| {
                let fields = fields
                    .into_iter()
                    .map(|field| {
                        let ty = self.field_type(&name, None, &field);
                        (field, ty)
                    })
                    .collect();
                (name, crate::Struct { fields, rules })
            })
            .collect()
    }

    pub(crate) fn is_error_type(&self, ty: &Type) -> bool {
        self.program.kind_of(ty) == Some(TypeKind::Error)
    }
}

/// A type with every variable left in it replaced by `Unknown`, for keeping in a signature
/// or a field's type, where no later check may settle it.
fn forget_vars(ty: &Type) -> Type {
    ty.map(&mut |part| matches!(part, Type::Var(_)).then_some(Type::Unknown))
}

fn field_needs_type(name: &str, ty: &Type, span: fennec_syntax::Span) -> Diagnostic {
    let example = match ty {
        Type::List(_) => format!("{name}: list[int] = []"),
        Type::Dict(..) => format!("{name}: dict[str, int] = {{}}"),
        Type::Set(_) => format!("{name}: set[int] = set()"),
        _ => format!("{name}: Enemy? = None"),
    };
    Diagnostic::new(format!("can't tell the type of `{name}` from its default"), span)
        .with_help(format!("write its type, like `{example}`"))
}

/// What a call gives back, from what the body gives back.
fn wrap(value: Type, error: Type, effects: Effects) -> Type {
    if effects.yields {
        return value;
    }
    match value {
        // A function that can fail passes a result it gives back through unchanged.
        Type::Result(..) => value,
        value if effects.fails => Type::result(value, error),
        value => value,
    }
}

fn needs_return_type(def: &FnDef, suggestion: &Type) -> Diagnostic {
    let params: Vec<String> = def
        .params
        .iter()
        .map(|param| {
            let prefix = if param.is_mut { "mut " } else { "" };
            match &param.ty {
                Some(ty) => format!("{prefix}{}: {}", param.name, written(ty)),
                None => format!("{prefix}{}", param.name),
            }
        })
        .collect();
    let signature = format!("def {}({})", def.name, params.join(", "));
    let help = if *suggestion == Type::Unknown {
        format!("write the type it returns after the parameters: `{signature} -> <type>:`")
    } else {
        format!("write it after the parameters: `{signature} -> {suggestion}:`")
    };
    Diagnostic::new(format!("`{}` calls itself, so its return type has to be written", def.name), def.span)
        .with_help(help)
}
