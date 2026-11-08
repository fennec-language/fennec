//! What a program is handed from outside: its command-line arguments, which `main` receives
//! as `args: Args`, and its standard input, as `stdin: Stdin`.

use std::io::BufRead;
use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

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
    /// Whether native code's output goes straight to this process's standard output as the
    /// program runs, rather than being kept for the host.
    pub live: bool,
}

impl Default for Inputs {
    fn default() -> Self {
        Inputs {
            program: String::new(),
            arguments: Vec::new(),
            stdin: Box::new(std::io::empty()),
            live: false,
        }
    }
}

/// Every line of input, without its line ending.
pub(crate) fn lines_of(input: &mut dyn BufRead, span: Span) -> Result<Value, Diagnostic> {
    let mut lines = Vec::new();
    for line in input.lines() {
        let line = line.map_err(|err| Diagnostic::new(format!("couldn't read input: {err}"), span))?;
        lines.push(Value::str(line.strip_suffix('\r').unwrap_or(&line)));
    }
    Ok(Value::list(lines))
}
