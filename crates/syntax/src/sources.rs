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

    /// The path of the file `span` is in.
    pub fn path(&self, span: Span) -> Option<&str> {
        self.file(span).map(|file| file.path.as_str())
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

    /// Renders several diagnostics, a blank line between each.
    pub fn render_all(&self, diagnostics: &[Diagnostic]) -> String {
        let rendered: Vec<String> = diagnostics.iter().map(|diagnostic| self.render(diagnostic)).collect();
        rendered.join("\n")
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
