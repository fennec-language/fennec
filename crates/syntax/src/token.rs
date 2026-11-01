//! The tokens the lexer produces, including Python-style indentation tokens.

use crate::span::Span;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Token {
    pub(crate) tok: Tok,
    pub(crate) span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok {
    Name(String),
    Int(i64),
    Float(f64),
    Str(String),
    FStr(Vec<FPart>),
    Kw(Kw),
    P(Punct),
    Newline,
    Indent,
    Dedent,
    Eof,
}

/// A piece of an f-string: literal text, or the source of an expression to parse later.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FPart {
    Text(String),
    Expr { source: String, offset: u32, spec: Option<String> },
}

/// Hard keywords. `struct`, `enum`, `error`, `test`, `interface`, `type`, `match`, `case` and
/// `where` are soft: they're keywords only where a declaration or `match` can start, so they
/// stay usable as names, as in Python.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kw {
    Def,
    If,
    Elif,
    Else,
    While,
    For,
    In,
    Return,
    Break,
    Continue,
    Pass,
    Mut,
    And,
    Or,
    Not,
    Is,
    True,
    False,
    None,
    Fail,
    Yield,
    With,
    As,
    Import,
    From,
    Export,
    Noalloc,
    Assert,
    /// A keyword the language has planned but this version doesn't support yet.
    Reserved(&'static str),
}

impl Kw {
    pub(crate) fn from_word(word: &str) -> Option<Kw> {
        Some(match word {
            "def" => Kw::Def,
            "if" => Kw::If,
            "elif" => Kw::Elif,
            "else" => Kw::Else,
            "while" => Kw::While,
            "for" => Kw::For,
            "in" => Kw::In,
            "return" => Kw::Return,
            "break" => Kw::Break,
            "continue" => Kw::Continue,
            "pass" => Kw::Pass,
            "mut" => Kw::Mut,
            "and" => Kw::And,
            "or" => Kw::Or,
            "not" => Kw::Not,
            "is" => Kw::Is,
            "True" => Kw::True,
            "False" => Kw::False,
            "None" => Kw::None,
            "fail" => Kw::Fail,
            "yield" => Kw::Yield,
            "with" => Kw::With,
            "as" => Kw::As,
            "import" => Kw::Import,
            "from" => Kw::From,
            "export" => Kw::Export,
            "noalloc" => Kw::Noalloc,
            "assert" => Kw::Assert,
            _ => {
                return RESERVED
                    .iter()
                    .find(|reserved| **reserved == word)
                    .map(|reserved| Kw::Reserved(reserved));
            }
        })
    }

    pub(crate) fn text(self) -> &'static str {
        match self {
            Kw::Def => "def",
            Kw::If => "if",
            Kw::Elif => "elif",
            Kw::Else => "else",
            Kw::While => "while",
            Kw::For => "for",
            Kw::In => "in",
            Kw::Return => "return",
            Kw::Break => "break",
            Kw::Continue => "continue",
            Kw::Pass => "pass",
            Kw::Mut => "mut",
            Kw::And => "and",
            Kw::Or => "or",
            Kw::Not => "not",
            Kw::Is => "is",
            Kw::True => "True",
            Kw::False => "False",
            Kw::None => "None",
            Kw::Fail => "fail",
            Kw::Yield => "yield",
            Kw::With => "with",
            Kw::As => "as",
            Kw::Import => "import",
            Kw::From => "from",
            Kw::Export => "export",
            Kw::Noalloc => "noalloc",
            Kw::Assert => "assert",
            Kw::Reserved(word) => word,
        }
    }
}

const RESERVED: [&str; 2] = ["extern", "unsafe"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Punct {
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Dot,
    Arrow,
    FatArrow,
    Question,
    /// `&`, on ints and joining the interfaces a type parameter has to fit: `T: Ordered & Hashable`.
    Amp,
    /// `|`, on ints and separating the alternatives of a `case`: `case .Int(_) | .Float(_):`.
    Pipe,
    Caret,
    Tilde,
    ShiftLeft,
    ShiftRight,
    Plus,
    Minus,
    Star,
    StarStar,
    Slash,
    SlashSlash,
    Percent,
    Assign,
    EqEq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    AmpEq,
    PipeEq,
    CaretEq,
    ShiftLeftEq,
    ShiftRightEq,
}

impl Punct {
    pub(crate) const THREE_CHAR: [(&'static str, Punct); 2] =
        [("<<=", Punct::ShiftLeftEq), (">>=", Punct::ShiftRightEq)];

    pub(crate) const TWO_CHAR: [(&'static str, Punct); 17] = [
        ("->", Punct::Arrow),
        ("=>", Punct::FatArrow),
        ("==", Punct::EqEq),
        ("!=", Punct::NotEq),
        ("<=", Punct::LtEq),
        (">=", Punct::GtEq),
        ("//", Punct::SlashSlash),
        ("**", Punct::StarStar),
        ("+=", Punct::PlusEq),
        ("-=", Punct::MinusEq),
        ("*=", Punct::StarEq),
        ("/=", Punct::SlashEq),
        ("<<", Punct::ShiftLeft),
        (">>", Punct::ShiftRight),
        ("&=", Punct::AmpEq),
        ("|=", Punct::PipeEq),
        ("^=", Punct::CaretEq),
    ];

    pub(crate) fn text(self) -> &'static str {
        let longer = Self::THREE_CHAR.iter().chain(&Self::TWO_CHAR);
        if let Some((text, _)) = longer.into_iter().find(|(_, punct)| *punct == self) {
            return text;
        }
        match self {
            Punct::LParen => "(",
            Punct::RParen => ")",
            Punct::LBracket => "[",
            Punct::RBracket => "]",
            Punct::LBrace => "{",
            Punct::RBrace => "}",
            Punct::Comma => ",",
            Punct::Colon => ":",
            Punct::Dot => ".",
            Punct::Plus => "+",
            Punct::Minus => "-",
            Punct::Star => "*",
            Punct::Slash => "/",
            Punct::Percent => "%",
            Punct::Assign => "=",
            Punct::Lt => "<",
            Punct::Gt => ">",
            Punct::Amp => "&",
            Punct::Pipe => "|",
            Punct::Caret => "^",
            Punct::Tilde => "~",
            _ => "?",
        }
    }

    pub(crate) fn single(c: char) -> Option<Punct> {
        Some(match c {
            '(' => Punct::LParen,
            ')' => Punct::RParen,
            '[' => Punct::LBracket,
            ']' => Punct::RBracket,
            '{' => Punct::LBrace,
            '}' => Punct::RBrace,
            ',' => Punct::Comma,
            ':' => Punct::Colon,
            '.' => Punct::Dot,
            '?' => Punct::Question,
            '&' => Punct::Amp,
            '|' => Punct::Pipe,
            '^' => Punct::Caret,
            '~' => Punct::Tilde,
            '+' => Punct::Plus,
            '-' => Punct::Minus,
            '*' => Punct::Star,
            '/' => Punct::Slash,
            '%' => Punct::Percent,
            '=' => Punct::Assign,
            '<' => Punct::Lt,
            '>' => Punct::Gt,
            _ => return None,
        })
    }
}
