//! Building enum cases, and `match`: each case's pattern is a run of tests that goes to the
//! next case when one fails, then its guard, then its body.

use fennec_check::{Checked, Type};
use fennec_syntax::{Arg, BinOp, Expr, ExprKind, Item, MatchArm, Module, Pattern, Span};

use super::{Lowered, Lowering, Tail};
use crate::{BlockId, CaseDef, EnumDef, InstKind, Operand, Terminator};

/// The module's enums the IR can hold: not generic ones, or ones with `where` rules, which
/// are checked whenever a value is made.
pub(super) fn definitions(module: &Module, checked: &Checked) -> Vec<EnumDef> {
    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Enum(def) if def.generics.is_empty() => {
                let found = checked.signatures.enumeration(&def.name).filter(|found| !found.rules)?;
                let cases = found
                    .cases
                    .iter()
                    .map(|(name, fields)| CaseDef { name: name.clone(), fields: fields.clone() })
                    .collect();
                Some(EnumDef { name: def.name.clone(), cases })
            }
            _ => None,
        })
        .collect()
}

impl Lowering<'_> {
    /// The fields of a case of an enum the IR holds.
    fn case_fields(&self, owner: &str, case: &str) -> Option<Vec<(String, Type)>> {
        let def = self.context.enums.get(owner)?;
        def.cases.iter().find(|written| written.name == case).map(|found| found.fields.clone())
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
            ExprKind::Variant(case) => match self.body.expr(expr.span)? {
                Type::Named(owner) => (owner.clone(), case),
                _ => return None,
            },
            ExprKind::Attr { object, name, optional: false } => match &object.kind {
                ExprKind::Name(owner) if self.lookup(owner).is_none() => (owner.clone(), name),
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
        let fields = self.field_values(&format!("{owner}.{case}"), &fields, args)?;
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
    fn branch_or(&mut self, cond: Operand, fail: BlockId) {
        let then = self.new_block();
        self.finish(Terminator::Branch { cond, then, otherwise: fail });
        self.current = then;
    }

    /// Tests a value against a pattern and binds its names, going to `fail` if it doesn't
    /// match and carrying on in the current block if it does.
    fn pattern(
        &mut self,
        pattern: &Pattern,
        value: &Operand,
        ty: &Type,
        fail: BlockId,
        span: Span,
    ) -> Lowered<()> {
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
            Pattern::Tuple(_) => return Err("it matches a tuple".to_owned()),
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
            None if !dotted && name == owner => {
                let found = self.context.checked.signatures.structure(owner);
                found.map(|found| found.fields.clone()).ok_or_else(|| format!("`{owner}` has no fields"))?
            }
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
