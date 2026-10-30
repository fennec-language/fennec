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

/// The column each line's trailing comment starts at. Neighbouring lines at the same
/// indentation that both end in a comment are lined up.
fn comment_columns(lines: &[OutLine]) -> Vec<usize> {
    let in_run = |line: &OutLine| line.comment.is_some() && !line.code.is_empty();
    let runs = lines.chunk_by(|above, below| in_run(above) && in_run(below) && above.indent == below.indent);
    let mut columns = Vec::with_capacity(lines.len());
    for run in runs {
        let widest = run.iter().map(|line| line.indent + width(&line.code)).max().unwrap_or(0);
        columns.extend(std::iter::repeat_n(widest + 2, run.len()));
    }
    columns
}
