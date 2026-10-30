//! A logical line as a tree of bracket groups, so the layout can break the outermost
//! group that doesn't fit. Comments inside brackets are kept in the tree where they were.

use crate::spacing::{Bracket, Spacing, Tok};
use fennec_syntax::TokenKind;

pub(crate) enum Node<'a> {
    Token {
        text: &'a str,
        space: bool,
        width: usize,
        /// How loosely it binds as a binary operator, or 0.
        precedence: u8,
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
    /// When the last item gets a comma after it.
    pub(crate) comma: Comma,
    /// The width written on one line, or None if it must break.
    pub(crate) flat: Option<usize>,
    /// Whether every item is a plain literal, so a broken list fills its lines.
    pub(crate) literals: bool,
}

/// When a list's last item gets a comma after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Comma {
    /// Never, because the parser wouldn't take one there: after a comprehension, or in an index.
    Never,
    /// When the list is broken across lines.
    WhenBroken,
    /// Always, even on one line: a one-item tuple, where the comma makes it a tuple.
    Always,
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
    /// How loosely each binary operator binds, or 0.
    pub(crate) precedence: &'t [u8],
}

impl<'t, 'a> Line<'t, 'a> {
    pub(crate) fn new(
        toks: &'t [Tok<'a>],
        leading: &'t [&'t [&'a str]],
        trailing: &'t [Option<&'a str>],
        spacing: &'t Spacing,
    ) -> Self {
        Line {
            toks,
            leading,
            trailing,
            space: &spacing.space,
            brackets: &spacing.brackets,
            precedence: &spacing.precedence,
        }
    }

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
        fn cut<T>(slice: &[T], at: usize) -> (&[T], &[T]) {
            slice.split_at(at.min(slice.len()))
        }
        let (toks, rest_toks) = cut(self.toks, at);
        let (leading, rest_leading) = cut(self.leading, at);
        let (trailing, rest_trailing) = cut(self.trailing, at);
        let (space, rest_space) = cut(self.space, at);
        let (brackets, rest_brackets) = cut(self.brackets, at);
        let (precedence, rest_precedence) = cut(self.precedence, at);
        (
            Line { toks, leading, trailing, space, brackets, precedence },
            Line {
                toks: rest_toks,
                leading: rest_leading,
                trailing: rest_trailing,
                space: rest_space,
                brackets: rest_brackets,
                precedence: rest_precedence,
            },
        )
    }

    /// The line laid out to fit in `room`: as it is if it fits, or else with its expression
    /// wrapped in parentheses when that's what lets it break across lines.
    pub(crate) fn shaped(&self, room: usize) -> Item<'a> {
        let item = self.build();
        if item.flat.is_none_or(|width| width <= room) {
            return item;
        }
        self.wrap_range().map_or(item, |(from, to)| self.wrapped(from, to))
    }

    /// The expression a too-long statement can wrap in parentheses: an assignment's value,
    /// what follows `return`, `assert`, `fail` or `yield`, or the condition of an `if` or
    /// `while`. Only one with an operator to break at, and nothing that reads oddly split.
    fn wrap_range(&self) -> Option<(usize, usize)> {
        let mut depth = 0_usize;
        let depths: Vec<usize> = self
            .toks
            .iter()
            .map(|tok| {
                let before = depth;
                match tok.text {
                    "(" | "[" | "{" if tok.kind == TokenKind::Punct => depth += 1,
                    ")" | "]" | "}" if tok.kind == TokenKind::Punct => depth = depth.saturating_sub(1),
                    _ => {}
                }
                before.min(depth)
            })
            .collect();
        let top = |at: usize| depths.get(at) == Some(&0);
        let first = self.toks.first()?;
        let len = self.toks.len();
        let (from, to) = match first.text {
            "return" | "assert" | "fail" | "yield" if first.kind == TokenKind::Keyword => (1, len),
            "if" | "elif" | "while" if first.kind == TokenKind::Keyword => {
                (1, len.checked_sub(1).filter(|&last| self.text(last) == ":")?)
            }
            _ => {
                let assign =
                    (0..len).find(|&at| top(at) && self.is_punct_in(at, &["=", "+=", "-=", "*=", "/="]))?;
                (assign + 1, len)
            }
        };
        let range = from..to;
        let operator = range.clone().any(|at| top(at) && self.precedence.get(at).is_some_and(|&p| p > 0));
        let blocked = range
            .clone()
            .any(|at| top(at) && matches!(self.text(at), "if" | "else" | "for" | "=>" | "match"));
        let one_group = self.opens(from) && (from + 1..to).all(|at| !top(at) || at + 1 == to);
        (operator && !blocked && !one_group && from < to).then_some((from, to))
    }

    fn is_punct_in(&self, at: usize, texts: &[&str]) -> bool {
        self.toks.get(at).is_some_and(|tok| tok.kind == TokenKind::Punct && texts.contains(&tok.text))
    }

    /// The line with tokens `from..to` wrapped in parentheses that have to break.
    fn wrapped(&self, from: usize, to: usize) -> Item<'a> {
        let (mut head, _) = self.item(0, from, false);
        let (mut inner, _) = self.item(from, to, false);
        let (tail, _) = self.item(to, self.toks.len(), false);
        inner.leading.clear();
        inner.trailing.clear();
        head.leading.clear();
        head.trailing.clear();
        head.nodes.push(Node::Group(Group {
            space: from > 0,
            open: "(",
            close: ")",
            open_comment: None,
            items: vec![inner],
            dangling: Vec::new(),
            comma: Comma::Never,
            flat: None,
            literals: false,
        }));
        head.nodes.extend(tail.nodes);
        head.flat = None;
        head
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
        // Each pass takes at least one token, so this bounds the loop.
        for _ in start..end {
            if at >= end {
                break;
            }
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
                let precedence = self.precedence.get(at).copied().unwrap_or(0);
                item.nodes.push(Node::Token {
                    text,
                    space: self.space(at),
                    width: text.chars().count(),
                    precedence,
                });
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
            comma: Comma::Never,
            flat: None,
            literals: false,
        };
        let mut at = open + 1;
        let mut has_for = false;
        let mut ends_with_comma = false;
        while at < end && !self.closes(at) {
            let (mut item, stop) = self.item(at, end, true);
            has_for |= item.nodes.iter().any(|node| matches!(node, Node::Token { text: "for", .. }));
            at = stop;
            if item.nodes.is_empty() {
                group.dangling.extend(item.leading);
                break;
            }
            ends_with_comma = false;
            if self.is_punct(at, ",") {
                item.trailing.extend(self.leading(at).iter().copied());
                item.trailing.extend(self.trailing(at));
                ends_with_comma = true;
                at += 1;
            }
            group.items.push(item);
        }
        group.dangling.extend_from_slice(self.leading(at));
        group.close = self.text(at);
        let bracket = self.brackets.get(open).copied().flatten().unwrap_or(Bracket::Paren);
        group.comma = comma(bracket, group.items.len(), ends_with_comma, has_for);
        group.flat = group_flat(&group);
        group.literals = group.items.len() > 1 && group.items.iter().all(is_literal);
        (group, at)
    }
}

/// The longest string, quotes included, that counts as a plain literal for filling lines.
const SHORT_STRING: usize = 20;

/// Whether an item is a plain literal: a number, possibly negative, `True`, `False`, `None`, or
/// a short string.
fn is_literal(item: &Item<'_>) -> bool {
    let plain = |text: &str| {
        text.starts_with(|c: char| c.is_ascii_digit())
            || matches!(text, "True" | "False" | "None")
            || text.starts_with('"') && text.chars().count() <= SHORT_STRING
    };
    item.leading.is_empty()
        && item.trailing.is_empty()
        && match item.nodes.as_slice() {
            [Node::Token { text, .. }] => plain(text),
            [Node::Token { text: "-", .. }, Node::Token { text, .. }] => {
                text.starts_with(|c: char| c.is_ascii_digit())
            }
            _ => false,
        }
}

/// When a list's last item gets a comma: when it's broken, wherever the parser takes one.
fn comma(bracket: Bracket, items: usize, ends_with_comma: bool, has_for: bool) -> Comma {
    let allowed = match bracket {
        Bracket::Call | Bracket::Params | Bracket::Literal => !has_for,
        Bracket::Paren => items > 1 || ends_with_comma,
        Bracket::LambdaParams | Bracket::Index | Bracket::Generics => ends_with_comma,
    };
    if bracket == Bracket::Paren && items == 1 && ends_with_comma {
        Comma::Always
    } else if allowed {
        Comma::WhenBroken
    } else {
        Comma::Never
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
    let mut width = group.open.len() + group.close.len() + usize::from(group.comma == Comma::Always);
    for (at, item) in group.items.iter().enumerate() {
        if !item.leading.is_empty() || !item.trailing.is_empty() {
            return None;
        }
        width += item.flat? + if at > 0 { 2 } else { 0 };
    }
    Some(width)
}
