//! What each kit a program depends on holds. The compiler works out every capability the program's
//! code hands to each kit, and `kit.lock` records what each was accepted as holding, in its `holds`
//! lines. A check stops where a kit would now receive what its line doesn't list, until a person
//! accepts it with `fennec update`, which shows the change and writes it: nothing here offers a fix.
//!
//! A kit receives, worked out from the values' types once every function is checked:
//!
//! - What's handed to another kit's function or method, the value it's called on included, and
//!   what's handed to the building of another kit's struct, error or enum case. Operators, `for`,
//!   `str` and `with` on another kit's value hand over only that value, handed over when it was
//!   built.
//! - In a generic function, each value as its type is in each copy the program makes: a value of
//!   type `S` handed on is an `Fs` in the copy called with an `Fs`, though no code names `Fs`.
//! - A function handed over holds what it captures: nothing for a named function, what an arrow
//!   function or a nested `def` reads from around it, and, for one whose making can't be seen,
//!   everything the program's code holds.
//! - What the parameters take of another kit's function used as a value, which whoever holds it
//!   can call with that, and of an arrow function a kit makes, which is called with that: the
//!   standard library's code calls functions it's handed with values it was handed.
//!
//! The kits a dependency depends on are counted too, as the compiler sees the whole program. Code
//! handed back to the program's own kit, and the standard library, aren't counted: the program's
//! own code is the person's, and std's is Fennec's.

use std::collections::{BTreeMap, BTreeSet};

use fennec_syntax::{Diagnostic, ExprKind, KIT_MARK, Span};

use crate::checker::Checker;
use crate::function::FnCx;
use crate::types::Type;

/// Where the values a call or a build hands over go.
#[derive(Clone, Debug)]
pub(crate) enum To {
    /// A function or method, by its key.
    Function(String),
    /// A struct, error or enum being built, by its name.
    Type(String),
    /// A method of a type parameter, by its name: whose it is depends on the copy of the function
    /// the call is in.
    Interface(String),
}

/// One value handed over.
#[derive(Clone, Debug)]
pub(crate) struct Value {
    pub(crate) span: Span,
    /// The arrow function it is, or that the local name it's read from holds, by its span.
    pub(crate) arrow: Option<(u32, u32)>,
}

/// The values one call or build hands over.
#[derive(Clone, Debug)]
pub(crate) struct Handed {
    pub(crate) to: To,
    pub(crate) values: Vec<Value>,
}

/// An arrow function, by what it reads from around it and what its parameters take.
#[derive(Clone, Debug, Default)]
pub(crate) struct Arrow {
    /// Where it reads names that aren't its own parameters.
    pub(crate) reads: Vec<Span>,
    pub(crate) params: Vec<Type>,
}

/// What the program's code hands from kit to kit, noted as it's checked.
#[derive(Clone, Debug, Default)]
pub(crate) struct Handovers {
    /// Each call or build, by the function it's in and its span.
    pub(crate) handed: BTreeMap<String, BTreeMap<(u32, u32), Handed>>,
    /// Each arrow function, by the function it's in and its span.
    pub(crate) arrows: BTreeMap<String, BTreeMap<(u32, u32), Arrow>>,
}

impl FnCx<'_> {
    /// Notes the values a call or build at `span` hands to `to`: `values` are the arguments, and
    /// the value a method is called on before them. A function checked again notes the same
    /// values, so the last check's are kept.
    pub(crate) fn note_handed(&mut self, to: To, span: Span, values: &[&fennec_syntax::Expr]) {
        if !self.report {
            return;
        }
        let values: Vec<Value> =
            values.iter().map(|value| Value { span: value.span, arrow: self.arrow_in(value) }).collect();
        let calls = self.checker.handovers.handed.entry(self.key.clone()).or_default();
        let handed = calls.entry((span.start, span.end)).or_insert_with(|| Handed { to, values: Vec::new() });
        for value in values {
            match handed.values.iter_mut().find(|had| had.span == value.span) {
                Some(had) => *had = value,
                None => handed.values.push(value),
            }
        }
    }

    /// The arrow function a value is, or that the local name it reads holds, when that's known.
    fn arrow_in(&self, value: &fennec_syntax::Expr) -> Option<(u32, u32)> {
        match &value.kind {
            ExprKind::Lambda { .. } => Some((value.span.start, value.span.end)),
            ExprKind::Name(name) => self.arrow_locals.get(name).copied().flatten(),
            _ => None,
        }
    }

    /// Notes an arrow function: the names it reads from around it, and what it takes.
    pub(crate) fn note_arrow_made(
        &mut self,
        span: Span,
        params: &[String],
        body: &fennec_syntax::Expr,
        takes: &[Type],
    ) {
        if !self.report {
            return;
        }
        let mut reads = Vec::new();
        fennec_syntax::read::in_expr(body, &mut |expr| {
            if let ExprKind::Name(name) = &expr.kind
                && !params.contains(name)
            {
                reads.push(expr.span);
            }
        });
        let arrows = self.checker.handovers.arrows.entry(self.key.clone()).or_default();
        arrows.insert((span.start, span.end), Arrow { reads, params: takes.to_vec() });
    }
}

/// Whose code a function is: a kit the program depends on, the program's own, or the standard
/// library's.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kit {
    Std,
    Own,
    Named(String),
}

/// A type's parameters as they are in one copy of a generic function.
type Given = Vec<(String, Type)>;

/// Places in the source, by where they start and end.
type Spans = BTreeSet<(u32, u32)>;

/// What a value holds, and how it's held, to say so.
#[derive(Clone, Debug, Default)]
struct Held {
    capabilities: BTreeSet<&'static str>,
    /// The type it's held in, when it isn't a capability itself.
    inside: Option<Type>,
    /// Whether it's a function, which holds what it captures.
    function: bool,
    /// Whether it's a function whose making can't be seen, which may hold anything the program's
    /// code holds.
    unseen: bool,
}

/// The capability a value of a standard library type is, by the name `kit.lock` gives it: a `File`
/// and a `Database` reach a file or storage already open, as an `Fs` reaches every file.
fn held_as(name: &str) -> Option<&'static str> {
    let found = match name {
        "Database" | "Transaction" | "PgDatabase" | "PgPool" | "PgTransaction" => "Database",
        other => other,
    };
    fennec_syntax::HOLDABLE.iter().copied().find(|holdable| *holdable == found)
}

/// Works out what the program's code hands each kit, and refuses each capability a kit would now
/// receive that `kit.lock` doesn't list for it, where it's first handed over. A module checked only
/// to work this out keeps what it found, for `fennec update` to write, and refuses nothing.
pub(crate) fn refused(checker: &mut Checker) -> Vec<Diagnostic> {
    // A program with no kit besides its own hands nothing over.
    if !checker.program.functions.keys().any(|key| key.contains(KIT_MARK)) {
        return Vec::new();
    }
    let mut walk = Walk::new(checker);
    walk.run();
    let received = std::mem::take(&mut walk.received);
    if walk.checker.holds.only_worked_out {
        walk.checker.worked_out = received
            .into_iter()
            .map(|(kit, held)| {
                let held = held
                    .into_iter()
                    .map(|(capability, receipt)| (capability.to_owned(), receipt.span))
                    .collect();
                (kit, held)
            })
            .collect();
        return Vec::new();
    }
    newly_received(&walk.checker.holds.accepted, received)
}

/// One capability a kit receives where it's first handed over, and how.
#[derive(Clone, Debug)]
struct Receipt {
    span: Span,
    how: How,
}

/// How a kit receives a capability.
#[derive(Clone, Debug)]
enum How {
    /// In a value another kit's code hands it, `None` for the program's own.
    Handed { by: Option<String>, held: Held },
    /// Through its function, used as a value.
    AsValue { function: String },
    /// Through an arrow function it makes, which is called with what it takes.
    Made,
}

/// One place a kit is first handed what `kit.lock` doesn't list, and everything new it's handed there.
type Place = (Receipt, Vec<&'static str>);

/// An error for each place a kit is first handed what `kit.lock` doesn't list for it, with every
/// such capability first handed there.
fn newly_received(
    accepted: &[(String, Vec<String>)],
    received: BTreeMap<String, BTreeMap<&'static str, Receipt>>,
) -> Vec<Diagnostic> {
    // Each place a kit is first handed something new, with everything new first handed there.
    let mut places: BTreeMap<((u32, u32), String), Place> = BTreeMap::new();
    for (kit, held) in received {
        let listed: Vec<&str> = accepted
            .iter()
            .filter(|(had, _)| *had == kit)
            .flat_map(|(_, held)| held.iter().map(String::as_str))
            .collect();
        for (capability, receipt) in held {
            if listed.contains(&capability) {
                continue;
            }
            let place = places
                .entry(((receipt.span.start, receipt.span.end), kit.clone()))
                .or_insert_with(|| (receipt.clone(), Vec::new()));
            place.1.push(capability);
        }
    }
    let mut found: Vec<Diagnostic> = places
        .into_iter()
        .map(|((_, kit), (receipt, capabilities))| unaccepted(&kit, &receipt, &capabilities))
        .collect();
    found.sort_by(|first, second| {
        (first.span.start, &first.message).cmp(&(second.span.start, &second.message))
    });
    found
}

/// The error for a kit that would now receive `capabilities`, first handed over as `receipt` says.
/// It never has a fix: what a kit holds is accepted by a person, with `fennec update`.
fn unaccepted(kit: &str, receipt: &Receipt, capabilities: &[&'static str]) -> Diagnostic {
    let listed = listed(capabilities);
    let unlisted = "which kit.lock doesn't list for it";
    let (message, unseen) = match &receipt.how {
        How::Handed { by, held } => {
            let what = if held.unseen {
                format!(
                    "a function here whose making can't be seen, so it may hold anything the program holds: {listed}"
                )
            } else if held.function {
                format!("a function that holds {listed} here")
            } else {
                match &held.inside {
                    Some(inside) => format!("{listed}, in {}, here", with_article(&inside.to_string())),
                    None => format!("{listed} here"),
                }
            };
            let from = by.as_ref().map(|by| format!(", from `{by}`'s code")).unwrap_or_default();
            (format!("`{kit}` would now receive {what}{from}, {unlisted}"), held.unseen)
        }
        How::AsValue { function } => (
            format!(
                "`{kit}.{}` takes {listed} and is used here as a value, so `{kit}` would now receive {listed}, {unlisted}",
                fennec_syntax::without_kits(function)
            ),
            false,
        ),
        How::Made => (
            format!(
                "`{kit}` makes a function here that takes {listed}, so it would now receive {listed}, {unlisted}"
            ),
            false,
        ),
    };
    let accept = "a dependency receiving a capability is a change a person accepts: `fennec update` shows what each kit would now hold and records it in kit.lock, to be reviewed with this change. Nothing here is fixed for you";
    let help = if unseen {
        format!(
            "write the function where it's handed over, as an arrow function or a nested `def`, and only what it reads counts. Otherwise, {accept}"
        )
    } else {
        accept.to_owned()
    };
    Diagnostic::new(message, receipt.span).with_help(help)
}

/// The walk over every copy of every function the program checks.
struct Walk<'c> {
    checker: &'c mut Checker,
    handovers: Handovers,
    /// What each kit receives, by kit and capability, where it's first handed over.
    received: BTreeMap<String, BTreeMap<&'static str, Receipt>>,
    /// What each type holds, once worked out.
    held: BTreeMap<Type, BTreeSet<&'static str>>,
    /// Everything the program's code holds, once worked out: what a function whose making can't
    /// be seen may hold.
    everything: Option<BTreeSet<&'static str>>,
    /// Set while `everything` is being worked out, when a function or a type parameter in a type
    /// adds nothing to it, and what types hold isn't kept, as it's not yet whole.
    gathering: bool,
    /// The copies of functions looked at, by key and what their type parameters are.
    seen: BTreeSet<(String, Given)>,
}

impl<'c> Walk<'c> {
    fn new(checker: &'c mut Checker) -> Self {
        let handovers = std::mem::take(&mut checker.handovers);
        Walk {
            checker,
            handovers,
            received: BTreeMap::new(),
            held: BTreeMap::new(),
            everything: None,
            gathering: false,
            seen: BTreeSet::new(),
        }
    }

    fn run(&mut self) {
        // The program's own functions, and every method, which a value of its type may have called
        // where nothing names it, as `str` by printing it. What a dependency's function does counts
        // once something reaches it, as each copy reaches what it calls and names.
        let mut queue: Vec<(String, Given)> = Vec::new();
        for (key, entry) in &self.checker.program.functions {
            let reachable = self.kit_of(key) == Kit::Own || entry.owner.is_some();
            if !entry.prelude && reachable && !self.is_generic(key) {
                queue.push((key.clone(), Vec::new()));
            }
        }
        for test in &self.checker.program.tests {
            queue.push((format!("test \"{}\"", test.name), Vec::new()));
        }
        while let Some((key, given)) = queue.pop() {
            if !self.seen.insert((key.clone(), given.clone())) {
                continue;
            }
            queue.extend(self.copy(&key, &given));
        }
    }

    /// Notes that `kit` receives each of `capabilities` at `span`, as `how` says, keeping the first
    /// place in the source for each.
    fn receive(&mut self, kit: &str, capabilities: &BTreeSet<&'static str>, span: Span, how: &How) {
        let held = self.received.entry(kit.to_owned()).or_default();
        for capability in capabilities {
            let receipt = Receipt { span, how: how.clone() };
            match held.get(capability) {
                Some(had) if (had.span.start, had.span.end) <= (span.start, span.end) => {}
                _ => {
                    held.insert(capability, receipt);
                }
            }
        }
    }

    /// Whether a function has type parameters of its own, or from the function or type it's in.
    fn is_generic(&self, key: &str) -> bool {
        let Some(entry) = self.checker.program.functions.get(key) else { return false };
        !entry.def.generics.is_empty()
            || !entry.inherited.is_empty()
            || !entry.extension.is_empty()
            || entry
                .owner
                .as_deref()
                .and_then(|owner| self.checker.program.type_info(owner))
                .is_some_and(|info| !info.def.generics.is_empty())
    }

    /// Looks at one copy of a function, giving back the copies of generic functions it calls.
    fn copy(&mut self, key: &str, given: &Given) -> Vec<(String, Given)> {
        let from = self.kit_of(key);
        let mut copies = Vec::new();
        let calls = self.handovers.handed.get(key).cloned().unwrap_or_default();
        for (span, handed) in &calls {
            let span = Span { start: span.0, end: span.1 };
            let Some((to, callee)) = self.receiver(key, given, handed) else { continue };
            if let Kit::Named(to) = &to
                && from != Kit::Named(to.clone())
            {
                for value in &handed.values {
                    self.hand_over((key, given), (&from, to), value);
                }
            }
            if let Some(callee) = callee {
                let given = if self.is_generic(&callee) {
                    self.given_to(key, given, span, &callee, handed)
                } else {
                    Vec::new()
                };
                copies.push((callee, given));
            }
        }
        if let Kit::Named(kit) = &from {
            self.arrows_made(key, given, &kit.clone());
        }
        self.functions_as_values(key, given, &from);
        copies.extend(self.named_functions(key, given));
        copies
    }

    /// The copies of the functions a function names as values, which whatever's handed them can
    /// call: a nested one with the type parameters of the copy it's in.
    fn named_functions(&self, key: &str, given: &Given) -> Vec<(String, Given)> {
        let Some(body) = self.checker.typed.get(key) else { return Vec::new() };
        let named: BTreeSet<&String> = body
            .exprs()
            .filter_map(|(_, ty)| match ty {
                Type::Function(function) => Some(function),
                _ => None,
            })
            .collect();
        named
            .into_iter()
            .filter_map(|function| {
                let entry = self.checker.program.functions.get(function)?;
                // One with type parameters of its own is reached by its calls, which say what they are.
                let own_generics = !entry.def.generics.is_empty() || !entry.extension.is_empty();
                let owner_generic = entry
                    .owner
                    .as_deref()
                    .and_then(|owner| self.checker.program.type_info(owner))
                    .is_some_and(|info| !info.def.generics.is_empty());
                if entry.prelude || own_generics || owner_generic {
                    return None;
                }
                let inherited = given
                    .iter()
                    .filter(|(name, _)| entry.inherited.iter().any(|generic| &generic.name == name));
                Some((function.clone(), inherited.cloned().collect()))
            })
            .collect()
    }

    /// Whose code a call or build hands its values to, and the function called, when it's one.
    fn receiver(&mut self, key: &str, given: &Given, handed: &Handed) -> Option<(Kit, Option<String>)> {
        match &handed.to {
            To::Function(callee) => Some((self.kit_of(callee), Some(callee.clone()))),
            To::Type(name) => Some((self.kit_of_type(name), None)),
            To::Interface(method) => {
                let object = handed.values.first()?;
                let ty = self.type_at(key, given, object.span)?;
                let owner = ty.type_name()?.to_owned();
                let callee = format!("{owner}.{method}");
                let kit = self.kit_of_type(&owner);
                Some((kit, self.checker.program.functions.contains_key(&callee).then_some(callee)))
            }
        }
    }

    /// The types a generic function's parameters are where it's called, in this copy of its caller.
    fn given_to(&mut self, key: &str, given: &Given, span: Span, callee: &str, handed: &Handed) -> Given {
        let mut chosen: Given = Vec::new();
        if let Some(entry) = self.checker.program.functions.get(callee)
            && let Some(call) = self.checker.generic_calls.get(&(span.start, span.end))
        {
            // Its parameters in the order a call chooses them: the type's, the function's it's in,
            // then its own.
            let names = self.checker.program.generics_of(entry).into_iter().map(|generic| generic.name);
            chosen.extend(names.zip(call.types.iter().map(|ty| self.checker.settled(ty).substitute(given))));
        }
        // A method of a generic type has the type's parameters as the value it's called on has them.
        let owner = self.checker.program.functions.get(callee).and_then(|entry| entry.owner.clone());
        if let (Some(owner), Some(object)) = (owner, handed.values.first())
            && let Some(Type::Generic(_, args)) = self.type_at(key, given, object.span)
            && let Some(info) = self.checker.program.type_info(&owner)
        {
            chosen.extend(info.def.generics.iter().map(|generic| generic.name.clone()).zip(args));
        }
        // A nested function has the type parameters of the function it's in.
        if let Some(entry) = self.checker.program.functions.get(callee)
            && !entry.inherited.is_empty()
        {
            chosen.extend(
                given
                    .iter()
                    .filter(|(name, _)| entry.inherited.iter().any(|generic| &generic.name == name))
                    .cloned(),
            );
        }
        chosen
    }

    /// A value's type, as it is in this copy of the function it's in.
    fn type_at(&self, key: &str, given: &Given, span: Span) -> Option<Type> {
        let ty = self.checker.typed.get(key)?.expr(span)?;
        Some(self.checker.settled(ty).substitute(given))
    }

    /// Refuses a value handed from one kit's code to another's that holds what the line for it
    /// Notes what a value handed from one kit's code to another's gives the kit it's handed to.
    fn hand_over(&mut self, (key, given): (&str, &Given), (from, to): (&Kit, &str), value: &Value) {
        let Some(ty) = self.type_at(key, given, value.span) else { return };
        // A value of the kit's own type was handed to it when it was built, from wherever that was.
        if ty.type_name().is_some_and(|name| self.kit_of_type(name) == Kit::Named(to.to_owned())) {
            return;
        }
        let held = self.value_held(key, given, &ty, value.arrow);
        let by = match from {
            Kit::Named(by) => Some(by.clone()),
            _ => None,
        };
        let capabilities = held.capabilities.clone();
        self.receive(to, &capabilities, value.span, &How::Handed { by, held });
    }

    /// Notes what each arrow function a kit makes takes, which the kit receives: whatever it's
    /// handed to calls it with that.
    fn arrows_made(&mut self, key: &str, given: &Given, kit: &str) {
        let arrows = self.handovers.arrows.get(key).cloned().unwrap_or_default();
        for (span, arrow) in arrows {
            let mut takes = BTreeSet::new();
            for param in &arrow.params {
                let param = self.checker.settled(param).substitute(given);
                takes.extend(self.type_held(&param).capabilities);
            }
            self.receive(kit, &takes, Span { start: span.0, end: span.1 }, &How::Made);
        }
    }

    /// Notes what each function of another kit used as a value, not called, takes, which that kit
    /// receives: whoever holds it can call it with that.
    fn functions_as_values(&mut self, key: &str, given: &Given, from: &Kit) {
        let Some(body) = self.checker.typed.get(key) else { return };
        let (called, naming) = self.function_names(key);
        let named: Vec<(Span, String)> = body
            .exprs()
            .filter(|(span, _)| {
                let at = (span.start, span.end);
                !called.contains(&at) && naming.contains(&at)
            })
            .filter_map(|(span, ty)| match ty {
                Type::Function(function) => Some((span, function.clone())),
                _ => None,
            })
            .collect();
        for (span, function) in named {
            let Kit::Named(to) = self.kit_of(&function) else { continue };
            if *from == Kit::Named(to.clone()) {
                continue;
            }
            let Some(signature) = self.checker.signature(&function) else { continue };
            let mut takes = BTreeSet::new();
            for param in &signature.params {
                let param = self.checker.settled(&param.ty).substitute(given);
                takes.extend(self.type_held(&param).capabilities);
            }
            self.receive(&to, &takes, span, &How::AsValue { function });
        }
    }

    /// The spans of what's called in a function, and of each name written there that names a
    /// function, rather than a local holding one.
    fn function_names(&self, key: &str) -> (Spans, Spans) {
        let mut spans = BTreeSet::new();
        let mut naming = BTreeSet::new();
        let functions = &self.checker.program.functions;
        let mut look = |expr: &fennec_syntax::Expr| {
            if let ExprKind::Call { callee, .. } = &expr.kind {
                spans.insert((callee.span.start, callee.span.end));
                if let ExprKind::Index { object, .. } = &callee.kind {
                    spans.insert((object.span.start, object.span.end));
                }
            }
            if let ExprKind::Name(name) = &expr.kind
                && functions.contains_key(name)
            {
                naming.insert((expr.span.start, expr.span.end));
            }
        };
        if let Some(entry) = self.checker.program.functions.get(key) {
            fennec_syntax::read::in_function(&entry.def, &mut look);
        } else if let Some(test) =
            self.checker.program.tests.iter().find(|test| format!("test \"{}\"", test.name) == key)
        {
            fennec_syntax::read::in_stmts(&test.body, &mut look);
        }
        (spans, naming)
    }

    /// What a value of type `ty` holds, the arrow function it is when that's known.
    fn value_held(&mut self, key: &str, given: &Given, ty: &Type, arrow: Option<(u32, u32)>) -> Held {
        match ty {
            Type::Arrow(..) => {
                let mut held =
                    match arrow.and_then(|arrow| self.handovers.arrows.get(key)?.get(&arrow).cloned()) {
                        Some(made) => {
                            let mut held = Held::default();
                            for read in &made.reads {
                                if let Some(read) = self.type_at(key, given, *read) {
                                    held.capabilities
                                        .extend(self.value_held(key, given, &read, None).capabilities);
                                }
                            }
                            held
                        }
                        None => Held { capabilities: self.everything(), unseen: true, ..Held::default() },
                    };
                held.function = true;
                held
            }
            Type::Function(function) => {
                let mut held = Held { capabilities: self.captured_by(function), ..Held::default() };
                held.function = true;
                held
            }
            other => self.type_held(other),
        }
    }

    /// What a named function captures: nothing for one at the top of a file, and for a nested
    /// `def`, what it reads that isn't its own.
    fn captured_by(&mut self, function: &str) -> BTreeSet<&'static str> {
        let Some(body) = self.checker.typed.get(function) else { return BTreeSet::new() };
        let own: BTreeSet<&str> = body.names().map(|(_, name, _)| name).collect();
        let Some(entry) = self.checker.program.functions.get(function) else { return BTreeSet::new() };
        let mut reads = Vec::new();
        fennec_syntax::read::in_function(&entry.def, &mut |expr| {
            if let ExprKind::Name(name) = &expr.kind
                && !own.contains(name.as_str())
                && !entry.def.params.iter().any(|param| &param.name == name)
            {
                reads.push(expr.span);
            }
        });
        let types: Vec<Type> = reads.iter().filter_map(|span| body.expr(*span).cloned()).collect();
        let mut held = BTreeSet::new();
        for ty in types {
            let ty = self.checker.settled(&ty);
            held.extend(self.value_held(function, &Vec::new(), &ty, None).capabilities);
        }
        held
    }

    /// Everything the program's code holds: what a function whose making can't be seen may hold.
    fn everything(&mut self) -> BTreeSet<&'static str> {
        if let Some(everything) = &self.everything {
            return everything.clone();
        }
        if self.gathering {
            return BTreeSet::new();
        }
        self.gathering = true;
        let types: BTreeSet<Type> = self
            .checker
            .typed
            .iter()
            .filter(|(key, _)| self.checker.program.functions.get(*key).is_none_or(|entry| !entry.prelude))
            .flat_map(|(_, body)| body.exprs().map(|(_, ty)| self.checker.settled(ty)))
            .filter(|ty| !matches!(ty, Type::Arrow(..)))
            .collect();
        let mut everything = BTreeSet::new();
        for ty in types {
            everything.extend(self.type_held(&ty).capabilities);
        }
        self.gathering = false;
        self.everything = Some(everything.clone());
        everything
    }

    /// What a value of a type holds, in it or anything inside it: its fields, its cases' fields,
    /// the types it's given, and what a function in it may hold.
    fn type_held(&mut self, ty: &Type) -> Held {
        let capabilities = self.capabilities_in(ty, &mut Vec::new());
        let inside = ty.type_name().is_none_or(|name| held_as(name).is_none()).then(|| ty.clone());
        Held { capabilities, inside, ..Held::default() }
    }

    fn capabilities_in(&mut self, ty: &Type, open: &mut Vec<String>) -> BTreeSet<&'static str> {
        if let Some(held) = self.held.get(ty) {
            return held.clone();
        }
        let mut held = BTreeSet::new();
        match ty {
            Type::Named(name) | Type::Generic(name, _) if held_as(name).is_some() => {
                held.extend(held_as(name));
            }
            // A function in a value may hold anything: what it captures can't be seen from here.
            Type::Arrow(..) => {
                if open.is_empty() {
                    return BTreeSet::new();
                }
                held.extend(self.everything());
            }
            // A type parameter left unknown may be anything the program holds.
            Type::Param(_) => held.extend(self.everything()),
            _ => {}
        }
        for child in ty.children() {
            if !matches!(ty, Type::Arrow(..)) {
                held.extend(self.capabilities_in(&child.clone(), open));
            }
        }
        if let Type::Named(name) | Type::Generic(name, _) = ty
            && !open.contains(name)
            && let Some(info) = self.checker.program.type_info(name)
        {
            let def = info.def.clone();
            // A field typed by the type's own parameter holds what it's given: what `ty` says.
            let args = match ty {
                Type::Generic(_, args) => args.clone(),
                _ => Vec::new(),
            };
            let given: Given = def.generics.iter().map(|generic| generic.name.clone()).zip(args).collect();
            open.push(name.clone());
            let fields: Vec<(Option<String>, String)> = def
                .fields
                .iter()
                .map(|field| (None, field.name.clone()))
                .chain(def.cases.iter().flat_map(|case| {
                    case.fields.iter().map(|field| (Some(case.name.clone()), field.name.clone()))
                }))
                .collect();
            for (case, field) in fields {
                let field = self.checker.field_type(name, case.as_deref(), &field).substitute(&given);
                // Its own parameters are what `ty` gives them, which the children have already.
                if !matches!(field, Type::Param(_)) {
                    held.extend(self.capabilities_in(&field, open));
                }
            }
            open.pop();
        }
        if open.is_empty() && !self.gathering {
            self.held.insert(ty.clone(), held.clone());
        }
        held
    }

    /// Whose code a function is, by its key.
    fn kit_of(&self, key: &str) -> Kit {
        if key.starts_with("test \"") {
            return Kit::Own;
        }
        if self.checker.program.functions.get(key).is_some_and(|entry| entry.prelude) {
            return Kit::Std;
        }
        kit_by_name(key)
    }

    /// Whose code a type is, by its name.
    fn kit_of_type(&self, name: &str) -> Kit {
        if self.checker.program.type_info(name).is_none_or(|info| info.prelude) {
            return Kit::Std;
        }
        kit_by_name(name)
    }
}

/// The kit a name in the merged program belongs to: the part before its kit's mark, or the
/// program's own when it has none.
fn kit_by_name(name: &str) -> Kit {
    match name.split_once(KIT_MARK) {
        Some((kit, _)) if !kit.is_empty() && kit.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
            Kit::Named(kit.to_owned())
        }
        _ => Kit::Own,
    }
}

/// Capabilities as a message lists them: "an `Fs`", "a `Clock` and an `Fs`".
fn listed(capabilities: &[&'static str]) -> String {
    let each: Vec<String> = capabilities.iter().map(|capability| with_article(capability)).collect();
    joined(&each)
}

/// A name in backticks, after "a" or "an" as it's said: "an `Fs`", "a `File`", "an `Env`".
fn with_article(name: &str) -> String {
    let said_with_a_vowel =
        name.starts_with("Fs") || name.starts_with(['A', 'E', 'I', 'O', 'U', 'a', 'e', 'i', 'o', 'u']);
    format!("{} `{name}`", if said_with_a_vowel { "an" } else { "a" })
}

fn joined(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
