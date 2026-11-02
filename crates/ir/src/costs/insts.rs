//! What each instruction reads and writes.

use crate::{FormatPart, InstKind, LocalId, Operand, Terminator};

/// The local an instruction gives its result to, if any.
pub(super) fn dest(inst: &InstKind) -> Option<LocalId> {
    match inst {
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
        InstKind::Builtin { dest, .. } | InstKind::CallClosure { dest, .. } | InstKind::Call { dest, .. } => {
            *dest
        }
        InstKind::SetField { .. }
        | InstKind::SetIndex { .. }
        | InstKind::SetSlot { .. }
        | InstKind::Lock { .. }
        | InstKind::Unlock { .. } => None,
    }
}

/// The values an instruction reads.
pub(super) fn operands(inst: &InstKind) -> Vec<&Operand> {
    match inst {
        InstKind::Copy { value, .. }
        | InstKind::Unary { value, .. }
        | InstKind::IsCase { value, .. }
        | InstKind::GetCaseField { value, .. }
        | InstKind::GetTupleItem { value, .. }
        | InstKind::SetField { value, .. }
        | InstKind::MakeSome { value, .. }
        | InstKind::IsNone { value, .. }
        | InstKind::GetSome { value, .. }
        | InstKind::IsErr { value, .. }
        | InstKind::GetOk { value, .. }
        | InstKind::GetErr { value, .. }
        | InstKind::IsError { value, .. }
        | InstKind::AsError { value, .. } => vec![value],
        InstKind::MakeErr { error, .. } => vec![error],
        InstKind::Binary { lhs, rhs, .. } => vec![lhs, rhs],
        InstKind::GetField { object, .. } => vec![object],
        InstKind::GetIndex { object, index, .. } => vec![object, index],
        InstKind::SetIndex { index, value, .. } => vec![index, value],
        InstKind::Slice { object, start, end, .. } => {
            std::iter::once(object).chain(start).chain(end).collect()
        }
        InstKind::Make { fields: items, .. }
        | InstKind::MakeCase { fields: items, .. }
        | InstKind::MakeList { items, .. }
        | InstKind::MakeSet { items, .. }
        | InstKind::MakeTuple { items, .. }
        | InstKind::MakeClosure { captures: items, .. }
        | InstKind::Builtin { args: items, .. }
        | InstKind::Call { args: items, .. } => items.iter().collect(),
        InstKind::MakeDict { pairs, .. } => pairs.iter().flat_map(|(key, value)| [key, value]).collect(),
        InstKind::Format { parts, .. } => parts
            .iter()
            .filter_map(|part| match part {
                FormatPart::Value { value, .. } => Some(value),
                FormatPart::Text(_) => None,
            })
            .collect(),
        InstKind::MakeOk { value, .. } => value.iter().collect(),
        InstKind::CallClosure { closure, args, .. } => std::iter::once(closure).chain(args).collect(),
        InstKind::Constant { .. } => Vec::new(),
        InstKind::MakeGenerator { args, .. } => args.iter().map(|(_, value)| value).collect(),
        InstKind::GetSlot { generator, .. } | InstKind::Resume { generator, .. } => vec![generator],
        InstKind::SetSlot { generator, value, .. } => vec![generator, value],
        InstKind::Lock { handle, .. } | InstKind::Unlock { handle } => vec![handle],
    }
}

/// The locals an instruction needs the value of.
pub(crate) fn reads(inst: &InstKind) -> Vec<LocalId> {
    let mut read: Vec<LocalId> = operands(inst).into_iter().filter_map(local).collect();
    if let InstKind::SetField { object, .. } | InstKind::SetIndex { object, .. } = inst {
        read.push(*object);
    }
    read
}

pub(super) fn local(operand: &Operand) -> Option<LocalId> {
    match operand {
        Operand::Local(id) => Some(*id),
        _ => None,
    }
}

/// The locals a block's end needs the value of.
pub(crate) fn terminator_reads(end: &Terminator) -> Vec<LocalId> {
    let value = match end {
        Terminator::Branch { cond, .. } => Some(cond),
        Terminator::Return(value) => value.as_ref(),
        Terminator::NoMatch { value, .. } => Some(value),
        Terminator::Jump(_) | Terminator::Stop { .. } | Terminator::Unreachable => None,
    };
    value.and_then(local).into_iter().collect()
}
