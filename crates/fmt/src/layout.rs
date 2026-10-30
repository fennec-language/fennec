//! Writes a logical line's tree as physical lines. A group stays on one line if it fits,
//! with what follows it up to the next place a line could break; otherwise each of its
//! items goes on its own line, one indent deeper, each ending with a comma.

use crate::tree::{Group, Item, Node};
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
        self.current.comment = Some(comment.to_owned());
    }

    /// Puts a comment on a line of its own.
    pub(crate) fn own_line_comment(&mut self, comment: &str, indent: usize) {
        self.start(indent);
        self.current.code.push_str(comment);
        self.start(indent);
    }

    /// Writes an item starting on the current line. `rest` is the width that must follow it
    /// on the same line, like the comma after a list item.
    pub(crate) fn item(&mut self, item: &Item<'_>, indent: usize, rest: usize) {
        if let Some(width) = item.flat
            && self.column + width + rest <= WIDTH
        {
            self.flat_nodes(&item.nodes);
            return;
        }
        let follows = following_widths(&item.nodes, rest);
        for (at, node) in item.nodes.iter().enumerate() {
            match node {
                // An item starts its line, where `space` writes nothing.
                Node::Token { text, space, width } => {
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
                Node::Token { text, space, width } => {
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
        if group.trailing_comma {
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
            let comma = at + 1 < count || group.trailing_comma || group.comma_allowed;
            self.item(item, inner, usize::from(comma));
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

/// The list inside a group that holds just that list, with no comments or trailing comma.
fn hugged<'g, 'a>(group: &'g Group<'a>) -> Option<&'g Group<'a>> {
    if group.trailing_comma || group.open_comment.is_some() || !group.dangling.is_empty() {
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

/// For each node, the width that has to fit on the same line after it: the tokens up to the
/// next group's opening bracket, where a line could break, or else to the end and `rest`.
fn following_widths(nodes: &[Node<'_>], rest: usize) -> Vec<usize> {
    let mut follows = vec![0; nodes.len()];
    let mut after = rest;
    for (at, node) in nodes.iter().enumerate().rev() {
        if let Some(slot) = follows.get_mut(at) {
            *slot = after;
        }
        after = match node {
            Node::Token { space, width, .. } => usize::from(*space) + width + after,
            Node::Group(group) => usize::from(group.space) + group.open.len(),
            Node::Comment { .. } => 0,
        };
    }
    follows
}
