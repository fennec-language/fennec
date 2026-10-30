//! Imports are sorted: `std` first, then the rest, each alphabetically, and so are the names
//! in `from x import a, b`. Order can't change what a program means, since importing a module
//! runs no code. A comment on its own line above an import moves with it, except above the
//! first import, where it stays put: it's usually about the whole file.

use super::{Driver, write};
use crate::spacing::{BlockKind, Tok, analyze};
use crate::tree::Line;
use fennec_syntax::TokenKind;

/// One import line, taken apart for sorting.
struct Import<'s> {
    /// `std` imports come first.
    group: u8,
    module: String,
    toks: Vec<Tok<'s>>,
    /// Its own-line comments, unless it's first, and the comment at its end.
    comments: Vec<&'s str>,
    trailing: Option<&'s str>,
}

impl Import<'_> {
    fn key(&self) -> (u8, &str, Vec<&str>) {
        (self.group, &self.module, self.toks.iter().map(|tok| tok.text).collect())
    }
}

impl<'s> Driver<'s, '_> {
    /// Whether the line starting at token `at` is an import at the top level.
    pub(super) fn is_import(&self, at: usize) -> bool {
        self.blocks.len() == 1
            && self
                .all
                .get(at)
                .is_some_and(|tok| tok.kind == TokenKind::Keyword && matches!(tok.text, "from" | "import"))
    }

    /// Writes the run of import lines starting at token `start`, sorted, and marks their
    /// tokens as written.
    pub(super) fn imports(&mut self, start: usize) {
        let mut lines = Vec::new();
        let mut at = start;
        while let Some(end) = (at..self.all.len())
            .find(|&end| self.all.get(end).is_some_and(|tok| tok.kind == TokenKind::Newline))
        {
            lines.push((at, end));
            if !self.is_import(end + 1) {
                break;
            }
            at = end + 1;
        }
        let first = self.tokens.get(start).map_or(0, |token| token.span.start);
        let entries = self.entries(start, first, 0);
        self.write_entries(&entries, 0, false);

        let mut imports: Vec<Import<'s>> = lines
            .iter()
            .map(|&(line_start, end)| self.import(line_start, end, line_start == start))
            .collect();
        imports.sort_by(|a, b| a.key().cmp(&b.key()));
        let mut group = imports.first().map_or(0, |import| import.group);
        for import in &imports {
            if import.group != group {
                self.writer.blank();
                group = import.group;
            }
            for comment in &import.comments {
                self.writer.own_line_comment(comment, 0);
            }
            let spacing = analyze(&import.toks, BlockKind::TopLevel);
            let none: Vec<&[&str]> = vec![&[]; import.toks.len()];
            let trailing = vec![None; import.toks.len()];
            let line = Line::new(&import.toks, &none, &trailing, &spacing);
            write(&mut self.writer, &line.build(), 0, import.trailing);
        }

        let last = lines.last().map_or(start, |&(_, end)| end);
        self.skipped.extend(start..=last);
        self.prev_end = last.checked_sub(1).and_then(|at| self.tokens.get(at)).map(|token| token.span.end);
        if let Some(block) = self.blocks.last_mut() {
            block.started = true;
            block.last_was_item = false;
        }
        self.pending = BlockKind::Other;
        self.last_widths = vec![0];
    }

    /// The import on tokens `start..end`, with the names after `import` sorted.
    fn import(&self, start: usize, end: usize, is_first: bool) -> Import<'s> {
        let toks = self.toks(start, end);
        let is_from = toks.first().is_some_and(|tok| tok.text == "from");
        // The names start after `import`, which is after `from` and the module.
        let names_at = toks.iter().position(|tok| tok.kind == TokenKind::Keyword && tok.text == "import");
        let split = if is_from { names_at.map_or(toks.len(), |at| at + 1) } else { toks.len() };
        let (head, names) = toks.split_at(split);
        let module: String =
            head.iter().skip(1).take_while(|tok| tok.text != "import").map(|tok| tok.text).collect();
        let mut sorted: Vec<&[Tok<'s>]> = names.split(|tok| tok.text == ",").collect();
        sorted.sort_by_key(|name| name.iter().map(|tok| tok.text).collect::<Vec<_>>());
        let comma = Tok { kind: TokenKind::Punct, text: "," };
        let mut rebuilt = head.to_vec();
        for (index, name) in sorted.iter().enumerate() {
            if index > 0 {
                rebuilt.push(comma);
            }
            rebuilt.extend_from_slice(name);
        }
        let group = u8::from(!(module == "std" || module.starts_with("std.")));
        let comments = if is_first { Vec::new() } else { self.comments.leading(start).to_vec() };
        let trailing = end.checked_sub(1).and_then(|last| self.comments.trailing(last));
        Import { group, module, toks: rebuilt, comments, trailing }
    }
}
