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
    if let Some(problem) = hidden_direction(source, base) {
        return Err(problem);
    }
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

/// The characters that change the direction text is shown in without showing themselves: the
/// embeddings, overrides and isolates, and the marks. One in a comment or a string can make code
/// read differently from what it does, as `if access != "user\u202e \u2066// admin"` does.
pub(crate) const HIDDEN_DIRECTION: [char; 12] = [
    '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}',
    '\u{2069}', '\u{200e}', '\u{200f}', '\u{061c}',
];

/// The first character of `source` that changes the direction text is shown in, refused wherever
/// it is, comments and strings included. A string that needs one writes its escape.
fn hidden_direction(source: &str, base: u32) -> Option<Diagnostic> {
    let (at, c) = source.char_indices().find(|(_, c)| HIDDEN_DIRECTION.contains(c))?;
    let code = u32::from(c);
    Some(
        Diagnostic::new(
            format!("this line has a hidden character, U+{code:04X}, that changes the direction text is shown in"),
            Span::new(at, at + c.len_utf8()).shifted(base),
        )
        .with_help(format!(
            "it can make code read differently from what it does: take it out, or write `\\u{code:04x}` in a string that needs it"
        )),
    )
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
                'f' if self.peek(1) == Some('"') => self.fstring(start, false)?,
                't' if self.peek(1) == Some('"') => self.fstring(start, true)?,
                'r' if self.peek(1) == Some('"') => self.raw_string(start)?,
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
        if self.peek(0).is_some_and(|c| c.is_ascii_alphabetic()) {
            // A duration, in one part or several from the largest unit down: `1h30min`.
            let mut parts = vec![(text, self.unit())];
            while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                let part_start = self.offset();
                self.digits();
                if self.peek(0) == Some('.') && self.peek(1).is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                    self.digits();
                }
                let number = self.source.get(part_start..self.offset()).unwrap_or("").replace('_', "");
                parts.push((number, self.unit()));
            }
            let written = self.source.get(start..self.offset()).unwrap_or("");
            let nanos = duration(written, &parts)
                .map_err(|(message, help)| Diagnostic::new(message, self.span(start)).with_help(help))?;
            self.push(Tok::Duration(nanos), start);
            return Ok(());
        }
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

    /// The letters of a duration's unit, like `ms`.
    fn unit(&mut self) -> String {
        let start = self.offset();
        while self.peek(0).is_some_and(|c| c.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        self.source.get(start..self.offset()).unwrap_or("").to_owned()
    }

    fn digits(&mut self) {
        while self.peek(0).is_some_and(|c| c.is_ascii_digit() || c == '_') {
            self.pos += 1;
        }
    }

    fn punct(&mut self, start: usize) -> Result<(), Diagnostic> {
        let rest = self.source.get(start..).unwrap_or("");
        let longer = Punct::THREE_CHAR.iter().chain(&Punct::TWO_CHAR);
        if let Some(&(text, punct)) = longer.into_iter().find(|(text, _)| rest.starts_with(text)) {
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

    /// `r"..."`: backslashes are kept as written, for patterns like `r"\d+"`.
    fn raw_string(&mut self, start: usize) -> Result<(), Diagnostic> {
        self.pos += 2;
        let mut text = String::new();
        loop {
            match self.peek(0) {
                None | Some('\n') => return Err(self.unterminated(start)),
                Some('"') => break,
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
            Some('x') => return self.code_escape(2, at),
            Some('u') => return self.code_escape(4, at),
            Some('U') => return self.code_escape(8, at),
            None | Some('\n') => return Err(self.unterminated(start)),
            Some(other) => {
                return Err(Diagnostic::new(
                    format!("`\\{other}` isn't an escape Fennec knows"),
                    self.span(at),
                )
                .with_help(
                    "the escapes are \\n, \\t, \\r, \\0, \\\\, \\\", \\xhh, \\uhhhh and \\Uhhhhhhhh; to keep backslashes as written, use a raw string like r\"\\d+\"",
                ));
            }
        };
        self.pos += 1;
        Ok(c)
    }

    /// `\x41`, `\u00e9` or `\U0001f98a`: a character written as its code in hex, with
    /// exactly `digits` digits. `at` is where the backslash is.
    fn code_escape(&mut self, digits: u32, at: usize) -> Result<char, Diagnostic> {
        let letter = self.peek(0).unwrap_or('x');
        self.pos += 1;
        let mut code: u32 = 0;
        for _ in 0..digits {
            let Some(digit) = self.peek(0).and_then(|c| c.to_digit(16)) else {
                let example = match letter {
                    'x' => "\\x41",
                    'u' => "\\u00e9",
                    _ => "\\U0001f98a",
                };
                return Err(Diagnostic::new(
                    format!("`\\{letter}` needs {digits} hex digits, like `{example}`"),
                    self.span(at),
                ));
            };
            code = code * 16 + digit;
            self.pos += 1;
        }
        char::from_u32(code).ok_or_else(|| {
            let written = self.source.get(at..self.offset()).unwrap_or_default();
            Diagnostic::new(format!("`{written}` isn't the code of a character"), self.span(at))
        })
    }

    /// An f-string, or a template string when `template`: the same text and `{}`s, built as a
    /// `Template` instead of joined.
    fn fstring(&mut self, start: usize, template: bool) -> Result<(), Diagnostic> {
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
                    let mut part = self.fstring_expr(start)?;
                    // `{hp=}` shows what's written and then its value: `hp=100`.
                    if let FPart::Expr { source, .. } = &mut part
                        && let Some(shown) = shown_as_written(source)
                    {
                        text.push_str(source);
                        *source = shown;
                    }
                    if !text.is_empty() {
                        parts.push(FPart::Text(std::mem::take(&mut text)));
                    }
                    parts.push(part);
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
        self.push(if template { Tok::TStr(parts) } else { Tok::FStr(parts) }, start);
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

/// What an f-string's `{expression=}` shows: written out with its `=`, as in Python, the
/// expression before the `=`, or `None` for a plain `{expression}`. A `=` that ends `==`, `!=`,
/// `<=` or `>=` is the comparison's own.
fn shown_as_written(source: &str) -> Option<String> {
    let before = source.trim_end().strip_suffix('=')?;
    if before.ends_with(['=', '!', '<', '>']) || before.trim().is_empty() {
        return None;
    }
    Some(before.to_owned())
}

/// The units a duration literal can end in, with how many nanoseconds each is. Minutes are
/// `min`, so `m` is never a unit.
const UNITS: [(&str, i128); 6] = [
    ("ns", 1),
    ("us", 1_000),
    ("ms", 1_000_000),
    ("s", 1_000_000_000),
    ("min", 60_000_000_000),
    ("h", 3_600_000_000_000),
];

/// How a duration is written, for help lines.
const WRITTEN: &str =
    "a duration ends in `ns`, `us`, `ms`, `s`, `min` or `h`, like `30s`, `1.5s`, `200ms` or `1h30min`";

/// The nanoseconds a duration literal stands for, from its parts: each a number and a unit,
/// the largest unit first. It's worked out from the digits as written, so `0.1s` is exactly
/// 100 million nanoseconds. A problem comes back as a message and a help line.
fn duration(written: &str, parts: &[(String, String)]) -> Result<i64, (String, String)> {
    let too_long = || {
        (
            format!("`{written}` is too long for a duration"),
            "a duration holds up to about 292 years of nanoseconds".to_owned(),
        )
    };
    let mut total: i128 = 0;
    let mut previous = i128::MAX;
    for (number, unit) in parts {
        if unit.is_empty() {
            return Err((format!("`{written}` ends without a unit"), WRITTEN.to_owned()));
        }
        let Some(&(_, per)) = UNITS.iter().find(|(name, _)| name == unit) else {
            let help = if unit == "m" { "minutes are `min`, like `5min`" } else { WRITTEN };
            return Err((format!("`{written}` uses `{unit}`, which isn't a unit of time"), help.to_owned()));
        };
        if per >= previous {
            return Err((
                format!("`{written}` has its units out of order"),
                "write each unit once, the largest first, like `1h30min`".to_owned(),
            ));
        }
        previous = per;
        let nanos = part_nanos(number, per).map_err(|whole| {
            if whole {
                (
                    format!("`{written}` isn't a whole number of nanoseconds"),
                    "a duration counts whole nanoseconds: write fewer decimal places".to_owned(),
                )
            } else {
                too_long()
            }
        })?;
        total = total.checked_add(nanos).ok_or_else(too_long)?;
    }
    i64::try_from(total).map_err(|_| too_long())
}

/// The nanoseconds in `number` of a unit `per` nanoseconds long. It fails with `true` when
/// that isn't a whole number of nanoseconds, and `false` when it's too big.
fn part_nanos(number: &str, per: i128) -> Result<i128, bool> {
    let (mantissa, exponent) = match number.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().map_err(|_| false)?),
        None => (number, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits: i128 = format!("{whole}{fraction}").parse().map_err(|_| false)?;
    let shift = exponent - i32::try_from(fraction.len()).map_err(|_| false)?;
    let nanos = digits.checked_mul(per).ok_or(false)?;
    let ten = 10_i128.checked_pow(shift.unsigned_abs()).ok_or(false)?;
    if shift >= 0 {
        nanos.checked_mul(ten).ok_or(false)
    } else if nanos % ten == 0 {
        Ok(nanos / ten)
    } else {
        Err(true)
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

    /// `{hp=}` shows what's written, with its `=` and its spaces, and then the value, as in
    /// Python. A `=` ending a comparison is the comparison's.
    #[test]
    fn an_fstring_shows_what_ends_in_equals_as_written() {
        use crate::token::FPart;
        let parts = |text: &str| -> Vec<FPart> {
            let tokens = lex(text, 0).unwrap();
            let Some(Tok::FStr(parts)) = tokens.into_iter().map(|token| token.tok).next() else {
                panic!("not an f-string: {text}")
            };
            parts
        };
        let code = |source: &str, offset: u32, spec: Option<&str>| FPart::Expr {
            source: source.to_owned(),
            offset,
            spec: spec.map(str::to_owned),
        };
        assert_eq!(parts("f\"{hp=}\""), [FPart::Text("hp=".to_owned()), code("hp", 3, None)]);
        assert_eq!(parts("f\"at {hp=:,}\""), [FPart::Text("at hp=".to_owned()), code("hp", 6, Some(","))]);
        assert_eq!(parts("f\"{ hp = }\""), [FPart::Text(" hp = ".to_owned()), code(" hp ", 3, None)]);
        assert_eq!(parts("f\"{hp == 1}\""), [code("hp == 1", 3, None)]);
        assert_eq!(parts("f\"{hp >= 1}\""), [code("hp >= 1", 3, None)]);
    }
}
