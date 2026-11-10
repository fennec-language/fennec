//! What a program is handed from outside: its command-line arguments, which `main` receives
//! as `args: Args`, and its standard input, as `stdin: Stdin`.

use std::io::BufRead;

use fennec_syntax::{Diagnostic, Span};

use crate::value::Value;

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

/// `stdin.read_line()`: the next line of standard input without its ending, `None` at the end,
/// or an `IoError`. It reads through the process's one buffer, as `lines()` does, so nothing read
/// ahead is lost between them, and in a task it waits on a blocking thread.
pub(crate) fn read_line() -> Value {
    let read = crate::stdlib::fs::blocking(|| {
        let mut line = Vec::new();
        std::io::stdin().lock().read_until(b'\n', &mut line).map(|_| line)
    });
    match read {
        Ok(line) if line.is_empty() => Value::Ok(std::rc::Rc::new(Value::None)),
        Ok(mut line) => {
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            Value::Ok(std::rc::Rc::new(Value::str(&String::from_utf8_lossy(&line))))
        }
        Err(error) => crate::stdlib::fs::io_error(&format!("couldn't read input: {error}")),
    }
}

/// `stdin.read_chunk(size)`: up to `size` bytes of standard input, empty at the end, or an
/// `IoError`, through the same buffer as `read_line`.
pub(crate) fn read_chunk(size: usize) -> Value {
    let read = crate::stdlib::fs::blocking(move || {
        let mut chunk = vec![0; size];
        std::io::Read::read(&mut std::io::stdin().lock(), &mut chunk).map(|count| {
            chunk.truncate(count);
            chunk
        })
    });
    match read {
        Ok(chunk) => Value::Ok(std::rc::Rc::new(Value::Bytes(std::rc::Rc::from(chunk)))),
        Err(error) => crate::stdlib::fs::io_error(&format!("couldn't read input: {error}")),
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
