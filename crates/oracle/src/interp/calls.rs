//! Calls: working out what's called and with what, then calling it. The two are apart so an
//! `except` handles only the failure of the call it follows, never one passed up by a `?` in its
//! arguments.

use fennec_check::Type;
use fennec_syntax::{Arg, CompKind, Expr, ExprKind, Field, FnDef, Span, TypeExpr};

use super::typed::Bound;
use super::{Binding, Flow, Interp, MOST_ITEMS, Place, Run, crash, unsupported};
use crate::load::Kind;
use crate::value::{Func, Held, Text, Value};

/// An argument as it's passed: a value, the place a `ref` argument names, or a generator written
/// straight into a builtin's call, which reads it there.
#[derive(Debug)]
pub(crate) enum ArgVal {
    Value(Option<String>, Value),
    /// A `None`, or one held in `Some`s, that the checker typed as this many levels of optional,
    /// so a parameter or field written with more holds it in a `Some` for each.
    Optional(Option<String>, Value, usize),
    Ref(Option<String>, Place),
    Lazy(Option<String>, &'static Expr),
}

impl ArgVal {
    pub(crate) fn name(&self) -> Option<&str> {
        match self {
            ArgVal::Value(name, _)
            | ArgVal::Optional(name, _, _)
            | ArgVal::Ref(name, _)
            | ArgVal::Lazy(name, _) => name.as_deref(),
        }
    }
}

/// What a method is called on: a place it may change, or a value.
#[derive(Debug)]
pub(crate) enum Receiver {
    Place(Place),
    Value(Value),
}

/// What a call calls.
pub(crate) enum Target {
    Def {
        def: &'static FnDef,
        parent: Option<u64>,
        receiver: Option<Receiver>,
        /// What its type parameters stand for at this call.
        types: Bound,
    },
    /// `value.serialize(ref out)` of a value whose type has no `def serialize` of its own, which
    /// the oracle models from the type, as `saving.rs` says.
    Serialize {
        value: Value,
        ty: Type,
    },
    /// `T.deserialize(ref src)` of a type with no `def deserialize` of its own, as `saving.rs`
    /// models it.
    Deserialize(Type),
    Lambda {
        params: Vec<String>,
        body: &'static Expr,
        captured: Vec<(String, Value)>,
    },
    Construct {
        ty: String,
        case: Option<String>,
    },
    Builtin(String),
    /// A built-in method, of a `Buffer` when `buffer` says, which is held as a list.
    Method {
        receiver: Receiver,
        name: String,
        buffer: bool,
    },
    /// A function of a built-in type, as `int.parse`.
    Static {
        ty: String,
        name: String,
    },
    /// A value already worked out.
    Given(Value),
}

/// The built-in types' names, as `int.parse` names one.
const BUILT_IN_TYPES: &[&str] = &[
    "int", "float", "str", "bool", "bytes", "list", "dict", "set", "i8", "i16", "i32", "i64", "u8", "u16",
    "u32", "u64", "f32", "f64",
];

/// A name, or names joined by dots, as `std.collections` is written: what an import binds.
fn dotted(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Name(name) => Some(name.clone()),
        ExprKind::Attr { object, name, optional: false } => Some(format!("{}.{name}", dotted(object)?)),
        _ => None,
    }
}

/// The standard library's readers and writers the runtime has in Rust, each with the one
/// written in Fennec beside it that the oracle runs in its place.
const NATIVE_FORMATS: &[(&str, &str)] =
    &[("std.toml.dumps", "std.toml.native_dumps"), ("std.toml.loads", "std.toml.native_loads")];

/// The builtin functions the oracle models, and the ones it knows it doesn't.
const BUILTINS: &[&str] = &[
    "len",
    "abs",
    "str",
    "repr",
    "int",
    "float",
    "range",
    "enumerate",
    "zip",
    "sorted",
    "reversed",
    "min",
    "max",
    "sum",
    "any",
    "all",
    "list",
    "set",
    "copy",
    "round",
    "i8",
    "i16",
    "i32",
    "i64",
    "u8",
    "u16",
    "u32",
    "u64",
    "f32",
    "f64",
    "parallel",
    "bytes",
    "Ok",
    "Err",
    "replace",
    "embed_file",
    "embed_folder",
    "failure_of",
];

impl Interp {
    pub(crate) fn call(
        &mut self,
        expr: &'static Expr,
        callee: &'static Expr,
        args: &'static [Arg],
    ) -> Run<Value> {
        if let Some(counted) = self.length_in_place(callee, args, expr.span)? {
            return Ok(counted);
        }
        self.call_direct(expr, callee, args)
    }

    /// A call worked out and made, with any part read through a type's own `def []` to be
    /// changed written back once it's done.
    pub(crate) fn call_direct(
        &mut self,
        expr: &'static Expr,
        callee: &'static Expr,
        args: &'static [Arg],
    ) -> Run<Value> {
        let mark = self.write_backs.len();
        match self.prepare(expr, callee, args) {
            Ok((target, given)) => {
                let lent = lent_here(args, &given);
                let ended = self.invoke_settled(mark, target, given, expr);
                // What a `ref` lent out of a struct here comes back when the call ends, however it
                // ends but by a crash, and that struct's `where` rules run on it then: a broken one
                // stops the program before a failure passes up.
                if matches!(ended, Ok(_) | Err(Flow::Fail(..))) {
                    for place in &lent {
                        self.rules_along(place, expr.span)?;
                    }
                }
                ended
            }
            Err(failed) => self.settled(mark, Err(failed), expr.span),
        }
    }

    /// Makes a call worked out since `mark`, and writes back what it changed through a type's
    /// own `def []=`. A call that can't change what it reached writes nothing back, however it
    /// ended.
    pub(crate) fn invoke_settled(
        &mut self,
        mark: usize,
        target: Target,
        given: Vec<ArgVal>,
        expr: &'static Expr,
    ) -> Run<Value> {
        let changes = match self.changes_what_it_reaches(&target, &given, expr.span)? {
            Some(changes) => changes,
            // Nothing read through a type's own `def []` waits to be written back, so it doesn't
            // matter.
            None if self.write_backs.len() <= mark => false,
            None => {
                let name = match &target {
                    Target::Method { name, .. } => name.as_str(),
                    _ => "",
                };
                return unsupported(format!(
                    "the method `{name}` through a type's own `def []`, not known to change its value or only read it"
                ));
            }
        };
        let called = self.invoke(target, given, expr);
        if !changes {
            self.settle_write_backs(mark, false, expr.span)?;
        }
        self.settled(mark, called, expr.span)
    }

    /// Whether a call can change what it reaches through places: a method that takes `self` as a
    /// `ref`, a built-in method that changes its value, or a `ref` argument. `None` for a built-in
    /// method on a place that's neither known to change its value nor known only to read it.
    fn changes_what_it_reaches(
        &mut self,
        target: &Target,
        given: &[ArgVal],
        span: Span,
    ) -> Run<Option<bool>> {
        if given.iter().any(|arg| matches!(arg, ArgVal::Ref(..))) {
            return Ok(Some(true));
        }
        Ok(match target {
            Target::Def { def, receiver: Some(Receiver::Place(_)), .. } => {
                Some(super::own_index::takes_ref_self(def))
            }
            Target::Method { receiver: Receiver::Place(place), name, .. } => {
                if super::methods::changes(name) {
                    Some(true)
                } else if self.peek_place(place, span, |held| super::methods::reads(held, name))? {
                    Some(false)
                } else {
                    None
                }
            }
            _ => Some(false),
        })
    }

    /// Works out what a call calls and its arguments, left to right, without calling it.
    pub(crate) fn prepare(
        &mut self,
        expr: &'static Expr,
        callee: &'static Expr,
        args: &'static [Arg],
    ) -> Run<(Target, Vec<ArgVal>)> {
        let target = match &callee.kind {
            ExprKind::Name(name) => self.named_target(name, callee.span)?,
            ExprKind::Attr { object, name, optional: false } => {
                match self.attr_target(object, name, args, expr)? {
                    Some(target) => target,
                    None => return Ok((Target::Given(Value::None), Vec::new())),
                }
            }
            ExprKind::Variant(case) => {
                let ty = self.enum_named_at(case, &[expr.span, callee.span])?;
                Target::Construct { ty, case: Some(case.clone()) }
            }
            ExprKind::Index { object, index } => match self.bracketed_target(expr, (object, index), args)? {
                Some(target) => target,
                None => return Ok((Target::Given(Value::None), Vec::new())),
            },
            _ => {
                let value = self.eval(callee)?;
                self.func_target(value)?
            }
        };
        let mut target = self.bound_target(target, callee, args, expr);
        // Left to right: a method taking its value as a copy reads it before its arguments, which
        // could change it.
        if let Target::Def { def, receiver, .. } = &mut target
            && def.params.first().is_some_and(|param| param.is_self && !param.is_mut)
            && let Some(Receiver::Place(place)) = receiver
        {
            let read = self.read_place(place, callee.span)?;
            *receiver = Some(Receiver::Value(read));
        }
        // So does a built-in method that only reads what it's called on.
        if let Target::Method { receiver, name, buffer: false } = &mut target
            && let Receiver::Place(place) = receiver
            && self.peek_place(place, callee.span, |held| super::methods::only_reads(held, name))?
        {
            let read = self.read_place(place, callee.span)?;
            *receiver = Receiver::Value(read);
        }
        let lazy = matches!(target, Target::Builtin(_));
        let mut given = Vec::with_capacity(args.len());
        for arg in args {
            given.push(self.arg_value(arg, lazy)?);
        }
        Ok((target, given))
    }

    /// A call of a function the program wrote, with what its type parameters stand for here, from
    /// the types where it's written.
    fn bound_target(&self, target: Target, callee: &Expr, args: &[Arg], expr: &Expr) -> Target {
        let Target::Def { def, parent, receiver, .. } = target else { return target };
        let object = match &callee.kind {
            ExprKind::Attr { object, .. } => Some(&**object),
            ExprKind::Index { object: inner, .. } => match &inner.kind {
                ExprKind::Attr { object, .. } => Some(&**object),
                _ => None,
            },
            _ => None,
        };
        let mut types = self.bound_at_call(def, object, args, expr);
        // Type arguments written in brackets, as `layout_unless_locked[T]()`, which may name a type
        // nothing else in the call settles.
        if let ExprKind::Index { index, .. } = &callee.kind {
            let written: Vec<&Expr> = match &index.kind {
                ExprKind::Tuple(items) => items.iter().collect(),
                _ => vec![&**index],
            };
            if written.len() == def.generics.len() {
                for (generic, written) in def.generics.iter().zip(written) {
                    if let Ok(ty) = self.type_argument(written) {
                        types.retain(|(name, _)| *name != generic.name);
                        types.push((generic.name.clone(), ty));
                    }
                }
            }
        }
        Target::Def { def, parent, receiver, types }
    }

    /// What `name[...](...)` calls, written with type arguments in brackets: `None` when it's a
    /// method of a missing value in a chain an `else` ends, so nothing's called.
    fn bracketed_target(
        &mut self,
        expr: &'static Expr,
        (object, index): (&'static Expr, &'static Expr),
        args: &'static [Arg],
    ) -> Run<Option<Target>> {
        let target = match &object.kind {
            ExprKind::Attr { object: owner, name, optional: false } => {
                match self.attr_target(owner, name, args, expr)? {
                    Some(target) => target,
                    None => return Ok(None),
                }
            }
            ExprKind::Name(name)
                if (self.program.function_at(name, object.span).is_some()
                    || self.program.std_name(name, object.span).is_some())
                    && !self.is_bound(name) =>
            {
                self.named_target(name, object.span)?
            }
            // A `Buffer` is held as the list it prints as, and its room as `methods.rs` says.
            ExprKind::Name(name)
                if name == "Buffer"
                    && args.iter().all(|arg| arg.name.as_deref() == Some("capacity"))
                    && !self.program.types.contains_key("Buffer") =>
            {
                Target::Given(Value::List(Held::new(Vec::new())))
            }
            // `std.layout`'s `SoaList[T]()`, which the checker types as a `list[T]`: its items are
            // laid out field by field, which nothing a program does can see.
            ExprKind::Name(name)
                if name == "SoaList"
                    && args.is_empty()
                    && !self.is_bound(name)
                    && self
                        .program
                        .imported
                        .get(name, object.span)
                        .is_some_and(|from| from == "std.layout.SoaList") =>
            {
                Target::Given(Value::List(Held::new(Vec::new())))
            }
            ExprKind::Name(name) if matches!(name.as_str(), "list" | "dict" | "set") && args.is_empty() => {
                Target::Given(match name.as_str() {
                    "list" => Value::List(Held::new(Vec::new())),
                    "dict" => Value::Dict(Held::new(Vec::new())),
                    _ => Value::Set(Held::new(Vec::new())),
                })
            }
            ExprKind::Name(name) if self.program.types.contains_key(&*self.type_named(name)) => {
                Target::Construct { ty: self.type_named(name).into_owned(), case: None }
            }
            // `array[T, N]()`: N items, each T's zero, as the checker typed it. An array is
            // held as the list it prints as.
            ExprKind::Name(name) if name == "array" && args.is_empty() && !self.is_bound(name) => {
                match self.program.type_at(expr.span).cloned() {
                    Some(ty @ Type::Array(..)) => Target::Given(self.zero_of(&ty, expr.span)?),
                    _ => return unsupported("`array[...]()` whose type isn't known"),
                }
            }
            ExprKind::Name(name) if name == "Table" && args.is_empty() && self.is_the_table(object.span) => {
                Target::Given(Value::Table(Box::new(crate::value::Table::new(self.shared.new_tag()))))
            }
            ExprKind::Name(name) => match self.program.imported.get(name, object.span) {
                Some(from) if !self.is_bound(name) && super::library::has_function(from) => {
                    Target::Builtin(from.clone())
                }
                Some(from) if !self.is_bound(name) => match self.native_format(from) {
                    Some(found) => self.named_target(&found, object.span)?,
                    None => return unsupported(from.clone()),
                },
                Some(from) => return unsupported(from.clone()),
                // `intrinsics.channel_send[T](...)`, as a module run from its source calls one.
                None => {
                    match name.split_once('.').map(|(module, function)| format!("std.{module}.{function}")) {
                        Some(path) if TYPED_INTRINSICS.contains(&path.as_str()) => {
                            Target::Given(self.typed_intrinsic(&path, index)?)
                        }
                        Some(path) if super::library::has_function(&path) => Target::Builtin(path),
                        _ => return unsupported(format!("`{name}[...]()`")),
                    }
                }
            },
            _ => return unsupported("calling something in brackets"),
        };
        Ok(Some(target))
    }

    /// `len(place)`, counted where the value is rather than on a copy of it.
    fn length_in_place(
        &mut self,
        callee: &'static Expr,
        args: &'static [Arg],
        span: Span,
    ) -> Run<Option<Value>> {
        let ExprKind::Name(name) = &callee.kind else { return Ok(None) };
        let [arg] = args else { return Ok(None) };
        if name != "len" || arg.is_mut || arg.name.is_some() || self.is_bound(name) {
            return Ok(None);
        }
        let Some(place) = self.place_of(&arg.value)? else { return Ok(None) };
        let counted = self.peek_place(&place, span, |value| match value {
            Value::Str(text) => Some(text.char_count()),
            Value::List(items) | Value::Tuple(items) | Value::Set(items) => Some(items.len()),
            Value::Dict(pairs) => Some(pairs.len()),
            Value::Table(table) => Some(table.len()),
            _ => None,
        })?;
        if let Some(count) = counted {
            return Ok(Some(Value::Int(super::as_int(count))));
        }
        let value = self.read_place(&place, span)?;
        self.length(value, span).map(|count| Some(Value::Int(count)))
    }

    pub(crate) fn arg_value(&mut self, arg: &'static Arg, lazy: bool) -> Run<ArgVal> {
        if arg.is_mut {
            let Some(place) = self.place_for_change(&arg.value)? else {
                return unsupported("a `ref` argument that isn't a place");
            };
            return Ok(ArgVal::Ref(arg.name.clone(), place));
        }
        if lazy && matches!(arg.value.kind, ExprKind::Comprehension { kind: CompKind::Generator, .. }) {
            return Ok(ArgVal::Lazy(arg.name.clone(), &arg.value));
        }
        let value = self.eval(&arg.value)?;
        Ok(match self.optional_levels(&value, arg.value.span) {
            Some(levels) => ArgVal::Optional(arg.name.clone(), value, levels),
            None => ArgVal::Value(arg.name.clone(), value),
        })
    }

    fn named_target(&mut self, name: &str, span: Span) -> Run<Target> {
        if let Some(value) = self.lookup(name, span)? {
            return self.func_target(value);
        }
        let name = &*self.type_named(name);
        if let Some(def) = self.program.function_at(name, span) {
            return Ok(Target::Def { def, parent: None, receiver: None, types: Bound::new() });
        }
        if let Some(declared) = self.program.types.get(name) {
            if declared.kind == Kind::Enum || !declared.def.cases.is_empty() {
                return unsupported(format!("calling the enum `{name}`"));
            }
            return Ok(Target::Construct { ty: name.to_owned(), case: None });
        }
        if BUILTINS.contains(&name) {
            return Ok(Target::Builtin(name.to_owned()));
        }
        // A core error made by name, as the standard library's own code makes `InvalidValue`s.
        if matches!(name, "InvalidValue" | "ParseError") {
            return Ok(Target::Builtin(name.to_owned()));
        }
        // A function the standard library's own code calls that the oracle models in
        // `library.rs`, an intrinsic or one of a module modeled on its own: a module run from its
        // source names it `module.name`, as `intrinsics.text_byte` or `json.quoted`.
        if let Some((module, function)) = name.split_once('.') {
            let path = format!("std.{module}.{function}");
            if super::library::has_function(&path) {
                return Ok(Target::Builtin(path));
            }
        }
        // An empty `StrWriter`, as the standard library's own code makes one to write into.
        if name == "StrWriter" {
            let parts = vec![("_parts".to_owned(), Value::List(Held::new(Vec::new())))];
            return Ok(Target::Given(Value::instance("StrWriter", None, parts)));
        }
        // A name imported from a module run from its own source, under the name it's run by.
        if let Some(found) = self.program.std_name(name, span).filter(|found| *found != name) {
            return self.named_target(found, span);
        }
        if let Some(from) = self.program.imported.get(name, span) {
            if super::library::has_function(from) {
                return Ok(Target::Builtin(from.clone()));
            }
            // `toml.loads` imported by name, run as `std.toml`'s own reader, as `attr_target` says.
            if let Some(found) = self.native_format(from) {
                return self.named_target(&found, span);
            }
            return unsupported(from.clone());
        }
        unsupported(format!("calling `{name}`"))
    }

    /// What the tables call the reader or writer written in Fennec the oracle runs in place of
    /// the runtime's `path`, as `NATIVE_FORMATS` pairs them, when its module is run from source.
    fn native_format(&self, path: &str) -> Option<String> {
        let (_, native) = NATIVE_FORMATS.iter().find(|(called, _)| *called == path)?;
        self.program.from_std.get(*native).cloned()
    }

    pub(crate) fn func_target(&self, value: Value) -> Run<Target> {
        match value {
            Value::Func(func) => match *func {
                Func::Named(name) => match self.program.functions.get(name.as_str()) {
                    Some(def) => Ok(Target::Def { def, parent: None, receiver: None, types: Bound::new() }),
                    None => unsupported(format!("calling `{name}`")),
                },
                Func::Builtin(name) => Ok(Target::Builtin(name)),
                Func::Nested { def, frame } => {
                    Ok(Target::Def { def, parent: Some(frame), receiver: None, types: Bound::new() })
                }
                Func::Lambda { params, body, captured } => {
                    Ok(Target::Lambda { params, body, captured: captured.into_inner() })
                }
            },
            other => unsupported(format!("calling a {}", other.kind())),
        }
    }

    /// `object.name(...)`: a method, a type's own function or case, or `None` when `object` is a
    /// missing value in a chain an `else` ends, so nothing's called.
    fn attr_target(
        &mut self,
        object: &'static Expr,
        name: &str,
        args: &'static [Arg],
        expr: &'static Expr,
    ) -> Run<Option<Target>> {
        if let ExprKind::Name(owner) = &object.kind
            && !self.is_bound(owner)
            && !self.program.constant_at(owner, object.span)
        {
            if name == "deserialize"
                && let Some(target) = self.deserialize_target(owner)?
            {
                return Ok(Some(target));
            }
            let owner = &self.type_named(owner).into_owned();
            if let Some(declared) = self.program.types.get(owner.as_str()) {
                if declared.def.cases.iter().any(|case| case.name == name) {
                    return Ok(Some(Target::Construct { ty: owner.clone(), case: Some(name.to_owned()) }));
                }
                return match self.program.method(owner, name) {
                    Some(def) if !def.takes_self() => {
                        Ok(Some(Target::Def { def, parent: None, receiver: None, types: Bound::new() }))
                    }
                    _ => unsupported(format!("`{owner}.{name}`")),
                };
            }
            if owner == "IoError" && super::files::IO_CASES.contains(&name) && self.is_std_type(owner) {
                return Ok(Some(Target::Construct { ty: owner.clone(), case: Some(name.to_owned()) }));
            }
            if BUILT_IN_TYPES.contains(&owner.as_str()) {
                // `u64.parse(text)`, written in Fennec in the core.
                if let Some(def) = self.program.method(owner, name).filter(|def| !def.takes_self()) {
                    return Ok(Some(Target::Def { def, parent: None, receiver: None, types: Bound::new() }));
                }
                return Ok(Some(Target::Static { ty: owner.clone(), name: name.to_owned() }));
            }
            if let Some(from) = self.program.imported.get(owner, object.span) {
                let path = format!("{from}.{name}");
                // `math.dot(...)`, of a module run from its own source.
                if let Some(found) = self.program.from_std.get(&path) {
                    return self.named_target(&found.clone(), expr.span).map(Some);
                }
                // `toml.dumps` and `toml.loads` are the runtime's, written in Rust; `std.toml`'s
                // own reader and writer, written in Fennec, which the compiler uses for a type
                // with its own `def serialize`, are run in their place, so the two are held to
                // each other.
                if let Some(found) = self.native_format(&path) {
                    return self.named_target(&found, expr.span).map(Some);
                }
                if super::library::has_function(&path) {
                    return Ok(Some(Target::Builtin(path)));
                }
                return unsupported(path);
            }
            return unsupported(format!("`{owner}.{name}`"));
        }
        // `std.collections.Deque()`, through `import std.collections`, which binds `std`.
        if let Some(dotted) = dotted(object)
            && let Some((root, _)) = dotted.split_once('.')
            && !self.is_bound(root)
            && let Some(from) = self.program.imported.get(&dotted, object.span)
        {
            let path = format!("{from}.{name}");
            if let Some(found) = self.program.from_std.get(&path) {
                return self.named_target(&found.clone(), expr.span).map(Some);
            }
            if super::library::has_function(&path) {
                return Ok(Some(Target::Builtin(path)));
            }
            return unsupported(path);
        }
        if name == "expect" {
            return self.expected(object, args, expr).map(Some);
        }
        if name == "serialize"
            && let Some(target) = self.serialize_target(object)?
        {
            return Ok(Some(target));
        }
        // A part read through a type's plain `def []` is written back when the call changes it.
        let receiver = match self.place_for_change(object)? {
            Some(place) => Receiver::Place(place),
            None => Receiver::Value(self.eval(object)?),
        };
        self.method_target(receiver, name, object.span)
    }

    /// The method `name` of what `receiver` holds: one the program wrote, or a built-in one, or
    /// `None` when it holds a missing value in a chain an `else` ends.
    pub(crate) fn method_target(
        &mut self,
        receiver: Receiver,
        name: &str,
        span: Span,
    ) -> Run<Option<Target>> {
        let object_span = span;
        let owner = match &receiver {
            Receiver::Place(place) => self.type_name_at(place, object_span)?,
            Receiver::Value(value) => type_name_of(value),
        };
        if owner.as_deref() == Some("None") {
            return Ok(None);
        }
        if let Some(owner) = owner
            && let Some(def) = self.program.method(&owner, name)
        {
            if !def.takes_self() {
                return unsupported("calling a type's function on a value");
            }
            return Ok(Some(Target::Def {
                def,
                parent: None,
                receiver: Some(receiver),
                types: Bound::new(),
            }));
        }
        let buffer = matches!(self.program.type_at(object_span), Some(Type::Buffer(_)));
        Ok(Some(Target::Method { receiver, name: name.to_owned(), buffer }))
    }

    /// `call().expect("reason")`: the call's value, or a stop with the reason if it fails.
    fn expected(&mut self, object: &'static Expr, args: &'static [Arg], expr: &'static Expr) -> Run<Target> {
        let ran = match &object.kind {
            ExprKind::Call { callee, args } => self.call_direct(object, callee, args),
            _ => self.eval(object),
        };
        match ran {
            Ok(Value::Instance(instance)) if self.program.method(&instance.ty, "expect").is_some() => {
                unsupported("a type's own `expect`")
            }
            Ok(value) => Ok(Target::Given(value)),
            Err(Flow::Fail(error, _)) => {
                let reason = match args.first().map(|arg| &arg.value.kind) {
                    Some(ExprKind::Str(reason)) => reason.clone(),
                    _ => return unsupported("`expect` with a reason that isn't written out"),
                };
                let shown = self.repr(&error)?;
                crash(format!("{reason}: {shown}"), expr.span)
            }
            Err(other) => Err(other),
        }
    }

    /// The name of the type of what's at a place, for finding its methods.
    fn type_name_at(&self, place: &Place, span: Span) -> Run<Option<String>> {
        self.peek_place(place, span, type_name_of)
    }

    /// The enum or error a leading-dot case like `.Circle` is of, from the type the checker gave
    /// where it's written.
    pub(crate) fn enum_named_at(&self, case: &str, spans: &[Span]) -> Run<String> {
        for span in spans {
            if let Some(ty) = self.program.type_at(*span) {
                let mut names = Vec::new();
                type_names(ty, &mut names);
                for name in names {
                    if name == "IoError" && super::files::IO_CASES.contains(&case) && self.is_std_type(&name)
                    {
                        return Ok(name);
                    }
                    if let Some(declared) = self.program.types.get(name.as_str())
                        && declared.def.cases.iter().any(|known| known.name == case)
                    {
                        return Ok(name);
                    }
                }
            }
        }
        unsupported(format!("the case `.{case}` of a type the oracle doesn't know"))
    }

    /// Calls what `prepare` worked out.
    pub(crate) fn invoke(&mut self, target: Target, given: Vec<ArgVal>, expr: &'static Expr) -> Run<Value> {
        match target {
            Target::Def { def, parent, receiver, types } => {
                self.call_def_bound((def, types), parent, receiver, given, expr.span)
            }
            Target::Deserialize(ty) => {
                let [ArgVal::Ref(_, src)] = given.as_slice() else {
                    return unsupported("`deserialize` given something other than `ref` to a `Deserializer`");
                };
                self.deserialize_from(&ty, src, expr.span)
            }
            Target::Serialize { value, ty } => {
                let [ArgVal::Ref(_, out)] = given.as_slice() else {
                    return unsupported("`serialize` given something other than `ref` to a `Serializer`");
                };
                self.serialize_into(&value, &ty, out, expr.span)?;
                Ok(Value::Unit)
            }
            Target::Lambda { params, body, captured } => self.call_lambda(&params, body, captured, given),
            Target::Construct { ty, case } => self.construct(&ty, case.as_deref(), given, expr.span),
            Target::Builtin(name) => self.builtin(&name, given, expr),
            Target::Method { receiver, name, buffer: false } => self.method(receiver, &name, given, expr),
            Target::Method { receiver, name, buffer: true } => {
                self.buffer_method(receiver, &name, given, expr.span)
            }
            Target::Static { ty, name } => self.static_call(&ty, &name, given, expr.span),
            Target::Given(value) => Ok(value),
        }
    }

    /// Calls a value that's a function, with plain values.
    pub(crate) fn call_value(&mut self, func: &Value, values: Vec<Value>, expr: &'static Expr) -> Run<Value> {
        let target = self.func_target(func.clone())?;
        let given = values.into_iter().map(|value| ArgVal::Value(None, value)).collect();
        self.invoke(target, given, expr)
    }

    pub(crate) fn call_lambda(
        &mut self,
        params: &[String],
        body: &'static Expr,
        captured: Vec<(String, Value)>,
        given: Vec<ArgVal>,
    ) -> Run<Value> {
        if given.len() != params.len() {
            return unsupported("an arrow function called with another number of arguments");
        }
        let mut vars: Vec<(String, Binding)> =
            captured.into_iter().map(|(name, value)| (name, Binding::Value(value))).collect();
        for (param, arg) in params.iter().zip(given) {
            let binding = match arg {
                ArgVal::Ref(_, place) => Binding::Ref(place.lent_alone()),
                other => Binding::Value(self.arg_as_value(other)?),
            };
            vars.push((param.clone(), binding));
        }
        let previous = self.push_frame(vars, None)?;
        let value = self.eval(body);
        self.pop_frame(previous);
        value
    }

    /// An argument's value, reading the place a `ref` one names.
    pub(crate) fn arg_as_value(&mut self, arg: ArgVal) -> Run<Value> {
        match arg {
            ArgVal::Value(_, value) | ArgVal::Optional(_, value, _) => Ok(value),
            ArgVal::Ref(_, place) => self.read_place(&place, Span::default()),
            ArgVal::Lazy(_, expr) => self.eval(expr),
        }
    }

    /// Calls a function the program wrote: its parameters bound to the arguments, by position
    /// and then by name, and the defaults of those left out.
    pub(crate) fn call_def(
        &mut self,
        def: &'static FnDef,
        parent: Option<u64>,
        receiver: Option<Receiver>,
        given: Vec<ArgVal>,
        span: Span,
    ) -> Run<Value> {
        self.call_def_bound((def, Bound::new()), parent, receiver, given, span)
    }

    /// `call_def`, with what the function's type parameters stand for in this call.
    pub(crate) fn call_def_bound(
        &mut self,
        (def, types): (&'static FnDef, Bound),
        parent: Option<u64>,
        receiver: Option<Receiver>,
        given: Vec<ArgVal>,
        span: Span,
    ) -> Run<Value> {
        if def.body.is_empty() {
            return unsupported(format!("the intrinsic `{}`", def.name));
        }
        let mut vars = Vec::new();
        let mut params = def.params.iter().peekable();
        if let Some(param) = params.next_if(|param| param.is_self) {
            let binding = match receiver {
                Some(Receiver::Place(place)) if param.is_mut => Binding::Ref(place),
                Some(Receiver::Place(place)) => Binding::Value(self.read_place(&place, span)?),
                Some(Receiver::Value(value)) => Binding::Value(value),
                None => return unsupported("a method called without its value"),
            };
            vars.push(("self".to_owned(), binding));
        }
        let params: Vec<&fennec_syntax::Param> = params.collect();
        let mut slots: Vec<Option<Binding>> = params.iter().map(|_| None).collect();
        // The parameters given a `None` the checker typed as an optional, with its levels, for
        // those written with more to hold it in a `Some` once their types are known, below.
        let mut optionals: Vec<(&'static str, usize, &'static TypeExpr)> = Vec::new();
        let mut next = 0;
        for arg in given {
            let at = if let Some(name) = arg.name() {
                params.iter().position(|param| param.name == name)
            } else {
                next += 1;
                Some(next - 1)
            };
            let Some(slot) = at.and_then(|at| slots.get_mut(at)) else {
                return unsupported("an argument no parameter takes");
            };
            if let (ArgVal::Optional(_, _, levels), Some(param)) = (&arg, at.and_then(|at| params.get(at)))
                && let Some(written) = &param.ty
            {
                optionals.push((param.name.as_str(), *levels, written));
            }
            *slot = Some(match arg {
                ArgVal::Ref(_, place) => Binding::Ref(place.lent_alone()),
                other => Binding::Value(self.arg_as_value(other)?),
            });
        }
        let mut defaults = Vec::new();
        for (param, slot) in params.iter().zip(slots) {
            match slot {
                Some(binding) => vars.push((param.name.clone(), binding)),
                None => match &param.default {
                    Some(default) => defaults.push((param.name.clone(), default)),
                    None => return unsupported(format!("a call leaving out `{}`", param.name)),
                },
            }
        }
        if self.is_generator(def) {
            for (name, default) in defaults {
                let value = self.plain_value(default)?;
                vars.push((name, Binding::Value(value)));
            }
            let shown = self.program.type_at(span).map(shown_type);
            return self.generator_from_def(def, vars, shown);
        }
        let previous = self.push_frame(vars, parent)?;
        self.frame_typed(def, types, optionals);
        let ran = (|| {
            for (name, default) in defaults {
                let value = self.eval(default)?;
                self.bind(&name, value)?;
            }
            // Its names last until the frame's let go of, below, where compiled code frees them.
            self.stmts(&def.body)
        })();
        let came = super::stream::letting_go(|| self.pop_frame(previous));
        // Letting go of a generator partway, its cleanup failing, stops the program where the
        // function ends, as compiled code's free does, unless it's stopped already.
        if !matches!(ran, Err(Flow::Crash(..) | Flow::Unsupported(_))) {
            self.came_of_letting_go(came, span)?;
        }
        match ran {
            Ok(()) => Ok(Value::Unit),
            Err(Flow::Return(value)) => Ok(value),
            Err(Flow::Break | Flow::Continue) => unsupported("a `break` or `continue` outside a loop"),
            Err(other) => Err(other),
        }
    }

    /// Notes on the call's new frame whose method it is, what its type parameters stand for and
    /// how many levels of optional it gives back. Then each parameter in `optionals`, given a
    /// `None` the checker typed as fewer levels than the parameter is written with, is held in a
    /// `Some` for each level more.
    fn frame_typed(
        &mut self,
        def: &'static FnDef,
        types: Bound,
        optionals: Vec<(&'static str, usize, &'static TypeExpr)>,
    ) {
        if let Some(frame) = self.frames.get_mut(self.cur) {
            if let Some(owner) = self.program.owner_of(def) {
                frame.owner = Some(owner.to_owned());
            }
            frame.types = types;
        }
        let returns = def.ret.as_ref().map_or(0, |ret| self.levels_written(ret));
        if let Some(frame) = self.frames.get_mut(self.cur) {
            frame.returns = returns;
        }
        for (name, given, written) in optionals {
            let wanted = self.levels_written(written);
            let Some(frame) = self.frames.get_mut(self.cur) else { break };
            if let Some((_, Binding::Value(value))) = frame.vars.iter_mut().rfind(|(bound, _)| bound == name)
            {
                *value = super::typed::held_in(std::mem::replace(value, Value::Unit), Some(given), wanted);
            }
        }
    }

    /// A default written as a plain value, worked out on its own.
    pub(crate) fn plain_value(&mut self, expr: &'static Expr) -> Run<Value> {
        let previous = self.push_frame(Vec::new(), None)?;
        let value = self.eval(expr);
        self.pop_frame(previous);
        value
    }

    /// The zero an `array[T, N]()` fills its items with: `0`, `0.0`, `False`, `""`, `None`, an
    /// array of zeros, or a struct made from its defaults, as `docs/language.md` says.
    fn zero_of(&mut self, ty: &Type, span: Span) -> Run<Value> {
        Ok(match ty {
            Type::Int => Value::Int(0),
            Type::Float => Value::Float(0.0),
            Type::Sized(num) if num.is_float() => Value::F32(0.0),
            Type::Bool => Value::Bool(false),
            Type::Str | Type::LiteralStr => Value::Str(Text::from(String::new())),
            Type::Optional(_) => Value::None,
            Type::Array(item, size) => {
                let size = usize::try_from(*size).unwrap_or(usize::MAX);
                let mut items = Vec::with_capacity(size.min(MOST_ITEMS));
                for _ in 0..size {
                    items.push(self.zero_of(item, span)?);
                }
                Value::List(Held::new(items))
            }
            Type::Named(name) if self.program.types.contains_key(name.as_str()) => {
                self.construct(name, None, Vec::new(), span)?
            }
            other => return unsupported(format!("the zero of a {other:?} in an array")),
        })
    }

    /// Makes a struct, an error, or a case: values given by position go to the public fields in
    /// order, then the named ones, then each field left out takes its default. A struct with
    /// `where` rules is checked once it's made.
    pub(crate) fn construct(
        &mut self,
        ty: &str,
        case: Option<&str>,
        given: Vec<ArgVal>,
        span: Span,
    ) -> Run<Value> {
        let instance = match self.made_unruled(ty, case, given)? {
            Value::Instance(instance) => instance.into_inner(),
            other => return Ok(other),
        };
        self.ruled(instance, span)
    }

    /// A value made as [`construct`] makes it, before its `where` rules are looked at.
    pub(crate) fn made_unruled(&mut self, ty: &str, case: Option<&str>, given: Vec<ArgVal>) -> Run<Value> {
        if ty == "IoError" && self.is_std_type(ty) {
            return self.io_case(case, given);
        }
        let Some(declared) = self.program.types.get(ty) else {
            return unsupported(format!("making a `{ty}`"));
        };
        let fields: &'static [Field] = match case {
            Some(case) => match declared.def.cases.iter().find(|known| known.name == case) {
                Some(found) => &found.fields,
                None => return unsupported(format!("the case `{ty}.{case}`")),
            },
            None => &declared.def.fields,
        };
        let positional: Vec<usize> = (0..fields.len())
            .filter(|at| case.is_some() || fields.get(*at).is_some_and(|field| !field.name.starts_with('_')))
            .collect();
        let mut values: Vec<Option<Value>> = fields.iter().map(|_| None).collect();
        let mut next = 0;
        for arg in given {
            let at = if let Some(name) = arg.name() {
                fields.iter().position(|field| field.name == name)
            } else {
                next += 1;
                positional.get(next - 1).copied()
            };
            let given = match &arg {
                ArgVal::Optional(_, _, levels) => Some(*levels),
                _ => None,
            };
            let wanted = at.and_then(|at| fields.get(at)).and_then(|field| field.ty.as_ref());
            let value = self.arg_as_value(arg)?;
            // A field written as a `T??` holds a `T?`'s `None` in a `Some`. A type parameter's
            // levels aren't counted, as what it stands for isn't known here.
            let value = super::typed::held_in(value, given, wanted.map_or(0, super::typed::levels_spelled));
            match at.and_then(|at| values.get_mut(at)) {
                Some(slot) => *slot = Some(value),
                None => return unsupported("a value no field takes"),
            }
        }
        let mut made = Vec::with_capacity(fields.len());
        for (field, value) in fields.iter().zip(values) {
            let value = match (value, &field.default) {
                (Some(value), _) => value,
                (None, Some(default)) => self.plain_value(default)?,
                (None, None) => return unsupported(format!("making a `{ty}` without `{}`", field.name)),
            };
            made.push((field.name.clone(), value));
        }
        let instance = crate::value::Instance {
            ty: ty.to_owned(),
            case: case.map(str::to_owned),
            fields: made,
            notes: Vec::new(),
        };
        Ok(Value::Instance(Held::new(instance)))
    }

    /// A value just made, if its `where` rules hold. Where the checker typed it as something that
    /// can fail, a broken rule is an `InvalidValue`, and otherwise it stops the program.
    pub(crate) fn ruled(&mut self, instance: crate::value::Instance, span: Span) -> Run<Value> {
        if let Some(broken) = self.broken_rule(&instance)? {
            if matches!(self.program.type_at(span), Some(Type::Result(..))) {
                let message =
                    Value::Str(Text::from(format!("{}.{broken} breaks its `where` rule", instance.ty)));
                let error = Value::instance("InvalidValue", None, vec![("message".to_owned(), message)]);
                return Err(Flow::Fail(error, span));
            }
            return crash(format!("this breaks the `where` rule on `{}.{broken}`", instance.ty), span);
        }
        Ok(Value::Instance(Held::new(instance)))
    }

    /// The first field whose `where` rule a struct breaks, if any.
    pub(crate) fn broken_rule(&mut self, instance: &crate::value::Instance) -> Run<Option<String>> {
        if instance.case.is_some() {
            return Ok(None);
        }
        let Some(declared) = self.program.types.get(instance.ty.as_str()) else { return Ok(None) };
        if declared.def.fields.iter().all(|field| field.rule.is_none()) {
            return Ok(None);
        }
        let vars = instance
            .fields
            .iter()
            .map(|(name, value)| (name.clone(), Binding::Value(value.clone())))
            .collect();
        let previous = self.push_frame(vars, None)?;
        let found = (|| {
            for field in &declared.def.fields {
                if let Some(rule) = &field.rule
                    && !self.truth(rule)?
                {
                    return Ok(Some(field.name.clone()));
                }
            }
            Ok(None)
        })();
        self.pop_frame(previous);
        found
    }

    /// The name a case `.Case` is made of, where it's written, then made with these values.
    pub(crate) fn construct_case_at(
        &mut self,
        case: &str,
        expr: &'static Expr,
        given: Vec<ArgVal>,
    ) -> Run<Value> {
        if given.is_empty()
            && let Some(value) = super::json::option_case_typed(self.program.type_at(expr.span), case)
        {
            return Ok(value);
        }
        let ty = self.enum_named_at(case, &[expr.span])?;
        self.construct(&ty, Some(case), given, expr.span)
    }
}

/// The type's name a value's methods are found under.
pub(crate) fn type_name_of(value: &Value) -> Option<String> {
    Some(match value {
        Value::Instance(instance) => instance.ty.clone(),
        Value::Str(_) => "str".to_owned(),
        Value::Int(_) => "int".to_owned(),
        Value::Float(_) => "float".to_owned(),
        Value::F32(_) => "f32".to_owned(),
        Value::Bool(_) => "bool".to_owned(),
        Value::List(_) => "list".to_owned(),
        Value::Dict(_) => "dict".to_owned(),
        Value::Set(_) => "set".to_owned(),
        Value::Sized(num, _) => num.name().to_owned(),
        Value::None => "None".to_owned(),
        _ => return None,
    })
}

/// Every type name a type mentions at its top: itself, what an optional holds, a union's members.
fn type_names(ty: &Type, names: &mut Vec<String>) {
    match ty {
        Type::Named(name) | Type::Generic(name, _) => {
            names.push(name.clone());
        }
        Type::Optional(inner) => type_names(inner, names),
        Type::Result(value, error) => {
            type_names(value, names);
            type_names(error, names);
        }
        Type::Union(members) => {
            for member in members {
                type_names(member, names);
            }
        }
        Type::Errors(errors) => {
            names.extend(errors.iter().cloned());
        }
        _ => {}
    }
}

/// A type as a generator prints it, with the program's own type names as they're written.
pub(crate) fn shown_type(ty: &Type) -> String {
    ty.to_string().replace(fennec_syntax::rename::RENAMED, "")
}

/// The intrinsics that answer for the type they're called with, `T` in `shape_hash[T]()`: what
/// `std.pack` writes beside a value, as `docs/pack.md` pins it, and a type's name as written.
const TYPED_INTRINSICS: &[&str] = &[
    "std.intrinsics.shape_hash",
    "std.intrinsics.layout",
    "std.intrinsics.shape_locked",
    "std.intrinsics.type_name",
];

impl Interp {
    /// What a typed intrinsic gives for the type `written` stands for here.
    fn typed_intrinsic(&self, path: &str, written: &'static Expr) -> Run<Value> {
        let ty = self.type_argument(written)?;
        Ok(match path {
            "std.intrinsics.shape_hash" => Value::Int(self.shape_hash(&ty)?),
            "std.intrinsics.layout" => Value::Str(Text::from(self.layout(&ty)?)),
            // `pack_lock()` is empty, so no type's layout is locked.
            "std.intrinsics.shape_locked" => Value::Bool(false),
            _ => match ty.source() {
                Some(name) => Value::Str(Text::from(name)),
                None => return unsupported("the name of a type that can't be written"),
            },
        })
    }
}

/// The places this call's `ref` arguments lend out of something written here, as `ref stats.level`
/// does, rather than handing on a name that's lent already, whose lender is further out.
fn lent_here(args: &[Arg], given: &[ArgVal]) -> Vec<Place> {
    let lent = given.iter().filter_map(|arg| match arg {
        ArgVal::Ref(_, place) => Some(place),
        _ => None,
    });
    args.iter()
        .filter(|arg| arg.is_mut)
        .zip(lent)
        .filter(|(arg, _)| !matches!(arg.value.kind, ExprKind::Name(_)))
        .map(|(_, place)| place.clone())
        .collect()
}
