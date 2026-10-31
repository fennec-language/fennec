//! Building enum cases, and `match`: each case's pattern is a run of tests that goes to the
//! next case when one fails, then its guard, then its body.

use fennec_check::{Checked, Type};
use fennec_syntax::{Arg, BinOp, Expr, ExprKind, Item, MatchArm, Module, Pattern, Span};

use super::{Lowered, Lowering, Tail};
use crate::{BlockId, CaseDef, EnumDef, InstKind, Operand, Terminator};

/// The module's enums the IR can hold: not generic ones, or ones with `where` rules, which
/// are checked whenever a value is made.
pub(super) fn definitions(modules: &[&Module], checked: &Checked) -> Vec<EnumDef> {
    let mut seen = std::collections::HashSet::new();
    let defs = modules.iter().flat_map(|module| &module.items).filter_map(|item| match item {
        Item::Enum(def) if def.generics.is_empty() => Some(def),
        _ => None,
    });
    defs.filter(|def| seen.insert(def.name.as_str()))
        .filter_map(|def| {
            let found = checked.signatures.enumeration(&def.name).filter(|found| !found.rules)?;
            let cases = found
                .cases
                .iter()
                .map(|(name, fields)| CaseDef { name: name.clone(), fields: fields.clone() })
                .collect();
            Some(EnumDef { name: def.name.clone(), cases })
        })
        .collect()
}

impl Lowering<'_> {
    /// The fields of a case of an enum the IR holds.
    fn case_fields(&self, owner: &str, case: &str) -> Option<Vec<(String, Type)>> {
        let context = self.context;
        if let Some(copy) = context.type_instance(owner) {
            let found = context.checked.signatures.enumeration(&copy.base)?;
            let (_, fields) = found.cases.iter().find(|(name, _)| name == case)?;
            return Some(
                fields.iter().map(|(name, ty)| (name.clone(), context.concrete(ty, &copy.subst))).collect(),
            );
        }
        let def = context.enums.get(owner)?;
        let found = def.cases.iter().find(|written| written.name == case)?;
        let plain = super::generics::Subst::new();
        Some(found.fields.iter().map(|(name, ty)| (name.clone(), context.concrete(ty, &plain))).collect())
    }

    /// The enum, case and arguments when an expression builds an enum case: `.Red`,
    /// `Color.Red`, `.Circle(1.0)` or `Shape.Circle(1.0)`. A leading-dot case's enum is the
    /// type the checker gave it.
    pub(super) fn built_case<'e>(&self, expr: &'e Expr) -> Option<(String, &'e str, &'e [Arg])> {
        let (callee, args) = match &expr.kind {
            ExprKind::Call { callee, args } => (callee.as_ref(), args.as_slice()),
            _ => (expr, &[][..]),
        };
        let (owner, case) = match &callee.kind {
            ExprKind::Variant(case) => match self.type_at(expr.span)? {
                Type::Named(owner) => (owner, case),
                _ => return None,
            },
            ExprKind::Attr { object, name, optional: false } => match &object.kind {
                ExprKind::Name(owner) if self.lookup(owner).is_none() => {
                    // A generic enum's case is of the copy the typed tree says it is.
                    match self.type_at(expr.span) {
                        Some(Type::Named(copy))
                            if self.context.generic_types.contains_key(owner.as_str()) =>
                        {
                            (copy, name)
                        }
                        _ => (owner.clone(), name),
                    }
                }
                _ => return None,
            },
            _ => return None,
        };
        self.case_fields(&owner, case)?;
        Some((owner, case, args))
    }

    pub(super) fn make_case(
        &mut self,
        owner: &str,
        case: &str,
        args: &[Arg],
        span: Span,
    ) -> Lowered<Operand> {
        let fields = self.case_fields(owner, case).unwrap_or_default();
        let fields = self.field_values((owner, Some(case)), &fields, args)?;
        let dest = self.temporary(Type::Named(owner.to_owned()));
        let kind = InstKind::MakeCase { dest, type_name: owner.to_owned(), case: case.to_owned(), fields };
        self.emit(kind, span);
        Ok(Operand::Local(dest))
    }

    /// A `match`, with what becomes of each case's value. Cases are tried in order. A value
    /// that gets past them all ends the program, which the checker rules out for an enum.
    pub(super) fn match_expr(&mut self, subject: &Expr, arms: &[MatchArm], tail: Tail) -> Lowered<()> {
        let ty = self.ty(subject)?;
        let value = self.expr(subject)?;
        let join = self.new_block();
        for arm in arms {
            let next = self.new_block();
            self.scopes.push(std::collections::HashMap::new());
            self.pattern(&arm.pattern, &value, &ty, next, arm.span)?;
            if let Some(guard) = &arm.guard {
                let cond = self.expr(guard)?;
                self.branch_or(cond, next);
            }
            let lowered = self.stmts(&arm.body, tail);
            self.scopes.pop();
            lowered?;
            self.finish(Terminator::Jump(join));
            self.current = next;
        }
        self.finish(Terminator::NoMatch { value, span: subject.span });
        self.current = join;
        Ok(())
    }

    /// Carries on in a new block when `cond` is true, and goes to `fail` when it isn't.
    pub(super) fn branch_or(&mut self, cond: Operand, fail: BlockId) {
        let then = self.new_block();
        self.finish(Terminator::Branch { cond, then, otherwise: fail });
        self.current = then;
    }

    /// Tests a value against a pattern and binds its names, going to `fail` if it doesn't
    /// match and carrying on in the current block if it does.
    pub(super) fn pattern(
        &mut self,
        pattern: &Pattern,
        value: &Operand,
        ty: &Type,
        fail: BlockId,
        span: Span,
    ) -> Lowered<()> {
        match (pattern, ty) {
            (Pattern::Wildcard | Pattern::Or(_), _) | (Pattern::Bind(_), Type::Optional(_)) => {}
            (_, Type::Result(ok, error)) => {
                return self.result_pattern(pattern, value, (ok, error), fail, span);
            }
            (_, Type::Optional(inner)) => {
                return self.optional_pattern(pattern, value, (ty, inner), fail, span);
            }
            _ => {}
        }
        match pattern {
            Pattern::Wildcard => {}
            Pattern::Bind(name) => {
                // Each alternative of an or-pattern binds the same local.
                let bound = self.scopes.last().and_then(|scope| scope.get(name).copied());
                let dest = bound.unwrap_or_else(|| self.declare(name, ty.clone()));
                self.emit(InstKind::Copy { dest, value: value.clone() }, span);
            }
            Pattern::Literal(literal) => {
                let rhs = self.expr(literal)?;
                self.check_operands(BinOp::Eq, value, &rhs)?;
                let dest = self.temporary(Type::Bool);
                self.emit(InstKind::Binary { dest, op: BinOp::Eq, lhs: value.clone(), rhs }, span);
                self.branch_or(Operand::Local(dest), fail);
            }
            Pattern::Or(alternatives) => {
                let matched = self.new_block();
                for alternative in alternatives {
                    let next = self.new_block();
                    self.pattern(alternative, value, ty, next, span)?;
                    self.finish(Terminator::Jump(matched));
                    self.current = next;
                }
                self.finish(Terminator::Jump(fail));
                self.current = matched;
            }
            Pattern::Variant { name, dotted, args } => {
                return self.variant((name, *dotted, args), (value, ty), fail, span);
            }
            Pattern::Tuple(parts) => {
                let Type::Tuple(types) = ty else { return Err(format!("it matches a tuple on {ty}")) };
                for (at, (part, part_ty)) in parts.iter().zip(types).enumerate() {
                    if matches!(part, Pattern::Wildcard) {
                        continue;
                    }
                    let dest = self.temporary(part_ty.clone());
                    self.emit(InstKind::GetTupleItem { dest, value: value.clone(), at }, span);
                    self.pattern(part, &Operand::Local(dest), part_ty, fail, span)?;
                }
            }
        }
        Ok(())
    }

    /// `.Circle(radius)`, testing the case and then its fields, or `Point(x, y)`, which
    /// always matches a `Point` and only tests its fields.
    fn variant(
        &mut self,
        (name, dotted, args): (&str, bool, &[Pattern]),
        (value, ty): (&Operand, &Type),
        fail: BlockId,
        span: Span,
    ) -> Lowered<()> {
        if let Type::Errors(names) = ty
            && names.iter().any(|error| error == name)
        {
            let dest = self.temporary(Type::Bool);
            let value_of = value.clone();
            self.emit(InstKind::IsError { dest, value: value_of, type_name: name.to_owned() }, span);
            self.branch_or(Operand::Local(dest), fail);
            let one = Type::Named(name.to_owned());
            let held = self.temporary(one.clone());
            self.emit(InstKind::AsError { dest: held, value: value.clone() }, span);
            return self.variant((name, dotted, args), (&Operand::Local(held), &one), fail, span);
        }
        let Type::Named(owner) = ty else { return Err(format!("it matches `{name}` on {ty}")) };
        let case = self.case_fields(owner, name);
        let fields = match &case {
            Some(fields) => {
                let dest = self.temporary(Type::Bool);
                let test = InstKind::IsCase { dest, value: value.clone(), case: name.to_owned() };
                self.emit(test, span);
                self.branch_or(Operand::Local(dest), fail);
                fields.clone()
            }
            None if !dotted && name == owner => self.struct_fields(owner)?,
            None => return Err(format!("it matches `{name}` on {ty}")),
        };
        for (arg, (field, field_ty)) in args.iter().zip(&fields) {
            if matches!(arg, Pattern::Wildcard) {
                continue;
            }
            if !self.context.holds(field_ty) {
                return Err(format!("it binds `{name}.{field}`, which is {field_ty}"));
            }
            let dest = self.temporary(field_ty.clone());
            let read = if case.is_some() {
                let case = name.to_owned();
                InstKind::GetCaseField { dest, value: value.clone(), case, field: field.clone() }
            } else {
                InstKind::GetField { dest, object: value.clone(), field: field.clone() }
            };
            self.emit(read, span);
            self.pattern(arg, &Operand::Local(dest), field_ty, fail, span)?;
        }
        Ok(())
    }
}
