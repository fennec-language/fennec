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

use std::collections::BTreeMap;

use fennec_syntax::{Diagnostic, Span};

use crate::checker::Checker;
use crate::function::FnCx;
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

/// What a method of a capability waits on, if it waits.
fn capability_waits(owner: &str, method: &str) -> bool {
    match owner {
        "Net" | "Connection" | "Server" | "UdpSocket" => true,
        "Clock" => method == "sleep",
        "Signals" => method == "wait",
        "Process" => method == "run",
        "Stdin" => matches!(method, "read" | "read_line" | "lines"),
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
    }

    /// Notes the wait of a capability's method that has one.
    pub(crate) fn note_capability_wait(&mut self, ty: &Type, name: &str, span: Span) {
        if let Some(owner) = ty.type_name().filter(|owner| capability_waits(owner, name)) {
            self.note_wait(&format!("`{owner}.{name}`"), span);
        }
    }

    /// Keeps a call made inside a lock body, for when every function's waits are known.
    pub(crate) fn note_lock_call(&mut self, callee: &str, name: &str, span: Span) {
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

    fn note_waits(&mut self, key: &str, what: &str) {
        let outer = self.outermost(key);
        self.waits.entry(outer).or_insert_with(|| what.to_owned());
    }

    /// Reports each call inside a lock to something that waits, following what waits up the
    /// call graph from what waits itself.
    pub(crate) fn waits_under_locks(&mut self) {
        if self.lock_calls.is_empty() {
            return;
        }
        let waiting = self.up_the_calls(&self.waits);
        let locking = self.up_the_calls(&self.takes_locks);
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
