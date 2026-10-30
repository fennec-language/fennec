//! A logical line as a tree of bracket groups, so the layout can break the outermost
//! group that doesn't fit. Comments inside brackets are kept in the tree where they were.

use crate::spacing::{Bracket, Tok};
use fennec_syntax::TokenKind;

pub(crate) enum Node<'a> {
    Token {
        text: &'a str,
        space: bool,
        width: usize,
    },
    Group(Group<'a>),
    /// A comment in the middle of a line. A trailing one ends its line; an own-line one
    /// sits on a line of its own.
    Comment {
        text: &'a str,
        own_line: bool,
    },
}

/// A bracketed, comma-separated list.
pub(crate) struct Group<'a> {
    pub(crate) space: bool,
    pub(crate) open: &'a str,
    pub(crate) close: &'a str,
    /// A comment right after the opening bracket.
    pub(crate) open_comment: Option<&'a str>,
    pub(crate) items: Vec<Item<'a>>,
    /// Own-line comments before the closing bracket.
    pub(crate) dangling: Vec<&'a str>,
    /// Whether the source ends the list with a comma.
    pub(crate) trailing_comma: bool,
    /// Whether a comma can be added after the last item when the list is broken.
    pub(crate) comma_allowed: bool,
    /// The width written on one line, or None if it must break.
    pub(crate) flat: Option<usize>,
}

/// One element of a group, or a whole logical line.
pub(crate) struct Item<'a> {
    pub(crate) leading: Vec<&'a str>,
    pub(crate) nodes: Vec<Node<'a>>,
    /// Comments after the item, written after its comma.
    pub(crate) trailing: Vec<&'a str>,
    pub(crate) flat: Option<usize>,
}

/// The tokens of a logical line with their comments and spacing.
pub(crate) struct Line<'t, 'a> {
    pub(crate) toks: &'t [Tok<'a>],
    pub(crate) leading: &'t [&'t [&'a str]],
    pub(crate) trailing: &'t [Option<&'a str>],
    /// Whether each token has a space before it.
    pub(crate) space: &'t [bool],
    /// What each opening bracket is for.
    pub(crate) brackets: &'t [Option<Bracket>],
}

impl<'a> Line<'_, 'a> {
    fn text(&self, at: usize) -> &'a str {
        self.toks.get(at).map_or("", |tok| tok.text)
    }

    fn is_punct(&self, at: usize, text: &str) -> bool {
        self.toks.get(at).is_some_and(|tok| tok.kind == TokenKind::Punct && tok.text == text)
    }

    fn leading(&self, at: usize) -> &[&'a str] {
        self.leading.get(at).copied().unwrap_or(&[])
    }

    fn trailing(&self, at: usize) -> Option<&'a str> {
        self.trailing.get(at).copied().flatten()
    }

    fn space(&self, at: usize) -> bool {
        self.space.get(at).copied().unwrap_or(false)
    }

    /// The line cut in two before token `at`.
    pub(crate) fn split_at(&self, at: usize) -> (Self, Self) {
        let cut = |tokens: usize| tokens.min(at);
        let (toks, rest_toks) = self.toks.split_at(cut(self.toks.len()));
        let (leading, rest_leading) = self.leading.split_at(cut(self.leading.len()));
        let (trailing, rest_trailing) = self.trailing.split_at(cut(self.trailing.len()));
        let (space, rest_space) = self.space.split_at(cut(self.space.len()));
        let (brackets, rest_brackets) = self.brackets.split_at(cut(self.brackets.len()));
        (
            Line { toks, leading, trailing, space, brackets },
            Line {
                toks: rest_toks,
                leading: rest_leading,
                trailing: rest_trailing,
                space: rest_space,
                brackets: rest_brackets,
            },
        )
    }

    /// The whole line as one item. The first token's leading comments and the last token's
    /// trailing comment belong to the line, so they're left out.
    pub(crate) fn build(&self) -> Item<'a> {
        let (mut item, _) = self.item(0, self.toks.len(), false);
        item.leading.clear();
        item.trailing.clear();
        item
    }

    /// Builds the item starting at `at`, up to `end` or, inside brackets, a comma or closing
    /// bracket at this depth. Returns the item and where it stopped.
    fn item(&self, start: usize, end: usize, in_group: bool) -> (Item<'a>, usize) {
        let mut item = Item { leading: Vec::new(), nodes: Vec::new(), trailing: Vec::new(), flat: Some(0) };
        let mut at = start;
        while at < end {
            if in_group && (self.is_punct(at, ",") || self.closes(at)) {
                break;
            }
            let leading = self.leading(at);
            if at == start {
                item.leading.extend_from_slice(leading);
            } else {
                item.nodes.extend(leading.iter().map(|text| Node::Comment { text, own_line: true }));
            }
            let last = if self.opens(at) {
                let (group, close) = self.group(at, end);
                item.nodes.push(Node::Group(group));
                close
            } else {
                let text = self.text(at);
                item.nodes.push(Node::Token { text, space: self.space(at), width: text.chars().count() });
                at
            };
            at = last + 1;
            if let Some(comment) = self.trailing(last) {
                if at >= end || in_group && (self.is_punct(at, ",") || self.closes(at)) {
                    item.trailing.push(comment);
                } else {
                    item.nodes.push(Node::Comment { text: comment, own_line: false });
                }
            }
        }
        item.flat = flat_width(&item.nodes);
        (item, at)
    }

    fn opens(&self, at: usize) -> bool {
        self.toks
            .get(at)
            .is_some_and(|tok| tok.kind == TokenKind::Punct && matches!(tok.text, "(" | "[" | "{"))
    }

    fn closes(&self, at: usize) -> bool {
        self.toks
            .get(at)
            .is_some_and(|tok| tok.kind == TokenKind::Punct && matches!(tok.text, ")" | "]" | "}"))
    }

    /// Builds the group opening at `open`. Returns it and the index of its closing bracket.
    fn group(&self, open: usize, end: usize) -> (Group<'a>, usize) {
        let mut group = Group {
            space: self.space(open),
            open: self.text(open),
            close: "",
            open_comment: self.trailing(open),
            items: Vec::new(),
            dangling: Vec::new(),
            trailing_comma: false,
            comma_allowed: false,
            flat: None,
        };
        let mut at = open + 1;
        let mut has_for = false;
        while at < end && !self.closes(at) {
            let (mut item, stop) = self.item(at, end, true);
            has_for |= item.nodes.iter().any(|node| matches!(node, Node::Token { text: "for", .. }));
            at = stop;
            if item.nodes.is_empty() {
                group.dangling.extend(item.leading);
                break;
            }
            group.trailing_comma = false;
            if self.is_punct(at, ",") {
                item.trailing.extend(self.leading(at).iter().copied());
                item.trailing.extend(self.trailing(at));
                group.trailing_comma = true;
                at += 1;
            }
            group.items.push(item);
        }
        group.dangling.extend_from_slice(self.leading(at));
        group.close = self.text(at);
        let bracket = self.brackets.get(open).copied().flatten().unwrap_or(Bracket::Paren);
        group.comma_allowed = comma_allowed(bracket, group.items.len(), group.trailing_comma, has_for);
        let single_tuple = bracket == Bracket::Paren && group.items.len() == 1;
        let magic = group.trailing_comma && !single_tuple;
        group.flat = if magic { None } else { group_flat(&group) };
        (group, at)
    }
}

/// Whether a comma may follow the last item, so the list can end with one when broken.
fn comma_allowed(bracket: Bracket, items: usize, trailing_comma: bool, has_for: bool) -> bool {
    match bracket {
        Bracket::Call | Bracket::Params | Bracket::List | Bracket::Brace => !has_for,
        Bracket::Paren => items > 1 || trailing_comma,
        Bracket::LambdaParams | Bracket::Index | Bracket::Generics => trailing_comma,
    }
}

fn flat_width(nodes: &[Node<'_>]) -> Option<usize> {
    let mut width = 0;
    for (at, node) in nodes.iter().enumerate() {
        width += match node {
            Node::Token { space, width, .. } => usize::from(*space && at > 0) + width,
            Node::Group(group) => usize::from(group.space && at > 0) + group.flat?,
            Node::Comment { .. } => return None,
        };
    }
    Some(width)
}

fn group_flat(group: &Group<'_>) -> Option<usize> {
    if group.open_comment.is_some() || !group.dangling.is_empty() {
        return None;
    }
    let mut width = group.open.len() + group.close.len() + usize::from(group.trailing_comma);
    for (at, item) in group.items.iter().enumerate() {
        if !item.leading.is_empty() || !item.trailing.is_empty() {
            return None;
        }
        width += item.flat? + if at > 0 { 2 } else { 0 };
    }
    Some(width)
}
