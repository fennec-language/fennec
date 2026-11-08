//! `positional-arguments`: an argument whose meaning can't be read from the call has to be named,
//! or the program doesn't compile.
//! That's a bare `True` or `False`, an argument beside a parameter of the same type, which is easy
//! to swap unnoticed, and any argument past the first when there are three or more, unless every
//! two neighbors have types that can't hold each other's values, so a swap wouldn't compile. The
//! first argument and `self` are exempt, and so are parameters named `a`, `b`, `c` and `d`, which
//! say only which comes first, as in `dot(a, b)`: naming them at the call would say nothing more. A call this can't be sure of the target of isn't flagged:
//! a name a function binds itself might be anything, and so might a method or case with the same
//! name but different parameters on two types, since which type isn't known here.

use std::collections::{HashMap, HashSet};

use fennec_syntax::{
    Arg, Diagnostic, Expr, ExprKind, Field, FnDef, Generic, Item, Module, Param, Sources, Span, Stmt,
    StmtKind, TypeDef, TypeExpr,
};

use crate::Lint;
use crate::sizes::{blocks, functions};

/// One parameter a callee takes.
#[derive(PartialEq)]
struct Taken<'m> {
    name: &'m str,
    ty: Option<&'m TypeExpr>,
    /// Whether its type is one of the callee's type parameters, which could be anything.
    generic: bool,
}

/// What a callee takes, without `self`.
type Takes<'m> = Vec<Taken<'m>>;

fn taken<'m>(name: &'m str, ty: Option<&'m TypeExpr>, generics: &[&[Generic]]) -> Taken<'m> {
    let generic =
        ty.is_some_and(|ty| generics.iter().flat_map(|list| list.iter()).any(|g| g.name == ty.name));
    Taken { name, ty, generic }
}

fn takes_params<'m>(params: &'m [Param], generics: &[&[Generic]]) -> Takes<'m> {
    params
        .iter()
        .filter(|param| !param.is_self)
        .map(|param| taken(&param.name, param.ty.as_ref(), generics))
        .collect()
}

fn takes_fields<'m>(fields: &'m [Field], generics: &[Generic]) -> Takes<'m> {
    fields.iter().map(|field| taken(&field.name, field.ty.as_ref(), &[generics])).collect()
}

/// What the program's own functions, constructors and methods take.
#[derive(Default)]
struct Callees<'m> {
    named: HashMap<&'m str, Takes<'m>>,
    /// Every method with a name, on any type: the type isn't known here.
    methods: HashMap<&'m str, Vec<Takes<'m>>>,
    /// Every enum case with a name, written `.Circle(...)` or `Shape.Circle(...)`.
    cases: HashMap<&'m str, Vec<Takes<'m>>>,
    enums: HashSet<&'m str>,
    /// The names some function binds for itself, which might not mean what's defined here.
    locals: HashSet<&'m str>,
    /// Interfaces and aliases, whose values can be of other types.
    open_types: HashSet<&'m str>,
}

/// The names a block binds, and those of the blocks inside it.
fn bound<'m>(stmts: &'m [Stmt], names: &mut HashSet<&'m str>) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Assign { target, .. } => {
                if let ExprKind::Name(name) = &target.kind {
                    names.insert(name);
                }
            }
            StmtKind::For { names: loop_names, .. } => names.extend(loop_names.iter().map(String::as_str)),
            StmtKind::With { names: held, .. } => names.extend(held.iter().map(|held| held.name.as_str())),
            _ => {}
        }
        for block in blocks(stmt) {
            bound(block, names);
        }
    }
}

impl<'m> Callees<'m> {
    fn of(module: &'m Module) -> Self {
        let mut callees = Callees::default();
        // The standard library's come first, so the program's own take over from them.
        for library in fennec_syntax::prelude_modules().unwrap_or_default() {
            callees.add_items(library);
        }
        callees.add_items(module);
        for def in functions(module) {
            callees.locals.extend(def.params.iter().map(|param| param.name.as_str()));
            bound(&def.body, &mut callees.locals);
        }
        callees
    }

    fn add_items(&mut self, module: &'m Module) {
        for item in &module.items {
            match item {
                Item::Function(def) => {
                    self.named.insert(&def.name, takes_params(&def.params, &[&def.generics]));
                }
                Item::Struct(def) | Item::Error(def) => {
                    self.named.insert(&def.name, takes_fields(&def.fields, &def.generics));
                    self.add_methods(&def.methods, &def.generics);
                }
                Item::Enum(def) => self.add_enum(def),
                Item::Extend(extension) => self.add_methods(&extension.methods, &extension.generics),
                Item::Interface(interface) => {
                    self.open_types.insert(&interface.name);
                }
                Item::TypeAlias { name, .. } => {
                    self.open_types.insert(name);
                }
                _ => {}
            }
        }
    }

    fn add_enum(&mut self, def: &'m TypeDef) {
        self.enums.insert(&def.name);
        self.add_methods(&def.methods, &def.generics);
        for case in &def.cases {
            self.cases.entry(&case.name).or_default().push(takes_fields(&case.fields, &def.generics));
        }
    }

    fn add_methods(&mut self, methods: &'m [FnDef], owners: &[Generic]) {
        for method in methods {
            let takes = takes_params(&method.params, &[owners, &method.generics]);
            self.methods.entry(&method.name).or_default().push(takes);
        }
    }

    /// Whether no value of one parameter's type is a value of the other's, so passing them
    /// swapped wouldn't compile. `int` and `int?` aren't: an `int` passes as either.
    fn distinct(&self, a: &Taken<'_>, b: &Taken<'_>) -> bool {
        let fixed = |taken: &Taken<'_>| {
            let ty = taken.ty?;
            (!taken.generic && !self.open_types.contains(ty.name.as_str())).then(|| ty.name.clone())
        };
        fixed(a).zip(fixed(b)).is_some_and(|(a, b)| a != b)
    }

    /// What a call's target takes, if there's only one thing it can be.
    fn of_call(&self, callee: &Expr) -> Option<&Takes<'m>> {
        let the_one = |found: &'_ Vec<Takes<'m>>| -> bool {
            found.first().is_some_and(|first| found.iter().all(|other| other == first))
        };
        match &callee.kind {
            ExprKind::Name(name) if self.locals.contains(name.as_str()) => None,
            ExprKind::Name(name) => self.named.get(name.as_str()),
            ExprKind::Variant(name) => self.cases.get(name.as_str()).filter(|found| the_one(found))?.first(),
            ExprKind::Attr { object, name, .. } => {
                let on_an_enum =
                    matches!(&object.kind, ExprKind::Name(owner) if self.enums.contains(owner.as_str()));
                let found = if on_an_enum { &self.cases } else { &self.methods };
                found.get(name.as_str()).filter(|found| the_one(found))?.first()
            }
            _ => None,
        }
    }
}

fn same_type(a: Option<&TypeExpr>, b: Option<&TypeExpr>) -> bool {
    fn same(a: &TypeExpr, b: &TypeExpr) -> bool {
        a.name == b.name
            && a.optional == b.optional
            && a.args.len() == b.args.len()
            && a.args.iter().zip(&b.args).all(|(a, b)| same(a, b))
    }
    a.zip(b).is_some_and(|(a, b)| same(a, b))
}

/// Parameter names that say only where a parameter comes, which the position says already, as in
/// `dot(a, b)` or `max(a, b)`: an argument isn't asked to be named for one of these.
const PLACEHOLDERS: [&str; 4] = ["a", "b", "c", "d"];

fn placeholder(taken: Option<&Taken<'_>>) -> bool {
    taken.is_some_and(|taken| PLACEHOLDERS.contains(&taken.name))
}

/// Why the positional argument at `at`, not the first, has to be named, if it does, where `many`
/// says there are three or more that a swap could go unnoticed among.
fn needs_name(args: &[&Arg], takes: &Takes<'_>, at: usize, many: bool) -> Option<String> {
    let param = |at: usize| takes.get(at);
    let ty = |at: usize| param(at).and_then(|taken| taken.ty);
    if let Some(ExprKind::Bool(value)) = args.get(at).map(|arg| &arg.value.kind) {
        let written = if *value { "True" } else { "False" };
        return Some(format!("a bare `{written}` doesn't say what it's for"));
    }
    for other in [at - 1, at + 1] {
        if same_type(ty(at), ty(other)) && !(placeholder(param(at)) && placeholder(param(other))) {
            let (one, two) = (param(at.min(other))?.name, param(at.max(other))?.name);
            let shown = ty(at).map_or_else(String::new, type_text);
            return Some(format!(
                "`{one}` and `{two}` both take `{shown}`, so swapping them would still compile"
            ));
        }
    }
    many.then(|| "three or more arguments in a row are easy to get backwards".to_owned())
}

/// A type as it's written: `list[int]`, `shared Account`, `(int, str)?`.
fn type_text(ty: &TypeExpr) -> String {
    let all = |separator: &str| ty.args.iter().map(type_text).collect::<Vec<_>>().join(separator);
    let text = match (ty.name.as_str(), ty.args.as_slice()) {
        ("shared" | "with", [held]) => format!("{} {}", ty.name, type_text(held)),
        ("|", _) => all(" | "),
        ("tuple", _) => format!("({})", all(", ")),
        ("->", [params @ .., returns]) => {
            let params: Vec<String> = params.iter().map(type_text).collect();
            format!("({}) -> {}", params.join(", "), type_text(returns))
        }
        (name, []) => name.to_owned(),
        (name, _) => format!("{name}[{}]", all(", ")),
    };
    if !ty.optional {
        return text;
    }
    if ty.name == "|" || ty.name == "->" || ty.name == "shared" {
        format!("({text})?")
    } else {
        format!("{text}?")
    }
}

/// Where an argument starts as written: before any `(` its value's span leaves out, as in
/// `(seed >> 16) & 255`, so a name written there goes outside them.
fn written_start(sources: &Sources, value: Span) -> u32 {
    let Some((_, text, base)) = sources.file_at(value) else { return value.start };
    let Ok(mut at) = usize::try_from(value.start - base) else { return value.start };
    let mut start = at;
    while let Some(before) = at.checked_sub(1).and_then(|before| text.as_bytes().get(before)) {
        match before {
            b'(' => start = at - 1,
            b' ' | b'\t' | b'\n' => {}
            _ => break,
        }
        at -= 1;
    }
    u32::try_from(start).map_or(value.start, |start| base + start)
}

fn call(callees: &Callees<'_>, args: &[Arg], takes: &Takes<'_>, sources: &Sources) -> Option<Lint> {
    let positional: Vec<&Arg> = args.iter().take_while(|arg| arg.name.is_none()).collect();
    if positional.len() < 2 || positional.len() > takes.len() || positional.iter().any(|arg| arg.is_mut) {
        return None;
    }
    let passed = takes.get(..positional.len())?;
    let many = positional.len() >= 3
        && !passed.iter().all(|taken| placeholder(Some(taken)))
        && !passed.windows(2).all(|pair| matches!(pair, [a, b] if callees.distinct(a, b)));
    let (first, why) =
        (1..positional.len()).find_map(|at| needs_name(&positional, takes, at, many).map(|why| (at, why)))?;
    let name = takes.get(first)?.name;
    let at = positional.get(first)?.value.span;
    let mut diagnostic = Diagnostic::new(format!("{why}: name it, as `{name}=`"), at)
        .with_help("an argument named at the call shows which parameter it's for, so a swap can't hide");
    // What follows a named argument has to be named too.
    for (arg, taken) in positional.iter().zip(takes).skip(first) {
        let start = written_start(sources, arg.value.span);
        diagnostic = diagnostic.with_fix(Span { start, end: start }, format!("{}=", taken.name));
    }
    Some(Lint::new("positional-arguments", diagnostic))
}

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    let callees = Callees::of(module);
    let mut found = Vec::new();
    fennec_syntax::read::exprs(module, &mut |expr| {
        if let ExprKind::Call { callee, args } = &expr.kind {
            found.extend(callees.of_call(callee).and_then(|takes| call(&callees, args, takes, sources)));
        }
    });
    found
}
