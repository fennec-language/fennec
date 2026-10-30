//! The interpreter's state, and `run`, which calls a program's `main`.

use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;

use fennec_syntax::{Diagnostic, FnDef, Module, Span};

use crate::scope::Frame;
use crate::value::Value;

/// How deep calls may nest before the program is stopped, as in Python.
pub(crate) const MAX_DEPTH: usize = 1000;

pub(crate) struct Interp<'out> {
    pub(crate) functions: HashMap<String, Rc<FnDef>>,
    pub(crate) frame: Frame,
    pub(crate) depth: usize,
    pub(crate) out: &'out mut dyn Write,
}

/// How a statement finished.
pub(crate) enum Flow {
    Normal(Value),
    Return(Value),
    Break,
    Continue,
}

/// Runs `module`'s `main`, writing what it prints to `out`.
pub fn run(module: &Module, out: &mut dyn Write) -> Result<(), Diagnostic> {
    let mut functions = HashMap::new();
    for function in &module.functions {
        let previous = functions.insert(function.name.clone(), Rc::new(function.clone()));
        if previous.is_some() {
            return Err(Diagnostic::new(
                format!("`{}` is defined twice", function.name),
                function.span,
            ));
        }
    }
    let Some(main) = functions.get("main").cloned() else {
        return Err(
            Diagnostic::new("this program has no `main` function", Span::default())
                .with_help("add `def main(stdout: Stdout):`"),
        );
    };
    let mut args = Vec::new();
    for param in &main.params {
        if param.ty.name != "Stdout" {
            return Err(
                Diagnostic::new(format!("`main` can't receive a `{}`", param.ty.name), param.span)
                    .with_help("so far `main` can receive `stdout: Stdout`"),
            );
        }
        args.push(Value::Stdout);
    }
    let mut interp = Interp {
        functions,
        frame: Frame::new(),
        depth: 0,
        out,
    };
    interp.call_with_values(&main, args, main.span)?;
    interp
        .out
        .flush()
        .map_err(|err| Diagnostic::new(format!("couldn't write output: {err}"), Span::default()))
}
