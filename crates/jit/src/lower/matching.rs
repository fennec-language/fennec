//! `match` natively, when its subject is a plain enum and native code can test every
//! pattern: a switch on the case's number, then any nested tests in order, with the
//! fields a pattern names bound straight from the value's words.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{Block, InstBuilder, Value};
use fennec_syntax::{Diagnostic, ExprKind, MatchArm, Pattern, Span};

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
                matches!((&expr.kind, ty), (ExprKind::Int(_), Ty::Int) | (ExprKind::Bool(_), Ty::Bool))
            }
            Pattern::Variant { name, dotted: false, .. } if name == "Ok" || name == "Err" => false,
            Pattern::Variant { name, dotted, args } => {
                let Ty::Struct(id) = ty else { return false };
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
            Pattern::Tuple(_) => false,
        }
    }

    /// A struct or enum field's value, by its position among the fields a pattern names.
    fn pattern_fields(&mut self, name: &str, (value, ty): Lowered) -> Result<Vec<Lowered>, Diagnostic> {
        let span = self.function.def.span;
        let (Ty::Struct(id), Some(value)) = (ty, value) else { return Err(internal("not a struct", span)) };
        let layout = self.layout(id)?;
        let mut fields = Vec::new();
        match &layout.cases {
            Some(cases) => {
                let case = cases.iter().find(|case| case.name == name);
                let case = case.ok_or_else(|| internal("unknown case", span))?;
                let words = self.leaves(value, id)?;
                for field in &case.fields {
                    fields.push(self.case_field(&words, field)?);
                }
            }
            None => {
                for field in &layout.fields {
                    fields.push(self.load_field(value, id, field)?);
                }
            }
        }
        Ok(fields)
    }

    /// Whether a value matches a pattern, or `None` when it always does. With `top`, an
    /// enum's case isn't tested: the switch already has.
    fn test(&mut self, pattern: &Pattern, value: Lowered, top: bool) -> Result<Option<Value>, Diagnostic> {
        match pattern {
            Pattern::Wildcard | Pattern::Bind(_) => Ok(None),
            Pattern::Literal(expr) => {
                let known = match expr.kind {
                    ExprKind::Int(n) => n,
                    ExprKind::Bool(b) => i64::from(b),
                    _ => return Err(internal("an untestable literal", expr.span)),
                };
                let found = value.0.ok_or_else(|| internal("a missing value", expr.span))?;
                Ok(Some(self.b.ins().icmp_imm_s(IntCC::Equal, found, known)))
            }
            Pattern::Variant { name, args, .. } => {
                let mut passes = None;
                if let (Ty::Struct(id), Some(found), false) = (value.1, value.0, top)
                    && let Some(cases) = self.cases(id)?
                {
                    let at = cases.iter().position(|case| case.name == *name).unwrap_or(0);
                    let tag = self.leaves(found, id)?.first().copied();
                    let tag = tag.ok_or_else(|| internal("an enum without words", Span::default()))?;
                    passes = Some(self.b.ins().icmp_imm_s(IntCC::Equal, tag, i64::try_from(at).unwrap_or(0)));
                }
                if args.iter().all(|arg| matches!(arg, Pattern::Wildcard | Pattern::Bind(_))) {
                    return Ok(passes);
                }
                let fields = self.pattern_fields(name, value)?;
                for (arg, field) in args.iter().zip(fields) {
                    if let Some(inner) = self.test(arg, field, false)? {
                        passes = Some(match passes {
                            Some(outer) => self.b.ins().band(outer, inner),
                            None => inner,
                        });
                    }
                }
                Ok(passes)
            }
            Pattern::Tuple(_) => Err(internal("an untestable pattern", Span::default())),
        }
    }

    /// Binds the names a pattern gives the parts of a value that matched it.
    fn bind_pattern(&mut self, pattern: &Pattern, (value, ty): Lowered) -> Result<(), Diagnostic> {
        match pattern {
            Pattern::Bind(name) => {
                if let (Ty::Struct(id), Some(value)) = (ty, value) {
                    return self.declare_struct(name, value, id, false);
                }
                self.bind_native(name, ty, value, false);
                Ok(())
            }
            Pattern::Variant { name, args, .. }
                if args.iter().any(|arg| matches!(arg, Pattern::Bind(_) | Pattern::Variant { .. })) =>
            {
                let fields = self.pattern_fields(name, (value, ty))?;
                for (arg, field) in args.iter().zip(fields) {
                    self.bind_pattern(arg, field)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// `match` natively: on a plain enum, a switch on its case, then each case's arms in
    /// order; on anything else, each arm in order. `None` when some pattern needs the
    /// interpreter.
    pub(super) fn match_native(
        &mut self,
        subject: Lowered,
        arms: &[MatchArm],
        span: Span,
    ) -> Result<Option<Vec<Arm>>, Diagnostic> {
        let (Some(value), ty @ (Ty::Int | Ty::Bool | Ty::Struct(_))) = subject else { return Ok(None) };
        if !arms.iter().all(|arm| self.testable(&arm.pattern, ty)) {
            return Ok(None);
        }
        // Read a struct's leaves once, so every test and arm sees the value as it was.
        let subject = match ty {
            Ty::Struct(id) => {
                let leaves = self.leaves(value, id)?;
                (Some(self.struct_of_leaves(leaves)), ty)
            }
            _ => subject,
        };
        let bodies: Vec<Block> = arms.iter().map(|_| self.b.create_block()).collect();
        let no_match = self.b.create_block();
        let mut falls_through = false;
        let cases = match ty {
            Ty::Struct(id) => self.cases(id)?,
            _ => None,
        };
        if let (Some(cases), Ty::Struct(id), Some(value)) = (cases, ty, subject.0) {
            let tag = self.leaves(value, id)?.first().copied();
            let tag = tag.ok_or_else(|| internal("an enum without words", span))?;
            let entries = self.switch_cases(tag, cases.len());
            for (case, entry) in cases.iter().zip(entries) {
                self.b.switch_to_block(entry);
                self.b.seal_block(entry);
                let fitting = arms.iter().zip(&bodies).filter(|(arm, _)| match &arm.pattern {
                    Pattern::Variant { name, .. } => *name == case.name,
                    _ => true,
                });
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
            self.bind_pattern(&arm.pattern, subject)?;
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
            let Some(passes) = self.test(&arm.pattern, subject, top)? else {
                self.b.ins().jump(*body, &[]);
                return Ok(false);
            };
            let next = self.b.create_block();
            self.b.ins().brif(passes, *body, &[], next, &[]);
            self.b.switch_to_block(next);
            self.b.seal_block(next);
        }
        Ok(true)
    }
}
