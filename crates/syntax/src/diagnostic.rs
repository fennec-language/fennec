//! Errors that point at the code they're about, with a fix where there is one.

use std::fmt::Write as _;

use crate::span::Span;

/// Between a message native code stops with and the help shown under it: what native code writes
/// in one string, the runtime shows as an error and its help.
pub const HELP_MARK: &str = "\u{1}help\u{1}";

/// A problem in a Fennec program: what's wrong, where, and how to fix it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub message: String,
    pub span: Span,
    pub help: Option<String>,
    /// Another place the problem involves, like the first of two methods with one name, and
    /// what it is, for the note about it. Boxed with `hint`'s reason: few have one.
    pub elsewhere: Option<Box<(Span, Also)>>,
    /// Changes to the source that fix the problem, which `fennec fix` makes. Each one leaves
    /// what the program does as it was.
    pub fixes: Vec<Edit>,
    /// A change that would fix the problem but may change what the program does, like adding
    /// `mut`. `fennec fix` never makes it: it's shown as `fix: ...`, to be made by hand.
    /// Boxed, since few have one and every `Result` that fails carries a diagnostic.
    pub hint: Option<Box<Hint>>,
}

/// A fix to make by hand: what it says, and the change it is, for an editor to offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    pub says: String,
    pub edits: Vec<Edit>,
}

/// What a diagnostic's other place is, for the note that points at it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Also {
    /// Another one of the same thing, like the first of two methods with one name.
    Other,
    /// The loop going over what changed.
    Loop,
    /// Where a generator was used first.
    FirstUse,
}

/// One change to a source: the text at `span` becomes `text`. An empty span inserts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub span: Span,
    pub text: String,
}

/// What joins a dependency's name to each name it defines in the merged program, as in `physics·Body`.
/// It's nothing a program can write, so a kit's names can't meet another's.
pub const KIT_MARK: char = '·';

/// A message as people read it: a name from another kit is written `physics.Body`, as code
/// reaches it.
fn with_kits_dotted(message: String) -> String {
    if message.contains(KIT_MARK) { message.replace(KIT_MARK, ".") } else { message }
}

/// A type's name as a running program shows it, without the kit the merged program marks it with:
/// `Body`, the same as inside its own kit.
#[must_use]
pub fn without_kits(name: &str) -> String {
    let mut shown = String::new();
    let mut parts = name.split(KIT_MARK).peekable();
    while let Some(part) = parts.next() {
        let word = |c: char| c.is_alphanumeric() || c == '_';
        shown.push_str(if parts.peek().is_some() { part.trim_end_matches(word) } else { part });
    }
    shown
}

impl Diagnostic {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Diagnostic {
            message: with_kits_dotted(message.into()),
            span,
            help: None,
            elsewhere: None,
            fixes: Vec::new(),
            hint: None,
        }
    }

    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(with_kits_dotted(help.into()));
        self
    }

    /// Points at another place the problem involves.
    #[must_use]
    pub fn also_at(self, span: Span) -> Self {
        let what = self.elsewhere.as_ref().map_or(Also::Other, |elsewhere| elsewhere.1);
        self.also_at_the(what, span)
    }

    /// Points at another place the problem involves, saying what it is.
    #[must_use]
    pub fn also_at_the(mut self, what: Also, span: Span) -> Self {
        self.elsewhere = Some(Box::new((span, what)));
        self
    }

    /// The other place the problem involves, if there's one.
    pub fn also(&self) -> Option<Span> {
        self.elsewhere.as_ref().map(|elsewhere| elsewhere.0)
    }

    /// What `also` points at, as the notes about it say.
    pub fn also_is(&self) -> &'static str {
        match self.elsewhere.as_ref().map_or(Also::Other, |elsewhere| elsewhere.1) {
            Also::Other => "the other one",
            Also::Loop => "the loop",
            Also::FirstUse => "its first use",
        }
    }

    /// Adds a change to the source that fixes the problem.
    #[must_use]
    pub fn with_fix(mut self, span: Span, text: impl Into<String>) -> Self {
        self.fixes.push(Edit { span, text: text.into() });
        self
    }

    /// Adds a fix to make by hand, saying `says`: the text at `span` becomes `text`.
    #[must_use]
    pub fn with_hint(mut self, says: impl Into<String>, span: Span, text: impl Into<String>) -> Self {
        self.hint = Some(Box::new(Hint { says: says.into(), edits: vec![Edit { span, text: text.into() }] }));
        self
    }

    /// The help and the fix to make by hand, as lines under the error, each starting with `pad`.
    fn notes(&self, pad: &str) -> String {
        let mut written = String::new();
        if let Some(help) = &self.help {
            let _ = writeln!(written, "{pad} = help: {help}");
        }
        if let Some(hint) = &self.hint {
            let _ = writeln!(written, "{pad} = fix: {}", hint.says);
        }
        written
    }

    /// Renders the diagnostic with the line it points at, for a terminal.
    pub fn render(&self, path: &str, source: &str) -> String {
        let line = |at| line_of(source, at);
        let also = self.also().map(|span| place(path, source, span, &line));
        self.render_with(path, source, also.as_deref(), &line)
    }

    /// Like `render`, with where `also` points written out, as `path:line:column`, and `line_of`
    /// saying which line of `source` an offset is on, as the free function does.
    pub(crate) fn render_with(
        &self,
        path: &str,
        source: &str,
        also: Option<&str>,
        line_of: &dyn Fn(usize) -> (usize, usize),
    ) -> String {
        if self.span.in_prelude() {
            return format!("error: {}\n --> the standard library\n{}", self.message, self.notes(" "));
        }
        let start = usize::try_from(self.span.start).unwrap_or(0).min(source.len());
        let (line_number, line_start) = line_of(start);
        let line_end =
            source.get(start..).and_then(|rest| rest.find('\n')).map_or(source.len(), |at| start + at);
        let line = source.get(line_start..line_end).unwrap_or("");
        let column = source.get(line_start..start).map_or(0, |text| text.chars().count());
        let end = usize::try_from(self.span.end).unwrap_or(start).clamp(start, line_end);
        let width = source.get(start..end).map_or(1, |text| text.chars().count()).max(1);

        let gutter = line_number.to_string();
        let pad = " ".repeat(gutter.len());
        let notes = self.notes(&pad);
        let help = if notes.is_empty() { notes } else { format!("{pad} |\n{notes}") };
        let note =
            also.map_or_else(String::new, |also| format!("{pad} = note: {} is at {also}\n", self.also_is()));
        format!(
            "error: {message}\n{pad}--> {path}:{line_number}:{col}\n{pad} |\n{gutter} | {line}\n{pad} | {indent}{carets}\n{help}{note}",
            message = self.message,
            col = column + 1,
            indent = " ".repeat(column),
            carets = "^".repeat(width),
        )
    }
}

/// The line an offset into `source` is on, from 1, and where that line starts, by reading up to it.
/// An offset inside a character is on the first line.
pub(crate) fn line_of(source: &str, at: usize) -> (usize, usize) {
    let before = source.get(..at).unwrap_or("");
    (before.matches('\n').count() + 1, before.rfind('\n').map_or(0, |newline| newline + 1))
}

/// Where a span starts, as `path:line:column`, with `line_of` as `render_with` takes it.
pub(crate) fn place(
    path: &str,
    source: &str,
    span: Span,
    line_of: &dyn Fn(usize) -> (usize, usize),
) -> String {
    let start = usize::try_from(span.start).unwrap_or(0).min(source.len());
    let (line, line_start) = line_of(start);
    let column = source.get(line_start..start).map_or(0, |text| text.chars().count()) + 1;
    format!("{path}:{line}:{column}")
}

/// A source with the fixes of `diagnostics` made, for a file whose spans start at `base`.
/// Fixes outside the file, or overlapping an earlier one, are left out. `None` when there's
/// nothing to change.
pub fn apply_fixes(source: &str, base: u32, diagnostics: &[Diagnostic]) -> Option<String> {
    let end = base.saturating_add(u32::try_from(source.len()).unwrap_or(u32::MAX));
    let mut edits: Vec<&Edit> = diagnostics
        .iter()
        .flat_map(|diagnostic| &diagnostic.fixes)
        .filter(|edit| base <= edit.span.start && edit.span.start <= edit.span.end && edit.span.end <= end)
        .collect();
    if edits.is_empty() {
        return None;
    }
    edits.sort_by_key(|edit| (edit.span.start, edit.span.end));
    edits.dedup();
    let mut fixed = String::with_capacity(source.len());
    let mut at = 0;
    for edit in edits {
        let start = usize::try_from(edit.span.start - base).unwrap_or(usize::MAX);
        let stop = usize::try_from(edit.span.end - base).unwrap_or(usize::MAX);
        if start < at {
            continue;
        }
        fixed.push_str(source.get(at..start)?);
        fixed.push_str(&edit.text);
        at = stop;
    }
    fixed.push_str(source.get(at..)?);
    Some(fixed)
}
