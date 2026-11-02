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
        match (limited.capability, method) {
            (Capability::Process, "only") => {
                let Some(Value::Str(program)) = args.first() else {
                    return self.process_method(method, args, span);
                };
                let allowed = limited.allowed.iter().filter(|name| ***name == **program).cloned().collect();
                Ok(Limited::to(Capability::Process, allowed))
            }
            (Capability::Process, "run") => {
                let Some(Value::Str(program)) = args.first() else {
                    return self.process_method(method, args, span);
                };
                if limited.allowed.iter().any(|name| **name == **program) {
                    return self.process_method(method, args, span);
                }
                let message =
                    format!("this Process can run only {}, so it can't run `{program}`", limited.listed());
                let fields =
                    vec![("program", Value::Str(Rc::clone(program))), ("message", Value::str(&message))];
                Ok(Value::Err(Rc::new(self.make_value("ProcessError", fields, span)?)))
            }
            (Capability::Process, _) => self.process_method(method, args, span),
            (Capability::Fs, _) => self.limited_fs(limited, method, args, span),
            (capability, _) => self.call_capability(capability, method, args, span),
        }
    }

    fn limited_fs(
        &mut self,
        limited: &Rc<Limited>,
        method: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let Some(Value::Str(path)) = args.first().map(path_text) else {
            return self.fs_method(method, args, span);
        };
        let full = absolute(&path);
        let reachable = limited.allowed.iter().any(|folder| inside(&full, folder));
        if method == "only" {
            let allowed = if reachable { vec![Rc::from(full)] } else { Vec::new() };
            return Ok(Limited::to(Capability::Fs, allowed));
        }
        if reachable {
            return self.fs_method(method, args, span);
        }
        if method == "exists" {
            return Ok(Value::Bool(false));
        }
        let reason = format!("`{path}` is outside what this Fs can reach: {}", limited.listed());
        let error = self.make_value("IoError", vec![("reason", Value::str(&reason))], span)?;
        Ok(Value::Err(Rc::new(error)))
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
