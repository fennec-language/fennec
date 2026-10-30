//! The file as blocks of logical lines: indentation, blank lines between lines, and the
//! comments on lines of their own between them.

mod bodies;
mod imports;

use std::collections::HashSet;

use crate::comments::{Comments, text};
use crate::layout::Writer;
use crate::spacing::{BlockKind, Tok, analyze};
use crate::tree::{Item, Line};
use crate::{INDENT, WIDTH};
use fennec_syntax::{SourceToken, TokenKind};

struct Block {
    /// How far the source indented this block, to place comments by their column.
    width: u32,
    kind: BlockKind,
    /// Whether anything has been written in this block yet.
    started: bool,
    /// Whether the last line written at this level was a declaration with a body.
    last_was_item: bool,
}

impl Block {
    fn new(width: u32, kind: BlockKind) -> Self {
        Block { width, kind, started: false, last_was_item: false }
    }
}

/// An own-line comment, or the line it comes before, waiting to be written.
struct Entry<'a> {
    comment: Option<&'a str>,
    level: usize,
    /// Blank lines before it in the source.
    blank: usize,
}

pub(crate) struct Driver<'s, 't> {
    tokens: &'t [SourceToken],
    /// Every token with its text.
    all: Vec<Tok<'s>>,
    /// Tokens already written as part of an earlier line: a guard clause's body, joined on.
    skipped: HashSet<usize>,
    comments: Comments<'s>,
    pub(crate) writer: Writer,
    blocks: Vec<Block>,
    /// The source indentation of each block open at the end of the previous line.
    last_widths: Vec<u32>,
    /// Where the last thing written ended in the source, to count blank lines.
    prev_end: Option<u32>,
    /// The kind of block the next indent opens.
    pending: BlockKind,
}

impl<'s, 't> Driver<'s, 't> {
    pub(crate) fn new(source: &'s str, tokens: &'t [SourceToken], comments: Comments<'s>) -> Self {
        let all =
            tokens.iter().map(|token| Tok { kind: token.kind, text: text(source, token.span) }).collect();
        Driver {
            tokens,
            all,
            skipped: HashSet::new(),
            comments,
            writer: Writer::default(),
            blocks: vec![Block::new(0, BlockKind::TopLevel)],
            last_widths: vec![0],
            prev_end: None,
            pending: BlockKind::Other,
        }
    }

    /// Writes the file line by line. Each chunk of tokens up to a line break is the indents
    /// and dedents before a logical line, then the line.
    pub(crate) fn run(&mut self) {
        let tokens = self.tokens;
        let mut offset = 0;
        for chunk in tokens.split_inclusive(|token| token.kind == TokenKind::Newline) {
            let before_break = chunk.iter().take_while(|token| token.kind != TokenKind::Newline);
            for (index, token) in before_break.enumerate() {
                let at = offset + index;
                if self.skipped.contains(&at) {
                    continue;
                }
                match token.kind {
                    TokenKind::Indent => {
                        let width =
                            tokens.get(at + 1).map_or(0, |next| self.comments.column(next.span.start));
                        let kind = std::mem::replace(&mut self.pending, BlockKind::Other);
                        self.blocks.push(Block::new(width, kind));
                    }
                    // Dedents never outnumber indents, so the top level is never popped.
                    TokenKind::Dedent => {
                        self.blocks.pop();
                    }
                    TokenKind::Eof => self.end_of_file(at),
                    _ if self.is_import(at) => {
                        self.imports(at);
                        break;
                    }
                    _ => {
                        let end = chunk.iter().position(|token| token.kind == TokenKind::Newline);
                        self.line(at, offset + end.unwrap_or(chunk.len()));
                        break;
                    }
                }
            }
            offset += chunk.len();
        }
    }

    fn level(&self) -> usize {
        self.blocks.len().saturating_sub(1)
    }

    fn toks(&self, start: usize, end: usize) -> &[Tok<'s>] {
        self.all.get(start..end).unwrap_or(&[])
    }

    /// Writes the logical line made of tokens `start..end`, with the comments before it. A
    /// guard clause written as a block is joined onto one line when it fits.
    fn line(&mut self, start: usize, end: usize) {
        let level = self.level();
        let kind = self.blocks.last().map_or(BlockKind::Other, |block| block.kind);
        let is_item = is_item(self.toks(start, end), kind);
        let first = self.tokens.get(start).map_or(0, |token| token.span.start);
        let entries = self.entries(start, first, level);
        self.write_entries(&entries, level, is_item);

        let joined = self.guard_block(start, end).filter(|&(body_start, body_end)| {
            self.write_line(&[(start, end), (body_start, body_end)], level, kind, body_end + 2, true)
        });
        let last = if let Some((_, body_end)) = joined {
            self.skipped.extend(end + 1..=body_end + 1);
            body_end
        } else {
            self.write_line(&[(start, end)], level, kind, end + 1, false);
            end
        };
        self.prev_end = last.checked_sub(1).and_then(|at| self.tokens.get(at)).map(|token| token.span.end);
        if let Some(block) = self.blocks.last_mut() {
            block.started = true;
            block.last_was_item = is_item;
        }
        self.pending = if joined.is_some() { BlockKind::Other } else { opened_kind(self.toks(start, end)) };
        self.last_widths = self.blocks.iter().map(|block| block.width).collect();
    }

    /// Writes one logical line made of the tokens in `ranges`, at `level`. A body after its
    /// `:` moves to a line of its own, unless it makes a guard clause that fits; `next` is
    /// the first token after the statement, to see whether an `else` follows. With `only_if_fits`,
    /// a line that doesn't fit isn't written, and the result says so.
    fn write_line(
        &mut self,
        ranges: &[(usize, usize)],
        level: usize,
        kind: BlockKind,
        next: usize,
        only_if_fits: bool,
    ) -> bool {
        let indexes: Vec<usize> = ranges.iter().flat_map(|&(start, end)| start..end).collect();
        let toks: Vec<Tok<'_>> = indexes.iter().filter_map(|&at| self.all.get(at).copied()).collect();
        let spacing = analyze(&toks, kind);
        let leading: Vec<&[&str]> = indexes.iter().map(|&at| self.comments.leading(at)).collect();
        let trailing: Vec<Option<&str>> = indexes.iter().map(|&at| self.comments.trailing(at)).collect();
        let line = Line::new(&toks, &leading, &trailing, &spacing);
        let room = WIDTH.saturating_sub(level * INDENT);
        let item = line.build();
        let fits = item.flat.is_some_and(|width| level * INDENT + width <= WIDTH);
        if only_if_fits && !fits {
            return false;
        }
        let comment = trailing.last().copied().flatten();
        let split = bodies::inline_body(&toks, kind).filter(|&colon| {
            let (header, body) = toks.split_at(colon + 1);
            !(bodies::guards(header, body) && fits && !self.has_else(next))
        });
        match split {
            Some(colon) => {
                let (header, body) = line.split_at(colon + 1);
                write(&mut self.writer, &header.shaped(room), level, None);
                write(&mut self.writer, &body.shaped(room.saturating_sub(INDENT)), level + 1, comment);
            }
            None => write(&mut self.writer, &line.shaped(room), level, comment),
        }
        true
    }

    /// The own-line comments before token `at`, then the line itself, with their levels and
    /// the blank lines before each in the source.
    fn entries(&mut self, at: usize, first: u32, level: usize) -> Vec<Entry<'s>> {
        let texts = self.comments.leading(at).to_vec();
        let starts = self.comments.starts.get(at).cloned().unwrap_or_default();
        let mut entries = Vec::with_capacity(texts.len() + 1);
        for (comment, start) in texts.into_iter().zip(starts) {
            let blank = self.prev_end.map_or(0, |end| self.comments.blank_lines(end, start));
            entries.push(Entry { comment: Some(comment), level: self.comment_level(start, level), blank });
            self.prev_end = Some(start);
        }
        let blank = self.prev_end.map_or(0, |end| self.comments.blank_lines(end, first));
        entries.push(Entry { comment: None, level, blank });
        entries
    }

    /// The level of an own-line comment between blocks: as deep as its column reaches, but no
    /// deeper than the block above it and no shallower than the line below it.
    fn comment_level(&self, offset: u32, level: usize) -> usize {
        let column = self.comments.column(offset);
        let deepest = self.last_widths.len().saturating_sub(1);
        if deepest <= level {
            return level;
        }
        (level..=deepest)
            .rev()
            .find(|&depth| self.last_widths.get(depth).is_some_and(|&width| width <= column))
            .unwrap_or(level)
    }

    /// Writes the comments before a line and the blank lines between them. Declarations at the
    /// top level and methods in a type get one blank line around them, comments included.
    fn write_entries(&mut self, entries: &[Entry<'_>], level: usize, is_item: bool) {
        let kind = self.blocks.last().map_or(BlockKind::Other, |block| block.kind);
        let separated = matches!(kind, BlockKind::TopLevel | BlockKind::TypeBody)
            && (is_item || self.blocks.last().is_some_and(|block| block.last_was_item));
        // The comments right above the line, with no blank line between, go with it.
        let touching = entries
            .windows(2)
            .rev()
            .take_while(|pair| matches!(pair, [above, below] if below.blank == 0 && above.level == level))
            .count();
        let attached = entries.len().saturating_sub(1 + touching);
        for (at, entry) in entries.iter().enumerate() {
            let started = self.blocks.get(entry.level).is_none_or(|block| block.started);
            let wanted = entry.blank.min(1) == 1 || separated && at == attached;
            if started && wanted {
                self.writer.blank();
            }
            if let Some(comment) = entry.comment {
                self.writer.own_line_comment(comment, entry.level * INDENT);
                if let Some(block) = self.blocks.get_mut(entry.level) {
                    block.started = true;
                }
            }
        }
    }

    fn end_of_file(&mut self, at: usize) {
        let texts = self.comments.leading(at).to_vec();
        let starts = self.comments.starts.get(at).cloned().unwrap_or_default();
        for (comment, start) in texts.into_iter().zip(starts) {
            let blank = self.prev_end.map_or(0, |end| self.comments.blank_lines(end, start));
            if blank > 0 && self.prev_end.is_some() {
                self.writer.blank();
            }
            self.writer.own_line_comment(comment, self.comment_level(start, 0) * INDENT);
            self.prev_end = Some(start);
        }
    }
}

/// Whether a line is a declaration that gets blank lines around it: anything with a body at
/// the top level, where only declarations have one, or a method in a type.
fn is_item(toks: &[Tok<'_>], kind: BlockKind) -> bool {
    match kind {
        BlockKind::TopLevel => top_level_colon(toks),
        BlockKind::TypeBody => top_level_colon(toks) && toks.iter().any(|tok| tok.text == "def"),
        BlockKind::MatchBody | BlockKind::Other => false,
    }
}

/// Writes one line of code at `level`, with the comment that ends it.
fn write(writer: &mut Writer, item: &Item<'_>, level: usize, comment: Option<&str>) {
    let indent = level * INDENT;
    writer.start(indent);
    writer.item(item, indent, 0, false);
    if let Some(comment) = comment {
        writer.trailing_comment(comment);
    }
    writer.finish();
}

/// Whether a line has a `:` outside brackets, which a line with a body always has.
fn top_level_colon(toks: &[Tok<'_>]) -> bool {
    let mut depth = 0_usize;
    for tok in toks.iter().filter(|tok| tok.kind == TokenKind::Punct) {
        match tok.text {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth = depth.saturating_sub(1),
            ":" if depth == 0 => return true,
            _ => {}
        }
    }
    false
}

/// The kind of block a line opens, if an indented block follows it.
fn opened_kind(toks: &[Tok<'_>]) -> BlockKind {
    let mut words = toks.iter().skip_while(|tok| matches!(tok.text, "export" | "noalloc"));
    if let (Some(first), Some(second)) = (words.next(), words.next())
        && first.kind == TokenKind::Name
        && second.kind == TokenKind::Name
        && matches!(first.text, "struct" | "enum" | "error" | "interface")
    {
        return BlockKind::TypeBody;
    }
    let starts_match = toks.windows(2).any(|pair| {
        matches!(pair, [word, next] if word.kind == TokenKind::Name
            && word.text == "match"
            && !matches!(next.text, "=" | "(" | "." | "," | ")" | ":"))
    });
    if starts_match { BlockKind::MatchBody } else { BlockKind::Other }
}
