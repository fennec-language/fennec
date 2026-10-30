//! The signature table and the order functions are checked in. A function is checked the
//! first time a caller needs its signature, or after that in name order, so each body is
//! checked once. A function that's still being checked when it's needed again calls itself,
//! directly or through others, so it has to have its return type written; until it's done,
//! its written signature stands in.

use std::collections::BTreeMap;

use fennec_syntax::{Diagnostic, FnDef, Module};

use crate::effects::{self, Effects};
use crate::function::{Binding, FnCx};
use crate::program::{FnEntry, Program, TypeKind, written};
use crate::types::Type;
use crate::{ParamType, Signature, Signatures};

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
            diagnostics,
        }
    }

    pub(crate) fn run(mut self) -> Result<Signatures, Vec<Diagnostic>> {
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
        if self.diagnostics.is_empty() {
            let structs = self.structs();
            let locals = std::mem::take(&mut self.locals);
            let functions = self
                .states
                .into_iter()
                .filter_map(|(key, state)| match state {
                    State::Done(signature) => Some((key, signature)),
                    State::Checking { .. } => None,
                })
                .collect();
            return Ok(Signatures { functions, structs, locals });
        }
        let mut diagnostics = self.diagnostics;
        diagnostics.sort_by(|a, b| (a.span.start, &a.message).cmp(&(b.span.start, &b.message)));
        diagnostics.dedup();
        Err(diagnostics)
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

    /// A signature from what's written: the return type is unknown if it isn't written.
    fn declared(&self, key: &str) -> Option<Signature> {
        let entry = self.program.functions.get(key)?;
        let returned = entry.def.ret.as_ref().map_or(Type::Unknown, |ret| self.program.resolve(ret));
        let effects = effects::of_body(&entry.def.body);
        Some(Signature {
            params: self.params(entry),
            returns: wrap(returned, Type::Unknown, effects),
            can_fail: effects.fails,
            generator: effects.yields,
        })
    }

    fn params(&self, entry: &FnEntry) -> Vec<ParamType> {
        entry
            .def
            .params
            .iter()
            .map(|param| ParamType {
                name: param.name.clone(),
                ty: match (&param.ty, &entry.owner) {
                    (Some(ty), _) => self.program.resolve(ty),
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
        let value = def.ret.as_ref().map_or(body.value, |ret| self.program.resolve(ret));
        signature.returns = wrap(value, body.error, effects);
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
        let ty = match (&found.ty, &found.default) {
            (Some(ty), _) => self.program.resolve(ty),
            (None, Some(default)) => match FnCx::type_of_default(self, default) {
                Type::None => Type::Unknown,
                ty => ty,
            },
            (None, None) => Type::Unknown,
        };
        self.fields.insert(key, Some(ty.clone()));
        ty
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
