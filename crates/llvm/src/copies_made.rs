//! The copies the native code makes that a program's source doesn't ask for. An item or field
//! is read out of where it is, changed, and put back where it was. That's a change in place, as
//! a `for ref` loop, `units[id].hp -= 1`, `heal(ref units[id])` and `mut unit = units[id]` with
//! `units[id] = unit` after all are, so nothing need be copied. The code generator takes the item
//! out when it can show nothing reads the place meanwhile. Where it can't, the item still has
//! its place as an owner while it's changed, so the first change copies it, every time.
//!
//! This is what the code generator itself decided, from the analysis it writes the code from,
//! so `@hot` and `fennec dev copies` can't miss one the way a model of the language could.

use fennec_check::Type;
use fennec_ir::{Function, InstKind, LocalId, Operand, Program};
use fennec_syntax::Span;

use crate::analysis::{Analysis, whole};
use fennec_layout::Layouts;

use crate::kinds::Kind;

/// An item or field the native code copies to change and put back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyMade {
    /// The function it's in, by its name in the IR.
    pub function: String,
    /// Where it's read out.
    pub span: Span,
    /// What it's read out of, as the source names it where it can: `units` or `world.units`.
    pub from: String,
}

/// Every item or field the native code reads out to change and put back and copies on its
/// first change, function by function in the order they're read, each once.
#[must_use]
pub fn copies_made(program: &Program) -> Vec<CopyMade> {
    let everything = whole(program);
    let layouts = crate::layouts_of(program);
    let mut found = Vec::new();
    for function in program.functions.iter().chain(&program.constants) {
        let Some(known) = everything.analyses.get(function.name.as_str()) else { continue };
        for block in &function.blocks {
            for (at, inst) in block.insts.iter().enumerate() {
                let Some((read, from)) = read_out(&inst.kind) else { continue };
                if !held_in_a_block(&layouts, function, read) {
                    continue;
                }
                // The name it's changed under: the local it's read into, or the one that's
                // copied into straight after, as `mut unit = units[id]` is.
                let item = match block.insts.get(at + 1).map(|next| &next.kind) {
                    Some(InstKind::Copy { dest, value: Operand::Local(copied) }) if *copied == read => *dest,
                    _ => read,
                };
                if known.is_taken(read) || known.is_taken(item) {
                    continue;
                }
                if changed(program, function, item) && put_back(function, item) {
                    found.push(CopyMade {
                        function: function.name.clone(),
                        span: inst.span,
                        from: named(function, known, from),
                    });
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    found.retain(|copy| seen.insert((copy.function.clone(), copy.span.start, copy.span.end)));
    found
}

/// What an instruction reads an item or field out of into a local of its own.
fn read_out(inst: &InstKind) -> Option<(LocalId, LocalId)> {
    match inst {
        InstKind::GetIndex { dest, object: Operand::Local(from), .. }
        | InstKind::GetField { dest, object: Operand::Local(from), .. } => Some((*dest, *from)),
        InstKind::Builtin { dest: Some(dest), name, args, .. } if name == "Table.at_slot" => {
            match args.first() {
                Some(Operand::Local(from)) => Some((*dest, *from)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Whether a local's value is held in a block, which a change copies when it's shared, or is
/// a struct or enum kept in place that holds one, whose block a change copies. A struct of plain
/// fields kept in place is copied as it's read, with nothing made, so it isn't one.
fn held_in_a_block(layouts: &Layouts, function: &Function, local: LocalId) -> bool {
    let Some(ty) = function.locals.get(local.0).map(|local| &local.ty) else { return false };
    match Kind::of(layouts, ty) {
        Ok(Kind::Boxed(_) | Kind::List(_) | Kind::Dict(..)) => true,
        Ok(Kind::Struct(id) | Kind::Enum(id)) => layouts.holds_counted(id),
        _ => matches!(ty, Type::Set(_) | Type::Table(_)),
    }
}

/// Whether anything changes what a local holds in place: a field or item set, a builtin that
/// changes it, or a call given it as a `mut` parameter.
fn changed(program: &Program, function: &Function, item: LocalId) -> bool {
    function.blocks.iter().flat_map(|block| &block.insts).any(|inst| match &inst.kind {
        InstKind::SetField { object, .. } | InstKind::SetIndex { object, .. } => *object == item,
        InstKind::Builtin { args, changes: true, .. } => args.first() == Some(&Operand::Local(item)),
        InstKind::Call { function: callee, args, .. } => {
            let Some(callee) = program.functions.iter().find(|candidate| candidate.name == *callee) else {
                return false;
            };
            args.iter()
                .enumerate()
                .any(|(at, arg)| *arg == Operand::Local(item) && callee.mut_params.contains(&at))
        }
        _ => false,
    })
}

/// Whether a local is written back into a field or an item.
fn put_back(function: &Function, item: LocalId) -> bool {
    function.blocks.iter().flat_map(|block| &block.insts).any(|inst| {
        matches!(&inst.kind,
            InstKind::SetIndex { value: Operand::Local(put), .. } | InstKind::SetField { value: Operand::Local(put), .. }
                if *put == item)
    })
}

/// A local's name as the source has it, or the field it was read from, as `world.units`.
fn named(function: &Function, known: &Analysis, local: LocalId) -> String {
    let name = |local: LocalId| function.locals.get(local.0).and_then(|local| local.name.clone());
    if let Some(name) = name(local) {
        return name;
    }
    if let Some((from, field)) = known.field_of(local)
        && let Some(root) = name(*from)
    {
        return format!("{root}.{field}");
    }
    "an item".to_owned()
}
