//! What a program may cost at run time that its source doesn't spell out: where a large value
//! may be copied because something still refers to it, and where memory may be allocated. A
//! game engine has to be able to show that its hot paths do neither.
//!
//! These are findings for tools: `fennec dev copies` prints them, and a lint can read them. It's
//! worked out from the typed IR, where each allocation and each change is its own instruction
//! and generics are gone. Everything here is what *may* happen: a short string fits in its
//! header, and a list with room doesn't grow.

mod flow;
mod insts;
mod loops;

use std::collections::{HashMap, HashSet};

use fennec_check::Type;
use fennec_syntax::{BinOp, Span};

use crate::{Function, InstKind, Program};

/// What may cost something at one place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cost {
    /// Changing `value` may copy it, since something else may still read it: `why` says what.
    Copy { value: String, why: Shared },
    /// `what` may allocate.
    Allocation { what: String },
}

/// Why a value being changed may still be shared, so changing it copies it first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shared {
    /// Another name holds it, and reads it again at `later`.
    HeldBy { name: String, later: Option<Span> },
    /// It was put into `container`, which is read again at `later`.
    PutInto { container: String, later: Option<Span> },
    /// It came from the caller, who still holds it.
    Caller,
}

/// A place in a program that may copy or allocate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The function it's in, by its key: `name`, or `Type.name`.
    pub function: String,
    pub span: Span,
    pub cost: Cost,
    /// How many loops it's inside in its own function.
    pub loops: usize,
    /// A function that calls this one from inside a loop, however many calls away, if any does.
    pub called_from_loop: Option<String>,
}

impl Finding {
    /// Whether it runs inside a loop, here or further up the calls.
    pub fn is_hot(&self) -> bool {
        self.loops > 0 || self.called_from_loop.is_some()
    }

    /// Whether it happens each time round a loop: one it's in, or one that calls its function,
    /// however many calls away.
    pub fn per_iteration(&self) -> bool {
        self.is_hot()
    }

    /// What it costs, in a few words, without where it is.
    pub fn message(&self) -> String {
        match &self.cost {
            Cost::Copy { value, why: Shared::HeldBy { name, .. } } => {
                format!("`{value}` is copied here, since `{name}` still holds it and is used again")
            }
            Cost::Copy { value, why: Shared::PutInto { container, .. } } => {
                format!("`{value}` is copied here, since it was put into `{container}`, which is used again")
            }
            Cost::Copy { value, why: Shared::Caller } => {
                format!("`{value}` is copied here, since the caller still holds it")
            }
            Cost::Allocation { what } => format!("{what} may allocate"),
        }
    }
}

/// Everything in a lowered program's own code that may copy or allocate, hot code first: code
/// in the most loops, then code called from a loop. What the standard library does counts at
/// the call to it, since that's what a program can change.
pub fn costs(program: &Program) -> Vec<Finding> {
    let library_allocates = library_allocating(program);
    let by_name: HashMap<&str, &Function> = program.functions.iter().map(|f| (f.name.as_str(), f)).collect();
    let callers = loops::callers_in_loops(program);
    let mut findings = Vec::new();
    for function in program.functions.iter().filter(|function| is_own(function)) {
        let depths = loops::depths(function);
        let mut found: Vec<(Span, Cost, usize)> = flow::copies(function, &by_name, &depths);
        for (block, depth) in function.blocks.iter().zip(&depths) {
            for inst in &block.insts {
                let what = allocation(function, &inst.kind, &library_allocates);
                found.extend(what.map(|what| (inst.span, Cost::Allocation { what }, *depth)));
            }
        }
        findings.extend(found.into_iter().filter(|(span, ..)| !span.in_prelude()).map(
            |(span, cost, loops)| {
                let called_from_loop = callers.get(function.name.as_str()).map(|caller| (*caller).to_owned());
                Finding { function: function.name.clone(), span, cost, loops, called_from_loop }
            },
        ));
    }
    findings.sort_by_key(|finding| {
        (std::cmp::Reverse(finding.loops), finding.called_from_loop.is_none(), finding.span.start)
    });
    findings.dedup();
    findings
}

/// Whether a function is the program's rather than the standard library's. A copy of a generic
/// is the program's where the generic is.
fn is_own(function: &Function) -> bool {
    !function.blocks.iter().flat_map(|block| &block.insts).any(|inst| inst.span.in_prelude())
}

/// Every instruction of a function.
fn insts(function: &Function) -> impl Iterator<Item = &InstKind> {
    function.blocks.iter().flat_map(|block| &block.insts).map(|inst| &inst.kind)
}

/// The standard library's functions that may allocate, or call one that does.
fn library_allocating(program: &Program) -> HashSet<&str> {
    let none = HashSet::new();
    let library: Vec<&Function> = program.functions.iter().filter(|function| !is_own(function)).collect();
    let mut found: HashSet<&str> = HashSet::new();
    for function in &library {
        if insts(function).any(|kind| allocation(function, kind, &none).is_some()) {
            found.insert(&function.name);
        }
    }
    loop {
        let before = found.len();
        for function in &library {
            let calls_one = insts(function).any(|kind| match kind {
                InstKind::Call { function: callee, .. } => found.contains(callee.as_str()),
                _ => false,
            });
            if calls_one {
                found.insert(&function.name);
            }
        }
        if found.len() == before {
            return found;
        }
    }
}

/// Whether a value keeps something on the heap.
fn on_the_heap(ty: &Type) -> bool {
    matches!(ty, Type::Str | Type::Bytes | Type::List(_) | Type::Set(_) | Type::Dict(..))
}

/// The methods that may make a collection bigger.
const GROWS: [&str; 7] = ["append", "insert", "extend", "add", "update", "set", "push"];

/// What an instruction may allocate, in a few words.
fn allocation(function: &Function, inst: &InstKind, library: &HashSet<&str>) -> Option<String> {
    let of = |id: &crate::LocalId| function.locals.get(id.0).map(|local| &local.ty);
    match inst {
        InstKind::MakeList { .. } => Some("this list".to_owned()),
        InstKind::MakeSet { .. } => Some("this set".to_owned()),
        InstKind::MakeDict { .. } => Some("this dict".to_owned()),
        InstKind::MakeTuple { .. } => Some("this tuple".to_owned()),
        InstKind::Format { .. } => Some("this f-string".to_owned()),
        InstKind::MakeClosure { captures, .. } if !captures.is_empty() => Some("this closure".to_owned()),
        InstKind::MakeCase { type_name, case, fields, .. } if !fields.is_empty() => {
            Some(format!("`{type_name}.{case}`"))
        }
        InstKind::Binary { dest, op: BinOp::Add, .. } if of(dest).is_some_and(on_the_heap) => {
            Some(format!("joining these to make a {}", of(dest)?))
        }
        InstKind::Slice { dest, .. } if matches!(of(dest), Some(Type::List(_))) => {
            Some("this slice of a list".to_owned())
        }
        InstKind::Builtin { name, changes: true, .. }
            if GROWS.contains(&name.rsplit('.').next().unwrap_or(name)) =>
        {
            Some(format!("growing it with `{name}`"))
        }
        InstKind::Builtin { dest: Some(dest), name, .. } if of(dest).is_some_and(on_the_heap) => {
            Some(format!("what `{name}` gives back"))
        }
        InstKind::Call { function: callee, .. } if library.contains(callee.as_str()) => {
            Some(format!("the call to `{callee}`"))
        }
        _ => None,
    }
}

pub(crate) use insts::{reads as inst_reads, terminator_reads};
