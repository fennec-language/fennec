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
    Expr {
        source: String,
        offset: u32,
        spec: Option<String>,
    },
}

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
            Kw::Reserved(word) => word,
        }
    }
}

const RESERVED: [&str; 20] = [
    "as",
    "assert",
    "case",
    "enum",
    "error",
    "export",
    "extern",
    "fail",
    "from",
    "import",
    "interface",
    "match",
    "noalloc",
    "struct",
    "test",
    "type",
    "unsafe",
    "where",
    "with",
    "yield",
];

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
}

impl Punct {
    pub(crate) const TWO_CHAR: [(&'static str, Punct); 12] = [
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
    ];

    pub(crate) fn text(self) -> &'static str {
        if let Some((text, _)) = Self::TWO_CHAR.iter().find(|(_, punct)| *punct == self) {
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
