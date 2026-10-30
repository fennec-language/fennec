//! Calls: to compiled functions natively, a few builtins on numbers natively, and
//! `stdout.print`. Calls to methods, types and other builtins run in the interpreter.

use cranelift_codegen::ir::{InstBuilder, Value, types};
use cranelift_module::Module;
use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, FStrPart, Span};

use super::Lowerer;
use super::expr::Lowered;
use crate::helpers::Helper;
use crate::runtime;
use crate::types::Ty;

impl Lowerer<'_, '_> {
    pub(super) fn call(&mut self, expr: &Expr, callee: &Expr, args: &[Arg]) -> Result<Lowered, Diagnostic> {
        let plain = args.iter().all(|arg| arg.name.is_none() && !arg.is_mut);
        if let ExprKind::Attr { object, name, optional: false } = &callee.kind
            && (name == "print" || name == "write")
            && plain
            && matches!(&object.kind, ExprKind::Name(object) if self.local(object).is_some_and(|local| local.ty == Ty::Stdout))
        {
            self.print(args, name == "print")?;
            return Ok((None, Ty::Unit));
        }
        if let ExprKind::Attr { object, name, optional: false } = &callee.kind {
            let result = match &object.kind {
                ExprKind::Name(type_name) if self.local(type_name).is_none() => {
                    let key = format!("{type_name}.{name}");
                    let constructor =
                        self.shared.compiled.get(&key).is_some_and(|compiled| !compiled.takes_self);
                    if constructor { self.call_compiled(&key, args, expr.span)? } else { None }
                }
                _ => self.call_method(expr, object, name, args)?,
            };
            if let Some(result) = result {
                return Ok(result);
            }
        }
        if let ExprKind::Name(name) = &callee.kind
            && self.local(name).is_none()
        {
            if let Some(result) = self.call_compiled(name, args, expr.span)? {
                return Ok(result);
            }
            if plain && let Some(result) = self.builtin(name, args, expr.span)? {
                return Ok(result);
            }
        }
        self.fallback(expr)
    }

    /// Which argument goes to each parameter, or `None` when they don't line up, which the
    /// interpreter then reports.
    fn arrange<'x>(params: &[(String, Ty, bool)], args: &'x [Arg]) -> Option<Vec<&'x Arg>> {
        let mut slots: Vec<Option<&Arg>> = vec![None; params.len()];
        let mut next = 0;
        for arg in args {
            let at = if let Some(name) = &arg.name {
                params.iter().position(|(param, _, _)| param == name)?
            } else {
                next += 1;
                next - 1
            };
            let slot = slots.get_mut(at)?;
            if slot.is_some() {
                return None;
            }
            *slot = Some(arg);
        }
        slots.into_iter().collect()
    }

    /// A call to a compiled function, natively. `None` when the arguments don't suit a
    /// native call.
    fn call_compiled(&mut self, name: &str, args: &[Arg], span: Span) -> Result<Option<Lowered>, Diagnostic> {
        let Some(compiled) = self.shared.compiled.get(name) else { return Ok(None) };
        let (id, params, ret) = (compiled.id, compiled.params.clone(), compiled.ret);
        let Some(arranged) = Self::arrange(&params, args) else { return Ok(None) };
        for (arg, (_, ty, is_mut)) in arranged.iter().zip(&params) {
            let fits = match (&arg.value.kind, arg.is_mut) {
                (ExprKind::Name(local), true) => {
                    *is_mut && self.local(local).is_some_and(|local| local.ty == *ty && local.is_mut)
                }
                (_, true) => false,
                (ExprKind::Name(local), false) if matches!(ty, Ty::Stdout | Ty::Fs) => {
                    self.local(local).is_some_and(|local| local.ty == *ty)
                }
                (_, false) => !is_mut && !matches!(ty, Ty::Stdout | Ty::Fs),
            };
            if !fits {
                return Ok(None);
            }
        }
        let mut lowered: Vec<Option<Lowered>> = vec![None; params.len()];
        for arg in args {
            let at = arranged.iter().position(|arranged| std::ptr::eq(*arranged, arg)).unwrap_or(0);
            if !arg.is_mut
                && let Some(slot) = lowered.get_mut(at)
            {
                *slot = Some(self.expr(&arg.value)?);
            }
        }
        let mut values = Vec::new();
        let mut mut_natives = Vec::new();
        for ((arg, (_, ty, is_mut)), lowered) in arranged.iter().zip(&params).zip(lowered) {
            match (lowered, is_mut, &arg.value.kind) {
                (Some((value, from)), _, _) => {
                    if !matches!(ty, Ty::Stdout | Ty::Fs) {
                        values.push(self.convert(value, from, *ty, arg.value.span)?);
                    }
                }
                (None, true, ExprKind::Name(local)) => {
                    let id = self.name_id(local);
                    if *ty == Ty::Obj {
                        values.push(self.help(Helper::Take, &[id])?);
                        values.push(id);
                    } else if let Some(var) = self.local(local).and_then(|local| local.var) {
                        values.push(self.b.use_var(var));
                        mut_natives.push(var);
                    }
                }
                _ => {}
            }
        }
        let mut results = self.raw_call(id, values, span)?.into_iter();
        let value = if super::abi(ret).is_some() { results.next() } else { None };
        for (var, result) in mut_natives.into_iter().zip(results) {
            self.b.def_var(var, result);
        }
        if ret == Ty::Obj {
            self.dirty = true;
        }
        Ok(Some((value, ret)))
    }

    /// Calls a compiled function by its key with arguments already as its parameters take
    /// them, giving back its result.
    pub(super) fn call_key(
        &mut self,
        key: &str,
        values: Vec<Value>,
        span: Span,
    ) -> Result<Lowered, Diagnostic> {
        let compiled = self.shared.compiled.get(key).map(|compiled| (compiled.id, compiled.ret));
        let (id, ret) = compiled.ok_or_else(|| super::internal("unknown function", span))?;
        let results = self.raw_call(id, values, span)?;
        let value = if super::abi(ret).is_some() { results.first().copied() } else { None };
        if ret == Ty::Obj {
            self.dirty = true;
        }
        Ok((value, ret))
    }

    /// Calls a compiled function, after checking calls aren't nested too deep: like the
    /// interpreter, more than 1000 deep fails, at the call.
    fn raw_call(
        &mut self,
        id: cranelift_module::FuncId,
        values: Vec<Value>,
        span: Span,
    ) -> Result<Vec<Value>, Diagnostic> {
        let depth = self.depth.ok_or_else(|| super::internal("no depth", span))?;
        let too_deep = self.b.ins().icmp_imm_s(
            cranelift_codegen::ir::condcodes::IntCC::SignedGreaterThanOrEqual,
            depth,
            fennec_interp::embed::MAX_DEPTH,
        );
        self.fail_if(too_deep, runtime::TOO_DEEP, span);
        let deeper = self.b.ins().iadd_imm_s(depth, 1);
        let mut args = vec![deeper];
        args.extend(values);
        let callee = self.module.declare_func_in_func(id, self.b.func);
        let call = self.b.ins().call(callee, &args);
        let results = self.b.inst_results(call).to_vec();
        self.check_failed();
        Ok(results)
    }

    /// Builtins on numbers that compile to a few instructions.
    fn builtin(&mut self, name: &str, args: &[Arg], span: Span) -> Result<Option<Lowered>, Diagnostic> {
        if !matches!(name, "abs" | "min" | "max" | "float" | "int") || self.shared.compiled.contains_key(name)
        {
            return Ok(None);
        }
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.expr(&arg.value)?);
        }
        let result = match (name, values.as_slice()) {
            ("abs", [(Some(x), Ty::Int)]) => {
                let zero = self.int(0);
                let (negated, overflow) = self.b.ins().ssub_overflow(zero, *x);
                self.fail_if(overflow, runtime::OVERFLOW, span);
                let negative =
                    self.b.ins().icmp_imm_s(cranelift_codegen::ir::condcodes::IntCC::SignedLessThan, *x, 0);
                (self.b.ins().select(negative, negated, *x), Ty::Int)
            }
            ("abs", [(Some(x), Ty::Float)]) => (self.b.ins().fabs(*x), Ty::Float),
            ("min" | "max", [(Some(a), Ty::Int), (Some(b), Ty::Int)]) => {
                (if name == "min" { self.b.ins().smin(*a, *b) } else { self.b.ins().smax(*a, *b) }, Ty::Int)
            }
            ("min" | "max", [(Some(a), Ty::Float), (Some(b), Ty::Float)]) => {
                (if name == "min" { self.b.ins().fmin(*a, *b) } else { self.b.ins().fmax(*a, *b) }, Ty::Float)
            }
            ("float", [(Some(x), Ty::Int)]) => (self.b.ins().fcvt_from_sint(types::F64, *x), Ty::Float),
            ("float", [(Some(x), Ty::Float)]) => (*x, Ty::Float),
            ("int", [(Some(x), Ty::Int)]) => (*x, Ty::Int),
            ("int", [(Some(x), Ty::Float)]) => (self.b.ins().fcvt_to_sint_sat(types::I64, *x), Ty::Int),
            _ => {
                let callee = Expr { kind: ExprKind::Name(name.to_owned()), span };
                let call =
                    Expr { kind: ExprKind::Call { callee: Box::new(callee), args: args.to_vec() }, span };
                return self.interpret_call(&call, &values).map(Some);
            }
        };
        Ok(Some((Some(result.0), result.1)))
    }

    /// `stdout.print(...)`: each argument's text, separated by spaces, then a newline.
    fn print(&mut self, args: &[Arg], newline: bool) -> Result<(), Diagnostic> {
        for (at, arg) in args.iter().enumerate() {
            if at > 0 {
                self.print_text(" ")?;
            }
            match &arg.value.kind {
                ExprKind::Str(text) => self.print_text(text)?,
                ExprKind::FStr(parts) => {
                    for part in parts {
                        match part {
                            FStrPart::Text(text) => self.print_text(text)?,
                            FStrPart::Expr { expr, spec: None } => self.print_value(expr)?,
                            FStrPart::Expr { expr, spec: Some(spec) } => {
                                let (value, ty) = self.expr(expr)?;
                                let handle = self.handle_of(value, ty, expr.span)?;
                                let spec = self.string_id(spec);
                                let [start, end] = self.span_args(expr.span);
                                self.call_helper(Helper::PrintSpec, &[handle, spec, start, end])?;
                            }
                        }
                    }
                }
                _ => self.print_value(&arg.value)?,
            }
        }
        if newline {
            self.print_text("\n")?;
        }
        Ok(())
    }

    fn print_value(&mut self, expr: &Expr) -> Result<(), Diagnostic> {
        let (value, ty) = self.expr(expr)?;
        let helper = match ty {
            Ty::Int => Helper::PrintInt,
            Ty::Float => Helper::PrintFloat,
            Ty::Bool => Helper::PrintBool,
            Ty::Obj => Helper::PrintValue,
            Ty::Unit => return Ok(()),
            Ty::Stdout | Ty::Fs => {
                let fallback = self.fallback(expr)?;
                let handle = self.handle_of(fallback.0, fallback.1, expr.span)?;
                return self.call_helper(Helper::PrintValue, &[handle]).map(|_| ());
            }
        };
        let args: Vec<Value> = value.into_iter().collect();
        self.call_helper(helper, &args).map(|_| ())
    }

    fn print_text(&mut self, text: &str) -> Result<(), Diagnostic> {
        let id = self.string_id(text);
        self.call_helper(Helper::PrintStr, &[id]).map(|_| ())
    }
}
