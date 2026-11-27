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

/// Which string a `"""` one is: `"""`, `r"""`, `f"""` or `t"""`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quoted {
    Plain,
    Raw,
    Formatted,
    Template,
}

struct Lexer<'a> {
    source: &'a str,
    /// The byte where the lexer is, always at a character's start or the end. It's read by
    /// byte, with no copy of the source by character, which cost 16 bytes for each of its bytes.
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
                ' ' | '\r' => self.bump(),
                '\t' => return Err(self.tab_error(start)),
                '\n' => {
                    self.bump();
                    if self.depth == 0 {
                        self.newline(start);
                        self.at_line_start = true;
                    }
                }
                '#' => self.skip_comment(),
                '"' if self.triple_at(0) => self.triple(start, 0, Quoted::Plain)?,
                'r' if self.triple_at(1) => self.triple(start, 1, Quoted::Raw)?,
                'f' if self.triple_at(1) => self.triple(start, 1, Quoted::Formatted)?,
                't' if self.triple_at(1) => self.triple(start, 1, Quoted::Template)?,
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

    /// The character `ahead` characters on from where the lexer is.
    fn peek(&self, ahead: usize) -> Option<char> {
        self.source.get(self.pos..)?.chars().nth(ahead)
    }

    fn offset(&self) -> usize {
        self.pos.min(self.source.len())
    }

    /// Steps past the character where the lexer is.
    fn bump(&mut self) {
        self.pos += self.peek(0).map_or(1, char::len_utf8);
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
            self.bump();
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
                self.bump();
            }
            match self.peek(0) {
                None => return Ok(false),
                Some('\n' | '\r') => self.bump(),
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
            self.bump();
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
            self.bump();
            self.digits();
        }
        if matches!(self.peek(0), Some('e' | 'E')) {
            is_float = true;
            self.bump();
            if matches!(self.peek(0), Some('+' | '-')) {
                self.bump();
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
                    self.bump();
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
            self.bump();
        }
        self.source.get(start..self.offset()).unwrap_or("").to_owned()
    }

    fn digits(&mut self) {
        while self.peek(0).is_some_and(|c| c.is_ascii_digit() || c == '_') {
            self.bump();
        }
    }

    fn punct(&mut self, start: usize) -> Result<(), Diagnostic> {
        let rest = self.source.get(start..).unwrap_or("");
        if rest.starts_with(":=") {
            return Err(self.walrus(start));
        }
        let longer = Punct::THREE_CHAR.iter().chain(&Punct::TWO_CHAR);
        if let Some(&(text, punct)) = longer.into_iter().find(|(text, _)| rest.starts_with(text)) {
            self.pos += text.len();
            self.push(Tok::P(punct), start);
            return Ok(());
        }
        let c = self.peek(0).unwrap_or(' ');
        let Some(punct) = Punct::single(c) else {
            return Err(self.unexpected(start, c));
        };
        match punct {
            Punct::LParen | Punct::LBracket | Punct::LBrace => self.depth += 1,
            Punct::RParen | Punct::RBracket | Punct::RBrace => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        self.bump();
        self.push(Tok::P(punct), start);
        Ok(())
    }

    /// A character no token starts with, at byte `start`. Where it's a habit from Python, what to
    /// write instead: `;` between statements, a string in `'`s, a `\` that goes on to the next
    /// line, `!r` in an f-string's `{}`, or `!` for `not`.
    fn unexpected(&self, start: usize, c: char) -> Diagnostic {
        let rest = self.source.get(start..).unwrap_or("");
        let after = rest.get(c.len_utf8()..).unwrap_or("");
        let one = Span::new(start, start + c.len_utf8()).shifted(self.base);
        match c {
            ';' => Diagnostic::new("Fennec doesn't use `;`", one).with_help("put each statement on its own line"),
            '\'' => {
                let line = after.split('\n').next().unwrap_or("");
                // The text up to the closing `'` on this line, when it can go in `"`s as it is.
                let quoted = line.find('\'').and_then(|close| line.get(..close).map(|text| (close, text)));
                match quoted {
                    Some((close, text)) if !text.contains(['"', '\\', '{', '}']) => Diagnostic::new(
                        "a string is written in double quotes",
                        Span::new(start, start + close + 2).shifted(self.base),
                    )
                    .with_help(format!("write `\"{text}\"`")),
                    _ => Diagnostic::new("a string is written in double quotes", one)
                        .with_help("write it in `\"`s, as `\"text\"`"),
                }
            }
            '\\' if after.trim_start_matches([' ', '\t', '\r']).chars().next().is_none_or(|next| next == '\n') => {
                Diagnostic::new("Fennec doesn't go on to the next line with `\\`", one).with_help(
                    "put what goes on in brackets, which can span lines, as `total = (1 +` with `2)` on the next line",
                )
            }
            '!' if matches!(rest.trim_end(), "!r" | "!s" | "!a") => Diagnostic::new(
                format!("an f-string has no `{}`", rest.trim_end()),
                Span::new(start, start + 2).shifted(self.base),
            )
            .with_help("`{name}` shows the value as `str(name)` does, and a format goes after `:`, as `{price:.2f}`"),
            '!' => Diagnostic::new("Fennec writes `not`, not `!`", one).with_help("write `not done`"),
            _ => Diagnostic::new(format!("unexpected character `{c}`"), one),
        }
    }

    /// `:=`, Python's way to name a value inside an expression, at byte `start`.
    fn walrus(&self, start: usize) -> Diagnostic {
        let help = match self.tokens.last().map(|token| &token.tok) {
            Some(Tok::Name(name)) => {
                format!("give `{name}` its value on a line of its own first, as `{name} = ...`, then use it")
            }
            _ => "give the value a name on a line of its own first, then use the name".to_owned(),
        };
        Diagnostic::new("Fennec doesn't have `:=`", Span::new(start, start + 2).shifted(self.base))
            .with_help(help)
    }

    fn string(&mut self, start: usize) -> Result<(), Diagnostic> {
        self.bump();
        let mut text = String::new();
        loop {
            match self.peek(0) {
                None | Some('\n') => return Err(self.unterminated(start)),
                Some('"') => break,
                Some('\\') => text.push(self.escape(start)?),
                Some(c) => {
                    text.push(c);
                    self.bump();
                }
            }
        }
        self.bump();
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
                    self.bump();
                }
            }
        }
        self.bump();
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
        self.bump();
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
        self.bump();
        Ok(c)
    }

    /// `\x41`, `\u00e9` or `\U0001f98a`: a character written as its code in hex, with
    /// exactly `digits` digits. `at` is where the backslash is.
    fn code_escape(&mut self, digits: u32, at: usize) -> Result<char, Diagnostic> {
        let letter = self.peek(0).unwrap_or('x');
        self.bump();
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
            self.bump();
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
                Some('{' | '}') => self.fstring_brace(start, &mut text, &mut parts)?,
                Some(c) => {
                    text.push(c);
                    self.bump();
                }
            }
        }
        self.bump();
        if !text.is_empty() {
            parts.push(FPart::Text(text));
        }
        self.push(if template { Tok::TStr(parts) } else { Tok::FStr(parts) }, start);
        Ok(())
    }

    /// A brace in an f-string or a template string, where the lexer is: `{{` or `}}` for one
    /// brace, or `{expression}`, which ends the text before it.
    fn fstring_brace(
        &mut self,
        start: usize,
        text: &mut String,
        parts: &mut Vec<FPart>,
    ) -> Result<(), Diagnostic> {
        match (self.peek(0), self.peek(1)) {
            (Some(brace @ ('{' | '}')), Some(next)) if next == brace => {
                text.push(brace);
                self.pos += 2;
            }
            (Some('{'), _) => {
                let mut part = self.fstring_expr(start)?;
                // `{hp=}` shows what's written and then its value: `hp=100`.
                if let FPart::Expr { source, .. } = &mut part
                    && let Some(shown) = shown_as_written(source)
                {
                    text.push_str(source);
                    *source = shown;
                }
                if !text.is_empty() {
                    parts.push(FPart::Text(std::mem::take(text)));
                }
                parts.push(part);
            }
            _ => {
                return Err(Diagnostic::new("`}` without a matching `{`", self.span(self.offset()))
                    .with_help("write `}}` for a literal brace"));
            }
        }
        Ok(())
    }

    /// Whether `"""` starts `ahead` characters on.
    fn triple_at(&self, ahead: usize) -> bool {
        (ahead..ahead + 3).all(|at| self.peek(at) == Some('"'))
    }

    /// The character that starts at byte `at`, or none where none starts there.
    fn char_at(&self, at: usize) -> Option<char> {
        self.source.get(at..)?.chars().next()
    }

    fn offset_at(&self, at: usize) -> usize {
        at.min(self.source.len())
    }

    /// The byte after the character at byte `at`.
    fn after(&self, at: usize) -> usize {
        at + self.char_at(at).map_or(1, char::len_utf8)
    }

    /// A string between `"""`s, which may span lines, after `prefix` letters, as the `f` of
    /// `f"""`. A newline right after the opening `"""` isn't part of it. When the closing `"""` is
    /// on a line of its own, the spaces before it come off the front of every line after the
    /// opening one, and a line with fewer is refused; a line of nothing but spaces may have fewer,
    /// and is empty. The newline before that closing line is kept. Escapes and `{}`s are read as on
    /// one line.
    fn triple(&mut self, start: usize, prefix: usize, kind: Quoted) -> Result<(), Diagnostic> {
        let content = self.pos + prefix + 3;
        // Where the closing `"""` is: a backslash escapes what follows it, but not in a raw one.
        let mut end = content;
        loop {
            match self.char_at(end) {
                None => return Err(self.unclosed_triple(start, prefix)),
                Some('\\') if kind != Quoted::Raw => end = self.after(end + 1),
                Some('"') if self.char_at(end + 1) == Some('"') && self.char_at(end + 2) == Some('"') => {
                    break;
                }
                Some(_) => end = self.after(end),
            }
        }
        let first = if self.char_at(content) == Some('\n') { content + 1 } else { content };
        // The closing `"""` alone on its line: the spaces before it, and the text up to and with
        // the newline before them.
        let last_newline = (content..end).rev().find(|&at| self.char_at(at) == Some('\n'));
        let (indent, body_end) = match last_newline {
            Some(newline) if (newline + 1..end).all(|at| self.char_at(at) == Some(' ')) => {
                (end - newline - 1, newline + 1)
            }
            _ => (0, end),
        };
        self.pos = first;
        let mut text = String::new();
        let mut parts = Vec::new();
        // Text right after the opening `"""` has no spaces of its own to take off.
        let mut line_start = first != content;
        while self.pos < body_end {
            if line_start {
                line_start = false;
                let spaces = (self.pos..body_end).take_while(|&at| self.char_at(at) == Some(' ')).count();
                if self.char_at(self.pos + spaces) == Some('\n') {
                    self.pos += spaces.min(indent);
                } else if spaces < indent {
                    return Err(self.under_indented(self.pos + spaces, indent));
                } else {
                    self.pos += indent;
                }
                continue;
            }
            match self.peek(0) {
                Some('\n') => {
                    text.push('\n');
                    self.bump();
                    line_start = true;
                }
                Some('\\') if kind != Quoted::Raw && self.peek(1) == Some('\n') => {
                    return Err(Diagnostic::new(
                        "a `\\` can't end a line in a string",
                        self.span(self.offset()),
                    )
                    .with_help("write `\\n` for a newline, or take the `\\` out"));
                }
                Some('\\') if kind != Quoted::Raw => text.push(self.escape(start)?),
                Some('{' | '}') if matches!(kind, Quoted::Formatted | Quoted::Template) => {
                    if self.peek(0) == Some('{') && self.peek(1) != Some('{') {
                        self.brace_on_its_line()?;
                    }
                    self.fstring_brace(start, &mut text, &mut parts)?;
                }
                Some(c) => {
                    text.push(c);
                    self.bump();
                }
                None => break,
            }
        }
        self.pos = end + 3;
        let tok = match kind {
            Quoted::Plain | Quoted::Raw => Tok::Str(text),
            Quoted::Formatted | Quoted::Template => {
                if !text.is_empty() {
                    parts.push(FPart::Text(text));
                }
                if kind == Quoted::Template { Tok::TStr(parts) } else { Tok::FStr(parts) }
            }
        };
        self.push(tok, start);
        Ok(())
    }

    /// A `{` in a `"""` string closes on its own line and holds no `"`, as in a string on one line.
    fn brace_on_its_line(&self) -> Result<(), Diagnostic> {
        let mut nesting = 0_usize;
        let mut at = self.pos + 1;
        loop {
            match self.char_at(at) {
                None | Some('\n') => {
                    return Err(Diagnostic::new(
                        "this `{` isn't closed on its line",
                        self.span(self.offset()),
                    )
                    .with_help("close it with `}` on the same line, or write `{{` for a brace"));
                }
                Some('"') => {
                    let quote = Span::new(self.offset_at(at), self.offset_at(at) + 1).shifted(self.base);
                    return Err(Diagnostic::new("a `\"` can't go inside `{}` in a string", quote).with_help(
                        "give the value a name before the string, and write the name in the `{}`",
                    ));
                }
                Some('(' | '[' | '{') => nesting += 1,
                Some(')' | ']') => nesting = nesting.saturating_sub(1),
                Some('}') if nesting == 0 => return Ok(()),
                Some('}') => nesting -= 1,
                Some(_) => {}
            }
            at = self.after(at);
        }
    }

    /// A `"""` with no `"""` after it, named by the line it's on, which may be far above where
    /// the file ends.
    fn unclosed_triple(&self, start: usize, prefix: usize) -> Diagnostic {
        let line = self.source.get(..start).map_or(0, |before| before.matches('\n').count()) + 1;
        Diagnostic::new(
            format!("the `\"\"\"` on line {line} is never closed"),
            Span::new(start, start + prefix + 3).shifted(self.base),
        )
        .with_help("close it with `\"\"\"`")
    }

    /// A line of a `"""` string with fewer spaces before it than the closing `"""` has, at its
    /// first character.
    fn under_indented(&self, at: usize, indent: usize) -> Diagnostic {
        let offset = self.offset_at(at);
        let width = self.char_at(at).map_or(1, char::len_utf8);
        Diagnostic::new(
            "this line is indented less than the closing `\"\"\"`",
            Span::new(offset, offset + width).shifted(self.base),
        )
        .with_help(format!(
            "indent it at least {indent} spaces, as far as the closing `\"\"\"` is, or move the closing `\"\"\"` left"
        ))
    }

    /// Reads `{expression}` or `{expression:spec}` inside an f-string.
    fn fstring_expr(&mut self, start: usize) -> Result<FPart, Diagnostic> {
        self.bump();
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
            self.bump();
        }
        let end = self.offset();
        self.bump();
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

    /// The first token of `source`, which is a string.
    fn first(source: &str) -> Tok {
        lex(source, 0).unwrap().into_iter().map(|token| token.tok).next().unwrap()
    }

    /// A `"""` string keeps its lines, and loses the newline after its opening `"""` and the
    /// spaces before a closing `"""` on its own line, from every line (#294).
    #[test]
    fn a_string_in_triple_quotes_keeps_its_lines_less_the_closing_indent() {
        let text = |source: &str| match first(source) {
            Tok::Str(text) => text,
            other => panic!("not a string: {other:?}"),
        };
        assert_eq!(text("\"\"\"\n    one\n      two\n\n    three\n    \"\"\""), "one\n  two\n\nthree\n");
        // A line of nothing but spaces may have fewer, and keeps those past the indent.
        assert_eq!(text("\"\"\"\n    one\n  \n      \n    two\n    \"\"\""), "one\n\n  \ntwo\n");
        // Closed after text, nothing comes off; text after the opening `"""` has none to lose.
        assert_eq!(text("\"\"\"one\n  two\"\"\""), "one\n  two");
        assert_eq!(text("\"\"\"Usage\n    more\n    \"\"\""), "Usage\nmore\n");
        assert_eq!(text("\"\"\"\"\"\""), "");
        assert_eq!(text("\"\"\"\n\"\"\""), "");
        // Escapes as on one line, and three quotes written with them; a raw one keeps backslashes.
        assert_eq!(text("\"\"\"\n    a\\tb \\\"\\\"\\\" c\n    \"\"\""), "a\tb \"\"\" c\n");
        assert_eq!(text("r\"\"\"\n    \\d+ {x}\n    \"\"\""), "\\d+ {x}\n");
        // The statement goes on after it, on its last line.
        let tokens: Vec<Tok> = lex("x = \"\"\"\n    a\n    \"\"\" + \"b\"\ny = 1\n", 0)
            .unwrap()
            .into_iter()
            .map(|t| t.tok)
            .collect();
        assert!(tokens.contains(&Tok::Str("a\n".to_owned())) && tokens.contains(&Tok::Str("b".to_owned())));
        assert_eq!(tokens.iter().filter(|tok| **tok == Tok::Newline).count(), 2, "{tokens:?}");
    }

    /// An f-string or a template in `"""`s reads its `{}`s where they're written in the file.
    #[test]
    fn an_fstring_in_triple_quotes_reads_its_braces_where_they_are() {
        use crate::token::FPart;
        let source = "f\"\"\"\n    Hello, {name}!\n      {{you}}\n    \"\"\"";
        let at = u32::try_from(source.find("name").unwrap()).unwrap();
        let expected = [
            FPart::Text("Hello, ".to_owned()),
            FPart::Expr { source: "name".to_owned(), offset: at, spec: None },
            FPart::Text("!\n  {you}\n".to_owned()),
        ];
        assert_eq!(first(source), Tok::FStr(expected.to_vec()));
        assert_eq!(first(&source.replacen('f', "t", 1)), Tok::TStr(expected.to_vec()));
    }

    /// Each mistake in a `"""` string is said where it is, with how to fix it.
    #[test]
    fn a_string_in_triple_quotes_says_what_is_wrong_and_where() {
        let wrong = |source: &str, at: &str| {
            let problem = lex(source, 0).unwrap_err();
            let place = u32::try_from(source.find(at).unwrap()).unwrap();
            assert_eq!(problem.span.start, place, "{source}: {}", problem.message);
            (problem.message, problem.help.unwrap_or_default())
        };
        let (said, help) = wrong("x = 1\ny = \"\"\"abc\n\nz = 2\n", "\"\"\"");
        assert_eq!(said, "the `\"\"\"` on line 2 is never closed");
        assert_eq!(help, "close it with `\"\"\"`");
        let (said, help) = wrong("x = \"\"\"\n    one\n  two\n    \"\"\"\n", "two");
        assert_eq!(said, "this line is indented less than the closing `\"\"\"`");
        assert!(help.contains("at least 4 spaces"), "{help}");
        let (said, _) = wrong("x = \"\"\"\n    one\\\n    \"\"\"\n", "\\");
        assert_eq!(said, "a `\\` can't end a line in a string");
        let (said, _) = wrong("x = f\"\"\"\n    {a\n    }\n    \"\"\"\n", "{");
        assert_eq!(said, "this `{` isn't closed on its line");
        let (said, _) = wrong("x = f\"\"\"\n    {d[\"k\"]}\n    \"\"\"\n", "\"k");
        assert_eq!(said, "a `\"` can't go inside `{}` in a string");
    }
}
