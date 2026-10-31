//! `match` natively, when its subject is a plain enum and native code can test every
//! pattern: a switch on the case's number, then any nested tests in order, with the
//! fields a pattern names bound straight from the value's words.

use cranelift_codegen::entity::EntityRef;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{Block, InstBuilder, MemFlagsData, Value, types};
use fennec_syntax::{Diagnostic, Expr, ExprKind, MatchArm, Pattern};

use super::expr::Lowered;
use super::flow::Arm;
use super::{Lowerer, internal};
use crate::helpers::Helper;
use crate::types::Ty;

impl Lowerer<'_, '_> {
    /// Whether native code can test a pattern against a value of this kind, the way the
    /// interpreter would.
    fn testable(&self, pattern: &Pattern, ty: Ty) -> bool {
        match pattern {
            Pattern::Wildcard | Pattern::Bind(_) => true,
            Pattern::Literal(expr) => {
                matches!(
                    (&expr.kind, ty),
                    (ExprKind::Int(_), Ty::Int) | (ExprKind::Bool(_), Ty::Bool) | (ExprKind::Str(_), Ty::Str)
                )
            }
            Pattern::Variant { name, dotted: false, .. } if name == "Ok" || name == "Err" => false,
            Pattern::Variant { name, dotted, args } => {
                let (Ty::Struct(id) | Ty::Boxed(id)) = ty else { return false };
                let Ok(layout) = self.layout(id) else { return false };
                let fields = match &layout.cases {
                    Some(cases) => match cases.iter().find(|case| case.name == *name) {
                        Some(case) => &case.fields,
                        None => return false,
                    },
                    None if !dotted && layout.name == *name => &layout.fields,
                    None => return false,
                };
                args.len() <= fields.len()
                    && args.iter().zip(fields).all(|(arg, field)| self.testable(arg, field.ty))
            }
            // Alternatives that bind names go to the interpreter, which keeps track of which matched.
            Pattern::Or(alternatives) => alternatives.iter().all(|alternative| {
                let mut names = Vec::new();
                bound_names(alternative, &mut names);
                names.is_empty() && self.testable(alternative, ty)
            }),
            Pattern::Tuple(_) => false,
        }
    }

    /// The fields of a struct or enum case a pattern names, in order. A boxed value's
    /// boxed fields come back as they're stored, with no owner of their own.
    fn pattern_fields(&mut self, name: &str, (value, ty): Lowered) -> Result<Vec<Lowered>, Diagnostic> {
        let span = self.function.def.span;
        let (Ty::Struct(id) | Ty::Boxed(id), Some(value)) = (ty, value) else {
            return Err(internal("not a struct", span));
        };
        let layout = self.layout(id)?;
        let fields = match &layout.cases {
            Some(cases) => {
                &cases
                    .iter()
                    .find(|case| case.name == name)
                    .ok_or_else(|| internal("unknown case", span))?
                    .fields
            }
            None => &layout.fields,
        };
        let mut found = Vec::new();
        if let (Ty::Struct(_), None) = (ty, &layout.cases) {
            for field in fields {
                found.push(self.load_field(value, id, field)?);
            }
            return Ok(found);
        }
        let words = if layout.boxed { self.block_words(value, id)? } else { self.leaves(value, id)? };
        for field in fields {
            found.push(self.case_field(&words, field)?);
        }
        Ok(found)
    }

    /// An enum value's case number.
    fn tag_of(&mut self, value: Value, ty: Ty) -> Result<Value, Diagnostic> {
        let span = self.function.def.span;
        match ty {
            Ty::Boxed(_) => Ok(self.b.ins().load(types::I64, MemFlagsData::trusted(), value, 16)),
            Ty::Struct(id) => self
                .leaves(value, id)?
                .first()
                .copied()
                .ok_or_else(|| internal("an enum without words", span)),
            _ => Err(internal("not an enum", span)),
        }
    }

    /// Tests a value against a pattern, jumping to `fail` when it doesn't match and going on
    /// where it does. With `top`, an enum's case is already known. Gives back whether it
    /// tested anything. A field is read only once the case holding it is known.
    fn test_into(
        &mut self,
        pattern: &Pattern,
        value: Lowered,
        top: bool,
        fail: Block,
    ) -> Result<bool, Diagnostic> {
        let mut tested = false;
        match pattern {
            Pattern::Wildcard | Pattern::Bind(_) | Pattern::Tuple(_) => {}
            Pattern::Or(alternatives) => {
                let passed = self.b.create_block();
                for alternative in alternatives {
                    let next = self.b.create_block();
                    self.test_into(alternative, value, false, next)?;
                    self.b.ins().jump(passed, &[]);
                    self.b.switch_to_block(next);
                    self.b.seal_block(next);
                }
                self.b.ins().jump(fail, &[]);
                self.b.switch_to_block(passed);
                self.b.seal_block(passed);
                tested = true;
            }
            Pattern::Literal(expr) => {
                let found = value.0.ok_or_else(|| internal("a missing value", expr.span))?;
                let passes = match &expr.kind {
                    ExprKind::Int(n) => self.b.ins().icmp_imm_s(IntCC::Equal, found, *n),
                    ExprKind::Bool(b) => self.b.ins().icmp_imm_s(IntCC::Equal, found, i64::from(*b)),
                    ExprKind::Str(text) => self.equals_literal(found, text)?,
                    _ => return Err(internal("an untestable literal", expr.span)),
                };
                self.pass_or(passes, fail);
                tested = true;
            }
            Pattern::Variant { name, args, .. } => {
                if let (Ty::Struct(id) | Ty::Boxed(id), Some(found), false) = (value.1, value.0, top)
                    && let Some(cases) = self.cases(id)?
                {
                    let at = cases.iter().position(|case| case.name == *name).unwrap_or(0);
                    let tag = self.tag_of(found, value.1)?;
                    let passes = self.b.ins().icmp_imm_s(IntCC::Equal, tag, i64::try_from(at).unwrap_or(0));
                    self.pass_or(passes, fail);
                    tested = true;
                }
                if args.iter().any(|arg| !matches!(arg, Pattern::Wildcard | Pattern::Bind(_))) {
                    let fields = self.pattern_fields(name, value)?;
                    for (arg, field) in args.iter().zip(fields) {
                        tested |= self.test_into(arg, field, false, fail)?;
                    }
                }
            }
        }
        Ok(tested)
    }

    /// Goes on when `passes`, else jumps to `fail`.
    fn pass_or(&mut self, passes: Value, fail: Block) {
        let next = self.b.create_block();
        self.b.ins().brif(passes, next, &[], fail, &[]);
        self.b.switch_to_block(next);
        self.b.seal_block(next);
    }

    /// Binds the names a pattern gives the parts of a value that matched it. A boxed part
    /// is borrowed from the subject when `borrows`, and otherwise gets an owner, the name.
    fn bind_pattern(
        &mut self,
        pattern: &Pattern,
        (value, ty): Lowered,
        borrows: bool,
    ) -> Result<(), Diagnostic> {
        match pattern {
            Pattern::Bind(name) => {
                if let (Ty::Struct(id), Some(value)) = (ty, value) {
                    return self.declare_struct(name, value, id, false);
                }
                if let (true, Some(value), false) = (ty.counted(), value, borrows) {
                    self.retain(value);
                }
                self.bind_native(name, ty, value, false);
                if let (true, true, Some(local)) = (ty.counted(), borrows, self.local(name)) {
                    self.borrowed_names.extend(local.var.map(EntityRef::index));
                }
                Ok(())
            }
            Pattern::Variant { name, args, .. }
                if args.iter().any(|arg| matches!(arg, Pattern::Bind(_) | Pattern::Variant { .. })) =>
            {
                let fields = self.pattern_fields(name, (value, ty))?;
                for (arg, field) in args.iter().zip(fields) {
                    self.bind_pattern(arg, field, borrows)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Whether an expression reads a name that can't change, or a field of one.
    fn fixed_root(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Name(name) => self.local(name).is_some_and(|local| !local.is_mut),
            ExprKind::Attr { object, .. } => self.fixed_root(object),
            _ => false,
        }
    }

    /// `match` natively: on a plain enum, a switch on its case, then each case's arms in
    /// order; on anything else, each arm in order. `None` when some pattern needs the
    /// interpreter.
    pub(super) fn match_native(
        &mut self,
        subject: Lowered,
        written: &Expr,
        arms: &[MatchArm],
    ) -> Result<Option<Vec<Arm>>, Diagnostic> {
        let span = written.span;
        let (Some(value), ty @ (Ty::Int | Ty::Bool | Ty::Struct(_) | Ty::Boxed(_) | Ty::Str)) = subject
        else {
            return Ok(None);
        };
        if !arms.iter().all(|arm| self.testable(&arm.pattern, ty)) {
            return Ok(None);
        }
        // A boxed subject read from a name that can't change stays alive through the arms,
        // which borrow its fields. Any other is owned here, and let go once an arm has
        // taken what it binds.
        let borrows = !ty.counted() || (self.borrowed.contains(&value) && self.fixed_root(written));
        // Read a struct's leaves once, so every test and arm sees the value as it was.
        let subject = match ty {
            Ty::Struct(id) => {
                let leaves = self.leaves(value, id)?;
                (Some(self.struct_of_leaves(leaves)), ty)
            }
            ty if ty.counted() && !borrows => (Some(self.own(value, ty)), ty),
            _ => subject,
        };
        let bodies: Vec<Block> = arms.iter().map(|_| self.b.create_block()).collect();
        let no_match = self.b.create_block();
        let mut falls_through = false;
        let cases = match ty {
            Ty::Struct(id) | Ty::Boxed(id) => self.cases(id)?,
            _ => None,
        };
        if let (Some(cases), Some(value)) = (cases, subject.0) {
            let tag = self.tag_of(value, ty)?;
            let entries = self.switch_cases(tag, cases.len());
            for (case, entry) in cases.iter().zip(entries) {
                self.b.switch_to_block(entry);
                self.b.seal_block(entry);
                let fitting = arms.iter().zip(&bodies).filter(|(arm, _)| fits_case(&arm.pattern, &case.name));
                if self.try_arms(fitting, subject, true)? {
                    self.b.ins().jump(no_match, &[]);
                    falls_through = true;
                }
            }
        } else if self.try_arms(arms.iter().zip(&bodies), subject, false)? {
            self.b.ins().jump(no_match, &[]);
            falls_through = true;
        }
        let mut ends = Vec::new();
        for (arm, body) in arms.iter().zip(bodies) {
            self.b.switch_to_block(body);
            self.b.seal_block(body);
            self.open_scope();
            self.bind_pattern(&arm.pattern, subject, borrows)?;
            if !borrows {
                self.consume(subject.0, subject.1)?;
            }
            let (value, ty) = self.tail(&arm.body)?;
            self.close_scope()?;
            ends.extend(self.arm_end(value, ty)?);
        }
        self.b.switch_to_block(no_match);
        self.b.seal_block(no_match);
        if falls_through {
            // Nothing matched: fail like the interpreter.
            self.b.set_cold_block(no_match);
            let handle = self.handle_of(subject.0, subject.1, span)?;
            let [start, end] = self.span_args(span);
            self.call_helper(Helper::NoMatch, &[handle, start, end])?;
        }
        self.b.ins().jump(self.bail, &[]);
        Ok(Some(ends))
    }

    /// Tests arms in order, jumping to the body of the first that matches. With `top`, an
    /// enum's case is already known. Gives back whether no arm surely matches, leaving the
    /// current block for when none did.
    fn try_arms<'m>(
        &mut self,
        arms: impl Iterator<Item = (&'m MatchArm, &'m Block)>,
        subject: Lowered,
        top: bool,
    ) -> Result<bool, Diagnostic> {
        for (arm, body) in arms {
            let next = self.b.create_block();
            let tested = self.test_into(&arm.pattern, subject, top, next)?;
            self.b.ins().jump(*body, &[]);
            if !tested {
                return Ok(false);
            }
            self.b.switch_to_block(next);
            self.b.seal_block(next);
        }
        Ok(true)
    }
}

/// Whether a pattern could match an enum's case.
fn fits_case(pattern: &Pattern, case: &str) -> bool {
    match pattern {
        Pattern::Variant { name, .. } => name == case,
        Pattern::Or(alternatives) => alternatives.iter().any(|alternative| fits_case(alternative, case)),
        _ => true,
    }
}

/// The names a pattern binds.
fn bound_names(pattern: &Pattern, names: &mut Vec<String>) {
    match pattern {
        Pattern::Bind(name) => names.push(name.clone()),
        Pattern::Variant { args: parts, .. } | Pattern::Tuple(parts) | Pattern::Or(parts) => {
            for part in parts {
                bound_names(part, names);
            }
        }
        Pattern::Wildcard | Pattern::Literal(_) => {}
    }
}
