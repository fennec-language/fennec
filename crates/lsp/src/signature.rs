//! Signature help: inside a call's parentheses, the signature of what's called, with the
//! argument being written marked. A function's parameters, a struct's fields for its
//! constructor, an enum case's fields, and a method's parameters after `self`, found from the
//! object's type the way completion finds its members.
//!
//! The call is found by reading the text up to the cursor, skipping strings and comments, to
//! the innermost `(` still open. Half-written calls don't parse, so the text is mended the way
//! completion mends it: the call's parentheses and what's in them are taken out, and for a
//! method its name too, which leaves the object on its own.

use std::path::Path;

use fennec_kit::{Entry, Overlay};
use fennec_syntax::{Field, FnDef, Item, Span};

use crate::completion::{Known, Object, Written, object, owner};
use crate::json::Json;
use crate::references::imported;
use crate::text;

/// A call open at the cursor.
#[derive(Debug, PartialEq, Eq)]
struct Call {
    /// Where its `(` is.
    open: usize,
    /// How many arguments come before the one being written.
    commas: usize,
    /// Where the argument being written starts.
    argument: usize,
}

/// Reads `text` from its start, skipping strings and comments, and calls `bracket` on each
/// bracket and comma outside them, with its offset.
fn scan(text: &str, mut bracket: impl FnMut(usize, char) -> bool) {
    let mut quote = None;
    let mut chars = text.char_indices();
    while let Some((at, ch)) = chars.next() {
        if let Some(open) = quote {
            match ch {
                '\\' => {
                    chars.next();
                }
                '\n' => quote = None,
                _ if ch == open => quote = None,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '#' if !chars.by_ref().any(|(_, ch)| ch == '\n') => return,
            '(' | '[' | '{' | ')' | ']' | '}' | ',' if !bracket(at, ch) => return,
            _ => {}
        }
    }
}

/// The innermost call whose `(` is open at `offset`.
fn open_call(text: &str, offset: usize) -> Option<Call> {
    let mut open: Vec<(char, Call)> = Vec::new();
    scan(text.get(..offset)?, |at, ch| {
        match ch {
            ',' => {
                if let Some((_, call)) = open.last_mut() {
                    call.commas += 1;
                    call.argument = at + 1;
                }
            }
            '(' | '[' | '{' => open.push((ch, Call { open: at, commas: 0, argument: at + 1 })),
            _ => {
                open.pop();
            }
        }
        true
    });
    open.into_iter().rev().find(|(ch, _)| *ch == '(').map(|(_, call)| call)
}

/// Where the call whose `(` is at `open` ends: just past its `)`, or the end of the line the
/// cursor is on when it isn't closed yet.
fn call_end(text: &str, open: usize, offset: usize) -> usize {
    let line_end = text.get(offset..).and_then(|rest| rest.find('\n')).map_or(text.len(), |at| offset + at);
    let mut depth = 0_usize;
    let mut end = None;
    scan(text.get(open..).unwrap_or(""), |at, ch| {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth == 0 {
            end = Some(open + at + 1);
        }
        depth > 0
    });
    end.unwrap_or(line_end)
}

/// What's called.
#[derive(Debug, PartialEq, Eq)]
enum Callee {
    /// A function or a constructor, by name.
    Name(String),
    /// A method of the object before the dot at this offset.
    Member { dot: usize, name: String },
    /// `.Case(`.
    Case(String),
}

/// What's called by the call whose `(` is at `open`.
fn callee(text: &str, open: usize) -> Option<Callee> {
    let word = |ch: char| ch.is_ascii_alphanumeric() || ch == '_';
    let before = text.get(..open)?;
    let length: usize = before.chars().rev().take_while(|ch| word(*ch)).map(char::len_utf8).sum();
    let name = before.get(open - length..)?.to_owned();
    if name.is_empty() || name.starts_with(|ch: char| ch.is_ascii_digit()) {
        return None;
    }
    let ends_object = |ch: char| word(ch) || matches!(ch, ')' | ']' | '}' | '"' | '\'');
    Some(match before.get(..open - length)?.strip_suffix('.') {
        Some(object) if object.ends_with(ends_object) => Callee::Member { dot: open - length - 1, name },
        Some(_) => Callee::Case(name),
        None => Callee::Name(name),
    })
}

/// A signature as the protocol shows it.
struct Shape {
    label: String,
    /// Where each parameter is in the label, in UTF-16 units.
    params: Vec<(usize, usize)>,
    /// Each parameter's name, to find the one a keyword argument names.
    names: Vec<String>,
    doc: Option<String>,
}

impl Shape {
    /// `head(each, parameter) tail`, with each parameter as written.
    fn new(head: &str, params: Vec<(String, String)>, tail: &str, doc: Option<String>) -> Shape {
        let mut label = format!("{head}(");
        let mut places = Vec::new();
        let mut names = Vec::new();
        for (at, (name, written)) in params.into_iter().enumerate() {
            if at > 0 {
                label.push_str(", ");
            }
            let start = label.encode_utf16().count();
            label.push_str(&written);
            places.push((start, label.encode_utf16().count()));
            names.push(name);
        }
        label.push(')');
        label.push_str(tail);
        Shape { label, params: places, names, doc }
    }

    fn protocol(&self) -> Json {
        let params = self.params.iter().map(|(start, end)| {
            Json::object([("label", Json::Array(vec![Json::number(*start), Json::number(*end)]))])
        });
        let mut fields = vec![
            ("label".to_owned(), Json::string(self.label.as_str())),
            ("parameters".to_owned(), Json::Array(params.collect())),
        ];
        if let Some(doc) = &self.doc {
            let doc =
                Json::object([("kind", Json::string("markdown")), ("value", Json::string(doc.as_str()))]);
            fields.push(("documentation".to_owned(), doc));
        }
        Json::Object(fields)
    }
}

/// A span's text as written, on one line.
fn written(known: &Known<'_>, span: Span, place: Written) -> Option<String> {
    let (text, start, end) = known.place(span, place)?;
    let shown = text.get(start..end)?.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(shown.trim_end_matches([',', ':']).to_owned())
}

/// A function's or method's signature, without `self`.
fn function(known: &Known<'_>, head: &str, def: &FnDef, place: Written) -> Shape {
    let params = def
        .params
        .iter()
        .filter(|param| !param.is_self)
        .map(|param| {
            (param.name.clone(), written(known, param.span, place).unwrap_or_else(|| param.name.clone()))
        })
        .collect();
    let tail = def
        .ret
        .as_ref()
        .and_then(|ret| written(known, ret.span, place))
        .map_or_else(String::new, |ret| format!(" -> {ret}"));
    Shape::new(head, params, &tail, known.described(def.span, place).1)
}

/// A constructor's or a case's signature: its fields, each as its first line is written.
fn fields(known: &Known<'_>, head: &str, fields: &[Field], span: Span, place: Written) -> Shape {
    let params = fields
        .iter()
        .map(|field| {
            let first = known.described(field.span, place).0;
            (field.name.clone(), first.unwrap_or_else(|| field.name.clone()))
        })
        .collect();
    Shape::new(head, params, "", known.described(span, place).1)
}

/// What a name called as a function can be: a function, or a struct's or error's constructor.
/// A function the file imports from the standard library is named for its module there.
fn named(known: &Known<'_>, name: &str, path: &str) -> Vec<Shape> {
    let in_file = |span: Span| known.sources.file_at(span).is_some_and(|(file, ..)| file == path);
    let imports =
        imported(known.sources).into_iter().any(|(listed, span, _)| listed == name && in_file(span));
    let qualified =
        |declared: &str| imports && declared.rsplit_once('.').is_some_and(|(_, last)| last == name);
    let mut shapes = Vec::new();
    for (item, place) in known.items() {
        match item {
            Item::Function(def) if def.name == name || qualified(&def.name) => {
                shapes.push(function(known, name, def, place));
            }
            Item::Struct(def) | Item::Error(def) if def.name == name => {
                shapes.push(fields(known, name, &def.fields, def.span, place));
            }
            _ => {}
        }
    }
    shapes.truncate(1);
    shapes
}

/// What a method called on the object ending at `at` can be.
fn method(known: &Known<'_>, at: u32, name: &str) -> Vec<Shape> {
    let of = |owner: &str, takes_self: bool| -> Vec<Shape> {
        let methods = known.methods(owner).into_iter();
        let methods = methods.filter(|(def, _)| def.name == name && def.takes_self() == takes_self);
        methods.map(|(def, place)| function(known, name, def, place)).collect()
    };
    match object(known, at) {
        Some(Object::Value(ty)) => owner(&ty).map(|owner| of(owner, true)).unwrap_or_default(),
        Some(Object::Type(owner)) => {
            let mut shapes = of(owner, false);
            for (def, place) in known.types().filter(|(def, _)| def.name == owner) {
                let cases = def.cases.iter().filter(|case| case.name == name);
                shapes.extend(
                    cases.map(|case| {
                        fields(known, &format!("{owner}.{name}"), &case.fields, case.span, place)
                    }),
                );
            }
            shapes
        }
        Some(Object::Unknown) => {
            let own: Vec<&str> = known
                .types()
                .filter(|(_, place)| matches!(place, Written::Program))
                .map(|(def, _)| def.name.as_str())
                .collect();
            own.into_iter().flat_map(|owner| of(owner, true)).collect()
        }
        None => Vec::new(),
    }
}

/// What `.Case(` can be: each enum's case of that name.
fn case(known: &Known<'_>, name: &str) -> Vec<Shape> {
    let mut shapes = Vec::new();
    for (def, place) in known.types() {
        for found in def.cases.iter().filter(|case| case.name == name && !case.fields.is_empty()) {
            shapes.push(fields(known, &format!(".{name}"), &found.fields, found.span, place));
        }
    }
    shapes
}

/// Which parameter the argument being written is: the one it names, as in `power=3`, or the
/// one in its place.
fn active(text: &str, offset: usize, call: &Call, shape: &Shape) -> usize {
    let written = text.get(call.argument..offset).unwrap_or("").trim_start();
    let named = written
        .split_once('=')
        .filter(|(name, value)| {
            !value.starts_with('=') && name.trim().chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        })
        .and_then(|(name, _)| shape.names.iter().position(|param| param == name.trim()));
    named.unwrap_or(call.commas)
}

/// The signature help for byte `offset` of `text`, the open document `file` of `program`.
pub(crate) fn help(program: &Path, file: &Path, text: &str, offset: usize, mut overlay: Overlay) -> Json {
    let Some(call) = open_call(text, offset) else { return Json::Null };
    let Some(callee) = callee(text, call.open) else { return Json::Null };
    let cut = match &callee {
        Callee::Member { dot, .. } => *dot,
        Callee::Name(_) | Callee::Case(_) => call.open,
    };
    // A call that isn't closed yet may seem to end at the `)` of the call around it. Cutting it
    // only up to the cursor mends that one.
    let ends = [call_end(text, call.open, offset), offset];
    let Some(loaded) = ends.into_iter().find_map(|end| {
        let mended = format!("{}{}", text.get(..cut)?, text.get(end..)?);
        overlay.insert(file.to_path_buf(), mended);
        fennec_kit::load_with(program, &text::shown(program), Entry::Everything, &overlay).ok()
    }) else {
        return Json::Null;
    };
    let known = Known { module: &loaded.module, sources: &loaded.sources };
    let path = text::shown(file);
    let shapes = match &callee {
        Callee::Name(name) => named(&known, name, &path),
        Callee::Case(name) => case(&known, name),
        Callee::Member { dot, name } => {
            let at = loaded
                .sources
                .by_path(&path)
                .and_then(|(_, base)| base.checked_add(u32::try_from(*dot).ok()?));
            at.map(|at| method(&known, at, name)).unwrap_or_default()
        }
    };
    let Some(first) = shapes.first() else { return Json::Null };
    let parameter = active(text, offset, &call, first);
    Json::object([
        ("signatures", Json::Array(shapes.iter().map(Shape::protocol).collect())),
        ("activeSignature", Json::number(0)),
        ("activeParameter", Json::number(parameter)),
    ])
}

#[cfg(test)]
mod tests {
    use super::{Call, Callee, call_end, callee, open_call};

    #[test]
    fn the_open_call_skips_strings_comments_and_inner_brackets() {
        let text = "x = hit(fighter, [1, 2], \"(,\", ";
        assert_eq!(open_call(text, text.len()), Some(Call { open: 7, commas: 3, argument: text.len() - 1 }));
        assert_eq!(callee(text, 7), Some(Callee::Name("hit".to_owned())));
        let text = "y = point.moved(1, f(2)) # (\nz = (";
        assert_eq!(open_call(text, 23), Some(Call { open: 15, commas: 1, argument: 18 }));
        assert_eq!(callee(text, 15), Some(Callee::Member { dot: 9, name: "moved".to_owned() }));
        assert_eq!(call_end(text, 15, 23), 24);
        assert_eq!(callee("x = .Lit(", 8), Some(Callee::Case("Lit".to_owned())));
        assert_eq!(callee("x = (1 + 2)(", 11), None);
    }
}
