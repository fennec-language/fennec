//! The files of a program with several, so a span can be traced back to its file and line.

use crate::diagnostic::Diagnostic;
use crate::span::Span;

/// Every file of a program, each at its own range of byte offsets. A file parsed with
/// `parse_at(source, base)` has spans in `base..base + len`, so spans from different files
/// never overlap and one `Span` says which file it's in.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    files: Vec<File>,
}

#[derive(Clone, Debug)]
struct File {
    path: String,
    text: String,
    base: u32,
}

impl Sources {
    pub fn new() -> Self {
        Sources::default()
    }

    /// Adds a file, shown as `path` in messages. Returns the base offset to parse it at.
    pub fn add(&mut self, path: impl Into<String>, text: impl Into<String>) -> u32 {
        // One byte of room after each file, so a span at its very end stays in it.
        let base = self.files.last().map_or(0, |file| file.end().saturating_add(1));
        self.files.push(File { path: path.into(), text: text.into(), base });
        base
    }

    /// Renders a diagnostic against the file it points into. Spans from no file, like the
    /// standard library's, render against the first file, as they do with one file.
    pub fn render(&self, diagnostic: &Diagnostic) -> String {
        let Some(file) = self.file(diagnostic.span).or_else(|| self.files.first()) else {
            return format!("error: {}\n", diagnostic.message);
        };
        let local = Span {
            start: diagnostic.span.start.saturating_sub(file.base),
            end: diagnostic.span.end.saturating_sub(file.base),
        };
        Diagnostic { span: local, ..diagnostic.clone() }.render(&file.path, &file.text)
    }

    /// Every file, as its path and text, in the order they were added.
    pub fn files(&self) -> impl Iterator<Item = (&str, &str)> {
        self.files.iter().map(|file| (file.path.as_str(), file.text.as_str()))
    }

    /// Renders several diagnostics, a blank line between each.
    pub fn render_all(&self, diagnostics: &[Diagnostic]) -> String {
        let rendered: Vec<String> = diagnostics.iter().map(|diagnostic| self.render(diagnostic)).collect();
        rendered.join("\n")
    }

    /// Where a span starts: its file's path, its line and column (from 1), and that line's
    /// text.
    pub fn locate(&self, span: Span) -> Option<(&str, usize, usize, &str)> {
        let file = self.file(span)?;
        let start = usize::try_from(span.start.saturating_sub(file.base)).ok()?.min(file.text.len());
        let before = file.text.get(..start)?;
        let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
        let line_end = file.text.get(start..)?.find('\n').map_or(file.text.len(), |at| start + at);
        let column = file.text.get(line_start..start)?.chars().count() + 1;
        let line = before.matches('\n').count() + 1;
        Some((file.path.as_str(), line, column, file.text.get(line_start..line_end)?))
    }

    /// Each file the fixes of `diagnostics` change, with its new text.
    pub fn fixed(&self, diagnostics: &[Diagnostic]) -> Vec<(String, String)> {
        self.files
            .iter()
            .filter_map(|file| {
                let text = crate::diagnostic::apply_fixes(&file.text, file.base, diagnostics)?;
                Some((file.path.clone(), text))
            })
            .collect()
    }

    fn file(&self, span: Span) -> Option<&File> {
        self.files.iter().rev().find(|file| file.base <= span.start && span.start <= file.end())
    }
}

impl File {
    fn end(&self) -> u32 {
        self.base.saturating_add(u32::try_from(self.text.len()).unwrap_or(u32::MAX))
    }
}
