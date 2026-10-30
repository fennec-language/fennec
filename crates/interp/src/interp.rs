//! The interpreter's state, and its entry points: running a program's `main`, evaluating
//! REPL input, and running tests.

use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;

use fennec_syntax::{Diagnostic, FnDef, Item, Module, Span, Stmt, TestDef, TypeDef};

use crate::exec::Flow;
use crate::format;
use crate::prelude;
use crate::scope::Frame;
use crate::value::{Callable, Capability, Value};

/// How deep calls may nest before the program is stopped, as in Python.
pub(crate) const MAX_DEPTH: usize = 1000;

/// The message of the internal diagnostic that carries a `?` or `fail` up to the enclosing
/// function. It never reaches the user: calls turn it back into a returned value.
pub(crate) const PASSING_UP: &str = "\0passing up";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TypeKind {
    Struct,
    Enum,
    Error,
}

pub(crate) struct TypeInfo {
    pub(crate) def: TypeDef,
    pub(crate) kind: TypeKind,
    pub(crate) methods: HashMap<String, Rc<Callable>>,
}

/// A running Fennec program: its declarations, the current call's names, and its output.
pub struct Interp<'out> {
    pub(crate) functions: HashMap<String, Rc<Callable>>,
    pub(crate) types: HashMap<String, Rc<TypeInfo>>,
    pub(crate) tests: Vec<TestDef>,
    pub(crate) frame: Frame,
    pub(crate) depth: usize,
    pub(crate) out: Box<dyn Write + 'out>,
    /// The value a `?` or `fail` is carrying up to the enclosing function.
    pub(crate) passing_up: Option<Flow>,
    /// Values collected by `yield`, one list per running generator.
    pub(crate) yields: Vec<Vec<Value>>,
}

/// Runs `module`'s `main`, writing what it prints to `out`.
pub fn run(module: &Module, out: &mut dyn Write) -> Result<(), Diagnostic> {
    let mut interp = Interp::new(out)?;
    let statements = interp.load(module)?;
    if let Some(stmt) = statements.first() {
        return Err(Diagnostic::new("only declarations can go at the top level of a file", stmt.span));
    }
    interp.run_main()
}

impl<'out> Interp<'out> {
    pub fn new(out: &'out mut dyn Write) -> Result<Self, Diagnostic> {
        Self::with_output(Box::new(out))
    }

    /// Like `new`, owning where the output goes.
    pub fn with_output(out: Box<dyn Write + 'out>) -> Result<Self, Diagnostic> {
        let mut interp = Interp {
            functions: HashMap::new(),
            types: HashMap::new(),
            tests: Vec::new(),
            frame: Frame::new(),
            depth: 0,
            out,
            passing_up: None,
            yields: Vec::new(),
        };
        for source in prelude::SOURCES {
            let module = fennec_syntax::parse(source)?;
            interp.load(&module)?;
        }
        Ok(interp)
    }

    /// Registers a module's declarations and returns its top-level statements, which only
    /// the REPL runs.
    pub fn load(&mut self, module: &Module) -> Result<Vec<Stmt>, Diagnostic> {
        let mut statements = Vec::new();
        for item in &module.items {
            match item {
                Item::Function(def) => {
                    self.functions.insert(def.name.clone(), Rc::new(callable(def, Vec::new())));
                }
                Item::Struct(def) => self.register_type(def, TypeKind::Struct),
                Item::Enum(def) => self.register_type(def, TypeKind::Enum),
                Item::Error(def) => self.register_type(def, TypeKind::Error),
                Item::Import(import) => prelude::check_import(import)?,
                Item::Test(test) => self.tests.push(test.clone()),
                Item::Interface(_) | Item::TypeAlias { .. } => {}
                Item::Stmt(stmt) => statements.push(stmt.clone()),
            }
        }
        Ok(statements)
    }

    fn register_type(&mut self, def: &TypeDef, kind: TypeKind) {
        let methods = def
            .methods
            .iter()
            .map(|method| (method.name.clone(), Rc::new(callable(method, Vec::new()))))
            .collect();
        self.types.insert(def.name.clone(), Rc::new(TypeInfo { def: def.clone(), kind, methods }));
    }

    /// Calls `main`, handing it the capabilities it asks for.
    pub fn run_main(&mut self) -> Result<(), Diagnostic> {
        let Some(main) = self.functions.get("main").cloned() else {
            return Err(Diagnostic::new("this program has no `main` function", Span::default())
                .with_help("add `def main(stdout: Stdout):`"));
        };
        let mut args = Vec::new();
        for param in &main.def.params {
            let ty = param.ty.as_ref().map_or("", |ty| ty.name.as_str());
            args.push(Value::Capability(match ty {
                "Stdout" => Capability::Stdout,
                "Fs" => Capability::Fs,
                other => {
                    return Err(Diagnostic::new(format!("`main` can't receive a `{other}`"), param.span)
                        .with_help("`main` can receive `stdout: Stdout` and `fs: Fs`"));
                }
            }));
        }
        let result = self.call_with_values(&main, args, main.def.span)?;
        self.flush()?;
        match result {
            Value::Err(error) => {
                Err(Diagnostic::new(format!("`main` failed: {}", format::repr(&error)), main.def.span))
            }
            _ => Ok(()),
        }
    }

    /// Starts a REPL session, where `stdout` and `fs` are already defined.
    pub fn start_repl(&mut self) {
        self.frame.declare("stdout", Value::Capability(Capability::Stdout), false);
        self.frame.declare("fs", Value::Capability(Capability::Fs), false);
    }

    /// Runs a chunk typed into the REPL. Returns how to echo its last expression, if anything.
    pub fn eval_repl(&mut self, module: &Module) -> Result<Option<String>, Diagnostic> {
        let statements = self.load(module)?;
        let mut echo = None;
        for stmt in &statements {
            echo = self.exec_top(stmt)?;
        }
        self.flush()?;
        Ok(echo)
    }

    pub(crate) fn flush(&mut self) -> Result<(), Diagnostic> {
        self.out
            .flush()
            .map_err(|err| Diagnostic::new(format!("couldn't write output: {err}"), Span::default()))
    }

    pub(crate) fn type_info(&self, name: &str) -> Option<Rc<TypeInfo>> {
        self.types.get(name).cloned()
    }
}

/// Wraps a function definition, noting whether it can fail and whether it's a generator.
pub(crate) fn callable(def: &FnDef, captured: Vec<(String, Value)>) -> Callable {
    let (fallible, generator) = crate::scan::body_effects(&def.body);
    Callable { def: Rc::new(def.clone()), captured, fallible, generator }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    /// Output that takes writes but can't be flushed, like a file on a full disk.
    struct Unflushable;

    impl Write for Unflushable {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("the disk is full"))
        }
    }

    #[test]
    fn capabilities_equal_themselves() {
        let source = "def main(stdout: Stdout, fs: Fs):\n    stdout.print(stdout == stdout, fs == fs)\n";
        let module = fennec_syntax::parse(source).unwrap();
        let mut out = Vec::new();
        super::run(&module, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "True True\n");
    }

    #[test]
    fn output_that_cant_be_flushed_is_an_error() {
        let module = fennec_syntax::parse("def main(stdout: Stdout):\n    stdout.print(1)\n").unwrap();
        let error = super::run(&module, &mut Unflushable).unwrap_err();
        assert_eq!(error.message, "couldn't write output: the disk is full");
    }
}
