//! Calls: to the program's functions and methods, to the standard library's written in
//! Fennec, and to what the engines provide. An argument to a `mut` parameter that's a field
//! is read into a temporary and written back after the call.

use fennec_check::Type;
use fennec_syntax::{Arg, Expr, ExprKind, Span};

use super::places::Place;
use super::{Lowered, Lowering};
use crate::{InstKind, LocalId, Operand, Program};

/// A place to write back after a call, and what holds its value for the call.
type WriteBack = (Place, LocalId);

/// The standard library's functions a program's calls reach, which aren't lowered yet.
pub(super) fn external(program: &Program) -> Vec<String> {
    let mut found: Vec<String> = program
        .functions
        .iter()
        .chain(&program.constants)
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match &inst.kind {
            InstKind::Call { function, .. } | InstKind::MakeClosure { function, .. } => {
                Some(function.clone())
            }
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
pub(super) fn owner_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::List(_) => Some("list"),
        Type::Set(_) => Some("set"),
        Type::Dict(..) => Some("dict"),
        Type::Tuple(_) => Some("tuple"),
        Type::Bytes => Some("bytes"),
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
        if let Some(ty) = self.closure_callee(callee) {
            return self.call_closure(callee, &ty, args, keep, span);
        }
        let mut write_backs = Vec::new();
        let dest;
        if let Some((mut name, object, changes)) = self.builtin(callee) {
            let mut values = Vec::with_capacity(args.len() + 1);
            if let Some(object) = object {
                values.push(if changes {
                    self.place(object, &mut write_backs, span)?
                } else {
                    self.expr(object)?
                });
            }
            let mut key = None;
            for arg in args {
                let is_key = arg.name.as_deref() == Some("key");
                if arg.is_mut || (arg.name.is_some() && !is_key) {
                    return Err(format!("its call to `{name}` has a named or `mut` argument"));
                }
                let value = self.expr(&arg.value)?;
                if is_key { key = Some(value) } else { values.push(value) }
            }
            if let Some(key) = key {
                values.push(key);
                name.push_str(".key");
            }
            dest = keep.map(|ty| self.temporary(ty));
            self.emit(InstKind::Builtin { dest, name, args: values, changes }, span);
        } else {
            let (function, values) = self.direct(callee, args, &mut write_backs, span)?;
            dest = keep.map(|ty| self.temporary(ty));
            self.emit(InstKind::Call { dest, function, args: values }, span);
        }
        for (place, held) in write_backs {
            self.store(place.local, &place.steps, Operand::Local(held), span)?;
        }
        Ok(dest)
    }

    /// Whether a name is a type's rather than a value's, as in `Point.origin()`.
    fn names_a_type(&self, name: &str) -> bool {
        self.lookup(name).is_none() && !self.context.constants.contains_key(name)
    }

    /// What the engines provide that a callee names: its name, its object for a method, and
    /// whether it changes that object.
    fn builtin<'e>(&self, callee: &'e Expr) -> Option<(String, Option<&'e Expr>, bool)> {
        match &callee.kind {
            ExprKind::Name(name) => {
                // What the checker accepted and nothing here defines, like an import of
                // `sqrt`, is the engines'.
                let written = self.context.checked.signatures.get(name).is_some();
                let plain = self.lookup(name).is_none()
                    && !self.context.functions.contains(name.as_str())
                    && (fennec_check::provided_everywhere(name) || !written);
                plain.then(|| (name.clone(), None, false))
            }
            ExprKind::Attr { object, name, optional: false } => {
                if matches!(&object.kind, ExprKind::Name(owner) if self.names_a_type(owner)) {
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
                ExprKind::Name(owner) if self.names_a_type(owner) => (format!("{owner}.{name}"), None),
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
        let signature =
            self.context.checked.signatures.get(&key).ok_or_else(|| format!("it calls `{key}`"))?;
        let given = args.len() + usize::from(object.is_some());
        if given != signature.params.len() {
            return Err(format!("its call to `{key}` isn't a plain call"));
        }
        if own && !signature.params.iter().all(|param| self.context.holds(&param.ty)) {
            return Err(format!("`{key}` takes something the IR doesn't hold yet"));
        }
        let first = usize::from(object.is_some());
        let mut values: Vec<Option<Operand>> = vec![None; given];
        if let Some(object) = object {
            let changed = signature.params.first().is_some_and(|param| param.is_mut);
            let value = if changed { self.place(object, write_backs, span)? } else { self.expr(object)? };
            if let Some(slot) = values.first_mut() {
                *slot = Some(value);
            }
        }
        // Arguments run in the order they're written, and each lands in its parameter.
        for (position, arg) in args.iter().enumerate() {
            let at = match &arg.name {
                Some(name) => signature.params.iter().position(|param| param.name == *name),
                None => Some(position + first),
            };
            let param = at.and_then(|at| signature.params.get(at));
            let param = param.ok_or_else(|| format!("an argument to `{key}` matches no parameter"))?;
            let value = if param.is_mut {
                self.place(&arg.value, write_backs, span)?
            } else {
                let lowered = self.expr(&arg.value)?;
                if own { self.coerce(lowered, &param.ty, arg.value.span)? } else { lowered }
            };
            if let Some(slot) = at.and_then(|at| values.get_mut(at)) {
                *slot = Some(value);
            }
        }
        let values: Option<Vec<Operand>> = values.into_iter().collect();
        Ok((key, values.ok_or("a parameter has no argument")?))
    }

    /// Something a call changes: a local itself, or a temporary holding a field or item
    /// that's written back after the call.
    fn place(&mut self, expr: &Expr, write_backs: &mut Vec<WriteBack>, span: Span) -> Lowered<Operand> {
        let place = self.place_of(expr)?;
        if place.steps.is_empty() {
            return Ok(Operand::Local(place.local));
        }
        let Operand::Local(held) = self.read_steps(place.local, &place.steps, span)? else {
            return Err("it changes something other than a field or item".to_owned());
        };
        write_backs.push((place, held));
        Ok(Operand::Local(held))
    }
}
