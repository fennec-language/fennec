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
        let signatures = &self.context.checked.signatures;
        let found = signatures.structure(type_name).or_else(|| signatures.error_type(type_name));
        found.map(|found| found.fields.as_slice()).ok_or_else(|| format!("`{type_name}` has no fields"))
    }

    /// The struct a call builds, when the callee is a struct's name.
    pub(super) fn constructs<'e>(&self, callee: &'e Expr) -> Option<&'e str> {
        let ExprKind::Name(name) = &callee.kind else { return None };
        let named =
            self.context.structs.contains(name.as_str()) || self.context.errors.contains(name.as_str());
        (self.lookup(name).is_none() && named).then_some(name.as_str())
    }

    /// `Point(1, y=2)`: arguments run in the order they're written, and each lands in its
    /// field.
    pub(super) fn make(&mut self, type_name: &str, args: &[Arg], span: Span) -> Lowered<Operand> {
        let fields = self.struct_fields(type_name)?.to_vec();
        let fields = self.field_values((type_name, None), &fields, args)?;
        let dest = self.temporary(Type::Named(type_name.to_owned()));
        self.emit(InstKind::Make { dest, type_name: type_name.to_owned(), fields }, span);
        Ok(Operand::Local(dest))
    }

    /// A value for each field: arguments run in the order they're written, and each lands
    /// in its field. Then the fields left out get their defaults, in the order they're
    /// written. `owner` names the type being built, and its case.
    pub(super) fn field_values(
        &mut self,
        owner: (&str, Option<&str>),
        fields: &[(String, Type)],
        args: &[Arg],
    ) -> Lowered<Vec<Operand>> {
        let mut values: Vec<Option<Operand>> = vec![None; fields.len()];
        for (position, arg) in args.iter().enumerate() {
            let at = match &arg.name {
                Some(name) => fields.iter().position(|(field, _)| field == name),
                None => Some(position),
            };
            let value = self.expr(&arg.value)?;
            let field = at.and_then(|at| fields.get(at)).ok_or("an argument matches no field")?;
            let value = self.coerce(value, &field.1, arg.value.span)?;
            if let Some(slot) = at.and_then(|at| values.get_mut(at)) {
                *slot = Some(value);
            }
        }
        let mut filled = Vec::with_capacity(fields.len());
        for (value, (name, ty)) in values.into_iter().zip(fields) {
            filled.push(match value {
                Some(value) => value,
                None => self.field_default(owner, name, ty)?,
            });
        }
        Ok(filled)
    }

    /// The struct type of an expression, when it's one the IR holds.
    fn struct_of(&self, expr: &Expr) -> Option<String> {
        match self.body.expr(expr.span)? {
            Type::Named(name)
                if self.context.structs.contains(name.as_str())
                    || self.context.errors.contains(name.as_str()) =>
            {
                Some(name.clone())
            }
            _ => None,
        }
    }

    /// `object.field`. A field that's been narrowed, like an optional known not to be
    /// `None`, reads as a different type than it's declared with, which isn't lowered yet.
    pub(super) fn get_field(&mut self, expr: &Expr, object: &Expr, field: &str) -> Lowered<Operand> {
        let owner = self.struct_of(object).ok_or_else(|| format!("it reads `.{field}` of something else"))?;
        let declared = self.struct_fields(&owner)?.iter().find(|(name, _)| name == field);
        let declared =
            declared.map(|(_, ty)| ty.clone()).ok_or_else(|| format!("`{owner}` has no `{field}`"))?;
        if !self.context.holds(&declared) {
            return Err(format!("it reads `{owner}.{field}`, which is {declared}"));
        }
        let object = self.expr(object)?;
        let dest = self.temporary(declared);
        self.emit(InstKind::GetField { dest, object, field: field.to_owned() }, expr.span);
        self.narrowed(Operand::Local(dest), expr)
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
            let held = ty.clone().ok_or("it changes a field that doesn't exist")?;
            value = self.operate(op, (read, value), &held, span)?;
        }
        let wanted = ty.ok_or("it changes a field that doesn't exist")?;
        let value = self.coerce(value, &wanted, span)?;
        self.emit(InstKind::SetField { object, path, value }, span);
        Ok(())
    }

    /// The local and path of fields `a.b.c` names.
    pub(super) fn field_place(&self, target: &Expr) -> Lowered<(LocalId, Vec<String>)> {
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
}
