//! Calls: to the program's functions and methods, to the standard library's written in
//! Fennec, and to what the engines provide. An argument to a `mut` parameter that's a field
//! is read into a temporary and written back after the call.

use fennec_check::Type;
use fennec_syntax::{Arg, Expr, ExprKind, Span};

use super::{Lowered, Lowering};
use crate::{InstKind, LocalId, Operand, Program};

/// A field to write back after a call: the local, the path to the field, and what holds it.
type WriteBack = (LocalId, Vec<String>, LocalId);

/// The standard library's functions a program's calls reach, which aren't lowered yet.
pub(super) fn external(program: &Program) -> Vec<String> {
    let mut found: Vec<String> = program
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match &inst.kind {
            InstKind::Call { function, .. } => Some(function.clone()),
            _ => None,
        })
        .filter(|name| {
            !program.functions.iter().any(|function| function.name == *name)
                && !program.not_lowered.iter().any(|skipped| skipped.function == *name)
        })
        .collect();
    found.sort();
    found.dedup();
    found
}

/// The name methods of a built-in type are keyed under.
fn owner_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::List(_) => Some("list"),
        Type::Str => Some("str"),
        Type::Int => Some("int"),
        Type::Float => Some("float"),
        Type::Bool => Some("bool"),
        Type::Named(name) => Some(name),
        _ => None,
    }
}

impl Lowering<'_> {
    /// Lowers a call. When it's given the type of what the call gives back, that's kept in a
    /// new temporary.
    pub(super) fn call(
        &mut self,
        callee: &Expr,
        args: &[Arg],
        keep: Option<Type>,
        span: Span,
    ) -> Lowered<Option<LocalId>> {
        let mut write_backs = Vec::new();
        let dest;
        if let Some((name, object, changes)) = self.builtin(callee) {
            let mut values = Vec::with_capacity(args.len() + 1);
            if let Some(object) = object {
                values.push(if changes {
                    self.place(object, &mut write_backs, span)?
                } else {
                    self.expr(object)?
                });
            }
            for arg in args {
                if arg.name.is_some() || arg.is_mut {
                    return Err(format!("its call to `{name}` has a named or `mut` argument"));
                }
                values.push(self.expr(&arg.value)?);
            }
            dest = keep.map(|ty| self.temporary(ty));
            self.emit(InstKind::Builtin { dest, name, args: values, changes }, span);
        } else {
            let (function, values) = self.direct(callee, args, &mut write_backs, span)?;
            dest = keep.map(|ty| self.temporary(ty));
            self.emit(InstKind::Call { dest, function, args: values }, span);
        }
        for (object, path, held) in write_backs {
            self.emit(InstKind::SetField { object, path, value: Operand::Local(held) }, span);
        }
        Ok(dest)
    }

    /// What the engines provide that a callee names: its name, its object for a method, and
    /// whether it changes that object.
    fn builtin<'e>(&self, callee: &'e Expr) -> Option<(String, Option<&'e Expr>, bool)> {
        match &callee.kind {
            ExprKind::Name(name) => {
                let plain = self.lookup(name).is_none()
                    && !self.context.functions.contains(name.as_str())
                    && fennec_check::provided_everywhere(name);
                plain.then(|| (name.clone(), None, false))
            }
            ExprKind::Attr { object, name, optional: false } => {
                if matches!(&object.kind, ExprKind::Name(owner) if self.lookup(owner).is_none()) {
                    return None;
                }
                let ty = self.body.expr(object.span)?;
                let changes = fennec_check::builtin_method_changes(ty, name)?;
                let owner = owner_name(ty)?;
                let written = self.context.checked.signatures.get(&format!("{owner}.{name}")).is_some();
                (!written).then(|| (format!("{owner}.{name}"), Some(object.as_ref()), changes))
            }
            _ => None,
        }
    }

    /// The key and arguments of a direct call, with one value given for each parameter. A
    /// method's object comes first.
    fn direct(
        &mut self,
        callee: &Expr,
        args: &[Arg],
        write_backs: &mut Vec<WriteBack>,
        span: Span,
    ) -> Lowered<(String, Vec<Operand>)> {
        let (key, object) = match &callee.kind {
            ExprKind::Name(name) if self.lookup(name).is_none() => (name.clone(), None),
            ExprKind::Attr { object, name, optional: false } => match &object.kind {
                // `Point.origin()`: a function on the type, with no object.
                ExprKind::Name(owner) if self.lookup(owner).is_none() => (format!("{owner}.{name}"), None),
                _ => {
                    let ty = self.body.expr(object.span);
                    let owner = ty
                        .filter(|ty| self.context.holds(ty))
                        .and_then(owner_name)
                        .ok_or_else(|| format!("it calls `.{name}` on something else"))?;
                    (format!("{owner}.{name}"), Some(object.as_ref()))
                }
            },
            _ => return Err("it calls something other than a function by name".to_owned()),
        };
        let own = self.context.functions.contains(key.as_str());
        if !own && object.is_none() {
            return Err(format!("it calls `{key}`, which isn't a function of this program"));
        }
        let signature =
            self.context.checked.signatures.get(&key).ok_or_else(|| format!("it calls `{key}`"))?;
        let given = args.len() + usize::from(object.is_some());
        if signature.can_fail || signature.generator || given != signature.params.len() {
            return Err(format!("its call to `{key}` isn't a plain call"));
        }
        if own && !signature.params.iter().all(|param| self.context.holds(&param.ty)) {
            return Err(format!("`{key}` takes something the IR doesn't hold yet"));
        }
        let changes: Vec<bool> = signature.params.iter().map(|param| param.is_mut).collect();
        let mut values = Vec::with_capacity(given);
        let written = object.into_iter().chain(args.iter().map(|arg| &arg.value));
        for (at, value) in written.enumerate() {
            values.push(if changes.get(at) == Some(&true) {
                self.place(value, write_backs, span)?
            } else {
                self.expr(value)?
            });
        }
        if args.iter().any(|arg| arg.name.is_some()) {
            return Err(format!("its call to `{key}` has a named argument"));
        }
        Ok((key, values))
    }

    /// Something a call changes: a local itself, or a temporary holding a field that's
    /// written back after the call.
    fn place(&mut self, expr: &Expr, write_backs: &mut Vec<WriteBack>, span: Span) -> Lowered<Operand> {
        let (object, path) = self.field_place(expr)?;
        if path.is_empty() {
            return Ok(Operand::Local(object));
        }
        let Operand::Local(held) = self.read_place(object, &path, span)? else {
            return Err("it changes something other than a field".to_owned());
        };
        write_backs.push((object, path, held));
        Ok(Operand::Local(held))
    }
}
