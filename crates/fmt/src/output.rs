//! The finished lines as text. Trailing comments on neighbouring lines at the same
//! indentation line up, two spaces after the longest line's code.

use crate::layout::OutLine;

/// Writes the lines out, each ending with `newline`.
pub(crate) fn render(lines: &[OutLine], newline: &str) -> String {
    let first = lines.iter().position(|line| !is_blank(line)).unwrap_or(lines.len());
    let last = lines.iter().rposition(|line| !is_blank(line)).map_or(first, |at| at + 1);
    let lines = lines.get(first..last).unwrap_or(&[]);
    let columns = comment_columns(lines);
    let mut out = String::with_capacity(lines.iter().map(|line| line.indent + line.code.len() + 1).sum());
    for (line, column) in lines.iter().zip(columns) {
        if !is_blank(line) {
            push_spaces(&mut out, line.indent);
        }
        out.push_str(&line.code);
        if let Some(comment) = &line.comment {
            if line.code.is_empty() {
                out.push_str(comment);
            } else {
                push_spaces(&mut out, column.saturating_sub(line.indent + width(&line.code)));
                out.push_str(comment);
            }
        }
        out.push_str(newline);
    }
    out
}

fn is_blank(line: &OutLine) -> bool {
    line.code.is_empty() && line.comment.is_none()
}

fn width(text: &str) -> usize {
    text.chars().count()
}

fn push_spaces(out: &mut String, count: usize) {
    out.extend(std::iter::repeat_n(' ', count));
}

/// The column each line's trailing comment starts at.
fn comment_columns(lines: &[OutLine]) -> Vec<usize> {
    let mut columns = vec![0; lines.len()];
    let mut start = 0;
    while start < lines.len() {
        let in_run = |line: &OutLine, indent: usize| {
            line.comment.is_some() && !line.code.is_empty() && line.indent == indent
        };
        let Some(first) = lines.get(start) else { break };
        if !in_run(first, first.indent) {
            start += 1;
            continue;
        }
        let end = (start..lines.len())
            .find(|&at| lines.get(at).is_none_or(|line| !in_run(line, first.indent)))
            .unwrap_or(lines.len());
        let run = lines.get(start..end).unwrap_or(&[]);
        let column = run.iter().map(|line| line.indent + width(&line.code)).max().unwrap_or(0) + 2;
        for slot in columns.get_mut(start..end).unwrap_or(&mut []) {
            *slot = column;
        }
        start = end;
    }
    columns
}
