//! Which types can ever cross to another task: what the backend asks, to keep a value's count
//! plainly when no other thread can ever reach it, and ask whose it is only when one can.
//!
//! A value crosses when it's sent on a channel, captured by or given back from a task `launch`
//! starts, read or changed by `parallel`'s workers, or held in a `shared` value, whose lock hands
//! it from task to task. Whatever is inside a value that crosses crosses with it: its fields and
//! its cases' fields, the items of a list, dict, set or tuple, what an optional or a result holds,
//! and what a function captures.
//!
//! It's worked out over every copy the program makes of every function it checks, the standard
//! library's included, as each copy's types are: a `Pool[T]`'s own channels make its `T` cross,
//! and a generic function that sends a `T` makes each type it's called with cross. A function
//! whose making can't be seen, as a parameter, captures what any arrow function or nested `def`
//! of its type does, in any copy: that's where every such value is made.
//!
//! What can't be worked out counts as crossing: signatures made without it say every type does,
//! and so does a check that meets a type parameter it can't place.

use std::collections::{BTreeMap, BTreeSet};

use fennec_syntax::{Expr, ExprKind, Stmt, StmtKind};

use crate::checker::Checker;
use crate::types::Type;

/// Which types can cross to another task, as the backend reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crossing {
    types: BTreeSet<Type>,
    /// Whether it was worked out: if not, every type is said to cross.
    worked_out: bool,
}

impl Default for Crossing {
    /// Nothing worked out, so every type crosses: a count kept plainly is never wrong for that.
    fn default() -> Self {
        Crossing { types: BTreeSet::new(), worked_out: false }
    }
}

impl Crossing {
    /// Whether a value of this type can ever be reached from another task than the one that made
    /// it: then its count is kept so two threads can change it.
    #[must_use]
    pub fn crosses(&self, ty: &Type) -> bool {
        !self.worked_out || self.types.contains(ty)
    }

    /// Whether it was worked out. When it wasn't, every type crosses.
    #[must_use]
    pub fn worked_out(&self) -> bool {
        self.worked_out
    }

    /// Every type that crosses, when it was worked out.
    pub fn types(&self) -> impl Iterator<Item = &Type> {
        self.types.iter()
    }

    /// The same, with each `LiteralStr` the `str` it is to the engines.
    pub(crate) fn without_literal(self) -> Crossing {
        Crossing {
            types: self.types.iter().map(Type::without_literal).collect(),
            worked_out: self.worked_out,
        }
    }
}

/// A function's type parameters as they are in one copy of it.
type Given = Vec<(String, Type)>;

/// The standard library's channels: a value of one of these hands its item to another task.
const CHANNELS: [&str; 4] = ["Sender", "Receiver", "Inbox", "Channel"];

/// What `std.intrinsics` hands to another task: a message sent or received, and a task and what it
/// fails with. Each type each is called with crosses.
const HANDED_ON: [&str; 4] =
    ["intrinsics.channel_send", "intrinsics.channel_receive", "intrinsics.launch", "intrinsics.launch_end"];

/// Works out which types can cross to another task, from what a check found.
pub(crate) fn crossing(checker: &mut Checker) -> Crossing {
    let reached = reached(checker);
    if !starts_tasks(checker, &reached) {
        return Crossing { types: BTreeSet::new(), worked_out: true };
    }
    let mut walk = Walk::new(checker, reached);
    let seeds = walk.seeds();
    for seed in seeds {
        walk.cross(&seed);
    }
    if walk.unplaced {
        return Crossing::default();
    }
    Crossing { types: walk.types, worked_out: true }
}

/// The functions the program can reach, by the outermost key the checker notes calls under: its
/// own, its tests, and what they call, however many calls down. What the standard library has
/// checked that nothing reaches, as a server's methods checked to lower a program that has none,
/// hands nothing to another task in this program.
fn reached(checker: &Checker) -> BTreeSet<String> {
    let mut called: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (callee, callers) in &checker.callers {
        for caller in callers {
            called.entry(caller.as_str()).or_default().push(callee.as_str());
        }
    }
    let mut queue: Vec<String> = checker
        .typed
        .keys()
        .filter(|key| checker.program.functions.get(*key).is_none_or(|entry| !entry.prelude))
        .map(|key| checker.outermost(key))
        .collect();
    let mut reached = BTreeSet::new();
    while let Some(key) = queue.pop() {
        if reached.contains(&key) {
            continue;
        }
        if let Some(callees) = called.get(key.as_str()) {
            queue.extend(callees.iter().map(|callee| (*callee).to_owned()));
        }
        reached.insert(key);
    }
    reached
}

/// Whether anything the program reaches can hand a value to another task: a channel, a task, a
/// `parallel` loop or a `shared` value. Without one, nothing crosses.
fn starts_tasks(checker: &Checker, reached: &BTreeSet<String>) -> bool {
    let in_reach = |key: &str| reached.contains(&checker.outermost(key));
    let channel = |ty: &Type| {
        let mut found = false;
        visit(ty, &mut |part| found |= hands_on(checker, part).is_some());
        found
    };
    !checker.launches.is_empty()
        || !checker.shared_values.is_empty()
        || checker
            .generic_calls
            .values()
            .any(|call| in_reach(&call.caller) && HANDED_ON.contains(&call.callee.as_str()))
        || checker.typed.iter().any(|(key, body)| in_reach(key) && body.exprs().any(|(_, ty)| channel(ty)))
        || checker.program.functions.values().any(|entry| !entry.prelude && has_parallel(&entry.def.body))
        || checker.program.tests.iter().any(|test| has_parallel(&test.body))
}

/// What a value of this type hands to another task, by its type alone: a channel's item, or a
/// `shared` value, which a lock hands from task to task, itself.
fn hands_on(checker: &Checker, ty: &Type) -> Option<Vec<Type>> {
    match ty {
        Type::Generic(name, items) if CHANNELS.contains(&name.as_str()) => Some(items.clone()),
        Type::Named(name) | Type::Generic(name, _)
            if checker.program.type_info(name).is_some_and(|info| info.def.shared) =>
        {
            Some(vec![ty.clone()])
        }
        _ => None,
    }
}

/// Whether a type has a type parameter in it.
fn holds_param(ty: &Type) -> bool {
    let mut found = false;
    visit(ty, &mut |part| found |= matches!(part, Type::Param(_)));
    found
}

/// What a `?` passes up from a value of this type: the error a result can fail with.
fn failure_of(ty: &Type) -> Vec<Type> {
    match ty {
        Type::Result(_, error) => vec![(**error).clone()],
        _ => Vec::new(),
    }
}

/// Whether a body has a `parallel` loop or comprehension.
fn has_parallel(body: &[Stmt]) -> bool {
    let mut found = false;
    each_stmt(body, &mut |stmt| {
        if let StmtKind::For { iter, .. } = &stmt.kind {
            found |= crate::parallel::parallel_items(iter).is_some();
        }
    });
    fennec_syntax::read::in_stmts(body, &mut |expr| {
        if let ExprKind::Comprehension { iter, .. } = &expr.kind {
            found |= crate::parallel::parallel_items(iter).is_some();
        }
    });
    found
}

/// Every statement in a body, inside its blocks too, but not inside a nested `def`, which is a
/// function of its own.
fn each_stmt<'b>(body: &'b [Stmt], look: &mut dyn FnMut(&'b Stmt)) {
    for stmt in body {
        look(stmt);
        for inner in blocks(&stmt.kind) {
            each_stmt(inner, look);
        }
    }
}

/// The blocks a statement holds.
fn blocks(kind: &StmtKind) -> Vec<&[Stmt]> {
    match kind {
        StmtKind::If { branches, otherwise } => {
            branches.iter().map(|(_, body)| body.as_slice()).chain(otherwise.as_deref()).collect()
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => vec![body],
        StmtKind::OnFailure { cleanup, body, .. } => vec![cleanup, body],
        StmtKind::Lend { otherwise, .. } => vec![otherwise],
        _ => Vec::new(),
    }
}

/// Every part of a type, the type itself first.
fn visit(ty: &Type, look: &mut dyn FnMut(&Type)) {
    look(ty);
    for child in ty.children() {
        visit(child, look);
    }
}

/// A place in the source, by where it starts and ends.
type Place = (u32, u32);

/// Where, in one function, something is handed to another task, found once from its syntax and
/// read in each of its copies, as that copy's types have it.
#[derive(Default)]
struct Spots {
    /// Each value whose type crosses whole: what a `parallel` loop or comprehension goes over,
    /// what its body reads from around it and what a comprehension makes, a `with launch`'s
    /// arrow, and a `shared` value.
    whole: Vec<(u32, u32)>,
    /// Each `?` in a worker's body, whose failure the runtime hands back to the caller, by what
    /// it's written after; and each value a worker's body fails with.
    failing: Vec<(u32, u32)>,
    failed: Vec<(u32, u32)>,
    /// Each arrow function, with where it reads names that aren't its own.
    arrows: Vec<(Place, Vec<Place>)>,
    /// For a nested `def`, where it reads names that aren't its own.
    nested: Option<Vec<(u32, u32)>>,
}

fn at(expr: &Expr) -> (u32, u32) {
    (expr.span.start, expr.span.end)
}

/// Where `body` reads names other than `own`.
fn reads_in(part: &Expr, own: &[String], found: &mut Vec<(u32, u32)>) {
    fennec_syntax::read::in_expr(part, &mut |expr| {
        if let ExprKind::Name(name) = &expr.kind
            && !own.contains(name)
        {
            found.push(at(expr));
        }
    });
}

/// What a worker's body can fail with: each `?` in it, and each `fail`.
fn failures_in(body: &[Stmt], spots: &mut Spots) {
    fennec_syntax::read::in_stmts(body, &mut |expr| {
        if let ExprKind::Try { value, .. } = &expr.kind {
            spots.failing.push(at(value));
        }
    });
    each_stmt(body, &mut |stmt| {
        if let StmtKind::Fail(value) = &stmt.kind {
            spots.failed.push(at(value));
        }
    });
}

/// The places in a body where something is handed to another task, and its arrow functions.
fn spots_in(body: &[Stmt], spots: &mut Spots) {
    each_stmt(body, &mut |stmt| {
        let StmtKind::For { names, iter, body, .. } = &stmt.kind else { return };
        let Some(items) = crate::parallel::parallel_items(iter) else { return };
        spots.whole.push(at(items));
        fennec_syntax::read::in_stmts(body, &mut |expr| {
            if let ExprKind::Name(name) = &expr.kind
                && !names.contains(name)
            {
                spots.whole.push(at(expr));
            }
        });
        failures_in(body, spots);
    });
    fennec_syntax::read::in_stmts(body, &mut |expr| match &expr.kind {
        ExprKind::Comprehension { names, iter, elem, value, cond, .. } => {
            let Some(items) = crate::parallel::parallel_items(iter) else { return };
            spots.whole.push(at(items));
            spots.whole.push(at(expr));
            for part in [Some(elem), value.as_ref(), cond.as_ref()].into_iter().flatten() {
                reads_in(part, names, &mut spots.whole);
                fennec_syntax::read::in_expr(part, &mut |inner| {
                    if let ExprKind::Try { value, .. } = &inner.kind {
                        spots.failing.push(at(value));
                    }
                });
            }
        }
        ExprKind::Lambda { params, body } => {
            let mut reads = Vec::new();
            reads_in(body, params, &mut reads);
            spots.arrows.push((at(expr), reads));
        }
        _ => {}
    });
}

struct Walk<'c> {
    checker: &'c mut Checker,
    /// The functions the program can reach, by outermost key.
    reached: BTreeSet<String>,
    /// Where each function hands something to another task, by key, once worked out.
    spots: BTreeMap<String, Spots>,
    /// The calls to generic functions each function makes, by the caller's key.
    calls: BTreeMap<String, Vec<crate::generic_copies::Call>>,
    /// Each type's methods, by its name.
    methods: BTreeMap<String, Vec<String>>,
    /// Each copy of each function looked at, by key and what its type parameters are.
    copies: BTreeSet<(String, Given)>,
    /// What each arrow function and nested `def` the program makes captures, by its type.
    arrows: BTreeMap<Type, BTreeSet<Type>>,
    /// The types that cross, so far.
    types: BTreeSet<Type>,
    /// Set when a type that crosses has a type parameter no copy placed.
    unplaced: bool,
}

impl<'c> Walk<'c> {
    fn new(checker: &'c mut Checker, reached: BTreeSet<String>) -> Self {
        let mut calls: BTreeMap<String, Vec<crate::generic_copies::Call>> = BTreeMap::new();
        for call in checker.generic_calls.values() {
            calls.entry(call.caller.clone()).or_default().push(call.clone());
        }
        let mut methods: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (key, entry) in &checker.program.functions {
            if let Some(owner) = &entry.owner
                && checker.typed.contains_key(key)
            {
                methods.entry(owner.clone()).or_default().push(key.clone());
            }
        }
        Walk {
            checker,
            reached,
            spots: BTreeMap::new(),
            calls,
            methods,
            copies: BTreeSet::new(),
            arrows: BTreeMap::new(),
            types: BTreeSet::new(),
            unplaced: false,
        }
    }

    /// Every copy of every function the program reaches, and what each hands to another task.
    fn seeds(&mut self) -> Vec<Type> {
        let mut queue: Vec<(String, Given)> = self
            .checker
            .typed
            .keys()
            .filter(|key| !self.is_generic(key) && self.reached.contains(&self.checker.outermost(key)))
            .map(|key| (key.clone(), Vec::new()))
            .collect();
        let mut seeds = self.launched();
        while let Some((key, given)) = queue.pop() {
            if !self.copies.insert((key.clone(), given.clone())) {
                continue;
            }
            let types: BTreeMap<(u32, u32), Type> = match self.checker.typed.get(&key) {
                Some(body) => body
                    .exprs()
                    .map(|(span, ty)| ((span.start, span.end), self.checker.settled(ty).substitute(&given)))
                    .collect(),
                None => BTreeMap::new(),
            };
            seeds.extend(self.handed_on(&key, &given, &types));
            queue.extend(self.called(&key, &given, &types));
        }
        seeds
    }

    /// What each `with launch` captures, gives back and fails with, and each `shared` value
    /// holds, as the program as written has them. Once lowering writes a launch in, its arrow is
    /// handed to `std.intrinsics.launch`, whose type it gives crosses.
    fn launched(&self) -> Vec<Type> {
        let mut seeds = Vec::new();
        for launch in self.checker.launches.values() {
            let parts = [Some(&launch.value), launch.error.as_ref(), launch.message.as_ref()];
            seeds.extend(parts.into_iter().flatten().map(|ty| self.checker.settled(ty)));
        }
        seeds.extend(self.checker.shared_values.values().map(|ty| self.checker.settled(ty)));
        seeds
    }

    /// Whether a function has type parameters, its own or from what it's in.
    fn is_generic(&self, key: &str) -> bool {
        self.checker
            .program
            .functions
            .get(key)
            .is_some_and(|entry| !self.checker.program.generics_of(entry).is_empty())
    }

    /// Where a function hands something to another task, worked out once from its syntax.
    fn spots(&mut self, key: &str) -> &Spots {
        if !self.spots.contains_key(key) {
            let mut spots = Spots::default();
            if let Some(entry) = self.checker.program.functions.get(key) {
                spots_in(&entry.def.body, &mut spots);
                // A nested `def` captures what it reads that isn't its own.
                if key.contains('.') && entry.owner.is_none() {
                    let own: Vec<String> = self
                        .checker
                        .typed
                        .get(key)
                        .map(|body| body.names().map(|(_, name, _)| name.to_owned()).collect())
                        .unwrap_or_default();
                    let mut reads = Vec::new();
                    fennec_syntax::read::in_stmts(&entry.def.body, &mut |expr| {
                        if let ExprKind::Name(name) = &expr.kind
                            && !own.contains(name)
                        {
                            reads.push(at(expr));
                        }
                    });
                    spots.nested = Some(reads);
                }
            } else if let Some(test) =
                self.checker.program.tests.iter().find(|test| format!("test \"{}\"", test.name) == key)
            {
                spots_in(&test.body, &mut spots);
            }
            for launch in self.checker.launches.values() {
                if self.checker.typed.get(key).is_some_and(|body| body.expr(launch.task.span).is_some()) {
                    spots.whole.push(at(&launch.task));
                }
            }
            self.spots.insert(key.to_owned(), spots);
        }
        self.spots.entry(key.to_owned()).or_default()
    }

    /// What a copy of a function hands to another task: the items of its channels and each value
    /// of a `shared` type in it, what it gives `std.intrinsics` to send or run, what each
    /// `parallel` loop or comprehension in it reads, makes and fails with, and each `with launch`'s
    /// arrow. It notes what each arrow function and nested `def` in it captures, as this copy has it.
    fn handed_on(&mut self, key: &str, given: &Given, types: &BTreeMap<(u32, u32), Type>) -> Vec<Type> {
        let mut seeds = Vec::new();
        for ty in types.values() {
            visit(ty, &mut |part| seeds.extend(hands_on(self.checker, part).into_iter().flatten()));
        }
        for call in self.calls.get(key).into_iter().flatten() {
            if HANDED_ON.contains(&call.callee.as_str()) {
                seeds.extend(call.types.iter().map(|ty| self.checker.settled(ty).substitute(given)));
            }
        }
        let type_at = |spot: &Place| types.get(spot).cloned();
        let spots = self.spots(key);
        seeds.extend(spots.whole.iter().filter_map(type_at));
        seeds.extend(spots.failed.iter().filter_map(type_at));
        seeds.extend(spots.failing.iter().filter_map(type_at).flat_map(|ty| failure_of(&ty)));
        let mut made: Vec<(Type, BTreeSet<Type>)> = spots
            .arrows
            .iter()
            .filter_map(|(arrow, reads)| Some((type_at(arrow)?, reads.iter().filter_map(type_at).collect())))
            .collect();
        if let Some(reads) = &spots.nested {
            let captured: BTreeSet<Type> = reads.iter().filter_map(type_at).collect();
            made.push((Type::Function(key.to_owned()), captured.clone()));
            // Handed where a function of its type goes, it's a value of that type.
            if let Some(signature) = self.checker.signature_found(key).cloned() {
                let params = signature.params.iter().map(|param| param.ty.substitute(given)).collect();
                made.push((Type::Arrow(params, Box::new(signature.returns.substitute(given))), captured));
            }
        }
        for (ty, captured) in made {
            self.arrows.entry(ty).or_default().extend(captured);
        }
        seeds
    }

    /// The copies of the generic functions a copy calls, and of the methods of each generic type
    /// it has a value of: a method's copy is the type's, as its value has it.
    fn called(&self, key: &str, given: &Given, types: &BTreeMap<(u32, u32), Type>) -> Vec<(String, Given)> {
        let mut copies = Vec::new();
        for call in self.calls.get(key).into_iter().flatten() {
            let Some(entry) = self.checker.program.functions.get(&call.callee) else { continue };
            let names = self.checker.program.generics_of(entry).into_iter().map(|generic| generic.name);
            let types = call.types.iter().map(|ty| self.checker.settled(ty).substitute(given));
            copies.push((call.callee.clone(), names.zip(types).collect()));
        }
        let mut made: BTreeSet<(String, Vec<Type>)> = BTreeSet::new();
        for ty in types.values() {
            visit(ty, &mut |part| {
                if let Type::Generic(name, args) = part
                    && self.methods.contains_key(name)
                {
                    made.insert((name.clone(), args.clone()));
                }
            });
        }
        for (name, args) in made {
            let Some(info) = self.checker.program.type_info(&name) else { continue };
            let given: Given =
                info.def.generics.iter().map(|generic| generic.name.clone()).zip(args).collect();
            for method in self.methods.get(&name).into_iter().flatten() {
                copies.push((method.clone(), given.clone()));
            }
        }
        copies
    }

    /// Notes that a type crosses, and with it everything a value of it holds.
    fn cross(&mut self, ty: &Type) {
        let mut queue = vec![ty.clone()];
        while let Some(ty) = queue.pop() {
            if matches!(ty, Type::Unknown | Type::Var(_) | Type::Unit | Type::None)
                || !self.types.insert(ty.clone())
            {
                continue;
            }
            match &ty {
                // A type parameter no copy placed may be anything.
                Type::Param(_) => self.unplaced = true,
                // A function crossing takes what it captures with it, and runs where it's sent,
                // given what it's called with there, and giving back what it gives, or fails with.
                Type::Arrow(..) => {
                    queue.extend(self.arrows.get(&ty).into_iter().flatten().cloned());
                    queue.extend(ty.children().into_iter().cloned());
                }
                Type::Function(key) => {
                    queue.extend(self.arrows.get(&ty).into_iter().flatten().cloned());
                    // A generic function's own types are placed in each copy of it, walked already.
                    if let Some(signature) = self.checker.signature_found(key).cloned() {
                        let types =
                            signature.params.into_iter().map(|param| param.ty).chain([signature.returns]);
                        queue.extend(types.filter(|ty| !holds_param(ty)));
                    }
                }
                Type::Errors(names) => queue.extend(names.iter().map(|name| Type::named(name))),
                _ => queue.extend(ty.children().into_iter().cloned()),
            }
            if let Type::Named(name) | Type::Generic(name, _) = &ty {
                queue.extend(self.fields_of(&ty, name));
            }
        }
    }

    /// The types of a struct's fields and its cases' fields, as `ty` gives its parameters.
    fn fields_of(&mut self, ty: &Type, name: &str) -> Vec<Type> {
        let Some(info) = self.checker.program.type_info(name) else { return Vec::new() };
        let def = info.def.clone();
        let args = match ty {
            Type::Generic(_, args) => args.clone(),
            _ => Vec::new(),
        };
        let given: Given = def.generics.iter().map(|generic| generic.name.clone()).zip(args).collect();
        let mut fields: Vec<(Option<String>, String)> =
            def.fields.iter().map(|field| (None, field.name.clone())).collect();
        for case in &def.cases {
            fields.extend(case.fields.iter().map(|field| (Some(case.name.clone()), field.name.clone())));
        }
        let mut types = Vec::new();
        for (case, field) in fields {
            match self.checker.field_type_known(name, case.as_deref(), &field) {
                Some(ty) => types.push(ty.substitute(&given)),
                // A field typed by a default nothing has worked out may hold anything.
                None => self.unplaced = true,
            }
        }
        types
    }
}
