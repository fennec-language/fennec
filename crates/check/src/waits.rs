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
use crate::function::{FnCx, Origin};
use crate::types::Type;

/// What to do instead, on every message about waiting inside a lock.
/// What to do instead, on every message about a lock taken inside another.
const TOGETHER: &str =
    "take them in one `lock`, like `lock a, b as ref x, ref y:`, so they can't be taken in opposite orders";

const WAIT_FIRST: &str = "do the waiting first, then take the lock: a task that waits while it holds one stops every task that wants it";

/// A call made inside a lock body, to be judged once every function is checked.
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
#[derive(Default)]
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
}

impl Values {
    /// What an arrow function does of what `own` holds, itself or through what it calls:
    /// `Some(None)` when it calls a function value that can't be known.
    fn arrow_reaches(
        &self,
        at: (u32, u32),
        own: &BTreeMap<(u32, u32), String>,
        _found: &BTreeMap<String, String>,
        reached: impl Fn(&str) -> Option<String>,
    ) -> Option<Option<String>> {
        if let Some(what) = own.get(&at) {
            return Some(Some(what.clone()));
        }
        for callee in self.calls.get(&at).into_iter().flatten() {
            match callee {
                None => return Some(None),
                Some(key) => {
                    if let Some(what) = reached(key) {
                        return Some(Some(what));
                    }
                }
            }
        }
        None
    }
}

/// A function value passed for a function parameter.
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
enum Given {
    Arrow((u32, u32)),
    Function(String),
    /// The caller's own parameter, by the caller's key and the parameter's name.
    Param(String, String),
    Unknown,
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

    /// A call through a function value, as `work()` with `work: () -> nothing`. A parameter is
    /// followed to what each caller passes for it; anything else can't be known, so inside a
    /// lock it's refused.
    pub(crate) fn note_value_call(&mut self, callee: &Expr, span: Span) {
        let param = match &callee.kind {
            ExprKind::Name(name) if self.origin(name) == Some(Origin::Param) => Some(name.clone()),
            _ => None,
        };
        let held = self.lock.clone();
        match param {
            Some(name) if self.arrows_open.is_empty() => {
                self.checker.values.called.push((self.key.clone(), name, held));
            }
            _ => {
                for arrow in &self.arrows_open {
                    self.checker.values.calls.entry(*arrow).or_default().insert(None);
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

    /// What's passed for a function parameter of the function whose arguments are being matched.
    pub(crate) fn note_passed_function(&mut self, param: &str, value: &Expr) {
        let Some(callee) = self.calling.last().cloned().filter(|callee| !callee.is_empty()) else { return };
        if !self.report {
            return;
        }
        let given = match &value.kind {
            ExprKind::Lambda { .. } => Given::Arrow((value.span.start, value.span.end)),
            ExprKind::Name(name) if self.origin(name) == Some(Origin::Param) => {
                Given::Param(self.key.clone(), name.clone())
            }
            ExprKind::Name(name) => match self.lookup(name) {
                Some((Type::Function(key), _)) => Given::Function(key),
                None if self.checker.program.functions.contains_key(name) => Given::Function(name.clone()),
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
    pub(crate) fn note_takes_lock(&mut self, key: &str, what: &str) {
        let outer = self.outermost(key);
        self.takes_locks.entry(outer).or_insert_with(|| what.to_owned());
    }

    /// Each function that does what `own` says some do, itself or through what it calls, with
    /// what the first one found does.
    fn up_the_calls(&self, own: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut found = own.clone();
        let mut pending: Vec<String> = found.keys().cloned().collect();
        while let Some(key) = pending.pop() {
            let what = found.get(&key).cloned().unwrap_or_default();
            for caller in self.callers.get(&key).into_iter().flatten() {
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
                found.iter().find(|(key, _)| key.ends_with(&suffix)).map(|(_, what)| what.clone())
            }
            None => found.get(&self.outermost(callee)).cloned(),
        }
    }

    /// Reports each function value passed for a parameter that's called inside a lock, there or
    /// in what it's handed on to, when what's passed waits, locks, or can't be known.
    fn values_under_locks(&mut self, waiting: &BTreeMap<String, String>, locking: &BTreeMap<String, String>) {
        let values = std::mem::take(&mut self.values);
        // The parameters called at all, and those called inside a lock, with the lock: a
        // parameter handed on to one is called as that one is.
        let mut called: BTreeSet<(String, String)> =
            values.called.iter().map(|(key, param, _)| (key.clone(), param.clone())).collect();
        let mut locked: BTreeMap<(String, String), String> = values
            .called
            .iter()
            .filter_map(|(key, param, held)| Some(((key.clone(), param.clone()), held.clone()?)))
            .collect();
        loop {
            let mut grew = false;
            for passed in &values.passed {
                let Given::Param(key, param) = &passed.given else { continue };
                let target = (passed.callee.clone(), passed.param.clone());
                let mine = (key.clone(), param.clone());
                if called.contains(&target) && called.insert(mine.clone()) {
                    grew = true;
                }
                let held = locked
                    .get(&target)
                    .cloned()
                    .or_else(|| passed.held.clone().filter(|_| called.contains(&target)));
                if let Some(held) = held
                    && !locked.contains_key(&mine)
                {
                    locked.insert(mine, held);
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        // A function that calls what it's given, called through a function value, would be given
        // what can't be followed.
        for passed in &values.passed {
            if let Given::Function(key) = &passed.given
                && called.contains(&(passed.callee.clone(), passed.param.clone()))
                && let Some((_, param)) = called.iter().find(|(function, _)| function == key)
            {
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
        for passed in &values.passed {
            let target = (passed.callee.clone(), passed.param.clone());
            let held = locked
                .get(&target)
                .cloned()
                .or_else(|| passed.held.clone().filter(|_| called.contains(&target)));
            let Some(held) = held else { continue };
            let said = format!("`{}` calls `{}` {held}, and ", passed.name, passed.param);
            let (waits, locks) = match &passed.given {
                // An arrow function written inside a lock was checked as running there.
                Given::Param(..) | Given::Arrow(_) if passed.held.is_some() => continue,
                Given::Param(..) => continue,
                Given::Function(key) => (self.reached(waiting, key).map(Some), self.reached(locking, key)),
                Given::Arrow(at) => (
                    values.arrow_reaches(*at, &values.waits, waiting, |key| self.reached(waiting, key)),
                    values.locks.get(at).cloned().or_else(|| {
                        values
                            .arrow_reaches(*at, &values.locks, locking, |key| self.reached(locking, key))
                            .flatten()
                    }),
                ),
                Given::Unknown => (Some(None), None),
            };
            let problem = match (waits, locks) {
                (Some(Some(what)), _) => {
                    Diagnostic::new(format!("{said}this waits on {what}"), passed.span).with_help(WAIT_FIRST)
                }
                (Some(None), _) => Diagnostic::new(
                    format!("{said}what this holds can't be known, so it can't be shown not to wait"),
                    passed.span,
                )
                .with_help("pass an arrow function or a `def` by name, so what it does can be checked"),
                (None, Some(what)) => {
                    Diagnostic::new(format!("{said}this takes {what}"), passed.span).with_help(TOGETHER)
                }
                (None, None) => continue,
            };
            self.diagnostics.push(problem);
        }
    }

    fn note_waits(&mut self, key: &str, what: &str) {
        let outer = self.outermost(key);
        self.waits.entry(outer).or_insert_with(|| what.to_owned());
    }

    /// Reports each call inside a lock to something that waits, following what waits up the
    /// call graph from what waits itself.
    pub(crate) fn waits_under_locks(&mut self) {
        let waiting = self.up_the_calls(&self.waits);
        let locking = self.up_the_calls(&self.takes_locks);
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
