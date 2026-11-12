//! `std.re`: the methods of a `Regex`, which run the `fennec-re` engine. Matches report
//! where they are in characters, the way Fennec indexes strings.

use std::collections::HashMap;
use std::rc::Rc;

use fennec_re::Regex;
use fennec_syntax::{Diagnostic, Span};

/// Part of a replacement: text as written, or what a group caught.
enum Part {
    Text(String),
    Group(usize),
}

/// How many compiled patterns are kept, so a program compiling patterns it's given, as a server
/// might from requests, doesn't hold every one it ever saw.
const MOST_KEPT: usize = 256;

/// The longest pattern kept compiled. A longer one is compiled each time it's used.
const LONGEST_KEPT: usize = 4096;

/// Patterns compiled so far, each kept for the next time it's used, up to `MOST_KEPT` of them:
/// past that, the one used longest ago goes.
#[derive(Default)]
pub(crate) struct Regexes {
    kept: HashMap<String, (Rc<Regex>, u64)>,
    /// Counts each use, so the one used longest ago has the lowest number.
    uses: u64,
}

impl Regexes {
    /// A pattern, compiled once and kept while it's among the most recently used.
    pub(crate) fn compiled(&mut self, pattern: &str) -> Result<Rc<Regex>, fennec_re::Error> {
        self.uses += 1;
        if let Some((regex, used)) = self.kept.get_mut(pattern) {
            *used = self.uses;
            return Ok(Rc::clone(regex));
        }
        let regex = Rc::new(Regex::new(pattern)?);
        if pattern.len() <= LONGEST_KEPT {
            if self.kept.len() >= MOST_KEPT
                && let Some(oldest) =
                    self.kept.iter().min_by_key(|(_, (_, used))| *used).map(|(key, _)| key.clone())
            {
                self.kept.remove(&oldest);
            }
            self.kept.insert(pattern.to_owned(), (Rc::clone(&regex), self.uses));
        }
        Ok(regex)
    }

    /// How many patterns are kept compiled.
    #[cfg(test)]
    fn kept(&self) -> usize {
        self.kept.len()
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

#[cfg(test)]
mod tests {
    use super::Regexes;

    /// A program compiling a new pattern each time keeps at most 256 of them, the ones it used
    /// last, and a pattern too long to keep still compiles and matches.
    #[test]
    fn only_the_patterns_used_last_are_kept() {
        let mut regexes = Regexes::default();
        let first = regexes.compiled("a+").ok();
        for n in 0..1000 {
            assert!(regexes.compiled(&format!("x{n}")).is_ok());
            // Used all along, it's never the one that goes.
            assert!(regexes.compiled("a+").is_ok());
        }
        assert_eq!(regexes.kept(), 256);
        let again = regexes.compiled("a+").ok();
        assert!(first.zip(again).is_some_and(|(first, again)| std::rc::Rc::ptr_eq(&first, &again)));
        let long = "b".repeat(10_000);
        assert!(regexes.compiled(&long).is_ok());
        assert_eq!(regexes.kept(), 256);
    }
}
