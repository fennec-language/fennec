//! Branches that give a value: joining them, `match`, `value else default`, and `value?`.
//! Patterns are tested by the runtime; the arms are compiled natively.

use cranelift_codegen::ir::{Block, InstBuilder};
use fennec_syntax::{Diagnostic, Expr, MatchArm, Pattern, Span};

use super::expr::Lowered;
use super::{Lowerer, abi};
use crate::helpers::Helper;
use crate::types::Ty;

/// A branch that has finished, waiting to be joined with the others.
pub(super) struct Arm {
    block: Block,
    ty: Ty,
}

impl Lowerer<'_, '_> {
    /// Ends a branch with its value, unless it already left (with `return`, say).
    pub(super) fn arm_end(&mut self, value: Option<super::Value>, ty: Ty) -> Result<Option<Arm>, Diagnostic> {
        if self.is_dead() {
            return Ok(None);
        }
        let block = self.b.create_block();
        let ty = if value.is_some() { ty } else { Ty::Unit };
        // A boxed value crosses owned; a struct crosses as its leaves, so it can stay in
        // registers.
        let value = value.map(|value| self.own(value, ty));
        let values = match (value, ty) {
            (Some(value), Ty::Struct(id)) => self.leaves(value, id)?,
            (Some(value), ty) if abi(ty).is_some() => vec![value],
            _ => Vec::new(),
        };
        for value in &values {
            let value_ty = self.b.func.dfg.value_type(*value);
            self.b.append_block_param(block, value_ty);
        }
        let args: Vec<_> = values.into_iter().map(Into::into).collect();
        self.b.ins().jump(block, &args);
        Ok(Some(Arm { block, ty }))
    }

    /// Joins branches into one value. Branches that give the same kind of machine value
    /// stay machine values; mixed ones become objects; any without a value gives nothing.
    pub(super) fn join(&mut self, arms: Vec<Arm>, span: Span) -> Result<Lowered, Diagnostic> {
        let first = arms.first().map_or(Ty::Unit, |arm| arm.ty);
        let joined = if arms.iter().all(|arm| arm.ty == first) {
            first
        } else if arms.iter().any(|arm| arm.ty == Ty::Unit) {
            Ty::Unit
        } else {
            Ty::Obj
        };
        let merge = self.b.create_block();
        let merge_tys = match joined {
            Ty::Struct(id) => {
                let mut tys = Vec::new();
                for (_, leaf) in self.leaf_types(id)? {
                    tys.extend(abi(leaf));
                }
                tys
            }
            ty => abi(ty).into_iter().collect(),
        };
        for ty in &merge_tys {
            self.b.append_block_param(merge, *ty);
        }
        for arm in arms {
            self.b.switch_to_block(arm.block);
            self.b.seal_block(arm.block);
            let params = self.b.block_params(arm.block).to_vec();
            let value = match arm.ty {
                Ty::Struct(_) => Some(self.struct_of_leaves(params)),
                _ => params.first().copied(),
            };
            let values = match joined {
                Ty::Struct(id) => {
                    let converted = self.convert(value, arm.ty, joined, span)?;
                    self.leaves(converted, id)?
                }
                _ if merge_tys.is_empty() => Vec::new(),
                _ => vec![self.convert(value, arm.ty, joined, span)?],
            };
            let args: Vec<_> = values.into_iter().map(Into::into).collect();
            self.b.ins().jump(merge, &args);
        }
        self.b.switch_to_block(merge);
        let params = self.b.block_params(merge).to_vec();
        Ok(match joined {
            Ty::Struct(_) => (Some(self.struct_of_leaves(params)), joined),
            _ => (params.first().copied(), joined),
        })
    }

    pub(super) fn match_value(
        &mut self,
        subject: &Expr,
        arms: &[MatchArm],
        span: Span,
    ) -> Result<Lowered, Diagnostic> {
        let (value, ty) = self.expr(subject)?;
        if matches!(ty, Ty::Int | Ty::Bool | Ty::Struct(_) | Ty::Boxed(_) | Ty::Str) {
            // Arms drop temporaries back to here, keeping the ones made before the match.
            let mark = if self.frame && self.dirty { Some(self.help(Helper::Mark, &[])?) } else { None };
            self.bases.extend(mark);
            let lowered = self.match_native((value, ty), subject, arms);
            if mark.is_some() {
                self.bases.pop();
            }
            if let Some(ends) = lowered? {
                return self.join(ends, span);
            }
        }
        self.note(subject.span, "the runtime matches each pattern in turn");
        let subject_handle = self.handle_of(value, ty, subject.span)?;
        // The arms' statements drop temporaries back to here, keeping the subject.
        let mark = self.help(Helper::Mark, &[])?;
        self.bases.push(mark);
        let lowered = self.match_arms(subject_handle, arms, subject.span);
        self.bases.pop();
        let ends = lowered?;
        self.join(ends, span)
    }

    fn match_arms(
        &mut self,
        subject: super::Value,
        arms: &[MatchArm],
        span: Span,
    ) -> Result<Vec<Arm>, Diagnostic> {
        let mut ends = Vec::new();
        for arm in arms {
            let id = i64::try_from(self.shared.tables.patterns.len()).unwrap_or(0);
            self.shared.tables.patterns.push(arm.pattern.clone());
            let id = self.int(id);
            let [start, end] = self.span_args(arm.span);
            let matched = self.help(Helper::Matches, &[subject, id, start, end])?;
            let then = self.b.create_block();
            let next = self.b.create_block();
            self.b.ins().brif(matched, then, &[], next, &[]);
            self.b.switch_to_block(then);
            self.open_scope();
            let mut bound = Vec::new();
            bindings(&arm.pattern, &mut bound);
            if !bound.is_empty() {
                self.ensure_scope()?;
                self.call_helper(Helper::Bind, &[])?;
                for name in bound {
                    self.bind_object(&name);
                }
            }
            let (value, ty) = self.tail(&arm.body)?;
            self.close_scope()?;
            ends.extend(self.arm_end(value, ty)?);
            self.b.switch_to_block(next);
        }
        let [start, end] = self.span_args(span);
        self.call_helper(Helper::NoMatch, &[subject, start, end])?;
        self.b.ins().jump(self.bail, &[]);
        self.after_terminator();
        Ok(ends)
    }

    /// Records a name the runtime already created in the frame.
    fn bind_object(&mut self, name: &str) {
        if let Some(scope) = self.scopes.last_mut() {
            scope
                .locals
                .insert(name.to_owned(), super::Local { var: None, ty: Ty::Obj, is_mut: false, list: None });
        }
    }

    /// `value else default`: the value inside a result or optional, or else the default.
    pub(super) fn else_value(&mut self, value: &Expr, default: &Expr) -> Result<Lowered, Diagnostic> {
        let (value, ty) = self.expr(value)?;
        if ty != Ty::Obj {
            return Ok((value, ty));
        }
        let handle = self.handle_of(value, ty, default.span)?;
        let passes = self.help(Helper::Passes, &[handle])?;
        let fallback = self.b.create_block();
        let present = self.b.create_block();
        self.b.ins().brif(passes, fallback, &[], present, &[]);
        self.b.switch_to_block(fallback);
        let (default_value, default_ty) = self.expr(default)?;
        let mut arms: Vec<_> = self.arm_end(default_value, default_ty)?.into_iter().collect();
        self.b.switch_to_block(present);
        let inner = self.help(Helper::Unwrap, &[handle])?;
        arms.extend(self.arm_end(Some(inner), Ty::Obj)?);
        self.join(arms, default.span)
    }

    /// `value?`: the value inside a result or optional, or else leave the function with the
    /// failure (wrapped in `context`'s error, given `from`) or None.
    pub(super) fn try_value(&mut self, value: &Expr, context: Option<&Expr>) -> Result<Lowered, Diagnostic> {
        let (lowered, ty) = self.expr(value)?;
        if ty != Ty::Obj {
            return Ok((lowered, ty));
        }
        let handle = self.handle_of(lowered, ty, value.span)?;
        let passes = self.help(Helper::Passes, &[handle])?;
        let up = self.b.create_block();
        let present = self.b.create_block();
        self.b.ins().brif(passes, up, &[], present, &[]);
        self.b.switch_to_block(up);
        let failure = match context {
            Some(context) => {
                let (wrapper, wrapper_ty) = self.expr(context)?;
                let wrapper = self.handle_of(wrapper, wrapper_ty, context.span)?;
                self.help(Helper::WithCause, &[wrapper, handle])?
            }
            None => handle,
        };
        self.pass_up(failure)?;
        self.b.switch_to_block(present);
        let inner = self.help(Helper::Unwrap, &[handle])?;
        Ok((Some(inner), Ty::Obj))
    }
}

/// The names a pattern binds.
fn bindings(pattern: &Pattern, names: &mut Vec<String>) {
    match pattern {
        Pattern::Bind(name) => names.push(name.clone()),
        Pattern::Variant { args, .. } | Pattern::Tuple(args) => {
            for arg in args {
                bindings(arg, names);
            }
        }
        Pattern::Wildcard | Pattern::Literal(_) => {}
    }
}
