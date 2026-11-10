//! The linter. It has no settings and no way to turn a lint off, so every lint either has an
//! automatic fix, which `fennec fix` makes, or can't be wrong, because it counts something or
//! matches an exact form.

mod asserts;
mod calls;
mod chains;
mod compares;
mod conversions;
mod copies;
mod costs;
mod dead;
mod docs;
mod flow;
mod items;
mod lookups;
mod loops;
mod names;
mod params;
mod paths;
mod rewrites;
mod sizes;
mod stale;

use fennec_syntax::{Diagnostic, Item, Module, Sources};

/// How much a finding matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// A lint, which fails `fennec lint` until it's fixed.
    Error,
    /// Correct code worth knowing about, like what may copy, which never fails anything.
    Hint,
}

/// One thing a lint found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lint {
    /// The lint's name, like `needless-return`.
    pub rule: &'static str,
    pub severity: Severity,
    pub diagnostic: Diagnostic,
}

impl Lint {
    fn new(rule: &'static str, diagnostic: Diagnostic) -> Self {
        let message = format!("{rule}: {}", diagnostic.message);
        Lint { rule, severity: Severity::Error, diagnostic: Diagnostic { message, ..diagnostic } }
    }

    fn hint(rule: &'static str, diagnostic: Diagnostic) -> Self {
        Lint { severity: Severity::Hint, ..Lint::new(rule, diagnostic) }
    }

    /// What it says, without the rule's name in front.
    #[must_use]
    pub fn message(&self) -> &str {
        let message = &self.diagnostic.message;
        message.strip_prefix(self.rule).and_then(|rest| rest.strip_prefix(": ")).unwrap_or(message)
    }
}

/// Each call whose positional arguments are easy to get backwards, which is a compile error
/// rather than a lint: a bare `True` or `False`, neighbors of one type, or three or more that
/// could swap unnoticed. Each says which names to write, and its fix writes them.
#[must_use]
pub fn positional_arguments(module: &Module, sources: &Sources) -> Vec<Lint> {
    calls::of(module, sources)
}

/// Everything the lints find in a loaded program, in the order it's written.
pub fn lint(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut found = in_each_file(module, sources, &Facts::of(module, sources));
    found.extend(across_files(module, sources));
    found.sort_by_key(|lint| (lint.diagnostic.span.start, lint.rule));
    found
}

/// What the lints that look at one file at a time find in the files `wanted` takes, by the path
/// they're shown by, in the order they're written: all `lint` finds, and in a kit all
/// `kit_lints` finds, but for what `across_files` finds. In a file, these depend only on its own
/// text and on what the whole program defines, never on another file's bodies, so an edit that
/// changes only bodies changes what they find in the files it edited and nowhere else, as long as
/// `Facts` stay the same.
pub fn within_files(
    module: &Module,
    sources: &Sources,
    kit: bool,
    wanted: &dyn Fn(&str) -> bool,
) -> Vec<Lint> {
    let facts = Facts::of(module, sources);
    let taken = |item: &&Item| sources.file_at(item_span(item)).is_some_and(|(path, ..)| wanted(path));
    let part = Module { items: module.items.iter().filter(taken).cloned().collect(), ..Default::default() };
    let mut found = in_each_file(&part, &sources.only(wanted), &facts);
    if kit {
        found.extend(asserts::in_kit(&part, facts.declares_expect));
    }
    found.sort_by_key(|lint| (lint.diagnostic.span.start, lint.rule));
    found
}

/// What the lints that look across a program's files find: `unused-param`, which looks at every
/// call, and `unused-item`, which looks at every name written.
#[must_use]
pub fn across_files(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut found = params::of(module, sources);
    found.extend(items::of(module, sources));
    found
}

/// What the lints of one file at a time take from all of a program: which files extend a type,
/// whose imports are used for that, and whether it declares an `expect` of its own, which
/// `expect-in-kit` then leaves alone. A nested function can be named `expect`, so an edit to a
/// body can change these, and then every file's lints have to be found again.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Facts {
    pub extending: Vec<String>,
    pub declares_expect: bool,
}

impl Facts {
    #[must_use]
    pub fn of(module: &Module, sources: &Sources) -> Facts {
        let mut extending: Vec<String> =
            dead::extending(module, sources).into_iter().map(str::to_owned).collect();
        extending.sort();
        extending.dedup();
        Facts { extending, declares_expect: asserts::declares_expect(module) }
    }
}

fn in_each_file(module: &Module, sources: &Sources, facts: &Facts) -> Vec<Lint> {
    let extending: Vec<&str> = facts.extending.iter().map(String::as_str).collect();
    let mut found = sizes::of(module, sources);
    found.extend(rewrites::of(module, sources));
    found.extend(flow::of(module, sources));
    found.extend(dead::of(module, sources, &extending));
    found.extend(loops::of(module, sources));
    found.extend(names::of(module, sources));
    found.extend(chains::of(module, sources));
    found.extend(paths::of(sources));
    found.extend(asserts::of(module));
    found.extend(docs::of(module, sources));
    found
}

/// Where an item is written.
fn item_span(item: &Item) -> fennec_syntax::Span {
    match item {
        Item::Function(def) => def.span,
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => def.span,
        Item::Interface(interface) => interface.span,
        Item::Extend(extension) => extension.span,
        Item::TypeAlias { span, .. } | Item::Const { span, .. } => *span,
        Item::Import(import) => import.span,
        Item::Test(test) => test.span,
        Item::Stmt(stmt) => stmt.span,
    }
}

/// What only a kit is held to, beyond `lint`: its code outside `main` and its tests doesn't
/// stop the program with `.expect(...)`, but gives the failure back to its caller. Whether a
/// program is a kit is up to whoever loaded it.
#[must_use]
pub fn kit_lints(module: &Module) -> Vec<Lint> {
    asserts::in_kit(module, asserts::declares_expect(module))
}

/// Each struct copied from one place into another while what it came from can still change, as
/// a hint: `world.selected = world.units[i]` is a snapshot, which a program that meant a link
/// would write as a `ref`. It needs the typed tree, so it's asked for where the whole program is
/// checked anyway.
#[must_use]
pub fn stale_copies(module: &Module, sources: &Sources) -> Vec<Lint> {
    stale::of(module, sources)
}

/// Every conversion to the type a value has already, in a module that checks: `str(s)` on text,
/// `int(n)` on an int, `float(x)` on a float, and `str(x)` shown in an f-string. Each fix is
/// tried, so they're asked for where the whole program is checked anyway.
#[must_use]
pub fn needless_conversions(module: &Module, sources: &Sources) -> Vec<Lint> {
    conversions::of(module, sources)
}

/// Every `table[key] if key in table else default` on a dict, in a module that checks, which
/// `table.get(key) else default` says with one lookup. Like `needless_conversions`, each fix is tried,
/// so they're asked for where the whole program is checked anyway.
#[must_use]
pub fn double_lookups(module: &Module, sources: &Sources) -> Vec<Lint> {
    lookups::of(module, sources)
}

/// Every `x == x` and `x != x` on a name or field of a plain type, in a module that checks. It
/// needs the types, so it's asked for where the whole program is checked anyway, apart from `lint`.
#[must_use]
pub fn self_compares(module: &Module, sources: &Sources) -> Vec<Lint> {
    compares::of(module, sources)
}

/// A hint for each place a lowered program may copy, and each it may allocate in code that runs
/// often, from what `fennec_ir::costs` finds, in its order: hot code first.
#[must_use]
pub fn costs(findings: &[fennec_ir::Finding]) -> Vec<Lint> {
    costs::of(findings)
}

/// Each cost in a `@hot` function, or in one it calls, as a compile error, and each call it can't
/// see into. A hot path may not allocate or copy.
#[must_use]
pub fn hot_costs(findings: &[fennec_ir::Finding], reach: &fennec_ir::HotReach) -> Vec<Diagnostic> {
    costs::hot(findings, reach)
}
