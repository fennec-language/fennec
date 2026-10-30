//! `std.re`: the methods of a `Regex`, which run the `fennec-re` engine. Matches report
//! where they are in characters, the way Fennec indexes strings.

use std::rc::Rc;

use fennec_re::{Anchor, Captures, Regex};
use fennec_syntax::{Diagnostic, Span};

use crate::call::CallArg;
use crate::collections::split_key;
use crate::interp::Interp;
use crate::methods::no_method;
use crate::value::{Instance, Value};

/// Part of a replacement: text as written, or what a group caught.
enum Part {
    Text(String),
    Group(usize),
}

impl Interp<'_> {
    /// A pattern, compiled once and kept.
    pub(crate) fn regex(&mut self, pattern: &str) -> Result<Rc<Regex>, fennec_re::Error> {
        if let Some(regex) = self.regexes.get(pattern) {
            return Ok(Rc::clone(regex));
        }
        let regex = Rc::new(Regex::new(pattern)?);
        self.regexes.insert(pattern.to_owned(), Rc::clone(&regex));
        Ok(regex)
    }

    /// Why a `Regex` being made has a pattern that isn't valid, if it does.
    pub(crate) fn pattern_problem(
        &mut self,
        type_name: &str,
        fields: &[(String, Value)],
        span: Span,
    ) -> Option<Diagnostic> {
        match (type_name, fields) {
            ("Regex", [(_, Value::Str(pattern))]) => {
                self.regex(pattern).err().map(|error| invalid(&error, span))
            }
            _ => None,
        }
    }

    pub(crate) fn regex_method(
        &mut self,
        receiver: &Instance,
        method: &str,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let (args, key) = split_key(args, method)?;
        if key.is_some() {
            return Err(Diagnostic::new(format!("`{method}` doesn't take `key`"), span));
        }
        let pattern = match receiver.field("pattern") {
            Some(Value::Str(pattern)) => Rc::clone(pattern),
            _ => return Err(Diagnostic::new("this `Regex` has no pattern", span)),
        };
        let regex = self.regex(&pattern).map_err(|error| invalid(&error, span))?;
        match (method, args.as_slice()) {
            ("match" | "fullmatch" | "search", [Value::Str(text)]) => {
                let anchor = match method {
                    "match" => Anchor::Start,
                    "fullmatch" => Anchor::Both,
                    _ => Anchor::Search,
                };
                match regex.find_at(text, 0, anchor) {
                    Some(captures) => {
                        let start = chars(text, 0, captures.whole().0);
                        self.found(text, &captures, start, span)
                    }
                    None => Ok(Value::None),
                }
            }
            ("find_all", [Value::Str(text)]) => {
                let mut matches = Vec::new();
                // Counting characters from the last match keeps this linear.
                let (mut byte, mut char) = (0, 0);
                for captures in regex.find_all(text) {
                    let start = captures.whole().0;
                    char += chars(text, byte, start);
                    byte = start;
                    matches.push(self.found(text, &captures, char, span)?);
                }
                Ok(Value::list(matches))
            }
            ("replace", [Value::Str(text), Value::Str(replacement)]) => {
                let parts = template(replacement, regex.groups(), span)?;
                Ok(Value::str(&replace(text, &regex, &parts)))
            }
            ("split", [Value::Str(text)]) => {
                let mut pieces = Vec::new();
                let mut from = 0;
                for captures in regex.find_all(text) {
                    let (start, end) = captures.whole();
                    pieces.push(Value::str(text.get(from..start).unwrap_or("")));
                    from = end;
                }
                pieces.push(Value::str(text.get(from..).unwrap_or("")));
                Ok(Value::list(pieces))
            }
            ("match" | "fullmatch" | "search" | "find_all" | "replace" | "split", _) => {
                Err(Diagnostic::new(format!("`{method}` was called with the wrong arguments"), span))
            }
            _ => Err(no_method(&Value::Instance(Rc::new(receiver.clone())), method, span)),
        }
    }

    /// A `Match`, given where the whole match starts in characters.
    fn found(
        &mut self,
        text: &str,
        captures: &Captures,
        start: usize,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let (from, to) = captures.whole();
        let int = |n: usize| Value::Int(i64::try_from(n).unwrap_or(i64::MAX));
        let groups = captures
            .groups
            .iter()
            .skip(1)
            .map(|group| group.map_or(Value::None, |(from, to)| Value::str(text.get(from..to).unwrap_or(""))))
            .collect();
        let fields = vec![
            ("text", Value::str(text.get(from..to).unwrap_or(""))),
            ("start", int(start)),
            ("end", int(start + chars(text, from, to))),
            ("groups", Value::list(groups)),
        ];
        self.make_value("Match", fields, span)
    }
}

/// How many characters lie between two byte offsets.
fn chars(text: &str, from: usize, to: usize) -> usize {
    text.get(from..to).map_or(0, |part| part.chars().count())
}

pub(crate) fn invalid(error: &fennec_re::Error, span: Span) -> Diagnostic {
    Diagnostic::new(format!("this pattern isn't valid: {} (at character {})", error.message, error.at), span)
}

/// Reads a replacement: `\1` to `\99` and `\g<n>` stand for groups, `\\` for a backslash.
fn template(replacement: &str, groups: usize, span: Span) -> Result<Vec<Part>, Diagnostic> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            text.push(c);
            continue;
        }
        let group = match chars.next() {
            Some('\\') => {
                text.push('\\');
                continue;
            }
            Some(first @ '0'..='9') => {
                let mut digits = first.to_string();
                if let Some(second) = chars.next_if(char::is_ascii_digit) {
                    digits.push(second);
                }
                digits
            }
            Some('g') if chars.next_if_eq(&'<').is_some() => {
                let digits: String = chars.by_ref().take_while(|&c| c != '>').collect();
                digits
            }
            other => {
                let shown = other.map_or_else(String::new, String::from);
                return Err(Diagnostic::new(
                    format!("`\\{shown}` isn't something a replacement knows"),
                    span,
                )
                .with_help(
                    "in a replacement, `\\1` is group 1, `\\g<10>` is group 10 and `\\\\` is a backslash",
                ));
            }
        };
        let number: usize = group
            .parse()
            .map_err(|_| Diagnostic::new(format!("`\\g<{group}>` needs a group's number"), span))?;
        if number > groups {
            return Err(Diagnostic::new(format!("the pattern has no group {number}"), span));
        }
        if !text.is_empty() {
            parts.push(Part::Text(std::mem::take(&mut text)));
        }
        parts.push(Part::Group(number));
    }
    if !text.is_empty() {
        parts.push(Part::Text(text));
    }
    Ok(parts)
}

fn replace(text: &str, regex: &Regex, parts: &[Part]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    for captures in regex.find_all(text) {
        let (start, end) = captures.whole();
        out.push_str(text.get(from..start).unwrap_or(""));
        for part in parts {
            match part {
                Part::Text(part) => out.push_str(part),
                Part::Group(number) => {
                    let group = captures.groups.get(*number).copied().flatten();
                    out.push_str(group.and_then(|(from, to)| text.get(from..to)).unwrap_or(""));
                }
            }
        }
        from = end;
    }
    out.push_str(text.get(from..).unwrap_or(""));
    out
}
