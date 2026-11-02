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
//! Every value is a copy, so changing an item's insides reads it, changes it and writes it
//! back, and a closure captures copies of what it reads. A `match` is a run of tests, one case
//! after another, which a backend can turn into a jump table. A comprehension is a loop that
//! fills a collection, a generator fills a list as it runs, and a loop over anything but a
//! list goes through a list of its items. A `mut` parameter is copied back: when the call
//! returns, the local passed for it holds what the parameter held. Nothing is implicit: a
//! value going where an optional or a result goes is wrapped by an instruction, and a name
//! known not to be `None` is unwrapped where it's read. An error of one type can go where a
//! set of error types is expected as it is. A constant whose value is a literal is written
//! out where it's read.
//!
//! A `shared` type is the one exception to values being copies: a value of one is a counted
//! handle to a cell, so every local holding it reaches the same struct. [`InstKind::Make`] of
//! one makes a new cell, and reading, setting and changing its fields goes through to the cell,
//! just as it would for a struct held in a local. Its methods take the handle as their first
//! parameter, which is never `mut`, since the handle itself doesn't change. A call to one of
//! them, or a read of a field, takes the handle's lock first and gives it back after, with
//! [`InstKind::Lock`] and [`InstKind::Unlock`]. Inside its own methods, `self` is locked
//! already, so what they do with `self` takes no lock.
//!
//! Generics are gone by now: a generic function or type is copied for each set of types a
//! program uses it with, so `Deque[T]` for ints is the struct `Deque[int]`, and its `append`
//! is the function `Deque.append[int]`. The standard library's functions come in as they're
//! reached. Functions that nothing calls and that are generic aren't lowered. A function that
//! can't be lowered is listed in [`Program::not_lowered`], with why, and no example has one.
//! Tests aren't lowered yet.

mod costs;
pub mod facts;
mod lower;
mod print;
mod validate;

use fennec_check::Type;
use fennec_syntax::{BinOp, Span, UnaryOp};

pub use costs::{Cost, Finding, Shared, costs};
pub use lower::{lower, lower_some};
pub use validate::validate;

/// A program's functions: those lowered so far, and those that couldn't be yet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Program {
    pub structs: Vec<StructDef>,
    pub enums: Vec<EnumDef>,
    /// Every error type, the standard library's included, which programs get values of.
    pub errors: Vec<StructDef>,
    /// Every generator that's passed around as a value.
    pub generators: Vec<GeneratorDef>,
    /// The value of each constant that isn't a literal, as a function of no arguments. An
    /// engine works each out once, before anything needs it.
    pub constants: Vec<Function>,
    pub functions: Vec<Function>,
    pub not_lowered: Vec<NotLowered>,
    /// What the program calls that isn't here or in `not_lowered`. Lowering a whole program
    /// leaves this empty.
    pub external: Vec<String>,
}

/// A struct of the program, with its fields' types in the order they're written.
#[derive(Clone, Debug, PartialEq)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<(String, Type)>,
    /// Whether it's a `shared` type, whose values are handles to a cell with a lock.
    pub shared: bool,
}

/// A generator's block's slots, by number. Every generator's start the same way, so a loop can
/// read one without knowing which it is. Its locals' slots are in [`GeneratorDef::slots`].
pub mod slot {
    /// Where it goes on from: 0 to start, a positive number from a `yield`, -1 finished, -2
    /// running, -3 failed.
    pub const PC: usize = 0;
    /// Whether anything has started using it.
    pub const STARTED: usize = 1;
    /// What it failed with, when `PC` says it did.
    pub const FAILURE: usize = 2;
    /// The item it last gave.
    pub const ITEM: usize = 3;
    /// Where the locals it keeps start.
    pub const LOCALS: usize = 4;
}

/// A generator: its block's contents, and the functions that run it.
#[derive(Clone, Debug, PartialEq)]
pub struct GeneratorDef {
    pub name: String,
    pub item: Type,
    /// The types of the locals it keeps, from [`slot::LOCALS`] on.
    pub slots: Vec<Type>,
    /// Takes the block, and gives a bool: whether there's another item.
    pub resume: String,
    /// Closes what's open when it's abandoned, if it can have anything open.
    pub abandon: Option<String>,
    /// Whether it can fail.
    pub fails: bool,
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
    /// The `for` loops that count one step at a time, which a backend or the optimizer can
    /// use to leave out checks.
    pub counts: Vec<Count>,
}

/// A `for` loop's count. It's set once before the loop, the limit is read once, and it's short
/// of the limit whenever the body runs, so the one step after the body can't overflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Count {
    pub count: LocalId,
    pub stop: LocalId,
    /// It steps by -1, and stays above the limit, rather than by 1 and below it.
    pub down: bool,
}

/// A place a function holds a value. Temporaries have no name.
#[derive(Clone, Debug, PartialEq)]
pub struct Local {
    pub name: Option<String>,
    pub ty: Type,
    /// Whether it's what an explicit `copy(...)` gives: a separate value on purpose, so changing
    /// it later is never reported as a copy.
    pub copied: bool,
}

/// A local of the function, by position in [`Function::locals`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
    /// `object[start:end]` on a list, str, bytes or tuple. A bound that's left out is the end it
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
    /// local its first argument names. A call with named arguments has their names on the
    /// end, in name order, like `sorted.key`, and they come last in that order.
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
    /// A closure of a function, whose first parameters take the values captured here.
    MakeClosure {
        dest: LocalId,
        function: String,
        captures: Vec<Operand>,
    },
    /// Calls a closure. `dest` is `None` when nothing keeps the result.
    CallClosure {
        dest: Option<LocalId>,
        closure: Operand,
        args: Vec<Operand>,
    },
    /// The value of a constant in [`Program::constants`].
    Constant {
        dest: LocalId,
        name: String,
    },
    /// A new generator's block, with these values in these slots. Every other slot is zero.
    MakeGenerator {
        dest: LocalId,
        generator: String,
        args: Vec<(usize, Operand)>,
    },
    /// A slot of a generator's block (see [`slot`]). A list, string or boxed value read from
    /// a slot gets a new owner, like [`InstKind::Copy`]. A slot nothing wrote is zero.
    GetSlot {
        dest: LocalId,
        generator: Operand,
        slot: usize,
    },
    /// Writes a slot, which takes an owner of the value and releases what the slot held. A
    /// fresh block's slots are zero, so that's safe from the start.
    SetSlot {
        generator: Operand,
        slot: usize,
        value: Operand,
    },
    /// Runs a generator to its next item, by calling the function its block names, giving
    /// whether there is one. The item is then in [`slot::ITEM`]. When nothing more comes,
    /// [`slot::PC`] says whether that's because it failed. `known` names the function when
    /// the IR can see which it is, so a backend can call it directly.
    Resume {
        dest: LocalId,
        generator: Operand,
        known: Option<String>,
    },
    /// A direct call, by the callee's key. `dest` is `None` when nothing keeps the result.
    Call {
        dest: Option<LocalId>,
        function: String,
        args: Vec<Operand>,
    },
    /// Waits for a `shared` handle's lock and takes it: to change the cell with `write`, and
    /// otherwise to read it, which other readers may do at the same time.
    Lock {
        handle: Operand,
        write: bool,
    },
    /// Gives back the lock the last [`InstKind::Lock`] of this handle took.
    Unlock {
        handle: Operand,
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
    /// Ends the program: an `assert` that failed, or a `where` rule that a value written
    /// out breaks.
    Stop {
        message: String,
        span: Span,
    },
    /// The checker has shown control never gets here.
    Unreachable,
}

impl InstKind {
    /// The local this instruction gives a new value to, if it does.
    pub fn dest(&self) -> Option<LocalId> {
        match self {
            InstKind::Copy { dest, .. }
            | InstKind::Unary { dest, .. }
            | InstKind::Binary { dest, .. }
            | InstKind::Make { dest, .. }
            | InstKind::GetField { dest, .. }
            | InstKind::MakeCase { dest, .. }
            | InstKind::IsCase { dest, .. }
            | InstKind::GetCaseField { dest, .. }
            | InstKind::MakeList { dest, .. }
            | InstKind::MakeSet { dest, .. }
            | InstKind::MakeDict { dest, .. }
            | InstKind::MakeTuple { dest, .. }
            | InstKind::GetTupleItem { dest, .. }
            | InstKind::GetIndex { dest, .. }
            | InstKind::Slice { dest, .. }
            | InstKind::Format { dest, .. }
            | InstKind::MakeSome { dest, .. }
            | InstKind::IsNone { dest, .. }
            | InstKind::GetSome { dest, .. }
            | InstKind::MakeOk { dest, .. }
            | InstKind::MakeErr { dest, .. }
            | InstKind::IsErr { dest, .. }
            | InstKind::GetOk { dest, .. }
            | InstKind::GetErr { dest, .. }
            | InstKind::IsError { dest, .. }
            | InstKind::AsError { dest, .. }
            | InstKind::MakeClosure { dest, .. }
            | InstKind::MakeGenerator { dest, .. }
            | InstKind::GetSlot { dest, .. }
            | InstKind::Resume { dest, .. }
            | InstKind::Constant { dest, .. } => Some(*dest),
            InstKind::Builtin { dest, .. }
            | InstKind::Call { dest, .. }
            | InstKind::CallClosure { dest, .. } => *dest,
            InstKind::SetField { .. }
            | InstKind::SetIndex { .. }
            | InstKind::SetSlot { .. }
            | InstKind::Lock { .. }
            | InstKind::Unlock { .. } => None,
        }
    }
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
            Terminator::Return(_)
            | Terminator::NoMatch { .. }
            | Terminator::Stop { .. }
            | Terminator::Unreachable => Vec::new(),
        }
    }
}
