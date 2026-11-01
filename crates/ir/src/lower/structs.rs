//! Building structs, reading and changing their fields, and calling functions and methods.

use std::collections::{HashMap, HashSet};

use fennec_check::{Signatures, Type};
use fennec_syntax::{Arg, Expr, ExprKind, Item, Module, Span};

use super::places::Step;
use super::{Context, Lowered, Lowering};
use crate::{InstKind, LocalId, Operand, StructDef, Terminator, type_in};

/// The module's structs the IR can hold: not generic ones.
pub(super) fn definitions(modules: &[&Module], signatures: &Signatures) -> Vec<StructDef> {
    let mut seen = HashSet::new();
    let defs = modules.iter().flat_map(|module| &module.items).filter_map(|item| match item {
        Item::Struct(def) if def.generics.is_empty() => Some(def),
        _ => None,
    });
    defs.filter(|def| seen.insert(def.name.as_str()))
        .filter_map(|def| {
            let found = signatures.structure(&def.name)?;
            Some(StructDef { name: def.name.clone(), fields: found.fields.clone() })
        })
        .collect()
}

/// Adds the copies of generic types a program uses, whose fields can ask for more.
pub(super) fn add_copies(
    context: &Context<'_>,
    structs: &mut Vec<StructDef>,
    enums: &mut Vec<crate::EnumDef>,
) {
    let signatures = &context.signatures;
    let concrete = |fields: &[(String, Type)], subst: &super::generics::Subst| -> Vec<(String, Type)> {
        fields.iter().map(|(name, ty)| (name.clone(), context.concrete(ty, subst))).collect()
    };
    for def in structs.iter_mut() {
        def.fields = concrete(&def.fields, &super::generics::Subst::new());
    }
    for def in enums.iter_mut() {
        for case in &mut def.cases {
            case.fields = concrete(&case.fields, &super::generics::Subst::new());
        }
    }
    let mut done = HashSet::new();
    loop {
        let copies: Vec<(String, super::generics::TypeInstance)> = context
            .instances
            .types
            .borrow()
            .iter()
            .map(|(name, copy)| (name.clone(), copy.clone()))
            .collect();
        let waiting: Vec<_> = copies.into_iter().filter(|(name, _)| done.insert(name.clone())).collect();
        if waiting.is_empty() {
            return;
        }
        for (name, copy) in waiting {
            if let Some(found) = signatures.structure(&copy.base) {
                structs.push(StructDef { name, fields: concrete(&found.fields, &copy.subst) });
            } else if let Some(found) = signatures.enumeration(&copy.base) {
                let cases = found
                    .cases
                    .iter()
                    .map(|(case, fields)| crate::CaseDef {
                        name: case.clone(),
                        fields: concrete(fields, &copy.subst),
                    })
                    .collect();
                enums.push(crate::EnumDef { name, cases });
            }
        }
    }
}

impl Lowering<'_> {
    /// The fields of a struct or error type, with their types as they are for this copy.
    pub(super) fn struct_fields(&self, type_name: &str) -> Lowered<Vec<(String, Type)>> {
        let context = self.context;
        let instance = context.type_instance(type_name);
        let base = instance.as_ref().map_or(type_name, |instance| instance.base.as_str());
        let subst = instance.as_ref().map(|instance| instance.subst.clone()).unwrap_or_default();
        let signatures = &context.signatures;
        let found = signatures.structure(base).or_else(|| signatures.error_type(base));
        let found = found.ok_or_else(|| format!("`{type_name}` has no fields"))?;
        Ok(found.fields.iter().map(|(name, ty)| (name.clone(), context.concrete(ty, &subst))).collect())
    }

    /// The struct a call builds, when the callee is a struct's name. A generic struct is the
    /// copy the typed tree says it is.
    pub(super) fn constructs(&self, callee: &Expr, span: Span) -> Option<String> {
        let callee = match &callee.kind {
            ExprKind::Index { object, .. } => object,
            _ => callee,
        };
        let member = self.std_member(callee);
        let name = match &callee.kind {
            ExprKind::Name(name) if self.lookup(name).is_none() => name.as_str(),
            ExprKind::Attr { .. } => member.as_deref()?,
            _ => return None,
        };
        if self.context.structs.contains(name) || self.context.errors.contains(name) {
            return Some(name.to_owned());
        }
        self.context.generic_types.get(name)?;
        match self.type_at(span)? {
            Type::Named(copy) => Some(copy),
            Type::Result(ok, _) => match *ok {
                Type::Named(copy) => Some(copy),
                _ => None,
            },
            _ => None,
        }
    }

    /// `Point(1, y=2)`: arguments run in the order they're written, and each lands in its
    /// field.
    pub(super) fn make(&mut self, type_name: &str, args: &[Arg], span: Span) -> Lowered<Operand> {
        let fields = self.struct_fields(type_name)?;
        let values = self.field_values((type_name, None), &fields, args)?;
        let base = self.context.type_instance(type_name).map_or(type_name.to_owned(), |copy| copy.base);
        if let Some(rules) = self.context.rules.get(base.as_str()) {
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
        let result = self.type_at(span).filter(|ty| matches!(ty, Type::Result(..)));
        let instance = self.context.type_instance(type_name);
        let base = instance.as_ref().map_or(type_name, |copy| copy.base.as_str());
        let body = self.context.signatures.types().body(&format!("fields of {base}"));
        let body = body.ok_or_else(|| format!("the fields of `{type_name}` have no types"))?;
        let dest = result.as_ref().map(|ty| self.temporary(ty.clone()));
        let join = self.new_block();
        let outer_body = std::mem::replace(&mut self.body, body);
        let outer_scopes = std::mem::replace(&mut self.scopes, vec![HashMap::new()]);
        let copy_of = instance.map(|copy| copy.subst).unwrap_or_default();
        let outer_subst = std::mem::replace(&mut self.subst, copy_of);
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
        self.subst = outer_subst;
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
        match self.type_at(expr.span)? {
            Type::Named(name)
                if self.context.structs.contains(name.as_str())
                    || self.context.errors.contains(name.as_str())
                    || self.context.type_instance(&name).is_some() =>
            {
                Some(name)
            }
            _ => None,
        }
    }

    /// `object.field`. A field that's been narrowed, like an optional known not to be
    /// `None`, reads as a different type than it's declared with, which isn't lowered yet.
    pub(super) fn get_field(&mut self, expr: &Expr, object: &Expr, field: &str) -> Lowered<Operand> {
        if let Some(Type::Function(key)) = self.type_at(expr.span) {
            return self.function_value(&key, expr);
        }
        if let Some(member) = self.std_member(expr) {
            return self.std_value(member, expr);
        }
        if let ExprKind::Name(owner) = &object.kind
            && self.lookup(owner).is_none()
            && self.context.signatures.get(&format!("{owner}.{field}")).is_some()
        {
            // A function on a type that takes nothing needs no parentheses.
            let dest = self.temporary(self.ty(expr)?);
            let function = format!("{owner}.{field}");
            self.emit(InstKind::Call { dest: Some(dest), function, args: Vec::new() }, expr.span);
            return Ok(Operand::Local(dest));
        }
        let owner = self.struct_of(object).ok_or_else(|| format!("it reads `.{field}` of something else"))?;
        let fields = self.struct_fields(&owner)?;
        let declared = fields.iter().find(|(name, _)| name == field);
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
        let fields = self.struct_fields(owner)?;
        let found = fields.iter().find(|(name, _)| name == field);
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

    /// `replace(value, field=new)`: a copy with some fields changed, whose rules run.
    pub(super) fn replace(&mut self, args: &[Arg], span: Span) -> Lowered<Operand> {
        let [value, changes @ ..] = args else { return Err("`replace` has no value".to_owned()) };
        let copied = self.expr(&value.value)?;
        let ty = type_in(&self.locals, &copied).ok_or("a value has no type")?;
        let Type::Named(owner) = &ty else { return Err(format!("it replaces fields of {ty}")) };
        let dest = self.temporary(ty.clone());
        self.emit(InstKind::Copy { dest, value: copied }, span);
        for change in changes {
            let field = change.name.clone().ok_or("`replace` needs its fields named")?;
            let fields = self.struct_fields(owner)?;
            let found = fields.iter().find(|(name, _)| *name == field);
            let wanted =
                found.map(|(_, ty)| ty.clone()).ok_or_else(|| format!("`{owner}` has no `{field}`"))?;
            let lowered = self.expr(&change.value)?;
            let value = self.coerce(lowered, &wanted, change.value.span)?;
            self.store(dest, &[Step::Field(field)], value, span)?;
        }
        Ok(Operand::Local(dest))
    }
}
