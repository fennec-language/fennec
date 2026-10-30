//! What a program is handed from outside: its command-line arguments, which `main` receives
//! as `args: Args`, and its standard input, as `stdin: Stdin`.

use std::io::BufRead;
use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use crate::interp::Interp;
use crate::methods::no_method;
use crate::value::{Capability, Value};

/// A program's command-line arguments and standard input, for a host to hand over before
/// running it. By default there are no arguments and no input.
pub struct Inputs {
    /// The program's name, as `args.program`.
    pub program: String,
    /// The arguments after the program's name, as `for arg in args`.
    pub arguments: Vec<String>,
    pub stdin: Box<dyn BufRead + Send>,
}

impl Default for Inputs {
    fn default() -> Self {
        Inputs { program: String::new(), arguments: Vec::new(), stdin: Box::new(std::io::empty()) }
    }
}

/// The inputs as the interpreter keeps them.
pub(crate) struct Given {
    program: Rc<str>,
    arguments: Vec<Value>,
    stdin: Box<dyn BufRead + Send>,
}

impl From<Inputs> for Given {
    fn from(inputs: Inputs) -> Self {
        Given {
            program: Rc::from(inputs.program),
            arguments: inputs.arguments.iter().map(|arg| Value::str(arg)).collect(),
            stdin: inputs.stdin,
        }
    }
}

impl Interp<'_> {
    /// Hands the program its command-line arguments and standard input.
    pub fn give(&mut self, inputs: Inputs) {
        self.given = Given::from(inputs);
    }

    /// The command-line arguments, each a `str`.
    pub(crate) fn arguments(&self) -> Vec<Value> {
        self.given.arguments.clone()
    }

    /// `args.program`: the program's name.
    pub(crate) fn args_field(&self, name: &str, span: Span) -> Result<Value, Diagnostic> {
        if name == "program" {
            return Ok(Value::Str(Rc::clone(&self.given.program)));
        }
        Err(Diagnostic::new(format!("`Args` has no field `{name}`"), span)
            .with_help("loop over `args` for the arguments, or use `args.program` for the program's name"))
    }

    /// `stdin.lines()`: every line of input, without its line ending.
    pub(crate) fn stdin_method(
        &mut self,
        method: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        if method != "lines" || !args.is_empty() {
            return Err(no_method(&Value::Capability(Capability::Stdin), method, span));
        }
        let mut lines = Vec::new();
        for line in (&mut self.given.stdin).lines() {
            let line = line.map_err(|err| Diagnostic::new(format!("couldn't read input: {err}"), span))?;
            lines.push(Value::str(line.strip_suffix('\r').unwrap_or(&line)));
        }
        Ok(Value::list(lines))
    }
}
