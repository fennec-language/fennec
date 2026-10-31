//! Building structs, reading and changing their fields, and calling functions and methods.

use std::collections::{HashMap, HashSet};

use fennec_check::{Checked, Type};
use fennec_syntax::{Arg, Expr, ExprKind, Item, Module, Span};

use super::{Lowered, Lowering};
use crate::{InstKind, LocalId, Operand, StructDef, Terminator, type_in};

/// The module's structs the IR can hold: not generic ones.
pub(super) fn definitions(modules: &[&Module], checked: &Checked) -> Vec<StructDef> {
    let mut seen = HashSet::new();
    let defs = modules.iter().flat_map(|module| &module.items).filter_map(|item| match item {
        Item::Struct(def) if def.generics.is_empty() => Some(def),
        _ => None,
    });
    defs.filter(|def| seen.insert(def.name.as_str()))
        .filter_map(|def| {
            let found = checked.signatures.structure(&def.name)?;
            Some(StructDef { name: def.name.clone(), fields: found.fields.clone() })
        })
        .collect()
}

impl Lowering<'_> {
    pub(super) fn struct_fields(&self, type_name: &str) -> Lowered<&[(String, Type)]> {
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
        let values = self.field_values((type_name, None), &fields, args)?;
        if let Some(rules) = self.context.rules.get(type_name) {
            return self.make_checked(type_name, (&fields, values), rules, span);
        }
        let dest = self.temporary(Type::Named(type_name.to_owned()));
        self.emit(InstKind::Make { dest, type_name: type_name.to_owned(), fields: values }, span);
        Ok(Operand::Local(dest))
    }

    /// A struct with `where` rules. Built from values written out, it's the struct, and
    /// breaking a rule ends the program. Otherwise it's a result, and breaking a rule gives
    /// an `InvalidValue`. Rules run in the order the fields are written, and see the fields.
    fn make_checked(
        &mut self,
        type_name: &str,
        (fields, values): (&[(String, Type)], Vec<Operand>),
        rules: &[(&str, &Expr)],
        span: Span,
    ) -> Lowered<Operand> {
        let result = self.body.expr(span).filter(|ty| matches!(ty, Type::Result(..))).cloned();
        let body = self.context.checked.types.body(&format!("fields of {type_name}"));
        let body = body.ok_or_else(|| format!("the fields of `{type_name}` have no types"))?;
        let dest = result.as_ref().map(|ty| self.temporary(ty.clone()));
        let join = self.new_block();
        let outer_body = std::mem::replace(&mut self.body, body);
        let outer_scopes = std::mem::replace(&mut self.scopes, vec![HashMap::new()]);
        for ((name, ty), value) in fields.iter().zip(&values) {
            let local = self.declare(name, ty.clone());
            self.emit(InstKind::Copy { dest: local, value: value.clone() }, span);
        }
        let mut lowered = Ok(());
        for (field, rule) in rules {
            lowered = lowered.and_then(|()| self.rule(rule, (type_name, field), (dest, join), span));
        }
        self.body = outer_body;
        self.scopes = outer_scopes;
        lowered?;
        let made = self.temporary(Type::Named(type_name.to_owned()));
        self.emit(InstKind::Make { dest: made, type_name: type_name.to_owned(), fields: values }, span);
        let Some(dest) = dest else {
            self.finish(Terminator::Jump(join));
            self.current = join;
            return Ok(Operand::Local(made));
        };
        self.emit(InstKind::MakeOk { dest, value: Some(Operand::Local(made)) }, span);
        self.finish(Terminator::Jump(join));
        self.current = join;
        Ok(Operand::Local(dest))
    }

    /// Tests one `where` rule, carrying on in a new block if it holds.
    fn rule(
        &mut self,
        rule: &Expr,
        (type_name, field): (&str, &str),
        (dest, join): (Option<LocalId>, crate::BlockId),
        span: Span,
    ) -> Lowered<()> {
        let cond = self.expr(rule)?;
        let (then, broken) = (self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond, then, otherwise: broken });
        self.current = broken;
        if let Some(dest) = dest {
            let message = format!("{type_name}.{field} breaks its `where` rule");
            let error = self.temporary(Type::Named("InvalidValue".to_owned()));
            let fields = vec![Operand::Str(message)];
            self.emit(InstKind::Make { dest: error, type_name: "InvalidValue".to_owned(), fields }, span);
            self.emit(InstKind::MakeErr { dest, error: Operand::Local(error) }, span);
            self.finish(Terminator::Jump(join));
        } else {
            let message = format!("this breaks the `where` rule on `{type_name}.{field}`");
            self.finish(Terminator::Stop { message, span });
        }
        self.current = then;
        Ok(())
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
        if let Some(Type::Function(key)) = self.body.expr(expr.span) {
            return self.function_value(&key.clone(), expr);
        }
        if let Some(member) = self.std_member(expr) {
            return self.std_value(member, expr);
        }
        if let ExprKind::Name(owner) = &object.kind
            && self.lookup(owner).is_none()
            && self.context.checked.signatures.get(&format!("{owner}.{field}")).is_some()
        {
            // A function on a type that takes nothing needs no parentheses.
            let dest = self.temporary(self.ty(expr)?);
            let function = format!("{owner}.{field}");
            self.emit(InstKind::Call { dest: Some(dest), function, args: Vec::new() }, expr.span);
            return Ok(Operand::Local(dest));
        }
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

    /// `object?.field`: the field if the object isn't `None`, and `None` if it is.
    pub(super) fn optional_field(&mut self, expr: &Expr, object: &Expr, field: &str) -> Lowered<Operand> {
        let value = self.expr(object)?;
        let Some(Type::Optional(inner)) = type_in(&self.locals, &value) else {
            return Err(format!("it uses `?.{field}` on something other than an optional"));
        };
        let Type::Named(owner) = inner.as_ref() else {
            return Err(format!("it reads `?.{field}` of something else"));
        };
        let found = self.struct_fields(owner)?.iter().find(|(name, _)| name == field);
        let declared =
            found.map(|(_, ty)| ty.clone()).ok_or_else(|| format!("`{owner}` has no `{field}`"))?;
        let ty = self.ty(expr)?;
        let span = expr.span;
        let dest = self.temporary(ty.clone());
        let missing = self.temporary(Type::Bool);
        self.emit(InstKind::IsNone { dest: missing, value: value.clone() }, span);
        let (absent, present, join) = (self.new_block(), self.new_block(), self.new_block());
        let cond = Operand::Local(missing);
        self.finish(Terminator::Branch { cond, then: absent, otherwise: present });
        self.current = absent;
        self.emit(InstKind::Copy { dest, value: Operand::None }, span);
        self.finish(Terminator::Jump(join));
        self.current = present;
        let inside = self.temporary(*inner);
        self.emit(InstKind::GetSome { dest: inside, value }, span);
        let read = self.temporary(declared);
        let object = Operand::Local(inside);
        self.emit(InstKind::GetField { dest: read, object, field: field.to_owned() }, span);
        let read = self.coerce(Operand::Local(read), &ty, span)?;
        self.emit(InstKind::Copy { dest, value: read }, span);
        self.finish(Terminator::Jump(join));
        self.current = join;
        Ok(Operand::Local(dest))
    }

    /// `replace(value, field=new)`: a copy with some fields changed. Rules aren't checked
    /// again.
    pub(super) fn replace(&mut self, args: &[Arg], span: Span) -> Lowered<Operand> {
        let [value, changes @ ..] = args else { return Err("`replace` has no value".to_owned()) };
        let copied = self.expr(&value.value)?;
        let ty = type_in(&self.locals, &copied).ok_or("a value has no type")?;
        let Type::Named(owner) = &ty else { return Err(format!("it replaces fields of {ty}")) };
        let dest = self.temporary(ty.clone());
        self.emit(InstKind::Copy { dest, value: copied }, span);
        for change in changes {
            let field = change.name.clone().ok_or("`replace` needs its fields named")?;
            let found = self.struct_fields(owner)?.iter().find(|(name, _)| *name == field);
            let wanted =
                found.map(|(_, ty)| ty.clone()).ok_or_else(|| format!("`{owner}` has no `{field}`"))?;
            let lowered = self.expr(&change.value)?;
            let value = self.coerce(lowered, &wanted, change.value.span)?;
            self.emit(InstKind::SetField { object: dest, path: vec![field], value }, span);
        }
        Ok(Operand::Local(dest))
    }
}
