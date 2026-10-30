//! The interpreter's state, and its entry points: running a program's `main`, evaluating
//! REPL input, and running tests.

use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;

use fennec_syntax::{Diagnostic, Extension, FnDef, Item, Module, Span, Stmt, TestDef, TypeDef};

use crate::exec::Flow;
use crate::format;
use crate::prelude;
use crate::scope::Frame;
use crate::value::{Callable, Capability, Value};

/// How deep calls may nest before the program is stopped, as in Python.
const MAX_DEPTH: usize = 1000;

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

/// The built-in types the standard library's code may give methods to with `extend`.
const BUILT_IN_OWNERS: [&str; 8] = ["str", "bytes", "int", "float", "bool", "list", "set", "dict"];

/// A running Fennec program: its declarations, the current call's names, and its output.
pub struct Interp<'out> {
    pub(crate) functions: HashMap<String, Rc<Callable>>,
    /// The constants at the top level of the program, by name.
    pub(crate) constants: HashMap<String, crate::constants::Constant>,
    pub(crate) types: HashMap<String, Rc<TypeInfo>>,
    /// Builtins that exist only once imported, by the name they're imported under.
    pub(crate) imports: HashMap<String, &'static str>,
    /// What the standard library's own code imports, which only that code sees.
    pub(crate) prelude_imports: HashMap<String, &'static str>,
    /// Imported modules, by the name they're used under, like `json` in `json.loads(...)`.
    pub(crate) modules: HashMap<String, &'static str>,
    pub(crate) tests: Vec<TestDef>,
    pub(crate) frame: Frame,
    pub(crate) depth: usize,
    pub(crate) max_depth: usize,
    pub(crate) out: Box<dyn Write + 'out>,
    /// The value a `?` or `fail` is carrying up to the enclosing function.
    pub(crate) passing_up: Option<Flow>,
    /// Values collected by `yield`, one list per running generator.
    pub(crate) yields: Vec<Vec<Value>>,
    /// Each regex pattern used so far, compiled.
    pub(crate) regexes: HashMap<String, Rc<fennec_re::Regex>>,
    /// How many more steps the program may take, if it's limited.
    pub(crate) steps_left: Option<u64>,
    step_limit: Option<u64>,
    files: bool,
    clock: bool,
    /// The command-line arguments and standard input `main` can receive.
    pub(crate) given: crate::inputs::Given,
    /// The methods the standard library gives built-in types, like `str`'s `def *`, by type.
    pub(crate) built_in_methods: HashMap<String, HashMap<String, Rc<Callable>>>,
}

/// Runs `module`'s `main`, writing what it prints to `out`.
pub fn run(module: &Module, out: &mut dyn Write) -> Result<(), Diagnostic> {
    Interp::new(out)?.run_program(module)
}

impl<'out> Interp<'out> {
    pub fn new(out: &'out mut dyn Write) -> Result<Self, Diagnostic> {
        Self::with_output(Box::new(out))
    }

    /// Like `new`, owning where the output goes.
    pub fn with_output(out: Box<dyn Write + 'out>) -> Result<Self, Diagnostic> {
        let mut interp = Interp {
            functions: HashMap::new(),
            constants: HashMap::new(),
            types: HashMap::new(),
            imports: HashMap::new(),
            prelude_imports: HashMap::new(),
            modules: HashMap::new(),
            tests: Vec::new(),
            frame: Frame::new(),
            depth: 0,
            max_depth: MAX_DEPTH,
            out,
            passing_up: None,
            yields: Vec::new(),
            regexes: HashMap::new(),
            steps_left: None,
            step_limit: None,
            files: true,
            clock: true,
            given: crate::inputs::Inputs::default().into(),
            built_in_methods: HashMap::new(),
        };
        for source in fennec_syntax::prelude() {
            let module = fennec_syntax::parse_prelude(source)?;
            interp.load(&module)?;
        }
        interp.prelude_imports = std::mem::take(&mut interp.imports);
        Ok(interp)
    }

    /// Stops `main`, each test, and each REPL chunk once it has taken `steps` steps, so a
    /// loop that never ends can't hang the host. A step is one statement or expression.
    pub fn limit_steps(&mut self, steps: u64) {
        self.step_limit = Some(steps);
    }

    /// Stops the program once calls nest `depth` deep, for hosts with less stack than the
    /// default limit of 1000 needs.
    pub fn limit_depth(&mut self, depth: usize) {
        self.max_depth = depth;
    }

    /// Refuses `main` the `Fs` capability, for hosts that have no files, like a browser.
    pub fn deny_files(&mut self) {
        self.files = false;
    }

    /// Refuses `main` the `Clock` capability, for hosts with no clock to read, like a
    /// browser running WebAssembly.
    pub fn deny_clock(&mut self) {
        self.clock = false;
    }

    /// Gives the program its full step budget again.
    pub(crate) fn refill_steps(&mut self) {
        self.steps_left = self.step_limit;
    }

    /// Takes one step, or stops the program if it has run out.
    pub(crate) fn step(&mut self, span: Span) -> Result<(), Diagnostic> {
        match self.steps_left {
            None => Ok(()),
            Some(0) => Err(out_of_steps(self.step_limit.unwrap_or(0), span)),
            Some(left) => {
                self.steps_left = Some(left - 1);
                Ok(())
            }
        }
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
                Item::Import(import) => {
                    let (modules, functions) = prelude::check_import(import)?;
                    self.modules.extend(modules);
                    self.imports.extend(functions);
                }
                Item::Test(test) => self.tests.push(test.clone()),
                Item::Const { name, value, .. } => {
                    let waiting = crate::constants::Constant::Waiting(Rc::new(value.clone()));
                    self.constants.insert(name.clone(), waiting);
                }
                Item::Interface(_) | Item::TypeAlias { .. } | Item::Extend(_) => {}
                Item::Stmt(stmt) => statements.push(stmt.clone()),
            }
        }
        // Extensions come last, so the type they extend is known wherever it's declared.
        for item in &module.items {
            if let Item::Extend(extension) = item {
                self.extend_type(extension)?;
            }
        }
        Ok(statements)
    }

    /// Adds an extension's methods to its type, as if they were written in its declaration.
    fn extend_type(&mut self, extension: &Extension) -> Result<(), Diagnostic> {
        if BUILT_IN_OWNERS.contains(&extension.name.as_str()) {
            let methods = self.built_in_methods.entry(extension.name.clone()).or_default();
            for method in &extension.methods {
                methods.insert(method.name.clone(), Rc::new(callable(method, Vec::new())));
            }
            return Ok(());
        }
        let info = self
            .types
            .get(&extension.name)
            .ok_or_else(|| Diagnostic::new(format!("`{}` isn't a type", extension.name), extension.span))?;
        let mut methods = info.methods.clone();
        for method in &extension.methods {
            methods.insert(method.name.clone(), Rc::new(callable(method, Vec::new())));
        }
        let extended = TypeInfo { def: info.def.clone(), kind: info.kind, methods };
        self.types.insert(extension.name.clone(), Rc::new(extended));
        Ok(())
    }

    /// A method the standard library gives a built-in value's type, like `str`'s `def *`.
    pub(crate) fn built_in_method(&self, value: &Value, method: &str) -> Option<Rc<Callable>> {
        let owner = match value {
            Value::Str(_) => "str",
            Value::Bytes(_) => "bytes",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Bool(_) => "bool",
            Value::List(_) | Value::Packed(_) => "list",
            Value::Set(_) => "set",
            Value::Dict(_) => "dict",
            _ => return None,
        };
        self.built_in_methods.get(owner)?.get(method).cloned()
    }

    fn register_type(&mut self, def: &TypeDef, kind: TypeKind) {
        let methods = def
            .methods
            .iter()
            .map(|method| (method.name.clone(), Rc::new(callable(method, Vec::new()))))
            .collect();
        self.types.insert(def.name.clone(), Rc::new(TypeInfo { def: def.clone(), kind, methods }));
    }

    /// Loads a program and calls its `main`.
    pub fn run_program(&mut self, module: &Module) -> Result<(), Diagnostic> {
        let statements = self.load(module)?;
        if let Some(stmt) = statements.first() {
            return Err(Diagnostic::new(
                "only declarations and constants can go at the top level of a file",
                stmt.span,
            ));
        }
        self.run_main()
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
                "Fs" if self.files => Capability::Fs,
                "Fs" => {
                    return Err(Diagnostic::new(
                        "`main` can't receive `Fs` here: there are no files",
                        param.span,
                    )
                    .with_help("where this runs, `main` can only receive `stdout: Stdout`"));
                }
                "Clock" if self.clock => Capability::Clock,
                "Args" => Capability::Args,
                "Stdin" => Capability::Stdin,
                "Clock" => {
                    return Err(Diagnostic::new(
                        "`main` can't receive `Clock` here: there's no clock",
                        param.span,
                    )
                    .with_help("where this runs, `main` can only receive `stdout: Stdout`"));
                }
                other => {
                    return Err(Diagnostic::new(format!("`main` can't receive a `{other}`"), param.span)
                        .with_help(
                            "`main` can receive `stdout: Stdout`, `fs: Fs`, `clock: Clock`, `args: Args` and `stdin: Stdin`",
                        ));
                }
            }));
        }
        self.refill_steps();
        let result = self.call_with_values(&main, args, main.def.span)?;
        self.flush()?;
        match result {
            Value::Err(error) => {
                Err(Diagnostic::new(format!("`main` failed: {}", format::repr(&error)), main.def.span))
            }
            _ => Ok(()),
        }
    }

    /// Starts a REPL session, where `stdout`, `fs` and `clock` are already defined.
    pub fn start_repl(&mut self) {
        self.frame.declare("stdout", Value::Capability(Capability::Stdout), false);
        self.frame.declare("fs", Value::Capability(Capability::Fs), false);
        self.frame.declare("clock", Value::Capability(Capability::Clock), false);
    }

    /// Runs a chunk typed into the REPL. Returns how to echo its last expression, if anything.
    pub fn eval_repl(&mut self, module: &Module) -> Result<Option<String>, Diagnostic> {
        let statements = self.load(module)?;
        self.refill_steps();
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

fn out_of_steps(limit: u64, span: Span) -> Diagnostic {
    Diagnostic::new(format!("stopped after {limit} steps: this ran too long"), span)
        .with_help("check for a loop that never ends, or recursion that never stops")
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
