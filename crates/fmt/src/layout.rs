//! Writes a logical line's tree as physical lines. A group stays on one line if it fits,
//! with what follows it up to the next place a line could break; otherwise each of its
//! items goes on its own line, one indent deeper, each ending with a comma.

use crate::tree::{Comma, Group, Item, Node};
use crate::{INDENT, WIDTH};

/// A line of output. Code and its trailing comment are kept apart so trailing comments on
/// neighbouring lines can be lined up.
#[derive(Debug, Default)]
pub(crate) struct OutLine {
    pub(crate) indent: usize,
    pub(crate) code: String,
    pub(crate) comment: Option<String>,
}

#[derive(Default)]
pub(crate) struct Writer {
    pub(crate) lines: Vec<OutLine>,
    current: OutLine,
    /// The width of the current line so far, indentation included.
    column: usize,
}

impl Writer {
    pub(crate) fn blank(&mut self) {
        self.lines.push(OutLine::default());
    }

    /// Starts a new line at `indent`. Does nothing to a line with nothing on it yet.
    pub(crate) fn start(&mut self, indent: usize) {
        if !self.current.code.is_empty() || self.current.comment.is_some() {
            self.lines.push(std::mem::take(&mut self.current));
        }
        self.current.indent = indent;
        self.column = indent;
    }

    /// Ends the current line.
    pub(crate) fn finish(&mut self) {
        let indent = self.current.indent;
        self.start(indent);
    }

    fn text(&mut self, text: &str, width: usize) {
        if self.current.comment.is_some() {
            let indent = self.current.indent;
            self.start(indent);
        }
        self.current.code.push_str(text);
        self.column += width;
    }

    /// A space between tokens, unless a comment has just broken the line.
    fn space(&mut self) {
        if !self.current.code.is_empty() && self.current.comment.is_none() {
            self.text(" ", 1);
        }
    }

    /// Puts a comment at the end of the current line.
    pub(crate) fn trailing_comment(&mut self, comment: &str) {
        if let Some(existing) = &self.current.comment {
            let (indent, existing) = (self.current.indent, existing.clone());
            self.current.comment = None;
            self.own_line_comment(&existing, indent);
        }
        self.current.comment = Some(spaced(comment));
    }

    /// Puts a comment on a line of its own.
    pub(crate) fn own_line_comment(&mut self, comment: &str, indent: usize) {
        self.start(indent);
        self.current.code.push_str(&spaced(comment));
        self.start(indent);
    }

    /// Writes an item starting on the current line. `rest` is the width that must follow it
    /// on the same line, like the comma after a list item. Inside brackets, where a line can
    /// break anywhere, an item that doesn't fit breaks before each of its loosest operators.
    pub(crate) fn item(&mut self, item: &Item<'_>, indent: usize, rest: usize, in_brackets: bool) {
        if let Some(width) = item.flat
            && self.column + width + rest <= WIDTH
        {
            self.flat_nodes(&item.nodes);
            return;
        }
        let loosest = if in_brackets { loosest_operator(&item.nodes) } else { None };
        let follows = following_widths(&item.nodes, rest, loosest);
        for (at, node) in item.nodes.iter().enumerate() {
            match node {
                // An item starts its line, where `space` writes nothing.
                Node::Token { text, space, width, precedence } => {
                    if at > 0 && Some(*precedence) == loosest {
                        self.start(indent);
                    }
                    if *space {
                        self.space();
                    }
                    self.text(text, *width);
                }
                Node::Group(group) => {
                    if group.space {
                        self.space();
                    }
                    let follow = follows.get(at).copied().unwrap_or(0);
                    match group.flat {
                        Some(width) if self.column + width + follow <= WIDTH => self.flat_group(group),
                        _ => self.break_group(group, indent),
                    }
                }
                Node::Comment { text, own_line: false } => {
                    self.trailing_comment(text);
                    self.start(indent);
                }
                Node::Comment { text, own_line: true } => self.own_line_comment(text, indent),
            }
        }
    }

    fn flat_nodes(&mut self, nodes: &[Node<'_>]) {
        for (at, node) in nodes.iter().enumerate() {
            match node {
                Node::Token { text, space, width, .. } => {
                    if *space && at > 0 {
                        self.space();
                    }
                    self.text(text, *width);
                }
                Node::Group(group) => {
                    if group.space && at > 0 {
                        self.space();
                    }
                    self.flat_group(group);
                }
                Node::Comment { .. } => {}
            }
        }
    }

    fn flat_group(&mut self, group: &Group<'_>) {
        self.text(group.open, group.open.len());
        for (at, item) in group.items.iter().enumerate() {
            if at > 0 {
                self.text(", ", 2);
            }
            self.flat_nodes(&item.nodes);
        }
        if group.comma == Comma::Always {
            self.text(",", 1);
        }
        self.text(group.close, group.close.len());
    }

    /// Writes a group across lines. A list whose only item is another bracketed list hugs it:
    /// `join([` and `])` share lines, and the inner list's items go one indent deeper.
    fn break_group(&mut self, group: &Group<'_>, indent: usize) {
        if let Some(inner) = hugged(group) {
            self.text(group.open, group.open.len());
            self.break_group(inner, indent);
            self.text(group.close, group.close.len());
        } else {
            self.broken_group(group, indent);
        }
    }

    fn broken_group(&mut self, group: &Group<'_>, indent: usize) {
        if group.literals {
            self.filled_group(group, indent);
            return;
        }
        let inner = indent + INDENT;
        self.text(group.open, group.open.len());
        if let Some(comment) = group.open_comment {
            self.trailing_comment(comment);
        }
        let count = group.items.len();
        for (at, item) in group.items.iter().enumerate() {
            self.start(inner);
            for comment in &item.leading {
                self.own_line_comment(comment, inner);
            }
            let comma = at + 1 < count || group.comma != Comma::Never;
            self.item(item, inner, usize::from(comma), true);
            if comma {
                self.text(",", 1);
            }
            for comment in &item.trailing {
                self.trailing_comment(comment);
            }
        }
        for comment in &group.dangling {
            self.own_line_comment(comment, inner);
        }
        self.start(indent);
        self.text(group.close, group.close.len());
    }
}

impl Writer {
    /// Writes a broken list of plain literals with as many on each line as fit.
    fn filled_group(&mut self, group: &Group<'_>, indent: usize) {
        let inner = indent + INDENT;
        self.text(group.open, group.open.len());
        self.start(inner);
        for (at, item) in group.items.iter().enumerate() {
            let width = item.flat.unwrap_or(0) + 1;
            if at > 0 {
                if self.column + 1 + width <= WIDTH {
                    self.space();
                } else {
                    self.start(inner);
                }
            }
            self.flat_nodes(&item.nodes);
            self.text(",", 1);
        }
        self.start(indent);
        self.text(group.close, group.close.len());
    }
}

/// A comment with one space after its `#` or `##`: `#x` becomes `# x`. A `#!` line is left
/// as it is.
pub(crate) fn spaced(comment: &str) -> String {
    let text = comment.trim_start_matches('#');
    let hashes = comment.len() - text.len();
    if text.is_empty() || text.starts_with([' ', '!']) && (hashes == 1 || text.starts_with(' ')) {
        return comment.to_owned();
    }
    format!("{} {text}", "#".repeat(hashes))
}

/// The list inside a group that holds just that list, with no comments or trailing comma.
fn hugged<'g, 'a>(group: &'g Group<'a>) -> Option<&'g Group<'a>> {
    if group.comma == Comma::Always || group.open_comment.is_some() || !group.dangling.is_empty() {
        return None;
    }
    let [item] = group.items.as_slice() else { return None };
    if !item.leading.is_empty() || !item.trailing.is_empty() {
        return None;
    }
    match item.nodes.as_slice() {
        [Node::Group(inner)] if !inner.items.is_empty() => Some(inner),
        _ => None,
    }
}

/// The precedence of an item's loosest operator, to break before each one, unless the item
/// has a conditional, a comprehension, an arrow function or a match, which read oddly split.
fn loosest_operator(nodes: &[Node<'_>]) -> Option<u8> {
    let blocked = nodes
        .iter()
        .any(|node| matches!(node, Node::Token { text: "if" | "else" | "for" | "=>" | "match", .. }));
    let operators = nodes.iter().skip(1).filter_map(|node| match node {
        Node::Token { precedence, .. } if *precedence > 0 => Some(*precedence),
        _ => None,
    });
    if blocked { None } else { operators.min() }
}

/// For each node, the width that has to fit on the same line after it: the tokens up to the
/// next group's opening bracket or operator break, where a line could break, or else to the
/// end and `rest`.
fn following_widths(nodes: &[Node<'_>], rest: usize, loosest: Option<u8>) -> Vec<usize> {
    let mut follows = vec![0; nodes.len()];
    let mut after = rest;
    for (at, node) in nodes.iter().enumerate().rev() {
        if let Some(slot) = follows.get_mut(at) {
            *slot = after;
        }
        after = match node {
            Node::Token { precedence, .. } if Some(*precedence) == loosest => 0,
            Node::Token { space, width, .. } => usize::from(*space) + width + after,
            Node::Group(group) => usize::from(group.space) + group.open.len(),
            Node::Comment { .. } => 0,
        };
    }
    follows
}
