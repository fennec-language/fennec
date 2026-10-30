//! Checking one function: the names in scope as its body is walked, what they hold at each
//! point, and what the function gives back. Names belong to the block they're created in.
//!
//! What a name holds can change along the way: `if x is None: return` means `x` isn't None
//! afterwards. Where paths through the code meet again, after an `if` or a loop, a name holds
//! whatever it held on any path that gets there.

use fennec_syntax::{Diagnostic, Expr, FnDef, Span, StmtKind, TestDef};

use crate::ParamType;
use crate::checker::Checker;
use crate::effects;
use crate::messages::not_mut;
use crate::stmt::not_none;
use crate::types::{Type, accepts, join, join_all};

#[derive(Clone, Debug)]
pub(crate) struct Binding {
    pub(crate) name: String,
    pub(crate) ty: Type,
    pub(crate) is_mut: bool,
    pub(crate) origin: Origin,
}

/// Where a name came from, for explaining how to make it changeable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    Local,
    Param,
    /// A `for` loop's name.
    Loop,
    /// A name from around a nested function.
    Captured,
}

struct Scope {
    bindings: Vec<Binding>,
    /// An arrow function's scope: names from outside it can be read but not changed.
    barrier: bool,
}

/// What a function's body gives back.
pub(crate) struct BodyType {
    pub(crate) value: Type,
    pub(crate) error: Type,
    /// The return type to suggest writing, from everything that's known.
    pub(crate) suggestion: Type,
}

pub(crate) struct FnCx<'c> {
    pub(crate) checker: &'c mut Checker,
    /// The function's name in the signature table, for naming the functions inside it.
    pub(crate) key: String,
    /// The function's own name, for messages.
    pub(crate) name: String,
    scopes: Vec<Scope>,
    /// Whether to report problems: not for the standard library, or for a field default
    /// looked at only for its type.
    pub(crate) report: bool,
    /// The return type written in the signature, if any.
    pub(crate) declared: Option<Type>,
    pub(crate) generator: bool,
    /// Whether it can fail, so it can give back a result it gets unhandled.
    pub(crate) fallible: bool,
    pub(crate) returns: Vec<Type>,
    pub(crate) errors: Vec<Type>,
    pub(crate) yields: Vec<Type>,
}

impl<'c> FnCx<'c> {
    fn new(checker: &'c mut Checker, key: &str, name: &str, report: bool) -> Self {
        FnCx {
            checker,
            key: key.to_owned(),
            name: name.to_owned(),
            scopes: vec![Scope { bindings: Vec::new(), barrier: false }],
            report,
            declared: None,
            generator: false,
            fallible: false,
            returns: Vec::new(),
            errors: Vec::new(),
            yields: Vec::new(),
        }
    }

    pub(crate) fn check_function(
        checker: &'c mut Checker,
        key: &str,
        def: &FnDef,
        params: &[ParamType],
        captured: Vec<Binding>,
        report: bool,
    ) -> BodyType {
        let mut cx = FnCx::new(checker, key, &def.name, report);
        cx.declared = def.ret.as_ref().map(|ret| cx.checker.program.resolve(ret));
        let effects = effects::of_body(&def.body);
        (cx.generator, cx.fallible) = (effects.yields, effects.fails);
        for binding in captured {
            cx.declare_as(&binding.name, binding.ty, false, Origin::Captured);
        }
        cx.push(false);
        for param in params {
            cx.declare_as(&param.name, param.ty.clone(), param.is_mut, Origin::Param);
        }
        let flow = cx.stmts(&def.body, true);
        // A body that always leaves early has an unknown value, which fits any type.
        if let (Some(declared), Some(StmtKind::Expr(last))) =
            (&cx.declared, def.body.last().map(|stmt| &stmt.kind))
        {
            let declared = declared.clone();
            cx.check_return(&declared, &flow.ty, last.span);
        }
        let mut values = cx.returns.clone();
        if !flow.exits {
            values.push(flow.ty);
        }
        let suggestion = join_all(values.iter().filter(|ty| **ty != Type::Unknown));
        let value = if cx.generator {
            Type::list(join_all(&cx.yields))
        } else if values.is_empty() {
            Type::Unknown
        } else {
            join_all(&values)
        };
        BodyType { value, error: join_all(&cx.errors), suggestion }
    }

    /// Checks a value returned from a function whose return type is written.
    pub(crate) fn check_return(&mut self, declared: &Type, actual: &Type, span: Span) {
        let passed_through = self.fallible && matches!(actual, Type::Result(..));
        if self.generator || passed_through || accepts(declared, actual) {
            return;
        }
        let diagnostic = if actual.maybe_none() && !declared.maybe_none() {
            crate::messages::might_be_none(span)
        } else if matches!(actual, Type::Result(..)) {
            crate::messages::unhandled_result(span)
        } else {
            let (declared, actual) = (crate::types::a(declared), crate::types::a(actual));
            Diagnostic::new(format!("`{}` returns {declared}, but this is {actual}", self.name), span)
                .with_help(format!("return {declared}, or change the return type"))
        };
        self.error(diagnostic);
    }

    pub(crate) fn check_test(checker: &'c mut Checker, test: &TestDef) {
        let mut cx = FnCx::new(checker, "test", "test", true);
        for input in &test.inputs {
            let ty = input.ty.as_ref().map_or(Type::Unknown, |ty| cx.checker.program.resolve(ty));
            cx.declare(&input.name, ty, input.is_mut);
        }
        cx.stmts(&test.body, false);
    }

    /// Checks a type's field defaults against their types, and its `where` rules.
    pub(crate) fn check_fields(checker: &'c mut Checker, type_name: &str) {
        let Some(info) = checker.program.type_info(type_name) else { return };
        let def = info.def.clone();
        let mut cx = FnCx::new(checker, type_name, type_name, true);
        for field in &def.fields {
            let Some(default) = &field.default else { continue };
            let actual = cx.expr(default);
            if let Some(written) = &field.ty {
                let expected = cx.checker.program.resolve(written);
                if !accepts(&expected, &actual) {
                    let what = format!("`{}`", field.name);
                    cx.error(crate::messages::mismatch(&what, &expected, &actual, default.span));
                }
            }
        }
        let rules: Vec<&Expr> = def.fields.iter().filter_map(|field| field.rule.as_ref()).collect();
        if rules.is_empty() {
            return;
        }
        cx.push(false);
        for field in &def.fields {
            let ty = cx.checker.field_type(type_name, None, &field.name);
            cx.declare(&field.name, ty, false);
        }
        for rule in rules {
            cx.condition(rule, "where");
        }
    }

    /// The type of a field's default, without reporting anything: that happens once, when
    /// the type's fields are checked.
    pub(crate) fn type_of_default(checker: &'c mut Checker, default: &Expr) -> Type {
        FnCx::new(checker, "", "", false).expr(default)
    }

    pub(crate) fn error(&mut self, diagnostic: Diagnostic) {
        if self.report {
            self.checker.diagnostics.push(diagnostic);
        }
    }

    /// An expression's type, without reporting anything about it.
    pub(crate) fn peek_type(&mut self, expr: &Expr) -> Type {
        let report = std::mem::replace(&mut self.report, false);
        let ty = self.expr(expr);
        self.report = report;
        ty
    }

    pub(crate) fn push(&mut self, barrier: bool) {
        self.scopes.push(Scope { bindings: Vec::new(), barrier });
    }

    pub(crate) fn pop(&mut self) {
        self.scopes.pop();
    }

    pub(crate) fn declare(&mut self, name: &str, ty: Type, is_mut: bool) {
        self.declare_as(name, ty, is_mut, Origin::Local);
    }

    pub(crate) fn declare_as(&mut self, name: &str, ty: Type, is_mut: bool, origin: Origin) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.bindings.push(Binding { name: name.to_owned(), ty, is_mut, origin });
        }
    }

    /// Why a name can't be changed here, and how to fix it.
    pub(crate) fn cannot_change(&self, name: &str, span: Span) -> Diagnostic {
        let mut crossed = false;
        let mut found = None;
        for scope in self.scopes.iter().rev() {
            found = scope.bindings.iter().rev().find(|binding| binding.name == name);
            if found.is_some() {
                break;
            }
            crossed |= scope.barrier;
        }
        let Some(binding) = found else { return not_mut(name, span) };
        if crossed || binding.origin == Origin::Captured {
            let function = if crossed { "an arrow function" } else { "a nested function" };
            return Diagnostic::new(format!("{function} can't change `{name}`, which is defined outside it"), span)
                .with_help(format!(
                    "functions can read the names around them but not change them: return a value instead, and change `{name}` outside the function"
                ));
        }
        let help = match binding.origin {
            Origin::Param if name == "self" => {
                format!("to change `self`, write `mut self` in `{}`'s parameters", self.name)
            }
            Origin::Param => format!(
                "parameters are read-only: to change it, write `mut {name}` in `{}`'s parameters, and pass it with `mut` at each call",
                self.name
            ),
            Origin::Loop => format!("loop with `for mut {name} in ...` to change each item"),
            Origin::Local | Origin::Captured => return not_mut(name, span),
        };
        Diagnostic::new(format!("`{name}` isn't mut"), span).with_help(help)
    }

    /// What a name holds, and whether it can be changed from here.
    pub(crate) fn lookup(&self, name: &str) -> Option<(Type, bool)> {
        let mut crossed = false;
        for scope in self.scopes.iter().rev() {
            if let Some(binding) = scope.bindings.iter().rev().find(|binding| binding.name == name) {
                return Some((binding.ty.clone(), binding.is_mut && !crossed));
            }
            crossed |= scope.barrier;
        }
        None
    }

    fn binding_mut(&mut self, name: &str) -> Option<&mut Binding> {
        self.scopes
            .iter_mut()
            .rev()
            .flat_map(|scope| scope.bindings.iter_mut().rev())
            .find(|b| b.name == name)
    }

    /// Records what a name holds from now on.
    pub(crate) fn set_type(&mut self, name: &str, ty: Type) {
        if let Some(binding) = self.binding_mut(name) {
            binding.ty = ty;
        }
    }

    /// Every name in sight, for a nested function to read.
    pub(crate) fn visible(&self) -> Vec<Binding> {
        self.scopes.iter().flat_map(|scope| scope.bindings.iter().cloned()).collect()
    }

    /// What every name holds right now, to come back to after one path through the code.
    pub(crate) fn snapshot(&self) -> Vec<Type> {
        self.scopes.iter().flat_map(|scope| scope.bindings.iter().map(|b| b.ty.clone())).collect()
    }

    pub(crate) fn restore(&mut self, types: &[Type]) {
        let bindings = self.scopes.iter_mut().flat_map(|scope| scope.bindings.iter_mut());
        for (binding, ty) in bindings.zip(types) {
            binding.ty = ty.clone();
        }
    }

    /// Where paths meet: each name holds what it held on any of them.
    pub(crate) fn merge(&mut self, paths: &[Vec<Type>]) {
        let Some((first, rest)) = paths.split_first() else { return };
        let merged: Vec<Type> = first
            .iter()
            .enumerate()
            .map(|(at, ty)| {
                rest.iter()
                    .fold(ty.clone(), |ty, path| path.get(at).map_or(ty.clone(), |other| join(&ty, other)))
            })
            .collect();
        self.restore(&merged);
    }

    /// Starts following the field paths a condition checks for None, like `player.weapon`,
    /// so they can be narrowed like names.
    pub(crate) fn track(&mut self, cond: &Expr) {
        for path in not_none(cond, true).into_iter().chain(not_none(cond, false)) {
            if path.contains('.') && self.lookup(&path).is_none() {
                let ty = self.path_type(&path);
                self.declare(&path, ty, false);
            }
        }
    }

    fn path_type(&mut self, path: &str) -> Type {
        let report = std::mem::replace(&mut self.report, false);
        let mut fields = path.split('.');
        let root = fields.next().unwrap_or_default();
        let mut ty = self.lookup(root).map_or(Type::Unknown, |(ty, _)| ty);
        for field in fields {
            ty = self.field_of(&ty, field, false, Span::default());
        }
        self.report = report;
        ty
    }

    /// Forgets what was learned about a name's fields, because it's changing: each one holds
    /// its field's type again.
    pub(crate) fn forget_fields(&mut self, root: &str) {
        let prefix = format!("{root}.");
        let paths: Vec<String> =
            self.visible().into_iter().map(|b| b.name).filter(|name| name.starts_with(&prefix)).collect();
        for path in paths {
            let ty = self.path_type(&path);
            self.set_type(&path, ty);
        }
    }

    /// Records that these names aren't None.
    pub(crate) fn narrow(&mut self, names: &[String]) {
        for name in names {
            if let Some(binding) = self.binding_mut(name)
                && binding.ty.maybe_none()
            {
                binding.ty = binding.ty.without_none();
            }
        }
    }
}
