//! The builtins' signatures: Python's builtins that Fennec keeps, `Ok`, `Err`, `replace` and
//! `ignore`, and the methods of built-in values. Methods that change a value need it held in
//! a `mut` place.

use fennec_syntax::{Arg, Diagnostic, Expr, Span};

use crate::function::FnCx;
use crate::infer::Hole;
use crate::messages::{mismatch, needless_mut, no_method, wrong_args};
use crate::stdlib;
use crate::types::{Type, a, article};
use crate::{ParamType, Signature};

/// Each builtin, and how many plain arguments it takes (at least, at most). Standard library
/// functions, like `json.loads`, are in `stdlib`, and exist only once imported.
const BUILTINS: [(&str, usize, usize); 33] = [
    ("Err", 1, 1),
    ("Ok", 0, 1),
    ("all", 1, 1),
    ("any", 1, 1),
    ("bytes", 0, 1),
    ("enumerate", 1, 1),
    ("float", 1, 1),
    ("i8", 1, 1),
    ("i16", 1, 1),
    ("i32", 1, 1),
    ("u8", 1, 1),
    ("u16", 1, 1),
    ("u32", 1, 1),
    ("u64", 1, 1),
    ("f32", 1, 1),
    // `int` and `float` by the names of their sizes, as `i64(small)` widens.
    ("i64", 1, 1),
    ("f64", 1, 1),
    ("copy", 1, 1),
    ("int", 1, 1),
    ("list", 1, 1),
    ("max", 1, usize::MAX),
    ("min", 1, usize::MAX),
    ("parallel", 1, 1),
    ("range", 1, 3),
    ("replace", 1, usize::MAX),
    ("repr", 1, 1),
    ("reversed", 1, 1),
    ("round", 1, 2),
    ("set", 0, 1),
    ("sorted", 1, 1),
    ("str", 1, 1),
    ("sum", 1, 2),
    ("zip", 2, 2),
];

/// The names of the functions the language provides everywhere, called bare, as `round` and `len`.
pub(crate) fn names() -> impl Iterator<Item = &'static str> {
    ["len", "abs"].into_iter().chain(BUILTINS.iter().map(|(name, ..)| *name))
}

/// Builtins whose arguments have to be plain values, not results or optionals.
const NEEDS_VALUES: [&str; 14] =
    ["float", "int", "range", "round", "i8", "i16", "i32", "u8", "u16", "u32", "u64", "f32", "i64", "f64"];

pub(crate) fn is_builtin(name: &str) -> bool {
    BUILTINS.iter().any(|(builtin, ..)| *builtin == name) || stdlib::function(name).is_some()
}

/// The modules with a function of this name, like `json` and `toml` for `loads`.
pub(crate) fn modules_with(name: &str) -> Vec<&'static str> {
    stdlib::functions()
        .filter_map(|builtin| builtin.split_once('.').filter(|(_, function)| *function == name))
        .map(|(module, _)| module)
        .chain(fennec_syntax::importable_type(name))
        .collect()
}

/// A method of a built-in value: whether it changes the value, what it takes and gives
/// back, and how many arguments it takes.
pub(crate) struct Method {
    pub(crate) changes: bool,
    /// The types of its arguments, in order. Arguments past these can be anything.
    pub(crate) params: Vec<Type>,
    returns: Type,
    arity: (usize, usize),
    /// The types an arrow function passed to it receives, like the item for `remove_where`.
    receives: Vec<Type>,
}

pub(crate) fn reads(params: &[Type], returns: Type) -> Method {
    let count = params.len();
    Method { changes: false, params: params.to_vec(), returns, arity: (count, count), receives: Vec::new() }
}

fn changes(params: &[Type], returns: Type) -> Method {
    Method { changes: true, ..reads(params, returns) }
}

impl Method {
    /// Its signature as a person reads it, for a hover: `def split(self, str) -> list[str]`, with
    /// `ref self` when it changes the value, and the arguments that can be left out in `[...]`.
    /// The engines know only the arguments' types, not their names.
    pub(crate) fn shown(&self, name: &str) -> String {
        let receiver = if self.changes { "ref self" } else { "self" };
        let mut takes = vec![receiver.to_owned()];
        for (at, param) in self.params.iter().enumerate() {
            takes.push(if at >= self.arity.0 { format!("[{param}]") } else { param.to_string() });
        }
        // One that takes any number, as `print`.
        if self.arity.1 > self.params.len() {
            takes.push("...".to_owned());
        }
        // What can fail is written as a signature says it: `-> str fails IoError`.
        let (gives, fails) = match &self.returns {
            Type::Result(value, error) => (value.as_ref(), Some(error.as_ref())),
            other => (other, None),
        };
        let gives = if *gives == Type::Unit { String::new() } else { format!(" -> {gives}") };
        let fails = fails.map_or_else(String::new, |error| format!(" fails {error}"));
        format!("def {name}({}){gives}{fails}", takes.join(", "))
    }

    /// An arrow function passed to this method receives an item.
    fn receiving(self, item: &Type) -> Method {
        Method { receives: vec![item.clone()], ..self }
    }

    /// The last `optional` arguments can be left out.
    pub(crate) fn optional(self, optional: usize) -> Method {
        Method { arity: (self.arity.0.saturating_sub(optional), self.arity.1), ..self }
    }

    /// Takes any number of arguments of any type.
    fn any_args(self) -> Method {
        Method { arity: (0, usize::MAX), ..self }
    }
}

/// The signature of a method or operator the engines give a built-in type, for fitting it
/// to an interface: `abs` on an int, or `+` on text.
pub(crate) fn engine_signature(ty: &Type, name: &str) -> Option<Signature> {
    let param = |name: &str, ty: &Type, is_mut| ParamType {
        name: name.to_owned(),
        ty: ty.clone(),
        is_mut,
        default: None,
    };
    let (others, returns, is_mut) = if let Some(found) = method(ty, name) {
        (found.params, found.returns, found.changes)
    } else {
        let returns = match (ty, name) {
            (Type::Int | Type::Float, "+" | "-" | "*") | (Type::Str, "+") => ty.clone(),
            (Type::Int | Type::Float | Type::Str, "<") => Type::Bool,
            _ => return None,
        };
        (vec![ty.clone()], returns, false)
    };
    let mut params = vec![param("self", ty, is_mut)];
    params.extend(others.iter().map(|other| param("other", other, false)));
    Some(Signature { params, returns, can_fail: false, generator: false, generics: Vec::new() })
}

pub(crate) fn method(ty: &Type, name: &str) -> Option<Method> {
    // Every collection knows its length, and so does a tuple.
    let sized = matches!(
        ty,
        Type::List(_)
            | Type::Array(..)
            | Type::Buffer(_)
            | Type::Set(_)
            | Type::Dict(..)
            | Type::Table(_)
            | Type::Str
            | Type::Bytes
            | Type::Tuple(_)
    );
    if sized && name == "len" {
        return Some(reads(&[], Type::Int));
    }
    if matches!(ty, Type::Int | Type::Float | Type::Sized(crate::types::Num::F32)) && name == "abs" {
        return Some(reads(&[], ty.clone()));
    }
    if matches!(ty, Type::Float) && name == "is_nan" {
        return Some(reads(&[], Type::Bool));
    }
    match ty {
        Type::List(item) => list_method(item, name),
        Type::Buffer(item) => buffer_method(item, name),
        Type::Set(item) => set_method(item, name),
        Type::Dict(key, value) => dict_method(key, value, name),
        Type::Table(item) => id_map_method(item, name),
        Type::Str => str_method(name),
        Type::Bytes => bytes_method(name),
        Type::Named(type_name) => named_method(type_name, name),
        _ => None,
    }
}

fn list_method(item: &Type, name: &str) -> Option<Method> {
    let item = item.clone();
    Some(match name {
        "append" => changes(&[item], Type::Unit),
        "insert" => changes(&[Type::Int, item], Type::Unit),
        "clear" => changes(&[], Type::Unit),
        "remove_where" => changes(&[], Type::Unit).receiving(&item).any_args(),
        "sort" => changes(&[], Type::Unit).receiving(&item),
        "pop" => changes(&[Type::Int], item).optional(1),
        _ => return None,
    })
}

/// `Buffer[T]`'s methods. `b[i]` and `b[i] = v` are indexing, as for a list.
fn buffer_method(item: &Type, name: &str) -> Option<Method> {
    let item = item.clone();
    Some(match name {
        "push" => changes(std::slice::from_ref(&item), Type::Unit),
        "pop" => changes(&[], item),
        "extend" => changes(&[Type::Buffer(Box::new(item.clone()))], Type::Unit),
        "insert" => changes(&[Type::Int, item], Type::Unit),
        "remove" => changes(&[Type::Int], item),
        "reserve" | "truncate" => changes(&[Type::Int], Type::Unit),
        "capacity" => reads(&[], Type::Int),
        "is_unique" => reads(&[], Type::Bool),
        _ => return None,
    })
}

fn set_method(item: &Type, name: &str) -> Option<Method> {
    Some(match name {
        "add" | "discard" => changes(std::slice::from_ref(item), Type::Unit),
        "clear" => changes(&[], Type::Unit),
        _ => return None,
    })
}

fn dict_method(key: &Type, value: &Type, name: &str) -> Option<Method> {
    let (key, value) = (key.clone(), value.clone());
    Some(match name {
        "pop" => changes(&[key], value),
        "get" => reads(&[key, value.clone()], Type::optional(value)).optional(1),
        "keys" => reads(&[], Type::list(key)),
        "values" => reads(&[], Type::list(value)),
        "items" => reads(&[], Type::list(Type::Tuple(vec![key, value]))),
        "clear" => changes(&[], Type::Unit),
        _ => return None,
    })
}

/// `copy(x)` with no fields: every copy is a snapshot already, as a plain `=` or argument makes
/// one, so taking the call off changes nothing the program does.
fn plain_copy(call: Span, value: Span) -> Diagnostic {
    Diagnostic::new("there's no plain `copy(...)`: every copy is a snapshot already", call)
        .with_help("use the value itself, which `fennec fix` writes; `copy(x, field=new)` still changes fields as it copies")
        .with_fix(Span { start: call.start, end: value.start }, "")
        .with_fix(Span { start: value.end, end: call.end }, "")
}

fn id_map_method(item: &Type, name: &str) -> Option<Method> {
    let id = Type::Id(Box::new(item.clone()));
    let item = item.clone();
    Some(match name {
        "add" => changes(&[item], id),
        "remove" => changes(&[id], Type::optional(item)),
        "get" => reads(&[id], Type::optional(item)),
        "modify" => changes(&[id], Type::Bool).receiving(&item).any_args(),
        "remove_where" => changes(&[], Type::Unit).receiving(&item).any_args(),
        "values" => reads(&[], Type::list(item)),
        "ids" => reads(&[], Type::list(id)),
        "items" => reads(&[], Type::list(Type::Tuple(vec![id, item]))),
        "clear" => changes(&[], Type::Unit),
        _ => return None,
    })
}

fn str_method(name: &str) -> Option<Method> {
    let text = Type::Str;
    Some(match name {
        "upper" | "lower" | "strip" | "lstrip" | "rstrip" => reads(&[], Type::Str),
        "split" => reads(&[text], Type::list(Type::Str)).optional(1),
        "replace" => reads(&[text.clone(), text], Type::Str),
        "find" => reads(&[text], Type::optional(Type::Int)),
        "join" => reads(&[Type::list(Type::Str)], Type::Str),
        "encode" => reads(&[], Type::Bytes),
        "code" => reads(&[], Type::Int),
        "codes" => reads(&[], Type::list(Type::Int)),
        _ => return None,
    })
}

/// `data.decode()` reads UTF-8 text, and fails on bytes that aren't. `data.hex()` writes
/// each byte as two hex digits, with `sep` between bytes if it's given.
fn bytes_method(name: &str) -> Option<Method> {
    Some(match name {
        "decode" => reads(&[], Type::result(Type::Str, Type::named("InvalidValue"))),
        "hex" => reads(&[Type::Str], Type::Str).optional(1),
        _ => return None,
    })
}

/// The named arguments a method written in Rust takes, past its plain ones, like `sort`'s `key`.
pub(crate) fn named_arguments(ty: &Type, name: &str) -> &'static [&'static str] {
    match ty.type_name() {
        _ if name == "sort" => &["key"],
        Some("Net") if name == "_connect" => &["timeout", "tls"],
        Some("Net") if name == "_listen" => &["tls"],
        // `crlf=True` takes a line as ending only at `\r\n`, as HTTP and SMTP do.
        Some("Connection") if name == "read_line" => &["crlf", "timeout"],
        Some(owner) if waits_on_network(owner, name) => &["timeout"],
        Some("Net") if name == "_udp" => &["port"],
        // `durable=False` puts a whole file in its path's place without waiting for the disk.
        Some("Fs") if matches!(name, "write_text" | "write_bytes") => &["durable"],
        Some("Process") if name == "run" => &["cwd", "env", "input", "most_output", "timeout"],
        Some("Net") if matches!(name, "get" | "post" | "request") => {
            &["body", "headers", "json", "most_body", "params", "timeout"]
        }
        _ => &[],
    }
}

/// Whether a method waits on the network, so it takes `timeout=`, a `Duration`.
fn waits_on_network(type_name: &str, name: &str) -> bool {
    matches!(
        (type_name, name),
        ("Net", "_connect")
            | ("Connection", "read" | "_body" | "read_line" | "_read_head" | "start_tls" | "write")
            | ("Server", "_accept")
            | ("UdpSocket", "receive")
    )
}

/// Methods of capabilities, and of standard library types that are written in Rust.
fn named_method(type_name: &str, name: &str) -> Option<Method> {
    Some(match (type_name, name) {
        ("Stdout", "print" | "write") => reads(&[], Type::Unit).any_args(),
        ("Stdout", "captured") => reads(&[], Type::Str),
        ("Clock", "now") => reads(&[], Type::named("Instant")),
        ("Clock", "time") => reads(&[], Type::named("Time")),
        ("Args", "len") | ("Entropy", "seed") => reads(&[], Type::Int),
        // A test's printed lines, sorted, since tasks print in no fixed order; and standard
        // input's lines.
        ("Stdout", "sorted_lines") | ("Stdin", "lines") => reads(&[], Type::list(Type::Str)),
        // The next line without its ending, `None` at the end, and up to `size` bytes, empty at
        // the end: what makes `Stdin` a `Reader`.
        ("Stdin", "read_line") => reads(&[], Type::result(Type::optional(Type::Str), Type::named("IoError"))),
        ("Stdin", "read_chunk") => reads(&[Type::Int], Type::result(Type::Bytes, Type::named("IoError"))),
        ("Clock", "sleep") => reads(&[Type::named("Duration")], Type::Unit),
        ("Entropy", "bytes") => reads(&[Type::Int], Type::Bytes),
        _ => return stdlib::method(type_name, name),
    })
}

/// What to say of a method only a test's own body can call, on the fake capability it received.
fn test_only(ty: &Type, name: &str) -> Option<&'static str> {
    let Type::Named(owner) = ty else { return None };
    Some(match (owner.as_str(), name) {
        ("Stdout", "captured" | "sorted_lines") => {
            "only a test's own body can read what it printed, from the `Stdout` it received"
        }
        ("Net", "script" | "requests") => {
            "only a test's own body can script or read the fake `Net` it received"
        }
        // Only a test's fake `Process` is scripted, and says what ran.
        ("Process", "script" | "ran") => {
            "only a test's own body can script or read the fake `Process` it received"
        }
        ("Env", "set") => "only a test's own body can set what the fake `Env` it received holds",
        ("Signals", "stop") => "only a test's own body can stop the fake `Signals` it received",
        ("Stderr", "captured") => {
            "only a test's own body can read what it wrote, from the `Stderr` it received"
        }
        _ => return None,
    })
}

/// A method a built-in type doesn't have, with the name it has now if it was renamed: a table's
/// `insert` is `add`. The name comes straight after the value and its dot, as `fennec fmt` writes it.
fn renamed_method(object: &Expr, ty: &Type, name: &str, span: Span) -> Diagnostic {
    let diagnostic = no_method(ty, name, span);
    if name != "insert" || !matches!(ty, Type::Table(_)) {
        return diagnostic;
    }
    let at = object.span.end + 1;
    diagnostic
        .with_help("a table's `insert` is called `add` now")
        .with_fix(Span { start: at, end: at + 6 }, "add")
}

/// A `_` method is its type's own, written in Rust for the standard library's code, as a `_`
/// field is: called from anywhere else, it's refused.
fn private_method(ty: &Type, name: &str, span: Span) -> Option<Diagnostic> {
    if !name.starts_with('_') || span.in_prelude() {
        return None;
    }
    let owner = ty.type_name().unwrap_or_default();
    let message = format!("`{name}` is private to `{owner}`");
    let help = format!(
        "only the standard library's own code can reach `_` names: go through one of `{owner}`'s other methods"
    );
    Some(Diagnostic::new(message, span).with_help(help))
}

impl FnCx<'_> {
    /// `process.run` takes a program and its arguments, or one template.
    fn process_run_given(&mut self, given: &[(Type, Span)], span: Span) {
        // A template with no values says nothing of what its values are, so this does.
        let text_template = Type::Generic("Template".to_owned(), vec![Type::Str]);
        let first = given.first().map(|(ty, _)| self.checker.settled(ty));
        if matches!(&first, Some(Type::Generic(name, _)) if name == "Template")
            && let [(ty, _)] = given
        {
            self.checker.fits(&text_template, ty);
        }
        let open = |ty: &Type| matches!(ty, Type::Unknown | Type::Var(_));
        let fits = match (first.as_ref(), given.len()) {
            (Some(Type::Str), 2) => true,
            (Some(ty), 1) => matches!(ty, Type::Generic(name, _) if name == "Template") || open(ty),
            (Some(ty), 2) => open(ty),
            _ => false,
        };
        if !fits {
            let message =
                "`run` takes the program, then its arguments as a list, never one string a shell would split";
            let help = "like `process.run(\"git\", [\"log\", \"--oneline\"])`, or `process.run(\"ls\", [])` for none, or a template, `process.run(t\"git log {name}\")`";
            self.error(Diagnostic::new(message, span).with_help(help));
        }
    }

    /// Why a builtin method can't be called here, if it can't: it's private to the standard
    /// library, or it's for a test's own fakes and this isn't a test.
    fn refused_method(&self, ty: &Type, name: &str, span: Span) -> Option<Diagnostic> {
        if let Some(private) = private_method(ty, name, span) {
            return Some(private);
        }
        let help = test_only(ty, name).filter(|_| !self.in_a_test())?;
        Some(no_method(ty, name, span).with_help(help))
    }

    pub(crate) fn builtin_method(
        &mut self,
        object: &Expr,
        ty: &Type,
        name: &str,
        args: &[Arg],
        span: Span,
    ) -> Type {
        let Some(method) = method(ty, name) else {
            return self.unusable(renamed_method(object, ty, name, span), args);
        };
        if let Some(refused) = self.refused_method(ty, name, span) {
            return self.unusable(refused, args);
        }
        self.note_capability_wait(ty, name, span);
        if method.changes {
            self.mut_place(object, &format!("`{name}` changes it, so call it on something held in a mut name, like `items.{name}(...)`"));
        }
        if ty.type_name() == Some("Net") && matches!(name, "get" | "post" | "request") {
            self.one_request_body(args, span);
        }
        let named = named_arguments(ty, name);
        let plain = self.builtin_args(name, args, named, (&method.params, &method.receives), span);
        self.no_secret_printed(ty, name, &plain);
        if *ty == Type::named("Process") && name == "run" {
            self.process_run_given(&plain, span);
        }
        if (name == "write" && *ty == Type::named("Connection")
            || name == "send_to" && *ty == Type::named("UdpSocket"))
            && let [(data, at), ..] = plain.as_slice()
            && !matches!(data, Type::Str | Type::Bytes | Type::Unknown | Type::Var(_))
        {
            self.error(
                mismatch("what's written", &Type::Bytes, data, *at)
                    .with_help("a connection sends `bytes`, or a `str` as its UTF-8 bytes"),
            );
        }
        if name == "run" && *ty == Type::named("Process") {
            // Checked by `process_run_given`, above.
        } else if plain.len() < method.arity.0 || plain.len() > method.arity.1 {
            self.error(wrong_args(name, span));
        }
        // `Fs` takes a file's path as a `Path` or as text.
        let path = *ty == Type::named("Fs");
        for ((actual, at), expected) in plain.iter().zip(&method.params) {
            let as_path = path && *expected == Type::Str && *actual == Type::named("Path");
            // A generator of text, which can fail: `join` loops over it here, which passes its
            // failure up.
            let generated =
                name == "join" && matches!(actual, Type::Iterator(item, _) if **item == Type::Str);
            if as_path || generated || self.checker.fits(expected, actual) {
                continue;
            }
            if path && *expected == Type::Str {
                let message =
                    format!("`{name}` takes a path, as a str or a `Path`, but this is {}", a(actual));
                let help = "pass the path as text, as `\"data/scores.txt\"`, or a `Path`";
                self.error(Diagnostic::new(message, *at).with_help(help));
                continue;
            }
            let expected = self.checker.settled(expected);
            self.error(mismatch(&format!("`{name}`'s argument"), &expected, actual, *at));
        }
        // `get` has no default of its own, since an optional's `else` says it.
        if let (Type::Dict(_, value), "get", [(_, key), (_, default)]) = (ty, name, plain.as_slice()) {
            self.error(
                Diagnostic::new("`get` takes only a key", *default)
                    .with_help("write `table.get(key) else default`")
                    .with_fix(Span { start: span.start, end: span.start }, "(")
                    .with_fix(Span { start: key.end, end: default.start }, ") else "),
            );
            return value.as_ref().clone();
        }
        // `pop()` gives `None` for an empty list. `pop(at)` fails on a position that isn't there,
        // as `items[at]` does.
        if let (Type::List(item), "pop", []) = (ty, name, plain.as_slice()) {
            return Type::optional(item.as_ref().clone());
        }
        method.returns
    }

    /// `{name}(other=...)`, where `name` takes only `named` by name.
    fn no_parameter(&mut self, name: &str, other: &str, named: &[&str], at: Span) {
        let help = if named.is_empty() {
            format!("take `{other}=` off: `{name}` takes nothing by name")
        } else {
            named_help(other, named, &format!("take `{other}=` off: `{name}` takes "))
        };
        let message = format!("`{name}` has no parameter called `{other}`");
        self.error(Diagnostic::new(message, at).with_help(help));
    }

    /// Checks a builtin's arguments: none is `mut`, and only those in `named` may be named.
    /// Returns the plain arguments' types and where they are.
    fn builtin_args(
        &mut self,
        name: &str,
        args: &[Arg],
        named: &[&str],
        (params, receives): (&[Type], &[Type]),
        span: Span,
    ) -> Vec<(Type, Span)> {
        let mut plain = Vec::new();
        let mut start = None;
        let mut position = 0;
        for arg in args {
            let ty = self.arg_as(arg, receives, cased_param(arg, params, &mut position));
            match arg.name.as_deref() {
                Some("key") if !named.contains(&"key") => {
                    let message = format!("`{name}` doesn't take `key`");
                    self.error(Diagnostic::new(message, span).with_help("take `key=` off"));
                }
                Some("key") => {}
                Some("start") if named.contains(&"start") => start = Some((ty, arg.value.span)),
                Some("timeout") if named.contains(&"timeout") => {
                    if !self.checker.fits(&Type::named("Duration"), &ty) {
                        self.error(mismatch("`timeout`", &Type::named("Duration"), &ty, arg.value.span));
                    }
                }
                Some("body") if named.contains(&"body") => {
                    if !matches!(self.checker.settled(&ty), Type::Str | Type::Bytes) {
                        self.error(
                            mismatch("what's sent", &Type::Bytes, &ty, arg.value.span)
                                .with_help("a request sends `bytes`, or a `str` as its UTF-8 bytes"),
                        );
                    }
                }
                Some("json") if named.contains(&"json") => self.json_body(&ty, arg.value.span),
                Some("input") if named.contains(&"input") => {
                    if !matches!(self.checker.settled(&ty), Type::Str | Type::Bytes) {
                        self.error(
                            mismatch("what the program reads", &Type::Str, &ty, arg.value.span)
                                .with_help("`input=` is what the program reads from its standard input, as a `str` or `bytes`"),
                        );
                    }
                }
                Some("cwd") if named.contains(&"cwd") => {
                    if !matches!(self.checker.settled(&ty), Type::Str)
                        && !self.checker.fits(&Type::named("Path"), &ty)
                    {
                        self.error(
                            mismatch("`cwd`", &Type::Str, &ty, arg.value.span).with_help(
                                "`cwd=` is the folder the program runs in, as a `Path` or a `str`",
                            ),
                        );
                    }
                }
                Some(option @ ("headers" | "params" | "env")) if named.contains(&option) => {
                    let wanted = Type::Dict(Box::new(Type::Str), Box::new(Type::Str));
                    if !self.checker.fits(&wanted, &ty) {
                        self.error(mismatch(&format!("`{option}`"), &wanted, &ty, arg.value.span));
                    }
                }
                Some("tls") if named.contains(&"tls") => self.tls_option(name, &ty, arg.value.span),
                Some(option @ ("crlf" | "durable")) if named.contains(&option) => {
                    if !self.checker.fits(&Type::Bool, &ty) {
                        self.error(mismatch(&format!("`{option}`"), &Type::Bool, &ty, arg.value.span));
                    }
                }
                Some(option @ ("indent" | "level" | "port" | "most_output" | "most_body" | "most"))
                    if named.contains(&option) =>
                {
                    if !self.checker.fits(&Type::Int, &ty) {
                        self.error(mismatch(&format!("`{option}`"), &Type::Int, &ty, arg.value.span));
                    }
                }
                Some(option @ ("names" | "extra")) if named.contains(&option) => {
                    self.json_option(option, &ty, arg.value.span);
                }
                Some(other) => self.no_parameter(name, other, named, arg.value.span),
                None => plain.push((ty, arg.value.span)),
            }
        }
        if args.iter().any(|arg| arg.is_mut) {
            self.error(needless_mut(name, span));
        }
        plain.extend(start);
        plain
    }

    fn one_request_body(&mut self, args: &[Arg], span: Span) {
        let given = |option: &str| args.iter().any(|arg| arg.name.as_deref() == Some(option));
        if given("body") && given("json") {
            let message = "a request sends `body=` or `json=`, not both";
            self.error(
                Diagnostic::new(message, span).with_help("`json=` sends a value as JSON, as its body"),
            );
        }
    }

    /// `tls=`: whether `connect` uses TLS, and the `Certificate` a server shows, if any.
    fn tls_option(&mut self, method: &str, ty: &Type, span: Span) {
        let wanted =
            if method == "_connect" { Type::Bool } else { Type::optional(Type::named("Certificate")) };
        if !self.checker.fits(&wanted, ty) {
            self.error(mismatch("`tls`", &wanted, ty, span));
        }
    }

    /// `names=` and `extra=` take a case of `JsonNames` and `JsonExtra`, like `.CamelCase`.
    pub(crate) fn json_option(&mut self, option: &str, ty: &Type, span: Span) {
        let expected = Type::named(if option == "names" { "JsonNames" } else { "JsonExtra" });
        if !self.checker.fits(&expected, ty) {
            let cases = if option == "names" {
                "`.SnakeCase`, `.CamelCase`, `.PascalCase` or `.KebabCase`"
            } else {
                "`.Error` or `.Ignore`"
            };
            self.error(
                mismatch(&format!("`{option}`"), &expected, ty, span).with_help(format!("write {cases}")),
            );
        }
    }

    pub(crate) fn call_builtin(&mut self, name: &str, args: &[Arg], span: Span) -> Type {
        if name == "copy" && args.len() > 1 {
            return self.replace(args, span);
        }
        if let ("copy", [arg]) = (name, args)
            && arg.name.is_none()
        {
            let ty = self.expr(&arg.value);
            self.error(plain_copy(span, arg.value.span));
            return ty;
        }
        if name == "replace" {
            // The callee is the name the call starts with.
            let callee = Span { start: span.start, end: span.start + 7 };
            self.error(
                Diagnostic::new("`replace` is called `copy` now: `copy(value, field=new)`", callee)
                    .with_help("`fennec fix` writes `copy`")
                    .with_fix(callee, "copy"),
            );
            return self.replace(args, span);
        }
        // An arrow function passed as `key` receives the items of the first argument.
        let first = args
            .iter()
            .find(|arg| arg.name.is_none())
            .map_or(Type::Unknown, |arg| self.peek_type(&arg.value));
        let item = peek_item(&first);
        let named: &[&str] = match (name, stdlib::function(name)) {
            ("min" | "max" | "sorted", _) => &["key"],
            ("sum", _) => &["start"],
            (_, Some((_, function))) => function.named,
            _ => &[],
        };
        let sized = self.sized_among(name, args);
        let plain = self.builtin_args(name, args, named, (&sized, std::slice::from_ref(&item)), span);
        let (low, high) = match stdlib::function(name) {
            Some((_, function)) => function.arity,
            None => BUILTINS
                .iter()
                .find(|(builtin, ..)| *builtin == name)
                .map_or((0, 0), |(_, low, high)| (*low, *high)),
        };
        if plain.len() < low || plain.len() > high {
            self.error(wrong_args(name, span));
            return Type::Unknown;
        }
        if name == "sum" {
            return self.sum(args, &plain, span);
        }
        // `u8(300)`: a number written out that can't fit is an error now, as `x: u8 = 300` is.
        if let Some(num) = crate::types::Num::named(name)
            && let Some(arg) = args.iter().find(|arg| arg.name.is_none())
        {
            self.literal_fits(num, &arg.value);
        }
        if name == "str" {
            for (given, at) in &plain {
                self.shown(given, "`str` of this", *at);
            }
        }
        let result = self.builtin_result(name, &plain, span);
        let keyed = args.iter().any(|arg| arg.name.as_deref() == Some("key"));
        if matches!(name, "min" | "max") && !keyed {
            self.needs_order(name, &result, span);
        }
        result
    }

    /// For `min` and `max` of two or more values, the sized number one of them is, given for
    /// each, so a number literal among them takes that size: `max(speed, 0.0)` for an `f32`
    /// `speed`. Nothing for any other builtin.
    fn sized_among(&mut self, name: &str, args: &[Arg]) -> Vec<Type> {
        let plain: Vec<&Arg> = args.iter().filter(|arg| arg.name.is_none()).collect();
        if !matches!(name, "min" | "max") || plain.len() < 2 {
            return Vec::new();
        }
        let mut sized = None;
        for arg in &plain {
            if !crate::expect::number_literal(&arg.value) {
                let ty = self.peek_type(&arg.value);
                if let ty @ Type::Sized(_) = self.checker.settled(&ty) {
                    sized = Some(ty);
                    break;
                }
            }
        }
        sized.map_or_else(Vec::new, |ty| vec![ty; plain.len()])
    }

    /// A number written out, or one negated, given to `u8(...)` and the like, which has to fit.
    fn literal_fits(&mut self, num: crate::types::Num, value: &Expr) {
        use fennec_syntax::{ExprKind, UnaryOp};
        let written = match &value.kind {
            ExprKind::Int(n) => i128::from(*n),
            ExprKind::Unary { op: UnaryOp::Neg, expr } if let ExprKind::Int(n) = expr.kind => -i128::from(n),
            _ => return,
        };
        let Some((least, most)) = num.range() else { return };
        if written < least || written > most {
            let ty = Type::Sized(num);
            let message = format!("{written} doesn't fit in {ty}");
            let help = format!(
                "{} holds {least} to {most}: write one in that range, or use a wider type, as `int`",
                a(&ty)
            );
            self.error(Diagnostic::new(message, value.span).with_help(help));
        }
    }

    /// `min` and `max` compare the items with `<`, so the items need an order.
    fn needs_order(&mut self, name: &str, item: &Type, span: Span) {
        let item = self.checker.settled(item);
        if item.is_known()
            && let Err((why, fix)) = self.fits_bound(&item, "Ordered")
        {
            let message = format!("`{name}` compares its items with `<`, but {why}");
            self.error(Diagnostic::new(message, span).with_help(fix));
        }
    }

    fn builtin_result(&mut self, name: &str, plain: &[(Type, Span)], span: Span) -> Type {
        let needs_values =
            stdlib::function(name).map_or(NEEDS_VALUES.contains(&name), |(_, f)| f.needs_values);
        if needs_values && !plain.iter().all(|(ty, at)| self.usable(ty, *at)) {
            return Type::Unknown;
        }
        if let Some((module, _)) = stdlib::function(name) {
            return (module.result)(self, name, plain, span);
        }
        let types: Vec<Type> = plain.iter().map(|(ty, _)| ty.clone()).collect();
        let (first, at) = plain.first().cloned().unwrap_or((Type::Unit, span));
        match name {
            "copy" => first,
            "Ok" => {
                if crate::gone::written_by_hand(span) {
                    self.error(crate::gone::ok_call(span, plain.first().map(|(_, value)| *value)));
                }
                Type::result(first, self.checker.fresh(span, Hole::OkError))
            }
            "Err" => {
                if crate::gone::written_by_hand(span) {
                    let returned = self.returned_errs.get(&(span.start, span.end)).copied();
                    self.error(crate::gone::err_call(span, at, returned));
                } else if !self.checker.program.can_fail_with(&self.checker.settled(&first)) {
                    self.error(crate::program::not_an_error(
                        "`Err` holds an error, as `fail` does",
                        &first,
                        at,
                    ));
                }
                Type::result(self.checker.fresh(span, Hole::ErrValue), first)
            }
            "range" => {
                if !types.iter().all(|ty| self.checker.fits(&Type::Int, ty)) {
                    self.error(
                        Diagnostic::new("`range` takes one to three ints", span)
                            .with_help("give it ints, as `range(10)`, `range(2, 10)` or `range(0, 10, 2)`"),
                    );
                }
                Type::list(Type::Int)
            }
            // The value as it shows inside another, a failure as what it holds rather than its
            // words, and text in quotes.
            "str" | "repr" => Type::Str,
            "bytes" => {
                if let Some((items, at)) = plain.first()
                    && !self.checker.fits(&Type::list(Type::Int), items)
                {
                    self.error(mismatch("`bytes`'s argument", &Type::list(Type::Int), items, *at));
                }
                Type::Bytes
            }
            name if let Some(num) = crate::types::Num::named(name) => self.sized_conversion(num, &first, at),
            "int" | "float" | "i64" | "f64" => {
                // `i64` is `int` and `f64` is `float`, so they're their conversions, and what's built
                // from them is written as theirs.
                let plain = if matches!(name, "int" | "i64") { "int" } else { "float" };
                if plain != name {
                    self.checker.std_calls.insert((span.start, span.end), plain.to_owned());
                }
                if !matches!(
                    first,
                    Type::Sized(_) | Type::Int | Type::Float | Type::Str | Type::Bool | Type::Unknown
                ) {
                    let message = format!("`{name}` converts a number or text, not {}", a(&first));
                    let help = match self.checker.settled(&first) {
                        Type::List(_) | Type::Set(_) => {
                            format!("convert each item, as `[{name}(item) for item in items]`")
                        }
                        _ => format!("pass a number or text, as `{name}(\"42\")`"),
                    };
                    self.error(Diagnostic::new(message, at).with_help(help));
                }
                let number = if plain == "int" { Type::Int } else { Type::Float };
                // Text might not be a number, so reading one can fail.
                if self.checker.settled(&first) == Type::Str {
                    return Type::result(number, Type::named("InvalidValue"));
                }
                number
            }
            "round" => self.math(name, &types, span),
            _ => self.collection_builtin(name, plain, span),
        }
    }

    /// `f32(x)`, the `f32` nearest a number, or `u8(x)`, an integer of another size as this one,
    /// which stops the program if it doesn't fit. What comes from outside goes through
    /// `u8.from(x)?` or `u8.parse(s)?`.
    fn sized_conversion(&mut self, num: crate::types::Num, first: &Type, at: Span) -> Type {
        let name = num.name();
        if num.is_float() {
            // std's `f32.parse` reads text straight to the nearest `f32`, so it's rounded once:
            // written over this, as `int.parse` is over `int(text)`. A program writes `.parse`.
            if *first == Type::Str && at.in_prelude() {
                return Type::result(Type::Sized(num), Type::named("InvalidValue"));
            }
            if !matches!(first, Type::Int | Type::Float | Type::Sized(_) | Type::Unknown) {
                let message = format!("`{name}(...)` converts a number, not {}", a(first));
                let help = if *first == Type::Str {
                    format!("read the text with `{name}.parse`, as `{name}.parse(text)?`")
                } else {
                    format!("pass a number, as `{name}(1.5)`")
                };
                self.error(Diagnostic::new(message, at).with_help(help));
            }
            return Type::Sized(num);
        }
        let integer =
            matches!(first, Type::Int | Type::Unknown) || matches!(first, Type::Sized(n) if !n.is_float());
        if !integer {
            let message = format!("`{name}(...)` converts an integer, not {}", a(first));
            let help = match self.checker.settled(first) {
                Type::Str => format!("read the text with `{name}.parse`, as `{name}.parse(text)?`"),
                Type::Float => {
                    format!("make it a whole number first: `{name}(round(x))` or `{name}(int(x))`")
                }
                _ => format!("give `{name}` an integer"),
            };
            self.error(Diagnostic::new(message, at).with_help(help));
        }
        Type::Sized(num)
    }

    /// `round` and `std.math`'s functions, which take numbers. `sqrt`, `sin`, `cos` and `acos` take
    /// an `f32` too, and give one, computed in `f32`.
    pub(crate) fn math(&mut self, name: &str, types: &[Type], span: Span) -> Type {
        let first = types.first().cloned().unwrap_or(Type::Unknown);
        let f32_math = matches!(name, "math.sqrt" | "math.sin" | "math.cos" | "math.acos");
        if f32_math && self.checker.settled(&first) == Type::Sized(crate::types::Num::F32) && types.len() == 1
        {
            return first;
        }
        let number = matches!(first, Type::Int | Type::Float | Type::Unknown);
        let digits = types.get(1).is_none_or(|digits| self.checker.fits(&Type::Int, digits));
        if !number || !digits {
            let message = format!("`{name}` takes a number, but this is {}", a(&first));
            let help = if first == Type::Str {
                format!("read the text as a number with `float.parse`, as `{name}(float.parse(text)?)`")
            } else {
                format!("pass a number, as `{name}(2.5)`")
            };
            self.error(Diagnostic::new(message, span).with_help(help));
            return Type::Unknown;
        }
        match name {
            "round" if types.len() == 2 => Type::Float,
            "round" | "math.floor" | "math.ceil" => Type::Int,
            _ => Type::Float,
        }
    }

    /// Whether a generator that can fail goes into `enumerate` or `zip` anywhere but right in a
    /// `for` loop's own, as `for at, item in enumerate(made()?)`, reporting each that does. Only
    /// there does the loop own the generator and pass its failure up.
    fn failing_unlooped(&mut self, name: &str, plain: &[(Type, Span)]) -> bool {
        let mut found = false;
        for (ty, at) in plain {
            if self.checker.settled(ty).generator_failure().is_some() && !self.looped_here.contains(at) {
                let message = format!(
                    "a generator that can fail goes into `{name}` only as `made()?` written right in a `for` loop's own `{name}`"
                );
                let help = format!(
                    "write `for ... in {name}(made()?, ...):` there, or keep its items with `list(...)` first"
                );
                self.error(Diagnostic::new(message, *at).with_help(help));
                found = true;
            }
        }
        found
    }

    /// The builtins that work on the items of a collection.
    fn collection_builtin(&mut self, name: &str, plain: &[(Type, Span)], span: Span) -> Type {
        let types: Vec<Type> = plain.iter().map(|(ty, _)| ty.clone()).collect();
        let Some((first, at)) = plain.first().cloned() else {
            // Only `set()` takes nothing.
            return Type::Set(Box::new(self.checker.fresh(span, Hole::EmptySet)));
        };
        let items = |cx: &mut Self| cx.item_type(&first, at);
        match name {
            "min" | "max" if types.len() > 1 => {
                self.join_or_report(&types, span, &format!("`{name}` is given"))
            }
            "min" | "max" => items(self),
            "any" | "all" => {
                let item = items(self);
                if !self.checker.fits(&Type::Bool, &item) {
                    self.error(
                        Diagnostic::new(
                            format!("`{name}` needs bools, but got {}", article(&item.kind())),
                            span,
                        )
                        .with_help(
                            "Fennec has no truthiness: compare explicitly, like `any(x > 0 for x in xs)`",
                        ),
                    );
                }
                Type::Bool
            }
            "reversed" if first == Type::Str => Type::Str,
            "parallel" => {
                self.note_wait("`parallel`", span);
                Type::list(items(self))
            }
            "sorted" | "list" | "reversed" => Type::list(items(self)),
            "set" => Type::Set(Box::new(items(self))),
            "enumerate" | "zip" if self.failing_unlooped(name, plain) => Type::Unknown,
            "enumerate" => {
                let pairs = Type::Tuple(vec![Type::Int, items(self)]);
                if plain.iter().any(|(ty, _)| matches!(ty, Type::Iterator(..))) {
                    Type::iterator(pairs)
                } else {
                    Type::list(pairs)
                }
            }
            "zip" => {
                let first = items(self);
                let second = plain.get(1).map_or(Type::Unknown, |(ty, at)| self.item_type(ty, *at));
                let pairs = Type::Tuple(vec![first, second]);
                if plain.iter().any(|(ty, _)| matches!(ty, Type::Iterator(..))) {
                    Type::iterator(pairs)
                } else {
                    Type::list(pairs)
                }
            }
            _ => Type::Unknown,
        }
    }

    /// `bytes.fromhex(text)`: the bytes that hex digits spell, which fails for text that isn't
    /// pairs of them.
    pub(crate) fn bytes_from_hex(&mut self, args: &[Arg], span: Span) -> Type {
        let plain = self.builtin_args("fromhex", args, &[], (&[], &[]), span);
        match plain.as_slice() {
            [(text, at)] => {
                if !self.checker.fits(&Type::Str, text) {
                    self.error(mismatch("the hex digits", &Type::Str, text, *at));
                }
            }
            _ => self.error(wrong_args("fromhex", span)),
        }
        Type::result(Type::Bytes, Type::named("InvalidValue"))
    }

    /// `str.from_code(n)`: the character with a code, which fails for a number that isn't one.
    pub(crate) fn char_from_code(&mut self, args: &[Arg], span: Span) -> Type {
        let plain = self.builtin_args("from_code", args, &[], (&[], &[]), span);
        match plain.as_slice() {
            [(code, at)] => {
                if !self.checker.fits(&Type::Int, code) {
                    self.error(mismatch("the code", &Type::Int, code, *at));
                }
            }
            _ => self.error(wrong_args("from_code", span)),
        }
        Type::result(Type::Str, Type::named("InvalidValue"))
    }

    /// `copy(value, field=new, ...)`: a copy of a struct with some fields changed, each new
    /// value the type its field is.
    fn replace(&mut self, args: &[Arg], span: Span) -> Type {
        let types: Vec<Type> = args.iter().map(|arg| self.arg_value(arg, &[])).collect();
        let Some(ty) = types.first() else {
            self.error(
                Diagnostic::new("`copy` with fields takes a struct value, then the fields to change", span)
                    .with_help("like `copy(enemy, health=50)`"),
            );
            return Type::Unknown;
        };
        if args.iter().any(|arg| arg.is_mut) {
            self.error(needless_mut("copy", span));
        }
        let Some(type_name) = ty.type_name() else {
            if ty.is_known() {
                let message = format!("`copy` with fields copies a struct, but this is {}", a(ty));
                let help = "write the new value itself, as `count = 5`: `copy` with fields is for structs, as `copy(point, x=1)`";
                self.error(Diagnostic::new(message, span).with_help(help));
            }
            return ty.clone();
        };
        for (arg, actual) in args.iter().zip(&types).skip(1) {
            let Some(name) = &arg.name else {
                self.error(Diagnostic::new("name the field to change, like `health=50`", arg.value.span));
                continue;
            };
            self.private_member(type_name, name, arg.value.span);
            let has_field = self
                .checker
                .program
                .type_info(type_name)
                .is_some_and(|info| info.def.fields.iter().any(|field| &field.name == name));
            if !has_field {
                let fields: Vec<String> = self
                    .checker
                    .program
                    .type_info(type_name)
                    .map(|info| info.def.fields.iter().map(|field| field.name.clone()).collect())
                    .unwrap_or_default();
                let fields: Vec<&str> = fields.iter().map(String::as_str).collect();
                let help = named_help(name, &fields, "change one of its fields: ");
                self.error(
                    Diagnostic::new(format!("`{type_name}` has no field called `{name}`"), arg.value.span)
                        .with_help(help),
                );
                continue;
            }
            let expected = self
                .checker
                .field_type(type_name, None, name)
                .substitute(&self.checker.program.type_args(ty));
            if !self.checker.fits(&expected, actual) {
                self.error(mismatch(&format!("`{name}`"), &expected, actual, arg.value.span));
            }
        }
        ty.clone()
    }
}

/// The item type of a value looped over, without reporting anything.
fn peek_item(ty: &Type) -> Type {
    match ty {
        Type::List(item)
        | Type::Buffer(item)
        | Type::Set(item)
        | Type::Iterator(item, _)
        | Type::Table(item)
        | Type::Dict(item, _) => item.as_ref().clone(),
        Type::Tuple(items) if items.windows(2).all(|pair| pair.first() == pair.last()) => {
            items.first().cloned().unwrap_or(Type::Unknown)
        }
        Type::Str => Type::Str,
        Type::Bytes => Type::Int,
        _ => Type::Unknown,
    }
}

/// The help for `given=` where only `options` can be given by name: the one it's spelled most
/// like, or `otherwise` and then each of them.
fn named_help(given: &str, options: &[&str], otherwise: &str) -> String {
    if let Some(similar) = crate::messages::spelled_most_like(given, options.iter().copied()) {
        return format!("did you mean `{similar}=`?");
    }
    let shown: Vec<String> = options.iter().map(|option| format!("`{option}=`")).collect();
    format!("{otherwise}{}", shown.join(", "))
}

/// The parameter's type a builtin method's argument is given, for a leading-dot case or a
/// collection written with one, which is a case of the enum that type says, as `.Broken("bulb")`
/// is for a `list[Light]`'s `append`. Counts `position` along the positional arguments.
fn cased_param<'p>(arg: &Arg, params: &'p [Type], position: &mut usize) -> Option<&'p Type> {
    if arg.name.is_some() {
        return None;
    }
    *position += 1;
    let cased = crate::expect::leading_dot(&arg.value) || crate::expect::written_with_cases(&arg.value);
    let sized = |param: &&Type| matches!(param, Type::Sized(_)) && crate::expect::number_literal(&arg.value);
    params.get(*position - 1).filter(|param| cased || sized(param))
}

#[cfg(test)]
mod declared {
    //! What std declares the engines provide, held to the checker's tables, so a method's
    //! declaration, which hover and the docs show, says what the checker takes and gives.

    use fennec_syntax::{EngineDeclaration, FnDef, TypeExpr};

    use super::{Method, method, named_arguments};
    use crate::types::Type;

    /// Declarations that say what a program can write where the table says more, each with why.
    /// A declaration can't say an argument is given by position and can be left out: one with a
    /// default is passed by name. So each such method declares one form, and its doc the other,
    /// until the checker reads its signatures from the declarations (#2214).
    const AS_CALLED: [(&str, &str, &str); 4] = [
        (
            "list",
            "pop",
            "`pop()` gives `T?` and `pop(at)` gives `T`, which the checker works out from the call: the declaration says the first, and its doc both",
        ),
        (
            "str",
            "split",
            "`separator` is given by position and can be left out: the declaration says it's given, and its doc what's done without it",
        ),
        (
            "bytes",
            "hex",
            "`sep` is given by position and can be left out: the declaration says it's given, and its doc what's done without it",
        ),
        (
            "dict",
            "get",
            "the table takes a default only to refuse it with a fix, `get(key) else default`: `get` takes a key",
        ),
    ];

    /// A type as a declaration writes it, as the checker shows its own.
    fn written(ty: &TypeExpr) -> String {
        let args: Vec<String> = ty.args.iter().map(written).collect();
        let shown = match (ty.name.as_str(), args.split_last()) {
            ("|", _) => args.join(" | "),
            ("tuple", _) if args.len() == 1 => format!("({},)", args.join("")),
            ("tuple", _) => format!("({})", args.join(", ")),
            ("->", Some((returns, params))) => format!("({}) -> {returns}", params.join(", ")),
            ("fails", _) => args.join(" fails "),
            (name, _) if args.is_empty() => name.to_owned(),
            (name, _) => format!("{name}[{}]", args.join(", ")),
        };
        if ty.optional { format!("{shown}?") } else { shown }
    }

    /// The type a method declared on `owner` is called on: a built-in type with its parameters
    /// as written, or a type by its name.
    fn receiver(owner: &str, generics: &[fennec_syntax::Generic]) -> Type {
        let param = |at: usize| {
            Box::new(Type::Param(generics.get(at).map_or("T", |generic| &generic.name).to_owned()))
        };
        match owner {
            "str" => Type::Str,
            "bytes" => Type::Bytes,
            "int" => Type::Int,
            "float" => Type::Float,
            "list" => Type::List(param(0)),
            "set" => Type::Set(param(0)),
            "dict" => Type::Dict(param(0), param(1)),
            "Buffer" => Type::Buffer(param(0)),
            "Table" => Type::Table(param(0)),
            other => Type::named(other),
        }
    }

    /// The types an arrow among a declaration's parameters receives, as `remove_where`'s test does.
    fn received(def: &FnDef) -> Vec<String> {
        def.params
            .iter()
            .filter_map(|param| param.ty.as_ref())
            .find_map(|ty| {
                // `((T) -> K)?`, an arrow that can be left out, is held in parentheses.
                let arrow = if ty.name == "tuple" && ty.args.len() == 1 { &ty.args[0] } else { ty };
                let (_, params) = arrow.args.split_last().filter(|_| arrow.name == "->")?;
                Some(params.iter().map(written).collect())
            })
            .unwrap_or_default()
    }

    /// What's wrong with one declaration against the checker's table, if anything.
    fn mismatch(declared: &EngineDeclaration, found: &Method, ty: &Type) -> Option<String> {
        let def = &declared.def;
        let (taken_self, params) = def.params.split_first()?;
        if !taken_self.is_self {
            return Some("a method's declaration takes `self` first".to_owned());
        }
        if taken_self.is_mut != found.changes {
            let wanted = if found.changes {
                "`ref self`, since it changes it"
            } else {
                "`self`, since it doesn't change it"
            };
            return Some(format!("it should take {wanted}"));
        }
        let arrow_takes: Vec<String> = found.receives.iter().map(ToString::to_string).collect();
        if received(def) != arrow_takes {
            return Some(format!("its arrow should receive ({})", arrow_takes.join(", ")));
        }
        let returns = match &found.returns {
            Type::Result(value, error) => (value.as_ref(), Some(error.to_string())),
            other => (other, None),
        };
        let declared_returns = def.ret.as_ref().map_or_else(|| "nothing".to_owned(), written);
        let wanted_returns =
            if *returns.0 == Type::Unit { "nothing".to_owned() } else { returns.0.to_string() };
        if declared_returns != wanted_returns || def.fails.as_ref().map(written) != returns.1 {
            let fails = returns.1.map_or_else(String::new, |error| format!(" and fail with {error}"));
            return Some(format!("it should give back {wanted_returns}{fails}"));
        }
        // Any arguments at all, as an arrow and what goes with it: only what it receives is checked.
        if found.arity.1 == usize::MAX {
            return None;
        }
        let plain = found.params.len();
        let Some((positional, named)) = (plain <= params.len()).then(|| params.split_at(plain)) else {
            return Some(format!("it takes {plain} arguments before any named ones"));
        };
        for (param, wanted) in positional.iter().zip(&found.params) {
            // One with a default is passed by name, which none of the table's plain arguments is.
            if param.default.is_some() {
                return Some(format!(
                    "`{}` is given by position, so it's declared with no default",
                    param.name
                ));
            }
            let shown = param.ty.as_ref().map(written).unwrap_or_default();
            if shown != wanted.to_string() {
                return Some(format!("`{}` should be {wanted}, not {shown}", param.name));
            }
        }
        if found.arity.0 != plain {
            return Some(format!(
                "{} of its arguments can be left out by position, which a declaration can't say: list it in AS_CALLED",
                plain - found.arity.0
            ));
        }
        let mut declared_names: Vec<&str> = named.iter().map(|param| param.name.as_str()).collect();
        let mut wanted: Vec<&str> = named_arguments(ty, &def.name).to_vec();
        declared_names.sort_unstable();
        wanted.sort_unstable();
        if declared_names != wanted || named.iter().any(|param| param.default.is_none()) {
            return Some(format!("its named arguments should be {wanted:?}, each with a default"));
        }
        None
    }

    /// Each method std declares the engines provide is one the checker has, and says what the
    /// checker takes and gives: whether it changes what it's called on, its arguments' types,
    /// which can be left out, its named arguments, what an arrow it's given receives, and what it
    /// gives back and fails with.
    #[test]
    fn each_engine_method_std_declares_is_the_checkers() {
        let declared = fennec_syntax::engine_declarations().unwrap();
        let methods: Vec<&EngineDeclaration> =
            declared.iter().filter(|declaration| declaration.owner.is_some()).collect();
        assert!(methods.len() > 40, "only {} engine methods are declared", methods.len());
        let mut wrong = Vec::new();
        for declaration in methods {
            let Some((owner, generics)) = &declaration.owner else { continue };
            let ty = receiver(owner, generics);
            let name = &declaration.def.name;
            if AS_CALLED.iter().any(|(called_on, called, _)| called_on == owner && called == name) {
                continue;
            }
            let problem = match method(&ty, name) {
                None => Some("the checker has no such method".to_owned()),
                Some(found) => mismatch(declaration, &found, &ty),
            };
            if let Some(problem) = problem {
                wrong.push(format!("{}.fen: {owner}.{name}: {problem}", declaration.file));
            }
        }
        assert!(wrong.is_empty(), "declarations that aren't what the checker has:\n{}", wrong.join("\n"));
    }
}
