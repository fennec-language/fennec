//! One-line bodies. A guard clause, an `if` with no `elif` or `else` whose body is one
//! `return`, `fail`, `break` or `continue`, sits on one line when it fits: `if hp <= 0: return`.
//! Every other body goes on its own lines, whichever way it was written.

use super::Driver;
use crate::spacing::{BlockKind, Tok};
use fennec_syntax::TokenKind;

const JUMPS: [&str; 4] = ["return", "fail", "break", "continue"];

/// Where the `:` is in a block statement written with its body on the same line, like
/// `case .Point: 0.0`. None for a body that opens a block of its own, which stays put.
pub(super) fn inline_body(toks: &[Tok<'_>], kind: BlockKind) -> Option<usize> {
    let mut words = toks.iter().skip_while(|tok| matches!(tok.text, "export" | "noalloc"));
    let first = words.next()?;
    let opens_block =
        matches!(first.text, "def" | "if" | "elif" | "else" | "while" | "for" | "with" | "test")
            || first.text == "case" && kind == BlockKind::MatchBody;
    if !opens_block || toks.last().is_some_and(|tok| tok.text == ":") {
        return None;
    }
    // A test's header can have `:` in it, `test "t" for n: int:`, so its body starts after the
    // last one. No other header has one outside brackets, and no one-line body does.
    let mut colons = top_level_colons(toks);
    if first.text == "test" { colons.last() } else { colons.next() }
}

fn top_level_colons<'t>(toks: &'t [Tok<'_>]) -> impl DoubleEndedIterator<Item = usize> + 't {
    let mut depth = 0_usize;
    let depths: Vec<usize> = toks
        .iter()
        .map(|tok| {
            match tok.text {
                "(" | "[" | "{" if tok.kind == TokenKind::Punct => depth += 1,
                ")" | "]" | "}" if tok.kind == TokenKind::Punct => depth = depth.saturating_sub(1),
                _ => {}
            }
            depth
        })
        .collect();
    toks.iter()
        .zip(depths)
        .enumerate()
        .filter(|(_, (tok, depth))| *depth == 0 && tok.kind == TokenKind::Punct && tok.text == ":")
        .map(|(at, _)| at)
}

/// Whether `if` and the statement after its `:` make a guard clause, ignoring `else`.
pub(super) fn guards(header: &[Tok<'_>], body: &[Tok<'_>]) -> bool {
    header.first().is_some_and(|tok| tok.kind == TokenKind::Keyword && tok.text == "if")
        && body.first().is_some_and(|tok| tok.kind == TokenKind::Keyword && JUMPS.contains(&tok.text))
}

impl Driver<'_, '_> {
    /// Whether the `if` statement whose last line ends just before `next` goes on with an
    /// `elif` or `else`.
    pub(super) fn has_else(&self, next: usize) -> bool {
        self.all
            .get(next)
            .is_some_and(|tok| tok.kind == TokenKind::Keyword && matches!(tok.text, "elif" | "else"))
    }

    /// A guard clause written as a block, `if cond:` with one jump indented under it, that
    /// could be joined onto one line. Returns where its body's tokens start and end.
    pub(super) fn guard_block(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        let header_ends_block =
            end.checked_sub(1).and_then(|last| self.all.get(last)).is_some_and(|tok| tok.text == ":");
        let indented = self.all.get(end + 1).is_some_and(|tok| tok.kind == TokenKind::Indent);
        if !header_ends_block || !indented || self.comments.trailing(end - 1).is_some() {
            return None;
        }
        let body_start = end + 2;
        let body_end = (body_start..self.all.len())
            .find(|&at| self.all.get(at).is_some_and(|tok| tok.kind == TokenKind::Newline))?;
        let header = self.all.get(start..end)?;
        let body = self.all.get(body_start..body_end)?;
        let one_statement = self.all.get(body_end + 1).is_some_and(|tok| tok.kind == TokenKind::Dedent);
        let joinable = guards(header, body)
            && one_statement
            && body.last().is_none_or(|tok| tok.text != ":")
            && self.comments.leading(body_start).is_empty()
            && !self.has_else(body_end + 2);
        joinable.then_some((body_start, body_end))
    }
}
