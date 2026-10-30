//! Source text to tokens. Indentation becomes `Indent` and `Dedent` tokens, as in Python,
//! and line breaks inside brackets are ignored.

use crate::diagnostic::Diagnostic;
use crate::span::Span;
use crate::token::{FPart, Kw, Punct, Tok, Token};

/// Lexes `source`. `base` is added to every span, for code lexed out of an f-string.
pub(crate) fn lex(source: &str, base: u32) -> Result<Vec<Token>, Diagnostic> {
    lex_with_comments(source, base).map(|(tokens, _)| tokens)
}

/// Lexes `source`, also returning where each comment is, for the formatter.
pub(crate) fn lex_with_comments(source: &str, base: u32) -> Result<(Vec<Token>, Vec<Span>), Diagnostic> {
    let lexer = Lexer {
        source,
        chars: source.char_indices().collect(),
        pos: 0,
        tokens: Vec::new(),
        indents: vec![0],
        depth: 0,
        base,
        at_line_start: true,
        comments: Vec::new(),
    };
    lexer.run()
}

struct Lexer<'a> {
    source: &'a str,
    chars: Vec<(usize, char)>,
    pos: usize,
    tokens: Vec<Token>,
    indents: Vec<usize>,
    /// How many brackets are open; line breaks inside brackets don't end a statement.
    depth: usize,
    base: u32,
    at_line_start: bool,
    comments: Vec<Span>,
}

impl Lexer<'_> {
    fn run(mut self) -> Result<(Vec<Token>, Vec<Span>), Diagnostic> {
        loop {
            if self.at_line_start && self.depth == 0 && !self.indentation()? {
                break;
            }
            let Some(c) = self.peek(0) else { break };
            let start = self.offset();
            match c {
                ' ' | '\r' => self.pos += 1,
                '\t' => return Err(self.tab_error(start)),
                '\n' => {
                    self.pos += 1;
                    if self.depth == 0 {
                        self.newline(start);
                        self.at_line_start = true;
                    }
                }
                '#' => self.skip_comment(),
                '"' => self.string(start)?,
                'f' if self.peek(1) == Some('"') => self.fstring(start)?,
                '0'..='9' => self.number(start)?,
                c if c.is_ascii_alphabetic() || c == '_' => self.word(start),
                _ => self.punct(start)?,
            }
        }
        Ok(self.finish())
    }

    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.pos + ahead).map(|&(_, c)| c)
    }

    fn offset(&self) -> usize {
        self.chars.get(self.pos).map_or(self.source.len(), |&(at, _)| at)
    }

    fn span(&self, start: usize) -> Span {
        Span::new(start, self.offset()).shifted(self.base)
    }

    fn push(&mut self, tok: Tok, start: usize) {
        let span = self.span(start);
        self.tokens.push(Token { tok, span });
    }

    fn newline(&mut self, start: usize) {
        if self.tokens.last().is_some_and(|last| last.tok != Tok::Newline) {
            self.push(Tok::Newline, start);
        }
    }

    fn tab_error(&self, start: usize) -> Diagnostic {
        Diagnostic::new("tabs aren't allowed", Span::new(start, start + 1).shifted(self.base))
            .with_help("indent with 4 spaces")
    }

    fn skip_comment(&mut self) {
        let start = self.offset();
        while self.peek(0).is_some_and(|c| c != '\n') {
            self.pos += 1;
        }
        let span = self.span(start);
        self.comments.push(span);
    }

    /// Measures the indentation at the start of a line and emits `Indent` or `Dedent` tokens.
    /// Blank and comment-only lines are skipped. Returns false at the end of the file.
    fn indentation(&mut self) -> Result<bool, Diagnostic> {
        loop {
            let line_start = self.offset();
            let mut width = 0;
            while let Some(c) = self.peek(0) {
                match c {
                    ' ' => width += 1,
                    '\t' => return Err(self.tab_error(self.offset())),
                    _ => break,
                }
                self.pos += 1;
            }
            match self.peek(0) {
                None => return Ok(false),
                Some('\n' | '\r') => self.pos += 1,
                Some('#') => self.skip_comment(),
                Some(_) => {
                    self.at_line_start = false;
                    self.set_indent(width, line_start)?;
                    return Ok(true);
                }
            }
        }
    }

    fn set_indent(&mut self, width: usize, line_start: usize) -> Result<(), Diagnostic> {
        let top = self.indents.last().copied().unwrap_or(0);
        if width > top {
            self.indents.push(width);
            self.push(Tok::Indent, line_start);
            return Ok(());
        }
        while self.indents.last().is_some_and(|&indent| width < indent) {
            self.indents.pop();
            self.push(Tok::Dedent, line_start);
        }
        if self.indents.last().copied() == Some(width) {
            Ok(())
        } else {
            Err(Diagnostic::new(
                "this line's indentation doesn't match any block above it",
                self.span(line_start),
            )
            .with_help("line it up with the block it belongs to"))
        }
    }

    fn finish(mut self) -> (Vec<Token>, Vec<Span>) {
        let end = self.source.len();
        self.newline(end);
        while self.indents.len() > 1 {
            self.indents.pop();
            self.push(Tok::Dedent, end);
        }
        self.push(Tok::Eof, end);
        (self.tokens, self.comments)
    }

    fn word(&mut self, start: usize) {
        while self.peek(0).is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            self.pos += 1;
        }
        let word = self.source.get(start..self.offset()).unwrap_or("");
        let tok = Kw::from_word(word).map_or_else(|| Tok::Name(word.to_owned()), Tok::Kw);
        self.push(tok, start);
    }

    fn number(&mut self, start: usize) -> Result<(), Diagnostic> {
        self.digits();
        let mut is_float = false;
        if self.peek(0) == Some('.') && self.peek(1).is_some_and(|c| c.is_ascii_digit()) {
            is_float = true;
            self.pos += 1;
            self.digits();
        }
        if matches!(self.peek(0), Some('e' | 'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(0), Some('+' | '-')) {
                self.pos += 1;
            }
            self.digits();
        }
        let text: String =
            self.source.get(start..self.offset()).unwrap_or("").chars().filter(|&c| c != '_').collect();
        let tok = if is_float {
            text.parse()
                .map(Tok::Float)
                .map_err(|_| Diagnostic::new("this isn't a valid number", self.span(start)))?
        } else {
            text.parse().map(Tok::Int).map_err(|_| {
                Diagnostic::new("this number is too big for an int", self.span(start))
                    .with_help("ints are 64-bit: the largest is 9_223_372_036_854_775_807")
            })?
        };
        self.push(tok, start);
        Ok(())
    }

    fn digits(&mut self) {
        while self.peek(0).is_some_and(|c| c.is_ascii_digit() || c == '_') {
            self.pos += 1;
        }
    }

    fn punct(&mut self, start: usize) -> Result<(), Diagnostic> {
        let rest = self.source.get(start..).unwrap_or("");
        if let Some(&(text, punct)) = Punct::TWO_CHAR.iter().find(|(text, _)| rest.starts_with(text)) {
            self.pos += text.len();
            self.push(Tok::P(punct), start);
            return Ok(());
        }
        let c = self.peek(0).unwrap_or(' ');
        let Some(punct) = Punct::single(c) else {
            return Err(Diagnostic::new(
                format!("unexpected character `{c}`"),
                self.span(start).to(Span::new(start, start + c.len_utf8()).shifted(self.base)),
            ));
        };
        match punct {
            Punct::LParen | Punct::LBracket | Punct::LBrace => self.depth += 1,
            Punct::RParen | Punct::RBracket | Punct::RBrace => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        self.pos += 1;
        self.push(Tok::P(punct), start);
        Ok(())
    }

    fn string(&mut self, start: usize) -> Result<(), Diagnostic> {
        self.pos += 1;
        let mut text = String::new();
        loop {
            match self.peek(0) {
                None | Some('\n') => return Err(self.unterminated(start)),
                Some('"') => break,
                Some('\\') => text.push(self.escape(start)?),
                Some(c) => {
                    text.push(c);
                    self.pos += 1;
                }
            }
        }
        self.pos += 1;
        self.push(Tok::Str(text), start);
        Ok(())
    }

    fn unterminated(&self, start: usize) -> Diagnostic {
        Diagnostic::new("this string never ends", Span::new(start, start + 1).shifted(self.base))
            .with_help("close it with `\"` on the same line")
    }

    /// Reads a backslash escape and returns the character it stands for.
    fn escape(&mut self, start: usize) -> Result<char, Diagnostic> {
        let at = self.offset();
        self.pos += 1;
        let c = match self.peek(0) {
            Some('n') => '\n',
            Some('t') => '\t',
            Some('r') => '\r',
            Some('0') => '\0',
            Some(c @ ('\\' | '"' | '{' | '}')) => c,
            None | Some('\n') => return Err(self.unterminated(start)),
            Some(other) => {
                return Err(Diagnostic::new(
                    format!("`\\{other}` isn't an escape Fennec knows"),
                    self.span(at),
                )
                .with_help("the escapes are \\n, \\t, \\r, \\0, \\\\ and \\\""));
            }
        };
        self.pos += 1;
        Ok(c)
    }

    fn fstring(&mut self, start: usize) -> Result<(), Diagnostic> {
        self.pos += 2;
        let mut parts = Vec::new();
        let mut text = String::new();
        loop {
            match self.peek(0) {
                None | Some('\n') => return Err(self.unterminated(start)),
                Some('"') => break,
                Some('\\') => text.push(self.escape(start)?),
                Some('{') if self.peek(1) == Some('{') => {
                    text.push('{');
                    self.pos += 2;
                }
                Some('}') if self.peek(1) == Some('}') => {
                    text.push('}');
                    self.pos += 2;
                }
                Some('{') => {
                    if !text.is_empty() {
                        parts.push(FPart::Text(std::mem::take(&mut text)));
                    }
                    parts.push(self.fstring_expr(start)?);
                }
                Some('}') => {
                    return Err(Diagnostic::new("`}` without a matching `{`", self.span(self.offset()))
                        .with_help("write `}}` for a literal brace"));
                }
                Some(c) => {
                    text.push(c);
                    self.pos += 1;
                }
            }
        }
        self.pos += 1;
        if !text.is_empty() {
            parts.push(FPart::Text(text));
        }
        self.push(Tok::FStr(parts), start);
        Ok(())
    }

    /// Reads `{expression}` or `{expression:spec}` inside an f-string.
    fn fstring_expr(&mut self, start: usize) -> Result<FPart, Diagnostic> {
        self.pos += 1;
        let expr_start = self.offset();
        let mut nesting = 0_usize;
        let mut spec_start = None;
        loop {
            match self.peek(0) {
                None | Some('\n' | '"') => return Err(self.unterminated(start)),
                Some('(' | '[' | '{') => nesting += 1,
                Some(')' | ']') => nesting = nesting.saturating_sub(1),
                Some('}') if nesting == 0 => break,
                Some('}') => nesting -= 1,
                Some(':') if nesting == 0 && spec_start.is_none() => spec_start = Some(self.offset()),
                Some(_) => {}
            }
            self.pos += 1;
        }
        let end = self.offset();
        self.pos += 1;
        let expr_end = spec_start.unwrap_or(end);
        let source = self.source.get(expr_start..expr_end).unwrap_or("").to_owned();
        let spec = spec_start.and_then(|at| self.source.get(at + 1..end)).map(str::to_owned);
        let offset = Span::new(expr_start, expr_start).shifted(self.base).start;
        Ok(FPart::Expr { source, offset, spec })
    }
}

#[cfg(test)]
mod tests {
    use super::lex;
    use crate::token::Tok;

    #[test]
    fn blocks_still_open_at_the_end_are_closed() {
        let tokens: Vec<Tok> =
            lex("if x:\n    if y:\n        z\n", 0).unwrap().into_iter().map(|t| t.tok).collect();
        assert!(tokens.ends_with(&[Tok::Dedent, Tok::Dedent, Tok::Eof]), "{tokens:?}");
    }
}
