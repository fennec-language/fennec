//! Expressions: literals, names, arithmetic with Fennec's checks, comparisons, `and`/`or`,
//! conditional expressions, calls, a few builtins, and `stdout.print`.

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{InstBuilder, Value, types};
use fennec_syntax::{Arg, BinOp, Diagnostic, Expr, ExprKind, FStrPart, Span, UnaryOp};

use cranelift_module::Module;

use super::{Lowerer, abi};
use crate::runtime;
use crate::types::{Ty, unsupported};

impl Lowerer<'_, '_> {
    pub(super) fn expr(&mut self, expr: &Expr) -> Result<(Option<Value>, Ty), Diagnostic> {
        let span = expr.span;
        match &expr.kind {
            ExprKind::Int(n) => Ok((Some(self.b.ins().iconst(types::I64, *n)), Ty::Int)),
            ExprKind::Float(x) => Ok((Some(self.b.ins().f64const(*x)), Ty::Float)),
            ExprKind::Bool(b) => Ok((Some(self.b.ins().iconst(types::I8, i64::from(*b))), Ty::Bool)),
            ExprKind::Name(name) => {
                let local = self.local(name).ok_or_else(|| unsupported(&format!("`{name}` here"), span))?;
                let (var, ty) = (local.var, local.ty);
                Ok((var.map(|var| self.b.use_var(var)), ty))
            }
            ExprKind::Unary { op, expr: inner } => {
                let operand = self.scalar(inner)?;
                self.unary(*op, operand, span).map(|(value, ty)| (Some(value), ty))
            }
            ExprKind::Binary { op: op @ (BinOp::And | BinOp::Or), lhs, rhs } => {
                self.logic(*op, lhs, rhs).map(|value| (Some(value), Ty::Bool))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let left = self.scalar(lhs)?;
                let right = self.scalar(rhs)?;
                self.binary(*op, left, right, span).map(|(value, ty)| (Some(value), ty))
            }
            ExprKind::Cond { then, cond, otherwise } => self.conditional(then, cond, otherwise),
            ExprKind::Call { callee, args } => self.call(callee, args, span),
            _ => Err(unsupported("this kind of expression", span)),
        }
    }

    /// An expression that must produce a number or bool.
    pub(super) fn scalar(&mut self, expr: &Expr) -> Result<(Value, Ty), Diagnostic> {
        match self.expr(expr)? {
            (Some(value), ty) => Ok((value, ty)),
            (None, ty) => Err(unsupported(&format!("using {} as a value", ty.name()), expr.span)),
        }
    }

    pub(super) fn condition(&mut self, expr: &Expr) -> Result<Value, Diagnostic> {
        match self.scalar(expr)? {
            (value, Ty::Bool) => Ok(value),
            (_, ty) => Err(unsupported(&format!("a condition that's a {}", ty.name()), expr.span)),
        }
    }

    fn unary(
        &mut self,
        op: UnaryOp,
        (value, ty): (Value, Ty),
        span: Span,
    ) -> Result<(Value, Ty), Diagnostic> {
        match (op, ty) {
            (UnaryOp::Neg, Ty::Int) => {
                let zero = self.b.ins().iconst(types::I64, 0);
                let (result, overflow) = self.b.ins().ssub_overflow(zero, value);
                self.fail_if(overflow, runtime::OVERFLOW, span);
                Ok((result, Ty::Int))
            }
            (UnaryOp::Neg, Ty::Float) => Ok((self.b.ins().fneg(value), Ty::Float)),
            (UnaryOp::Not, Ty::Bool) => Ok((self.b.ins().bxor_imm_u(value, 1), Ty::Bool)),
            _ => Err(unsupported(&format!("this operator on a {}", ty.name()), span)),
        }
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

    fn conditional(
        &mut self,
        then: &Expr,
        cond: &Expr,
        otherwise: &Expr,
    ) -> Result<(Option<Value>, Ty), Diagnostic> {
        let test = self.condition(cond)?;
        let then_block = self.b.create_block();
        let else_block = self.b.create_block();
        let done = self.b.create_block();
        self.b.ins().brif(test, then_block, &[], else_block, &[]);
        self.b.switch_to_block(then_block);
        let (then_value, ty) = self.scalar(then)?;
        let abi_ty = abi(ty).ok_or_else(|| unsupported("this expression", then.span))?;
        self.b.append_block_param(done, abi_ty);
        self.b.ins().jump(done, &[then_value.into()]);
        self.b.switch_to_block(else_block);
        let (else_value, else_ty) = self.scalar(otherwise)?;
        if else_ty != ty {
            return Err(unsupported(
                &format!("an `if` that gives both {} and {}", ty.name(), else_ty.name()),
                otherwise.span,
            ));
        }
        self.b.ins().jump(done, &[else_value.into()]);
        self.b.switch_to_block(done);
        Ok((self.b.block_params(done).first().copied(), ty))
    }

    pub(super) fn binary(
        &mut self,
        op: BinOp,
        (a, a_ty): (Value, Ty),
        (b, b_ty): (Value, Ty),
        span: Span,
    ) -> Result<(Value, Ty), Diagnostic> {
        if a_ty != b_ty {
            return Err(unsupported(&format!("mixing {} and {}", a_ty.name(), b_ty.name()), span));
        }
        if let Some(cc) = int_compare(op).filter(|_| matches!(a_ty, Ty::Int | Ty::Bool)) {
            return Ok((self.b.ins().icmp(cc, a, b), Ty::Bool));
        }
        if let Some(cc) = float_compare(op).filter(|_| a_ty == Ty::Float) {
            return Ok((self.b.ins().fcmp(cc, a, b), Ty::Bool));
        }
        match a_ty {
            Ty::Int => self.int_arithmetic(op, a, b, span),
            Ty::Float => self.float_arithmetic(op, a, b, span),
            _ => Err(unsupported(&format!("`{}` on a {}", op.symbol(), a_ty.name()), span)),
        }
    }

    fn int_arithmetic(
        &mut self,
        op: BinOp,
        a: Value,
        b: Value,
        span: Span,
    ) -> Result<(Value, Ty), Diagnostic> {
        let checked = match op {
            BinOp::Add => Some(self.b.ins().sadd_overflow(a, b)),
            BinOp::Sub => Some(self.b.ins().ssub_overflow(a, b)),
            BinOp::Mul => Some(self.b.ins().smul_overflow(a, b)),
            _ => None,
        };
        if let Some((result, overflow)) = checked {
            self.fail_if(overflow, runtime::OVERFLOW, span);
            return Ok((result, Ty::Int));
        }
        match op {
            BinOp::Div => {
                let zero = self.b.ins().icmp_imm_s(IntCC::Equal, b, 0);
                self.fail_if(zero, runtime::DIVISION_BY_ZERO, span);
                let (x, y) =
                    (self.b.ins().fcvt_from_sint(types::F64, a), self.b.ins().fcvt_from_sint(types::F64, b));
                Ok((self.b.ins().fdiv(x, y), Ty::Float))
            }
            BinOp::FloorDiv | BinOp::Mod => {
                let zero = self.b.ins().icmp_imm_s(IntCC::Equal, b, 0);
                self.fail_if(zero, runtime::DIVISION_BY_ZERO, span);
                let minus_one = self.b.ins().icmp_imm_s(IntCC::Equal, b, -1);
                let lowest = self.b.ins().icmp_imm_s(IntCC::Equal, a, i64::MIN);
                let overflow = self.b.ins().band(minus_one, lowest);
                if op == BinOp::FloorDiv {
                    self.fail_if(overflow, runtime::OVERFLOW, span);
                }
                // Avoid the hardware trap on MIN % -1 (whose answer is 0) by dividing by 1.
                let one = self.b.ins().iconst(types::I64, 1);
                let divisor = self.b.ins().select(minus_one, one, b);
                let quotient = self.b.ins().sdiv(a, divisor);
                let quotient = self.b.ins().select(minus_one, a, quotient);
                let quotient = if op == BinOp::FloorDiv { self.b.ins().ineg(quotient) } else { quotient };
                let quotient = if op == BinOp::FloorDiv { self.b.ins().ineg(quotient) } else { quotient };
                let remainder = self.b.ins().srem(a, divisor);
                // Python rounds toward negative infinity: adjust when the signs differ.
                let nonzero = self.b.ins().icmp_imm_s(IntCC::NotEqual, remainder, 0);
                let signs = self.b.ins().bxor(remainder, b);
                let differ = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, signs, 0);
                let adjust = self.b.ins().band(nonzero, differ);
                Ok(if op == BinOp::FloorDiv {
                    let lower = self.b.ins().iadd_imm_s(quotient, -1);
                    (self.b.ins().select(adjust, lower, quotient), Ty::Int)
                } else {
                    let raised = self.b.ins().iadd(remainder, b);
                    (self.b.ins().select(adjust, raised, remainder), Ty::Int)
                })
            }
            _ => Err(unsupported(&format!("`{}` on ints", op.symbol()), span)),
        }
    }

    fn float_arithmetic(
        &mut self,
        op: BinOp,
        a: Value,
        b: Value,
        span: Span,
    ) -> Result<(Value, Ty), Diagnostic> {
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
            _ => return Err(unsupported(&format!("`{}` on floats", op.symbol()), span)),
        };
        Ok((value, Ty::Float))
    }

    fn call(&mut self, callee: &Expr, args: &[Arg], span: Span) -> Result<(Option<Value>, Ty), Diagnostic> {
        if let ExprKind::Attr { object, name, optional: false } = &callee.kind
            && matches!(&object.kind, ExprKind::Name(object) if self.local(object).is_some_and(|local| local.ty == Ty::Stdout))
            && (name == "print" || name == "write")
        {
            self.print(args, name == "print", span)?;
            return Ok((None, Ty::Unit));
        }
        let ExprKind::Name(name) = &callee.kind else { return Err(unsupported("this call", span)) };
        if args.iter().any(|arg| arg.name.is_some() || arg.is_mut) {
            return Err(unsupported("named or `mut` arguments", span));
        }
        if let Some(function) = self.compiled.get(name).cloned() {
            if function.params.len() != args.len() {
                return Err(unsupported("this call", span));
            }
            let mut values = Vec::with_capacity(args.len());
            for (arg, param) in args.iter().zip(&function.params) {
                let (value, ty) = self.expr(&arg.value)?;
                if ty != *param {
                    return Err(unsupported(
                        &format!("passing a {} where a {} is expected", ty.name(), param.name()),
                        arg.value.span,
                    ));
                }
                values.extend(value);
            }
            let callee = self.module.declare_func_in_func(function.id, self.b.func);
            let call = self.b.ins().call(callee, &values);
            let result = self.b.inst_results(call).first().copied();
            self.check_failed();
            return Ok((result, function.ret));
        }
        self.builtin(name, args, span)
    }

    fn builtin(&mut self, name: &str, args: &[Arg], span: Span) -> Result<(Option<Value>, Ty), Diagnostic> {
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.scalar(&arg.value)?);
        }
        let (value, ty) = match (name, values.as_slice()) {
            ("abs", [(x, Ty::Int)]) => {
                let zero = self.b.ins().iconst(types::I64, 0);
                let (negated, overflow) = self.b.ins().ssub_overflow(zero, *x);
                self.fail_if(overflow, runtime::OVERFLOW, span);
                let negative = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, *x, 0);
                (self.b.ins().select(negative, negated, *x), Ty::Int)
            }
            ("abs", [(x, Ty::Float)]) => (self.b.ins().fabs(*x), Ty::Float),
            ("min" | "max", [(a, Ty::Int), (b, Ty::Int)]) => {
                (if name == "min" { self.b.ins().smin(*a, *b) } else { self.b.ins().smax(*a, *b) }, Ty::Int)
            }
            ("min" | "max", [(a, Ty::Float), (b, Ty::Float)]) => {
                (if name == "min" { self.b.ins().fmin(*a, *b) } else { self.b.ins().fmax(*a, *b) }, Ty::Float)
            }
            ("float", [(x, Ty::Int)]) => (self.b.ins().fcvt_from_sint(types::F64, *x), Ty::Float),
            ("float", [(x, Ty::Float)]) | ("int", [(x, Ty::Int)]) => {
                (*x, values.first().map_or(Ty::Int, |v| v.1))
            }
            ("int", [(x, Ty::Float)]) => (self.b.ins().fcvt_to_sint_sat(types::I64, *x), Ty::Int),
            _ => return Err(unsupported(&format!("calling `{name}` like this"), span)),
        };
        Ok((Some(value), ty))
    }

    /// `stdout.print(...)`: each argument's text, separated by spaces, then a newline.
    fn print(&mut self, args: &[Arg], newline: bool, span: Span) -> Result<(), Diagnostic> {
        for (at, arg) in args.iter().enumerate() {
            if arg.name.is_some() || arg.is_mut {
                return Err(unsupported("named or `mut` arguments", span));
            }
            if at > 0 {
                self.print_text(" ");
            }
            match &arg.value.kind {
                ExprKind::Str(text) => self.print_text(text),
                ExprKind::FStr(parts) => {
                    for part in parts {
                        match part {
                            FStrPart::Text(text) => self.print_text(text),
                            FStrPart::Expr { expr, spec: None } => self.print_value(expr)?,
                            FStrPart::Expr { expr, .. } => {
                                return Err(unsupported("format specs", expr.span));
                            }
                        }
                    }
                }
                _ => self.print_value(&arg.value)?,
            }
        }
        if newline {
            self.print_text("\n");
        }
        Ok(())
    }

    fn print_value(&mut self, expr: &Expr) -> Result<(), Diagnostic> {
        let (value, ty) = self.scalar(expr)?;
        let helper = match ty {
            Ty::Int => self.helpers.print_int,
            Ty::Float => self.helpers.print_float,
            _ => self.helpers.print_bool,
        };
        let helper = self.module.declare_func_in_func(helper, self.b.func);
        self.b.ins().call(helper, &[value]);
        Ok(())
    }

    fn print_text(&mut self, text: &str) {
        let index = self.strings.iter().position(|existing| existing == text).unwrap_or_else(|| {
            self.strings.push(text.to_owned());
            self.strings.len() - 1
        });
        let index = self.b.ins().iconst(types::I64, i64::try_from(index).unwrap_or(0));
        let helper = self.module.declare_func_in_func(self.helpers.print_str, self.b.func);
        self.b.ins().call(helper, &[index]);
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
