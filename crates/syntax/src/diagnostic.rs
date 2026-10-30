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
}

impl Diagnostic {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Diagnostic { message: message.into(), span, help: None, also: None }
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
