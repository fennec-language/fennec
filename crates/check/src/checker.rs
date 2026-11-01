//! The signature table and the order functions are checked in. A function is checked the
//! first time a caller needs its signature, for the errors it can fail with, or after that in
//! name order. A function that's still being checked when it's needed again calls itself,
//! directly or through others; its written signature stands in, and the functions in such a
//! cycle are checked again until their errors stop growing.

use std::collections::{BTreeMap, BTreeSet};

use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Generic, Module, Span};

use crate::effects::{self, Effects};
use crate::function::{Binding, FnCx};
use crate::infer::Slot;
use crate::program::{FnEntry, Program, TypeKind, written, written_ret};
use crate::types::{Type, a};
use crate::{Body, Checked, ParamType, Signature, Signatures, Types};

enum State {
    /// Being checked now.
    Checking,
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
    /// The enum chosen for each leading-dot case that more than one enum has, by the span
    /// of the case, as the type expected where it's written decided.
    pub(crate) cases: BTreeMap<(u32, u32), String>,
    /// The calls to `sum` of floats with no start, which start at `0.0`.
    pub(crate) float_sums: BTreeSet<(u32, u32)>,
    /// The defaults each call leaves out.
    pub(crate) defaults: BTreeMap<(u32, u32), Vec<(String, Expr)>>,
    /// The type of each generator expression, which printing one shows.
    pub(crate) generator_types: BTreeMap<(u32, u32), String>,
    /// The `!=`s on a type with its own `def ==`, which become `not (a == b)`.
    pub(crate) not_equals: BTreeSet<(u32, u32)>,
    /// The `parallel(xs)` that a `for mut` loops over, which becomes `xs`: running in order
    /// is a correct way to run it, and it changes the items where they are.
    pub(crate) parallel_changes: BTreeSet<(u32, u32)>,
    /// The fields and method calls on a value that might be missing, in a chain an `else`
    /// ends, which are written in as optional.
    pub(crate) optional_steps: BTreeSet<(u32, u32)>,
    /// What each name for a standard library function written in Fennec means.
    pub(crate) std_names: BTreeMap<(u32, u32), String>,
    /// Each constant's type, once worked out. `None` while its value is being checked.
    pub(crate) const_types: BTreeMap<String, Option<Type>>,
    /// The functions being checked, innermost last.
    checking: Vec<String>,
    /// Functions that used a signature that may still grow: one being checked, so its
    /// errors aren't all known yet, or one waiting here itself. They're checked again.
    revisit: BTreeSet<String>,
    /// What each function to check again gave back last time, for calls to it while it's
    /// being checked again.
    previous: BTreeMap<String, Signature>,
    /// The functions that used each function's signature, so they're checked again when it
    /// changes.
    callers: BTreeMap<String, BTreeSet<String>>,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

/// How many times functions in a cycle of calls are checked again before the errors they
/// can fail with stop growing.
const REVISITS: usize = 16;

impl Checker {
    pub(crate) fn new(module: &Module) -> Self {
        let mut program = Program::default();
        for prelude in fennec_syntax::prelude_modules().unwrap_or_default() {
            program.add(prelude, true);
        }
        program.prelude_imports = std::mem::take(&mut program.imports);
        program.prelude_modules = std::mem::take(&mut program.modules);
        program.std_functions.clear();
        program.add(module, false);
        let mut diagnostics = std::mem::take(&mut program.problems);
        diagnostics.extend(crate::imports::unimported(module, &program));
        Checker {
            program,
            states: BTreeMap::new(),
            fields: BTreeMap::new(),
            locals: BTreeMap::new(),
            vars: Vec::new(),
            typed: BTreeMap::new(),
            cases: BTreeMap::new(),
            float_sums: BTreeSet::new(),
            defaults: BTreeMap::new(),
            generator_types: BTreeMap::new(),
            not_equals: BTreeSet::new(),
            parallel_changes: BTreeSet::new(),
            optional_steps: BTreeSet::new(),
            std_names: BTreeMap::new(),
            const_types: BTreeMap::new(),
            checking: Vec::new(),
            revisit: BTreeSet::new(),
            previous: BTreeMap::new(),
            callers: BTreeMap::new(),
            diagnostics,
        }
    }

    /// Checks everything, or for `reached_only`, the program's own code and just the
    /// standard library functions it reaches: the library's other code can't affect a run.
    pub(crate) fn run(mut self, reached_only: bool) -> Result<Checked, Vec<Diagnostic>> {
        self.check_declarations();
        let keys: Vec<String> = self
            .program
            .functions
            .iter()
            .filter(|(_, entry)| !(reached_only && entry.prelude))
            .map(|(key, _)| key.clone())
            .collect();
        for key in &keys {
            if !self.states.contains_key(key) {
                self.check_function(key);
            }
        }
        self.revisit_cycles();
        let constants: Vec<String> = self.program.constants.keys().cloned().collect();
        for name in constants {
            self.constant_type(&name);
        }
        for test in self.program.tests.clone() {
            FnCx::check_test(&mut self, &test);
        }
        let types: Vec<String> = self
            .program
            .types
            .values()
            .filter(|info| !(reached_only && info.prelude))
            .map(|info| info.def.name.clone())
            .collect();
        for name in types {
            FnCx::check_fields(&mut self, &name);
        }
        let structs = self.structs(TypeKind::Struct);
        let errors = self.structs(TypeKind::Error);
        self.settle_typed();
        if self.diagnostics.is_empty() {
            self.unknowns_left();
        }
        let places = if self.diagnostics.is_empty() { self.names_for_places() } else { BTreeSet::new() };
        if self.diagnostics.is_empty() {
            let enums = self.enums();
            // An enum nothing uses isn't checked, so reading its fields can't report anything.
            self.diagnostics.clear();
            let locals = std::mem::take(&mut self.locals);
            let functions = self
                .states
                .into_iter()
                .filter_map(|(key, state)| match state {
                    State::Done(signature) => Some((key, signature)),
                    State::Checking => None,
                })
                .collect();
            let signatures =
                Signatures { functions, structs, enums, errors, locals, types: Types::default() };
            let types = Types { functions: self.typed };
            return Ok(Checked {
                signatures,
                types,
                cases: self.cases,
                float_sums: self.float_sums,
                places,
                defaults: self.defaults,
                generator_types: self.generator_types,
                not_equals: self.not_equals,
                parallel_changes: self.parallel_changes,
                optional_steps: self.optional_steps,
                std_names: self.std_names,
            });
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
        for entry in self.program.functions.values().filter(|entry| !entry.prelude) {
            let generics = self.program.generics_of(entry);
            let names: Vec<String> = generics.iter().map(|generic| generic.name.clone()).collect();
            let def = &entry.def;
            let written = def.params.iter().filter_map(|param| param.ty.as_ref()).chain(def.ret.as_ref());
            problems.extend(written.filter_map(|ty| self.program.resolve_in(ty, &names).err()));
            problems.extend(self.bad_bounds(&def.generics));
        }
        for interface in self.program.interfaces.values() {
            for def in &interface.methods {
                let written = def.params.iter().filter_map(|param| param.ty.as_ref()).chain(def.ret.as_ref());
                let own = ["Self".to_owned()];
                problems.extend(written.filter_map(|ty| self.program.resolve_in(ty, &own).err()));
            }
        }
        for info in self.program.types.values().filter(|info| !info.prelude) {
            problems.extend(self.bad_bounds(&info.def.generics));
        }
        self.diagnostics.extend(problems);
    }

    /// Bounds that aren't interfaces, as in `T: Enemy`.
    fn bad_bounds(&self, generics: &[Generic]) -> Vec<Diagnostic> {
        generics
            .iter()
            .flat_map(|generic| &generic.bounds)
            .filter(|bound| !self.program.is_interface(&bound.name))
            .map(|bound| {
                Diagnostic::new(format!("`{}` isn't an interface", bound.name), bound.span).with_help(
                    "a type parameter can be bound to an interface, like `T: Ordered` or `T: Shape`",
                )
            })
            .collect()
    }

    /// A test with no inputs has a signature like a function's, under its key in the typed
    /// tree, so a compiler can treat it as one. It gives back nothing, or an error.
    pub(crate) fn record_test(&mut self, key: String, body: &[fennec_syntax::Stmt]) {
        let effects = effects::of_body(body);
        let signature = Signature {
            params: Vec::new(),
            returns: Type::Unit,
            can_fail: effects.fails,
            generator: effects.yields,
        };
        self.states.insert(key, State::Done(signature));
    }

    /// A function's signature, checking its body first if that's the only way to know its
    /// return type.
    pub(crate) fn signature(&mut self, key: &str) -> Option<Signature> {
        self.program.functions.get(key)?;
        if let Some(caller) = self.checking.last() {
            let caller = self.outermost(caller);
            self.callers.entry(self.outermost(key)).or_default().insert(caller);
        }
        match self.states.get_mut(key) {
            Some(State::Done(signature)) => {
                let signature = signature.clone();
                if self.revisit.contains(&self.outermost(key)) {
                    self.mark_stale();
                }
                return Some(signature);
            }
            Some(State::Checking) => {
                // Calls to a function from inside itself use what's written, or what it
                // gave back last time when it's checked again. Its errors aren't known yet,
                // so the caller is checked again once they are.
                self.mark_stale();
                return self.previous.get(key).cloned().or_else(|| self.declared(key));
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
        let generics = self.generic_names(entry);
        let written = written_ret(&entry.def).map(|ret| self.program.resolve_in(ret, &generics));
        written.and_then(Result::ok).unwrap_or(Type::Unknown)
    }

    /// A signature from what's written, for a function still being checked.
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

    fn generic_names(&self, entry: &FnEntry) -> Vec<String> {
        self.program.generics_of(entry).into_iter().map(|generic| generic.name).collect()
    }

    /// Works out every `mut x = place` in the program's own functions.
    fn names_for_places(&mut self) -> BTreeSet<(u32, u32)> {
        let mut places = BTreeSet::new();
        let mut problems = Vec::new();
        for (key, entry) in self.program.functions.iter().filter(|(_, entry)| !entry.prelude) {
            let (found, named) = crate::alias::check_function(self, &entry.def, self.typed.get(key));
            problems.extend(found);
            places.extend(named.into_iter().map(|span| (span.start, span.end)));
        }
        self.diagnostics.extend(problems);
        places
    }

    fn params(&self, entry: &FnEntry) -> Vec<ParamType> {
        let generics = self.generic_names(entry);
        entry
            .def
            .params
            .iter()
            .map(|param| ParamType {
                name: param.name.clone(),
                ty: match (&param.ty, &entry.owner) {
                    (Some(ty), _) => self.program.resolve_in(ty, &generics).unwrap_or(Type::Unknown),
                    (None, Some(owner)) if param.is_self => self.program.method_self_type(entry, owner),
                    (None, _) => self.program.default_type(param).unwrap_or(Type::Unknown),
                },
                is_mut: param.is_mut,
                default: param.default.clone(),
            })
            .collect()
    }

    fn check_function(&mut self, key: &str) {
        self.check_with(key, Vec::new());
    }

    /// Checks a function defined inside another, which can read the names around it.
    pub(crate) fn check_nested(
        &mut self,
        key: &str,
        def: &FnDef,
        captured: Vec<Binding>,
        prelude: bool,
        inherited: Vec<Generic>,
    ) {
        // Checked again with the function around it: what it gave back last time stands in
        // for calls to itself.
        if let Some(State::Done(signature)) = self.states.get(key) {
            self.previous.insert(key.to_owned(), known_errors(signature.clone()));
        }
        let entry = FnEntry { def: def.clone(), owner: None, prelude, inherited, extension: Vec::new() };
        self.program.functions.insert(key.to_owned(), entry);
        self.check_with(key, captured);
    }

    /// Whether a body ends in a call made for what it changes: one that takes something `mut`,
    /// or a method that changes what it's called on, like `board.add(score)`.
    fn ends_in_change(&self, key: &str, body: &[fennec_syntax::Stmt]) -> bool {
        let Some(fennec_syntax::StmtKind::Expr(last)) = body.last().map(|stmt| &stmt.kind) else {
            return false;
        };
        let ExprKind::Call { callee, args } = &last.kind else { return false };
        if args.iter().any(|arg| arg.is_mut) {
            return true;
        }
        let ExprKind::Attr { object, name, .. } = &callee.kind else { return false };
        let Some(ty) = self.typed.get(key).and_then(|body| body.expr(object.span)) else { return false };
        if let Some(found) = crate::builtins::method(ty, name) {
            return found.changes;
        }
        let Some(owner) = ty.type_name() else { return false };
        let found = self.program.method(owner, name);
        found.is_some_and(|(_, entry)| entry.def.params.first().is_some_and(|param| param.is_mut))
    }

    fn check_with(&mut self, key: &str, captured: Vec<Binding>) {
        let Some(entry) = self.program.functions.get(key) else { return };
        let (def, prelude) = (entry.def.clone(), entry.prelude);
        let Some(mut signature) = self.declared(key) else { return };
        self.states.insert(key.to_owned(), State::Checking);
        let effects = effects::of_body(&def.body);
        self.checking.push(key.to_owned());
        let body = FnCx::check_function(self, key, &def, &signature.params, captured, !prelude);
        self.checking.pop();
        // No arrow means it gives back nothing. One that gives back a value anyway has to
        // write its type, and until it does, callers see the value so they aren't reported
        // as well.
        let value = self.settled(&body.value);
        // With no arrow, a call at the end made for what it changes is a statement, and what it
        // gives back is dropped. A failure still has to be handled.
        let dropped =
            def.ret.is_none() && self.ends_in_change(key, &def.body) && !matches!(value, Type::Result(..));
        let gives_back = !dropped && !matches!(value, Type::Unit | Type::Unknown);
        let value = match (written_ret(&def), &def.ret) {
            (Some(_), _) => self.written_return(key),
            (None, None) if gives_back => value,
            (None, _) => Type::Unit,
        };
        // A value that isn't fully known has been reported already.
        if def.ret.is_none() && gives_back && value.is_known() && !prelude {
            let fixable = self.can_write(&value);
            self.diagnostics.push(missing_return_type(&def, &value, effects.yields, fixable));
        }
        if let (Some(ret), Some(arrow)) = (&def.ret, def.arrow)
            && matches!(ret.name.as_str(), "None" | "nothing")
            && ret.args.is_empty()
            && !ret.optional
            && !prelude
        {
            self.diagnostics.push(arrow_to_nothing(&def.name, arrow, ret.span));
        }
        // Anything still open in what it gives back has been reported, and callers
        // mustn't settle it.
        signature.returns = forget_vars(&wrap(value, body.error, effects));
        self.states.insert(key.to_owned(), State::Done(signature));
    }

    /// Whether a program can write a type as it is, with every type it names in scope.
    fn can_write(&self, ty: &Type) -> bool {
        let mut writable = true;
        ty.map(&mut |part| {
            let name = match part {
                Type::Named(name) | Type::Generic(name, _) => Some(name.as_str()),
                Type::Id(_) => Some("Id"),
                Type::IdMap(_) => Some("IdMap"),
                _ => None,
            };
            writable &= name.is_none_or(|name| self.program.can_name(name));
            None
        });
        writable
    }

    /// Notes that the function being checked used a signature that may still grow.
    fn mark_stale(&mut self) {
        if let Some(caller) = self.checking.last() {
            let outermost = self.outermost(caller);
            self.revisit.insert(outermost);
        }
    }

    /// The function a nested function is in, all the way out: nested functions are checked
    /// again with the function around them, which gives them the names they read.
    fn outermost(&self, key: &str) -> String {
        let mut key = key.to_owned();
        while let Some(entry) = self.program.functions.get(&key)
            && entry.owner.is_none()
            && let Some((outer, _)) = key.rsplit_once('.')
        {
            key = outer.to_owned();
        }
        key
    }

    /// Checks again the functions that used signatures that were still growing, until a
    /// round changes none of them. A call cycle's errors are then every error any function
    /// in it can fail with.
    fn revisit_cycles(&mut self) {
        let mut keys = std::mem::take(&mut self.revisit);
        let mut pending: Vec<String> = keys.iter().cloned().collect();
        while let Some(key) = pending.pop() {
            for caller in self.callers.get(&key).into_iter().flatten() {
                if keys.insert(caller.clone()) {
                    pending.push(caller.clone());
                }
            }
        }
        for _ in 0..REVISITS {
            if keys.is_empty() {
                return;
            }
            let mut before = Vec::new();
            for key in &keys {
                if let Some(State::Done(signature)) = self.states.remove(key) {
                    before.push(signature.clone());
                    self.previous.insert(key.clone(), known_errors(signature));
                }
                let prefix = format!("{key}/");
                self.locals.retain(|name, _| !name.starts_with(&prefix));
            }
            for key in &keys {
                if !self.states.contains_key(key) {
                    self.check_function(key);
                }
            }
            let after: Vec<Signature> = keys
                .iter()
                .filter_map(|key| match self.states.get(key) {
                    Some(State::Done(signature)) => Some(signature.clone()),
                    _ => None,
                })
                .collect();
            // Functions marked while being checked again belong to the same cycles, unless
            // they're new.
            keys.extend(std::mem::take(&mut self.revisit));
            if before == after {
                return;
            }
        }
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
            (Some(ty), _) => match self.program.resolve_in(ty, &self.program.type_params(owner)) {
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
                Diagnostic::new("can't work out the type of this", Span { start, end })
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
    fn structs(&mut self, kind: TypeKind) -> BTreeMap<String, crate::Struct> {
        let structs: Vec<(String, Vec<String>, bool)> = self
            .program
            .types
            .values()
            .filter(|info| info.kind == kind)
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

    /// Every enum's cases, with the types of their fields.
    fn enums(&mut self) -> BTreeMap<String, crate::Enum> {
        let enums: Vec<fennec_syntax::TypeDef> = self
            .program
            .types
            .values()
            .filter(|info| info.kind == TypeKind::Enum)
            .map(|info| info.def.clone())
            .collect();
        let mut found = BTreeMap::new();
        for def in enums {
            let mut cases = Vec::new();
            let mut rules = false;
            for case in &def.cases {
                rules |= case.fields.iter().any(|field| field.rule.is_some());
                let fields = case
                    .fields
                    .iter()
                    .map(|field| {
                        (field.name.clone(), self.field_type(&def.name, Some(&case.name), &field.name))
                    })
                    .collect();
                cases.push((case.name.clone(), fields));
            }
            found.insert(def.name, crate::Enum { cases, rules });
        }
        found
    }

    pub(crate) fn is_error_type(&self, ty: &Type) -> bool {
        self.program.kind_of(ty) == Some(TypeKind::Error)
    }
}

/// A signature with the errors it couldn't tell yet left out, for calls to it while it's
/// checked again: what's known so far, which grows to the whole set.
fn known_errors(signature: Signature) -> Signature {
    let returns = match signature.returns {
        Type::Result(value, error) if !error.is_known() => {
            let error = match *error {
                Type::Errors(names) => Type::Errors(names),
                Type::Named(name) => Type::Named(name),
                _ => Type::Errors(Vec::new()),
            };
            Type::Result(value, Box::new(error))
        }
        other => other,
    };
    Signature { returns, ..signature }
}

/// A type with every variable left in it replaced by `Unknown`, for keeping in a signature
/// or a field's type, where no later check may settle it.
pub(crate) fn forget_vars(ty: &Type) -> Type {
    ty.map(&mut |part| matches!(part, Type::Var(_)).then_some(Type::Unknown))
}

fn field_needs_type(name: &str, ty: &Type, span: Span) -> Diagnostic {
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
    match value {
        // A function that can fail passes a result it gives back through unchanged.
        Type::Result(..) => value,
        value if effects.fails => Type::result(value, error),
        value => value,
    }
}

/// `-> None` on a `def`: no arrow already means it gives back nothing, so the fix removes it.
fn arrow_to_nothing(name: &str, arrow: Span, ret: Span) -> Diagnostic {
    Diagnostic::new(format!("`{name}` gives back nothing, so it has no arrow"), arrow.to(ret))
        .with_help("a `def` with no `->` gives back nothing: remove the arrow and what follows it")
        .with_fix(arrow.to(ret), "")
}

/// A `def` with no arrow that gives back a value anyway, with the fix that writes in its type.
fn missing_return_type(def: &FnDef, value: &Type, generator: bool, fixable: bool) -> Diagnostic {
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
    let (what, written) = match value {
        Type::Iterator(item) if generator => {
            (format!("yields {}", a(item)), item.source().map(|item| format!("Iterator[{item}]")))
        }
        value => (format!("gives back {}", a(value)), value.source()),
    };
    let message = format!("`{}` {what}, so its return type has to be written", def.name);
    let Some(written) = written.filter(|_| fixable) else {
        return Diagnostic::new(message, def.span).with_help(format!(
            "write the type it gives back after the parameters: `{signature} -> <type>:`"
        ));
    };
    let end = Span { start: def.span.end, end: def.span.end };
    Diagnostic::new(message, def.span)
        .with_help(format!("write it after the parameters: `{signature} -> {written}:`"))
        .with_fix(end, format!(" -> {written}"))
}
