//! Which list indexes can't be negative, so indexing needs no way back from the end.
//!
//! `xs[i]` with `i` negative counts from the end, so every index is compared with the length, and
//! one that isn't below it goes through a block that adds the length and compares again. Where
//! the index joins back, it's a phi of the two, so the next access with the same index works out
//! its address and its check again. Where `i` is proven not negative, one unsigned comparison is
//! the whole check, as in Rust. A build that checks proofs checks that it isn't negative.
//!
//! An int is known not negative, known to be -1 or more, or not known. Ints get that from
//! constants, lengths, sums and products of what isn't negative, and the like. A struct's field
//! is what every write of it in the program is, a parameter what every call passes, and a return
//! what every `return` gives, worked out over the whole program until nothing changes. A branch
//! on `x != -1` or `x >= 0` makes `x` not negative where it holds, so an index kept as `-1` for
//! none, as `richards` keeps its links, is proven once it's checked against that.
//!
//! What the program doesn't make itself isn't known: a field of a struct a builtin gives back or
//! the runtime passes in, and a parameter of a function a closure, a generator or the runtime may
//! call. A wrong proof would only turn a negative index's count from the end into a failure
//! that it's out of range, as the comparison is still made, unsigned.

use std::collections::{HashMap, HashSet};

use fennec_check::Type;
use fennec_ir::{BlockId, Function, InstKind, LocalId, Operand, Program, Terminator};
use fennec_syntax::{BinOp, UnaryOp};

/// How low an int can be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Floor {
    /// Nothing reaches here yet.
    Unset,
    /// Zero or more.
    NonNeg,
    /// -1 or more: an index, or -1 for none.
    MinusOne,
    /// Anything.
    Any,
}

impl Floor {
    fn join(self, other: Floor) -> Floor {
        self.max(other)
    }

    fn of_int(value: i64) -> Floor {
        match value {
            0.. => Floor::NonNeg,
            -1 => Floor::MinusOne,
            _ => Floor::Any,
        }
    }

    fn non_negative(self) -> bool {
        matches!(self, Floor::Unset | Floor::NonNeg)
    }
}

/// The list and text indexes proven not negative, and the products of two ints proven not
/// negative: each function's, by block and place in it.
pub(crate) type NonNegative = HashMap<String, HashSet<(usize, usize)>>;

/// What's known across the program, worked out until it stops changing.
#[derive(Default, PartialEq)]
struct Facts {
    fields: HashMap<(String, String), Floor>,
    params: HashMap<(String, usize), Floor>,
    returns: HashMap<String, Floor>,
}

/// What can't be known from the program's own code.
struct Outside {
    /// Structs the program doesn't always make itself, so their fields aren't known.
    structs: HashSet<String>,
    /// Functions something other than a call in the program may call.
    callees: HashSet<String>,
    /// Each struct's fields, by name, in order.
    fields: HashMap<String, Vec<(String, Type)>>,
    /// Each constant's value, where it's an int written out.
    constants: HashMap<String, i64>,
    /// The parameters each function changes in its caller's place.
    changes: HashMap<String, Vec<usize>>,
    /// The structs the program makes, whose every value it makes.
    made: HashSet<String>,
    /// The functions the program has, whose every return it sees.
    defined: HashSet<String>,
}

/// The indexes in `program` proven not negative.
pub(crate) fn non_negative(program: &Program) -> NonNegative {
    let outside = outside(program);
    // Each round starts from nothing written and reads what the last one found, so facts only go
    // up, through four states, and this ends where every write, call and return is accounted
    // for. What the program can't see, like a struct a builtin makes or a function the runtime
    // calls, is anything from the start.
    let mut facts = Facts::default();
    loop {
        let mut next = Facts::default();
        for function in program.functions.iter().chain(&program.constants) {
            walk(function, &facts, &outside, &mut next, None);
        }
        if next == facts {
            break;
        }
        facts = next;
    }
    let mut proven = NonNegative::new();
    for function in program.functions.iter().chain(&program.constants) {
        let mut found = HashSet::new();
        walk(function, &facts, &outside, &mut Facts::default(), Some(&mut found));
        if !found.is_empty() {
            proven.insert(function.name.clone(), found);
        }
    }
    proven
}

fn outside(program: &Program) -> Outside {
    let fields: HashMap<String, Vec<(String, Type)>> = program
        .structs
        .iter()
        .chain(&program.errors)
        .map(|def| (def.name.clone(), def.fields.clone()))
        .collect();
    let mut callees: HashSet<String> = HashSet::from(["main".to_owned()]);
    let mut made_outside: Vec<Type> = Vec::new();
    for function in program.functions.iter().chain(&program.constants) {
        for inst in function.blocks.iter().flat_map(|block| &block.insts) {
            match &inst.kind {
                InstKind::MakeClosure { function, .. } => {
                    callees.insert(function.clone());
                }
                InstKind::MakeGenerator { generator, .. } => {
                    callees.insert(generator.clone());
                }
                InstKind::Builtin { dest: Some(dest), .. } => {
                    if let Some(local) = function.locals.get(dest.0) {
                        made_outside.push(local.ty.clone());
                    }
                }
                _ => {}
            }
        }
    }
    for def in &program.generators {
        callees.insert(def.name.clone());
    }
    // A function no call here reaches is one something else calls, like a test.
    let called: HashSet<&str> = program
        .functions
        .iter()
        .chain(&program.constants)
        .flat_map(|function| function.blocks.iter().flat_map(|block| &block.insts))
        .filter_map(|inst| match &inst.kind {
            InstKind::Call { function, .. } => Some(function.as_str()),
            _ => None,
        })
        .collect();
    for function in &program.functions {
        if !called.contains(function.name.as_str()) {
            callees.insert(function.name.clone());
        }
    }
    // What a function others may call is given is made outside too.
    for function in program.functions.iter().filter(|function| callees.contains(&function.name)) {
        made_outside.extend(function.locals.iter().take(function.params).map(|local| local.ty.clone()));
    }
    let mut structs = HashSet::new();
    let mut waiting = made_outside;
    while let Some(ty) = waiting.pop() {
        named_in(&ty, &mut |name| {
            if structs.insert(name.to_owned())
                && let Some(def) = fields.get(name)
            {
                waiting.extend(def.iter().map(|(_, ty)| ty.clone()));
            }
        });
    }
    let constants = program
        .constants
        .iter()
        .filter_map(|function| Some((function.name.clone(), constant_int(function)?)))
        .collect();
    let changes = program
        .functions
        .iter()
        .map(|function| (function.name.clone(), function.mut_params.clone()))
        .collect();
    let made = program
        .functions
        .iter()
        .chain(&program.constants)
        .flat_map(|function| function.blocks.iter().flat_map(|block| &block.insts))
        .filter_map(|inst| match &inst.kind {
            InstKind::Make { type_name, .. } => Some(type_name.clone()),
            _ => None,
        })
        .collect();
    let defined =
        program.functions.iter().chain(&program.constants).map(|function| function.name.clone()).collect();
    Outside { structs, callees, fields, constants, changes, made, defined }
}

/// Calls `found` with each struct `ty` names, however deep.
fn named_in(ty: &Type, found: &mut dyn FnMut(&str)) {
    match ty {
        Type::Named(name) => found(name),
        Type::List(inner)
        | Type::Buffer(inner)
        | Type::Array(inner, _)
        | Type::Set(inner)
        | Type::Iterator(inner)
        | Type::Optional(inner)
        | Type::Id(inner)
        | Type::Table(inner) => named_in(inner, found),
        Type::Dict(key, value) | Type::Result(key, value) => {
            named_in(key, found);
            named_in(value, found);
        }
        Type::Tuple(items) | Type::Union(items) => {
            for item in items {
                named_in(item, found);
            }
        }
        Type::Generic(name, args) => {
            found(name);
            for arg in args {
                named_in(arg, found);
            }
        }
        Type::Arrow(params, returns) => {
            for param in params {
                named_in(param, found);
            }
            named_in(returns, found);
        }
        _ => {}
    }
}

/// A constant's value, when its function only gives back an int written out, or its negative.
fn constant_int(function: &Function) -> Option<i64> {
    let [block] = function.blocks.as_slice() else { return None };
    let mut values: HashMap<LocalId, i64> = HashMap::new();
    for inst in &block.insts {
        match &inst.kind {
            InstKind::Copy { dest, value: Operand::Int(value) } => {
                values.insert(*dest, *value);
            }
            InstKind::Unary { dest, op: UnaryOp::Neg, value: Operand::Int(value) } => {
                values.insert(*dest, value.checked_neg()?);
            }
            _ => return None,
        }
    }
    match &block.end {
        Terminator::Return(Some(Operand::Int(value))) => Some(*value),
        Terminator::Return(Some(Operand::Local(local))) => values.get(local).copied(),
        _ => None,
    }
}

/// Goes over `function` with what's known, `facts`, adding what it writes, passes and gives back
/// to `next`, and, when `found` is given, the places it indexes with what isn't negative.
fn walk(
    function: &Function,
    facts: &Facts,
    outside: &Outside,
    next: &mut Facts,
    mut found: Option<&mut HashSet<(usize, usize)>>,
) {
    let locals = function.locals.len();
    let mut start = vec![Floor::Any; locals];
    let external = outside.callees.contains(&function.name);
    for (at, floor) in start.iter_mut().enumerate().take(function.params) {
        *floor = if external {
            Floor::Any
        } else {
            facts.params.get(&(function.name.clone(), at)).copied().unwrap_or(Floor::Unset)
        };
    }
    let mut coming_in: Vec<Option<Vec<Floor>>> = vec![None; function.blocks.len()];
    if let Some(first) = coming_in.first_mut() {
        *first = Some(start);
    }
    let mut waiting: std::collections::BTreeSet<usize> = std::collections::BTreeSet::from([0]);
    // What's proven is found only once every block's state has settled: a pass before that may see
    // a local as it is on the first turn of a loop, before a later turn makes it negative.
    for settled in [false, true] {
        if settled {
            waiting = (0..function.blocks.len())
                .filter(|at| coming_in.get(*at).is_some_and(Option::is_some))
                .collect();
        }
        while let Some(at) = waiting.pop_first() {
            let (Some(block), Some(Some(entry))) = (function.blocks.get(at), coming_in.get(at)) else {
                continue;
            };
            let mut now = entry.clone();
            // The int each local was set to here, when it was set to one: for a constant compared with.
            let mut exact: HashMap<LocalId, i64> = HashMap::new();
            // What each comparison local compares, for a branch on it.
            let mut compares: HashMap<LocalId, (BinOp, Operand, Operand)> = HashMap::new();
            // Which local each copy here holds the same value as.
            let mut copies: HashMap<LocalId, LocalId> = HashMap::new();
            for (index, inst) in block.insts.iter().enumerate() {
                if settled && let Some(found) = found.as_deref_mut() {
                    let indexed = match &inst.kind {
                        InstKind::GetIndex { object, index: by, .. } if list_of(function, object) => Some(by),
                        InstKind::SetIndex { object, path, index: by, .. } if path.is_empty() => {
                            list_of(function, &Operand::Local(*object)).then_some(by)
                        }
                        _ => None,
                    };
                    if let Some(by) = indexed
                        && floor_of(by, &now).non_negative()
                    {
                        found.insert((at, index));
                    }
                    // A product of two ints that aren't negative overflows only past `i64::MAX`, which
                    // an unsigned multiply finds more cheaply than a signed one.
                    if let InstKind::Binary { op: BinOp::Mul, lhs, rhs, .. } = &inst.kind
                        && floor_of(lhs, &now).non_negative()
                        && floor_of(rhs, &now).non_negative()
                    {
                        found.insert((at, index));
                    }
                }
                step(function, &inst.kind, (facts, outside), next, &mut now, &mut exact);
                // What this writes no longer stands for what it did.
                let mut written = Vec::new();
                crate::analysis::visit(&inst.kind, |_| {}, |local| written.push(local));
                for local in &written {
                    let operand = Operand::Local(*local);
                    compares
                        .retain(|cond, (_, lhs, rhs)| cond != local && *lhs != operand && *rhs != operand);
                    copies.retain(|copy, of| copy != local && of != local);
                }
                match &inst.kind {
                    InstKind::Binary { dest, op, lhs, rhs } => {
                        let constant = |operand: &Operand| match operand {
                            Operand::Local(local) => {
                                exact.get(local).map_or_else(|| operand.clone(), |v| Operand::Int(*v))
                            }
                            _ => operand.clone(),
                        };
                        compares.insert(*dest, (*op, constant(lhs), constant(rhs)));
                    }
                    InstKind::Copy { dest, value: Operand::Local(from) } => {
                        let root = copies.get(from).copied().unwrap_or(*from);
                        copies.insert(*dest, root);
                    }
                    _ => {}
                }
            }
            // The settled pass only reads what's proven: nothing it sees goes on.
            if settled {
                continue;
            }
            let mut send =
                |to: BlockId, state: Vec<Floor>, waiting: &mut std::collections::BTreeSet<usize>| {
                    let Some(theirs) = coming_in.get_mut(to.0) else { return };
                    if let Some(known) = theirs {
                        let joined: Vec<Floor> = known.iter().zip(&state).map(|(a, b)| a.join(*b)).collect();
                        if joined != *known {
                            *known = joined;
                            waiting.insert(to.0);
                        }
                    } else {
                        *theirs = Some(state);
                        waiting.insert(to.0);
                    }
                };
            match &block.end {
                Terminator::Jump(to) => send(*to, now, &mut waiting),
                Terminator::Branch { cond, then, otherwise } => {
                    let (mut on_then, mut on_otherwise) = (now.clone(), now.clone());
                    if let Operand::Local(cond) = cond
                        && let Some((op, lhs, rhs)) = compares.get(cond)
                    {
                        // The local compared, and every local holding the same value, are refined.
                        let compared = [lhs, rhs].into_iter().find_map(|operand| match operand {
                            Operand::Local(local) => Some(*local),
                            _ => None,
                        });
                        let mut same: Vec<LocalId> = compared.into_iter().collect();
                        if let Some(compared) = compared {
                            let root = copies.get(&compared).copied().unwrap_or(compared);
                            same.push(root);
                            same.extend(copies.iter().filter(|(_, of)| **of == root).map(|(copy, _)| *copy));
                        }
                        for local in same {
                            let swap = |operand: &Operand| match operand {
                                Operand::Local(_) => Operand::Local(local),
                                other => other.clone(),
                            };
                            refine(*op, &swap(lhs), &swap(rhs), &mut on_then, &mut on_otherwise);
                        }
                    }
                    send(*then, on_then, &mut waiting);
                    send(*otherwise, on_otherwise, &mut waiting);
                }
                Terminator::Return(Some(value)) => {
                    let returned = floor_of(value, &now);
                    let known = next.returns.entry(function.name.clone()).or_insert(Floor::Unset);
                    *known = known.join(returned);
                }
                Terminator::Return(None)
                | Terminator::NoMatch { .. }
                | Terminator::Stop { .. }
                | Terminator::Unreachable => {}
            }
        }
    }
}

/// Whether `object` is a list, which these indexes are into.
fn list_of(function: &Function, object: &Operand) -> bool {
    let Operand::Local(local) = object else { return false };
    function.locals.get(local.0).is_some_and(|local| matches!(local.ty, Type::List(_)))
}

fn floor_of(operand: &Operand, now: &[Floor]) -> Floor {
    match operand {
        Operand::Int(value) => Floor::of_int(*value),
        Operand::Local(local) => now.get(local.0).copied().unwrap_or(Floor::Any),
        _ => Floor::Any,
    }
}

/// What holds where a branch on `lhs op rhs` goes either way: a local that's -1 or more and
/// compared unequal to -1, or at least 0, isn't negative there.
fn refine(op: BinOp, lhs: &Operand, rhs: &Operand, on_then: &mut [Floor], on_otherwise: &mut [Floor]) {
    let (local, constant, op) = match (lhs, rhs) {
        (Operand::Local(local), Operand::Int(value)) => (*local, *value, op),
        (Operand::Int(value), Operand::Local(local)) => {
            let flipped = match op {
                BinOp::Lt => BinOp::Gt,
                BinOp::LtEq => BinOp::GtEq,
                BinOp::Gt => BinOp::Lt,
                BinOp::GtEq => BinOp::LtEq,
                other => other,
            };
            (*local, *value, flipped)
        }
        _ => return,
    };
    let index_or_none = on_then.get(local.0).is_some_and(|floor| *floor <= Floor::MinusOne);
    // Where the comparison holds, and where it doesn't, the local is at least 0.
    let then_holds = match op {
        BinOp::NotEq => constant == -1 && index_or_none,
        BinOp::Eq | BinOp::GtEq => constant >= 0,
        BinOp::Gt => constant >= -1,
        _ => false,
    };
    let otherwise_holds = match op {
        BinOp::Eq => constant == -1 && index_or_none,
        BinOp::Lt => constant >= 0,
        BinOp::LtEq => constant >= -1,
        _ => false,
    };
    for (holds, state) in [(then_holds, on_then), (otherwise_holds, on_otherwise)] {
        if holds && let Some(floor) = state.get_mut(local.0) {
            *floor = Floor::NonNeg;
        }
    }
}

/// Steps over one instruction: what its dest is, and what it writes, passes and gives.
fn step(
    function: &Function,
    inst: &InstKind,
    (facts, outside): (&Facts, &Outside),
    next: &mut Facts,
    now: &mut [Floor],
    exact: &mut HashMap<LocalId, i64>,
) {
    let is_int =
        |local: LocalId| function.locals.get(local.0).is_some_and(|local| matches!(local.ty, Type::Int));
    let field_floor = |owner: &str, field: &str| -> Floor {
        if outside.structs.contains(owner) || !outside.made.contains(owner) {
            return Floor::Any;
        }
        facts.fields.get(&(owner.to_owned(), field.to_owned())).copied().unwrap_or(Floor::Unset)
    };
    let mut dest_floor: Option<(LocalId, Floor)> = None;
    match inst {
        InstKind::Copy { dest, value } => {
            dest_floor = Some((*dest, floor_of(value, now)));
            match value {
                Operand::Int(value) => {
                    exact.insert(*dest, *value);
                }
                Operand::Local(from) => {
                    if let Some(value) = exact.get(from).copied() {
                        exact.insert(*dest, value);
                    }
                }
                _ => {}
            }
        }
        InstKind::Constant { dest, name } => {
            let value = outside.constants.get(name).copied();
            dest_floor = Some((*dest, value.map_or(Floor::Any, Floor::of_int)));
            if let Some(value) = value {
                exact.insert(*dest, value);
            }
        }
        InstKind::Unary { dest, op, value } => {
            let floor = match (op, value) {
                (UnaryOp::Neg, Operand::Int(value)) => value.checked_neg().map_or(Floor::Any, Floor::of_int),
                _ => Floor::Any,
            };
            dest_floor = Some((*dest, floor));
            if let (UnaryOp::Neg, Operand::Int(value)) = (op, value)
                && let Some(value) = value.checked_neg()
            {
                exact.insert(*dest, value);
            }
        }
        InstKind::Binary { dest, op, lhs, rhs } => {
            let (a, b) = (floor_of(lhs, now), floor_of(rhs, now));
            let positive = |operand: &Operand| matches!(operand, Operand::Int(1..));
            let floor = match op {
                BinOp::Add | BinOp::Mul | BinOp::BitOr | BinOp::BitXor
                    if a.non_negative() && b.non_negative() =>
                {
                    Floor::NonNeg
                }
                BinOp::Add
                    if a <= Floor::MinusOne && b.non_negative()
                        || a.non_negative() && b <= Floor::MinusOne =>
                {
                    Floor::MinusOne
                }
                BinOp::Sub if a.non_negative() && matches!(rhs, Operand::Int(1)) => Floor::MinusOne,
                BinOp::Sub if matches!(rhs, Operand::Int(i64::MIN..=0)) => a,
                BinOp::FloorDiv | BinOp::Mod if a.non_negative() && positive(rhs) => Floor::NonNeg,
                BinOp::BitAnd if a.non_negative() || b.non_negative() => Floor::NonNeg,
                BinOp::ShiftRight if a.non_negative() => Floor::NonNeg,
                _ => Floor::Any,
            };
            dest_floor = Some((*dest, floor));
        }
        InstKind::GetField { dest, object, field } => {
            let owner = match object {
                Operand::Local(local) => function.locals.get(local.0).map(|local| &local.ty),
                _ => None,
            };
            let floor = match owner {
                Some(Type::Named(owner)) => field_floor(owner, field),
                _ => Floor::Any,
            };
            dest_floor = Some((*dest, floor));
        }
        InstKind::Builtin { dest: Some(dest), name, .. } => {
            let length = matches!(name.as_str(), "list.len" | "dict.len" | "set.len" | "str.len");
            dest_floor = Some((*dest, if length { Floor::NonNeg } else { Floor::Any }));
        }
        InstKind::Call { dest, function: callee, args } => {
            for (at, arg) in args.iter().enumerate() {
                let known = next.params.entry((callee.clone(), at)).or_insert(Floor::Unset);
                *known = known.join(floor_of(arg, now));
            }
            // What the callee changes in place isn't known after.
            let changed = outside.changes.get(callee).map_or(&[][..], Vec::as_slice);
            for at in changed {
                if let Some(Operand::Local(local)) = args.get(*at)
                    && let Some(slot) = now.get_mut(local.0)
                {
                    *slot = Floor::Any;
                    exact.remove(local);
                }
            }
            if let Some(dest) = dest {
                let returned = if outside.defined.contains(callee) {
                    facts.returns.get(callee).copied().unwrap_or(Floor::Unset)
                } else {
                    Floor::Any
                };
                dest_floor = Some((*dest, returned));
            }
        }
        InstKind::Make { dest, type_name, fields } => {
            if let Some(def) = outside.fields.get(type_name) {
                for ((name, _), value) in def.iter().zip(fields) {
                    let known = next.fields.entry((type_name.clone(), name.clone())).or_insert(Floor::Unset);
                    *known = known.join(floor_of(value, now));
                }
                // A field the make doesn't give isn't known.
                for (name, _) in def.iter().skip(fields.len()) {
                    next.fields.insert((type_name.clone(), name.clone()), Floor::Any);
                }
            }
            dest_floor = Some((*dest, Floor::Any));
        }
        InstKind::SetField { object, path, value } => {
            let mut owner = function.locals.get(object.0).map(|local| local.ty.clone());
            for (at, field) in path.iter().enumerate() {
                let Some(Type::Named(name)) = owner.clone() else { break };
                if at + 1 == path.len() {
                    let known = next.fields.entry((name.clone(), field.clone())).or_insert(Floor::Unset);
                    *known = known.join(floor_of(value, now));
                }
                owner = outside
                    .fields
                    .get(&name)
                    .and_then(|def| def.iter().find(|(named, _)| named == field))
                    .map(|(_, ty)| ty.clone());
            }
        }
        _ => {
            if let Some(dest) = inst.dest() {
                dest_floor = Some((dest, Floor::Any));
            }
        }
    }
    if let Some((dest, floor)) = dest_floor {
        if let Some(slot) = now.get_mut(dest.0) {
            *slot = if is_int(dest) { floor } else { Floor::Any };
        }
        if !matches!(inst, InstKind::Copy { .. } | InstKind::Constant { .. } | InstKind::Unary { .. }) {
            exact.remove(&dest);
        }
    }
    // Anything else a step writes, like a `mut` argument a call changes, isn't known after.
    let mut written = Vec::new();
    crate::analysis::visit(inst, |_| {}, |local| written.push(local));
    for local in written {
        if dest_floor.is_none_or(|(dest, _)| dest != local) {
            if let Some(slot) = now.get_mut(local.0) {
                *slot = Floor::Any;
            }
            exact.remove(&local);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How many list indexes in `function` are proven not negative.
    fn proven(source: &str, function: &str) -> Result<usize, String> {
        let mut module = fennec_syntax::parse(source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let program = fennec_ir::lower(&module, &checked);
        Ok(non_negative(&program).get(function).map_or(0, HashSet::len))
    }

    const LINKS: &str = "\
NONE = -1

struct Node:
    next: int

def walk(nodes: list[Node], start: int) -> int:
    mut at = start
    mut seen = 0
    while at != NONE:
        seen += 1
        at = nodes[at].next
    return seen

def last_of(nodes: list[Node], at: int) -> int:
    return nodes[at].next

def down(xs: list[int]) -> int:
    mut i = len(xs) - 1
    mut total = 0
    while i >= 0:
        total += xs[i]
        i -= 1
    return total

def down_too_far(xs: list[int]) -> int:
    mut i = len(xs) - 1
    mut total = 0
    while i > -3:
        total += xs[i]
        i -= 1
    return total

def main(stdout: Stdout):
    nodes = [Node(next=1), Node(next=NONE)]
    stdout.print(walk(nodes, start=0), last_of(nodes, at=NONE))
    stdout.print(down([1, 2, 3]), down_too_far([1, 2, 3]))
";

    /// An index kept as -1 for none is proven once it's compared with -1, and one counted down
    /// once it's compared with 0. One that may still be -1, or count past 0, isn't.
    #[test]
    fn an_index_is_proven_only_where_a_check_says_it_isnt_negative() -> Result<(), String> {
        assert_eq!(proven(LINKS, "walk")?, 1);
        assert_eq!(proven(LINKS, "last_of")?, 0);
        assert_eq!(proven(LINKS, "down")?, 1);
        assert_eq!(proven(LINKS, "down_too_far")?, 0);
        Ok(())
    }

    const CURSOR: &str = "\
import std.json as json

struct Cursor:
    at: int

def pick(xs: list[int], cursor: Cursor) -> int:
    return xs[cursor.at]

def main(stdout: Stdout):
    xs = [10, 20, 30]
    stdout.print(pick(xs, Cursor(at=1)))
    READ
";

    /// A field the program only sets to what isn't negative is proven, until something reads the
    /// struct from outside, like `json.loads`, which can put any int in it.
    #[test]
    fn a_field_a_reader_can_fill_isnt_proven() -> Result<(), String> {
        assert_eq!(proven(&CURSOR.replace("READ", ""), "pick")?, 1);
        let read = "stdout.print(pick(xs, json.loads[Cursor](\"{\\\"at\\\": -1}\") else Cursor(at=0)))";
        assert_eq!(proven(&CURSOR.replace("READ", read), "pick")?, 0);
        Ok(())
    }

    /// What a loop makes negative on a later turn isn't proven from its first: a sign that flips
    /// each turn, and an index that starts at 0 and steps down. Proving from the first turn, before
    /// the loop's state settled, gave fannkuch's `sign * total` an unsigned check, and it stopped.
    #[test]
    fn what_a_later_turn_makes_negative_isnt_proven() -> Result<(), String> {
        let flipped = "def f(n: int) -> int:\n    mut sign = 1\n    mut total = 0\n    for i in range(n):\n        total += sign * i\n        sign = -sign\n    return total\n\ndef main(stdout: Stdout):\n    stdout.print(f(5))\n    return\n";
        assert_eq!(proven(flipped, "f")?, 0);
        let stepped = "def g(xs: list[int]) -> int:\n    mut at = 0\n    mut total = 0\n    for _ in range(3):\n        total += xs[at]\n        at -= 1\n    return total\n\ndef main(stdout: Stdout):\n    stdout.print(g([1, 2, 3]))\n    return\n";
        assert_eq!(proven(stepped, "g")?, 0);
        Ok(())
    }

    /// A refinement's rules: where `x` compared with a constant is at least 0.
    #[test]
    fn comparisons_refine_what_they_rule_out() {
        let x = Operand::Local(LocalId(0));
        let cases = [
            (BinOp::NotEq, -1, Floor::MinusOne, (true, false)),
            (BinOp::NotEq, -1, Floor::Any, (false, false)),
            (BinOp::Eq, -1, Floor::MinusOne, (false, true)),
            (BinOp::Eq, 3, Floor::Any, (true, false)),
            (BinOp::GtEq, 0, Floor::Any, (true, false)),
            (BinOp::GtEq, -1, Floor::Any, (false, false)),
            (BinOp::Gt, -1, Floor::Any, (true, false)),
            (BinOp::Lt, 0, Floor::Any, (false, true)),
            (BinOp::Lt, -2, Floor::Any, (false, false)),
            (BinOp::LtEq, -1, Floor::Any, (false, true)),
        ];
        for (op, constant, before, (then, otherwise)) in cases {
            let (mut on_then, mut on_otherwise) = (vec![before], vec![before]);
            refine(op, &x, &Operand::Int(constant), &mut on_then, &mut on_otherwise);
            let got = (on_then == [Floor::NonNeg], on_otherwise == [Floor::NonNeg]);
            assert_eq!(got, (then, otherwise), "{op:?} {constant} from {before:?}");
        }
    }
}
