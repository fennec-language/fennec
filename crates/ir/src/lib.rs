//! Fennec's typed intermediate representation: what the backends and the build cache will
//! consume, lowered from the checker's typed tree.
//!
//! A function is a list of typed locals and a list of basic blocks. Each block is a run of
//! instructions, then one terminator that says where control goes. Locals can be written
//! more than once, so a loop needs no special form, and a backend builds SSA from them.
//! Every operator is written out with its operand types known, so `+` on ints and on strs
//! need nothing looked up later. Overflow, dividing by zero and so on are the operator's
//! meaning: each backend has to end the program the same way, at the instruction's span.
//!
//! Lowering grows in slices. So far it handles functions over ints, floats, bools, strs,
//! bytes, the program's own structs and enums, lists, sets, dicts, tuples, capabilities,
//! optionals and results: operators, names, `if`, `while`, `for` over anything but a value
//! with its own iterator, comprehensions, `match`, `return`, `fail`, `?`, `else`, direct calls,
//! what the engines provide, building values, and reading and changing fields and items. All
//! of them are values, so copying one copies it. A comprehension is a loop that fills a
//! collection, and a loop over anything but a list goes through a list of its items. A `match`
//! is a run of tests, one case after another, which a backend can turn into a jump table. A
//! `mut` parameter is copied back: when the call returns, the local passed for it holds what
//! the parameter held. Nothing is implicit: a value going where an optional or a result goes
//! is wrapped by an instruction, and a name known not to be `None` is unwrapped where it's
//! read. A constant whose value is a literal is written out where it's read. An error of
//! one type can go where a set of error types is expected as it is. A
//! function using anything else is listed in [`Program::not_lowered`], with why.

mod lower;
mod print;
mod validate;

use fennec_check::Type;
use fennec_syntax::{BinOp, Span, UnaryOp};

pub use lower::lower;
pub use validate::validate;

/// A program's functions: those lowered so far, and those that couldn't be yet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Program {
    pub structs: Vec<StructDef>,
    pub enums: Vec<EnumDef>,
    /// Every error type, the standard library's included, which programs get values of.
    pub errors: Vec<StructDef>,
    /// The value of each constant that isn't a literal, as a function of no arguments. An
    /// engine works each out once, before anything needs it.
    pub constants: Vec<Function>,
    pub functions: Vec<Function>,
    pub not_lowered: Vec<NotLowered>,
    /// The standard library's functions the program calls, written in Fennec. They aren't
    /// lowered yet.
    pub external: Vec<String>,
}

/// A struct of the program, with its fields' types in the order they're written.
#[derive(Clone, Debug, PartialEq)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<(String, Type)>,
}

/// An enum of the program, with its cases in the order they're written.
#[derive(Clone, Debug, PartialEq)]
pub struct EnumDef {
    pub name: String,
    pub cases: Vec<CaseDef>,
}

/// One case of an enum, with its fields' types in the order they're written.
#[derive(Clone, Debug, PartialEq)]
pub struct CaseDef {
    pub name: String,
    pub fields: Vec<(String, Type)>,
}

/// A function that isn't lowered yet, by its key in the typed tree, and what stopped it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotLowered {
    pub function: String,
    pub why: String,
}

/// One function, by its key in the typed tree: `name`, or `Type.name` for a method.
#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    pub name: String,
    /// How many of the first locals are parameters.
    pub params: usize,
    /// Which parameters are `mut`, by position.
    pub mut_params: Vec<usize>,
    /// `Type::Unit` for a function that gives back nothing.
    pub returns: Type,
    pub locals: Vec<Local>,
    /// The first block is where the function starts.
    pub blocks: Vec<Block>,
}

/// A place a function holds a value. Temporaries have no name.
#[derive(Clone, Debug, PartialEq)]
pub struct Local {
    pub name: Option<String>,
    pub ty: Type,
}

/// A local of the function, by position in [`Function::locals`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalId(pub usize);

/// A block of the function, by position in [`Function::blocks`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlockId(pub usize);

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub insts: Vec<Inst>,
    pub end: Terminator,
}

/// One step, with the source it came from, for reporting what goes wrong while it runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Inst {
    pub kind: InstKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InstKind {
    Copy {
        dest: LocalId,
        value: Operand,
    },
    Unary {
        dest: LocalId,
        op: UnaryOp,
        value: Operand,
    },
    /// An arithmetic or comparison operator on two operands of one type. `and` and `or`
    /// don't appear here, since they're branches.
    Binary {
        dest: LocalId,
        op: BinOp,
        lhs: Operand,
        rhs: Operand,
    },
    /// A new struct, with a value for each field in the order they're written.
    Make {
        dest: LocalId,
        type_name: String,
        fields: Vec<Operand>,
    },
    GetField {
        dest: LocalId,
        object: Operand,
        field: String,
    },
    /// Changes a field of a struct held in a local, following `path`: `["pos", "x"]` is
    /// `object.pos.x`.
    SetField {
        object: LocalId,
        path: Vec<String>,
        value: Operand,
    },
    /// A new value of one case of an enum, with a value for each of its fields in order.
    MakeCase {
        dest: LocalId,
        type_name: String,
        case: String,
        fields: Vec<Operand>,
    },
    /// Whether an enum value is of a case.
    IsCase {
        dest: LocalId,
        value: Operand,
        case: String,
    },
    /// A field of an enum value known to be of `case`.
    GetCaseField {
        dest: LocalId,
        value: Operand,
        case: String,
        field: String,
    },
    MakeList {
        dest: LocalId,
        items: Vec<Operand>,
    },
    MakeSet {
        dest: LocalId,
        items: Vec<Operand>,
    },
    /// A new dict. A key written twice keeps its first place and its last value.
    MakeDict {
        dest: LocalId,
        pairs: Vec<(Operand, Operand)>,
    },
    MakeTuple {
        dest: LocalId,
        items: Vec<Operand>,
    },
    /// One item of a tuple, by a position known here.
    GetTupleItem {
        dest: LocalId,
        value: Operand,
        at: usize,
    },
    /// An item of a list, str, bytes or tuple by position, or of a dict by key. An index out
    /// of range or a key that isn't there ends the program.
    GetIndex {
        dest: LocalId,
        object: Operand,
        index: Operand,
    },
    /// `object[start:end]` on a list, str or bytes. A bound that's left out is the end it
    /// would be, and either can count from the back.
    Slice {
        dest: LocalId,
        object: Operand,
        start: Option<Operand>,
        end: Option<Operand>,
    },
    /// Changes an item of a list or a dict's value for a key, in a local, following `path`
    /// to reach it.
    SetIndex {
        object: LocalId,
        path: Vec<String>,
        index: Operand,
        value: Operand,
    },
    /// An f-string.
    Format {
        dest: LocalId,
        parts: Vec<FormatPart>,
    },
    /// Something the engines provide, by name: a function like `sum`, or a method like
    /// `list.append` or `Stdout.print` with its object first. `changes` says it changes the
    /// local its first argument names.
    Builtin {
        dest: Option<LocalId>,
        name: String,
        args: Vec<Operand>,
        changes: bool,
    },
    /// `T` to `T?`. `None` itself is [`Operand::None`].
    MakeSome {
        dest: LocalId,
        value: Operand,
    },
    IsNone {
        dest: LocalId,
        value: Operand,
    },
    /// The value inside an optional known not to be `None`.
    GetSome {
        dest: LocalId,
        value: Operand,
    },
    /// A result that succeeded, holding nothing when `value` is `None`.
    MakeOk {
        dest: LocalId,
        value: Option<Operand>,
    },
    MakeErr {
        dest: LocalId,
        error: Operand,
    },
    IsErr {
        dest: LocalId,
        value: Operand,
    },
    /// The value inside a result known to have succeeded.
    GetOk {
        dest: LocalId,
        value: Operand,
    },
    /// The error inside a result known to have failed.
    GetErr {
        dest: LocalId,
        value: Operand,
    },
    /// Whether an error from a set of error types is of this one.
    IsError {
        dest: LocalId,
        value: Operand,
        type_name: String,
    },
    /// An error from a set of error types, known to be of the type `dest` holds.
    AsError {
        dest: LocalId,
        value: Operand,
    },
    /// The value of a constant in [`Program::constants`].
    Constant {
        dest: LocalId,
        name: String,
    },
    /// A direct call, by the callee's key. `dest` is `None` when nothing keeps the result.
    Call {
        dest: Option<LocalId>,
        function: String,
        args: Vec<Operand>,
    },
}

/// One piece of an f-string.
#[derive(Clone, Debug, PartialEq)]
pub enum FormatPart {
    Text(String),
    Value { value: Operand, spec: Option<String> },
}

/// A value an instruction reads.
#[derive(Clone, Debug, PartialEq)]
pub enum Operand {
    Local(LocalId),
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    None,
}

/// How a block ends.
#[derive(Clone, Debug, PartialEq)]
pub enum Terminator {
    Jump(BlockId),
    Branch {
        cond: Operand,
        then: BlockId,
        otherwise: BlockId,
    },
    Return(Option<Operand>),
    /// No case of a `match` took the value, which ends the program.
    NoMatch {
        value: Operand,
        span: Span,
    },
    /// The checker has shown control never gets here.
    Unreachable,
}

impl Function {
    /// The type of a value this function reads, or `None` for a local it doesn't have.
    pub fn type_of(&self, operand: &Operand) -> Option<Type> {
        type_in(&self.locals, operand)
    }
}

/// The type of a value, given the locals it can read.
fn type_in(locals: &[Local], operand: &Operand) -> Option<Type> {
    match operand {
        Operand::Local(id) => locals.get(id.0).map(|local| local.ty.clone()),
        Operand::Int(_) => Some(Type::Int),
        Operand::Float(_) => Some(Type::Float),
        Operand::Bool(_) => Some(Type::Bool),
        Operand::Str(_) => Some(Type::Str),
        Operand::None => Some(Type::None),
    }
}

impl Terminator {
    /// The blocks control can go to next.
    pub fn targets(&self) -> Vec<BlockId> {
        match self {
            Terminator::Jump(target) => vec![*target],
            Terminator::Branch { then, otherwise, .. } => vec![*then, *otherwise],
            Terminator::Return(_) | Terminator::NoMatch { .. } | Terminator::Unreachable => Vec::new(),
        }
    }
}
