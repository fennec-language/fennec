//! Nothing may wait inside a lock: a task that pauses while holding one stops every other task
//! that wants it, and two can stop each other for good. A lock body is checked for what it calls
//! directly, and each function for what it waits on: network calls, sleeping, channels, a
//! `parallel`. Once every function is checked, the waits are followed up the call graph across
//! the whole program, so a call inside a lock to something that waits however many calls down
//! is refused where the lock is.
//!
//! Locks are followed the same way: one lock is never taken inside another, so a call inside a
//! lock to something that takes one, however many calls down, is refused too, and two tasks can't
//! each hold a lock the other wants.

use std::collections::{BTreeMap, BTreeSet};

use fennec_syntax::{Diagnostic, Expr, ExprKind, Span};

use crate::checker::Checker;
use crate::function::FnCx;
use crate::types::Type;

/// What a function waits on that calls a function value it can't follow: one held in a list or a
/// field, say, which could be any function at all.
const UNFOLLOWED: &str = "a function value that can't be followed";

/// What to do instead, on every message about waiting inside a lock.
/// What to do instead, on every message about a lock taken inside another.
const TOGETHER: &str =
    "take them in one `lock`, like `lock a, b as ref x, ref y:`, so they can't be taken in opposite orders";

const WAIT_FIRST: &str = "do the waiting first, then take the lock: a task that waits while it holds one stops every task that wants it";

/// A call made inside a lock body, to be judged once every function is checked.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LockCall {
    /// The function called, or `*.method` for an interface's method, any type's.
    callee: String,
    /// How the call is written, for its message.
    name: String,
    span: Span,
    /// The lock held, as a message says it: "while `x` is locked".
    held: String,
}

/// What's followed through function values. A function that calls one of its function parameters
/// waits on whatever it's given, so what each call passes for that parameter is judged where it's
/// passed: an arrow function by what its body waits on, a named function by what it calls, and a
/// parameter of the caller's as that function's parameter, called the same way.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Values {
    /// Each function parameter a function calls, by the function's key and the parameter's name,
    /// with the lock held where it's called inside one.
    called: Vec<(String, String, Option<String>)>,
    /// What's passed for a function parameter, at each call.
    passed: Vec<Passed>,
    /// What each arrow function waits on itself, by span.
    waits: BTreeMap<(u32, u32), String>,
    /// The functions each arrow function calls, by span. `None` stands for a function value
    /// whose function can't be known.
    calls: BTreeMap<(u32, u32), BTreeSet<Option<String>>>,
    /// What each arrow function locks itself, by span.
    pub(crate) locks: BTreeMap<(u32, u32), String>,
    /// Each call inside a lock to an arrow function a local name holds: its span, the name, where
    /// it's called and the lock held.
    local_calls: Vec<((u32, u32), String, Span, String)>,
    /// Each function parameter a nested function calls, captured from the function it's in: the
    /// nested function's key, then the parameter's, by its function's key and its name. Calling
    /// the nested function inside a lock, or one inside the same function that calls it, calls
    /// the parameter inside it.
    through: Vec<(String, ParamKey)>,
    /// Each arrow function a nested function reads from around it, by the nested function's key
    /// and the arrow's span: the nested function does what the arrow's own body does, and calls
    /// each parameter the arrow hands on to one that calls it.
    pub(crate) captured_arrows: Vec<(String, (u32, u32))>,
}

impl Values {
    /// A parameter of `owner`'s that the nested function `nested` reads: it's called through the
    /// nested function, wherever that's called.
    pub(crate) fn note_captured_param(&mut self, nested: &str, owner: String, name: &str) {
        self.through.push((nested.to_owned(), (owner.clone(), name.to_owned())));
        self.called.push((owner, name.to_owned(), None));
    }
}

impl Values {
    /// Adds what one function's check followed.
    pub(crate) fn add(&mut self, other: &Values) {
        // Named in full, so a field added to `Values` can't be left out of what's carried over.
        let Values { called, passed, waits, calls, locks, local_calls, through, captured_arrows } = other;
        self.called.extend(called.iter().cloned());
        self.passed.extend(passed.iter().cloned());
        self.waits.extend(waits.clone());
        for (at, calls) in calls {
            self.calls.entry(*at).or_default().extend(calls.iter().cloned());
        }
        self.locks.extend(locks.clone());
        self.local_calls.extend(local_calls.iter().cloned());
        self.through.extend(through.iter().cloned());
        self.captured_arrows.extend(captured_arrows.iter().cloned());
    }
}

/// A function's parameter, by the function's key and the parameter's name.
type ParamKey = (String, String);

/// A function value passed for a function parameter.
#[derive(Clone, Debug, PartialEq)]
struct Passed {
    /// The function called, by key, and its name and the parameter's, for messages.
    callee: String,
    name: String,
    param: String,
    given: Given,
    span: Span,
    /// The lock held where the call is, as a message says it.
    held: Option<String>,
}

/// What a function value passed is.
#[derive(Clone, Debug, PartialEq)]
enum Given {
    /// An arrow function, by span, and whether it's written right there, where it was checked as
    /// running under the lock held there.
    Arrow((u32, u32), bool),
    Function(String),
    /// The caller's own parameter, by the caller's key and the parameter's name.
    Param(String, String),
    Unknown,
}

impl crate::checker::HoldsSpan for LockCall {
    fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool {
        let LockCall { callee: _, name: _, span, held: _ } = self;
        hit(*span)
    }
}

impl crate::checker::HoldsSpan for Values {
    // Named in full, so a field added later can't be left out of what's looked through.
    fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool {
        let Values { called: _, passed, waits, calls, locks, local_calls, through: _, captured_arrows } =
            self;
        passed.iter().any(|passed| {
            let Passed { callee: _, name: _, param: _, given, span, held: _ } = passed;
            hit(*span) || matches!(given, Given::Arrow(arrow, _) if arrow.holds_span(hit))
        }) || waits.holds_span(hit)
            || calls.holds_span(hit)
            || locks.holds_span(hit)
            || local_calls.iter().any(|(arrow, _, at, _)| arrow.holds_span(hit) || hit(*at))
            || captured_arrows.iter().any(|(_, arrow)| arrow.holds_span(hit))
    }
}

/// What a function value does, as a lock held while it's called sees it.
enum Verdict {
    Waits(String),
    Locks(String),
    /// It's a function value whose function can't be known, or calls one.
    Unknown,
    Fine,
}

impl Verdict {
    /// The problem with calling what's at `span` inside a lock, said after `said`.
    fn problem(self, said: &str, span: Span) -> Option<Diagnostic> {
        Some(match self {
            Verdict::Waits(what) => {
                Diagnostic::new(format!("{said}this waits on {what}"), span).with_help(WAIT_FIRST)
            }
            Verdict::Locks(what) => {
                Diagnostic::new(format!("{said}this takes {what}"), span).with_help(TOGETHER)
            }
            Verdict::Unknown => Diagnostic::new(
                format!("{said}what this holds can't be known, so it can't be shown not to wait"),
                span,
            )
            .with_help("pass an arrow function or a `def` by name, so what it does can be checked"),
            Verdict::Fine => return None,
        })
    }
}

/// What a method of a capability waits on, if it waits.
fn capability_waits(owner: &str, method: &str) -> bool {
    match owner {
        "Net" | "Connection" | "Server" | "UdpSocket" => true,
        "Clock" => method == "sleep",
        "Signals" => method == "wait",
        "Process" => method == "run",
        "Stdin" => matches!(method, "read" | "read_line" | "read_chunk" | "lines"),
        _ => false,
    }
}

/// What a standard library intrinsic waits on, said as a program knows it, if it waits.
pub(crate) fn intrinsic_waits(key: &str) -> Option<&'static str> {
    match key {
        "intrinsics.channel_send" => Some("a channel's `send`"),
        "intrinsics.channel_receive" => Some("a channel's `receive`"),
        "intrinsics.launch_end" => Some("the end of a `with launch`"),
        _ => None,
    }
}

impl FnCx<'_> {
    /// Notes that the code being checked waits on `what`: refused here inside a lock, and kept
    /// for whatever calls this function inside one.
    pub(crate) fn note_wait(&mut self, what: &str, span: Span) {
        self.not_under_lock(&format!("{what} waits"), span, WAIT_FIRST);
        let key = self.key.clone();
        self.checker.note_waits(&key, what);
        for arrow in &self.arrows_open {
            self.checker.values.waits.entry(*arrow).or_insert_with(|| what.to_owned());
        }
    }

    /// A function parameter, of a function's type or an optional one, read where the read isn't
    /// followed (see `direct_read`): kept in a list or a field, returned, unwrapped into another
    /// name, matched, or passed where what's passed isn't followed. Wherever it's called from
    /// then, it counts as called right here, with the lock held here, as a nested function's read
    /// of one does (see `captured_values`). An alias of one, `job = work`, is the parameter.
    pub(crate) fn note_function_read(&mut self, name: &str, span: Span) {
        if self.direct_read == Some((span.start, span.end)) {
            return;
        }
        let Some((owner, param)) = self.param_key(name) else { return };
        if !self.declared_type(name).is_some_and(|ty| self.takes_function(&ty)) {
            return;
        }
        // In an arrow function, as a call there is, it's something the arrow may call that can't
        // be followed.
        if !self.arrows_open.is_empty() {
            for arrow in &self.arrows_open {
                self.checker.values.calls.entry(*arrow).or_default().insert(None);
            }
            return;
        }
        if owner != self.key {
            self.checker.values.through.push((self.key.clone(), (owner.clone(), param.clone())));
        }
        self.checker.values.called.push((owner, param, self.lock.clone()));
    }

    /// A call through a function value, as `work()` with `work: () -> nothing`. A parameter is
    /// followed to what each caller passes for it; anything else can't be known, so inside a
    /// lock it's refused.
    pub(crate) fn note_value_call(&mut self, callee: &Expr, span: Span) {
        // A parameter of this function, or of one around it that a nested function captured it
        // from, by its function's key.
        let param = match &callee.kind {
            ExprKind::Name(name) => self.param_key(name),
            _ => None,
        };
        let held = self.lock.clone();
        // An arrow function a local name was declared with, when the name isn't `mut`: one that is
        // can hold another value by now than the arrow it was given.
        if let ExprKind::Name(name) = &callee.kind
            && let Some(arrow) = self.local_arrow(name)
        {
            for open in &self.arrows_open {
                self.checker.values.calls.entry(*open).or_default().insert(None);
            }
            if let Some(held) = held
                && self.report
            {
                self.checker.values.local_calls.push((arrow, name.clone(), span, held));
            }
            return;
        }
        match param {
            Some((owner, name)) if self.arrows_open.is_empty() => {
                if owner != self.key {
                    self.checker.values.through.push((self.key.clone(), (owner.clone(), name.clone())));
                }
                self.checker.values.called.push((owner, name, held));
            }
            _ => {
                for arrow in &self.arrows_open {
                    self.checker.values.calls.entry(*arrow).or_default().insert(None);
                }
                // Outside a lock, and not in an arrow function, which may never run here, this
                // function may wait on whatever the value does: called inside a lock, it's refused
                // for it, as the call would be inside one.
                if held.is_none() && self.arrows_open.is_empty() {
                    self.checker.unfollowed.insert(self.key.clone());
                }
                if let Some(held) = held
                    && self.report
                {
                    let named = match &callee.kind {
                        ExprKind::Name(name) => format!("`{name}`"),
                        _ => "this function value".to_owned(),
                    };
                    self.error(
                        Diagnostic::new(
                            format!("what {named} holds can't be known here, so it can't be shown not to wait {held}"),
                            span,
                        )
                        .with_help("call a `def` by name, or take the function as a parameter, so what each caller passes can be checked"),
                    );
                }
            }
        }
    }

    /// Whether a parameter of type `ty` is a function's, or an optional one's, which is called once
    /// it's narrowed: what's passed for either is followed to where it's called.
    pub(crate) fn takes_function(&mut self, ty: &Type) -> bool {
        match self.checker.settled(ty) {
            Type::Arrow(..) => true,
            Type::Optional(inner) => matches!(self.checker.settled(&inner), Type::Arrow(..)),
            _ => false,
        }
    }

    /// What's passed for a function parameter of the function whose arguments are being matched.
    pub(crate) fn note_passed_function(&mut self, param: &str, value: &Expr) {
        let Some(callee) = self.calling.last().cloned().filter(|callee| !callee.is_empty()) else { return };
        if !self.report {
            return;
        }
        let given = match &value.kind {
            // Nothing, for an optional one: there's no function to call.
            ExprKind::None => return,
            ExprKind::Lambda { .. } => Given::Arrow((value.span.start, value.span.end), true),
            // A parameter of this function, of one around it that a nested function captured, or one
            // a local name holds.
            ExprKind::Name(name) if let Some((owner, param)) = self.param_key(name) => {
                Given::Param(owner, param)
            }
            ExprKind::Name(name) if let Some(arrow) = self.local_arrow(name) => Given::Arrow(arrow, false),
            ExprKind::Name(name) => match self.lookup(name) {
                Some((Type::Function(key), _)) => Given::Function(key),
                None if self.names_function(name) => Given::Function(name.clone()),
                _ => Given::Unknown,
            },
            _ => Given::Unknown,
        };
        let name = callee.rsplit('.').next().unwrap_or_default().to_owned();
        self.checker.values.passed.push(Passed {
            callee,
            name,
            param: param.to_owned(),
            given,
            span: value.span,
            held: self.lock.clone(),
        });
    }

    /// Notes the wait of a capability's method that has one.
    pub(crate) fn note_capability_wait(&mut self, ty: &Type, name: &str, span: Span) {
        if let Some(owner) = ty.type_name().filter(|owner| capability_waits(owner, name)) {
            self.note_wait(&format!("`{owner}.{name}`"), span);
        }
    }

    /// Keeps a call made inside a lock body, for when every function's waits are known.
    pub(crate) fn note_lock_call(&mut self, callee: &str, name: &str, span: Span) {
        for arrow in &self.arrows_open {
            self.checker.values.calls.entry(*arrow).or_default().insert(Some(callee.to_owned()));
        }
        if let Some(held) = self.lock.clone()
            && self.report
        {
            self.checker.lock_calls.push(LockCall {
                callee: callee.to_owned(),
                name: name.to_owned(),
                span,
                held,
            });
        }
    }
}

impl Checker {
    /// Notes that the function `key` takes a lock itself: a nested function by its own key, so it's
    /// counted where it's called, not wherever the function around it is.
    pub(crate) fn note_takes_lock(&mut self, key: &str, what: &str) {
        self.takes_locks.entry(key.to_owned()).or_insert_with(|| what.to_owned());
    }

    /// Each function that does what `own` says some do, itself or through what it calls, with
    /// what the first one found does, by their own keys.
    fn up_the_calls(&self, own: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut found = own.clone();
        let mut pending: Vec<String> = found.keys().cloned().collect();
        while let Some(key) = pending.pop() {
            let what = found.get(&key).cloned().unwrap_or_default();
            // A type's method is called by every call of an interface's method of its name, on a
            // value of a type parameter, as `napper.nap(clock)`.
            let through_interface = self
                .program
                .functions
                .get(&key)
                .filter(|entry| entry.owner.is_some())
                .and_then(|_| key.rsplit_once('.'))
                .and_then(|(_, method)| self.own_callers.get(&format!("*.{method}")));
            let callers = self.own_callers.get(&key).into_iter().chain(through_interface).flatten();
            for caller in callers {
                if !found.contains_key(caller) {
                    found.insert(caller.clone(), what.clone());
                    pending.push(caller.clone());
                }
            }
        }
        found
    }

    /// What a call reaches in `found`: its function's, or for an interface's method, any type's.
    fn reached(&self, found: &BTreeMap<String, String>, callee: &str) -> Option<String> {
        match callee.strip_prefix("*.") {
            Some(method) => {
                let suffix = format!(".{method}");
                // A type's method, not a function written inside another that has its name.
                let method_of_a_type =
                    |key: &str| self.program.functions.get(key).is_some_and(|entry| entry.owner.is_some());
                found
                    .iter()
                    .find(|(key, _)| key.ends_with(&suffix) && method_of_a_type(key))
                    .map(|(_, what)| what.clone())
            }
            None => found.get(callee).cloned(),
        }
    }

    /// Reports each function value passed for a parameter that's called inside a lock, there or
    /// in what it's handed on to, and each arrow function a local name holds called inside one,
    /// when it waits, locks, or can't be known.
    fn values_under_locks(&mut self, waiting: &BTreeMap<String, String>, locking: &BTreeMap<String, String>) {
        let values = std::mem::take(&mut self.values);
        let (called, locked) = params_called(&values, &self.lock_calls, &self.own_callers);
        self.handed_on_by_value(&values, &called);
        for passed in &values.passed {
            let target = (passed.callee.clone(), passed.param.clone());
            let held = locked
                .get(&target)
                .cloned()
                .or_else(|| passed.held.clone().filter(|_| called.contains(&target)));
            let Some(held) = held else { continue };
            // An arrow function written inside a lock was checked as running there.
            if matches!(passed.given, Given::Arrow(_, true)) && passed.held.is_some() {
                continue;
            }
            let said = format!("`{}` calls `{}` {held}, and ", passed.name, passed.param);
            if let Some(problem) =
                self.verdict(&values, &passed.given, waiting, locking).problem(&said, passed.span)
            {
                self.diagnostics.push(problem);
            }
        }
        for (arrow, name, span, held) in &values.local_calls {
            let said = format!("`{name}` is called {held}, and ");
            let verdict = self.verdict(&values, &Given::Arrow(*arrow, false), waiting, locking);
            if let Some(problem) = verdict.problem(&said, *span) {
                self.diagnostics.push(problem);
            }
        }
    }

    /// A function that calls what it's given, called through a function value, would be given
    /// what can't be followed: it's called only by name.
    fn handed_on_by_value(&mut self, values: &Values, called: &BTreeSet<ParamKey>) {
        for passed in &values.passed {
            let Given::Function(key) = &passed.given else { continue };
            if !called.contains(&(passed.callee.clone(), passed.param.clone())) {
                continue;
            }
            let Some((_, param)) = called.iter().find(|(function, _)| function == key) else { continue };
            let name = key.rsplit('.').next().unwrap_or_default();
            self.diagnostics.push(
                Diagnostic::new(
                    format!("`{name}` calls the function it's given as `{param}`, so it's called only by name, where what it's given can be checked"),
                    passed.span,
                )
                .with_help(format!("pass an arrow function that calls it, like `() => {name}(...)`")),
            );
        }
    }

    /// What a function value passed does, as a lock held while it's called sees it.
    fn verdict(
        &self,
        values: &Values,
        given: &Given,
        waiting: &BTreeMap<String, String>,
        locking: &BTreeMap<String, String>,
    ) -> Verdict {
        match given {
            Given::Function(key) => match (self.reached(waiting, key), self.reached(locking, key)) {
                (Some(what), _) => Verdict::Waits(what),
                (None, Some(what)) => Verdict::Locks(what),
                (None, None) => Verdict::Fine,
            },
            Given::Arrow(at, _) => {
                if let Some(what) = values.waits.get(at) {
                    return Verdict::Waits(what.clone());
                }
                let calls = values.calls.get(at);
                for callee in calls.into_iter().flatten() {
                    match callee {
                        None => return Verdict::Unknown,
                        Some(key) => {
                            if let Some(what) = self.reached(waiting, key) {
                                return Verdict::Waits(what);
                            }
                        }
                    }
                }
                if let Some(what) = values.locks.get(at) {
                    return Verdict::Locks(what.clone());
                }
                let locks = calls.into_iter().flatten().flatten().find_map(|key| self.reached(locking, key));
                locks.map_or(Verdict::Fine, Verdict::Locks)
            }
            Given::Param(..) => Verdict::Fine,
            Given::Unknown => Verdict::Unknown,
        }
    }

    /// Has each nested function that reads an arrow function from around it do what the arrow's
    /// own body does, as if it called it: wait on what it waits on, take the lock it takes, and
    /// call what it calls. An arrow that calls what can't be known leaves the nested function
    /// counting for the whole of the function it's written in, where the arrow's body was checked.
    fn follow_captured_arrows(&mut self) {
        for (nested, arrow) in self.values.captured_arrows.clone() {
            if let Some(what) = self.values.waits.get(&arrow).cloned() {
                self.waits.entry(nested.clone()).or_insert(what);
            }
            if let Some(what) = self.values.locks.get(&arrow).cloned() {
                self.takes_locks.entry(nested.clone()).or_insert(what);
            }
            for call in self.values.calls.get(&arrow).cloned().into_iter().flatten() {
                let callees = match call {
                    // An interface's method, as `*.nap`, is each type's method of that name, as
                    // `reached` finds one inside a lock.
                    Some(callee) => match callee.strip_prefix("*.").map(str::to_owned) {
                        Some(method) => self.methods_named(&method),
                        None => vec![callee],
                    },
                    // What can't be known counts as all the function around does.
                    None => vec![
                        nested
                            .rsplit_once('.')
                            .map_or_else(|| nested.clone(), |(around, _)| around.to_owned()),
                    ],
                };
                for callee in callees {
                    self.own_callers.entry(callee).or_default().insert(nested.clone());
                }
            }
        }
    }

    /// Every type's method called `method`, by key: not a function written inside another that
    /// has its name.
    fn methods_named(&self, method: &str) -> Vec<String> {
        let suffix = format!(".{method}");
        self.program
            .functions
            .iter()
            .filter(|(key, entry)| key.ends_with(&suffix) && entry.owner.is_some())
            .map(|(key, _)| key.clone())
            .collect()
    }

    fn note_waits(&mut self, key: &str, what: &str) {
        self.waits.entry(key.to_owned()).or_insert_with(|| what.to_owned());
    }

    /// Reports each call inside a lock to something that waits, following what waits up the
    /// call graph from what waits itself.
    pub(crate) fn waits_under_locks(&mut self) {
        self.follow_captured_arrows();
        let mut waiting = self.up_the_calls(&self.waits);
        // What can't be followed, where nothing known to wait is reached: a known wait says more.
        let unfollowed = self.unfollowed.iter().map(|key| (key.clone(), UNFOLLOWED.to_owned())).collect();
        for (key, what) in self.up_the_calls(&unfollowed) {
            waiting.entry(key).or_insert(what);
        }
        let locking = self.up_the_calls(&self.takes_locks);
        self.lock_verdicts = lock_verdicts(&waiting, &locking);
        self.values_under_locks(&waiting, &locking);
        for call in std::mem::take(&mut self.lock_calls) {
            if let Some(what) = self.reached(&waiting, &call.callee) {
                let message = format!("`{}` waits on {what} {}", call.name, call.held);
                self.diagnostics.push(Diagnostic::new(message, call.span).with_help(WAIT_FIRST));
            } else if let Some(what) = self.reached(&locking, &call.callee) {
                let message = format!("`{}` takes {what} {}", call.name, call.held);
                self.diagnostics.push(Diagnostic::new(message, call.span).with_help(TOGETHER));
            }
        }
    }
}

/// What each function waits on and locks, as one line each: a function that does neither isn't
/// there.
fn lock_verdicts(
    waiting: &BTreeMap<String, String>,
    locking: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut verdicts: BTreeMap<String, String> =
        waiting.iter().map(|(key, what)| (key.clone(), format!("waits on {what}"))).collect();
    for (key, what) in locking {
        let takes = format!("takes {what}");
        verdicts
            .entry(key.clone())
            .and_modify(|verdict| *verdict = format!("{verdict}; {takes}"))
            .or_insert(takes);
    }
    verdicts
}

/// The function parameters called at all, and those called inside a lock, with the lock: a
/// parameter handed on to one is called as that one is, and one a nested function calls is called
/// inside each lock of `lock_calls` that calls the nested function, or one inside the same function
/// that calls it, by `own_callers`, and inside each lock that the nested function, passed by name
/// or called through an arrow function, is called in.
fn params_called(
    values: &Values,
    lock_calls: &[LockCall],
    own_callers: &BTreeMap<String, BTreeSet<String>>,
) -> (BTreeSet<ParamKey>, BTreeMap<ParamKey, String>) {
    let mut following = Following {
        values,
        own_callers,
        called: values.called.iter().map(|(key, param, _)| (key.clone(), param.clone())).collect(),
        locked: values
            .called
            .iter()
            .filter_map(|(key, param, held)| Some(((key.clone(), param.clone()), held.clone()?)))
            .collect(),
        through: values.through.clone(),
    };
    loop {
        let mut grew = following.handed_on_by_captured_arrows();
        grew |= following.through_lock_calls(lock_calls);
        grew |= following.passed_values();
        grew |= following.handed_on();
        if !grew {
            return (following.called, following.locked);
        }
    }
}

/// What `params_called` has found so far, grown until a round adds nothing.
struct Following<'a> {
    values: &'a Values,
    own_callers: &'a BTreeMap<String, BTreeSet<String>>,
    called: BTreeSet<ParamKey>,
    locked: BTreeMap<ParamKey, String>,
    /// `values.through`, and each parameter a captured arrow function hands on.
    through: Vec<(String, ParamKey)>,
}

impl Following<'_> {
    /// A captured arrow function that hands a parameter on, as `go = () => run(work)` read by
    /// `later`: the nested function calls the parameter through it, as if it handed it on.
    fn handed_on_by_captured_arrows(&mut self) -> bool {
        let mut grew = false;
        for (nested, at) in &self.values.captured_arrows {
            for passed in &self.values.passed {
                let Given::Param(key, param) = &passed.given else { continue };
                let entry = (nested.clone(), (key.clone(), param.clone()));
                if within_arrow(*at, passed.span) && self.is_called(passed) && !self.through.contains(&entry)
                {
                    self.through.push(entry);
                    grew = true;
                }
            }
        }
        grew
    }

    /// Each parameter a nested function calls, called inside each lock that calls the nested
    /// function, or one inside the same function that calls it.
    fn through_lock_calls(&mut self, lock_calls: &[LockCall]) -> bool {
        let mut grew = false;
        for (nested, param) in &self.through {
            if self.locked.contains_key(param) {
                continue;
            }
            let reaching = callers_inside(nested, &format!("{}.", param.0), self.own_callers);
            if let Some(call) = lock_calls.iter().find(|call| reaching.contains(&call.callee)) {
                self.locked.insert(param.clone(), call.held.clone());
                grew = true;
            }
        }
        grew
    }

    /// Each function value passed for a parameter called inside a lock, or passed inside one for a
    /// parameter called at all, and each arrow function a local name holds called inside one.
    /// - A nested function passed by name, as `run(step)`: so is each parameter it, or a nested
    ///   function it calls, calls from around it.
    /// - An arrow function: so is each function it calls, with what that calls from around it,
    ///   and each parameter it hands on to one that calls it.
    fn passed_values(&mut self) -> bool {
        let mut grew = false;
        let mut arrows: Vec<((u32, u32), String)> = Vec::new();
        for passed in &self.values.passed {
            let Some(held) = self.held_for(passed) else { continue };
            match &passed.given {
                Given::Function(function) => grew |= self.lock_captured_params(function, &held),
                Given::Arrow(at, _) => arrows.push((*at, held)),
                Given::Param(..) | Given::Unknown => {}
            }
        }
        arrows.extend(self.values.local_calls.iter().map(|(arrow, _, _, held)| (*arrow, held.clone())));
        for (at, held) in &arrows {
            for callee in self.values.calls.get(at).into_iter().flatten().flatten() {
                grew |= self.lock_captured_params(callee, held);
            }
            for passed in &self.values.passed {
                let Given::Param(key, param) = &passed.given else { continue };
                let mine = (key.clone(), param.clone());
                if within_arrow(*at, passed.span)
                    && self.is_called(passed)
                    && !self.locked.contains_key(&mine)
                {
                    self.locked.insert(mine, held.clone());
                    grew = true;
                }
            }
        }
        grew
    }

    /// A parameter handed on to one: called as that one is.
    fn handed_on(&mut self) -> bool {
        let mut grew = false;
        for passed in &self.values.passed {
            let Given::Param(key, param) = &passed.given else { continue };
            let mine = (key.clone(), param.clone());
            if self.is_called(passed) && self.called.insert(mine.clone()) {
                grew = true;
            }
            if let Some(held) = self.held_for(passed)
                && !self.locked.contains_key(&mine)
            {
                self.locked.insert(mine, held);
                grew = true;
            }
        }
        grew
    }

    /// Whether the parameter a value is passed for is called.
    fn is_called(&self, passed: &Passed) -> bool {
        self.called.contains(&(passed.callee.clone(), passed.param.clone()))
    }

    /// The lock a value passed is called inside: the one its parameter is called inside, or the
    /// one held where it's passed, for a parameter that's called at all.
    fn held_for(&self, passed: &Passed) -> Option<String> {
        let target = (passed.callee.clone(), passed.param.clone());
        self.locked
            .get(&target)
            .cloned()
            .or_else(|| passed.held.clone().filter(|_| self.called.contains(&target)))
    }

    /// See `lock_captured_params`.
    fn lock_captured_params(&mut self, function: &str, held: &str) -> bool {
        lock_captured_params(function, held, &self.through, self.own_callers, &mut self.locked)
    }
}

/// Locks each parameter that `function`, called inside the lock `held`, calls from around it:
/// itself a nested function that calls the parameter, or one that calls one, written inside the
/// parameter's own function. Whether any wasn't locked before.
fn lock_captured_params(
    function: &str,
    held: &str,
    through: &[(String, ParamKey)],
    own_callers: &BTreeMap<String, BTreeSet<String>>,
    locked: &mut BTreeMap<ParamKey, String>,
) -> bool {
    let mut grew = false;
    for (nested, param) in through {
        if !locked.contains_key(param)
            && callers_inside(nested, &format!("{}.", param.0), own_callers).contains(function)
        {
            locked.insert(param.clone(), held.to_owned());
            grew = true;
        }
    }
    grew
}

/// Whether `span` is inside the arrow function written at `arrow`.
fn within_arrow(arrow: (u32, u32), span: Span) -> bool {
    arrow.0 <= span.start && span.end <= arrow.1
}

/// `nested`, and every function that calls it, however many calls up, of those written inside the
/// function whose keys start with `inside`: the calls that reach it from within that function.
fn callers_inside(
    nested: &str,
    inside: &str,
    own_callers: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeSet<String> {
    let mut found = BTreeSet::from([nested.to_owned()]);
    let mut pending = vec![nested.to_owned()];
    while let Some(key) = pending.pop() {
        for caller in own_callers.get(&key).into_iter().flatten() {
            if caller.starts_with(inside) && found.insert(caller.clone()) {
                pending.push(caller.clone());
            }
        }
    }
    found
}
