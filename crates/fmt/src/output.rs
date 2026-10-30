//! The finished lines as text: each line's indentation, its code, and its trailing comment
//! two spaces after it.

use crate::layout::OutLine;

/// Writes the lines out, each ending with a line feed. Blank lines have no indentation, and
/// the writer never puts one first or last.
pub(crate) fn render(lines: &[OutLine]) -> String {
    let mut out = String::with_capacity(lines.iter().map(|line| line.indent + line.code.len() + 1).sum());
    for line in lines {
        out.extend(std::iter::repeat_n(' ', line.indent));
        out.push_str(&line.code);
        if let Some(comment) = &line.comment {
            if !line.code.is_empty() {
                out.push_str("  ");
            }
            out.push_str(comment);
        }
        out.push('\n');
    }
    out
}
