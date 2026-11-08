//! `std.re`: the methods of a `Regex`, which run the `fennec-re` engine. Matches report
//! where they are in characters, the way Fennec indexes strings.

use std::collections::HashMap;
use std::rc::Rc;

use fennec_re::{Anchor, Captures, Regex};
use fennec_syntax::{Diagnostic, Span};

use crate::methods::no_method;
use crate::value::{Instance, Value};


/// Part of a replacement: text as written, or what a group caught.
enum Part {
    Text(String),
    Group(usize),
}

/// Patterns compiled so far, each kept for the next time it's used.
#[derive(Default)]
pub(crate) struct Regexes(HashMap<String, Rc<Regex>>);

impl Regexes {
    /// A pattern, compiled once and kept.
    pub(crate) fn compiled(&mut self, pattern: &str) -> Result<Rc<Regex>, fennec_re::Error> {
        if let Some(regex) = self.0.get(pattern) {
            return Ok(Rc::clone(regex));
        }
        let regex = Rc::new(Regex::new(pattern)?);
        self.0.insert(pattern.to_owned(), Rc::clone(&regex));
        Ok(regex)
    }
}

/// How many characters lie between two byte offsets.
pub(crate) fn chars(text: &str, from: usize, to: usize) -> usize {
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

/// `text` with every match replaced, where `\\1` in `with` is group 1.
pub(crate) fn replaced(text: &str, regex: &Regex, with: &str, span: Span) -> Result<String, Diagnostic> {
    let parts = template(with, regex.groups(), span)?;
    Ok(replace(text, regex, &parts))
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
