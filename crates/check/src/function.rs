//! Checking one function: the names in scope as its body is walked, what they hold at each
//! point, and what the function gives back. Names belong to the block they're created in.
//!
//! What a name holds can change along the way: `if x is None: return` means `x` isn't None
//! afterwards. Where paths through the code meet again, after an `if` or a loop, a name holds
//! whatever it held on any path that gets there.

use fennec_syntax::{Diagnostic, Expr, ExprKind, Field, FnDef, Generic, Span, Stmt, StmtKind, TestDef};

use crate::ParamType;
use crate::checker::Checker;
use crate::effects;
use crate::infer::{Hole, unsettled};
use crate::loops::{blocks, exprs};
use crate::messages::not_mut;
use crate::stmt::{not_none, path};
use crate::types::Type;

#[derive(Clone, Debug)]
pub(crate) struct Binding {
    pub(crate) name: String,
    /// What it holds here, which can be narrower than `declared`: not None after a check.
    pub(crate) ty: Type,
    /// The type it was declared with. Every value given to it has to fit.
    pub(crate) declared: Type,
    pub(crate) is_mut: bool,
    pub(crate) origin: Origin,
    /// Where `mut ` would go to make it changeable: before the name in `name = value`.
    pub(crate) mut_at: Option<u32>,
    /// Whether it's a channel to `.Every` a `with` lends, which has no one next message to take.
    pub(crate) to_every: bool,
    /// The function whose parameter it is, by key, when it's one: kept when a nested function
    /// captures it, so the nested function calling it is a call of that function's parameter.
    pub(crate) param_of: Option<String>,
    /// The arrow function, by span, a name that isn't `mut` was given where it's declared: all it
    /// can hold, so calling it does what the arrow's own body does. It ends with the name's scope,
    /// as a name declared again after it holds something else.
    pub(crate) arrow: Option<(u32, u32)>,
    /// The function parameter, by its function's key and its name, a name that isn't `mut` was
    /// declared holding, as `job = work`: calling the name calls the parameter (see `param_key`).
    pub(crate) alias: Option<(String, String)>,
}

/// Where a name came from, for explaining how to make it changeable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    Local,
    Param,
    /// A `for` loop's name.
    Loop,
    /// A name from around a nested function.
    Captured,
    /// A `with` block's name for a `shared` handle it locks.
    Locked,
}

/// What the checker knows at one point in a function, to come back to after a path through the
/// code and to join where paths meet.
#[derive(Clone)]
pub(crate) struct Snapshot {
    /// What every name in sight holds.
    types: Vec<Type>,
    /// Whether a source has passed its `lend` on the way here.
    pub(crate) lent: Lending,
}

/// Whether a source has passed its `lend` on every way to somewhere in it that hasn't failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lending {
    /// Some way there hasn't.
    NotYet,
    /// Every way there has.
    Lent,
}

struct Scope {
    bindings: Vec<Binding>,
    /// An arrow function's scope: names from outside it can be read but not changed.
    barrier: bool,
    /// A `parallel` loop's or comprehension's body, which runs on other tasks: a generator that
    /// can fail, from outside it, can't pass its failure up from there.
    apart: bool,
    /// The names in this scope that may hold what a `with` lent, with the type it lent and the
    /// scope its block is: they're kept inside that block (see `lent.rs`).
    lent: Vec<(String, Type, usize)>,
}

/// What a function's body gives back.
pub(crate) struct BodyType {
    pub(crate) value: Type,
    pub(crate) error: Type,
}

pub(crate) struct FnCx<'c> {
    pub(crate) checker: &'c mut Checker,
    /// Each name the body gives a value with `=`, and where: one read above that line is read too
    /// early, which says more than that it doesn't exist.
    pub(crate) assigned: Vec<(String, Span)>,
    /// The function's name in the signature table, for naming the functions inside it.
    pub(crate) key: String,
    /// The function's own name, for messages.
    pub(crate) name: String,
    scopes: Vec<Scope>,
    /// Whether to report problems: not for the standard library but in `Scope::Library`'s check,
    /// and not for a field default looked at only for its type.
    pub(crate) report: bool,
    /// The return type written in the signature, if any.
    pub(crate) declared: Option<Type>,
    pub(crate) generator: bool,
    /// What a `with` source lends, from its `-> with T`, and how many `lend`s it has so far.
    pub(crate) lends: Option<Type>,
    pub(crate) lent: usize,
    /// Whether every way through the source to here that hasn't failed has passed its `lend`,
    /// kept with each path at a branch and joined where paths meet, as what names hold is.
    pub(crate) lent_here: Lending,
    /// Where a `lend` in a loop is, refused for that already.
    pub(crate) lend_in_loop: Option<Span>,
    /// The spans of the values `with` blocks are taking now, where a source may be called.
    pub(crate) with_values: Vec<(u32, u32)>,
    /// The `match`es written as statements of their own, which may change what they're over
    /// through a `ref` case.
    pub(crate) statement_matches: Vec<(u32, u32)>,
    /// The routes written with a method so far, for one that clashes with one before it.
    pub(crate) routes_here: Vec<crate::routes::Routed>,
    /// Whether it can fail, so it can give back a result it gets unhandled.
    pub(crate) fallible: bool,
    pub(crate) returns: Vec<Type>,
    pub(crate) errors: Vec<Type>,
    pub(crate) yields: Vec<Type>,
    /// The type parameters in scope, with the interfaces each has to fit.
    pub(crate) generics: Vec<Generic>,
    /// How many problems were reported. Types left open after one are consequences, not news.
    pub(crate) reported: usize,
    /// Types filled in for type parameters that weren't worked out when they were used,
    /// with the bounds they have to fit and where.
    pub(crate) pending_bounds: Vec<(Type, Vec<String>, Span)>,
    /// Calls to `sum` whose items weren't worked out when they were checked.
    pub(crate) pending_sums: Vec<crate::sum::SumCall>,
    /// Names read where a generator that can fail can't go, whose types weren't worked out yet
    /// when they were read: each name, its type, where, and whether it's an arrow's own
    /// parameter. They're looked at again once the function's types are.
    pub(crate) pending_generators: Vec<(String, Type, Span, bool)>,
    /// Every expression's type and every declared name's, as they're found.
    exprs: Vec<(Span, Type)>,
    pub(crate) names: Vec<(Span, String, Type)>,
    /// The first type variable this check made.
    first_var: usize,
    /// The type expected where the next expression checked is written, which decides the
    /// enum of a leading-dot case: `.Int(value)` passed for an `ExprKind` is `ExprKind.Int`.
    pub(crate) expecting: Option<Type>,
    /// The type expected of the value of the block being checked: the return type for a
    /// function's body, or what's expected of a `match` for its cases.
    pub(crate) tail: Option<Type>,
    /// Where each name that isn't `mut` was read from, as it's written: `units[at]` for
    /// `target = units[at]`.
    pub(crate) read_from: std::collections::HashMap<String, String>,
    /// The functions in sight, by name, that capture a `shared` handle.
    pub(crate) handle_closures: std::collections::HashSet<String>,
    /// The generators right inside what uses all of their items, by span.
    pub(crate) worked_out_here: std::collections::HashSet<(u32, u32)>,
    /// The `/`s between two ints, which give a float, so a mix with an int there can say `//`.
    pub(crate) int_divisions: std::collections::HashSet<(u32, u32)>,
    /// Each `Err(...)` a `return` gives back, by span, with the `return`'s span: `fennec fix`
    /// writes it `fail ...`.
    pub(crate) returned_errs: std::collections::HashMap<(u32, u32), Span>,
    /// How many generators kept for later this is inside.
    pub(crate) kept_generators: usize,
    /// Each `?` a `for` loops over right there, as the loop's own `made()?` or an argument of its
    /// `enumerate` or `zip`, by span. Only there does a generator that can fail go into
    /// `enumerate` or `zip`, or a `? from` wrap what a generator fails with.
    pub(crate) looped_here: Vec<Span>,
    /// The arrow functions being checked, innermost last, by span: what each waits on and calls
    /// is kept for wherever it's called.
    pub(crate) arrows_open: Vec<(u32, u32)>,
    /// The functions whose arguments are being matched, innermost last, for what's passed for a
    /// function parameter.
    pub(crate) calling: Vec<String>,
    /// The name, by span, being read where a function value read is followed or can't call it:
    /// a call's callee, an argument for a function parameter, or one side of an `is None` test.
    /// A function parameter read anywhere else counts as called right there (see
    /// `note_function_read`).
    pub(crate) direct_read: Option<(u32, u32)>,
    /// How many arrow functions and comprehensions this is inside, which `else` can't leave.
    pub(crate) value_functions: usize,
    /// How many loops this is inside, which `break` and `continue` need.
    pub(crate) loops: usize,
    /// How many arrow functions the code being checked is inside.
    pub(crate) arrows: usize,
    /// The fields and method calls in the chain before an `else`, which may be on a value
    /// that might be missing.
    pub(crate) else_chain: std::collections::HashSet<(u32, u32)>,
    /// The subjects of the `match`es `except` arms are written in as, by span, so a case they
    /// leave out is said to be the `except`'s.
    pub(crate) except_subjects: std::collections::HashSet<(u32, u32)>,
    /// Those of them whose arms take a whole type of error, already refused, so what they leave
    /// out isn't said again.
    pub(crate) except_refused: std::collections::HashSet<(u32, u32)>,
    /// Which locks are held while checking a lock body, a `shared` type's method or a `with`
    /// block that locks handles, for messages: "while `x` is locked".
    pub(crate) lock: Option<String>,
    /// The `shared` types whose locks are held, which a lock body can take only the inner
    /// types of: those they hold.
    pub(crate) locked_types: Vec<String>,
    /// Where each field path checked for `None`, like `holder.cell`, might have changed since, for
    /// saying so when it's read as if it couldn't be `None`.
    pub(crate) changed_at: std::collections::HashMap<String, Span>,
    /// The error it says it fails with, from `fails E`.
    pub(crate) fails: Option<Type>,
}

impl<'c> FnCx<'c> {
    pub(crate) fn new(checker: &'c mut Checker, key: &str, name: &str, report: bool) -> Self {
        let first_var = checker.vars.len();
        FnCx {
            assigned: Vec::new(),
            checker,
            key: key.to_owned(),
            name: name.to_owned(),
            scopes: vec![Scope { bindings: Vec::new(), barrier: false, apart: false, lent: Vec::new() }],
            report,
            declared: None,
            generator: false,
            lends: None,
            lent: 0,
            lent_here: Lending::NotYet,
            lend_in_loop: None,
            with_values: Vec::new(),
            statement_matches: Vec::new(),
            routes_here: Vec::new(),
            fallible: false,
            returns: Vec::new(),
            errors: Vec::new(),
            yields: Vec::new(),
            generics: Vec::new(),
            pending_bounds: Vec::new(),
            pending_sums: Vec::new(),
            pending_generators: Vec::new(),
            reported: 0,
            exprs: Vec::new(),
            names: Vec::new(),
            first_var,
            expecting: None,
            tail: None,
            read_from: std::collections::HashMap::new(),
            handle_closures: std::collections::HashSet::new(),
            worked_out_here: std::collections::HashSet::new(),
            int_divisions: std::collections::HashSet::new(),
            returned_errs: std::collections::HashMap::new(),
            kept_generators: 0,
            looped_here: Vec::new(),
            arrows_open: Vec::new(),
            calling: Vec::new(),
            direct_read: None,
            value_functions: 0,
            loops: 0,
            arrows: 0,
            else_chain: std::collections::HashSet::new(),
            except_subjects: std::collections::HashSet::new(),
            except_refused: std::collections::HashSet::new(),
            fails: None,
            lock: None,
            locked_types: Vec::new(),
            changed_at: std::collections::HashMap::new(),
        }
    }

    /// Ends a check: settles every type found, reports what nothing settled, and keeps the
    /// types under `key` in the typed tree. `extra` are more types that have to be settled,
    /// like the function's return type.
    pub(crate) fn finish(mut self, extra: &[Type]) {
        self.check_pending_bounds();
        self.check_pending_sums();
        self.check_pending_generators();
        // An expression looked at twice, like an argument peeked at first, keeps the type
        // it got last.
        let exprs: std::collections::BTreeMap<(u32, u32), Type> =
            self.exprs.iter().map(|(span, ty)| ((span.start, span.end), ty.clone())).collect();
        let names: std::collections::BTreeMap<((u32, u32), String), Type> = self
            .names
            .iter()
            .map(|(span, name, ty)| (((span.start, span.end), name.clone()), ty.clone()))
            .collect();
        crate::keys::check_dict_keys(&mut self, &exprs);
        let mut all: Vec<Type> = exprs.values().chain(names.values()).cloned().collect();
        all.extend(extra.iter().cloned());
        if self.reported == 0 {
            for (_, span, hole, name) in self.checker.open_since(self.first_var, &all) {
                self.error(unsettled(&hole, span, name.as_deref()));
            }
        }
        let checker = &mut *self.checker;
        let body = checker.typed.entry(self.key.clone()).or_default();
        body.names_outer = checker.names_outer.contains(&self.key);
        for (span, ty) in std::mem::take(&mut self.exprs) {
            body.exprs.insert((span.start, span.end), ty);
        }
        for (span, name, ty) in std::mem::take(&mut self.names) {
            checker.note_local(&self.key, &name, &ty);
            if let Some(body) = checker.typed.get_mut(&self.key) {
                body.names.insert(((span.start, span.end), name), ty);
            }
        }
    }

    /// Notes that this function names a `with` source's own function, constant or module, written
    /// in where something in sight names it too (`sight::outer`): IR lowering gives its own names
    /// apart first.
    pub(crate) fn note_outer(&mut self) {
        if !self.checker.names_outer.contains(&self.key) {
            self.checker.names_outer.insert(self.key.clone());
        }
    }

    /// A name read for what it stands for at the top level, a type or a special form: the name a
    /// `with` source's own is written in as (`sight::outer`), noted, and whether it was one, which
    /// nothing in sight takes.
    pub(crate) fn meant<'n>(&mut self, written: &'n str) -> (&'n str, bool) {
        match fennec_syntax::sight::outer(written) {
            Some(meant) => {
                self.note_outer();
                (meant, true)
            }
            None => (written, false),
        }
    }

    /// The type found for the expression written at a span, if it's been checked.
    pub(crate) fn recorded(&self, span: Span) -> Option<Type> {
        self.exprs.iter().rev().find(|(at, _)| *at == span).map(|(_, ty)| ty.clone())
    }

    /// Records an expression's type for the typed tree.
    pub(crate) fn record(&mut self, span: Span, ty: &Type) {
        self.exprs.push((span, ty.clone()));
    }

    pub(crate) fn check_function(
        checker: &'c mut Checker,
        key: &str,
        def: &FnDef,
        params: &[ParamType],
        captured: Vec<Binding>,
        report: bool,
    ) -> BodyType {
        let mut cx = FnCx::new(checker, key, &def.name, report);
        assigned_in(&def.body, &mut cx.assigned);
        let entry = cx.checker.program.functions.get(key);
        cx.generics = entry.map(|entry| cx.checker.program.generics_of(entry)).unwrap_or_default();
        cx.declared = crate::program::written_ret(def).map(|ret| cx.resolve(ret));
        cx.lends = cx.declared.as_ref().and_then(crate::lend::lent).cloned();
        cx.fails = def.fails.as_ref().map(|fails| cx.resolve(fails));
        if def.body.is_empty() {
            return cx.bodiless(def);
        }
        let effects = effects::of_body(&def.body);
        (cx.generator, cx.fallible) = (effects.yields, effects.fails || def.fails.is_some());
        cx.no_ref_parameters(def, effects.yields);
        for binding in captured {
            cx.capture(binding);
        }
        cx.push(false);
        for (param, written) in params.iter().zip(&def.params) {
            cx.default_written(written);
            if let Some(default) = &written.default {
                // A parameter with no type written takes it from its default, so only a written
                // one is expected of the default.
                let actual =
                    if written.ty.is_some() { cx.expr_as(default, &param.ty) } else { cx.expr(default) };
                if let (ExprKind::None, Some(ty)) = (&default.kind, &written.ty)
                    && !param.ty.maybe_none()
                {
                    cx.error(none_for_plain_type(&param.name, &param.ty, ty.span, default.span));
                } else {
                    cx.check_arg(param, &actual, default.span);
                }
            }
            cx.declare_as(&param.name, param.ty.clone(), param.is_mut, Origin::Param, written.span);
        }
        cx.main_is_given_capabilities(params, def);
        cx.self_locked(params, def);
        cx.tail = if cx.generator || cx.lends.is_some() { None } else { cx.declared.clone() };
        let flow = cx.stmts(&def.body, cx.lends.is_none());
        if cx.lends.is_some() {
            cx.source_finished(def, &flow);
        }
        // A body that always leaves early has an unknown value, which fits any type.
        if cx.lends.is_some() {
            // A source gives back what it lends, through `lend`, and nothing from its body.
        } else if let (Some(declared), Some(StmtKind::Expr(last))) =
            (&cx.declared, def.body.last().map(|stmt| &stmt.kind))
        {
            let declared = declared.clone();
            cx.check_return(&declared, &flow.ty, last.span);
        } else if let Some(declared) = cx.declared.clone() {
            cx.check_gives_back(&declared, &flow, def);
        }
        let mut values = cx.returns.clone();
        // A function that leaves with a bare `return` gives back nothing, so the value its
        // last line happens to have is dropped.
        let bare = !values.is_empty() && values.iter().all(|ty| *ty == Type::Unit);
        if let (false, false, Some(StmtKind::Expr(last))) =
            (bare, cx.lends.is_some(), def.body.last().map(|stmt| &stmt.kind))
        {
            cx.passes_up_unmarked(last, &flow.ty);
        }
        if !flow.exits && bare {
            if let (Type::Result(..), Some(last)) = (cx.checker.settled(&flow.ty), def.body.last()) {
                cx.error(crate::messages::unhandled(last.span));
            }
            values.push(Type::Unit);
        } else if !flow.exits {
            values.push(flow.ty);
        }
        let value = if let Some(declared) = &cx.declared {
            // Each value given back, or yielded, was checked against it already.
            declared.clone()
        } else if cx.generator {
            let yields = cx.yields.clone();
            Type::iterator(cx.joined(&yields, def.span, "yields"))
        } else if values.is_empty() {
            // A body that never finishes, like `while True:` with no `break`, gives nothing.
            Type::Unit
        } else {
            cx.joined(&values, def.span, "returns")
        };
        let errors = cx.errors.clone();
        let error =
            if errors.is_empty() { Type::Unknown } else { cx.joined(&errors, def.span, "fails with") };
        let (value, error) = (cx.checker.settled(&value), cx.checker.settled(&error));
        cx.finish(&[value.clone(), error.clone()]);
        BodyType { value, error }
    }

    /// A `def` with no body: an intrinsic, which the engines provide, gives back what its
    /// signature says. Only the standard library's `@intrinsic` functions have no body.
    fn bodiless(mut self, def: &FnDef) -> BodyType {
        if !def.annotations.iter().any(|annotation| annotation.name == "intrinsic") {
            self.error(Diagnostic::new(format!("`{}` has no body", def.name), def.span).with_help(
                "end the signature with `:` and write its body below: only std's `@intrinsic` functions have none",
            ));
        }
        let value = self.declared.clone().unwrap_or(Type::Unit);
        self.finish(std::slice::from_ref(&value));
        BodyType { value, error: Type::Unknown }
    }

    /// Checks that a function whose last line isn't a value doesn't run off its end, when its
    /// return type says it gives something back.
    fn check_gives_back(&mut self, declared: &Type, flow: &crate::stmt::Flow, def: &FnDef) {
        let nothing = matches!(self.checker.settled(&flow.ty), Type::Unit);
        if flow.exits || self.generator || !nothing || matches!(declared, Type::Unit | Type::Unknown) {
            return;
        }
        let at = def.body.last().map_or(def.span, |last| last.span);
        let declared = crate::types::a(declared);
        self.error(
            Diagnostic::new(
                format!("`{}` returns {declared}, but its last line gives back nothing", self.name),
                at,
            )
            .with_help(format!("end it with {declared}, or change the return type")),
        );
    }

    /// Joins what a function gives back from several places, reporting values that don't
    /// fit together.
    pub(crate) fn joined(&mut self, types: &[Type], span: Span, verb: &str) -> Type {
        if let Some(joined) = self.checker.join_all(types) {
            return joined;
        }
        let kinds: Vec<String> = types.iter().map(|ty| crate::types::a(&self.checker.settled(ty))).collect();
        let mut distinct = kinds.clone();
        distinct.dedup();
        self.error(
            Diagnostic::new(format!("`{}` {verb} {}", self.name, distinct.join(" and ")), span)
                .with_help("give back one type everywhere, or write the return type to say which"),
        );
        Type::Unknown
    }

    /// A written type, with this function's type parameters in scope.
    pub(crate) fn resolve(&mut self, written: &fennec_syntax::TypeExpr) -> Type {
        let names: Vec<String> = self.generics.iter().map(|generic| generic.name.clone()).collect();
        match self.checker.program.resolve_in(written, &names) {
            Ok(ty) => ty,
            Err(problem) => {
                self.error(problem);
                Type::Unknown
            }
        }
    }

    /// The type a written type means, or `None` if it means nothing, which is reported
    /// where it's checked.
    pub(crate) fn written_type(&self, written: &fennec_syntax::TypeExpr) -> Option<Type> {
        let names: Vec<String> = self.generics.iter().map(|generic| generic.name.clone()).collect();
        self.checker.program.resolve_in(written, &names).ok()
    }

    /// Reports a failure passed up with no `?`: `return load(path)`, or a last line `load(path)`,
    /// gives back what can fail as it is. `?` is how a failure passes up, everywhere, so the
    /// reader sees each place one can. `return Err(...)`, and a function that says it gives back
    /// a `Result`, give back a value.
    pub(crate) fn passes_up_unmarked(&mut self, value: &Expr, actual: &Type) {
        if !matches!(self.checker.settled(actual), Type::Result(..))
            || !(self.fallible || self.declared.is_none())
            || self.generator
            || matches!(self.declared, Some(Type::Result(..)))
        {
            return;
        }
        match &value.kind {
            ExprKind::Call { callee, .. } if matches!(&callee.kind, ExprKind::Name(name) if name == "Err" || name == "Ok") =>
                {}
            ExprKind::Match { .. } | ExprKind::Cond { .. } => {}
            _ => {
                let end = Span { start: value.span.end, end: value.span.end };
                self.error(
                    Diagnostic::new("this can fail, and a failure passes up only with `?`", value.span)
                        .with_help("write `?` after it: `fennec fix` makes this change")
                        .with_fix(end, "?"),
                );
            }
        }
    }

    /// Checks a value returned from a function whose return type is written.
    pub(crate) fn check_return(&mut self, declared: &Type, actual: &Type, span: Span) {
        let actual = self.checker.settled(actual);
        let passed_through = self.fallible && matches!(actual, Type::Result(..));
        // A result passed on as it is can fail its own ways, which become this function's too,
        // unless `fails E` says what it fails with, which the result is checked against below.
        if let (true, None, Type::Result(_, error)) = (passed_through, &self.fails, &actual)
            && error.is_known()
            && !self.errors.contains(error)
        {
            self.errors.push(error.as_ref().clone());
        }
        // A result passed through has to hold what it returns, and fail only with the error
        // `fails E` names.
        if let (true, Some(own)) = (passed_through, self.fails.clone()) {
            if !self.checker.fits(&Type::result(declared.clone(), own.clone()), &actual) {
                let message = match &actual {
                    Type::Result(_, error) if !self.checker.fits(&own, error) => {
                        let (own, error) = (crate::types::a(&own), crate::types::a(error));
                        format!("`{}` says it fails with {own}, but this fails with {error}", self.name)
                    }
                    _ => format!(
                        "`{}` returns {} or fails with {}, but this is {}",
                        self.name,
                        crate::types::a(declared),
                        crate::types::a(&own),
                        crate::types::a(&actual)
                    ),
                };
                self.error(
                    Diagnostic::new(message, span)
                        .with_help("fail with the error it names, or change `fails`"),
                );
            }
            return;
        }
        if self.generator || passed_through || self.checker.fits(declared, &actual) {
            return;
        }
        let diagnostic = if actual == Type::None && !declared.maybe_none() {
            // Python's `return None` from a function that gives back a plain value.
            let shown = crate::types::a(declared);
            Diagnostic::new(format!("`{}` gives back {shown}, which can't be `None`", self.name), span)
                .with_help(format!("write `-> {declared}?` to give back `None` when there's nothing"))
        } else if actual.maybe_none() && !declared.maybe_none() {
            crate::messages::might_be_none(span)
        } else if matches!(actual, Type::Result(..)) {
            crate::messages::unhandled_result(span)
        } else {
            let (declared, actual) = (crate::types::a(declared), crate::types::a(&actual));
            Diagnostic::new(format!("`{}` returns {declared}, but this is {actual}", self.name), span)
                .with_help(format!("return {declared}, or change the return type"))
        };
        self.error(diagnostic);
    }

    /// Whether this is the check of a `test` block, or of the function LLVM's tests make of one,
    /// whose name no program can write.
    pub(crate) fn in_a_test(&self) -> bool {
        self.key.starts_with("test \"") || self.key.starts_with("test:")
    }

    /// A default's type comes from its literal, so writing that type too is a mistake, and a
    /// default that isn't such a literal needs its type written.
    fn default_written(&mut self, param: &fennec_syntax::Param) {
        let Some(default) = &param.default else { return };
        // A default rules out `mut`, so the name starts the parameter.
        let name_end = param.span.start + u32::try_from(param.name.len()).unwrap_or(0);
        match &param.ty {
            None if crate::program::literal_type(default, &self.checker.program).is_none() => {
                let message = format!("can't tell the type of `{}` from its default", param.name);
                let help = match &default.kind {
                    ExprKind::List(_) => {
                        format!("say it, like `{} = list[int]()`", param.name)
                    }
                    ExprKind::Dict(_) => {
                        format!("say it, like `{} = dict[str, int]()`", param.name)
                    }
                    _ => format!("write its type, like `{}: Enemy? = None`", param.name),
                };
                self.error(Diagnostic::new(message, default.span).with_help(help));
            }
            None => {}
            Some(written) => {
                let resolved = self.resolve(written);
                let at = (param.name.as_str(), name_end);
                if let Some(problem) =
                    crate::program::default_problem(&self.checker.program, at, (written, &resolved), default)
                {
                    self.error(problem);
                }
            }
        }
    }

    /// A `shared` type's method, which holds its lock for the whole call.
    fn self_locked(&mut self, params: &[ParamType], def: &FnDef) {
        if def.params.first().is_some_and(|param| param.is_self)
            && params.first().is_some_and(|param| self.checker.program.takes_lock(&param.ty))
        {
            self.lock = Some("while `self` is locked".to_owned());
            self.locked_types = params
                .first()
                .and_then(|param| param.ty.type_name())
                .map(str::to_owned)
                .into_iter()
                .collect();
        }
    }

    /// Refuses a generator's `ref` parameters, when `def` is one. A generator runs a little at a
    /// time, after its call has given it back, so a `ref` it took would be held across every
    /// `yield` while its caller changes the place by its own name.
    fn no_ref_parameters(&mut self, def: &FnDef, generator: bool) {
        for written in def.params.iter().filter(|written| generator && written.is_mut) {
            self.error(ref_in_a_generator(written));
        }
    }

    /// Refuses a parameter of a program's `main` that isn't a capability, as `args: list[str]`: a
    /// program is started with its capabilities, and with nothing else to hand `main`.
    fn main_is_given_capabilities(&mut self, params: &[ParamType], def: &FnDef) {
        if self.key != "main" {
            return;
        }
        for (param, written) in params.iter().zip(&def.params) {
            let ty = self.checker.settled(&param.ty);
            if crate::shared::is_capability_type(&ty) && written.default.is_none() {
                continue;
            }
            let message = format!(
                "`main` can't be given `{}`: a program is started with its capabilities, and nothing else",
                param.name
            );
            let help = "`main` can take `stdout: Stdout`, `stderr: Stderr`, `stdin: Stdin`, `args: Args`, `fs: Fs`, `clock: Clock`, `process: Process`, `entropy: Entropy`, `net: Net`, `env: Env` and `signals: Signals`, in any order";
            self.error(Diagnostic::new(message, written.span).with_help(help));
        }
    }

    pub(crate) fn check_test(checker: &'c mut Checker, test: &TestDef) {
        let key = crate::program::test_key(&test.name);
        let mut cx = FnCx::new(checker, &key, "test", true);
        for capability in &test.capabilities {
            let name = capability.ty.as_ref().map_or("", |ty| ty.name.as_str());
            let fits = matches!(
                name,
                "Stdout" | "Stderr" | "Fs" | "Clock" | "Process" | "Entropy" | "Net" | "Env" | "Signals"
            ) && capability.default.is_none();
            if !fits {
                let message = format!("a test can't receive `{}` this way", capability.name);
                let help = "a test can receive `stdout: Stdout`, `stderr: Stderr`, `fs: Fs`, `clock: Clock`, `process: Process`, `entropy: Entropy`, `net: Net`, `env: Env` and `signals: Signals`";
                cx.error(Diagnostic::new(message, capability.span).with_help(help));
            }
            let ty = if fits { Type::named(name) } else { Type::Unknown };
            cx.declare(&capability.name, ty, capability.is_mut, capability.span);
        }
        for input in &test.inputs {
            let ty = input.ty.as_ref().map_or(Type::Unknown, |ty| cx.resolve(ty));
            cx.declare(&input.name, ty, input.is_mut, input.span);
        }
        // What it fails with is the compiler's to write, once it's worked it out (#2040).
        if let Some(fails) = &test.fails {
            if cx.checker.again {
                cx.fails = Some(cx.resolve(fails));
            } else {
                let message = "a test can't say what it fails with";
                cx.error(Diagnostic::new(message, fails.span).with_help("take the `fails` away"));
            }
        }
        cx.stmts(&test.body, false);
        // What it fails with, worked out from its body as a function's is, for lowering to write:
        // by its span, since a test and a bench may share a name.
        let errors = cx.errors.clone();
        if !errors.is_empty() {
            let error = cx.joined(&errors, test.span, "fails with");
            let error = cx.checker.settled(&error);
            cx.checker.test_errors.insert((test.span.start, test.span.end), error);
        }
        cx.finish(&[]);
        if test.inputs.is_empty() && test.capabilities.is_empty() {
            checker.record_test(key, &test.body);
        }
    }

    /// Checks a type's field defaults against their types, and its `where` rules.
    pub(crate) fn check_fields(checker: &'c mut Checker, type_name: &str) {
        let Some(info) = checker.program.type_info(type_name) else { return };
        let def = info.def.clone();
        let report = checker.reports(info.prelude);
        let mut cx = FnCx::new(checker, &format!("fields of {type_name}"), type_name, report);
        cx.generics.clone_from(&def.generics);
        for field in &def.fields {
            cx.check_field_default(type_name, None, field);
        }
        for case in &def.cases {
            for field in &case.fields {
                cx.check_field_default(type_name, Some(&case.name), field);
            }
        }
        let rules: Vec<&Expr> = def.fields.iter().filter_map(|field| field.rule.as_ref()).collect();
        if !rules.is_empty() {
            cx.push(false);
            for field in &def.fields {
                let ty = cx.checker.field_type(type_name, None, &field.name);
                cx.declare(&field.name, ty, false, field.span);
            }
            for rule in rules {
                cx.condition(rule, "where");
            }
            cx.pop();
            for field in &def.fields {
                crate::rules::check_field(&mut cx, type_name, field, &def.fields);
            }
        }
        cx.finish(&[]);
    }

    /// Works out a field's type, whether anything uses the field or not, and checks its default
    /// fits it.
    fn check_field_default(&mut self, type_name: &str, case: Option<&str>, field: &Field) {
        let expected = self.checker.field_type(type_name, case, &field.name);
        let Some(default) = &field.default else { return };
        // A field with no type written takes it from its default, so only a written one is
        // expected of the default.
        let actual = if field.ty.is_some() { self.expr_as(default, &expected) } else { self.expr(default) };
        if !self.checker.fits(&expected, &actual) {
            let what = format!("`{}`", field.name);
            self.error(crate::messages::mismatch(&what, &expected, &actual, default.span));
        }
        // A field whose type its default doesn't show has been reported already.
        self.reported += usize::from(expected == Type::Unknown);
    }

    /// The type of a field's default, without reporting anything: that happens once, when
    /// the type's fields are checked.
    pub(crate) fn type_of_default(checker: &'c mut Checker, default: &Expr, owner: &str) -> Type {
        let mut cx = FnCx::new(checker, "", "", false);
        // A default can name the type's own parameters, like `items = list[T]()`.
        cx.generics =
            cx.checker.program.type_info(owner).map(|info| info.def.generics.clone()).unwrap_or_default();
        let ty = cx.expr(default);
        cx.checker.settled(&ty)
    }

    pub(crate) fn error(&mut self, diagnostic: Diagnostic) {
        if self.report {
            self.reported += 1;
            self.checker.diagnostics.push(diagnostic);
        }
    }

    /// An expression's type, without reporting anything about it.
    pub(crate) fn peek_type(&mut self, expr: &Expr) -> Type {
        let report = std::mem::replace(&mut self.report, false);
        let ty = self.expr(expr);
        self.report = report;
        ty
    }

    pub(crate) fn push(&mut self, barrier: bool) {
        self.scopes.push(Scope { bindings: Vec::new(), barrier, apart: false, lent: Vec::new() });
    }

    /// Where the innermost scope is, counting out from the function's own.
    pub(crate) fn innermost_scope(&self) -> usize {
        self.scopes.len().saturating_sub(1)
    }

    /// The scope the name in sight is declared in, counting out from the function's own.
    pub(crate) fn scope_of(&self, name: &str) -> Option<usize> {
        self.scopes.iter().rposition(|scope| scope.bindings.iter().any(|binding| binding.name == name))
    }

    /// What a `with` lent that the name declared in scope `at` may hold: each by its type and the
    /// scope of the `with` block that lent it.
    pub(crate) fn lent_in(&self, at: usize, name: &str) -> Vec<(Type, usize)> {
        let Some(scope) = self.scopes.get(at) else { return Vec::new() };
        scope
            .lent
            .iter()
            .filter(|(held, ..)| held == name)
            .map(|(_, lent, block)| (lent.clone(), *block))
            .collect()
    }

    /// Notes that the name declared in scope `at` may hold what the `with` block at scope `block`
    /// lent, of type `lent`.
    pub(crate) fn hold_lent(&mut self, at: usize, name: &str, (lent, block): (Type, usize)) {
        if let Some(scope) = self.scopes.get_mut(at)
            && !scope.lent.iter().any(|(held, had, from)| held == name && *had == lent && *from == block)
        {
            scope.lent.push((name.to_owned(), lent, block));
        }
    }

    /// Whether any name in sight may hold what a `with` lent.
    pub(crate) fn any_lent(&self) -> bool {
        self.scopes.iter().any(|scope| !scope.lent.is_empty())
    }

    /// Marks the innermost scope as a `parallel` loop's or comprehension's body, which runs on
    /// other tasks.
    pub(crate) fn set_apart(&mut self) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.apart = true;
        }
    }

    /// A name read where a generator that can fail can't go, as one from around a nested
    /// function, an arrow function, a generator kept for later or a `parallel` body, or an arrow's
    /// own parameter: refused when its type is or holds one. A type not worked out yet is looked at
    /// again once the function's types are.
    pub(crate) fn no_failing_generator_here(&mut self, name: &str, ty: &Type, span: Span, param: bool) {
        let settled = self.checker.settled(ty);
        if !settled.held_generator_failures().is_empty() {
            self.error(failing_generator_reached(name, &settled, span, param));
        } else if !settled.is_known() {
            self.pending_generators.push((name.to_owned(), ty.clone(), span, param));
        }
    }

    /// The names `no_failing_generator_here` couldn't tell about as they were read, now that the
    /// function's types are worked out. One still open is a name a nested function read from the
    /// function around it, whose type that function works out: it's handed up to it.
    fn check_pending_generators(&mut self) {
        for (name, ty, span, param) in std::mem::take(&mut self.pending_generators) {
            let settled = self.checker.settled(&ty);
            if !settled.held_generator_failures().is_empty() {
                self.error(failing_generator_reached(&name, &settled, span, param));
            } else if !settled.is_known() {
                self.checker.generators_handed_up.push((name, ty, span, param));
            }
        }
    }

    /// The items of a `parallel` loop or comprehension over `iter`, when it's one: `parallel(xs)`
    /// written as the standard library's, and not a name of the program's own.
    pub(crate) fn parallel_over<'e>(&self, iter: &'e Expr) -> Option<&'e Expr> {
        let outer = crate::parallel::called_as_outer(iter);
        crate::parallel::parallel_items(iter).filter(|_| outer || self.lookup("parallel").is_none())
    }

    /// `parallel` over items that hold a generator that can fail is refused: each item goes to
    /// another task, where its failure can't pass up.
    pub(crate) fn no_failing_generator_apart(&mut self, item: &Type, span: Span) {
        if !self.checker.settled(item).held_generator_failures().is_empty() {
            self.error(crate::messages::failing_generator_in_parallel(span));
        }
    }

    /// The names declared in the innermost scope, with what each holds.
    pub(crate) fn bound_here(&self) -> Vec<(String, Type)> {
        self.scopes
            .last()
            .map(|scope| {
                scope.bindings.iter().map(|binding| (binding.name.clone(), binding.ty.clone())).collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn pop(&mut self) {
        self.scopes.pop();
    }

    /// Declares a name made at `at`: by an assignment, a loop, a pattern or a parameter.
    pub(crate) fn declare(&mut self, name: &str, ty: Type, is_mut: bool, at: Span) {
        self.declare_as(name, ty, is_mut, Origin::Local, at);
    }

    pub(crate) fn declare_as(&mut self, name: &str, ty: Type, is_mut: bool, origin: Origin, at: Span) {
        let param_of = (origin == Origin::Param).then(|| self.key.clone());
        self.declare_with(
            Binding {
                name: name.to_owned(),
                ty: ty.clone(),
                declared: ty,
                is_mut,
                origin,
                mut_at: None,
                to_every: false,
                param_of,
                arrow: None,
                alias: None,
            },
            at,
        );
    }

    /// Declares a name by assigning to it. A name first given `None` holds an optional of a
    /// type still to be worked out, from what's given to it later.
    pub(crate) fn declare_assigned(&mut self, name: &str, ty: Type, is_mut: bool, at: Span) {
        let declared = match self.checker.settled(&ty) {
            Type::None => Type::Optional(Box::new(self.checker.fresh(at, Hole::None))),
            other => other,
        };
        let binding = Binding {
            name: name.to_owned(),
            ty,
            declared,
            is_mut,
            origin: Origin::Local,
            mut_at: Some(at.start),
            to_every: false,
            param_of: None,
            arrow: None,
            alias: None,
        };
        self.declare_with(binding, at);
    }

    /// Declares a name with the type it was declared with, and what it holds now.
    pub(crate) fn declare_with(&mut self, binding: Binding, at: Span) {
        if !binding.name.contains('.') {
            self.checker.name_vars(&binding.declared, &binding.name);
            self.names.push((at, binding.name.clone(), binding.declared.clone()));
        }
        self.bind(binding);
    }

    /// A name from around a nested function, which it can read. What a crash runs for a `with`
    /// block can change its own copy of one too, as the `else:` it's written from does.
    fn capture(&mut self, binding: Binding) {
        let crash =
            self.key.rsplit('.').next().is_some_and(|own| own.starts_with(crate::lend_lowering::ON_CRASH));
        let is_mut = binding.is_mut && crash;
        self.bind(Binding { is_mut, origin: Origin::Captured, ..binding });
    }

    /// `name`, just declared by unpacking, which `mut` can't be written before.
    pub(crate) fn unpacked(&mut self, name: &str) {
        let here = self
            .scopes
            .last_mut()
            .and_then(|scope| scope.bindings.iter_mut().rev().find(|binding| binding.name == name));
        if let Some(binding) = here {
            binding.mut_at = None;
        }
    }

    fn bind(&mut self, binding: Binding) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.bindings.push(binding);
        }
    }

    /// Why a name can't be changed here, and how to fix it.
    pub(crate) fn cannot_change(&self, name: &str, span: Span) -> Diagnostic {
        let mut crossed = false;
        let mut found = None;
        for scope in self.scopes.iter().rev() {
            found = scope.bindings.iter().rev().find(|binding| binding.name == name);
            if found.is_some() {
                break;
            }
            crossed |= scope.barrier;
        }
        let Some(binding) = found else { return not_mut(name, span, None) };
        if crossed && self.kept_generators > 0 {
            let message =
                format!("a generator kept for later can't change `{name}`, which is defined outside it");
            return Diagnostic::new(message, span).with_help(
                "it runs after this line, when it's asked for an item: write it right inside `sum(...)` or `list(...)`, or write a loop",
            );
        }
        // A new name, from a `for` or a `case`, over one that can change.
        let same_name = self.scopes.iter().rev().flat_map(|scope| scope.bindings.iter().rev());
        let hidden = same_name.filter(|binding| binding.name == name).skip(1).find(|binding| binding.is_mut);
        if let Some(hidden) = hidden
            && !crossed
            && !binding.is_mut
        {
            // Only a local is written `mut`: a parameter, loop or `with` name is `ref`.
            let word = if matches!(hidden.origin, Origin::Local | Origin::Captured) { "mut" } else { "ref" };
            return Diagnostic::new(
                format!("this `{name}` is a new name, which hides the `{word} {name}` around it"),
                span,
            )
            .with_help(format!("give this one another name, to change the `{name}` around it"));
        }
        if crossed || binding.origin == Origin::Captured {
            let function = if crossed { "an arrow function" } else { "a nested function" };
            return Diagnostic::new(format!("{function} can't change `{name}`, which is defined outside it"), span)
                .with_help(format!(
                    "functions can read the names around them but not change them: return a value instead, and change `{name}` outside the function"
                ));
        }
        let help = match binding.origin {
            Origin::Param if name == "self" => {
                format!("to change `self`, write `ref self` in `{}`'s parameters", self.name)
            }
            Origin::Param => format!(
                "parameters are read-only: to change it, write `ref {name}` in `{}`'s parameters, and pass it with `ref` at each call",
                self.name
            ),
            Origin::Loop => format!("loop with `for ref {name} in ...` to change each item"),
            Origin::Locked => format!("it's locked to read: lock it to write with `as ref {name}`"),
            Origin::Local | Origin::Captured => return not_mut(name, span, binding.mut_at),
        };
        Diagnostic::new(format!("`{name}` isn't `ref`"), span).with_help(help)
    }

    /// The type a name was declared with.
    pub(crate) fn declared_type(&mut self, name: &str) -> Option<Type> {
        self.binding_mut(name).map(|binding| binding.declared.clone())
    }

    /// What a name holds, and whether it can be changed from here.
    pub(crate) fn lookup(&self, name: &str) -> Option<(Type, bool)> {
        let mut crossed = false;
        for scope in self.scopes.iter().rev() {
            if let Some(binding) = scope.bindings.iter().rev().find(|binding| binding.name == name) {
                return Some((binding.ty.clone(), binding.is_mut && !crossed));
            }
            crossed |= scope.barrier;
        }
        None
    }

    /// Whether `name` is from around the nested function, arrow function, generator kept for
    /// later or `parallel` body that this is in, rather than this function's own.
    pub(crate) fn came_from_around(&self, name: &str) -> bool {
        let mut crossed = false;
        for scope in self.scopes.iter().rev() {
            if let Some(binding) = scope.bindings.iter().rev().find(|binding| binding.name == name) {
                return crossed || binding.origin == Origin::Captured;
            }
            crossed |= scope.barrier || scope.apart;
        }
        false
    }

    /// The innermost name in sight that holds this type.
    pub(crate) fn name_holding(&self, ty: &Type) -> Option<String> {
        let mut bindings = self.scopes.iter().rev().flat_map(|scope| scope.bindings.iter().rev());
        bindings.find(|binding| binding.ty == *ty).map(|binding| binding.name.clone())
    }

    /// The name in sight, or function of the program's own, spelled most like `name`.
    pub(crate) fn similar_name(&self, name: &str) -> Option<String> {
        let names =
            self.scopes.iter().flat_map(|scope| scope.bindings.iter().map(|binding| binding.name.as_str()));
        let functions = self.checker.program.functions.iter();
        let functions = functions.filter(|(_, entry)| !entry.prelude && entry.owner.is_none());
        crate::messages::spelled_most_like(name, names.chain(functions.map(|(key, _)| key.as_str())))
            .map(str::to_owned)
    }

    /// Where the name in sight came from.
    pub(crate) fn origin(&self, name: &str) -> Option<Origin> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.bindings.iter().rev())
            .find(|binding| binding.name == name)
            .map(|binding| binding.origin)
    }

    /// The arrow function the local name in sight was declared with, by span, when it isn't
    /// `mut`: then it's all the name can hold. A `mut` name can hold another value by the time it's
    /// read, and a name declared again, even by unpacking, is another binding with none. A nested
    /// function's copy of one it captured holds the same.
    pub(crate) fn local_arrow(&self, name: &str) -> Option<(u32, u32)> {
        let binding = self
            .scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.bindings.iter().rev())
            .find(|binding| binding.name == name)?;
        (matches!(binding.origin, Origin::Local | Origin::Captured) && !binding.is_mut)
            .then_some(binding.arrow)
            .flatten()
    }

    /// The function parameter the name in sight is, by its function's key and its name: a
    /// parameter of this function, one around it a nested function captured, or one a name that
    /// isn't `mut` was declared holding, as `job = work`, however many times over.
    pub(crate) fn param_key(&self, name: &str) -> Option<(String, String)> {
        let binding = self
            .scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.bindings.iter().rev())
            .find(|binding| binding.name == name)?;
        match (&binding.origin, &binding.param_of, &binding.alias) {
            (Origin::Param | Origin::Captured, Some(owner), _) => Some((owner.clone(), name.to_owned())),
            (Origin::Local | Origin::Captured, None, Some(alias)) if !binding.is_mut => Some(alias.clone()),
            _ => None,
        }
    }

    /// Keeps the function value a name that isn't `mut` is declared with on the name's binding:
    /// an arrow function, `name = () => ...` (see `local_arrow`), or a function parameter,
    /// `name = work`, or an optional one unwrapped, `name = work else return` or `name = work?`
    /// (see `param_key`).
    pub(crate) fn keep_function_value(&mut self, target: &Expr, value: &Expr) {
        let ExprKind::Name(name) = &target.kind else { return };
        let arrow =
            matches!(value.kind, ExprKind::Lambda { .. }).then_some((value.span.start, value.span.end));
        // An `else` that leaves gives nothing, so the name holds the parameter's own function.
        let held = match &value.kind {
            ExprKind::Else { value: unwrapped, default, except: false } if default.leaves() => unwrapped,
            ExprKind::Try { value: unwrapped, context: None } => unwrapped,
            _ => value,
        };
        let alias = match &held.kind {
            ExprKind::Name(read) if read != name => self.param_key(read),
            _ => None,
        };
        if arrow.is_none() && alias.is_none() {
            return;
        }
        let Some(binding) = self
            .scopes
            .last_mut()
            .and_then(|scope| scope.bindings.iter_mut().rev().find(|b| b.name == *name))
        else {
            return;
        };
        if binding.origin == Origin::Local
            && !binding.is_mut
            && binding.arrow.is_none()
            && binding.alias.is_none()
        {
            binding.arrow = arrow;
            binding.alias = alias;
        }
    }

    pub(crate) fn binding_mut(&mut self, name: &str) -> Option<&mut Binding> {
        self.scopes
            .iter_mut()
            .rev()
            .flat_map(|scope| scope.bindings.iter_mut().rev())
            .find(|b| b.name == name)
    }

    /// Records what a name holds from now on.
    pub(crate) fn set_type(&mut self, name: &str, ty: Type) {
        if let Some(binding) = self.binding_mut(name) {
            binding.ty = ty;
        }
    }

    /// Every name in sight, for a nested function to read.
    pub(crate) fn visible(&self) -> Vec<Binding> {
        self.scopes.iter().flat_map(|scope| scope.bindings.iter().cloned()).collect()
    }

    /// What every name holds right now, and whether a source has lent on the way here, to come
    /// back to after one path through the code.
    pub(crate) fn snapshot(&self) -> Snapshot {
        let types =
            self.scopes.iter().flat_map(|scope| scope.bindings.iter().map(|b| b.ty.clone())).collect();
        Snapshot { types, lent: self.lent_here }
    }

    pub(crate) fn restore(&mut self, snapshot: &Snapshot) {
        self.restore_types(&snapshot.types);
        self.lent_here = snapshot.lent;
    }

    fn restore_types(&mut self, types: &[Type]) {
        let bindings = self.scopes.iter_mut().flat_map(|scope| scope.bindings.iter_mut());
        for (binding, ty) in bindings.zip(types) {
            binding.ty = ty.clone();
        }
    }

    /// Where paths meet: each name holds what it held on any of them, and a source has lent only
    /// if it did on every one.
    pub(crate) fn merge(&mut self, paths: &[Snapshot]) {
        let Some((first, rest)) = paths.split_first() else { return };
        let declared: Vec<Type> =
            self.scopes.iter().flat_map(|scope| scope.bindings.iter().map(|b| b.declared.clone())).collect();
        let mut merged = Vec::new();
        for (at, ty) in first.types.iter().enumerate() {
            let mut ty = ty.clone();
            for other in rest.iter().filter_map(|path| path.types.get(at)) {
                let fallback = declared.get(at).cloned().unwrap_or(Type::Unknown);
                ty = self.checker.join(&ty, other).unwrap_or(fallback);
            }
            merged.push(ty);
        }
        self.restore_types(&merged);
        let lent = paths.iter().all(|path| path.lent == Lending::Lent);
        self.lent_here = if lent { Lending::Lent } else { Lending::NotYet };
    }

    /// Starts following the field paths a condition checks for None, like `player.weapon`,
    /// so they can be narrowed like names.
    pub(crate) fn track(&mut self, cond: &Expr) {
        for path in not_none(cond, true).into_iter().chain(not_none(cond, false)) {
            if path.contains('.') && self.lookup(&path).is_none() {
                let ty = self.path_type(&path);
                self.declare(&path, ty, false, cond.span);
            }
        }
    }

    fn path_type(&mut self, path: &str) -> Type {
        let report = std::mem::replace(&mut self.report, false);
        let mut fields = path.split('.');
        let root = fields.next().unwrap_or_default();
        let mut ty = self.lookup(root).map_or(Type::Unknown, |(ty, _)| ty);
        for field in fields {
            ty = self.field_of(&ty, field, false, Span::default());
        }
        self.report = report;
        ty
    }

    /// Forgets what was learned about a place's fields, as `place` changes at `at`: the field paths
    /// it is, or is the start of, each hold their field's type again. One that was checked for
    /// `None` notes where it might have changed (`changed_at`). Paths it doesn't reach keep what
    /// was learned: `holder.cell.health = 1` and `holder.name = ""` leave `holder.cell` as checked.
    pub(crate) fn forget_under(&mut self, place: &str, at: Span) {
        let prefix = format!("{place}.");
        let paths: Vec<(String, Type)> = self
            .scopes
            .iter()
            .flat_map(|scope| &scope.bindings)
            .filter(|binding| {
                binding.name.starts_with(&prefix) || (place.contains('.') && binding.name == place)
            })
            .map(|binding| (binding.name.clone(), binding.ty.clone()))
            .collect();
        for (path, was) in paths {
            let ty = self.path_type(&path);
            if !self.checker.settled(&was).maybe_none() && self.checker.settled(&ty).maybe_none() {
                self.changed_at.insert(path.clone(), at);
            }
            self.set_type(&path, ty);
        }
    }

    /// "This might be None" for `span`, a field or method of `object`, when `object` is a field path
    /// checked for `None` that might have changed since: it says where. None when it isn't one.
    pub(crate) fn changed_since_checked(&self, object: &Expr, span: Span) -> Option<Diagnostic> {
        let path = path(object)?;
        let at = *self.changed_at.get(&path)?;
        Some(
            Diagnostic::new("this might be None", span)
                .with_help(format!(
                    "`{path}` was checked for `None`, but the line the note points at might have changed it: check it again after that line, with `if {path} is not None:`"
                ))
                .also_at_the(fennec_syntax::Also::Changed, at),
        )
    }

    /// At the top of a loop: the body may have run already, so what it changes could hold
    /// anything it was declared to, not just what it held before the loop.
    pub(crate) fn forget_changed_in(&mut self, body: &[Stmt]) {
        for stmt in body {
            if let StmtKind::Assign { target, .. } = &stmt.kind {
                match &target.kind {
                    ExprKind::Tuple(items) => items.iter().for_each(|item| self.forget_changing(item)),
                    _ => self.forget_changing(target),
                }
            }
            for expr in exprs(stmt) {
                self.forget_changed_by(expr);
            }
            for block in blocks(stmt) {
                self.forget_changed_in(block);
            }
        }
    }

    /// The same for an expression: what it passes with `mut`, the fields of what it calls a
    /// method on, which may be `mut self`, and the arms of a `match`.
    pub(crate) fn forget_changed_by(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    self.forget_changed_in(&arm.body);
                }
            }
            ExprKind::Call { callee, args } => {
                for arg in args.iter().filter(|arg| arg.is_mut) {
                    self.forget_changing(&arg.value);
                }
                if let ExprKind::Attr { object, .. } = &callee.kind
                    && let Some(name) = path(object)
                    && let Some(root) = name.split('.').next()
                    && self.lookup(root).is_some_and(|(_, is_mut)| is_mut)
                {
                    self.forget_under(&name, callee.span);
                }
            }
            _ => {}
        }
        for child in effects::children(expr) {
            self.forget_changed_by(child);
        }
    }

    fn forget_changing(&mut self, target: &Expr) {
        let Some(name) = path(target) else { return };
        if let Some(binding) = self.binding_mut(&name) {
            binding.ty = binding.declared.clone();
        }
        self.forget_under(&name, target.span);
    }

    /// Records which member of its union each name holds, or doesn't: a name known to hold one
    /// is that member's type, and one known not to is the union of the rest.
    pub(crate) fn narrow_members(&mut self, known: &[(String, String, bool)]) {
        for (name, member, holds) in known {
            let Some(member) = self.member_type(member, Span::default()) else { continue };
            // What's left of the union after an earlier `is`, as after `if s is Circle: return`,
            // is narrowed further; otherwise the union it was declared with.
            let current = self.lookup(name).map(|(ty, _)| self.checker.settled(&ty));
            let ty = match current {
                Some(Type::Union(members)) if members.contains(&member) => Type::Union(members),
                _ => match self.union_of(name) {
                    Some(ty) => ty,
                    None => continue,
                },
            };
            let Type::Union(members) = ty else { continue };
            if !members.contains(&member) {
                continue;
            }
            let narrowed = if *holds {
                member
            } else {
                let mut rest: Vec<Type> = members.into_iter().filter(|other| *other != member).collect();
                if rest.len() == 1 { rest.remove(0) } else { Type::Union(rest) }
            };
            if let Some(binding) = self.binding_mut(name) {
                binding.ty = narrowed;
            }
        }
    }

    /// The union a name or field path holds, by the type it was declared with, since what was
    /// last given to it may have narrowed it to one member already.
    fn union_of(&mut self, name: &str) -> Option<Type> {
        let binding = self
            .scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.bindings.iter().rev())
            .find(|b| b.name == name)?;
        let (ty, declared) = (binding.ty.clone(), binding.declared.clone());
        [declared, ty].into_iter().map(|ty| self.checker.settled(&ty)).find(|ty| matches!(ty, Type::Union(_)))
    }

    /// The type a name after `is` stands for, if it names one: `Circle` in `s is Circle`.
    pub(crate) fn member_type(&self, name: &str, span: Span) -> Option<Type> {
        let written =
            fennec_syntax::TypeExpr { name: name.to_owned(), args: Vec::new(), optional: false, span };
        self.checker.program.resolve(&written).ok().filter(|ty| !matches!(ty, Type::Unknown))
    }

    /// The type a name after `is` stands for, as it's written, when it names one no local takes:
    /// `Circle` in `s is Circle`. A `with` source's own, written into its caller where something
    /// there names it too (`sight::outer`), is the type.
    pub(crate) fn member_named<'n>(&mut self, written: &'n str, at: Span) -> Option<&'n str> {
        let (member, outer) = self.meant(written);
        ((outer || self.lookup(member).is_none()) && self.member_type(member, at).is_some()).then_some(member)
    }

    /// `s is Circle`: a bool, for a name or field holding a union with that member.
    pub(crate) fn is_member(&mut self, value: &Expr, member: &str, at: Span) -> Type {
        let ty = self.expr(value);
        let declared = path(value).and_then(|name| self.union_of(&name));
        let settled = declared.unwrap_or_else(|| self.checker.settled(&ty));
        let Some(wanted) = self.member_type(member, at) else { return Type::Bool };
        match &settled {
            Type::Union(members) if members.contains(&wanted) => {}
            Type::Union(_) => {
                let message = format!("`{member}` isn't one of the types in {}", crate::types::a(&settled));
                self.error(Diagnostic::new(message, at));
            }
            Type::Unknown => {}
            other => {
                let message =
                    format!("this is always {}, so `is {member}` says nothing", crate::types::a(other));
                self.error(
                    Diagnostic::new(message, value.span).with_help("`is` tells apart the types in a union"),
                );
            }
        }
        Type::Bool
    }

    /// Records that these names aren't None.
    pub(crate) fn narrow(&mut self, names: &[String]) {
        for name in names {
            self.changed_at.remove(name);
            let Some(binding) = self.binding_mut(name) else { continue };
            let (ty, declared) = (binding.ty.clone(), binding.declared.clone());
            let narrowed = match self.checker.settled(&ty) {
                // Known to be None here, so a path where it isn't can't be reached; it
                // holds whatever else it could.
                Type::None => self.checker.settled(&declared).without_none(),
                ty if ty.maybe_none() => ty.without_none(),
                _ => continue,
            };
            if let Some(binding) = self.binding_mut(name) {
                binding.ty = narrowed;
            }
        }
    }
}

/// The refusal of a name that is or holds a generator that can fail, read where its failure can't
/// pass up: from around, or as an arrow's own parameter.
fn failing_generator_reached(name: &str, ty: &Type, span: Span, param: bool) -> Diagnostic {
    let holds = ty.generator_failure().is_none();
    if param {
        crate::messages::failing_generator_given_to_arrow(name, holds, span)
    } else {
        crate::messages::failing_generator_inside(name, holds, span)
    }
}

/// Python's `name: str = None`: a `None` default for a parameter of a plain type, which can't be
/// `None`. The `?` that lets it be is a fix to make by hand, since every use of it changes.
/// A generator function's `ref` parameter, which would outlive the call that gave it.
fn ref_in_a_generator(param: &fennec_syntax::Param) -> Diagnostic {
    let name = &param.name;
    Diagnostic::new(
        format!(
            "a generator can't take `ref {name}`: it runs after its call has given it back, while what called it can change `{name}` too"
        ),
        param.span,
    )
    .with_help(format!(
        "take `{name}` without `ref`, as a copy of its own, or give back a `list` of the items rather than yielding them"
    ))
}

fn none_for_plain_type(name: &str, ty: &Type, written: Span, default: Span) -> Diagnostic {
    let shown = crate::types::a(ty);
    Diagnostic::new(format!("`{name}` is {shown}, which can't be `None`"), default).with_hint(
        format!("write `{name}: {ty}? = None` to let it be left out"),
        Span { start: written.end, end: written.end },
        "?",
    )
}

/// Each name `body` gives a value with `=`, however deep in its blocks, and where. `+=` and the
/// like change a value the name already has, so they don't count.
fn assigned_in(body: &[Stmt], found: &mut Vec<(String, Span)>) {
    for stmt in body {
        if let StmtKind::Assign { target, op: None, .. } = &stmt.kind
            && let ExprKind::Name(name) = &target.kind
        {
            found.push((name.clone(), stmt.span));
        }
        for block in blocks(stmt) {
            assigned_in(block, found);
        }
    }
}
