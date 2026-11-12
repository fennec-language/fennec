//! The syntax tree the parser produces.

use crate::span::Span;

/// A parsed `.fen` file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Module {
    pub items: Vec<Item>,
    /// Standard library functions as this program runs them, by the key the checker gives each,
    /// like `concurrent.told` or `Pool.borrowed`: rewritten as a program's own code is, as by a
    /// `with` source or `launch` written in where it's used. They stand in for the library's.
    pub std_rewritten: Vec<(String, FnDef)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Function(FnDef),
    Struct(TypeDef),
    Enum(TypeDef),
    Error(TypeDef),
    Interface(Interface),
    /// `extend Parser:`: methods for a type declared elsewhere in the kit.
    Extend(Extension),
    TypeAlias {
        name: String,
        ty: TypeExpr,
        span: Span,
    },
    /// `NAME = value` at the top level of a file: a name that never changes, with its value
    /// computed before the program runs. `NAME: type = value` writes its type.
    Const {
        name: String,
        ty: Option<TypeExpr>,
        value: Expr,
        span: Span,
    },
    Import(Import),
    Test(TestDef),
    /// A statement at the top level, while parsing: only a constant is allowed to stay.
    Stmt(Stmt),
}

#[derive(Clone, Debug, PartialEq)]
pub struct FnDef {
    pub name: String,
    /// Type parameters, like `T` in `def first[T](items: list[T]) -> T`.
    pub generics: Vec<Generic>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    /// Where `->` is, when the return type is written, so a fix can remove it.
    pub arrow: Option<Span>,
    /// The error it says it can fail with: `LoadError` in `-> float fails LoadError`.
    pub fails: Option<TypeExpr>,
    /// Empty only for a `def` written without one, which std's `@intrinsic` functions are.
    pub body: Vec<Stmt>,
    /// What's written above it, like `@intrinsic`.
    pub annotations: Vec<Annotation>,
    pub span: Span,
}

impl FnDef {
    /// Whether this is a method that takes `self`, rather than a named constructor.
    pub fn takes_self(&self) -> bool {
        self.params.first().is_some_and(|param| param.is_self)
    }
}

/// A type parameter, with the interfaces its types have to fit: `T`, `T: Shape` or
/// `T: Ordered & Hashable`.
#[derive(Clone, Debug, PartialEq)]
pub struct Generic {
    pub name: String,
    pub bounds: Vec<TypeExpr>,
    pub span: Span,
}

impl Generic {
    /// Whether the checker made this for a parameter typed by an interface, rather than the
    /// program writing it.
    pub fn is_hidden(&self) -> bool {
        self.name.contains(crate::visit::HIDDEN)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    /// Missing only for `self`.
    pub ty: Option<TypeExpr>,
    pub is_mut: bool,
    pub is_self: bool,
    /// The plain value it has when a call leaves it out, which is passed by name if at all.
    pub default: Option<Expr>,
    pub span: Span,
}

/// A written type, like `int`, `list[str]`, `(int, int)` or `Enemy?`. A function type,
/// `(int, str) -> bool`, is named `->`, with its parameters' types and then its return type.
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
    /// Type parameters, like `T` in `struct Deque[T]:`.
    pub generics: Vec<Generic>,
    pub fields: Vec<Field>,
    pub cases: Vec<Case>,
    pub methods: Vec<FnDef>,
    /// Whether it's a `shared` type: a counted handle to one cell, whose methods lock it.
    pub shared: bool,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Option<TypeExpr>,
    pub default: Option<Expr>,
    /// The `where` rule a value of this field must follow.
    pub rule: Option<Expr>,
    /// What's written above it, like `@skip`: data for code that reads the type, never a
    /// change to the field.
    pub annotations: Vec<Annotation>,
    pub span: Span,
}

impl Field {
    /// Whether `@skip` leaves the field out when a value is saved.
    pub fn is_skipped(&self) -> bool {
        self.annotations.iter().any(|annotation| annotation.name == "skip")
    }

    /// The name `@serialize(name=...)` gives the field when a value is saved, if it has one.
    pub fn saved_name(&self) -> Option<&str> {
        let args = self.annotations.iter().filter(|annotation| annotation.name == "serialize");
        args.flat_map(|annotation| &annotation.args).find(|arg| arg.name.as_deref() == Some("name")).and_then(
            |arg| match &arg.value.kind {
                ExprKind::Str(text) => Some(text.as_str()),
                _ => None,
            },
        )
    }
}

/// `@name` or `@name(args)` above a field: data the field carries, read at compile time.
/// A name after a `with`'s `as`.
#[derive(Clone, Debug, PartialEq)]
pub struct WithName {
    pub name: String,
    pub is_mut: bool,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub name: String,
    pub args: Vec<Arg>,
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

/// Methods added to a type outside its declaration. `span` covers `extend Name[T]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Extension {
    pub name: String,
    /// A generic type's parameters, like `T` in `extend list[T: Eq]:`. A bound makes the
    /// methods conditional: they exist only where the type's arguments fit it.
    pub generics: Vec<Generic>,
    pub methods: Vec<FnDef>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Import {
    pub module: Vec<String>,
    pub names: Vec<String>,
    /// The name each of `names` is bound under, when `as` renames it: `loads` in
    /// `from std.json import loads as json_loads` has `json_loads`. One per name.
    pub aliases: Vec<Option<String>>,
    /// The name a whole module is bound under, from `import game.physics as physics`.
    pub alias: Option<String>,
    pub span: Span,
}

impl Import {
    /// Each imported name, with the name it's bound under.
    pub fn bound(&self) -> impl Iterator<Item = (&str, &str)> {
        self.names.iter().enumerate().map(|(at, name)| {
            let alias = self.aliases.get(at).and_then(Option::as_deref);
            (name.as_str(), alias.unwrap_or(name))
        })
    }
}

/// `test "name":`, a test that names its capabilities `test "name"(fs: Fs):`, or a property
/// test `test "name" for items: list[int]:`.
#[derive(Clone, Debug, PartialEq)]
pub struct TestDef {
    pub name: String,
    pub capabilities: Vec<Param>,
    pub inputs: Vec<Param>,
    pub body: Vec<Stmt>,
    /// Whether it's a `bench` block, which is checked like a test and runs only for
    /// `fennec bench`.
    pub bench: bool,
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
    /// `with value as name:`, or `with a, b as mut x, mut y:`, whose `value` is the tuple of
    /// what it holds and which names each one.
    /// `with value as name:`, or, with `lock`, `lock value as ref name:`, which reaches a
    /// `shared` value: `name` is what it holds, for the block.
    With {
        value: Expr,
        names: Vec<WithName>,
        body: Vec<Stmt>,
        lock: bool,
    },
    Def(FnDef),
    /// Written in for a `with` block whose source has an `else:`. A `?` or `fail` in `body` runs
    /// that cleanup where it's written, but a failure can also leave from inside what the block
    /// calls: from a generator it reads, or a `parallel` loop's round. Those run `cleanup`
    /// first, with the failure in `name`, and `cleanup` ends by failing with it.
    OnFailure {
        name: String,
        cleanup: Vec<Stmt>,
        body: Vec<Stmt>,
    },
    /// `lend value`, in a function that gives back `with T`: the caller's `with` block runs
    /// here, with `value` lent to it. `otherwise` is the `except:` block, which runs when the
    /// caller's block fails, empty when there's none. `except` says it was written so, rather
    /// than with the `else:` of before 2026-11-11, which is refused.
    Lend {
        value: Expr,
        otherwise: Vec<Stmt>,
        except: bool,
    },
    Break,
    Continue,
    Pass,
}

/// The type `ref name = place` is written in with: `name: ref = place`.
pub const REF: &str = "ref";

impl StmtKind {
    /// `ref name = place`: the name, and the place it stands for.
    pub fn local_ref(&self) -> Option<(&str, &Expr)> {
        let Self::Assign { target, op: None, value, ty: Some(written), .. } = self else { return None };
        let ExprKind::Name(name) = &target.kind else { return None };
        (written.name == REF && written.args.is_empty()).then_some((name.as_str(), value))
    }
}

/// A file's bytes, shared since a file can be big and a syntax tree is copied. Written for
/// debugging, it's a hash and not every byte: hashing a tree that way still sees any change.
#[derive(Clone, PartialEq, Eq)]
pub struct Blob(pub std::sync::Arc<[u8]>);

impl Blob {
    pub fn hash(&self) -> fennec_blake3::Hash {
        fennec_blake3::hash(&self.0)
    }
}

impl From<Vec<u8>> for Blob {
    fn from(data: Vec<u8>) -> Self {
        Blob(data.into())
    }
}

impl std::fmt::Debug for Blob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Blob({}, {})", self.0.len(), self.hash())
    }
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
    /// A duration literal, like `30s`, in nanoseconds: a `Duration` from `std.time`.
    Duration(i64),
    /// A file's bytes, read when the program was loaded: what `embed_file("path")` becomes.
    Bytes(Blob),
    /// What `embed_folder("path")` becomes: each file under the folder, by its path from it
    /// written with `/`, in sorted order, with its bytes.
    Folder(Vec<(String, Blob)>),
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
    /// `value else default`: the value, or the default if it's None or an error. A default that
    /// leaves, like `else continue`, is a `match` with one case: see [`Expr::leaves`].
    /// `value except why:` is the same, handling only a failure: `except` says which was written.
    Else {
        value: Box<Expr>,
        default: Box<Expr>,
        except: bool,
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

impl Expr {
    /// The type an expression spells, as in `json.loads[list[int]]`: a name, a name with
    /// types in brackets, a tuple of types, or any of those with `?`. `None` for anything
    /// else.
    /// A type in brackets with others, or a number there: an array's size, `16` in
    /// `array[float, 16]`.
    fn as_type_or_size(&self) -> Option<TypeExpr> {
        match self.kind {
            ExprKind::Int(size) => {
                Some(TypeExpr { name: size.to_string(), args: Vec::new(), optional: false, span: self.span })
            }
            _ => self.as_type(),
        }
    }

    pub fn as_type(&self) -> Option<TypeExpr> {
        let plain = |name: &str, args: Vec<TypeExpr>| TypeExpr {
            name: name.to_owned(),
            args,
            optional: false,
            span: self.span,
        };
        match &self.kind {
            ExprKind::Name(name) => Some(plain(name, Vec::new())),
            ExprKind::None => Some(plain("None", Vec::new())),
            ExprKind::Index { object, index } => {
                let ExprKind::Name(name) = &object.kind else { return None };
                // `dict[str, int]` has two types; `list[(int, int)]` has one, a tuple, whose
                // span starts at its `(` rather than at its first item.
                let args = match &index.kind {
                    ExprKind::Tuple(items)
                        if items.first().is_some_and(|first| first.span.start == index.span.start) =>
                    {
                        items.iter().map(Expr::as_type_or_size).collect::<Option<Vec<_>>>()?
                    }
                    _ => vec![index.as_type()?],
                };
                Some(plain(name, args))
            }
            ExprKind::Tuple(items) => {
                Some(plain("tuple", items.iter().map(Expr::as_type).collect::<Option<_>>()?))
            }
            ExprKind::Try { value, context: None } => Some(TypeExpr { optional: true, ..value.as_type()? }),
            // `launch_end[Closed | Timeout]`: one written type named `|`, as in a signature.
            ExprKind::Binary { op: BinOp::BitOr, lhs, rhs } => {
                let mut members = match lhs.as_type()? {
                    left if left.name == "|" => left.args,
                    left => vec![left],
                };
                members.push(rhs.as_type()?);
                Some(plain("|", members))
            }
            _ => None,
        }
    }
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
    /// `if condition` after the pattern: the arm is taken only when it's true.
    pub guard: Option<Expr>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Pattern {
    Wildcard,
    Bind(String),
    /// `ref clicks` in `case .Button(ref clicks):`: a name for the field itself, so the case's
    /// body changes it in what the `match` is over. Once checked, it's written in as a plain name
    /// whose value is put back where the case ends.
    RefBind(String),
    Literal(Expr),
    /// `.Circle(radius)` (`dotted`), or `NotFound(path)`, `Ok(value)`, `Err(error)`. Written by
    /// name, as `IoError(reason=r)`, `fields` names each of `args`. The checker then puts them
    /// in the fields' order, with `_` for any it skips, so what runs a program can take `args`
    /// by position.
    Variant {
        name: String,
        dotted: bool,
        args: Vec<Pattern>,
        fields: Vec<String>,
    },
    Tuple(Vec<Pattern>),
    /// `.Int(_) | .Float(_)`: any of the alternatives, each binding the same names.
    Or(Vec<Pattern>),
}

impl Pattern {
    /// Whether the pattern has a `ref` in it anywhere.
    #[must_use]
    pub fn holds_ref(&self) -> bool {
        match self {
            Pattern::RefBind(_) => true,
            Pattern::Variant { args: patterns, .. } | Pattern::Tuple(patterns) | Pattern::Or(patterns) => {
                patterns.iter().any(Pattern::holds_ref)
            }
            Pattern::Wildcard | Pattern::Bind(_) | Pattern::Literal(_) => false,
        }
    }
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
    /// `~x`: every bit of an int flipped.
    BitNot,
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
    BitAnd,
    BitOr,
    BitXor,
    /// `a << n`: bits shifted past the top are lost, and it can't overflow.
    ShiftLeft,
    /// `a >> n`: the sign is kept, so it rounds down.
    ShiftRight,
}

impl Expr {
    /// Whether this is what `else continue`, `else return`, `else fail` or an `else:` block parse
    /// to: it never gives a value, because it always leaves.
    pub fn leaves(&self) -> bool {
        let ExprKind::Match { subject, arms } = &self.kind else { return false };
        let [MatchArm { pattern: Pattern::Wildcard | Pattern::Bind(_), guard: None, body, .. }] =
            arms.as_slice()
        else {
            return false;
        };
        let Some(Stmt { kind, .. }) = body.last() else { return false };
        matches!(subject.kind, ExprKind::Bool(false))
            && matches!(kind, StmtKind::Continue | StmtKind::Break | StmtKind::Return(_) | StmtKind::Fail(_))
    }

    /// The block an `else:` or `else as name:` holds, with the name it gives the failure, when this
    /// is what one parsed to: one case, over a value nothing wrote. The block leaves, or its last
    /// line is the value. `else continue`, `else return` and `else fail` are blocks of one line.
    pub fn else_block(&self) -> Option<(Option<&str>, &[Stmt])> {
        let ExprKind::Match { subject, arms } = &self.kind else { return None };
        if !matches!(subject.kind, ExprKind::Bool(false)) || subject.span.start != subject.span.end {
            return None;
        }
        match arms.as_slice() {
            [MatchArm { pattern: Pattern::Bind(name), guard: None, body, .. }] => Some((Some(name), body)),
            [MatchArm { pattern: Pattern::Wildcard, guard: None, body, .. }] => Some((None, body)),
            _ => None,
        }
    }

    /// What `assert value fails Pattern` checks, when this is what one parsed to: a `match` with one
    /// case and nothing in it.
    pub fn asserted_failure(&self) -> Option<(&Expr, &Pattern)> {
        let ExprKind::Match { subject, arms } = &self.kind else { return None };
        match arms.as_slice() {
            [MatchArm { pattern, guard: None, body, .. }] if body.is_empty() => Some((subject, pattern)),
            _ => None,
        }
    }

    /// The name `else as name:` gives the failure and the block it holds, when this is what one
    /// parsed to.
    pub fn failure_named(&self) -> Option<(&str, &[Stmt])> {
        match self.else_block()? {
            (Some(name), body) => Some((name, body)),
            (None, _) => None,
        }
    }

    /// Whether this is a plain value written out: a number, string, bool or `None`.
    pub fn is_plain_value(&self) -> bool {
        match &self.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Duration(_)
            | ExprKind::Bytes(_)
            | ExprKind::Folder(_)
            | ExprKind::Str(_)
            | ExprKind::Bool(_)
            | ExprKind::None => true,
            ExprKind::Unary { op: UnaryOp::Neg, expr } => {
                matches!(expr.kind, ExprKind::Int(_) | ExprKind::Float(_))
            }
            // An enum case, like `Mode.fast`, or an empty collection.
            ExprKind::Attr { object, optional: false, .. } => matches!(object.kind, ExprKind::Name(_)),
            // A type made with its type arguments, like `Packed[int]()` or `Grid[Tile](width=8)`.
            ExprKind::Call { callee, args } => {
                matches!(&callee.kind, ExprKind::Index { object, .. } if matches!(object.kind, ExprKind::Name(_)))
                    && args.iter().all(|arg| !arg.is_mut && arg.value.is_plain_value())
            }
            ExprKind::List(items) => items.is_empty(),
            ExprKind::Dict(pairs) => pairs.is_empty(),
            _ => false,
        }
    }
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
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::ShiftLeft => "<<",
            BinOp::ShiftRight => ">>",
        }
    }

    /// The name of the method that defines this operator on your own types, like `def +`.
    pub fn method_name(self) -> Option<&'static str> {
        match self {
            BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::FloorDiv
            | BinOp::Mod
            | BinOp::Lt
            | BinOp::Eq => Some(self.symbol()),
            _ => None,
        }
    }
}
