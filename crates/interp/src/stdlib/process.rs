//! `std.process`: the methods of the `Process` capability. A program is run directly, with its
//! arguments as a list, so nothing in them is ever read by a shell. A program that can't be
//! started fails with a `ProcessError` value; one that runs and exits with a status other
//! than 0 doesn't fail, since what that means is the program's to say. In a test, `Process` is
//! a fake that runs nothing and gives back what the test scripted.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use super::{Module, no_constants, no_functions};
use crate::interp::Interp;
use crate::methods::no_method;
use crate::value::{Capability, Value};

pub(crate) const MODULE: Module = Module { functions: &[], call: no_functions, constant: no_constants, methods: &[] };

impl Interp<'_> {
    pub(crate) fn process_method(&mut self, method: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
        let faked = self.world.as_ref().is_some_and(|world| !world.real());
        match (method, args) {
            ("run", [Value::Str(program), Value::List(items)]) => {
                let mut arguments = Vec::new();
                for item in items.iter() {
                    let Value::Str(text) = item else { return Err(wrong(method, span)) };
                    arguments.push(text.to_string());
                }
                if faked {
                    return Ok(self.fake_run(program, arguments));
                }
                self.start_program(program, &arguments)
            }
            ("script", [Value::Str(program), output @ Value::Instance(_)]) if faked => {
                if let Some(world) = &mut self.world {
                    world.scripts.insert(program.to_string(), output.clone());
                }
                Ok(Value::Unit)
            }
            ("only", [Value::Str(program)]) => Ok(crate::limits::Limited::to(Capability::Process, vec![Rc::clone(program)])),
            ("ran", []) => {
                let ran = self.world.as_ref().map(|world| world.ran.clone()).unwrap_or_default();
                let lists = ran.iter().map(|run| Value::list(run.iter().map(|part| Value::str(part)).collect()));
                Ok(Value::list(lists.collect()))
            }
            ("run" | "script", _) => Err(wrong(method, span)),
            _ => Err(no_method(&Value::Capability(Capability::Process), method, span)),
        }
    }

    /// Runs `program` and waits for it, keeping what it writes.
    fn start_program(&mut self, program: &str, arguments: &[String]) -> Result<Value, Diagnostic> {
        // A prompt this program printed shows before the other program starts.
        self.flush()?;
        Ok(run_program(program, arguments))
    }

    /// What a test's fake `Process` gives back for `program`: what the test scripted for it,
    /// or a `ProcessError` saying nothing was.
    fn fake_run(&mut self, program: &str, arguments: Vec<String>) -> Value {
        let scripted = self.world.as_mut().and_then(|world| {
            let mut run = vec![program.to_owned()];
            run.extend(arguments);
            world.ran.push(run);
            world.scripts.get(program).cloned()
        });
        if let Some(output) = scripted {
            return Value::Ok(Rc::new(output));
        }
        let message = format!(
            "the test's fake Process has nothing scripted for `{program}`: give it what to answer with `process.script(\"{program}\", Output(...))`"
        );
        process_error(program, &message)
    }

}

/// Runs `program` and waits for it, giving a result holding what it wrote.
pub(crate) fn run_program(program: &str, arguments: &[String]) -> Value {
    match std::process::Command::new(program).args(arguments).output() {
        Ok(done) => {
            let status = done.status.code().map_or(-1, i64::from);
            let fields = vec![
                ("status", Value::Int(status)),
                ("stdout", Value::str(&String::from_utf8_lossy(&done.stdout))),
                ("stderr", Value::str(&String::from_utf8_lossy(&done.stderr))),
            ];
            Value::Ok(Rc::new(Value::made("Output", fields)))
        }
        Err(error) => process_error(program, &error.to_string()),
    }
}

/// A failed result holding a `ProcessError`.
pub(crate) fn process_error(program: &str, message: &str) -> Value {
    let fields = vec![("program", Value::str(program)), ("message", Value::str(message))];
    Value::Err(Rc::new(Value::made_error("ProcessError", fields)))
}

fn wrong(method: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{method}` was called with the wrong arguments"), span)
}
