//! Which tokens on a line get a space between them, and what each bracket is for.
//! This is the only place that reads meaning into tokens, and it reads only as much as
//! spacing and line breaking need.

use fennec_syntax::TokenKind;

/// A token on a line: its kind and its source text.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Tok<'a> {
    pub(crate) kind: TokenKind,
    pub(crate) text: &'a str,
}

/// What a pair of brackets holds. It decides colons, `=` and trailing commas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bracket {
    /// Call arguments, or a pattern's fields: `f(a, b)`.
    Call,
    /// A function's parameters, or an enum case's fields: `Circle(radius: float = 1.0)`.
    Params,
    /// Grouping or a tuple: `(a + b)`, `(a, b)`.
    Paren,
    /// `(a, b) => body`.
    LambdaParams,
    /// Indexing, a slice, or a type's arguments: `xs[i]`, `xs[1:]`, `list[int]`.
    Index,
    /// Generic parameters after a function or type name: `def f[T: Shape]`.
    Generics,
    /// A list, dict or set, or a comprehension.
    Literal,
}

/// The kind of block a line sits in, for soft keywords.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockKind {
    TopLevel,
    /// The body of a struct, enum, error or interface.
    TypeBody,
    /// The `case` arms of a `match`.
    MatchBody,
    Other,
}

/// How a token behaves for spacing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// Something a value ends with: a name, a literal, a closing bracket, a postfix `?`.
    Operand,
    Keyword,
    Open,
    Close,
    Comma,
    Colon,
    SliceColon,
    Attr,
    VariantDot,
    UnaryMinus,
    KwargEq,
    Postfix,
    Operator,
}

/// The spacing and bracket kinds of one logical line.
pub(crate) struct Spacing {
    /// Whether each token has a space before it.
    pub(crate) space: Vec<bool>,
    /// What each opening bracket is for.
    pub(crate) brackets: Vec<Option<Bracket>>,
}

pub(crate) fn analyze(toks: &[Tok<'_>], block: BlockKind) -> Spacing {
    let mut analyzer = Analyzer {
        toks,
        stack: Vec::new(),
        roles: Vec::with_capacity(toks.len()),
        spacing: Spacing { space: Vec::with_capacity(toks.len()), brackets: vec![None; toks.len()] },
        declares_type: declares_type(toks),
        in_def_name: false,
        block,
    };
    for at in 0..toks.len() {
        analyzer.step(at, block);
    }
    analyzer.spacing
}

/// Whether a line declares a type, like `struct Box[T]:`, where `[` opens generic parameters.
fn declares_type(toks: &[Tok<'_>]) -> bool {
    let mut words = toks.iter().skip_while(|tok| matches!(tok.text, "export" | "noalloc"));
    matches!(
        (words.next(), words.next()),
        (Some(first), Some(second))
            if matches!(first.text, "struct" | "enum" | "error" | "interface") && second.kind == TokenKind::Name
    )
}

struct Analyzer<'t, 'a> {
    toks: &'t [Tok<'a>],
    stack: Vec<Bracket>,
    roles: Vec<Role>,
    spacing: Spacing,
    declares_type: bool,
    /// Between `def` and the `(` of its parameters, where `def []=(` is written without spaces.
    in_def_name: bool,
    block: BlockKind,
}

impl<'a> Analyzer<'_, 'a> {
    fn tok(&self, at: usize) -> Option<Tok<'a>> {
        self.toks.get(at).copied()
    }

    fn step(&mut self, at: usize, block: BlockKind) {
        let Some(tok) = self.tok(at) else { return };
        let prev = at.checked_sub(1).and_then(|before| self.roles.get(before)).copied();
        let in_def_name = self.in_def_name && self.stack.is_empty();
        let role = self.role(at, tok, prev, block);
        let glued = in_def_name && prev != Some(Role::Keyword);
        let space = !glued && prev.is_some_and(|prev| space_between(prev, role));
        if in_def_name && tok.text == "(" {
            self.in_def_name = false;
        }
        if tok.kind == TokenKind::Keyword && tok.text == "def" && self.stack.is_empty() {
            self.in_def_name = true;
        }
        self.spacing.space.push(space);
        self.roles.push(role);
    }

    fn role(&mut self, at: usize, tok: Tok<'_>, prev: Option<Role>, block: BlockKind) -> Role {
        let after_operand = prev.is_some_and(ends_operand);
        match tok.kind {
            TokenKind::Keyword if matches!(tok.text, "True" | "False" | "None") => Role::Operand,
            TokenKind::Keyword => Role::Keyword,
            TokenKind::Name if self.soft_keyword(at, tok.text, after_operand, block) => Role::Keyword,
            TokenKind::Punct => self.punct_role(at, tok.text, prev),
            // Names, numbers and strings; layout tokens never reach a line.
            _ => Role::Operand,
        }
    }

    /// Whether a name is acting as a soft keyword: `case` in a match, `where` after a
    /// field's type, or `match` starting a match.
    fn soft_keyword(&self, at: usize, word: &str, after_operand: bool, block: BlockKind) -> bool {
        match word {
            "case" => at == 0 && block == BlockKind::MatchBody,
            "where" => after_operand && self.stack.is_empty() && block == BlockKind::TypeBody,
            "match" => {
                let next = self.tok(at + 1).map_or("", |next| next.text);
                !after_operand && !matches!(next, "" | "=" | "(" | "." | "," | ")" | ":")
            }
            _ => false,
        }
    }

    fn punct_role(&mut self, at: usize, text: &str, prev: Option<Role>) -> Role {
        let after_operand = prev.is_some_and(ends_operand);
        match text {
            "(" | "[" | "{" => {
                let bracket = self.bracket(at, text, after_operand);
                self.stack.push(bracket);
                if let Some(slot) = self.spacing.brackets.get_mut(at) {
                    *slot = Some(bracket);
                }
                Role::Open
            }
            ")" | "]" | "}" => {
                self.stack.pop();
                Role::Close
            }
            "," => Role::Comma,
            ":" if self.stack.last() == Some(&Bracket::Index) => Role::SliceColon,
            ":" => Role::Colon,
            "." if after_operand => Role::Attr,
            "." => Role::VariantDot,
            "?" => Role::Postfix,
            "-" if !after_operand => Role::UnaryMinus,
            "=" if self.is_kwarg(at) => Role::KwargEq,
            _ => Role::Operator,
        }
    }

    fn bracket(&self, at: usize, text: &str, after_operand: bool) -> Bracket {
        let top = self.stack.is_empty();
        match text {
            "(" if top && self.in_def_name => Bracket::Params,
            "(" if top && at == 1 && self.block == BlockKind::TypeBody => Bracket::Params,
            "(" if after_operand => Bracket::Call,
            "(" if self.closes_before_arrow(at) => Bracket::LambdaParams,
            "(" => Bracket::Paren,
            "[" if top && (self.in_def_name || self.declares_type) => Bracket::Generics,
            "[" if after_operand => Bracket::Index,
            _ => Bracket::Literal,
        }
    }

    /// Whether the `(` at `at` holds an arrow function's parameters: names up to the first `)`,
    /// then `=>`.
    fn closes_before_arrow(&self, at: usize) -> bool {
        let close = self.toks.iter().skip(at).position(|tok| tok.text == ")");
        close.is_some_and(|length| self.tok(at + length + 1).is_some_and(|next| next.text == "=>"))
    }

    /// Whether `=` at `at` names a call argument, as in `Vec3(x=1.0)`.
    fn is_kwarg(&self, at: usize) -> bool {
        let in_call = self.stack.last() == Some(&Bracket::Call);
        let name_before = at.checked_sub(1).and_then(|before| self.tok(before));
        let opener = at.checked_sub(2).and_then(|before| self.tok(before));
        in_call
            && name_before.is_some_and(|tok| tok.kind == TokenKind::Name)
            && opener.is_some_and(|tok| matches!(tok.text, "(" | ","))
    }
}

fn space_between(prev: Role, next: Role) -> bool {
    if matches!(next, Role::Comma | Role::Colon | Role::SliceColon | Role::Close | Role::Postfix | Role::Attr)
    {
        return false;
    }
    if matches!(prev, Role::Open | Role::Attr | Role::VariantDot | Role::UnaryMinus | Role::SliceColon) {
        return false;
    }
    if prev == Role::KwargEq || next == Role::KwargEq {
        return false;
    }
    !(next == Role::Open && ends_operand(prev))
}

/// Whether a token can end a value, so that `(` after it is a call and `-` is subtraction.
fn ends_operand(role: Role) -> bool {
    matches!(role, Role::Operand | Role::Close | Role::Postfix)
}
