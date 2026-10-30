//! Expressions: literals, names, arithmetic with Fennec's checks, comparisons, `and`/`or`
//! and conditional expressions natively, and the conversions between machine values and
//! the runtime's objects. Anything else on objects runs in the interpreter.

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{InstBuilder, Value, types};
use fennec_syntax::{BinOp, Diagnostic, Expr, ExprKind, Span, UnaryOp};

use super::Lowerer;
use crate::helpers::Helper;
use crate::runtime;
use crate::types::{Ty, unsupported};

pub(super) type Lowered = (Option<Value>, Ty);

impl Lowerer<'_, '_> {
    pub(super) fn expr(&mut self, expr: &Expr) -> Result<Lowered, Diagnostic> {
        let span = expr.span;
        match &expr.kind {
            ExprKind::Int(n) => Ok((Some(self.int(*n)), Ty::Int)),
            ExprKind::Float(x) => Ok((Some(self.b.ins().f64const(*x)), Ty::Float)),
            ExprKind::Bool(b) => Ok((Some(self.b.ins().iconst(types::I8, i64::from(*b))), Ty::Bool)),
            ExprKind::Str(text) => Ok(self.literal(text)),
            ExprKind::None => Ok((Some(self.help(Helper::None, &[])?), Ty::Obj)),
            ExprKind::Name(name) => match self.local(name) {
                Some(local) if local.ty == Ty::Obj => {
                    let id = self.name_id(name);
                    Ok((Some(self.help(Helper::Get, &[id])?), Ty::Obj))
                }
                Some(local @ super::Local { ty: Ty::Struct(_), .. }) => {
                    let place = self.local_place(local).ok_or_else(|| unsupported("this struct", span))?;
                    Ok((Some(self.read_place(&place)), local.ty))
                }
                Some(local @ super::Local { ty: Ty::Boxed(_) | Ty::Str, var: Some(var), .. }) => {
                    let value = self.b.use_var(var);
                    Ok((Some(self.borrow(value)), local.ty))
                }
                Some(local) => Ok((local.var.map(|var| self.b.use_var(var)), local.ty)),
                None => self.fallback(expr),
            },
            ExprKind::Unary { op, expr: inner } => {
                let operand = self.expr(inner)?;
                match self.unary(*op, operand, span)? {
                    Some(result) => Ok(result),
                    None => self.object_unary(*op, operand, span),
                }
            }
            ExprKind::Binary { op: op @ (BinOp::And | BinOp::Or), lhs, rhs } => {
                self.logic(*op, lhs, rhs).map(|value| (Some(value), Ty::Bool))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let left = self.expr(lhs)?;
                let right = self.expr(rhs)?;
                self.operate(*op, left, right, span)
            }
            ExprKind::Variant(_) => match self.named_case(expr) {
                Some((id, at)) => match self.make_case(id, at, &[], span)? {
                    Some(made) => Ok(made),
                    None => self.fallback(expr),
                },
                None => self.fallback(expr),
            },
            ExprKind::Cond { then, cond, otherwise } => self.conditional(then, cond, otherwise),
            ExprKind::Else { value, default } => self.else_value(value, default),
            ExprKind::Try { value, context } => self.try_value(value, context.as_deref()),
            ExprKind::Call { callee, args } => self.call(expr, callee, args),
            ExprKind::Match { subject, arms } => self.match_value(subject, arms, span),
            ExprKind::Attr { object, name, optional } => self.read_field(expr, object, name, *optional),
            ExprKind::Index { object, index } => self.read_index_or_packed(expr, object, index),
            ExprKind::Slice { object, start, end } if self.static_ty(object) == Some(Ty::Str) => {
                let (text, _) = self.expr(object)?;
                let text = text.ok_or_else(|| unsupported("this string", span))?;
                let mut bounds = [None, None];
                for (bound, written) in bounds.iter_mut().zip([start, end]) {
                    if let Some(written) = written {
                        let (value, ty) = self.expr(written)?;
                        *bound = Some(self.convert(value, ty, Ty::Int, written.span)?);
                    }
                }
                self.string_slice(text, bounds, span)
            }
            _ => self.fallback(expr),
        }
    }

    /// An expression whose value must be a bool.
    pub(super) fn condition(&mut self, expr: &Expr) -> Result<Value, Diagnostic> {
        let (value, ty) = self.expr(expr)?;
        self.convert(value, ty, Ty::Bool, expr.span)
    }

    /// A value as another kind: a machine value as an object, or an object as a machine
    /// value, failing like the interpreter when it isn't one.
    pub(super) fn convert(
        &mut self,
        value: Option<Value>,
        from: Ty,
        to: Ty,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        if from == to {
            return value.ok_or_else(|| unsupported(&format!("using {} as a value", from.name()), span));
        }
        let handle = self.handle_of(value, from, span)?;
        let unbox = match to {
            Ty::Int => Helper::ToInt,
            Ty::Float => Helper::ToFloat,
            Ty::Bool => Helper::ToBool,
            Ty::Obj => return Ok(handle),
            Ty::Struct(id) => return self.unbox_struct(handle, id, span),
            Ty::Boxed(id) => return self.handle_to_box(handle, id, span),
            Ty::Str => {
                let [start, end] = self.span_args(span);
                return self.help(Helper::StrFromValue, &[handle, start, end]);
            }
            Ty::Unit | Ty::Cap(_) => {
                return Err(unsupported(&format!("using a value as {}", to.name()), span));
            }
        };
        let [start, end] = self.span_args(span);
        self.help(unbox, &[handle, start, end])
    }

    /// A value as an object, for the runtime.
    pub(super) fn handle_of(
        &mut self,
        value: Option<Value>,
        ty: Ty,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let boxing = match ty {
            Ty::Int => Helper::Int,
            Ty::Float => Helper::Float,
            Ty::Bool => Helper::Bool,
            Ty::Obj => return value.ok_or_else(|| unsupported("this value", span)),
            Ty::Unit => return self.help(Helper::Unit, &[]),
            Ty::Struct(id) => {
                let address = value.ok_or_else(|| unsupported("this value", span))?;
                return self.box_struct(address, id, span);
            }
            Ty::Boxed(id) => {
                let address = value.ok_or_else(|| unsupported("this value", span))?;
                return self.box_to_handle(address, id);
            }
            Ty::Str => {
                let address = value.ok_or_else(|| unsupported("this value", span))?;
                return self.box_to_handle(address, 0);
            }
            Ty::Cap(cap) => {
                let kind = self.int(cap.kind());
                return self.help(Helper::Capability, &[kind]);
            }
        };
        let value = value.ok_or_else(|| unsupported("this value", span))?;
        self.help(boxing, &[value])
    }

    fn unary(
        &mut self,
        op: UnaryOp,
        (value, ty): Lowered,
        span: Span,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let Some(value) = value else { return Ok(None) };
        Ok(Some(match (op, ty) {
            (UnaryOp::Neg, Ty::Int) => {
                let zero = self.int(0);
                let (result, overflow) = self.b.ins().ssub_overflow(zero, value);
                self.fail_if(overflow, runtime::OVERFLOW, span);
                (Some(result), Ty::Int)
            }
            (UnaryOp::Neg, Ty::Float) => (Some(self.b.ins().fneg(value)), Ty::Float),
            (UnaryOp::Not, Ty::Bool) => (Some(self.b.ins().bxor_imm_u(value, 1)), Ty::Bool),
            _ => return Ok(None),
        }))
    }

    /// `and` and `or`, which only evaluate their right side when it matters.
    fn logic(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> Result<Value, Diagnostic> {
        let left = self.condition(lhs)?;
        let right_block = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I8);
        if op == BinOp::And {
            self.b.ins().brif(left, right_block, &[], done, &[left.into()]);
        } else {
            self.b.ins().brif(left, done, &[left.into()], right_block, &[]);
        }
        self.b.switch_to_block(right_block);
        let right = self.condition(rhs)?;
        self.b.ins().jump(done, &[right.into()]);
        self.b.switch_to_block(done);
        self.b.block_params(done).first().copied().ok_or_else(|| unsupported("this expression", lhs.span))
    }

    fn conditional(&mut self, then: &Expr, cond: &Expr, otherwise: &Expr) -> Result<Lowered, Diagnostic> {
        let test = self.condition(cond)?;
        let then_block = self.b.create_block();
        let else_block = self.b.create_block();
        self.b.ins().brif(test, then_block, &[], else_block, &[]);
        self.b.switch_to_block(then_block);
        let (value, ty) = self.expr(then)?;
        let mut arms: Vec<_> = self.arm_end(value, ty)?.into_iter().collect();
        self.b.switch_to_block(else_block);
        let (value, ty) = self.expr(otherwise)?;
        arms.extend(self.arm_end(value, ty)?);
        self.join(arms, then.span.to(otherwise.span))
    }

    /// An operator on two lowered values, natively when it can be.
    pub(super) fn operate(
        &mut self,
        op: BinOp,
        left: Lowered,
        right: Lowered,
        span: Span,
    ) -> Result<Lowered, Diagnostic> {
        if let Some(result) = self.binary(op, left, right, span) {
            return Ok(result);
        }
        if (op, left.1, right.1) == (BinOp::Add, Ty::Str, Ty::Str) {
            self.note(span, "the runtime copies both strings to join them");
        }
        if let Some(result) = self.string_binary(op, left, right)? {
            return Ok(result);
        }
        match self.struct_equality(op, left, right)? {
            Some(result) => Ok(result),
            None => self.object_binary(op, left, right, span),
        }
    }

    /// An operator on machine values, or `None` when it needs the interpreter.
    pub(super) fn binary(
        &mut self,
        op: BinOp,
        (a, a_ty): Lowered,
        (b, b_ty): Lowered,
        span: Span,
    ) -> Option<Lowered> {
        let (Some(a), Some(b)) = (a, b) else { return None };
        if a_ty != b_ty {
            return None;
        }
        if let Some(cc) = int_compare(op)
            .filter(|_| a_ty == Ty::Int || (a_ty == Ty::Bool && matches!(op, BinOp::Eq | BinOp::NotEq)))
        {
            return Some((Some(self.b.ins().icmp(cc, a, b)), Ty::Bool));
        }
        if let Some(cc) = float_compare(op).filter(|_| a_ty == Ty::Float) {
            return Some((Some(self.b.ins().fcmp(cc, a, b)), Ty::Bool));
        }
        let result = match a_ty {
            Ty::Int => self.int_arithmetic(op, a, b, span),
            Ty::Float => self.float_arithmetic(op, a, b, span),
            _ => None,
        };
        result.map(|(value, ty)| (Some(value), ty))
    }

    fn int_arithmetic(&mut self, op: BinOp, a: Value, b: Value, span: Span) -> Option<(Value, Ty)> {
        if let (BinOp::Add | BinOp::Sub, Some(constant)) = (op, self.constant(b)) {
            return Some((self.add_constant(op, a, b, constant, span), Ty::Int));
        }
        let checked = match op {
            BinOp::Add => Some(self.b.ins().sadd_overflow(a, b)),
            BinOp::Sub => Some(self.b.ins().ssub_overflow(a, b)),
            BinOp::Mul => Some(self.b.ins().smul_overflow(a, b)),
            _ => None,
        };
        if let Some((result, overflow)) = checked {
            self.fail_if(overflow, runtime::OVERFLOW, span);
            return Some((result, Ty::Int));
        }
        match op {
            BinOp::Div => {
                let zero = self.b.ins().icmp_imm_s(IntCC::Equal, b, 0);
                self.fail_if(zero, runtime::DIVISION_BY_ZERO, span);
                let x = self.b.ins().fcvt_from_sint(types::F64, a);
                let y = self.b.ins().fcvt_from_sint(types::F64, b);
                Some((self.b.ins().fdiv(x, y), Ty::Float))
            }
            BinOp::FloorDiv | BinOp::Mod => Some((self.floor_div_mod(op, a, b, span), Ty::Int)),
            _ => None,
        }
    }

    /// `//` and `%` on ints, rounding toward negative infinity like Python.
    fn floor_div_mod(&mut self, op: BinOp, a: Value, b: Value, span: Span) -> Value {
        let zero = self.b.ins().icmp_imm_s(IntCC::Equal, b, 0);
        self.fail_if(zero, runtime::DIVISION_BY_ZERO, span);
        let minus_one = self.b.ins().icmp_imm_s(IntCC::Equal, b, -1);
        if op == BinOp::FloorDiv {
            let lowest = self.b.ins().icmp_imm_s(IntCC::Equal, a, i64::MIN);
            let overflow = self.b.ins().band(minus_one, lowest);
            self.fail_if(overflow, runtime::OVERFLOW, span);
        }
        // Dividing by -1 can trap on the lowest int, so divide by 1 and negate instead.
        let one = self.int(1);
        let divisor = self.b.ins().select(minus_one, one, b);
        let quotient = self.b.ins().sdiv(a, divisor);
        let negated = self.b.ins().ineg(quotient);
        let quotient = self.b.ins().select(minus_one, negated, quotient);
        let remainder = self.b.ins().srem(a, divisor);
        let nonzero = self.b.ins().icmp_imm_s(IntCC::NotEqual, remainder, 0);
        let signs = self.b.ins().bxor(remainder, b);
        let differ = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, signs, 0);
        let adjust = self.b.ins().band(nonzero, differ);
        if op == BinOp::FloorDiv {
            let lower = self.b.ins().iadd_imm_s(quotient, -1);
            self.b.ins().select(adjust, lower, quotient)
        } else {
            let raised = self.b.ins().iadd(remainder, b);
            self.b.ins().select(adjust, raised, remainder)
        }
    }

    fn float_arithmetic(&mut self, op: BinOp, a: Value, b: Value, span: Span) -> Option<(Value, Ty)> {
        if matches!(op, BinOp::Div | BinOp::FloorDiv | BinOp::Mod) {
            let zero = self.b.ins().f64const(0.0);
            let is_zero = self.b.ins().fcmp(FloatCC::Equal, b, zero);
            self.fail_if(is_zero, runtime::DIVISION_BY_ZERO, span);
        }
        let value = match op {
            BinOp::Add => self.b.ins().fadd(a, b),
            BinOp::Sub => self.b.ins().fsub(a, b),
            BinOp::Mul => self.b.ins().fmul(a, b),
            BinOp::Div => self.b.ins().fdiv(a, b),
            BinOp::FloorDiv => {
                let quotient = self.b.ins().fdiv(a, b);
                self.b.ins().floor(quotient)
            }
            BinOp::Mod => {
                let quotient = self.b.ins().fdiv(a, b);
                let floored = self.b.ins().floor(quotient);
                let product = self.b.ins().fmul(b, floored);
                self.b.ins().fsub(a, product)
            }
            _ => return None,
        };
        Some((value, Ty::Float))
    }
}

impl Lowerer<'_, '_> {
    /// The value of an int known while compiling, like the `1` in `n - 1`.
    pub(super) fn constant(&self, value: Value) -> Option<i64> {
        let inst = self.b.func.dfg.value_def(value).inst()?;
        match self.b.func.dfg.insts[inst] {
            cranelift_codegen::ir::InstructionData::UnaryImm {
                opcode: cranelift_codegen::ir::Opcode::Iconst,
                imm,
            } => Some(imm.bits()),
            _ => None,
        }
    }

    /// `a + c` or `a - c` for a constant `c`. It overflows exactly when `a` is past a bound
    /// known while compiling, a cheaper test than the general one.
    fn add_constant(&mut self, op: BinOp, a: Value, b: Value, constant: i64, span: Span) -> Value {
        let adds_up = (op == BinOp::Add) == (constant >= 0);
        let (cc, bound) = if adds_up {
            (IntCC::SignedGreaterThan, i64::MAX.checked_sub(constant.checked_abs().unwrap_or(i64::MAX)))
        } else {
            (IntCC::SignedLessThan, i64::MIN.checked_add(constant.checked_abs().unwrap_or(i64::MAX)))
        };
        if let Some(bound) = bound {
            let overflow = self.b.ins().icmp_imm_s(cc, a, bound);
            self.fail_if(overflow, runtime::OVERFLOW, span);
            return if op == BinOp::Add { self.b.ins().iadd(a, b) } else { self.b.ins().isub(a, b) };
        }
        // Only `- i64::MIN` lands here; the general check handles it.
        let (result, overflow) = if op == BinOp::Add {
            self.b.ins().sadd_overflow(a, b)
        } else {
            self.b.ins().ssub_overflow(a, b)
        };
        self.fail_if(overflow, runtime::OVERFLOW, span);
        result
    }
}

fn int_compare(op: BinOp) -> Option<IntCC> {
    Some(match op {
        BinOp::Eq => IntCC::Equal,
        BinOp::NotEq => IntCC::NotEqual,
        BinOp::Lt => IntCC::SignedLessThan,
        BinOp::LtEq => IntCC::SignedLessThanOrEqual,
        BinOp::Gt => IntCC::SignedGreaterThan,
        BinOp::GtEq => IntCC::SignedGreaterThanOrEqual,
        _ => return None,
    })
}

fn float_compare(op: BinOp) -> Option<FloatCC> {
    Some(match op {
        BinOp::Eq => FloatCC::Equal,
        BinOp::NotEq => FloatCC::NotEqual,
        BinOp::Lt => FloatCC::LessThan,
        BinOp::LtEq => FloatCC::LessThanOrEqual,
        BinOp::Gt => FloatCC::GreaterThan,
        BinOp::GtEq => FloatCC::GreaterThanOrEqual,
        _ => return None,
    })
}
