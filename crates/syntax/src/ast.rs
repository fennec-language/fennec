//! The syntax tree the parser produces.

use crate::span::Span;

/// A parsed `.fen` file, or a chunk typed into the REPL.
#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Function(FnDef),
    Struct(TypeDef),
    Enum(TypeDef),
    Error(TypeDef),
    Interface(Interface),
    TypeAlias {
        name: String,
        ty: TypeExpr,
        span: Span,
    },
    Import(Import),
    Test(TestDef),
    /// A statement at the top level, which only the REPL accepts.
    Stmt(Stmt),
}

#[derive(Clone, Debug, PartialEq)]
pub struct FnDef {
    pub name: String,
    /// Type parameters, like `T` in `def first[T](items: list[T]) -> T`.
    pub generics: Vec<Generic>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

impl FnDef {
    /// Whether this is a method that takes `self`, rather than a named constructor.
    pub fn takes_self(&self) -> bool {
        self.params.first().is_some_and(|param| param.is_self)
    }
}

/// A type parameter, with the interface its types have to fit: `T` or `T: Shape`.
#[derive(Clone, Debug, PartialEq)]
pub struct Generic {
    pub name: String,
    pub bound: Option<TypeExpr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    /// Missing only for `self`.
    pub ty: Option<TypeExpr>,
    pub is_mut: bool,
    pub is_self: bool,
    pub span: Span,
}

/// A written type, like `int`, `list[str]`, `(int, int)` or `Enemy?`.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeExpr {
    pub name: String,
    pub args: Vec<TypeExpr>,
    pub optional: bool,
    pub span: Span,
}

/// A struct, an error, or an enum. Enums have cases; structs and errors have fields.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeDef {
    pub name: String,
    pub fields: Vec<Field>,
    pub cases: Vec<Case>,
    pub methods: Vec<FnDef>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Option<TypeExpr>,
    pub default: Option<Expr>,
    /// The `where` rule a value of this field must follow.
    pub rule: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Case {
    pub name: String,
    pub fields: Vec<Field>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Interface {
    pub name: String,
    pub methods: Vec<FnDef>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Import {
    pub module: Vec<String>,
    pub names: Vec<String>,
    pub span: Span,
}

/// `test "name":`, or a property test `test "name" for items: list[int]:`.
#[derive(Clone, Debug, PartialEq)]
pub struct TestDef {
    pub name: String,
    pub inputs: Vec<Param>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    Expr(Expr),
    /// `name = value`, `mut name = value`, `a, b = pair`, `target op= value`, and
    /// `name: type = value`, which declares a name with its type written.
    Assign {
        target: Expr,
        op: Option<BinOp>,
        value: Expr,
        declare_mut: bool,
        ty: Option<TypeExpr>,
    },
    If {
        branches: Vec<(Expr, Vec<Stmt>)>,
        otherwise: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    For {
        names: Vec<String>,
        is_mut: bool,
        iter: Expr,
        body: Vec<Stmt>,
    },
    Return(Option<Expr>),
    Fail(Expr),
    Yield(Expr),
    Assert(Expr),
    With {
        value: Expr,
        name: Option<String>,
        is_mut: bool,
        body: Vec<Stmt>,
    },
    Def(FnDef),
    Break,
    Continue,
    Pass,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    FStr(Vec<FStrPart>),
    Bool(bool),
    None,
    Name(String),
    /// `.Circle`: an enum case of whatever enum has a case with this name.
    Variant(String),
    List(Vec<Expr>),
    Tuple(Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),
    Set(Vec<Expr>),
    /// `[elem for a, b in iter if cond]`, generator expressions, and set and dict comprehensions.
    Comprehension {
        kind: CompKind,
        elem: Box<Expr>,
        value: Option<Box<Expr>>,
        names: Vec<String>,
        iter: Box<Expr>,
        cond: Option<Box<Expr>>,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `then if cond else otherwise`.
    Cond {
        then: Box<Expr>,
        cond: Box<Expr>,
        otherwise: Box<Expr>,
    },
    /// `value else default`: the value, or the default if it's None or an error.
    Else {
        value: Box<Expr>,
        default: Box<Expr>,
    },
    /// `value?`, passing an error or None up; `value? from Context(...)` wraps the error.
    Try {
        value: Box<Expr>,
        context: Option<Box<Expr>>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    Attr {
        object: Box<Expr>,
        name: String,
        optional: bool,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    Slice {
        object: Box<Expr>,
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
    },
    /// `x => body` or `(a, b) => body`.
    Lambda {
        params: Vec<String>,
        body: Box<Expr>,
    },
    Match {
        subject: Box<Expr>,
        arms: Vec<MatchArm>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompKind {
    List,
    /// A generator expression passed straight to a call, like `sum(x for x in xs)`.
    Generator,
    Set,
    Dict,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Pattern {
    Wildcard,
    Bind(String),
    Literal(Expr),
    /// `.Circle(radius)` (`dotted`), or `NotFound(path)`, `Ok(value)`, `Err(error)`.
    Variant {
        name: String,
        dotted: bool,
        args: Vec<Pattern>,
    },
    Tuple(Vec<Pattern>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum FStrPart {
    Text(String),
    Expr { expr: Expr, spec: Option<String> },
}

/// A call argument: `value`, `name=value` or `mut name`.
#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    pub name: Option<String>,
    pub is_mut: bool,
    pub value: Expr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
    In,
    NotIn,
    Is,
    IsNot,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::FloorDiv => "//",
            BinOp::Mod => "%",
            BinOp::Pow => "**",
            BinOp::Eq => "==",
            BinOp::NotEq => "!=",
            BinOp::Lt => "<",
            BinOp::LtEq => "<=",
            BinOp::Gt => ">",
            BinOp::GtEq => ">=",
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::In => "in",
            BinOp::NotIn => "not in",
            BinOp::Is => "is",
            BinOp::IsNot => "is not",
        }
    }

    /// The name of the method that defines this operator on your own types, like `def +`.
    pub fn method_name(self) -> Option<&'static str> {
        match self {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::FloorDiv | BinOp::Mod | BinOp::Lt => {
                Some(self.symbol())
            }
            _ => None,
        }
    }
}
