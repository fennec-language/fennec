//! `std.process`: the methods of the `Process` capability. A program is run directly, with its
//! arguments as a list, so nothing in them is ever read by a shell. A program that can't be
//! started fails with a `ProcessError` value; one that runs and exits with a status other
//! than 0 doesn't fail, since what that means is the program's to say. In a test, `Process` is
//! a fake that runs nothing and gives back what the test scripted.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use crate::value::Value;

/// What `run` was given besides the program and its arguments: what the program reads from its
/// standard input, the folder it runs in, and variables it gets on top of this program's own.
#[derive(Default)]
pub(crate) struct Options {
    input: Option<Vec<u8>>,
    cwd: Option<String>,
    env: Vec<(String, String)>,
}

/// The program, its arguments and the options of a call to `run`, named as `options_on_name`
/// names it.
pub(crate) fn run_of(
    method: &str,
    args: &[Value],
    span: Span,
) -> Result<(String, Vec<String>, Options), Diagnostic> {
    let names: Vec<&str> = method.split('.').skip(1).collect();
    let (plain, given) = args.split_at(args.len().saturating_sub(names.len()));
    let [Value::Str(program), Value::List(items)] = plain else { return Err(wrong("run", span)) };
    let mut arguments = Vec::with_capacity(items.len());
    for item in items.iter() {
        let Value::Str(text) = item else { return Err(wrong("run", span)) };
        arguments.push(text.to_string());
    }
    let mut options = Options::default();
    for (name, value) in names.iter().zip(given) {
        match (*name, value) {
            ("input", Value::Str(text)) => options.input = Some(text.as_bytes().to_vec()),
            ("input", Value::Bytes(data)) => options.input = Some(data.to_vec()),
            ("cwd", folder) => {
                let Value::Str(folder) = crate::stdlib::fs::path_text(folder) else {
                    return Err(wrong("run", span));
                };
                options.cwd = Some(folder.to_string());
            }
            ("env", Value::Dict(pairs)) => {
                for (key, value) in pairs.iter() {
                    let (Value::Str(key), Value::Str(value)) = (key, value) else {
                        return Err(wrong("run", span));
                    };
                    options.env.push((key.to_string(), value.to_string()));
                }
            }
            (other, _) => {
                return Err(Diagnostic::new(format!("`run` has no parameter called `{other}`"), span));
            }
        }
    }
    Ok((program.to_string(), arguments, options))
}

/// Runs `program` and waits for it, giving a result holding what it wrote.
pub(crate) fn run_program(program: &str, arguments: &[String], options: &Options) -> Value {
    if let Some(folder) = &options.cwd
        && !std::path::Path::new(folder).is_dir()
    {
        return process_error(program, &format!("there's no folder `{folder}` to run it in"));
    }
    let mut command = std::process::Command::new(program);
    command.args(arguments);
    if let Some(folder) = &options.cwd {
        command.current_dir(folder);
    }
    command.envs(options.env.iter().map(|(name, value)| (name, value)));
    let ran = match &options.input {
        Some(input) => with_input(command, input),
        None => command.output(),
    };
    match ran {
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

/// Runs `command`, writing `input` to its standard input on a thread of its own while what it
/// writes is read, so neither side waits on the other. A program that stops reading early
/// leaves the rest unwritten, which isn't an error.
fn with_input(mut command: std::process::Command, input: &[u8]) -> std::io::Result<std::process::Output> {
    use std::io::Write as _;
    use std::process::Stdio;
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdin = child.stdin.take();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(&input);
        }
    });
    let output = child.wait_with_output();
    let _ = writer.join();
    output
}

/// A failed result holding a `ProcessError`.
pub(crate) fn process_error(program: &str, message: &str) -> Value {
    let fields = vec![("program", Value::str(program)), ("message", Value::str(message))];
    Value::Err(Rc::new(Value::made_error("ProcessError", fields)))
}

fn wrong(method: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{method}` was called with the wrong arguments"), span)
}
