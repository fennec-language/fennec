//! Completion: what can be written where the cursor is. After `object.`, the members of the
//! object's type; after a `.` with nothing before it, enum cases; anywhere else, the names in
//! scope there, the program's top-level names, the builtins and the keywords.
//!
//! While someone types, the program seldom parses: `point.` is half an expression. So the text
//! is mended first. After a dot, the dot, what's typed after it and a call or index right after
//! that are taken out, which leaves the object on its own; an object that starts its statement is
//! given to `_`, since a value left unused doesn't check. Anywhere else, what's typed becomes a
//! placeholder name. The mended
//! program is loaded like any other, and the walk go to definition makes finds the names in
//! scope at the placeholder, or the expression that ends at the dot. When the mended program
//! checks, that expression's type says which members it has; when it doesn't, every member of
//! the program's own types is a candidate, as go to definition does.

use std::collections::BTreeSet;
use std::path::Path;

use fennec_check::Type;
use fennec_kit::{Entry, Overlay};
use fennec_syntax::{FnDef, Item, Module, Sources, Span, TypeDef};

use crate::declaration::doc_comment;
use crate::definition::around;
use crate::json::Json;
use crate::references::imported;
use crate::text;

/// What takes the place of the word being typed, so the program parses.
const PLACEHOLDER: &str = "completing_here_";

/// The builtins, as the checker's `BUILTINS` lists them, which no file declares. `Ok` and `Err`
/// are left out: they're there only for what lowering writes.
const BUILTINS: [&str; 20] = [
    "all",
    "any",
    "bytes",
    "copy",
    "enumerate",
    "float",
    "int",
    "list",
    "max",
    "min",
    "parallel",
    "range",
    "replace",
    "reversed",
    "round",
    "set",
    "sorted",
    "str",
    "sum",
    "zip",
];

/// The words the language keeps, with those that are keywords only where a statement or a
/// declaration starts.
const KEYWORDS: [&str; 40] = [
    "and",
    "as",
    "assert",
    "bench",
    "break",
    "case",
    "const",
    "continue",
    "def",
    "elif",
    "else",
    "enum",
    "error",
    "except",
    "export",
    "extend",
    "fail",
    "False",
    "for",
    "from",
    "if",
    "import",
    "in",
    "interface",
    "is",
    "match",
    "mut",
    "None",
    "not",
    "or",
    "pass",
    "ref",
    "return",
    "shared",
    "struct",
    "test",
    "True",
    "while",
    "with",
    "yield",
];

/// The methods the engines give built-in types, as the checker's `method` knows them. Those
/// written in Fennec come from the standard library's `extend` blocks instead.
fn engine_methods(ty: &Type) -> &'static [&'static str] {
    match ty {
        // `len(x)` and `abs(x)` are builtins: `x.len()` and `x.abs()` are refused, so they're
        // never offered.
        Type::Str => &[
            "upper", "lower", "strip", "lstrip", "rstrip", "split", "replace", "find", "join", "encode",
            "code", "codes",
        ],
        Type::List(_) => &["append", "remove", "insert", "clear", "remove_where", "sort", "pop"],
        Type::Set(_) => &["add", "discard", "remove", "clear"],
        Type::Dict(..) => &["pop", "get", "keys", "values", "items", "clear"],
        Type::Table(_) => {
            &["add", "remove", "get", "modify", "remove_where", "values", "ids", "items", "clear"]
        }
        Type::Bytes => &["decode"],
        _ => &[],
    }
}

/// The protocol's numbers for the kinds of completion.
const METHOD: usize = 2;
const FUNCTION: usize = 3;
const FIELD: usize = 5;
const VARIABLE: usize = 6;
const CLASS: usize = 7;
const INTERFACE: usize = 8;
const MODULE: usize = 9;
const ENUM: usize = 13;
const KEYWORD: usize = 14;
const ENUM_MEMBER: usize = 20;
const CONSTANT: usize = 21;
const STRUCT: usize = 22;

/// What the word at the cursor is.
#[derive(Clone, Copy)]
enum Completing {
    /// A member of the object before the dot at this offset.
    Member { dot: usize },
    /// An enum case, after a dot with no object.
    Case,
    /// A name.
    Name,
}

/// The serialization hooks, `Serializable.serialize` and `Deserializable.deserialize`, which code
/// the compiler writes calls by name and a program hardly ever does, so they'd only crowd what's
/// offered after a dot, as on every `str`. They're offered once three letters or more of one are
/// typed.
const HOOKS: [&str; 2] = ["serialize", "deserialize"];

/// What's offered at byte `offset` of `text`, without the serialization hooks after a dot until
/// three letters or more of one are typed, and whether the editor should ask again as more is
/// typed: only while what's typed could still become a hook's first three letters, so typing
/// anything else doesn't ask again for each letter.
pub(crate) fn without_hooks(items: Vec<Json>, text: &str, offset: usize) -> (Vec<Json>, bool) {
    let Some((start, _, Completing::Member { .. })) = completing(text, offset) else { return (items, false) };
    let typed = text.get(start..offset).unwrap_or("");
    let shown = |label: &str| !HOOKS.contains(&label) || (typed.len() >= 3 && label.starts_with(typed));
    let before = items.len();
    let kept: Vec<Json> =
        items.into_iter().filter(|item| item.get("label").and_then(Json::as_str).is_none_or(shown)).collect();
    let more = kept.len() < before && HOOKS.iter().any(|hook| hook.starts_with(typed));
    (kept, more)
}

/// The word being typed at `offset`, as where it starts and ends, and what it is. None in a
/// comment.
fn completing(text: &str, offset: usize) -> Option<(usize, usize, Completing)> {
    let word = |ch: char| ch.is_ascii_alphanumeric() || ch == '_';
    let before = text.get(..offset)?;
    let line = before.rsplit('\n').next().unwrap_or(before);
    if line.contains('#') {
        return None;
    }
    let typed: usize = before.chars().rev().take_while(|ch| word(*ch)).map(char::len_utf8).sum();
    let start = offset - typed;
    let rest: usize = text.get(offset..)?.chars().take_while(|ch| word(*ch)).map(char::len_utf8).sum();
    let ends_object = |ch: char| word(ch) || matches!(ch, ')' | ']' | '}' | '"' | '\'');
    let kind = match text.get(..start)?.strip_suffix('.') {
        Some(object) if object.ends_with(ends_object) => Completing::Member { dot: start - 1 },
        Some(_) => Completing::Case,
        None => Completing::Name,
    };
    Some((start, offset + rest, kind))
}

/// The text with the word being typed mended, so it parses and checks, and how far that moved
/// the dot. After a dot, the word and a call or index right after it come out with the dot, so
/// `x = text.sp|lit(",")` reads `x = text`; an object that starts its statement is given to `_`,
/// as `_ = names`. A line still being written, a block's head with no `:` yet or one with a
/// bracket open, as `if text.` or `stdout.print(text.`, can't parse however the word is mended, so
/// the line becomes the object given to `_`. Gives back the mended text and where its dot is.
fn mended(text: &str, start: usize, end: usize, kind: Completing) -> Option<(String, usize)> {
    match kind {
        Completing::Member { dot } => {
            let after = past_group(text, end);
            let object = object_start(text, dot);
            let line = text.get(..object)?.rfind('\n').map_or(0, |newline| newline + 1);
            let line_end = text.get(dot..)?.find('\n').map_or(text.len(), |newline| dot + newline);
            let head = text.get(line..object)?;
            let written = text.get(object..dot)?;
            if head.trim().is_empty() && depth(text, object) == 0 {
                let mended = format!("{}_ = {written}{}", text.get(..object)?, text.get(after..)?);
                return Some((mended, dot + "_ = ".len()));
            }
            if unfinished(text.get(line..line_end)?) {
                let indent = head.get(..head.len() - head.trim_start().len()).unwrap_or("");
                let mended = format!("{}{indent}_ = {written}{}", text.get(..line)?, text.get(line_end..)?);
                return Some((mended, line + indent.len() + "_ = ".len() + written.len()));
            }
            Some((format!("{}{written}{}", text.get(..object)?, text.get(after..)?), dot))
        }
        Completing::Case | Completing::Name => {
            Some((format!("{}{PLACEHOLDER}{}", text.get(..start)?, text.get(end..)?), start))
        }
    }
}

/// Whether a line is still being written: a block's head with no `:` at its end yet, or one
/// with a bracket it doesn't close.
fn unfinished(line: &str) -> bool {
    let words = line.trim_start();
    let head =
        ["if ", "elif ", "while ", "for ", "with ", "match "].iter().any(|word| words.starts_with(word));
    head && !words.trim_end().ends_with(':') || depth(line, line.len()) > 0
}

/// Where the object that ends at the dot at byte `dot` starts: back over names, dots, and
/// brackets or a string each taken whole, as in `units[at].pos` or `f"{a}".upper`.
fn object_start(text: &str, dot: usize) -> usize {
    let bytes = text.as_bytes();
    let mut at = dot;
    loop {
        let before = at;
        match at.checked_sub(1).and_then(|last| bytes.get(last)) {
            Some(b')' | b']' | b'}') => {
                let mut nested = 0_usize;
                while let Some(last) = at.checked_sub(1) {
                    at = last;
                    match bytes.get(at) {
                        Some(b')' | b']' | b'}') => nested += 1,
                        Some(b'(' | b'[' | b'{') => {
                            nested -= 1;
                            if nested == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some(b'"') => {
                at -= 1;
                while at > 0 && bytes.get(at - 1) != Some(&b'"') {
                    at -= 1;
                }
                at = at.saturating_sub(1);
            }
            _ => {}
        }
        while at > 0 && bytes.get(at - 1).is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_') {
            at -= 1;
        }
        // A chain goes on through a dot before it: `a.b.c`.
        if at > 0 && bytes.get(at - 1) == Some(&b'.') && at != before {
            at -= 1;
            continue;
        }
        return at;
    }
}

/// The end of a call's or index's brackets right at byte `at`, or `at` when none starts there.
fn past_group(text: &str, at: usize) -> usize {
    let bytes = text.as_bytes();
    if !matches!(bytes.get(at), Some(b'(' | b'[')) {
        return at;
    }
    let mut nested = 0_usize;
    let mut in_text = false;
    for (offset, byte) in bytes.iter().enumerate().skip(at) {
        match byte {
            b'"' => in_text = !in_text,
            b'(' | b'[' | b'{' if !in_text => nested += 1,
            b')' | b']' | b'}' if !in_text => {
                nested -= 1;
                if nested == 0 {
                    return offset + 1;
                }
            }
            b'\n' if !in_text && nested == 0 => return at,
            _ => {}
        }
    }
    at
}

/// How many brackets are open at byte `at`, outside strings and comments, so a line inside a
/// call's arguments isn't taken for a statement of its own.
fn depth(text: &str, at: usize) -> usize {
    let mut nested = 0_usize;
    let mut in_text = false;
    let mut in_comment = false;
    for byte in text.as_bytes().iter().take(at) {
        match byte {
            b'\n' => in_comment = false,
            _ if in_comment => {}
            b'#' if !in_text => in_comment = true,
            b'"' => in_text = !in_text,
            b'(' | b'[' | b'{' if !in_text => nested += 1,
            b')' | b']' | b'}' if !in_text => nested = nested.saturating_sub(1),
            _ => {}
        }
    }
    nested
}

/// One thing to offer.
struct Candidate {
    label: String,
    kind: usize,
    detail: Option<String>,
    doc: Option<String>,
}

/// The candidates offered, in the order they're shown, each name once.
#[derive(Default)]
struct Offered {
    candidates: Vec<Candidate>,
    labels: BTreeSet<String>,
}

impl Offered {
    fn add(&mut self, label: &str, kind: usize, detail: Option<String>, doc: Option<String>) {
        let usable = label.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_')
            && label.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            && label != PLACEHOLDER;
        if usable && self.labels.insert(label.to_owned()) {
            self.candidates.push(Candidate { label: label.to_owned(), kind, detail, doc });
        }
    }

    fn items(self) -> Vec<Json> {
        self.candidates
            .into_iter()
            .enumerate()
            .map(|(at, candidate)| {
                let mut fields = vec![
                    ("label".to_owned(), Json::string(candidate.label)),
                    ("kind".to_owned(), Json::number(candidate.kind)),
                    ("sortText".to_owned(), Json::string(format!("{at:05}"))),
                ];
                if let Some(detail) = candidate.detail {
                    fields.push(("detail".to_owned(), Json::string(detail)));
                }
                if let Some(doc) = candidate.doc {
                    let doc =
                        Json::object([("kind", Json::string("markdown")), ("value", Json::string(doc))]);
                    fields.push(("documentation".to_owned(), doc));
                }
                Json::Object(fields)
            })
            .collect()
    }
}

/// Where a declaration is written: in one of the program's files, or in the standard library's
/// file with this text, whose spans the program's sources don't hold.
#[derive(Clone, Copy)]
pub(crate) enum Written {
    Program,
    Library(&'static str),
}

/// What's declared: the program's own items, and the standard library's, which loading leaves
/// for the checker to add.
pub(crate) struct Known<'a> {
    pub(crate) module: &'a Module,
    pub(crate) sources: &'a Sources,
    /// The module's types, when they're worked out already, as a quick analysis has them: `None`
    /// checks the whole module to find an object's type.
    pub(crate) checked: Option<&'a fennec_check::Checked>,
}

impl<'a> Known<'a> {
    pub(crate) fn items(&self) -> impl Iterator<Item = (&'a Item, Written)> + use<'a> {
        let library =
            fennec_syntax::prelude_modules().unwrap_or_default().iter().zip(fennec_syntax::prelude());
        let library = library
            .flat_map(|(module, text)| module.items.iter().map(move |item| (item, Written::Library(text))));
        self.module.items.iter().map(|item| (item, Written::Program)).chain(library)
    }

    /// The struct, enum and error types.
    pub(crate) fn types(&self) -> impl Iterator<Item = (&'a TypeDef, Written)> + use<'a> {
        self.items().filter_map(|(item, written)| match item {
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => Some((def, written)),
            _ => None,
        })
    }

    /// The methods declared on `owner`, in its own declaration or an `extend`.
    pub(crate) fn methods(&self, owner: &str) -> Vec<(&'a FnDef, Written)> {
        let mut found = Vec::new();
        for (item, written) in self.items() {
            let methods = match item {
                Item::Struct(def) | Item::Enum(def) | Item::Error(def) if def.name == owner => &def.methods,
                Item::Extend(extension) if extension.name == owner => &extension.methods,
                _ => continue,
            };
            found.extend(methods.iter().map(|method| (method, written)));
        }
        found
    }

    /// The text a declaration is written in, with the span's bytes in it.
    pub(crate) fn place(&self, span: Span, written: Written) -> Option<(&'a str, usize, usize)> {
        let (text, base) = match written {
            // Each library file's spans start where its own range does.
            Written::Library(text) => {
                let base = fennec_syntax::prelude_file_at(span).map_or(Span::PRELUDE, |(_, _, base)| base);
                (text, base)
            }
            Written::Program => {
                let (_, text, base) = self.sources.file_at(span)?;
                (text, base)
            }
        };
        let local = |at: u32| usize::try_from(at.saturating_sub(base)).ok();
        Some((text, local(span.start)?, local(span.end)?))
    }

    /// A declaration's first line as written, and its doc comment, when it has one.
    pub(crate) fn described(&self, span: Span, written: Written) -> (Option<String>, Option<String>) {
        let Some((text, start, _)) = self.place(span, written) else { return (None, None) };
        let line_start =
            text.get(..start).and_then(|before| before.rfind('\n')).map_or(0, |newline| newline + 1);
        let first = text.get(start..).and_then(|rest| rest.lines().next());
        let detail = first.map(|line| line.trim().trim_end_matches(':').to_owned());
        let doc = doc_comment(text, line_start);
        (detail, (!doc.is_empty()).then_some(doc))
    }

    /// Offers something declared in `span`, with its first line and its doc comment.
    fn offer(&self, offered: &mut Offered, label: &str, kind: usize, span: Span, written: Written) {
        let (detail, doc) = self.described(span, written);
        offered.add(label, kind, detail, doc);
    }
}

/// The text mended for completing at a place, as `mend` makes it.
pub(crate) struct Mend {
    /// What the word at the cursor is.
    kind: Completing,
    /// Where the word being typed starts, in the text as written.
    start: usize,
    /// The text with the word mended, so it parses.
    pub(crate) text: String,
    /// Where the dot before a member is, in the mended text.
    dot: usize,
}

/// The text at byte `offset` mended for completing there, or `None` where nothing's offered,
/// as in a comment.
pub(crate) fn mend(text: &str, offset: usize) -> Option<Mend> {
    let (start, end, kind) = completing(text, offset)?;
    let (mended, dot) = mended(text, start, end, kind)?;
    Some(Mend { kind, start, text: mended, dot })
}

/// What can be written at byte `offset` of `text`, the open document `file` of `program`,
/// loading and checking all of the program as mended. A session answers from a quick analysis
/// of the edited file instead when it can, with `offer`.
pub(crate) fn complete(
    program: &Path,
    file: &Path,
    text: &str,
    offset: usize,
    mut overlay: Overlay,
) -> Vec<Json> {
    let Some(mend) = mend(text, offset) else { return Vec::new() };
    overlay.insert(file.to_path_buf(), mend.text.clone());
    let loaded = fennec_kit::load_with(program, &text::shown(program), Entry::Everything, &overlay).ok();
    let Some(loaded) = loaded else {
        let mut offered = Offered::default();
        if let Completing::Name = mend.kind {
            builtins_and_keywords(&mut offered);
        }
        return offered.items();
    };
    let known = Known { module: &loaded.module, sources: &loaded.sources, checked: None };
    offer(&mend, &text::shown(file), &known)
}

/// What can be written where `mend` was made, in the file at `path`, from what `known` declares.
pub(crate) fn offer(mend: &Mend, path: &str, known: &Known<'_>) -> Vec<Json> {
    let mut offered = Offered::default();
    let at = |offset: usize| {
        let (_, base) = known.sources.by_path(path)?;
        base.checked_add(u32::try_from(offset).ok()?)
    };
    match mend.kind {
        Completing::Member { .. } => {
            if let Some(at) = at(mend.dot) {
                members(&mut offered, known, at);
            }
        }
        Completing::Case => cases(&mut offered, known),
        Completing::Name => {
            if let Some(at) = at(mend.start + 1) {
                names(&mut offered, known, path, at);
            }
        }
    }
    offered.items()
}

/// The name a type's members are declared under: a struct's own, or `str` for text.
pub(crate) fn owner(ty: &Type) -> Option<&str> {
    Some(match ty {
        Type::Named(name) | Type::Generic(name, _) => name,
        Type::Str => "str",
        Type::Bytes => "bytes",
        Type::Int => "int",
        Type::Float => "float",
        Type::List(_) => "list",
        Type::Set(_) => "set",
        Type::Dict(..) => "dict",
        _ => return None,
    })
}

/// What the object that ends at a place is.
pub(crate) enum Object<'a> {
    /// A value of this type.
    Value(Type),
    /// The type with this name itself, as in `Type.`.
    Type(&'a str),
    /// A standard library module, as in `json.`, which the file imports.
    Module(&'static str),
    /// A value whose type isn't known, since the program doesn't check.
    Unknown,
}

/// What the expression that ends at `at` is, if one does.
pub(crate) fn object<'a>(known: &Known<'a>, at: u32) -> Option<Object<'a>> {
    let found = around(known.module, known.sources, at);
    let (object, function) = found.ending?;
    let written = known.place(object, Written::Program).and_then(|(text, start, end)| text.get(start..end));
    let local = |name: &str| found.names.iter().any(|(declared, _)| declared == name);
    let is_type = |name: &str| known.types().any(|(def, _)| def.name == name);
    if let Some(name) = written.filter(|name| !local(name) && is_type(name)) {
        return Some(Object::Type(name));
    }
    // A module isn't a value, so the mended program doesn't check: it's known by its name.
    if let Some(module) = written.filter(|name| !local(name)).and_then(fennec_syntax::std_module) {
        return Some(Object::Module(module));
    }
    let ty = match known.checked {
        Some(checked) => checked.types.body(&function).and_then(|body| body.expr(object)).cloned(),
        None => fennec_check::check_typed(known.module)
            .ok()
            .and_then(|checked| checked.types.body(&function)?.expr(object).cloned()),
    };
    Some(ty.map_or(Object::Unknown, Object::Value))
}

/// The members of the object that ends at `at`: what its type has, or for `Type.` what the
/// type itself has.
fn members(offered: &mut Offered, known: &Known<'_>, at: u32) {
    match object(known, at) {
        Some(Object::Type(name)) => type_members(offered, known, name),
        Some(Object::Module(module)) => {
            for name in fennec_syntax::std_exports(module) {
                let kind = if name.starts_with(char::is_uppercase) { CLASS } else { FUNCTION };
                offered.add(name, kind, Some(format!("std.{module}")), None);
            }
        }
        Some(Object::Value(ty)) => {
            if let Some(owner) = owner(&ty) {
                value_members(offered, known, owner);
            }
            for name in engine_methods(&ty) {
                offered.add(name, METHOD, Some("built-in".to_owned()), None);
            }
        }
        // Without types, every member of the program's own types might be meant.
        Some(Object::Unknown) => {
            let own: Vec<&str> = known
                .types()
                .filter(|(_, written)| matches!(written, Written::Program))
                .map(|(def, _)| def.name.as_str())
                .collect();
            for name in own {
                value_members(offered, known, name);
            }
        }
        None => {}
    }
}

/// What a value of type `owner` has: its fields, then its methods. The standard library's
/// names that start with `_` are its own business.
fn value_members(offered: &mut Offered, known: &Known<'_>, owner: &str) {
    let shown = |name: &str, written: Written| matches!(written, Written::Program) || !name.starts_with('_');
    for (def, written) in known.types().filter(|(def, _)| def.name == owner) {
        for field in def.fields.iter().filter(|field| shown(&field.name, written)) {
            known.offer(offered, &field.name, FIELD, field.span, written);
        }
    }
    for (method, written) in known.methods(owner) {
        if method.takes_self() && shown(&method.name, written) {
            known.offer(offered, &method.name, METHOD, method.span, written);
        }
    }
}

/// What `Type.` reaches: an enum's cases, and the methods that don't take `self`.
fn type_members(offered: &mut Offered, known: &Known<'_>, owner: &str) {
    for (def, written) in known.types().filter(|(def, _)| def.name == owner) {
        for case in &def.cases {
            known.offer(offered, &case.name, ENUM_MEMBER, case.span, written);
        }
    }
    for (method, written) in known.methods(owner) {
        if !method.takes_self() {
            known.offer(offered, &method.name, FUNCTION, method.span, written);
        }
    }
}

/// Whether a top-level name can be used without importing it: the program's own, and the
/// standard library's that are everywhere.
fn everywhere(name: &str, written: Written) -> bool {
    matches!(written, Written::Program)
        || (fennec_syntax::importable_type(name).is_none() && !name.starts_with('_'))
}

/// Every enum case a `.Case` can name, with its enum beside it.
fn cases(offered: &mut Offered, known: &Known<'_>) {
    let enums = known.items().filter_map(|(item, written)| match item {
        Item::Enum(def) if everywhere(&def.name, written) => Some((def, written)),
        _ => None,
    });
    for (def, written) in enums {
        for case in &def.cases {
            let (_, doc) = known.described(case.span, written);
            offered.add(&case.name, ENUM_MEMBER, Some(format!("{}.{}", def.name, case.name)), doc);
        }
    }
}

/// The kind of completion a top-level item is, with its name and where it's declared.
fn top_level(item: &Item) -> Option<(&str, usize, Span)> {
    Some(match item {
        Item::Function(def) => (def.name.as_str(), FUNCTION, def.span),
        Item::Struct(def) => (def.name.as_str(), STRUCT, def.span),
        Item::Enum(def) => (def.name.as_str(), ENUM, def.span),
        Item::Error(def) => (def.name.as_str(), CLASS, def.span),
        Item::Interface(interface) => (interface.name.as_str(), INTERFACE, interface.span),
        Item::TypeAlias { name, span, .. } => (name.as_str(), CLASS, *span),
        Item::Const { name, span, .. } => (name.as_str(), CONSTANT, *span),
        Item::Extend(_) | Item::Test(_) | Item::Import(_) | Item::Stmt(_) => return None,
    })
}

/// The names that can be written at `at` in the file at `path`: those in scope, innermost
/// first, then the program's top-level names, what the file imports, the standard library's
/// names that are everywhere, the builtins and the keywords.
fn names(offered: &mut Offered, known: &Known<'_>, path: &str, at: u32) {
    for (name, declared) in around(known.module, known.sources, at).names.iter().rev() {
        let (detail, doc) = known.described(*declared, Written::Program);
        // A local's line is the statement that declares it, which says little about it.
        match detail {
            Some(line) if line.starts_with("def ") => offered.add(name, FUNCTION, Some(line), doc),
            _ => offered.add(name, VARIABLE, None, None),
        }
    }
    let top: Vec<(&str, usize, Span, Written)> = known
        .items()
        .filter_map(|(item, written)| top_level(item).map(|(name, kind, span)| (name, kind, span, written)))
        .collect();
    let mut own: Vec<_> = top.iter().filter(|(.., written)| matches!(written, Written::Program)).collect();
    own.sort_by_key(|(name, ..)| *name);
    for (name, kind, span, written) in own {
        known.offer(offered, name, *kind, *span, *written);
    }
    let in_file = |span: Span| known.sources.file_at(span).is_some_and(|(file, ..)| file == path);
    for (name, ..) in imported(known.sources).into_iter().filter(|(_, span, _)| in_file(*span)) {
        match top.iter().find(|(declared, ..)| *declared == name) {
            Some((_, kind, span, written)) => known.offer(offered, &name, *kind, *span, *written),
            None => offered.add(&name, MODULE, None, None),
        }
    }
    let mut library: Vec<_> = top
        .iter()
        .filter(|(name, _, _, written)| matches!(written, Written::Library(_)) && everywhere(name, *written))
        .collect();
    library.sort_by_key(|(name, ..)| *name);
    for (name, kind, span, written) in library {
        known.offer(offered, name, *kind, *span, *written);
    }
    builtins_and_keywords(offered);
}

fn builtins_and_keywords(offered: &mut Offered) {
    for name in BUILTINS {
        offered.add(name, FUNCTION, Some("built-in".to_owned()), None);
    }
    for word in KEYWORDS {
        offered.add(word, KEYWORD, None, None);
    }
}

#[cfg(test)]
mod tests {
    use super::{Completing, completing, mended};

    #[test]
    fn the_word_being_typed_is_mended_so_the_program_parses() {
        let text = "x = point.na\n";
        let Some((start, end, kind)) = completing(text, 12) else { panic!("nothing to complete") };
        assert!(matches!(kind, Completing::Member { dot: 9 }));
        assert_eq!(mended(text, start, end, kind), Some(("x = point\n".to_owned(), 9)));
        let text = "x = .Re\n";
        let Some((start, end, kind)) = completing(text, 7) else { panic!("nothing to complete") };
        assert!(matches!(kind, Completing::Case));
        assert_eq!(mended(text, start, end, kind), Some(("x = .completing_here_\n".to_owned(), 5)));
        // An object that starts its statement is given to `_`, and the dot moves with it.
        let text = "def f(names: list[str]):\n    names.ap\n";
        let Some((start, end, kind)) = completing(text, text.len() - 1) else {
            panic!("nothing to complete")
        };
        let Some((statement, dot)) = mended(text, start, end, kind) else { panic!("nothing mended") };
        assert_eq!(statement, "def f(names: list[str]):\n    _ = names\n");
        assert_eq!(statement.get(dot - 5..dot), Some("names"));
        // A line still being written becomes the object given to `_`.
        let text = "def f(text: str):\n    if text.\n";
        let Some((start, end, kind)) = completing(text, text.len() - 1) else {
            panic!("nothing to complete")
        };
        assert_eq!(
            mended(text, start, end, kind).map(|(text, _)| text).as_deref(),
            Some("def f(text: str):\n    _ = text\n")
        );
        let Some((_, _, kind)) = completing("x = f(", 6) else { panic!("nothing to complete") };
        assert!(matches!(kind, Completing::Name));
        assert!(completing("x = 1  # po", 11).is_none());
    }
}
