//! The files of a program with several, so a span can be traced back to its file and line.

use std::fmt::Write as _;

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
        let local = |span: Span, file: &File| Span {
            start: span.start.saturating_sub(file.base),
            end: span.end.saturating_sub(file.base),
        };
        let also = diagnostic.also().and_then(|span| {
            let other = self.file(span)?;
            Some(crate::diagnostic::place(&other.path, &other.text, local(span, other)))
        });
        Diagnostic { span: local(diagnostic.span, file), ..diagnostic.clone() }.render_with(
            &file.path,
            &file.text,
            also.as_deref(),
        )
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

    /// A failure's call chain, to go under it: the function it failed in, then each call it left
    /// on its way out, innermost first, with the file, line and column of each. Nothing when
    /// it left no call. Calls in the standard library's code are said to be there.
    pub fn render_trace(&self, failed_at: Span, trace: &Trace) -> String {
        if trace.is_empty() {
            return String::new();
        }
        let named = |span: Span| {
            self.function_at(span)
                .map_or_else(|| "the standard library".to_owned(), |name| format!("`{name}`"))
        };
        let mut text = format!("  = in {}\n", named(failed_at));
        // Calls the standard library's code makes among its own functions all say the same, once.
        let mut lines: Vec<(String, u64)> = Vec::new();
        let mut add = |line: String, times: u64| match lines.last_mut() {
            Some((last, already)) if *last == line => *already += times,
            _ => lines.push((line, times)),
        };
        for (call, times) in &trace.inner {
            add(self.called_from(*call, &named), *times);
        }
        if trace.skipped > 0 {
            add("... more calls".to_owned(), 1);
        }
        for (call, times) in &trace.outer {
            add(self.called_from(*call, &named), *times);
        }
        // How often isn't said: how deep a runaway recursion goes before it's stopped depends on
        // the machine, and a failure says the same wherever it runs.
        for (line, times) in lines {
            let _ = match times {
                1 => writeln!(text, "  = {line}"),
                _ => writeln!(text, "  = {line}, more than once"),
            };
        }
        text
    }

    /// A call in a call chain, as `called from ... at path:line:column`.
    fn called_from(&self, call: Span, named: &impl Fn(Span) -> String) -> String {
        match self.locate(call) {
            Some((path, line, column, _)) => format!("called from {} at {path}:{line}:{column}", named(call)),
            None => "called from the standard library".to_owned(),
        }
    }

    /// The name of the function written around a span, as `name`, or `Type.name` for a method:
    /// the nearest `def` above it that it's indented under. `None` outside any file.
    pub fn function_at(&self, span: Span) -> Option<String> {
        let file = self.file(span)?;
        let at = usize::try_from(span.start.saturating_sub(file.base)).ok()?.min(file.text.len());
        let before = file.text.get(..at)?;
        let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
        let here = file.text.get(line_start..)?;
        let mut indent = here.len() - here.trim_start().len();
        let words = |text: &str| -> Option<String> {
            let name: String = text.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            (!name.is_empty()).then_some(name)
        };
        // A failure at a `def`'s own line, like a function's whole span, is in that function.
        let mut function = here
            .trim_start()
            .strip_prefix("export ")
            .unwrap_or(here.trim_start())
            .strip_prefix("def ")
            .and_then(words);
        if function.is_some() {
            indent = here.len() - here.trim_start().len();
        }
        for line in before.get(..line_start)?.lines().rev() {
            let written = line.trim_start();
            if written.is_empty() || written.starts_with('#') {
                continue;
            }
            let this = line.len() - written.len();
            if this >= indent {
                continue;
            }
            indent = this;
            let written = written.strip_prefix("export ").unwrap_or(written);
            match &function {
                None => function = written.strip_prefix("def ").and_then(words),
                Some(name) => {
                    let owner = ["struct ", "extend ", "enum ", "error ", "shared struct "]
                        .iter()
                        .find_map(|word| written.strip_prefix(word))
                        .and_then(words);
                    if let Some(owner) = owner {
                        return Some(format!("{owner}.{name}"));
                    }
                }
            }
            if indent == 0 {
                break;
            }
        }
        function
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

    /// The file a span is in: its path, its text, and the offset its spans start at.
    pub fn file_at(&self, span: Span) -> Option<(&str, &str, u32)> {
        self.file(span).map(|file| (file.path.as_str(), file.text.as_str(), file.base))
    }

    /// Just the files `keep` says, by path, each at the offsets it has here, so spans into them
    /// still point where they did.
    #[must_use]
    pub fn only(&self, keep: &dyn Fn(&str) -> bool) -> Sources {
        Sources { files: self.files.iter().filter(|file| keep(&file.path)).cloned().collect() }
    }

    /// A file's text and the offset its spans start at, by its path.
    pub fn by_path(&self, path: &str) -> Option<(&str, u32)> {
        self.files.iter().find(|file| file.path == path).map(|file| (file.text.as_str(), file.base))
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

/// A failure's call chain: each call it left on its way out, innermost first. It stays small however
/// deep the failure went. A call left over and over in a row, as a recursion is, is one entry with
/// how often, and past the first [`Trace::KEPT`] entries only the last [`Trace::KEPT`] are kept,
/// with how many calls went between.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trace {
    inner: Vec<(Span, u64)>,
    outer: std::collections::VecDeque<(Span, u64)>,
    skipped: u64,
}

impl Trace {
    /// How many entries it keeps at each end.
    pub const KEPT: usize = 16;

    /// The failure left the call at `span`, `times` times in a row.
    pub fn push(&mut self, span: Span, times: u64) {
        let last = if self.outer.is_empty() { self.inner.last_mut() } else { self.outer.back_mut() };
        if let Some((at, already)) = last
            && *at == span
        {
            *already = already.saturating_add(times);
            return;
        }
        if self.outer.is_empty() && self.inner.len() < Self::KEPT {
            self.inner.push((span, times));
            return;
        }
        self.outer.push_back((span, times));
        if self.outer.len() > Self::KEPT
            && let Some((_, gone)) = self.outer.pop_front()
        {
            self.skipped = self.skipped.saturating_add(gone);
        }
    }

    /// `calls` calls the failure left, past what's kept.
    pub fn skip(&mut self, calls: u64) {
        self.skipped = self.skipped.saturating_add(calls);
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty() && self.skipped == 0
    }
}
