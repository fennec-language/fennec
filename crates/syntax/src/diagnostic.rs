//! Errors that point at the code they're about, with a fix where there is one.

use crate::span::Span;

/// A problem in a Fennec program: what's wrong, where, and how to fix it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub message: String,
    pub span: Span,
    pub help: Option<String>,
    /// Another place the problem involves, like the first of two methods with one name.
    pub also: Option<Span>,
    /// Changes to the source that fix the problem, which `fennec fix` makes.
    pub fixes: Vec<Edit>,
}

/// One change to a source: the text at `span` becomes `text`. An empty span inserts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub span: Span,
    pub text: String,
}

impl Diagnostic {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Diagnostic { message: message.into(), span, help: None, also: None, fixes: Vec::new() }
    }

    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Points at another place the problem involves.
    #[must_use]
    pub fn also_at(mut self, span: Span) -> Self {
        self.also = Some(span);
        self
    }

    /// Adds a change to the source that fixes the problem.
    #[must_use]
    pub fn with_fix(mut self, span: Span, text: impl Into<String>) -> Self {
        self.fixes.push(Edit { span, text: text.into() });
        self
    }

    /// Renders the diagnostic with the line it points at, for a terminal.
    pub fn render(&self, path: &str, source: &str) -> String {
        let also = self.also.map(|span| place(path, source, span));
        self.render_with(path, source, also.as_deref())
    }

    /// Like `render`, with where `also` points written out, as `path:line:column`.
    pub(crate) fn render_with(&self, path: &str, source: &str, also: Option<&str>) -> String {
        if self.span.in_prelude() {
            let help = self.help.as_ref().map_or_else(String::new, |help| format!("  = help: {help}\n"));
            return format!("error: {}\n --> the standard library\n{help}", self.message);
        }
        let start = usize::try_from(self.span.start).unwrap_or(0).min(source.len());
        let before = source.get(..start).unwrap_or("");
        let line_number = before.matches('\n').count() + 1;
        let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
        let line_end =
            source.get(start..).and_then(|rest| rest.find('\n')).map_or(source.len(), |at| start + at);
        let line = source.get(line_start..line_end).unwrap_or("");
        let column = source.get(line_start..start).map_or(0, |text| text.chars().count());
        let end = usize::try_from(self.span.end).unwrap_or(start).clamp(start, line_end);
        let width = source.get(start..end).map_or(1, |text| text.chars().count()).max(1);

        let gutter = line_number.to_string();
        let pad = " ".repeat(gutter.len());
        let help =
            self.help.as_ref().map_or_else(String::new, |help| format!("{pad} |\n{pad} = help: {help}\n"));
        let note =
            also.map_or_else(String::new, |also| format!("{pad} = note: the other one is at {also}\n"));
        format!(
            "error: {message}\n{pad}--> {path}:{line_number}:{col}\n{pad} |\n{gutter} | {line}\n{pad} | {indent}{carets}\n{help}{note}",
            message = self.message,
            col = column + 1,
            indent = " ".repeat(column),
            carets = "^".repeat(width),
        )
    }
}

/// Where a span starts, as `path:line:column`.
pub(crate) fn place(path: &str, source: &str, span: Span) -> String {
    let start = usize::try_from(span.start).unwrap_or(0).min(source.len());
    let before = source.get(..start).unwrap_or("");
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
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
