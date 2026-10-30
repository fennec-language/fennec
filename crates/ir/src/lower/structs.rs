//! Building structs, reading and changing their fields, and calling functions and methods.

use fennec_check::{Checked, Type};
use fennec_syntax::{Arg, BinOp, Expr, ExprKind, Item, Module, Span};

use super::{Lowered, Lowering};
use crate::{InstKind, LocalId, Operand, StructDef, type_in};

/// The module's structs the IR can hold: not generic ones, or ones with `where` rules, which
/// are checked whenever a value is made.
pub(super) fn definitions(module: &Module, checked: &Checked) -> Vec<StructDef> {
    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Struct(def) if def.generics.is_empty() => {
                let found = checked.signatures.structure(&def.name)?;
                let fields = found.fields.clone();
                (!found.rules).then(|| StructDef { name: def.name.clone(), fields })
            }
            _ => None,
        })
        .collect()
}

impl Lowering<'_> {
    fn struct_fields(&self, type_name: &str) -> Lowered<&[(String, Type)]> {
        let found = self.context.checked.signatures.structure(type_name);
        found.map(|found| found.fields.as_slice()).ok_or_else(|| format!("`{type_name}` has no fields"))
    }

    /// The struct a call builds, when the callee is a struct's name.
    pub(super) fn constructs<'e>(&self, callee: &'e Expr) -> Option<&'e str> {
        let ExprKind::Name(name) = &callee.kind else { return None };
        (self.lookup(name).is_none() && self.context.structs.contains(name.as_str())).then_some(name.as_str())
    }

    /// `Point(1, y=2)`: arguments run in the order they're written, and each lands in its
    /// field.
    pub(super) fn make(&mut self, type_name: &str, args: &[Arg], span: Span) -> Lowered<Operand> {
        let fields = self.struct_fields(type_name)?.to_vec();
        let mut values: Vec<Option<Operand>> = vec![None; fields.len()];
        for (position, arg) in args.iter().enumerate() {
            let at = match &arg.name {
                Some(name) => fields.iter().position(|(field, _)| field == name),
                None => Some(position),
            };
            let value = self.expr(&arg.value)?;
            let field = at.and_then(|at| fields.get(at)).ok_or("an argument matches no field")?;
            if type_in(&self.locals, &value).as_ref() != Some(&field.1) {
                return Err(format!("`{type_name}.{}` holds {}", field.0, field.1));
            }
            if let Some(slot) = at.and_then(|at| values.get_mut(at)) {
                *slot = Some(value);
            }
        }
        let fields: Option<Vec<Operand>> = values.into_iter().collect();
        let fields = fields.ok_or_else(|| format!("it leaves a field of `{type_name}` to its default"))?;
        let dest = self.temporary(Type::Named(type_name.to_owned()));
        self.emit(InstKind::Make { dest, type_name: type_name.to_owned(), fields }, span);
        Ok(Operand::Local(dest))
    }

    /// The struct type of an expression, when it's one the IR holds.
    fn struct_of(&self, expr: &Expr) -> Option<String> {
        match self.body.expr(expr.span)? {
            Type::Named(name) if self.context.structs.contains(name.as_str()) => Some(name.clone()),
            _ => None,
        }
    }

    /// `object.field`. A field that's been narrowed, like an optional known not to be
    /// `None`, reads as a different type than it's declared with, which isn't lowered yet.
    pub(super) fn get_field(&mut self, expr: &Expr, object: &Expr, field: &str) -> Lowered<Operand> {
        let ty = self.ty(expr)?;
        let owner = self.struct_of(object).ok_or_else(|| format!("it reads `.{field}` of something else"))?;
        let declared = self.struct_fields(&owner)?.iter().find(|(name, _)| name == field);
        if declared.map(|(_, declared)| declared) != Some(&ty) {
            return Err(format!("it reads `{owner}.{field}` as {ty}"));
        }
        let object = self.expr(object)?;
        let dest = self.temporary(ty);
        self.emit(InstKind::GetField { dest, object, field: field.to_owned() }, expr.span);
        Ok(Operand::Local(dest))
    }

    /// `object.path.to.field = value`, and `+=` and so on, on a struct held in a local.
    pub(super) fn assign_field(
        &mut self,
        target: &Expr,
        op: Option<BinOp>,
        value: &Expr,
        span: Span,
    ) -> Lowered<()> {
        let (object, path) = self.field_place(target)?;
        let mut ty = type_in(&self.locals, &Operand::Local(object));
        let mut read = Operand::Local(object);
        let mut value = self.expr(value)?;
        for field in &path {
            let Some(Type::Named(owner)) = &ty else {
                return Err(format!("it changes `.{field}` of something else"));
            };
            let found = self.struct_fields(owner)?.iter().find(|(name, _)| name == field);
            ty = found.map(|(_, ty)| ty.clone());
            let Some(held) = ty.clone().filter(|held| op.is_some() && self.context.holds(held)) else {
                continue;
            };
            let dest = self.temporary(held);
            self.emit(InstKind::GetField { dest, object: read, field: field.clone() }, span);
            read = Operand::Local(dest);
        }
        if let Some(op) = op {
            self.check_operands(op, &read, &value)?;
            let held = ty.clone().ok_or("it changes a field that doesn't exist")?;
            let dest = self.temporary(held);
            self.emit(InstKind::Binary { dest, op, lhs: read, rhs: value }, span);
            value = Operand::Local(dest);
        }
        if type_in(&self.locals, &value) != ty {
            return Err("it gives a field a value of another type".to_owned());
        }
        self.emit(InstKind::SetField { object, path, value }, span);
        Ok(())
    }

    /// The local and path of fields `a.b.c` names.
    fn field_place(&self, target: &Expr) -> Lowered<(LocalId, Vec<String>)> {
        let other = "it assigns to something other than a name or a field";
        match &target.kind {
            ExprKind::Name(name) => {
                self.lookup(name).map(|local| (local, Vec::new())).ok_or_else(|| other.to_owned())
            }
            ExprKind::Attr { object, name, optional: false } => {
                let (local, mut path) = self.field_place(object)?;
                path.push(name.clone());
                Ok((local, path))
            }
            _ => Err(other.to_owned()),
        }
    }

    /// The callee and arguments of a direct call to a function or method of this program,
    /// with one plain value given for each parameter. A method's object comes first.
    pub(super) fn call_parts(&mut self, callee: &Expr, args: &[Arg]) -> Lowered<(String, Vec<Operand>)> {
        let (key, object) = match &callee.kind {
            ExprKind::Name(name) if self.lookup(name).is_none() => (name.clone(), None),
            ExprKind::Attr { object, name, optional: false } => match &object.kind {
                // `Point.origin()`: a function on the type, with no object.
                ExprKind::Name(owner) if self.lookup(owner).is_none() => (format!("{owner}.{name}"), None),
                _ => {
                    let owner = self
                        .struct_of(object)
                        .ok_or_else(|| format!("it calls `.{name}` on something else"))?;
                    (format!("{owner}.{name}"), Some(object.as_ref()))
                }
            },
            _ => return Err("it calls something other than a function by name".to_owned()),
        };
        if !self.context.functions.contains(key.as_str()) {
            return Err(format!("it calls `{key}`, which isn't a function of this program"));
        }
        let signature = self.context.checked.signatures.get(&key).ok_or("a callee has no signature")?;
        let given = args.len() + usize::from(object.is_some());
        if signature.can_fail || signature.generator || given != signature.params.len() {
            return Err(format!("its call to `{key}` isn't a plain call"));
        }
        if !signature.params.iter().all(|param| !param.is_mut && self.context.holds(&param.ty)) {
            return Err(format!("`{key}` takes something other than plain values"));
        }
        let mut values = Vec::with_capacity(given);
        if let Some(object) = object {
            values.push(self.expr(object)?);
        }
        for arg in args {
            if arg.name.is_some() || arg.is_mut {
                return Err(format!("its call to `{key}` has a named or `mut` argument"));
            }
            values.push(self.expr(&arg.value)?);
        }
        Ok((key, values))
    }
}
