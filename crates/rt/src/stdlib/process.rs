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

/// The words of a `process.run(t"...")`: its text split at spaces, with each value part of the
/// word it's written in, whatever it holds, so a value can't add a word or end one. `None` if
/// this isn't a template of text.
pub(crate) fn words_of(template: &Value) -> Option<Vec<String>> {
    let Value::Instance(instance) = template else { return None };
    let (Value::List(strings), Value::List(values)) = (instance.field("strings")?, instance.field("values")?)
    else {
        return None;
    };
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    for (at, piece) in strings.iter().enumerate() {
        let Value::Str(piece) = piece else { return None };
        for letter in piece.chars() {
            if letter.is_whitespace() {
                words.extend(word.take());
            } else {
                word.get_or_insert_default().push(letter);
            }
        }
        match values.get(at) {
            Some(Value::Str(value)) => word.get_or_insert_default().push_str(value),
            Some(_) => return None,
            None => {}
        }
    }
    words.extend(word);
    Some(words)
}

/// The first value of a `process.run(t"...")` that starts an argument with `-`, by how it's
/// written in its `{}`: the program would read it as an option, like `--upload-pack=` naming a
/// program for git to run, rather than as what it was meant to be. A value later in an argument,
/// like `t"--depth={depth}"`, a `-` in the template's own text, and a value after an argument the
/// template wrote as `--`, which ends the options for most programs, are what the template says.
pub(crate) fn option_started(template: &Value) -> Option<String> {
    let Value::Instance(instance) = template else { return None };
    let (Value::List(strings), Value::List(values)) = (instance.field("strings")?, instance.field("values")?)
    else {
        return None;
    };
    let expressions = instance.field("expressions");
    // Whether the argument being read has anything in it yet, the template's own text of it, and
    // whether an argument the template wrote as `--` has ended the options already.
    let (mut started, mut written, mut options_ended) = (false, String::new(), false);
    for (at, piece) in strings.iter().enumerate() {
        let Value::Str(piece) = piece else { return None };
        for letter in piece.chars() {
            if letter.is_whitespace() {
                options_ended = options_ended || (started && written == "--");
                started = false;
                written.clear();
            } else {
                started = true;
                written.push(letter);
            }
        }
        if let Some(Value::Str(value)) = values.get(at) {
            if !started && !options_ended && value.starts_with('-') {
                let written = match &expressions {
                    Some(Value::List(written)) => match written.get(at) {
                        Some(Value::Str(expression)) => expression.to_string(),
                        _ => String::new(),
                    },
                    _ => String::new(),
                };
                return Some(written);
            }
            if !value.is_empty() {
                started = true;
                // A value in it makes it more than the template's own `--`.
                written.push('\0');
            }
        }
    }
    None
}

/// What a call to `run` asks to run: the program, its arguments and the options.
pub(crate) type Ran = (String, Vec<String>, Options);

/// The program, its arguments and the options of a call to `run`, named as `options_on_name`
/// names it, or the `ProcessError` it fails with, without running anything: a template with a
/// value that would be read as an option.
pub(crate) fn run_of(method: &str, args: &[Value], span: Span) -> Result<Result<Ran, Value>, Diagnostic> {
    let names: Vec<&str> = method.split('.').skip(1).collect();
    let (plain, given) = args.split_at(args.len().saturating_sub(names.len()));
    let (program, arguments) = match plain {
        [Value::Str(program), Value::List(items)] => {
            let mut arguments = Vec::with_capacity(items.len());
            for item in items.iter() {
                let Value::Str(text) = item else { return Err(wrong("run", span)) };
                arguments.push(text.to_string());
            }
            (program.to_string(), arguments)
        }
        [template] => {
            let mut words = words_of(template).ok_or_else(|| wrong("run", span))?.into_iter();
            let program =
                words.next().ok_or_else(|| Diagnostic::new("this template names no program to run", span))?;
            if let Some(expression) = option_started(template) {
                return Ok(Err(looks_like_an_option(&program, &expression)));
            }
            (program, words.collect())
        }
        _ => return Err(wrong("run", span)),
    };
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
    Ok(Ok((program, arguments, options)))
}

/// The failure of a template whose value `expression` starts an argument with `-`.
fn looks_like_an_option(program: &str, expression: &str) -> Value {
    let fields = vec![("program", Value::str(program)), ("expression", Value::str(expression))];
    Value::Err(Rc::new(Value::made_case("ProcessError", "LooksLikeAnOption", fields)))
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
    let fields = vec![("program", Value::str(program)), ("reason", Value::str(message))];
    Value::Err(Rc::new(Value::made_case("ProcessError", "NotRun", fields)))
}

fn wrong(method: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{method}` was called with the wrong arguments"), span)
}
