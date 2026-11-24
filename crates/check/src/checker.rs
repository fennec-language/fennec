//! The signature table and the order functions are checked in. A function is checked the
//! first time a caller needs its signature, for the errors it can fail with, or after that in
//! name order. A function that's still being checked when it's needed again calls itself,
//! directly or through others; its written signature stands in, and the functions in such a
//! cycle are checked again until their errors stop growing.

use std::collections::{BTreeMap, BTreeSet};

use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Generic, Module, Span, Stmt, TestDef};

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

/// How much of the standard library a check covers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// All of it.
    Everything,
    /// Just the functions the program reaches.
    Reached,
    /// Just the functions the program reaches, and every type's fields.
    ReachedWithFields,
    /// What a build checks: as `ReachedWithFields`, so lowering can have its signatures, and
    /// refusing all a check of what's written refuses, a lost change included.
    Building,
    /// All of it, with the library held to a program's rules: what it would be told were it a
    /// program's own code.
    Library,
}

/// What one function's or test's check writes in the tables of what to write into the module,
/// kept by its key when keeping (`Checker::keeping`): a check of the program once it's edited can
/// carry over what was found for one the edit left as it was, rather than check it again. Field
/// defaults and constants write into the checker's own tables, which each check works out again.
macro_rules! tables {
    ($($field:ident: $ty:ty),* $(,)?) => {
        #[derive(Clone, Debug, Default, PartialEq)]
        pub(crate) struct Tables {
            $(pub(crate) $field: $ty,)*
        }

        impl Checker {
            /// The tables as they are, leaving them empty for a function's check to write into.
            fn take_tables(&mut self) -> Tables {
                Tables { $($field: std::mem::take(&mut self.$field),)* }
            }

            /// Puts back what `take_tables` took.
            fn put_tables(&mut self, tables: Tables) {
                $(self.$field = tables.$field;)*
            }

            /// Adds what one function's check wrote.
            fn add_tables(&mut self, tables: &Tables) {
                $(self.$field.extend(tables.$field.clone());)*
            }
        }

        impl HoldsSpan for Tables {
            // Every table, so one added later can't be left out of what's looked through.
            fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool {
                $(self.$field.holds_span(hit) ||)* false
            }
        }
    };
}

/// What a check found that can hold spans: a function's kept result is carried over into a check
/// after an edit only if it holds none of the text the edit changed or moved (`Checker::carrying`).
/// `hit` says whether a span overlaps that text.
pub(crate) trait HoldsSpan {
    fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool;
}

/// What holds no span.
macro_rules! no_spans {
    ($($ty:ty),* $(,)?) => {
        $(impl HoldsSpan for $ty {
            fn holds_span(&self, _hit: &dyn Fn(Span) -> bool) -> bool {
                false
            }
        })*
    };
}

no_spans!(
    String,
    bool,
    usize,
    Type,
    crate::generic_copies::Call,
    crate::union_sources::Dispatch,
    crate::routes::Route,
    (String, Type, String, String),
);

impl HoldsSpan for (u32, u32) {
    fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool {
        hit(Span { start: self.0, end: self.1 })
    }
}

impl HoldsSpan for Span {
    fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool {
        hit(*self)
    }
}

impl HoldsSpan for (u32, u32, String) {
    fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool {
        hit(Span { start: self.0, end: self.1 })
    }
}

impl HoldsSpan for (String, Expr) {
    fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool {
        self.1.holds_span(hit)
    }
}

impl HoldsSpan for Expr {
    /// Its own span, and each expression's inside it, which a value the checker made, as a zero
    /// holding a field's default, has from other text.
    fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool {
        if hit(self.span) {
            return true;
        }
        let mut probe = [Stmt { kind: fennec_syntax::StmtKind::Expr(self.clone()), span: self.span }];
        let mut found = false;
        fennec_syntax::visit::in_stmts(&mut probe, &mut |inner| found = found || hit(inner.span));
        found
    }
}

impl HoldsSpan for crate::lend_lowering::Source {
    /// The source's definition, written in where it's used, and the task it starts.
    fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool {
        hit(self.def.span) || self.task.holds_span(hit)
    }
}

impl HoldsSpan for crate::launch::Launch {
    fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool {
        self.task.holds_span(hit)
    }
}

impl<T: HoldsSpan> HoldsSpan for Option<T> {
    fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool {
        self.as_ref().is_some_and(|inner| inner.holds_span(hit))
    }
}

impl<T: HoldsSpan> HoldsSpan for Vec<T> {
    fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool {
        self.iter().any(|inner| inner.holds_span(hit))
    }
}

impl<T: HoldsSpan> HoldsSpan for BTreeSet<T> {
    fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool {
        self.iter().any(|inner| inner.holds_span(hit))
    }
}

impl<K: HoldsSpan, V: HoldsSpan> HoldsSpan for BTreeMap<K, V> {
    fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool {
        self.iter().any(|(key, value)| key.holds_span(hit) || value.holds_span(hit))
    }
}

impl HoldsSpan for Graph {
    // Named in full, so a field added later can't be left out of what's looked through.
    fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool {
        let Graph { own_callers, waits, takes_locks, lock_calls, values, handovers, unfollowed, reached } =
            self;
        own_callers.holds_span(hit)
            || waits.holds_span(hit)
            || takes_locks.holds_span(hit)
            || lock_calls.holds_span(hit)
            || values.holds_span(hit)
            || handovers.holds_span(hit)
            || unfollowed.holds_span(hit)
            || reached.holds_span(hit)
    }
}

tables! {
    cases: BTreeMap<(u32, u32), String>,
    float_sums: BTreeSet<(u32, u32)>,
    sized_sums: BTreeMap<(u32, u32), bool>,
    defaults: BTreeMap<(u32, u32), Vec<(String, Expr)>>,
    generator_types: BTreeMap<(u32, u32), String>,
    not_equals: BTreeSet<(u32, u32)>,
    method_calls: BTreeSet<(u32, u32)>,
    array_zeros: BTreeMap<(u32, u32), Expr>,
    optional_steps: BTreeSet<(u32, u32)>,
    std_names: BTreeMap<(u32, u32), String>,
    names_outer: BTreeSet<String>,
    std_calls: BTreeMap<(u32, u32), String>,
    field_args: BTreeMap<(u32, u32), String>,
    named_patterns: BTreeMap<(u32, u32, String), Vec<String>>,
    shared_values: BTreeMap<(u32, u32), Type>,
    sources: BTreeMap<(u32, u32), crate::lend_lowering::Source>,
    widened_values: BTreeMap<(u32, u32), usize>,
    generic_sources: BTreeSet<String>,
    generic_calls: BTreeMap<(u32, u32), crate::generic_copies::Call>,
    generic_method_calls: BTreeMap<(u32, u32), crate::generic_copies::Call>,
    format_params: BTreeSet<(String, Type, String, String)>,
    union_sources: BTreeMap<(u32, u32), crate::union_sources::Dispatch>,
    opened_apart: BTreeSet<(u32, u32)>,
    launches: BTreeMap<(u32, u32), crate::launch::Launch>,
    routes: BTreeMap<(u32, u32), crate::routes::Route>,
    ref_matches: BTreeMap<(u32, u32), Vec<Option<Vec<String>>>>,
    ref_items: BTreeMap<(u32, u32), bool>,
    native_formats: BTreeMap<(u32, u32), String>,
    native_json_bodies: BTreeSet<(u32, u32)>,
    saved_types: BTreeSet<Type>,
}

/// What one function's check adds to what's followed across functions: whom it calls, what it
/// waits on and locks, what it calls inside a lock, the function values it passes, what it hands
/// from kit to kit, and whether it calls a function value it can't follow. It's added as soon as the check is done, since revisiting a cycle reads
/// whom each function calls, and kept by function for a later check to carry over.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Graph {
    own_callers: BTreeMap<String, BTreeSet<String>>,
    waits: BTreeMap<String, String>,
    takes_locks: BTreeMap<String, String>,
    lock_calls: Vec<crate::waits::LockCall>,
    values: crate::waits::Values,
    handovers: crate::holds::Handovers,
    /// The functions that call a function value they can't follow.
    unfollowed: BTreeSet<String>,
    /// For a test, the functions it called itself: a test notes no callers, so this is how what
    /// it reaches is reached again when it's carried over.
    reached: BTreeSet<String>,
}

impl Checker {
    /// What's followed as it is, leaving it empty for a function's check to add to.
    fn take_graph(&mut self) -> Graph {
        Graph {
            own_callers: std::mem::take(&mut self.own_callers),
            waits: std::mem::take(&mut self.waits),
            takes_locks: std::mem::take(&mut self.takes_locks),
            lock_calls: std::mem::take(&mut self.lock_calls),
            values: std::mem::take(&mut self.values),
            handovers: std::mem::take(&mut self.handovers),
            unfollowed: std::mem::take(&mut self.unfollowed),
            reached: BTreeSet::new(),
        }
    }

    /// Puts back what `take_graph` took.
    fn put_graph(&mut self, graph: Graph) {
        // A test's `reached` is kept by the test, not put back.
        let Graph { own_callers, waits, takes_locks, lock_calls, values, handovers, unfollowed, reached: _ } =
            graph;
        self.own_callers = own_callers;
        self.waits = waits;
        self.takes_locks = takes_locks;
        self.lock_calls = lock_calls;
        self.values = values;
        self.handovers = handovers;
        self.unfollowed = unfollowed;
    }

    /// Adds what one function's check found.
    fn add_graph(&mut self, graph: &Graph) {
        let mut all = self.take_graph();
        all.add(graph);
        self.put_graph(all);
    }

    /// Keeps apart, by `key`, what a function's or test's check just wrote in the tables and added
    /// to what's followed, and puts back what was there before it, `around`. A test's `reached`
    /// is the functions it called itself.
    fn keep_apart(&mut self, key: &str, around: (Tables, Graph), reached: BTreeSet<String>) {
        let (tables, followed) = around;
        let own = self.take_tables();
        self.put_tables(tables);
        if self.tables_merged {
            self.add_tables(&own);
        }
        self.tables_of.insert(key.to_owned(), own);
        let found = Graph { reached, ..self.take_graph() };
        self.put_graph(followed);
        self.found_graph(&found);
        self.graph_of.insert(key.to_owned(), found);
    }

    /// What a function's check found, `graph`, goes into what's followed once the outermost
    /// check it's inside is done: one checked inside another's, as a callee is when it's first
    /// called, isn't part of that other's own.
    fn found_graph(&mut self, graph: &Graph) {
        if self.checking.is_empty() {
            self.add_graph(graph);
            let inside = std::mem::take(&mut self.inner_graph);
            self.add_graph(&inside);
        } else {
            self.inner_graph.add(graph);
        }
    }
}

impl Graph {
    /// Adds what another function's check found.
    fn add(&mut self, graph: &Graph) {
        // Named in full, so a field added to `Graph` can't be left out of what's carried over.
        // A test's `reached` is its own, not added.
        let Graph { own_callers, waits, takes_locks, lock_calls, values, handovers, unfollowed, reached: _ } =
            graph;
        for (callee, callers) in own_callers {
            self.own_callers.entry(callee.clone()).or_default().extend(callers.iter().cloned());
        }
        self.waits.extend(waits.clone());
        self.takes_locks.extend(takes_locks.clone());
        self.lock_calls.extend(lock_calls.iter().cloned());
        self.values.add(values);
        self.handovers.add(handovers);
        self.unfollowed.extend(unfollowed.iter().cloned());
    }
}

impl Tables {
    /// Every type in what's written, for whether one is still open.
    fn types(&self) -> Vec<&Type> {
        let mut all: Vec<&Type> = self.sources.values().map(|source| &source.gives).collect();
        all.extend(
            self.generic_calls
                .values()
                .chain(self.generic_method_calls.values())
                .flat_map(|call| &call.types),
        );
        all.extend(self.launches.values().flat_map(crate::launch::Launch::types));
        all.extend(self.routes.values().flat_map(|route| route.takes.iter().map(|(_, ty)| ty)));
        all.extend(self.shared_values.values());
        all.extend(self.format_params.iter().map(|(_, ty, _, _)| ty));
        all.extend(&self.saved_types);
        all
    }
}

/// What the check before found for one function, to carry over.
struct Carried {
    /// For a test, what it fails with, by its span.
    test_error: Option<((u32, u32), Type)>,
    signature: Signature,
    body: Option<Body>,
    tables: Option<Tables>,
    graph: Option<Graph>,
    locals: Vec<(String, Type)>,
    /// For a function written inside another, its entry, which goes in when that one's carried.
    entry: Option<FnEntry>,
    /// The functions written inside it, carried with it.
    nested: Vec<String>,
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
    /// The sums of sized numbers with no start written, and whether theirs is a float's zero.
    pub(crate) sized_sums: BTreeMap<(u32, u32), bool>,
    /// The defaults each call leaves out.
    pub(crate) defaults: BTreeMap<(u32, u32), Vec<(String, Expr)>>,
    /// The type of each generator expression, which printing one shows.
    pub(crate) generator_types: BTreeMap<(u32, u32), String>,
    /// What a nested function's check couldn't tell about a generator that can fail, its types
    /// still open: handed up to the function around it, which looks again once its own are worked
    /// out (`FnCx::check_pending_generators`).
    pub(crate) generators_handed_up: Vec<(String, Type, Span, bool)>,
    /// The `!=`s on a type with its own `def ==`, which become `not (a == b)`.
    pub(crate) not_equals: BTreeSet<(u32, u32)>,
    /// The calls `len(x)` and `abs(x)`, which become `x.len()` and `x.abs()`.
    pub(crate) method_calls: BTreeSet<(u32, u32)>,
    /// Each `array[T, N]()` and the zero it's filled with, written in as `array.new(zero)`.
    pub(crate) array_zeros: BTreeMap<(u32, u32), Expr>,
    /// The fields and method calls on a value that might be missing, in a chain an `else`
    /// ends, which are written in as optional.
    pub(crate) optional_steps: BTreeSet<(u32, u32)>,
    /// What each name for a standard library function written in Fennec means.
    pub(crate) std_names: BTreeMap<(u32, u32), String>,
    /// The functions that name a `with` source's own function, constant or module where it's
    /// written in, with something in sight there named the same (`sight::outer`).
    pub(crate) names_outer: BTreeSet<String>,
    /// The calls sent to another function, by the call's span: a standard library function written
    /// in Fennec, like `crypto.blake3(chunks)` of many chunks, or a builtin's, as `i64(x)` is `int(x)`.
    pub(crate) std_calls: BTreeMap<(u32, u32), String>,
    /// The field each value given by position goes to, by the value's span, where that isn't
    /// the field at its position: from outside a type, the `_` fields are skipped.
    pub(crate) field_args: BTreeMap<(u32, u32), String>,
    /// The fields, in order, of each pattern written with them by name, by its case's span and
    /// the variant's name.
    pub(crate) named_patterns: BTreeMap<(u32, u32, String), Vec<String>>,
    /// What each `shared name = value` holds, by the value's span.
    pub(crate) shared_values: BTreeMap<(u32, u32), Type>,
    /// The source each call that lends is to, by the call's span.
    pub(crate) sources: BTreeMap<(u32, u32), crate::lend_lowering::Source>,
    /// Each name of a function passed where one failing more ways goes, by its span, with how many
    /// parameters it takes: it's written in as an arrow calling it, which fails all those ways.
    pub(crate) widened_values: BTreeMap<(u32, u32), usize>,
    /// Each generic function that calls a `with` source an interface gives its type parameter,
    /// which is copied for each type it's called with, where that type's own source is written in.
    pub(crate) generic_sources: BTreeSet<String>,
    /// Each call to a generic function, by its span, with the types its type parameters are there.
    pub(crate) generic_calls: BTreeMap<(u32, u32), crate::generic_copies::Call>,
    /// Each call to a method with type parameters, its type's or its own, by its span, with the
    /// types they are there: what `copies::growing` follows beside `generic_calls`.
    pub(crate) generic_method_calls: BTreeMap<(u32, u32), crate::generic_copies::Call>,
    /// What each generic function writes or reads with a format, by its type parameters: the
    /// function, the type, the format and the interface it's written or read through. Each call's
    /// own type is checked once every function is.
    pub(crate) format_params: BTreeSet<(String, Type, String, String)>,
    /// Each `with` source called on a union, by the call's span, written in for each member.
    pub(crate) union_sources: BTreeMap<(u32, u32), crate::union_sources::Dispatch>,
    /// The `with`s that open several handles, by their values' span, which lowering nests one
    /// inside another, innermost last.
    pub(crate) opened_apart: BTreeSet<(u32, u32)>,
    /// Each `with launch(...)`, by the call's span, which lowering writes in as a source.
    pub(crate) launches: BTreeMap<(u32, u32), crate::launch::Launch>,
    /// What each test or bench can fail with, worked out from its body, by its span: two may share
    /// a name (#2040).
    pub(crate) test_errors: BTreeMap<(u32, u32), Type>,
    /// Each route written with a method, as `router.get(...)`, by the call's span, which lowering
    /// writes in as a call to `handle`.
    pub(crate) routes: BTreeMap<(u32, u32), crate::routes::Route>,
    /// Each `match` with a `ref` case, by its span: for each of its cases, the fields of the case
    /// it matches when it has a `ref`, which lowering puts back where the case ends.
    pub(crate) ref_matches: BTreeMap<(u32, u32), Vec<Option<Vec<String>>>>,
    /// Each `for ref` whose body reads what it loops over, written in to go by position.
    pub(crate) live_refs: BTreeMap<(u32, u32), crate::ref_lowering::Through>,
    /// Each `for key, ref value in x.items()`, by its span: whether `x` is a `Table`, which
    /// lowering writes in as a loop over its ids, or a dict, over its keys.
    pub(crate) ref_items: BTreeMap<(u32, u32), bool>,
    /// The calls to `json` and `toml`'s `dumps` and `loads` whose type has its own `def serialize`
    /// or `def deserialize`, each with the function in std that does the same in Fennec.
    pub(crate) native_formats: BTreeMap<(u32, u32), String>,
    /// The values given as `json=` to an HTTP call whose type has its own `def serialize`, which
    /// native code writes through `json.native_dumps` before sending.
    pub(crate) native_json_bodies: BTreeSet<(u32, u32)>,
    /// Every type asked whether it's `Serializable` or `Deserializable`: where the tuples a
    /// program saves are found (see `tuple_lowering`).
    pub(crate) saved_types: BTreeSet<Type>,
    /// Each constant's type, once worked out. `None` while its value is being checked.
    pub(crate) const_types: BTreeMap<String, Option<Type>>,
    /// The functions being checked, innermost last.
    pub(crate) checking: Vec<String>,
    /// Functions that used a signature that may still grow: one being checked, so its
    /// errors aren't all known yet, or one waiting here itself. They're checked again.
    revisit: BTreeSet<String>,
    /// What each function to check again gave back last time, for calls to it while it's
    /// being checked again.
    previous: BTreeMap<String, Signature>,
    /// The functions that call each function, or use its signature, by their own keys, a nested
    /// function's its own. What waits and locks is followed up these, so a nested function counts
    /// for what it does, not for what the function around it does; and the functions that used a
    /// signature are checked again when it changes, each with the function it's written in (see
    /// `outer_callers`). One map serves both: kept twice, as one by each key and one by the
    /// outermost, each call noted made the check of a program's functions cost 1.3% more.
    pub(crate) own_callers: BTreeMap<String, BTreeSet<String>>,
    /// What each function waits on itself: a network call, a sleep, a channel.
    pub(crate) waits: BTreeMap<String, String>,
    /// The functions that call a function value they can't follow, outside a lock: each may wait
    /// on whatever it does, which counts only where nothing it reaches is known to wait.
    pub(crate) unfollowed: BTreeSet<String>,
    /// Set when what's checked is a program as lowering rewrote it, checked again: the rules
    /// about how a program is written were held to it as written, the first time.
    pub(crate) again: bool,
    /// What the check covers: for `Scope::Library`, the library's own code is reported as a
    /// program's is.
    scope: Scope,
    /// Each function that takes a lock itself, with how the first one's written.
    pub(crate) takes_locks: BTreeMap<String, String>,
    /// The calls made inside lock bodies, judged once every function's waits are known.
    pub(crate) lock_calls: Vec<crate::waits::LockCall>,
    /// What each function waits on or locks, itself or however many calls down, once every
    /// function is checked: what a `lock` that calls it is judged by.
    pub(crate) lock_verdicts: BTreeMap<String, String>,
    /// What each function's check wrote in the tables, by its key, which a later check can carry
    /// over for a function an edit left as it was.
    tables_of: BTreeMap<String, Tables>,
    /// Whether every function checked so far has had what it wrote added to the tables: once
    /// they're read, what a function checked after that writes goes in as it's checked.
    tables_merged: bool,
    /// What each function's check added to what's followed across functions, by its key.
    graph_of: BTreeMap<String, Graph>,
    /// The functions carried over from the check before and not checked since: one is checked
    /// again only when a function it calls gives back or fails with something else now, and is
    /// then no longer here.
    carried: BTreeSet<String>,
    /// What the check before found for each function the edit left as it was, carried over
    /// when it's first asked for, as it would be checked then: a library function only once
    /// something reaches it.
    carriable: BTreeMap<String, Carried>,
    /// What checks inside another function's found, for what's followed once that one is done.
    inner_graph: Graph,
    /// While a test is checked, the functions it calls itself (`Graph::reached`).
    test_reaching: Option<BTreeSet<String>>,
    /// Each function's signature as the check before had it, but those carried over.
    before: BTreeMap<String, Signature>,
    /// Each function's type parameters as the check before had them, with their bounds, which a
    /// call reads beside the signature: its own, those around it and its extension's.
    before_bounds: BTreeMap<String, Bounds>,
    /// The types of the constants and the fields the check before worked out, which whichever
    /// function reads one first works out: a check that carries over and finds any of them
    /// different is done again whole (`types_moved`).
    before_types: Option<(WorkedOut, WorkedOut)>,
    /// Whether what each function's check finds is kept apart, by its key, for a later check to
    /// carry over (`carrying`). A build that won't check again keeps nothing, and checks as it
    /// would with none of this.
    pub(crate) keeping: bool,
    /// What's followed through function values: which function parameters are called, what's
    /// passed for them, and what each arrow function waits on.
    pub(crate) values: crate::waits::Values,
    /// What the program's code hands from kit to kit, which is held to what `kit.lock` says each
    /// kit holds.
    pub(crate) handovers: crate::holds::Handovers,
    /// What `kit.lock` says each kit holds.
    pub(crate) holds: fennec_syntax::Holds,
    /// What each kit is handed, by kit and capability, where it's first handed over, when the
    /// module asks only for that to be worked out.
    pub(crate) worked_out: BTreeMap<String, BTreeMap<String, Span>>,
    /// Which types can cross to another task, once every function is checked.
    pub(crate) crossing: crate::Crossing,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

/// How many times functions in a cycle of calls are checked again before the errors they
/// can fail with stop growing.
const REVISITS: usize = 16;

/// The program's declarations, with the standard library's modules added first.
#[cfg(test)]
fn with_library<'m>(library: impl IntoIterator<Item = &'m Module>, module: &Module) -> Program {
    let mut program = library_alone(library, &crate::program::Copies);
    program.add(module, false);
    program
}

/// The standard library's declarations, which a program's are added to, each held as `hold` says.
fn library_alone<'m>(
    library: impl IntoIterator<Item = &'m Module>,
    hold: &impl crate::program::Holds<'m>,
) -> Program {
    let mut program = Program::default();
    for prelude in library {
        program.add_with(prelude, true, hold);
        program.problems.extend(crate::privacy::top_level_underscores(prelude));
    }
    // What a standard library module imports is its own, kept apart by where it's written: a
    // program names it only by importing it too.
    program
}

/// The standard library's declarations, worked out once and copied for each program: they're
/// the same every time. Each holds its definition where std's parse is kept for the whole
/// process, so none is copied, here or for a program.
fn library() -> &'static Program {
    static LIBRARY: std::sync::OnceLock<Program> = std::sync::OnceLock::new();
    LIBRARY.get_or_init(|| {
        library_alone(fennec_syntax::prelude_modules().unwrap_or_default(), &crate::program::Library)
    })
}

impl Checker {
    pub(crate) fn new(module: &Module) -> Self {
        let mut checker = Checker::with_imports_unchecked(module);
        checker.diagnostics.extend(crate::imports::unimported(module, &checker.program));
        checker
    }

    /// `new`, keeping what each function's check finds apart, for a later check to carry over.
    pub(crate) fn keeping(module: &Module) -> Self {
        let mut checker = Checker::new(module);
        checker.keeping = true;
        checker
    }

    /// `new`, but not yet looking for library types used without their import.
    fn with_imports_unchecked(module: &Module) -> Self {
        let mut program = library().clone();
        program.add(module, false);
        // The library's functions as this program runs them, rewritten as its own code is.
        for (key, def) in &module.std_rewritten {
            if let Some(entry) = program.functions.get_mut(key) {
                entry.def = crate::program::Held::Own(std::sync::Arc::new(def.clone()));
            }
        }
        let mut diagnostics = std::mem::take(&mut program.problems);
        diagnostics.extend(crate::privacy::top_level_underscores(module));
        Checker {
            program,
            states: BTreeMap::new(),
            fields: BTreeMap::new(),
            locals: BTreeMap::new(),
            vars: Vec::new(),
            typed: BTreeMap::new(),
            cases: BTreeMap::new(),
            float_sums: BTreeSet::new(),
            sized_sums: BTreeMap::new(),
            defaults: BTreeMap::new(),
            generator_types: BTreeMap::new(),
            generators_handed_up: Vec::new(),
            not_equals: BTreeSet::new(),
            method_calls: BTreeSet::new(),
            array_zeros: BTreeMap::new(),
            optional_steps: BTreeSet::new(),
            std_names: BTreeMap::new(),
            names_outer: BTreeSet::new(),
            std_calls: BTreeMap::new(),
            field_args: BTreeMap::new(),
            named_patterns: BTreeMap::new(),
            shared_values: BTreeMap::new(),
            sources: BTreeMap::new(),
            widened_values: BTreeMap::new(),
            generic_sources: BTreeSet::new(),
            generic_calls: BTreeMap::new(),
            generic_method_calls: BTreeMap::new(),
            format_params: BTreeSet::new(),
            union_sources: BTreeMap::new(),
            opened_apart: BTreeSet::new(),
            launches: BTreeMap::new(),
            test_errors: BTreeMap::new(),
            routes: BTreeMap::new(),
            ref_matches: BTreeMap::new(),
            live_refs: BTreeMap::new(),
            ref_items: BTreeMap::new(),
            native_formats: BTreeMap::new(),
            native_json_bodies: BTreeSet::new(),
            saved_types: BTreeSet::new(),
            const_types: BTreeMap::new(),
            checking: Vec::new(),
            revisit: BTreeSet::new(),
            previous: BTreeMap::new(),
            own_callers: BTreeMap::new(),
            waits: BTreeMap::new(),
            unfollowed: BTreeSet::new(),
            again: false,
            scope: Scope::Everything,
            takes_locks: BTreeMap::new(),
            lock_calls: Vec::new(),
            lock_verdicts: BTreeMap::new(),
            tables_of: BTreeMap::new(),
            tables_merged: false,
            graph_of: BTreeMap::new(),
            carried: BTreeSet::new(),
            carriable: BTreeMap::new(),
            inner_graph: Graph::default(),
            test_reaching: None,
            before: BTreeMap::new(),
            before_bounds: BTreeMap::new(),
            before_types: None,
            keeping: false,
            values: crate::waits::Values::default(),
            handovers: crate::holds::Handovers::default(),
            holds: module.holds.clone(),
            worked_out: BTreeMap::new(),
            crossing: crate::Crossing::default(),
            diagnostics,
        }
    }

    /// How many functions and tests can be carried over from the check before, those written
    /// inside another counted with it.
    pub(crate) fn carriable(&self) -> usize {
        self.carriable.len()
    }

    /// A check of `module`, a program as an edit left it, carrying over what the first check of
    /// the program before the edit, `previous`, found for each function the edit left as it was,
    /// with the functions written inside it: what it gives back and fails with, its typed body and
    /// locals, what it wrote in the tables and what it adds to what's followed across functions.
    /// The edit changed nothing the program declares besides its functions and tests
    /// (`check_to_build_keeping`), though a declaration may have moved. `dirty` is the text, in
    /// `previous`'s spans, that the edit changed or moved: one whose kept result holds a span there,
    /// as a default written in from a declaration after the edit, is checked again. So is one that
    /// calls a function that's gone, and so are the callers of each function checked again that gives
    /// back or fails with something else now (`run`). It keeps what it finds apart too, for a check
    /// after it.
    pub(crate) fn carrying(module: &Module, mut previous: Checked, dirty: &[Span]) -> Checker {
        let mut next = Checker::with_imports_unchecked(module);
        next.keeping = true;
        next.before_bounds =
            previous.defs.iter().map(|(key, entry)| (key.clone(), bounds_of(entry))).collect();
        next.before_types =
            Some((std::mem::take(&mut previous.const_types), std::mem::take(&mut previous.field_types)));
        // An edit that adds, takes away or renames a function, but one written inside another,
        // can change what a name means in one left as it was: everything's checked again.
        let outer = |functions: &BTreeMap<String, FnEntry>| -> Vec<String> {
            functions.iter().filter(|(_, entry)| !entry.nested).map(|(key, _)| key.clone()).collect()
        };
        if outer(&previous.defs) != outer(&next.program.functions) {
            next.diagnostics.extend(crate::imports::unimported(module, &next.program));
            return next;
        }
        let mut carried = left_as_they_were(&previous, &next.program);
        let mut nested = written_inside(&previous, &next.program, &mut carried);
        // What the check before found is taken, not copied: it's done with once this is made.
        let mut locals: BTreeMap<String, Vec<(String, Type)>> = BTreeMap::new();
        for (local, ty) in std::mem::take(&mut previous.signatures.locals) {
            // A local's name has no `/`, but a function's key can: `Path./` is a method.
            if let Some((key, _)) = local.rsplit_once('/')
                && carried.contains(key)
            {
                locals.entry(key.to_owned()).or_default().push((local, ty));
            }
        }
        // One whose check left a type variable open, as a generic library method's can, is
        // checked again: an open variable means nothing to another check, which has its own.
        let open = |key: &String| {
            let signature = previous.signatures.functions.get(key);
            let body = previous.types.functions.get(key);
            let tables = previous.tables_of.get(key);
            signature.is_some_and(|signature| {
                signature.returns.has_var() || signature.params.iter().any(|param| param.ty.has_var())
            }) || body.is_some_and(|body| body.exprs.values().chain(body.names.values()).any(Type::has_var))
                || tables.is_some_and(|tables| tables.types().into_iter().any(Type::has_var))
                || locals.get(key).is_some_and(|locals| locals.iter().any(|(_, ty)| ty.has_var()))
        };
        // One whose kept result holds a span of text the edit changed or moved: what it found there
        // is no longer what's there.
        let in_dirty = |at: Span| dirty.iter().any(|edited| overlaps(at, *edited));
        let stale = |key: &String| !dirty.is_empty() && previous.holds_span(key, &in_dirty);
        let unusable: Vec<String> = carried.iter().filter(|key| open(key) || stale(key)).cloned().collect();
        // A function and those written inside it are checked again together, whichever of them
        // can't be carried over: from the outermost.
        let outermost: BTreeSet<String> =
            unusable.iter().map(|key| outermost_carried(key, &nested)).collect();
        carried.retain(|key| {
            !outermost.iter().any(|outer| key == outer || key.starts_with(&format!("{outer}.")))
        });
        for outer in &outermost {
            nested.remove(outer);
        }
        // One carried over used no type without its import when the check before looked.
        let passed: BTreeSet<(&str, u32, u32)> = carried
            .iter()
            .filter_map(|key| previous.defs.get(key))
            .map(|entry| (entry.def.name.as_str(), entry.def.span.start, entry.def.span.end))
            .collect();
        let unimported = crate::imports::unimported_but(module, &next.program, &|def| {
            passed.contains(&(def.name.as_str(), def.span.start, def.span.end))
        });
        next.diagnostics.extend(unimported);
        for key in carried {
            let Some(signature) = previous.signatures.functions.remove(&key) else { continue };
            let entry = previous.defs.remove(&key);
            let test_error = previous
                .tests
                .get(&key)
                .and_then(|test| previous.test_errors.remove_entry(&(test.span.start, test.span.end)));
            let found = Carried {
                test_error,
                signature,
                body: previous.types.functions.remove(&key),
                tables: previous.tables_of.remove(&key),
                graph: previous.graph_of.remove(&key),
                locals: locals.remove(&key).unwrap_or_default(),
                entry: entry.filter(|_| !next.program.functions.contains_key(&key)),
                nested: nested.remove(&key).unwrap_or_default(),
            };
            next.carriable.insert(key, found);
        }
        // Each function's signature as the check before had it, for those checked again: what
        // their callers carried over are checked against (`recheck_callers_of_changed`).
        next.before = std::mem::take(&mut previous.signatures.functions);
        next
    }

    /// Carries over what the check before found for `key`, and for the functions written inside
    /// it, if the edit left it as it was: whether it did.
    fn carry(&mut self, key: &str) -> bool {
        let Some(found) = self.carriable.remove(key) else { return false };
        if let Some(entry) = found.entry {
            self.program.functions.insert(key.to_owned(), entry);
        }
        if let Some((at, error)) = found.test_error {
            self.test_errors.insert(at, error);
        }
        self.states.insert(key.to_owned(), State::Done(found.signature));
        if let Some(body) = found.body {
            self.typed.insert(key.to_owned(), body);
        }
        if let Some(tables) = found.tables {
            if self.tables_merged {
                self.add_tables(&tables);
            }
            self.tables_of.insert(key.to_owned(), tables);
        }
        // What its check asked for, by key, is reached as checking it would reach it.
        let mut called = Vec::new();
        if let Some(graph) = found.graph {
            called.extend(graph.own_callers.keys().cloned());
            called.extend(graph.reached.iter().cloned());
            self.found_graph(&graph);
            self.graph_of.insert(key.to_owned(), graph);
        }
        self.locals.extend(found.locals);
        self.carried.insert(key.to_owned());
        for inner in found.nested {
            self.carry(&inner);
        }
        for callee in called {
            if !self.states.contains_key(&callee) {
                self.check_function(&callee);
            }
        }
        true
    }

    /// Checks everything, or for `Scope::Reached`, the program's own code and just the
    /// standard library functions it reaches: the library's other code can't affect a run.
    /// `Scope::ReachedWithFields` also types every library type's fields, which lowering needs.
    pub(crate) fn run(self, scope: Scope) -> Result<Checked, Vec<Diagnostic>> {
        self.run_saying(scope).0
    }

    /// `run`, saying too whether a check that carried over what the one before found worked out a
    /// constant's or a field's type differently (`types_moved`): then what was carried over can't
    /// be trusted, and it's checked again whole.
    pub(crate) fn run_saying(mut self, scope: Scope) -> (Result<Checked, Vec<Diagnostic>>, bool) {
        self.start(scope);
        self.check_declarations();
        let keys: Vec<String> = self
            .program
            .functions
            .iter()
            .filter(|(_, entry)| matches!(scope, Scope::Everything | Scope::Library) || !entry.prelude)
            .map(|(key, _)| key.clone())
            .collect();
        for key in &keys {
            if !self.states.contains_key(key) {
                self.check_function(key);
            }
        }
        if matches!(scope, Scope::ReachedWithFields | Scope::Building) {
            self.check_implicit_methods();
        }
        self.recheck_callers_of_changed();
        self.revisit_cycles();
        // Every function's check is done but those reached later, from a test or a constant, so
        // what each wrote goes into the tables the rest reads.
        let kept = std::mem::take(&mut self.tables_of);
        for own in kept.values() {
            self.add_tables(own);
        }
        self.tables_of = kept;
        self.tables_merged = true;
        self.waits_under_locks();
        self.fails_unsaid();
        for name in self.program.constants.keys().cloned().collect::<Vec<_>>() {
            self.constant_type(&name);
        }
        self.check_tests();
        let types: Vec<String> = self
            .program
            .types
            .values()
            .filter(|info| !(scope == Scope::Reached && info.prelude))
            .map(|info| info.def.name.clone())
            .collect();
        for name in types {
            FnCx::check_fields(&mut self, &name);
        }
        let structs = self.structs(TypeKind::Struct);
        let errors = self.structs(TypeKind::Error);
        self.check_annotations();
        self.check_saving_pairs();
        self.check_function_annotations();
        self.check_shared_types();
        self.check_dels();
        crate::stdlib::formats::formats_at_calls(&mut self);
        self.settle_typed();
        // The library's check goes on past what's found, so each rule's findings are listed.
        if self.in_library() || self.diagnostics.is_empty() {
            self.unknowns_left();
        }
        if self.in_library() || self.diagnostics.is_empty() {
            self.body_rules(scope);
        }
        // A generic that comes back to itself with a bigger type each time can't be built.
        if self.in_library() || self.diagnostics.is_empty() {
            let growing = crate::copies::growing(&self);
            self.diagnostics.extend(growing);
        }
        // What a program as lowering rewrote it hands over is what it handed as written.
        if (self.in_library() || self.diagnostics.is_empty()) && !self.again {
            let refused = crate::holds::refused(&mut self);
            self.diagnostics.extend(refused);
        }
        if self.diagnostics.is_empty() {
            // Only when asked for: nothing a build lowers reads it yet (`crossing::with_crossing`).
            if crate::crossing::wanted() {
                self.crossing = crate::crossing::crossing(&mut self);
            }
            let moved = self.types_moved();
            return (Ok(self.checked(structs, errors)), moved);
        }
        let moved = self.types_moved();
        let mut diagnostics = self.diagnostics;
        diagnostics.sort_by(|a, b| (a.span.start, &a.message).cmp(&(b.span.start, &b.message)));
        diagnostics.dedup();
        (Err(diagnostics), moved)
    }

    /// Checks every test. When keeping, each test's check writes its own, kept by its key, as a
    /// function's does (`check_with`), and one the check before found as it is now is carried over.
    fn check_tests(&mut self) {
        // A test notes no callers, so one that calls a function that changed is found here.
        let changed = if self.carried.is_empty() { BTreeSet::new() } else { self.changed_functions() };
        for test in self.program.tests.clone() {
            if !self.keeping {
                FnCx::check_test(self, &test);
                continue;
            }
            let key = crate::program::test_key(&test.name);
            let calls_changed =
                self.carriable.get(&key).and_then(|found| found.graph.as_ref()).is_some_and(|graph| {
                    graph
                        .reached
                        .iter()
                        .chain(graph.own_callers.keys())
                        .any(|callee| changed.contains(callee))
                });
            if calls_changed {
                self.carriable.remove(&key);
            }
            if self.carry(&key) {
                continue;
            }
            let around = (self.take_tables(), self.take_graph());
            self.test_reaching = Some(BTreeSet::new());
            FnCx::check_test(self, &test);
            let reached = self.test_reaching.take().unwrap_or_default();
            self.keep_apart(&key, around, reached);
        }
    }

    /// Checks the standard library's methods that lowering calls where a program names none:
    /// printing a value calls its type's `str`, `sorted` its `<`, a `for` its `iter`, and so
    /// on. Lowering finds them by their signatures, so a type's own way would be passed over.
    fn check_implicit_methods(&mut self) {
        let implicit: Vec<String> = self
            .program
            .functions
            .iter()
            .filter(|(key, entry)| entry.prelude && entry.owner.is_some() && lowering_calls(key, entry))
            .map(|(key, _)| key.clone())
            .collect();
        for key in &implicit {
            if !self.states.contains_key(key) {
                self.check_function(key);
            }
        }
    }

    /// The types that write their own `serialize`, with whose it is: the standard library's, or
    /// one a program or a kit wrote by hand. What the checker writes for a type that writes
    /// neither method is parsed where std's code is, but it's the program's, and saves field by
    /// field, so it isn't among them.
    fn own_saving(&self) -> BTreeMap<String, crate::shapes::Writer> {
        let saving = self.program.functions.values().filter(|entry| entry.def.name == "serialize");
        saving
            .filter_map(|entry| {
                let owner = entry.owner.clone()?;
                let writer = if entry.prelude {
                    crate::shapes::Writer::Library
                } else if !entry.def.span.in_prelude() {
                    crate::shapes::Writer::Program
                } else {
                    return None;
                };
                Some((owner, writer))
            })
            .collect()
    }

    /// Marks for checking again each function carried over that calls one checked again whose
    /// signature isn't what it was: `revisit_cycles` checks those, and their callers in turn.
    fn recheck_callers_of_changed(&mut self) {
        if self.carried.is_empty() {
            return;
        }
        let changed = self.changed_functions();
        // Only what calls the changed function itself: one written inside a function this edit
        // changed is new with it, and what calls that outer function calls its signature, which is
        // looked at on its own.
        for key in changed {
            let callers: Vec<String> = self
                .own_callers
                .get(&key)
                .into_iter()
                .flatten()
                .map(|caller| self.outermost_of(caller).to_owned())
                .filter(|caller| self.carried.contains(caller))
                .collect();
            self.revisit.extend(callers);
        }
    }

    /// The functions checked again whose callers, carried over, read something of theirs that isn't
    /// what it was: what they give back and fail with, what they take (`Signature`), their type
    /// parameters' bounds, or for a `with` source, which its callers keep whole to write in where
    /// they use it, anything at all.
    fn changed_functions(&self) -> BTreeSet<String> {
        let bounds_rule_off = crate::kept_rule_off("bounds");
        self.states
            .iter()
            .filter_map(|(key, state)| match state {
                State::Done(signature) if !self.carried.contains(key) => {
                    let entry = self.program.functions.get(key);
                    let signature_changed = self.before.get(key).is_none_or(|was| was != signature);
                    let bounds_changed = !bounds_rule_off
                        && entry.is_some_and(|entry| self.before_bounds.get(key) != Some(&bounds_of(entry)));
                    let source = entry.is_some_and(|entry| {
                        entry.def.ret.as_ref().is_some_and(|ret| ret.name == crate::lend::WITH)
                    });
                    (signature_changed || bounds_changed || source).then(|| key.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// Whether a check that carried over what the one before found worked out a constant's or a
    /// field's type differently: each is worked out by whichever function reads it first, and
    /// every other that reads it was carried over with what it found then.
    /// Only those both worked out are compared: every constant and every field of the program's
    /// own types is worked out each check, and the library's can't change.
    fn types_moved(&self) -> bool {
        let differs = |before: &WorkedOut, now: &WorkedOut| {
            before.iter().any(|(key, was)| now.get(key).is_some_and(|is| is != was))
        };
        self.before_types.as_ref().is_some_and(|(constants, fields)| {
            differs(constants, &self.const_types) || differs(fields, &self.fields)
        })
    }

    /// A function's tables with every type in them settled, so a later check, which has
    /// type variables of its own, can carry them over.
    fn settled_tables(&self, mut tables: Tables) -> Tables {
        for source in tables.sources.values_mut() {
            source.gives = self.settled(&source.gives);
        }
        for call in tables.generic_calls.values_mut().chain(tables.generic_method_calls.values_mut()) {
            call.types = call.types.iter().map(|ty| self.settled(ty)).collect();
        }
        tables.launches = std::mem::take(&mut tables.launches)
            .into_iter()
            .map(|(at, launch)| (at, launch.settled(&|ty| self.settled(ty))))
            .collect();
        for route in tables.routes.values_mut() {
            for (_, ty) in &mut route.takes {
                *ty = self.settled(ty);
            }
        }
        for ty in tables.shared_values.values_mut() {
            *ty = self.settled(ty);
        }
        tables.format_params = std::mem::take(&mut tables.format_params)
            .into_iter()
            .map(|(function, ty, format, through)| (function, self.settled(&ty), format, through))
            .collect();
        tables.saved_types = tables.saved_types.iter().map(|ty| self.settled(ty)).collect();
        tables
    }

    /// What each source lends, each task launched and each generic call, now that what each is given
    /// is known, for lowering to write in.
    fn settled_writes(&mut self) -> SettledWrites {
        let mut sources = std::mem::take(&mut self.sources);
        for source in sources.values_mut() {
            source.gives = self.settled(&source.gives);
        }
        let launches = std::mem::take(&mut self.launches)
            .into_iter()
            .map(|(at, launch)| (at, launch.settled(&|ty| self.settled(ty))))
            .collect();
        let mut generic_calls = std::mem::take(&mut self.generic_calls);
        for call in generic_calls.values_mut() {
            call.types = call.types.iter().map(|ty| self.settled(ty)).collect();
        }
        (sources, launches, generic_calls)
    }

    /// What each function's check wrote in the tables and added to what's followed, settled, for a
    /// later check to carry over.
    fn settled_by_function(&mut self) -> (BTreeMap<String, Tables>, BTreeMap<String, Graph>) {
        let tables_of = std::mem::take(&mut self.tables_of)
            .into_iter()
            .map(|(key, tables)| (key, self.settled_tables(tables)))
            .collect();
        let graph_of = std::mem::take(&mut self.graph_of)
            .into_iter()
            .map(|(key, mut graph)| {
                graph.handovers = std::mem::take(&mut graph.handovers).settled(&|ty| self.settled(ty));
                (key, graph)
            })
            .collect();
        (tables_of, graph_of)
    }

    /// What a check that found nothing wrong gives back.
    fn checked(
        mut self,
        structs: BTreeMap<String, crate::Struct>,
        errors: BTreeMap<String, crate::Struct>,
    ) -> Checked {
        let (tables_of, graph_of) = self.settled_by_function();
        let (sources, launches, generic_calls) = self.settled_writes();
        let enums = self.enums();
        let locals = std::mem::take(&mut self.locals);
        // A type asked about before what it holds was known, like the list in `pack.dumps(pairs)`
        // before `pairs.append((1, "a"))`, is what it holds once the check is done.
        let saved_types = std::mem::take(&mut self.saved_types).iter().map(|ty| self.settled(ty)).collect();
        let own_saving = self.own_saving();
        let functions = self
            .states
            .into_iter()
            .filter_map(|(key, state)| match state {
                State::Done(signature) => Some((key, signature)),
                State::Checking => None,
            })
            .collect();
        let signatures = Signatures {
            functions,
            structs,
            enums,
            errors,
            locals,
            lock_verdicts: self.lock_verdicts,
            crossing: std::mem::take(&mut self.crossing),
            own_saving,
            ..Signatures::default()
        };
        let types = Types { functions: self.typed };
        Checked {
            signatures,
            types,
            test_errors: self.test_errors,
            cases: self.cases,
            float_sums: self.float_sums,
            sized_sums: self.sized_sums,
            defaults: self.defaults,
            generator_types: self.generator_types,
            not_equals: self.not_equals,
            method_calls: self.method_calls,
            array_zeros: self.array_zeros,
            optional_steps: self.optional_steps,
            std_names: self.std_names,
            std_calls: self.std_calls,
            field_args: self.field_args,
            named_patterns: self.named_patterns,
            shared_values: self.shared_values,
            sources,
            widened_values: self.widened_values,
            generic_sources: self.generic_sources,
            generic_calls,
            union_sources: self.union_sources,
            opened_apart: self.opened_apart,
            launches,
            routes: self.routes,
            ref_matches: self.ref_matches,
            live_refs: self.live_refs,
            ref_items: self.ref_items,
            native_formats: self.native_formats,
            native_json_bodies: self.native_json_bodies,
            saved_types,
            holds: self.worked_out,
            tables_of,
            graph_of,
            tests: if self.keeping {
                tests_by_key(&self.program.tests).map(|(key, test)| (key, test.clone())).collect()
            } else {
                BTreeMap::new()
            },
            defs: if self.keeping { std::mem::take(&mut self.program.functions) } else { BTreeMap::new() },
            const_types: if self.keeping { std::mem::take(&mut self.const_types) } else { BTreeMap::new() },
            field_types: if self.keeping { std::mem::take(&mut self.fields) } else { BTreeMap::new() },
        }
    }

    /// Reports types written in signatures and interfaces that don't mean anything, and
    /// type parameters bound to something that isn't an interface.
    fn check_declarations(&mut self) {
        let mut problems = Vec::new();
        for entry in self.program.functions.values().filter(|entry| self.reports(entry.prelude)) {
            let generics = self.program.generics_of(entry);
            let names: Vec<String> = generics.iter().map(|generic| generic.name.clone()).collect();
            let def = &entry.def;
            let written = def.params.iter().filter_map(|param| param.ty.as_ref()).chain(def.ret.as_ref());
            problems.extend(written.filter_map(|ty| self.program.resolve_in(ty, &names).err()));
            problems.extend(self.bad_bounds(&def.generics));
        }
        for interface in self.program.interfaces.values() {
            for def in &interface.methods {
                let written = def
                    .params
                    .iter()
                    .filter_map(|param| param.ty.as_ref())
                    .chain(def.ret.as_ref())
                    .chain(def.fails.as_ref());
                let own: Vec<String> = std::iter::once("Self".to_owned())
                    .chain(def.generics.iter().map(|generic| generic.name.clone()))
                    .collect();
                problems.extend(written.filter_map(|ty| self.program.resolve_in(ty, &own).err()));
            }
        }
        for info in self.program.types.values().filter(|info| self.reports(info.prelude)) {
            problems.extend(self.bad_bounds(&info.def.generics));
        }
        self.diagnostics.extend(problems);
        self.check_protocols();
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
    pub(crate) fn record_test(&mut self, key: String, body: &[Stmt]) {
        let effects = effects::of_body(body);
        let signature = Signature {
            params: Vec::new(),
            returns: Type::Unit,
            can_fail: effects.fails,
            generator: effects.yields,
            generics: Vec::new(),
        };
        self.states.insert(key, State::Done(signature));
    }

    /// A function's signature, checking its body first if that's the only way to know its
    /// return type.
    pub(crate) fn signature(&mut self, key: &str) -> Option<Signature> {
        self.program.functions.get(key)?;
        if let Some(caller) = self.checking.last() {
            note_caller(&mut self.own_callers, key, caller);
        } else if let Some(reached) = &mut self.test_reaching {
            reached.insert(key.to_owned());
        }
        match self.states.get_mut(key) {
            Some(State::Done(signature)) => {
                let signature = signature.clone();
                if self.revisit.contains(self.outermost_of(key)) {
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

    /// The error a function says it fails with, from `fails E`.
    fn declared_error(&self, key: &str) -> Option<Type> {
        let entry = self.program.functions.get(key)?;
        let generics = self.generic_names(entry);
        Some(self.program.resolve_in(entry.def.fails.as_ref()?, &generics).unwrap_or(Type::Unknown))
    }

    /// A function's signature once its check is done, without checking it or noting a caller.
    pub(crate) fn signature_found(&self, key: &str) -> Option<&Signature> {
        match self.states.get(key) {
            Some(State::Done(signature)) => Some(signature),
            _ => None,
        }
    }

    /// A signature from what's written, for a function still being checked.
    fn declared(&self, key: &str) -> Option<Signature> {
        let entry = self.program.functions.get(key)?;
        let returned = self.written_return(key);
        let effects = effects::of_body(&entry.def.body);
        // `-> T fails E` gives back a `T`, or fails with an `E`.
        let returns = match self.declared_error(key) {
            Some(error) if entry.def.ret.is_none() => Type::result(Type::Unit, error),
            Some(error) => Type::result(returned, error),
            None => wrap(returned, Type::Unknown, effects),
        };
        Some(Signature {
            params: self.params(entry),
            returns: failing_items(returns, effects.yields),
            can_fail: effects.fails || entry.def.fails.is_some(),
            generator: effects.yields,
            generics: entry.def.generics.iter().map(|generic| generic.name.clone()).collect(),
        })
    }

    fn generic_names(&self, entry: &FnEntry) -> Vec<String> {
        self.program.generics_of(entry).into_iter().map(|generic| generic.name).collect()
    }

    /// The rules over whole bodies, once their types are known: what a loop goes over and a call
    /// is given can't change another way, and a change has to be read.
    fn body_rules(&mut self, scope: Scope) {
        let mut problems = Vec::new();
        let keys: Vec<String> = self
            .program
            .functions
            .iter()
            .filter(|(_, entry)| self.reports(entry.prelude))
            .map(|(key, _)| key.clone())
            .collect();
        let mut settled = crate::loops::Settled::default();
        let mut live = Vec::new();
        for (key, entry) in self.program.functions.iter().filter(|(_, entry)| self.reports(entry.prelude)) {
            live.extend(crate::ref_lowering::live_loops(self, key, &entry.def));
            // What a call changes under a `ref` comes from the callee's body, however many calls
            // down, so a function carried over is held to these again.
            problems.extend(crate::loops::check_function(self, &mut settled, key, &entry.def));
            // One carried over from the check before held to the rest then, as it is now: they
            // read only its own typed body.
            if self.carried.contains(key) {
                continue;
            }
            problems.extend(crate::loops::check_arguments(&entry.def.body));
            problems.extend(crate::once::check_function(self, key, &entry.def));
            // What lowering writes reads a failure its own way; the program as written is held
            // to `?` and `else`.
            if !self.again {
                let passes = entry
                    .def
                    .fails
                    .as_ref()
                    .map_or(crate::plain::Passes::Nowhere, crate::plain::Passes::Declared);
                problems.extend(crate::plain::check_body(self, key, &entry.def.body, passes));
            }
        }
        self.live_refs.extend(live);
        for test in &self.program.tests {
            problems.extend(crate::loops::check_arguments(&test.body));
            if !self.again {
                let key = crate::program::test_key(&test.name);
                problems.extend(crate::plain::check_body(self, &key, &test.body, crate::plain::Passes::Test));
            }
        }
        // A change that's lost matters only once what's written is otherwise right. What's about
        // to be emitted has been checked as written and rewritten since: a `with` function's body
        // is written into its caller, where a change to a `ref` parameter is to a local, and a
        // failure leaves before anything reads it.
        if (self.in_library() || problems.is_empty()) && scope != Scope::ReachedWithFields && !self.again {
            for key in keys {
                let Some(def) = self.program.functions.get(&key).map(|entry| entry.def.clone()) else {
                    continue;
                };
                let counted = crate::lost::counted(self, &key, &def.body);
                problems.extend(crate::lost::check_body(self, &key, &def.body, &counted));
            }
            let tests: Vec<(String, Vec<Stmt>)> = self
                .program
                .tests
                .iter()
                .map(|test| (crate::program::test_key(&test.name), test.body.clone()))
                .collect();
            for (key, stmts) in tests {
                let counted = crate::lost::counted(self, &key, &stmts);
                problems.extend(crate::lost::check_body(self, &key, &stmts, &counted));
            }
        }
        self.diagnostics.extend(problems);
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
        if self.carry(key) {
            return;
        }
        // A function of the program's own has nothing around it to hand up to.
        let handed = self.generators_handed_up.len();
        self.check_with(key, Vec::new());
        self.generators_handed_up.truncate(handed);
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
        let entry = FnEntry {
            def: crate::program::Held::Own(std::sync::Arc::new(def.clone())),
            owner: None,
            prelude,
            inherited,
            extension: Vec::new(),
            nested: true,
        };
        self.program.functions.insert(key.to_owned(), entry);
        self.check_with(key, captured);
    }

    /// Whether a body ends in a call made for what it changes: one that takes something `mut`,
    /// or a method that changes what it's called on, like `board.add(score)`.
    fn ends_in_change(&self, key: &str, body: &[Stmt]) -> bool {
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
        // Checked now, as a function carried over is when what it calls changed: what it gives
        // back is this check's from here on, not the check before's.
        self.carried.remove(key);
        let effects = effects::of_body(&def.body);
        // When keeping, what this function's check writes is its own, kept by its key: what's in
        // the tables already is put back after.
        let around = self.keeping.then(|| (self.take_tables(), self.take_graph()));
        self.checking.push(key.to_owned());
        let prelude = prelude && !self.in_library();
        let body = FnCx::check_function(self, key, &def, &signature.params, captured, !prelude);
        self.checking.pop();
        if let Some((tables, followed)) = around {
            self.keep_apart(key, (tables, followed), BTreeSet::new());
        }
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
            let fixable = self.can_write(&value, def.span);
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
        if let Some(fails) = def.fails.as_ref().filter(|_| !prelude) {
            self.only_what_it_says(&def, fails, &body.error);
        }
        let returns = match self.declared_error(key) {
            Some(error) => Type::result(value, error),
            None => wrap(value, body.error, effects),
        };
        let returns = failing_items(returns, effects.yields);
        // Anything still open in what it gives back has been reported, and callers
        // mustn't settle it.
        signature.returns = forget_vars(&returns);
        self.states.insert(key.to_owned(), State::Done(signature));
    }

    /// Whether what's found in code of the library (`prelude`) or a program's is reported: a
    /// program's always is, and the library's only for `Scope::Library`.
    pub(crate) fn reports(&self, prelude: bool) -> bool {
        !prelude || self.in_library()
    }

    /// Whether this is `Scope::Library`'s check.
    pub(crate) fn in_library(&self) -> bool {
        self.scope == Scope::Library
    }

    /// Starts a check of `scope`.
    fn start(&mut self, scope: Scope) {
        self.scope = scope;
    }

    /// Whether what's at `span` is let off a rule that holds a program: the library's own code
    /// is, but in `Scope::Library`'s check.
    pub(crate) fn lets_off(&self, span: Span) -> bool {
        span.in_prelude() && !self.in_library()
    }

    /// Reports each function that can fail without saying so, once every function's errors are
    /// known: a signature says how a function can fail, so a reader sees it there. `main` is the
    /// exception, since nothing calls it and a failure out of it is printed, and so is a `test`.
    /// A `def` with no arrow that gives back a value has its `fails` written with its type, by
    /// `missing_return_type`.
    fn fails_unsaid(&mut self) {
        let mut found = Vec::new();
        for (key, entry) in &self.program.functions {
            let def = &entry.def;
            // A `test` block, and the function LLVM's tests make of one, have no signature.
            let test = key.starts_with("test \"") || key.starts_with("test:");
            // What the compiler writes in, like a crash's cleanup run as a function of its own, has
            // no reader to see a signature: what it can fail with is what its body says.
            let written = crate::lend_lowering::not_written_by_hand(def.span);
            if !self.reports(entry.prelude) || def.fails.is_some() || def.name == "main" || test || written {
                continue;
            }
            let Some(State::Done(signature)) = self.states.get(key) else { continue };
            let Type::Result(value, error) = &signature.returns else { continue };
            // One that says it gives back a `Result` gives back a value.
            if def.ret.as_ref().is_some_and(|ret| ret.name == "Result")
                || def.ret.is_none() && **value != Type::Unit
            {
                continue;
            }
            let said = match self.settled(error) {
                Type::Named(name) => name,
                Type::Errors(names) => names.join(" | "),
                _ => continue,
            };
            let end = def.ret.as_ref().map_or(def.span.end, |ret| ret.span.end);
            found.push(
                Diagnostic::new(
                    format!("`{}` can fail with {said}, so its signature says `fails {said}`", def.name),
                    def.span,
                )
                .with_help("write it after the return type: `fennec fix` makes this change")
                .with_fix(Span { start: end, end }, format!(" fails {said}")),
            );
        }
        self.diagnostics.extend(found);
    }

    /// Reports a `fails E` that doesn't name an error, or a body that can fail with an error
    /// other than `E`.
    fn only_what_it_says(&mut self, def: &FnDef, fails: &fennec_syntax::TypeExpr, errors: &Type) {
        let Ok(declared) = self.program.resolve(fails) else { return };
        // `fails ParseError | RangeError` was checked when it was resolved.
        if let Type::Errors(owns) = &declared {
            self.only_these(def, fails, owns, errors);
            return;
        }
        if !self.is_error_type(&declared) {
            if declared.is_known() {
                self.diagnostics.push(
                    Diagnostic::new(
                        format!("`fails` names an error, but `{}` is {}", written(fails), a(&declared)),
                        fails.span,
                    )
                    .with_help("declare the ways it can fail with `error`, like `error LoadError:`"),
                );
            }
            return;
        }
        let Some(own) = declared.type_name() else { return };
        let others: Vec<String> = match self.settled(errors) {
            Type::Named(name) if name != own => vec![name],
            Type::Errors(names) => names.into_iter().filter(|name| name != own).collect(),
            _ => Vec::new(),
        };
        if others.is_empty() {
            return;
        }
        let others: Vec<String> = others.iter().map(|name| format!("`{name}`")).collect();
        let others = others.join(" or ");
        self.diagnostics.push(
            Diagnostic::new(
                format!("`{}` says it fails with `{own}`, but it can fail with {others} too", def.name),
                fails.span,
            )
            .with_help(format!("handle {others} where it happens, or fail with a `{own}` in its place")),
        );
    }

    /// Reports a body of a function that says `fails A | B` that can fail some other way.
    fn only_these(&mut self, def: &FnDef, fails: &fennec_syntax::TypeExpr, owns: &[String], errors: &Type) {
        let others: Vec<String> = match self.settled(errors) {
            Type::Named(name) => vec![name],
            Type::Errors(names) => names,
            _ => Vec::new(),
        }
        .into_iter()
        .filter(|name| !owns.contains(name))
        .map(|name| format!("`{name}`"))
        .collect();
        if others.is_empty() {
            return;
        }
        let (others, owns) = (others.join(" or "), written(fails));
        self.diagnostics.push(
            Diagnostic::new(
                format!("`{}` says it fails with `{owns}`, but it can fail with {others} too", def.name),
                fails.span,
            )
            .with_help(format!("handle {others} where it happens, or add it after `fails`")),
        );
    }

    /// Whether a program can write a type as it is at `at`, with every type it names in scope in
    /// that file.
    fn can_write(&self, ty: &Type, at: Span) -> bool {
        let mut writable = true;
        ty.map(&mut |part| {
            let name = match part {
                Type::Named(name) | Type::Generic(name, _) => Some(name.as_str()),
                Type::Id(_) => Some("Id"),
                Type::Table(_) => Some("Table"),
                _ => None,
            };
            writable &= name.is_none_or(|name| self.program.can_name(name, at));
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
    pub(crate) fn outermost(&self, key: &str) -> String {
        self.outermost_of(key).to_owned()
    }

    /// `outermost`, as the part of `key` it is, so nothing is made: the outer function's key is
    /// always the start of a nested one's. A library module's function, `json.dumps`, is its own
    /// outermost: `json` is the module's name, not a function around it.
    pub(crate) fn outermost_of<'k>(&self, key: &'k str) -> &'k str {
        let mut key = key;
        while let Some(entry) = self.program.functions.get(key)
            && entry.nested
            && let Some((outer, _)) = key.rsplit_once('.')
        {
            key = outer;
        }
        key
    }

    /// The functions that call `outer`, or a function written inside it, or use its signature,
    /// each by the outermost key it's written under: those checked again when `outer` is. The
    /// keys of the functions written inside `outer` start with its own, so they're found together.
    fn outer_callers(&self, outer: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let from = (std::ops::Bound::Included(outer), std::ops::Bound::Unbounded);
        for (callee, callers) in self.own_callers.range::<str, _>(from) {
            if !callee.starts_with(outer) {
                break;
            }
            if self.outermost_of(callee) == outer {
                // A constant's value or a field's default is a caller too, but one worked out
                // once and kept, never checked again: only functions are.
                let functions =
                    callers.iter().filter(|caller| self.program.functions.contains_key(caller.as_str()));
                found.extend(functions.map(|caller| self.outermost_of(caller).to_owned()));
            }
        }
        found
    }

    /// Checks again the functions that used signatures that were still growing, until a
    /// round changes none of them. A call cycle's errors are then every error any function
    /// in it can fail with.
    fn revisit_cycles(&mut self) {
        let mut keys = std::mem::take(&mut self.revisit);
        let mut pending: Vec<String> = keys.iter().cloned().collect();
        while let Some(key) = pending.pop() {
            for caller in self.outer_callers(&key) {
                if keys.insert(caller.clone()) {
                    pending.push(caller);
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
    /// A field's type as far as it's known without working anything out: once its check is done,
    /// or as written. `None` for a field typed only by a default nothing has looked at.
    pub(crate) fn field_type_known(&self, owner: &str, case: Option<&str>, field: &str) -> Option<Type> {
        let key = format!("{owner}.{}.{field}", case.unwrap_or(""));
        if let Some(Some(ty)) = self.fields.get(&key) {
            return Some(ty.clone());
        }
        let info = self.program.type_info(owner)?;
        let fields = match case {
            Some(case) => info.def.cases.iter().find(|c| c.name == case).map(|c| c.fields.as_slice()),
            None => Some(info.def.fields.as_slice()),
        };
        let found = fields?.iter().find(|f| f.name == field)?;
        self.program.resolve_in(found.ty.as_ref()?, &self.program.type_params(owner)).ok()
    }

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
        let report = self.reports(info.prelude);
        let ty = match (&found.ty, &found.default) {
            (Some(ty), _) => match self.program.resolve_in(ty, &self.program.type_params(owner)) {
                Ok(ty) => ty,
                Err(problem) => {
                    self.diagnostics.push(problem);
                    Type::Unknown
                }
            },
            (None, Some(default)) => {
                // Worked out once, apart from the function that reads it first, which isn't
                // what calls what the default calls.
                self.checking.push(format!("default {key}"));
                let ty = FnCx::type_of_default(self, default, owner);
                self.checking.pop();
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
        // One carried over from the check before was settled when that check was done.
        for (_, body) in typed.iter_mut().filter(|(key, _)| !self.carried.contains(*key)) {
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
        // Looked up by key, not searched for in a list of the library's functions: that made this
        // check about a function's typed body against each of thousands of keys.
        let functions = &self.program.functions;
        let library = self.in_library();
        let prelude = |key: &str| !library && functions.get(key).is_some_and(|entry| entry.prelude);
        let mut unknown: Vec<(u32, u32)> = self
            .typed
            .iter()
            .filter(|(key, _)| !prelude(key))
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
                State::Done(signature) if !prelude(key) => Some((key, signature)),
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
        type Annotations = BTreeMap<String, Vec<fennec_syntax::Annotation>>;
        // Each struct's name, type parameters and fields' names, whether a field has a rule, and
        // what's written above each field.
        type Found = (String, Vec<String>, Vec<String>, (bool, Annotations));
        let structs: Vec<Found> = self
            .program
            .types
            .values()
            .filter(|info| info.kind == kind)
            .map(|info| {
                let names = info.def.fields.iter().map(|field| field.name.clone()).collect();
                let rules = info.def.fields.iter().any(|field| field.rule.is_some());
                let annotations: BTreeMap<String, Vec<fennec_syntax::Annotation>> = info
                    .def
                    .fields
                    .iter()
                    .filter(|field| !field.annotations.is_empty())
                    .map(|field| (field.name.clone(), field.annotations.clone()))
                    .collect();
                let generics = info.def.generics.iter().map(|generic| generic.name.clone()).collect();
                (info.def.name.clone(), generics, names, (rules, annotations))
            })
            .collect();
        structs
            .into_iter()
            .map(|(name, generics, fields, (rules, annotations))| {
                let fields = fields
                    .into_iter()
                    .map(|field| {
                        let ty = self.field_type(&name, None, &field);
                        (field, ty)
                    })
                    .collect();
                (name, crate::Struct { generics, fields, rules, annotations })
            })
            .collect()
    }

    /// Every enum's cases, with the types of their fields, and every error's that has cases.
    fn enums(&mut self) -> BTreeMap<String, crate::Enum> {
        let enums: Vec<crate::program::Held<fennec_syntax::TypeDef>> = self
            .program
            .types
            .values()
            .filter(|info| info.kind == TypeKind::Enum || !info.def.cases.is_empty())
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
            let generics = def.generics.iter().map(|generic| generic.name.clone()).collect();
            found.insert(def.name.clone(), crate::Enum { generics, cases, rules });
        }
        found
    }

    pub(crate) fn is_error_type(&self, ty: &Type) -> bool {
        self.program.kind_of(ty) == Some(TypeKind::Error)
    }
}

/// Notes that `caller` calls the function `key`. A function's signature is asked for at each call
/// of it, so the edge is nearly always noted already: then nothing is made.
fn note_caller(callers: &mut BTreeMap<String, BTreeSet<String>>, key: &str, caller: &str) {
    match callers.get_mut(key) {
        Some(noted) if noted.contains(caller) => {}
        Some(noted) => {
            noted.insert(caller.to_owned());
        }
        None => {
            callers.insert(key.to_owned(), BTreeSet::from([caller.to_owned()]));
        }
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
/// What a generator function's call gives back: a generator that fails as the function says, so
/// whatever loops over it knows what its failure is, wherever the generator has gone by then.
/// Anything else is as it was.
pub(crate) fn failing_items(returns: Type, generator: bool) -> Type {
    match returns {
        Type::Result(value, error) if generator => match *value {
            Type::Iterator(item, None) => {
                Type::Result(Box::new(Type::Iterator(item, Some(error.clone()))), error)
            }
            value => Type::Result(Box::new(value), error),
        },
        other => other,
    }
}

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
            let prefix = if param.is_mut { "ref " } else { "" };
            match &param.ty {
                Some(ty) => format!("{prefix}{}: {}", param.name, written(ty)),
                None => format!("{prefix}{}", param.name),
            }
        })
        .collect();
    let signature = format!("def {}({})", def.name, params.join(", "));
    let (what, written) = match value {
        Type::Iterator(item, _) if generator => {
            (format!("yields {}", a(item)), item.source().map(|item| format!(" -> Iterator[{item}]")))
        }
        // What can fail says so with `fails`, as `-> int fails IoError`, not as a `Result`.
        Type::Result(held, error) => {
            let failure = match error.as_ref() {
                Type::Errors(names) => Some(names.join(" | ")),
                other => other.source(),
            };
            let written = failure.and_then(|failure| match held.as_ref() {
                Type::Unit => Some(format!(" fails {failure}")),
                held => held.source().map(|held| format!(" -> {held} fails {failure}")),
            });
            (format!("gives back {}", a(value)), written)
        }
        value => (format!("gives back {}", a(value)), value.source().map(|value| format!(" -> {value}"))),
    };
    let message = format!("`{}` {what}, so its return type has to be written", def.name);
    let Some(written) = written.filter(|_| fixable) else {
        return Diagnostic::new(message, def.span).with_help(format!(
            "write the type it gives back after the parameters: `{signature} -> <type>:`"
        ));
    };
    let end = Span { start: def.span.end, end: def.span.end };
    Diagnostic::new(message, def.span)
        .with_help(format!("write it after the parameters: `{signature}{written}:`"))
        .with_fix(end, written)
}

/// Whether lowering may call a standard library method that a program never names: an
/// operator's, a protocol's like `str`, `iter` or `len`, an error's `message`, which printing it
/// calls, or any of `StrWriter`'s, which a type's own `str` is given.
fn lowering_calls(key: &str, entry: &FnEntry) -> bool {
    const PROTOCOLS: [&str; 11] = [
        "str",
        "iter",
        "len",
        "is_live",
        "hash",
        "serialize",
        "deserialize",
        "contains",
        "in",
        "own_text",
        "message",
    ];
    if entry.owner.as_deref() == Some("StrWriter") {
        return true;
    }
    let method = key.rsplit_once('.').map_or(key, |(_, method)| method);
    PROTOCOLS.contains(&method)
        || method.chars().next().is_some_and(|first| !first.is_alphanumeric() && first != '_')
}

/// The types of constants or fields, by name, as a check worked them out: `None` while one is.
pub(crate) type WorkedOut = BTreeMap<String, Option<Type>>;

/// What `Checker::settled_writes` gives: the sources, launches and generic calls, by span.
type SettledWrites = (
    BTreeMap<(u32, u32), crate::lend_lowering::Source>,
    BTreeMap<(u32, u32), crate::launch::Launch>,
    BTreeMap<(u32, u32), crate::generic_copies::Call>,
);

/// A function's type parameters, with their bounds, as a call reads them: its own, those of the
/// functions around it and its extension's.
type Bounds = (Vec<Generic>, Vec<Generic>, Vec<Generic>);

fn bounds_of(entry: &FnEntry) -> Bounds {
    (entry.def.generics.clone(), entry.inherited.clone(), entry.extension.clone())
}

/// Whether `functions` holds `entry` under `key`, as it was.
fn self_same(functions: &BTreeMap<String, FnEntry>, key: &str, entry: &FnEntry) -> bool {
    functions.get(key).is_some_and(|now| now == entry)
}

/// The functions written inside each of `carried`, by its key, which go with it, each added to
/// `carried` too. Its key sorts after the one it's in, so what's inside that goes too. One written
/// inside another is in `previous` alone: a check adds it as it checks the one it's in.
fn written_inside(
    previous: &Checked,
    program: &Program,
    carried: &mut BTreeSet<String>,
) -> BTreeMap<String, Vec<String>> {
    let mut nested: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for key in previous.defs.keys() {
        if program.functions.contains_key(key) || !previous.signatures.functions.contains_key(key) {
            continue;
        }
        if let Some((outer, _)) = key.rsplit_once('.')
            && carried.contains(outer)
        {
            nested.entry(outer.to_owned()).or_default().push(key.clone());
            carried.insert(key.clone());
        }
    }
    nested
}

/// Whether `at` overlaps `edited`, or touches it at either end: an edit there can make the token
/// ending or starting there longer. A span covering nothing does where it is.
fn overlaps(at: Span, edited: Span) -> bool {
    at.start <= edited.end && edited.start <= at.end
}

/// The outermost function `key` is written inside, by way of `nested`, which holds the functions
/// written inside each carried over: `key` itself when it's written inside none.
fn outermost_carried(key: &str, nested: &BTreeMap<String, Vec<String>>) -> String {
    let mut found = key.to_owned();
    while let Some((outer, _)) = nested.iter().find(|(_, inside)| inside.contains(&found)) {
        found.clone_from(outer);
    }
    found
}

/// The functions and tests `previous` checked that `program` has just as they were, but those that
/// call a function that's gone.
fn left_as_they_were(previous: &Checked, program: &Program) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (key, entry) in &previous.defs {
        if previous.signatures.functions.contains_key(key) && self_same(&program.functions, key, entry) {
            found.insert(key.clone());
        }
    }
    // Tests too, by their keys, the same way.
    for (key, now) in tests_by_key(&program.tests) {
        if previous.signatures.functions.contains_key(&key) && previous.tests.get(&key) == Some(now) {
            found.insert(key);
        }
    }
    // Every function it called is still there, as what was a function before.
    found.retain(|key| {
        previous.graph_of.get(key).is_none_or(|graph| {
            graph
                .own_callers
                .keys()
                .chain(&graph.reached)
                .all(|callee| !previous.defs.contains_key(callee) || program.functions.contains_key(callee))
        })
    });
    found
}

/// Each test by its key, but those whose name another shares, since their key is both's.
fn tests_by_key(tests: &[TestDef]) -> impl Iterator<Item = (String, &TestDef)> {
    let mut by_key: BTreeMap<String, Option<&TestDef>> = BTreeMap::new();
    for test in tests {
        by_key
            .entry(crate::program::test_key(&test.name))
            .and_modify(|found| *found = None)
            .or_insert(Some(test));
    }
    by_key.into_iter().filter_map(|(key, test)| Some((key, test?)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_type_a_library_module_imports_isnt_the_programs() {
        let library = fennec_syntax::prelude_modules().unwrap_or_default();
        let importer = fennec_syntax::parse("from std.time import Duration\n").unwrap();
        let program = fennec_syntax::parse("def wait(pause: Duration) -> int:\n    return 1\n").unwrap();
        let declared = super::with_library(library.iter().chain([&importer]), &program);
        let problems: Vec<String> = crate::imports::unimported(&program, &declared)
            .into_iter()
            .map(|problem| problem.message)
            .collect();
        assert_eq!(problems, ["`Duration` doesn't exist"]);
    }

    /// A library module's function, keyed under the module's name, as `intrinsics.launch` is.
    fn a_module_function(checker: &super::Checker) -> (String, super::FnEntry) {
        checker
            .program
            .functions
            .iter()
            .find(|(key, entry)| entry.owner.is_none() && !entry.nested && key.contains('.'))
            .map(|(key, entry)| (key.clone(), entry.clone()))
            .unwrap()
    }

    /// A library module's function is its own outermost, and a function written inside another
    /// is that other's.
    #[test]
    fn a_module_function_is_its_own_outermost_and_a_nested_one_is_its_outers() {
        let program = fennec_syntax::parse("def main():\n    pass\n").unwrap();
        let mut checker = super::Checker::new(&program);
        let (key, entry) = a_module_function(&checker);
        assert_eq!(checker.outermost_of(&key), key);
        let inner = super::FnEntry { nested: true, ..entry };
        checker.program.functions.insert("main.inner".to_owned(), inner);
        assert_eq!(checker.outermost_of("main.inner"), "main");
    }

    /// A library module's function that reads a signature still growing, as one calling itself
    /// does, is checked again itself, and so is what calls it: before, the module's name was
    /// marked, which is no function, so the function kept the signature it was first given while
    /// the callers of every function in its module were checked again against it.
    #[test]
    fn a_module_function_reading_a_growing_signature_is_checked_again_with_its_callers() {
        let program = fennec_syntax::parse("def main():\n    pass\n").unwrap();
        let mut checker = super::Checker::new(&program);
        let (key, _) = a_module_function(&checker);
        checker.checking.push(key.clone());
        checker.mark_stale();
        assert_eq!(checker.revisit, std::collections::BTreeSet::from([key.clone()]));
        super::note_caller(&mut checker.own_callers, &key, "main");
        assert_eq!(checker.outer_callers(&key), std::collections::BTreeSet::from(["main".to_owned()]));
    }
}
