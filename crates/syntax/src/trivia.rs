//! Tokens with their comments, for tools that rewrite source text, like the formatter.
//! The parser never sees comments; this is the one place they're kept.

use crate::diagnostic::Diagnostic;
use crate::lexer::lex_with_comments;
use crate::span::Span;
use crate::token::Tok;

/// A file's tokens and the spans of its comments, both in source order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lexed {
    pub tokens: Vec<SourceToken>,
    /// Each comment runs from its `#` to the end of its line.
    pub comments: Vec<Span>,
}

/// A token's kind and where it is. Its text is the source in its span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceToken {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Name,
    Keyword,
    Punct,
    Number,
    Str,
    FStr,
    Newline,
    Indent,
    Dedent,
    Eof,
}

/// Lexes a whole file, keeping its comments.
pub fn lex_source(source: &str) -> Result<Lexed, Diagnostic> {
    let (tokens, comments) = lex_with_comments(source, 0)?;
    let tokens =
        tokens.into_iter().map(|token| SourceToken { kind: kind(&token.tok), span: token.span }).collect();
    Ok(Lexed { tokens, comments })
}

fn kind(tok: &Tok) -> TokenKind {
    match tok {
        Tok::Name(_) => TokenKind::Name,
        Tok::Int(_) | Tok::Float(_) => TokenKind::Number,
        Tok::Str(_) => TokenKind::Str,
        Tok::FStr(_) => TokenKind::FStr,
        Tok::Kw(_) => TokenKind::Keyword,
        Tok::P(_) => TokenKind::Punct,
        Tok::Newline => TokenKind::Newline,
        Tok::Indent => TokenKind::Indent,
        Tok::Dedent => TokenKind::Dedent,
        Tok::Eof => TokenKind::Eof,
    }
}
