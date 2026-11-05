//! The examples in `##` doc comments, as tests: each Markdown code fence, with no language or
//! `fennec`, is a test of its own whose body is the fence's code. A fence naming another
//! language is only text. The code stays where it was written, so a failure points at its line.
//! It's in scope as the file's own tests are: it sees what the file sees. It's only code, not a
//! file, so it can't import or be handed capabilities.

use crate::ast::{Item, TestDef};
use crate::diagnostic::Diagnostic;
use crate::parser::parse_statements_at;
use crate::span::Span;
use crate::trivia::lex_source;

/// One line of a doc comment: where the text after its `##` starts in the file, and that text.
struct DocLine<'s> {
    /// Where the line starts in the file.
    line_start: usize,
    /// Where its comment ends.
    end: usize,
    /// Where the text after `##` starts.
    at: usize,
    text: &'s str,
}

/// The `##` comments that have a line to themselves, in order. Comments come from the lexer, so
/// `##` in a string isn't one.
fn doc_lines(source: &str) -> Vec<DocLine<'_>> {
    let Ok(lexed) = lex_source(source) else { return Vec::new() };
    let mut lines = Vec::new();
    for comment in lexed.comments {
        let (start, end) = (comment.start as usize, comment.end as usize);
        let line_start = source.get(..start).and_then(|before| before.rfind('\n')).map_or(0, |at| at + 1);
        let alone = source.get(line_start..start).is_some_and(|before| before.trim().is_empty());
        let Some(after) = source.get(start..end).and_then(|text| text.strip_prefix("##")) else { continue };
        if !alone || after.starts_with('#') {
            continue;
        }
        let text = after.strip_prefix(' ').unwrap_or(after);
        lines.push(DocLine { line_start, end, at: end - text.len(), text: text.trim_end_matches('\r') });
    }
    lines
}

/// Whether a line opens a fence, and whether what's in it runs.
fn opens(line: &DocLine<'_>) -> Option<bool> {
    let language = line.text.trim().strip_prefix("```")?;
    Some(matches!(language.trim(), "" | "fennec"))
}

/// Source with only `lines` left in it, the rest blanked, so each byte is where it was.
fn only(source: &str, lines: &[DocLine<'_>]) -> String {
    let mut bytes: Vec<u8> = source.bytes().map(|byte| if byte == b'\n' { b'\n' } else { b' ' }).collect();
    for line in lines {
        if let Some(place) = bytes.get_mut(line.at..line.at + line.text.len()) {
            place.copy_from_slice(line.text.as_bytes());
        }
    }
    String::from_utf8(bytes).unwrap_or_default()
}

/// A code fence in a doc comment.
struct Fence<'s> {
    open: DocLine<'s>,
    body: Vec<DocLine<'s>>,
    /// Where its last line's comment ends.
    last_end: usize,
    closed: bool,
    runs: bool,
}

/// The code fences in a file's doc comments, in order.
fn fences(source: &str) -> Vec<Fence<'_>> {
    let mut lines = doc_lines(source).into_iter().peekable();
    let mut fences = Vec::new();
    while let Some(open) = lines.next() {
        let Some(runs) = opens(&open) else { continue };
        let mut body = Vec::new();
        let mut last_end = open.end;
        let mut closed = false;
        // A fence's lines are one comment: each directly follows the one before.
        while let Some(next) = lines.next_if(|line| line.line_start == last_end + 1) {
            last_end = next.end;
            if next.text.trim() == "```" {
                closed = true;
                break;
            }
            body.push(next);
        }
        fences.push(Fence { open, body, last_end, closed, runs });
    }
    fences
}

/// The code of each example in a file's doc comments, with the rest blanked, so each byte is where
/// it is in the file.
pub(crate) fn example_code(source: &str) -> Vec<String> {
    fences(source)
        .iter()
        .filter(|fence| fence.closed && fence.runs)
        .map(|fence| only(source, &fence.body))
        .collect()
}

/// The examples in a file's doc comments, as tests. The file is shown as `shown`, its spans start
/// at byte `base`, and a fence that's never closed or code that doesn't parse is a mistake.
pub fn doc_examples(source: &str, base: u32, shown: &str) -> Result<Vec<Item>, Diagnostic> {
    let mut tests = Vec::new();
    let mut count = 0;
    for Fence { open, body, last_end, closed, runs } in fences(source) {
        if !closed {
            return Err(Diagnostic::new(
                "this doc example's code fence is never closed",
                Span::new(open.at, open.at + open.text.len()).shifted(base),
            )
            .with_help("end it with a line of ``` in the same comment"));
        }
        if !runs {
            continue;
        }
        let stmts = parse_statements_at(&only(source, &body), base)?;
        if stmts.is_empty() {
            continue;
        }
        count += 1;
        tests.push(Item::Test(TestDef {
            name: format!("doc example {count} in {shown}"),
            capabilities: Vec::new(),
            inputs: Vec::new(),
            body: stmts,
            bench: false,
            span: Span::new(open.at, last_end).shifted(base),
        }));
    }
    Ok(tests)
}
