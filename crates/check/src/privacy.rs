//! `_name` fields and methods: private to the file that declares their type. Code written in
//! that file can read, write, set or call one: the type's methods, the file's functions and other
//! types, an `extend` and the tests written there. Code in any other file, of the same kit or
//! another, can't, so it can't forge a value with a `_` field set to whatever it likes, which
//! capabilities like a network connection rely on.

use fennec_syntax::{Arg, Diagnostic, Edit, Field, Item, Module, Sources, Span, TokenKind, TypeDef};

use crate::function::FnCx;

/// Why a top-level name doesn't start with `_`, which is also how its error is known when its
/// fix is added.
pub(crate) const TOP_LEVEL_HELP: &str =
    "top-level names are already private to the kit; `_` marks a private field or method";

/// Reports each top-level name in a module that starts with `_`: a function, a type, an
/// interface, an alias or a constant. The standard library's modules are held to it too, by the
/// last part of the names their functions are given, like `json.article`.
pub(crate) fn top_level_underscores(module: &Module) -> Vec<Diagnostic> {
    let mut found = Vec::new();
    for item in &module.items {
        let (name, span) = match item {
            Item::Function(def) => (&def.name, def.span),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => (&def.name, def.span),
            Item::Interface(interface) => (&interface.name, interface.span),
            Item::TypeAlias { name, span, .. } | Item::Const { name, span, .. } => (name, *span),
            _ => continue,
        };
        let own = name.rsplit('.').next().unwrap_or(name);
        if own.len() > 1 && own.starts_with('_') {
            let message = format!("`{name}` is a top-level name, so it doesn't start with `_`");
            found.push(Diagnostic::new(message, span).with_help(TOP_LEVEL_HELP));
        }
    }
    found
}

/// A type's named constructors: its methods without `self` that give back one of it.
pub(crate) fn named_constructors(def: &TypeDef) -> Vec<&str> {
    def.methods
        .iter()
        .filter(|method| !method.takes_self())
        .filter(|method| method.ret.as_ref().is_some_and(|ret| ret.name == def.name || ret.name == "Self"))
        .map(|method| method.name.as_str())
        .collect()
}

/// Gives each top-level `_name` error its fix, which renames the name everywhere it's written
/// in the program's files, without the `_`. It's made only where that can't change what the
/// program means: each place the name is written is a name, not a field or method of that name
/// or one reached with `.`, the name isn't in an f-string, and the new name isn't written
/// anywhere already or one the language provides.
pub fn add_rename_fixes(diagnostics: &mut [Diagnostic], module: &Module, sources: &Sources) {
    for diagnostic in diagnostics.iter_mut() {
        if diagnostic.help.as_deref() != Some(TOP_LEVEL_HELP) || !diagnostic.fixes.is_empty() {
            continue;
        }
        let Some(old) = diagnostic.message.split('`').nth(1) else { continue };
        let new = old.trim_start_matches('_');
        if new.is_empty() || new.starts_with(|c: char| c.is_ascii_digit()) || crate::provided_everywhere(new)
        {
            continue;
        }
        if let Some(edits) = renamed(module, sources, old, new) {
            diagnostic.fixes = edits;
        }
    }
}

fn renamed(module: &Module, sources: &Sources, old: &str, new: &str) -> Option<Vec<Edit>> {
    let member = |name: &str| name == old;
    let has_member = module.items.iter().any(|item| match item {
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
            def.fields.iter().any(|field| member(&field.name))
                || def.methods.iter().any(|method| member(&method.name))
                || def.cases.iter().any(|case| member(&case.name))
        }
        Item::Interface(interface) => interface.methods.iter().any(|method| member(&method.name)),
        Item::Extend(extension) => extension.methods.iter().any(|method| member(&method.name)),
        _ => false,
    });
    if has_member {
        return None;
    }
    let mut edits = Vec::new();
    for (path, text) in sources.files() {
        let (_, base) = sources.by_path(path)?;
        let lexed = fennec_syntax::lex_source(text).ok()?;
        let mut after_dot = false;
        for token in &lexed.tokens {
            let start = usize::try_from(token.span.start).ok()?;
            let end = usize::try_from(token.span.end).ok()?;
            let written = text.get(start..end)?;
            match token.kind {
                TokenKind::Name if written == new => return None,
                TokenKind::Name if written == old => {
                    if after_dot {
                        return None;
                    }
                    let span = Span { start: base + token.span.start, end: base + token.span.end };
                    edits.push(Edit { span, text: new.to_owned() });
                }
                TokenKind::FStr if has_word(written, old) || has_word(written, new) => return None,
                _ => {}
            }
            after_dot = token.kind == TokenKind::Punct && written == ".";
        }
    }
    (!edits.is_empty()).then_some(edits)
}

/// Whether `name` is written in `text` as a whole word.
fn has_word(text: &str, name: &str) -> bool {
    let word = |ch: char| ch.is_ascii_alphanumeric() || ch == '_';
    text.match_indices(name).any(|(at, _)| {
        let before = text.get(..at).and_then(|text| text.chars().next_back());
        let after = text.get(at + name.len()..).and_then(|text| text.chars().next());
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

/// Whether `declared` and `used` are in the same file: one of a program's `files`, or one of
/// the standard library's. A program checked as one text, with no files listed, is one file.
/// A span the compiler wrote at no place of its own, `0..0`, is in no file, as
/// [`crate::import_place`] has it, so it never counts as the first file's code.
fn same_file(files: &[Span], declared: Span, used: Span) -> bool {
    match (crate::import_place(files, declared), crate::import_place(files, used)) {
        (crate::Place::File(declared), crate::Place::File(used)) => declared == used,
        (crate::Place::Std, crate::Place::Std) => {
            let file = |span: Span| fennec_syntax::prelude_file_at(span).map(|(name, _, _)| name);
            file(declared).is_some_and(|name| file(used) == Some(name))
        }
        _ => false,
    }
}

impl FnCx<'_> {
    /// Whether code at `span` reaches `owner`'s `_` names: code written in the file that declares
    /// `owner`, its methods and the functions nested in them included, or a method generated to
    /// save it, which no program writes.
    pub(crate) fn inside(&self, owner: &str, span: Span) -> bool {
        let program = &self.checker.program;
        let own_method =
            self.key.strip_prefix(owner).and_then(|rest| rest.strip_prefix('.')).is_some_and(|rest| {
                let method = rest.split('.').next().unwrap_or(rest);
                // The methods generated to save a type save all of it, and no program can write them.
                let generated = program
                    .functions
                    .get(&format!("{owner}.{method}"))
                    .is_some_and(|entry| !entry.prelude && entry.def.span.in_prelude());
                generated
                    || program
                        .type_info(owner)
                        .is_some_and(|info| info.def.methods.iter().any(|def| def.name == method))
            });
        own_method
            || program.type_info(owner).is_some_and(|info| {
                same_file(&program.files, info.def.span, span.copied_from(&program.copied))
            })
    }

    /// Reports a use of `owner`'s `_` field or method from outside it. One written in the
    /// standard library's own text is a std type's method's, as when a `with` source like
    /// `channel[T]` is written in where it's used: what reaches the name is still its own code.
    pub(crate) fn private_member(&mut self, owner: &str, member: &str, span: Span) {
        if !member.starts_with('_') || self.checker.lets_off(span) || self.inside(owner, span) {
            return;
        }
        let what = if self.checker.program.method(owner, member).is_some() { "call" } else { "reach" };
        self.error(Diagnostic::new(format!("`{member}` is private to `{owner}`'s file"), span).with_help(format!(
            "only code in the file that declares `{owner}` can {what} `_` names: go through one of its other methods"
        )));
    }

    /// Reports arguments that set `owner`'s `_` fields when it's made from outside it, by name
    /// or by position.
    pub(crate) fn private_fields_given(&mut self, owner: &str, fields: &[Field], args: &[Arg], span: Span) {
        // From outside, values given by position go to the public fields alone.
        let outside = !self.inside(owner, span);
        let mut settable = fields.iter().filter(|field| !(outside && field.name.starts_with('_')));
        for arg in args {
            let field = if let Some(name) = &arg.name {
                fields.iter().find(|field| &field.name == name)
            } else {
                settable.next()
            };
            if let Some(field) = field {
                self.private_member(owner, &field.name, arg.value.span);
            }
        }
    }
}
