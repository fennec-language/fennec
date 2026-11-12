//! `fennec fix` for a capability a function uses but wasn't given, like `fs` in a function that
//! takes no `Fs`: it gives the function the parameter, passes it at every call, and gives each
//! caller the parameter in turn, up to the first that has one, `main`, or a test. It only makes
//! changes that can't mean anything else, so where a function in the chain is a method, or is
//! named anywhere but in a plain call, it makes none and the error stays.

use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The errors about each capability, by its name and type, with the function or test each is in.
type Asked<'m> = Vec<((String, String), Vec<(usize, Owner<'m>)>)>;

use fennec_syntax::{Diagnostic, Edit, Expr, ExprKind, FnDef, Item, Module, Sources, Span, TestDef};

/// The help on a capability used by its usual name in a function that wasn't given it.
pub(crate) const NOT_GIVEN: &str = "a function reaches outside itself only through what's in its signature";

/// Gives the errors about a capability a function wasn't given the fix that threads it through.
/// The errors about one capability get one fix between them, on the first, so a caller two of
/// them share is given it once.
pub fn add_capability_fixes(diagnostics: &mut [Diagnostic], module: &Module, sources: &Sources) {
    let kit = Kit::of(module);
    let mut asked: Asked<'_> = Vec::new();
    for (at, diagnostic) in diagnostics.iter().enumerate() {
        let Some((name, ty)) = not_given(diagnostic) else { continue };
        let Some(owner) = kit.owner(diagnostic.span) else { continue };
        let capability = (name, ty);
        match asked.iter_mut().find(|(asked, _)| *asked == capability) {
            Some((_, owners)) => owners.push((at, owner)),
            None => asked.push((capability, vec![(at, owner)])),
        }
    }
    for ((name, ty), owners) in asked {
        let first = owners.first().map(|(at, _)| *at);
        let starts = owners.into_iter().map(|(_, owner)| owner).collect();
        let edits = Threading { kit: &kit, sources, name: &name, ty: &ty }.edits(starts);
        if let (Some(first), Some(edits)) = (first.and_then(|first| diagnostics.get_mut(first)), edits) {
            first.fixes.extend(edits);
        }
    }
}

/// The capability an error says a function wasn't given, by its name and its type, when it's one
/// with no fix of its own.
fn not_given(diagnostic: &Diagnostic) -> Option<(String, String)> {
    if !diagnostic.fixes.is_empty() || !diagnostic.help.as_deref()?.starts_with(NOT_GIVEN) {
        return None;
    }
    let name = diagnostic.message.strip_prefix('`')?.strip_suffix("` doesn't exist")?;
    let mut letters = name.chars();
    let ty: String = letters.next()?.to_uppercase().chain(letters).collect();
    Some((name.to_owned(), ty))
}

/// What a use of a capability is in: a function written at the top level, or a test.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Owner<'m> {
    Function(&'m str),
    Test(usize),
}

/// A kit's functions and tests, and everything that names a function.
struct Kit<'m> {
    functions: HashMap<&'m str, &'m FnDef>,
    tests: Vec<&'m TestDef>,
    /// Where each function is called by its name, by what makes the call.
    calls: HashMap<&'m str, Vec<(Owner<'m>, &'m Expr)>>,
    /// Functions named anywhere but in a plain call from a function or a test: as a value, after a
    /// dot, or in a method, a constant or a statement at the top level.
    named_otherwise: BTreeSet<&'m str>,
    /// The spans of methods, where nothing is threaded.
    methods: Vec<Span>,
}

impl<'m> Kit<'m> {
    fn of(module: &'m Module) -> Kit<'m> {
        let mut kit = Kit {
            functions: HashMap::new(),
            tests: Vec::new(),
            calls: HashMap::new(),
            named_otherwise: BTreeSet::new(),
            methods: Vec::new(),
        };
        for item in &module.items {
            match item {
                Item::Function(def) => {
                    kit.functions.insert(&def.name, def);
                }
                Item::Test(test) => kit.tests.push(test),
                Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                    kit.methods.extend(def.methods.iter().map(extent));
                }
                Item::Interface(interface) => kit.methods.extend(interface.methods.iter().map(extent)),
                Item::Extend(extension) => kit.methods.extend(extension.methods.iter().map(extent)),
                _ => {}
            }
        }
        for item in &module.items {
            let owner = match item {
                Item::Function(def) => Some(Owner::Function(&def.name)),
                Item::Test(test) => {
                    kit.tests.iter().position(|seen| std::ptr::eq(*seen, test)).map(Owner::Test)
                }
                _ => None,
            };
            let mut found: Vec<(&'m str, Option<&'m Expr>)> = Vec::new();
            let mut look = |expr: &'m Expr| named(expr, &mut found);
            match item {
                Item::Function(def) => read_function(def, &mut look),
                Item::Test(test) => read_stmts(&test.body, &mut look),
                _ => read_item(item, &mut look),
            }
            kit.record(owner, found);
        }
        kit
    }

    /// Keeps what an item names: each call by name where it's a function's or a test's, and every
    /// other naming as one that stops threading.
    fn record(&mut self, owner: Option<Owner<'m>>, found: Vec<(&'m str, Option<&'m Expr>)>) {
        let mut called: HashMap<&str, usize> = HashMap::new();
        let mut named: BTreeMap<&str, usize> = BTreeMap::new();
        for (name, call) in found {
            match call {
                Some(call) => {
                    *called.entry(name).or_default() += 1;
                    match owner {
                        Some(owner) => self.calls.entry(name).or_default().push((owner, call)),
                        None => {
                            self.named_otherwise.insert(name);
                        }
                    }
                }
                None => *named.entry(name).or_default() += 1,
            }
        }
        // A call's callee is read as a name too, so a name read more often than it's called is
        // read somewhere else as well.
        for (name, times) in named {
            if times > called.get(name).copied().unwrap_or(0) {
                self.named_otherwise.insert(name);
            }
        }
    }

    /// The function or test a use at `span` is in, if it's one a parameter can be threaded to.
    fn owner(&self, span: Span) -> Option<Owner<'m>> {
        let inside = |outer: Span| outer.start <= span.start && span.end <= outer.end;
        if self.methods.iter().any(|method| inside(*method)) {
            return None;
        }
        if let Some(def) = self.functions.values().find(|def| inside(extent(def))) {
            return Some(Owner::Function(&def.name));
        }
        self.tests.iter().position(|test| inside(test.span)).map(Owner::Test)
    }
}

/// What a function covers, from `def` to the end of its body: its own span is its signature.
fn extent(def: &FnDef) -> Span {
    let end = def.body.last().map_or(def.span.end, |last| last.span.end.max(def.span.end));
    Span { start: def.span.start, end }
}

/// Notes a name `expr` reads, or a call by name, with the call.
fn named<'m>(expr: &'m Expr, found: &mut Vec<(&'m str, Option<&'m Expr>)>) {
    match &expr.kind {
        ExprKind::Name(name) | ExprKind::Attr { name, .. } => found.push((name, None)),
        ExprKind::Call { callee, .. } => {
            if let ExprKind::Name(name) = &callee.kind {
                found.push((name, Some(expr)));
            }
        }
        _ => {}
    }
}

fn read_function<'m>(def: &'m FnDef, look: &mut dyn FnMut(&'m Expr)) {
    fennec_syntax::read::in_function(def, look);
}

fn read_stmts<'m>(body: &'m [fennec_syntax::Stmt], look: &mut dyn FnMut(&'m Expr)) {
    fennec_syntax::read::in_stmts(body, look);
}

fn read_item<'m>(item: &'m Item, look: &mut dyn FnMut(&'m Expr)) {
    match item {
        Item::Const { value, .. } => fennec_syntax::read::in_expr(value, look),
        Item::Stmt(stmt) => fennec_syntax::read::in_stmts(std::slice::from_ref(stmt), look),
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
            for method in &def.methods {
                read_function(method, look);
            }
        }
        Item::Interface(interface) => interface.methods.iter().for_each(|method| read_function(method, look)),
        Item::Extend(extension) => extension.methods.iter().for_each(|method| read_function(method, look)),
        _ => {}
    }
}

/// Whether a caller takes the capability already.
enum Held<'m> {
    /// It does, by this name.
    As(&'m str),
    /// It doesn't, so it's given it too.
    Not,
    /// It doesn't, but something else of its has the capability's name, or it can't be read, so
    /// passing it would mean something else.
    NameTaken,
}

/// Threading one capability through a kit.
struct Threading<'k, 'm> {
    kit: &'k Kit<'m>,
    sources: &'k Sources,
    /// What it's called, like `fs`.
    name: &'k str,
    /// Its type, like `Fs`.
    ty: &'k str,
}

impl<'m> Threading<'_, 'm> {
    /// The edits that give each of `starts` the capability and pass it down from where it's held,
    /// or None where that's more than a plain change.
    fn edits(&self, starts: Vec<Owner<'m>>) -> Option<Vec<Edit>> {
        let mut given: BTreeSet<Owner<'m>> = BTreeSet::new();
        let mut waiting = starts;
        let mut edits: Vec<Edit> = Vec::new();
        while let Some(owner) = waiting.pop() {
            if !given.insert(owner) {
                continue;
            }
            if !matches!(self.held_by(owner), Held::Not) {
                return None;
            }
            edits.push(self.parameter(owner)?);
            let Owner::Function(function) = owner else { continue };
            if function == "main" {
                continue;
            }
            if self.kit.named_otherwise.contains(function) {
                return None;
            }
            for (caller, call) in self.kit.calls.get(function).into_iter().flatten() {
                let passed = match self.held_by(*caller) {
                    Held::As(held) => held,
                    Held::Not => {
                        waiting.push(*caller);
                        self.name
                    }
                    Held::NameTaken => return None,
                };
                edits.push(self.argument(call, passed)?);
            }
        }
        edits.sort_by_key(|edit| (edit.span.start, edit.span.end));
        edits.dedup();
        Some(edits)
    }

    /// Whether `owner` takes the capability already, and what it calls it.
    fn held_by(&self, owner: Owner<'m>) -> Held<'m> {
        let Some(params) = self.params(owner) else { return Held::NameTaken };
        if let Some(held) = params.iter().find(|param| param.ty.as_ref().is_some_and(|ty| self.is_it(ty))) {
            return Held::As(&held.name);
        }
        if params.iter().any(|param| param.name == self.name) {
            return Held::NameTaken;
        }
        Held::Not
    }

    fn is_it(&self, ty: &fennec_syntax::TypeExpr) -> bool {
        ty.name == self.ty && ty.args.is_empty() && !ty.optional
    }

    fn params(&self, owner: Owner<'m>) -> Option<&'m [fennec_syntax::Param]> {
        match owner {
            Owner::Function(name) => Some(&self.kit.functions.get(name)?.params),
            Owner::Test(at) => Some(&self.kit.tests.get(at)?.capabilities),
        }
    }

    /// The edit that gives `owner` the parameter, first among the ones it takes.
    fn parameter(&self, owner: Owner<'m>) -> Option<Edit> {
        let written = format!("{}: {}", self.name, self.ty);
        let params = self.params(owner)?;
        // A test can't be given what a program is started with.
        if matches!(owner, Owner::Test(_)) && matches!(self.ty, "Args" | "Stdin") {
            return None;
        }
        if let Some(first) = params.first() {
            return Some(insert(first.span.start, format!("{written}, ")));
        }
        match owner {
            Owner::Function(name) => {
                let def = self.kit.functions.get(name)?;
                let open = self.open_paren_of_def(def)?;
                Some(insert(open + 1, written))
            }
            Owner::Test(at) => {
                let test = self.kit.tests.get(at)?;
                Some(insert(self.end_of_test_name(test)?, format!("({written})")))
            }
        }
    }

    /// The edit that passes `passed` first in `call`.
    fn argument(&self, call: &Expr, passed: &str) -> Option<Edit> {
        let ExprKind::Call { callee, args } = &call.kind else { return None };
        let (text, base) = self.text_at(call.span)?;
        let after_callee = usize::try_from(callee.span.end.checked_sub(base)?).ok()?;
        let rest = text.get(after_callee..)?;
        let open = after_callee + rest.find(|ch: char| !ch.is_whitespace())?;
        if text.as_bytes().get(open) != Some(&b'(') {
            return None;
        }
        let at = base + u32::try_from(open + 1).ok()?;
        let written = if args.is_empty() { passed.to_owned() } else { format!("{passed}, ") };
        Some(insert(at, written))
    }

    /// Where the `(` of a function's parameters is: after its name and any `[...]` of type
    /// parameters.
    fn open_paren_of_def(&self, def: &FnDef) -> Option<u32> {
        let (text, base) = self.text_at(def.span)?;
        let start = usize::try_from(def.span.start.checked_sub(base)?).ok()?;
        let end = usize::try_from(def.span.end.checked_sub(base)?).ok()?;
        let written = text.get(start..end)?;
        let keyword = written.find("def ")?;
        let after = keyword + "def ".len();
        let named = after + written.get(after..)?.find(def.name.as_str())? + def.name.len();
        let mut depth = 0_usize;
        for (at, ch) in written.get(named..)?.char_indices() {
            match ch {
                '[' => depth += 1,
                ']' => depth = depth.checked_sub(1)?,
                '(' if depth == 0 => return Some(base + u32::try_from(start + named + at).ok()?),
                ch if depth == 0 && !ch.is_whitespace() => return None,
                _ => {}
            }
        }
        None
    }

    /// Where a test's name ends: just after the closing quote of `test "..."`.
    fn end_of_test_name(&self, def: &TestDef) -> Option<u32> {
        let (text, base) = self.text_at(def.span)?;
        let start = usize::try_from(def.span.start.checked_sub(base)?).ok()?;
        let written = text.get(start..)?;
        let open = written.find('"')?;
        let mut escaped = false;
        for (at, ch) in written.get(open + 1..)?.char_indices() {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => return Some(base + u32::try_from(start + open + 1 + at + 1).ok()?),
                '\n' => return None,
                _ => {}
            }
        }
        None
    }

    /// The text of the file `span` is in, and where its spans start.
    fn text_at(&self, span: Span) -> Option<(&str, u32)> {
        let (_, text, base) = self.sources.file_at(span)?;
        Some((text, base))
    }
}

/// An edit that writes `text` at `at`, changing nothing there.
fn insert(at: u32, text: String) -> Edit {
    Edit { span: Span { start: at, end: at }, text }
}
