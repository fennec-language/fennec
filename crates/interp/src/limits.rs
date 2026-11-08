//! Capabilities narrowed with `only`: a `Process` that can run only the programs named, and an
//! `Fs` that reaches only what's inside the folders named. Narrowing again only takes more
//! away. Something outside what's allowed fails the way the capability fails, with a
//! `ProcessError` or an `IoError` value that says what was allowed, rather than stopping the
//! program. Folders are compared by their paths with `.` and `..` worked out, so a path can't
//! climb out of its folder by naming its parent.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use crate::interp::Interp;
use crate::stdlib::fs::path_text;
use crate::value::{Capability, Value};

#[derive(Debug, PartialEq)]
pub(crate) struct Limited {
    pub(crate) capability: Capability,
    /// The programs a `Process` may run, or the folders an `Fs` may reach, as absolute paths.
    pub(crate) allowed: Vec<Rc<str>>,
}

impl Limited {
    pub(crate) fn to(capability: Capability, allowed: Vec<Rc<str>>) -> Value {
        Value::Limited(Rc::new(Limited { capability, allowed }))
    }

    /// What's allowed, for a message: "`git` or `ls`", or "nothing".
    fn listed(&self) -> String {
        if self.allowed.is_empty() {
            return "nothing".to_owned();
        }
        let names: Vec<String> = self.allowed.iter().map(|name| format!("`{name}`")).collect();
        names.join(" or ")
    }
}

/// A path made absolute, against the folder the program runs in, with `.` and `..` worked out.
pub(crate) fn absolute(path: &str) -> String {
    let joined = if path.starts_with('/') {
        path.to_owned()
    } else {
        let here = std::env::current_dir().map(|dir| dir.display().to_string()).unwrap_or_default();
        format!("{here}/{path}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => drop(parts.pop()),
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Whether `path` is `folder` or inside it, both absolute.
fn inside(path: &str, folder: &str) -> bool {
    path == folder || folder == "/" || path.strip_prefix(folder).is_some_and(|rest| rest.starts_with('/'))
}

impl Interp<'_> {
    pub(crate) fn limited_method(
        &mut self,
        limited: &Rc<Limited>,
        method: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        narrowed(limited, method, args, &mut |capability, method, args| {
            // A test's capabilities are fakes, narrowed like any others.
            if let Some(world) = self.world.as_mut()
                && let Some(done) = world.method(capability, method, args, span)
            {
                return done;
            }
            match capability {
                Capability::Process => self.process_method(method, args, span),
                Capability::Fs => crate::stdlib::fs::fs_method(method, args, span),
                other => self.call_capability(other, method, args, span),
            }
        })
    }

    /// A method of a capability that `only` doesn't narrow.
    fn call_capability(
        &mut self,
        capability: Capability,
        method: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let args = args.iter().map(|value| crate::call::CallArg::plain(value.clone(), span)).collect();
        self.read_method(&Value::Capability(capability), method, args, span)
    }
}

/// Runs a method of a whole capability.
type CallWhole<'c> = dyn FnMut(Capability, &str, &[Value]) -> Result<Value, Diagnostic> + 'c;

/// A method of a narrowed capability: what it may do goes to `call`, and what it may not fails.
pub(crate) fn narrowed(
    limited: &Rc<Limited>,
    method: &str,
    args: &[Value],
    call: &mut CallWhole<'_>,
) -> Result<Value, Diagnostic> {
    match (limited.capability, method) {
        (Capability::Process, "only") => {
            let Some(Value::Str(program)) = args.first() else {
                return call(Capability::Process, method, args);
            };
            let allowed = limited.allowed.iter().filter(|name| ***name == **program).cloned().collect();
            Ok(Limited::to(Capability::Process, allowed))
        }
        // `run.cwd` and the like are `run` with options, and run only what it may.
        (Capability::Process, run) if run == "run" || run.starts_with("run.") => {
            let Some(Value::Str(program)) = args.first() else {
                return call(Capability::Process, method, args);
            };
            if limited.allowed.iter().any(|name| **name == **program) {
                return call(Capability::Process, method, args);
            }
            let message =
                format!("this Process can run only {}, so it can't run `{program}`", limited.listed());
            Ok(crate::stdlib::process::process_error(program, &message))
        }
        (Capability::Process, _) => call(Capability::Process, method, args),
        (Capability::Fs, _) => narrowed_fs(limited, method, args, call),
        (capability, _) => call(capability, method, args),
    }
}

fn narrowed_fs(
    limited: &Rc<Limited>,
    method: &str,
    args: &[Value],
    call: &mut CallWhole<'_>,
) -> Result<Value, Diagnostic> {
    let Some(Value::Str(path)) = args.first().map(path_text) else {
        return call(Capability::Fs, method, args);
    };
    let full = absolute(&path);
    let reachable = limited.allowed.iter().any(|folder| inside(&full, folder));
    if method == "only" {
        let allowed = if reachable { vec![Rc::from(full)] } else { Vec::new() };
        return Ok(Limited::to(Capability::Fs, allowed));
    }
    if reachable {
        return call(Capability::Fs, method, args);
    }
    if method == "exists" {
        return Ok(Value::Bool(false));
    }
    let reason = format!("`{path}` is outside what this Fs can reach: {}", limited.listed());
    Ok(crate::stdlib::fs::io_error(&reason))
}
