//! Patterns to a syntax tree: literals, `.`, classes like `[a-z]` and `\d`, anchors, `* + ?`
//! and their lazy forms, `{m,n}`, groups, `(?:...)` and `|`.

use crate::Error;

/// The most a `{m,n}` repeat may ask for.
const MAX_REPEAT: u32 = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Node {
    Empty,
    Char(char),
    /// `.`: any character but a newline.
    Any,
    Class(Class),
    Look(Look),
    /// A group, numbered from 1 when it captures.
    Group(Option<usize>, Box<Node>),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: u32,
        max: Option<u32>,
        greedy: bool,
    },
}

/// A place in the text a pattern can require without matching a character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Look {
    Start,
    End,
    WordBoundary,
    NotWordBoundary,
}

/// A set of characters, like `[a-z_]` or `\d`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Class {
    pub(crate) items: Vec<Item>,
    pub(crate) negated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    Range(char, char),
    /// `\d`, `\w` or `\s`, or their opposites when negated.
    Named(Named, bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Named {
    Digit,
    Word,
    Space,
}

impl Named {
    fn matches(self, c: char) -> bool {
        match self {
            Named::Digit => c.is_ascii_digit(),
            Named::Word => is_word(c),
            Named::Space => c.is_whitespace(),
        }
    }
}

/// What `\w` matches: letters, digits and `_`.
pub(crate) fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Class {
    pub(crate) fn matches(&self, c: char) -> bool {
        let found = self.items.iter().any(|item| match *item {
            Item::Range(low, high) => low <= c && c <= high,
            Item::Named(named, negated) => named.matches(c) != negated,
        });
        found != self.negated
    }

    fn named(named: Named, negated: bool) -> Class {
        Class { items: vec![Item::Named(named, false)], negated }
    }
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    groups: usize,
}

/// Parses a pattern, giving back its tree and how many groups capture.
pub(crate) fn parse(pattern: &str) -> Result<(Node, usize), Error> {
    let mut parser = Parser { chars: pattern.chars().collect(), pos: 0, groups: 0 };
    let node = parser.alternation()?;
    match parser.peek() {
        Some(')') => Err(parser.error("this `)` has no `(` before it")),
        Some(_) => Err(parser.error("unexpected character")),
        None => Ok((node, parser.groups)),
    }
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.pos + ahead).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        let found = self.peek() == Some(c);
        if found {
            self.pos += 1;
        }
        found
    }

    fn error(&self, message: &str) -> Error {
        Error { message: message.to_owned(), at: self.pos }
    }

    fn error_at(message: &str, at: usize) -> Error {
        Error { message: message.to_owned(), at }
    }

    /// `a|b|c`.
    fn alternation(&mut self) -> Result<Node, Error> {
        let mut options = vec![self.concat()?];
        while self.eat('|') {
            options.push(self.concat()?);
        }
        Ok(if options.len() == 1 { options.pop().unwrap_or(Node::Empty) } else { Node::Alt(options) })
    }

    /// Atoms one after another, up to a `|` or `)`.
    fn concat(&mut self) -> Result<Node, Error> {
        let mut parts = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            parts.push(self.repeats(atom)?);
        }
        Ok(match parts.len() {
            0 => Node::Empty,
            1 => parts.pop().unwrap_or(Node::Empty),
            _ => Node::Concat(parts),
        })
    }

    /// The `*`, `+`, `?` or `{m,n}` after an atom, if any, and a `?` making it lazy.
    fn repeats(&mut self, atom: Node) -> Result<Node, Error> {
        let start = self.pos;
        let Some((min, max)) = self.quantifier()? else { return Ok(atom) };
        if matches!(atom, Node::Look(_) | Node::Empty) {
            return Err(Self::error_at("there's nothing here to repeat", start));
        }
        let greedy = !self.eat('?');
        if matches!(self.peek(), Some('*' | '+' | '?'))
            || (self.peek() == Some('{') && self.counted().is_some())
        {
            return Err(self.error("this repeats a repeat; put the inner one in a group"));
        }
        Ok(Node::Repeat { node: Box::new(atom), min, max, greedy })
    }

    fn quantifier(&mut self) -> Result<Option<(u32, Option<u32>)>, Error> {
        let bounds = match self.peek() {
            Some('*') => (0, None),
            Some('+') => (1, None),
            Some('?') => (0, Some(1)),
            Some('{') => {
                let Some((bounds, length)) = self.counted() else { return Ok(None) };
                let start = self.pos;
                self.pos += length;
                if bounds.1.is_some_and(|max| max < bounds.0) {
                    return Err(Self::error_at("in `{m,n}`, m can't be bigger than n", start));
                }
                if bounds.0 > MAX_REPEAT || bounds.1.is_some_and(|max| max > MAX_REPEAT) {
                    return Err(Self::error_at("repeats are limited to 1000", start));
                }
                return Ok(Some(bounds));
            }
            _ => return Ok(None),
        };
        self.pos += 1;
        Ok(Some(bounds))
    }

    /// Reads `{m}`, `{m,}`, `{,n}` or `{m,n}` without moving, giving its bounds and length.
    /// Anything else starting with `{` is a plain `{`, as in Python.
    fn counted(&self) -> Option<((u32, Option<u32>), usize)> {
        let rest: String = self.chars.get(self.pos..)?.iter().take_while(|&&c| c != '}').collect();
        let inner = rest.strip_prefix('{')?;
        if self.chars.get(self.pos + rest.chars().count()) != Some(&'}') {
            return None;
        }
        let number = |text: &str| -> Option<u32> {
            if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            Some(text.parse().unwrap_or(u32::MAX))
        };
        let bounds = match inner.split_once(',') {
            None => {
                let count = number(inner)?;
                (count, Some(count))
            }
            Some((low, high)) => {
                let min = if low.is_empty() { 0 } else { number(low)? };
                let max = if high.is_empty() { None } else { Some(number(high)?) };
                if low.is_empty() && high.is_empty() {
                    return None;
                }
                (min, max)
            }
        };
        Some((bounds, rest.chars().count() + 1))
    }

    fn atom(&mut self) -> Result<Node, Error> {
        let start = self.pos;
        let Some(c) = self.peek() else { return Ok(Node::Empty) };
        if matches!(c, '*' | '+' | '?') || (c == '{' && self.counted().is_some()) {
            return Err(self.error("there's nothing here to repeat"));
        }
        self.pos += 1;
        Ok(match c {
            '(' => self.group(start)?,
            '[' => Node::Class(self.class(start)?),
            '.' => Node::Any,
            '^' => Node::Look(Look::Start),
            '$' => Node::Look(Look::End),
            '\\' => self.escape(false)?,
            c => Node::Char(c),
        })
    }

    fn group(&mut self, start: usize) -> Result<Node, Error> {
        let number = if self.peek() == Some('?') {
            if self.peek_at(1) != Some(':') {
                return Err(self.error("the only group extension so far is `(?:...)`"));
            }
            self.pos += 2;
            None
        } else {
            self.groups += 1;
            Some(self.groups)
        };
        let inner = self.alternation()?;
        if !self.eat(')') {
            return Err(Self::error_at("this `(` is never closed", start));
        }
        Ok(Node::Group(number, Box::new(inner)))
    }

    /// After a `\`. Inside a class, `\b` is a backspace rather than a word boundary.
    fn escape(&mut self, in_class: bool) -> Result<Node, Error> {
        let at = self.pos.saturating_sub(1);
        let Some(c) = self.peek() else {
            return Err(Self::error_at("a `\\` at the end of a pattern escapes nothing", at));
        };
        self.pos += 1;
        Ok(match c {
            'd' => Node::Class(Class::named(Named::Digit, false)),
            'D' => Node::Class(Class::named(Named::Digit, true)),
            'w' => Node::Class(Class::named(Named::Word, false)),
            'W' => Node::Class(Class::named(Named::Word, true)),
            's' => Node::Class(Class::named(Named::Space, false)),
            'S' => Node::Class(Class::named(Named::Space, true)),
            'b' if in_class => Node::Char('\u{8}'),
            'b' => Node::Look(Look::WordBoundary),
            'B' if !in_class => Node::Look(Look::NotWordBoundary),
            'n' => Node::Char('\n'),
            't' => Node::Char('\t'),
            'r' => Node::Char('\r'),
            'f' => Node::Char('\u{c}'),
            'v' => Node::Char('\u{b}'),
            '0' => Node::Char('\0'),
            c if c.is_ascii_alphanumeric() => {
                return Err(Self::error_at(&format!("`\\{c}` isn't an escape patterns know"), at));
            }
            c => Node::Char(c),
        })
    }

    /// `[...]`, after the `[`.
    fn class(&mut self, start: usize) -> Result<Class, Error> {
        let negated = self.eat('^');
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let Some(c) = self.peek() else {
                return Err(Self::error_at("this `[` is never closed", start));
            };
            if c == ']' && !first {
                self.pos += 1;
                break;
            }
            first = false;
            let low = self.class_char()?;
            let low = match low {
                Node::Char(low) => low,
                Node::Class(class) => {
                    items.extend(class.items.iter().map(|item| match *item {
                        Item::Named(named, _) => Item::Named(named, class.negated),
                        range @ Item::Range(..) => range,
                    }));
                    continue;
                }
                _ => continue,
            };
            if self.peek() == Some('-') && self.peek_at(1).is_some_and(|next| next != ']') {
                let dash = self.pos;
                self.pos += 1;
                let Node::Char(high) = self.class_char()? else {
                    return Err(Self::error_at("a range needs a character at each end", dash));
                };
                if high < low {
                    return Err(Self::error_at(&format!("the range `{low}-{high}` is backwards"), dash));
                }
                items.push(Item::Range(low, high));
            } else {
                items.push(Item::Range(low, low));
            }
        }
        Ok(Class { items, negated })
    }

    fn class_char(&mut self) -> Result<Node, Error> {
        let c = self.peek().unwrap_or(']');
        self.pos += 1;
        if c == '\\' { self.escape(true) } else { Ok(Node::Char(c)) }
    }
}
