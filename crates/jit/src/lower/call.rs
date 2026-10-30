//! Calls: to compiled functions natively, a few builtins on numbers natively, and
//! `stdout.print`. Calls to methods, types and other builtins run in the interpreter.

use cranelift_codegen::ir::{InstBuilder, Value, types};
use cranelift_module::Module;
use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, FStrPart, Span};

use super::Lowerer;
use super::expr::Lowered;
use crate::helpers::Helper;
use crate::runtime;
use crate::types::{Cap, Ty};

impl Lowerer<'_, '_> {
    pub(super) fn call(&mut self, expr: &Expr, callee: &Expr, args: &[Arg]) -> Result<Lowered, Diagnostic> {
        let plain = args.iter().all(|arg| arg.name.is_none() && !arg.is_mut);
        if let ExprKind::Attr { object, name, optional: false } = &callee.kind
            && (name == "print" || name == "write")
            && plain
            && matches!(&object.kind, ExprKind::Name(object) if self.local(object).is_some_and(|local| local.ty == Ty::Cap(Cap::Stdout)))
        {
            self.print(args, name == "print")?;
            return Ok((None, Ty::Unit));
        }
        if let (ExprKind::Attr { object, name, optional: false }, [arg]) = (&callee.kind, args)
            && name == "append"
            && plain
            && let Some((list, item)) = self.packed(object)
        {
            self.append_packed(&list, item, &arg.value, expr.span)?;
            return Ok((None, Ty::Unit));
        }
        if let (ExprKind::Name(name), [arg]) = (&callee.kind, args)
            && name == "len"
            && plain
            && self.local(name).is_none()
            && let Some((list, item)) = self.packed(&arg.value)
        {
            return Ok((Some(self.list_length(&list, item)?), Ty::Int));
        }
        if let Some(name) = self.std_member(callee) {
            if let Some(id) = self.layouts.id(name)
                && let Some(result) = self.construct(id, args, expr.span)?
            {
                return Ok(result);
            }
            if plain && let Some(result) = self.builtin(name, callee, args, expr.span)? {
                return Ok(result);
            }
            return self.fallback(expr);
        }
        if let ExprKind::Attr { object, name, optional: false } = &callee.kind
            && let Some(Ty::Struct(id)) = self.static_ty(object)
            && let Some(result) = self.call_struct_method(expr, object, id, name, args)?
        {
            return Ok(result);
        }
        if let ExprKind::Name(type_name) = &callee.kind
            && self.local(type_name).is_none()
            && let Some(id) = self.layouts.id(type_name)
            && let Some(result) = self.construct(id, args, expr.span)?
        {
            return Ok(result);
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
            if plain && let Some(result) = self.builtin(name, callee, args, expr.span)? {
                return Ok(result);
            }
        }
        self.fallback(expr)
    }

    /// For `math.sqrt` where `math` is an imported module, or `std.math.sqrt` after
    /// `import std.math`, the name after the last dot.
    fn std_member<'e>(&self, callee: &'e Expr) -> Option<&'e str> {
        let ExprKind::Attr { object, name, optional: false } = &callee.kind else { return None };
        let alias = fennec_syntax::dotted(object)?;
        let first = alias.split('.').next().unwrap_or_default();
        (self.local(first).is_none() && self.shared.modules.contains(&alias)).then_some(name.as_str())
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
                (ExprKind::Name(local), false) if matches!(ty, Ty::Cap(_)) => {
                    self.local(local).is_some_and(|local| local.ty == *ty)
                }
                (_, false) => !is_mut && !matches!(ty, Ty::Cap(_)),
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
        let mut reloads = Vec::new();
        for ((arg, (_, ty, is_mut)), lowered) in arranged.iter().zip(&params).zip(lowered) {
            match (lowered, is_mut, &arg.value.kind) {
                (Some((value, from)), _, _) => {
                    if !matches!(ty, Ty::Cap(_)) {
                        values.push(self.convert(value, from, *ty, arg.value.span)?);
                    }
                }
                (None, true, ExprKind::Name(local)) => {
                    let id = self.name_id(local);
                    if *ty == Ty::Obj {
                        values.push(self.help(Helper::Take, &[id])?);
                        values.push(id);
                    } else if let (Ty::Struct(id), Some(place)) =
                        (ty, self.local(local).and_then(|local| self.local_place(local)))
                    {
                        // The callee changes the caller's struct through its address.
                        let (address, vars) = self.place_address(&place, *id)?;
                        values.push(address);
                        reloads.push((address, vars, *id));
                    } else if let Some(var) = self.local(local).and_then(|local| local.var) {
                        values.push(self.b.use_var(var));
                        mut_natives.push(var);
                    }
                }
                _ => {}
            }
        }
        if let Some(body) = self.inlinable(name) {
            return self.inline_call(name, &body, values, None, span).map(Some);
        }
        let mut values = self.addresses_for(&params, values)?;
        let dest = self.dest_for(ret)?;
        values.splice(0..0, dest);
        let mut results = self.raw_call(id, &values, self.is_prelude(name), span).into_iter();
        let value = if let Some(dest) = dest {
            Some(dest)
        } else if super::abi(ret).is_some() {
            results.next()
        } else {
            None
        };
        for (var, result) in mut_natives.into_iter().zip(results) {
            self.b.def_var(var, result);
        }
        for (address, vars, id) in reloads {
            self.reload_place(address, vars, id)?;
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
        if let Some(body) = self.inlinable(key) {
            return self.inline_call(key, &body, values, None, span);
        }
        let params =
            self.shared.compiled.get(key).map(|compiled| compiled.params.clone()).unwrap_or_default();
        let dest = self.dest_for(ret)?;
        let mut values = self.addresses_for(&params, values)?;
        values.splice(0..0, dest);
        let results = self.raw_call(id, &values, self.is_prelude(key), span);
        let value = if dest.is_some() {
            dest
        } else if super::abi(ret).is_some() {
            results.first().copied()
        } else {
            None
        };
        if ret == Ty::Obj {
            self.dirty = true;
        }
        Ok((value, ret))
    }

    /// Whether a compiled function is from the standard library.
    fn is_prelude(&self, key: &str) -> bool {
        self.shared.compiled.get(key).is_some_and(|compiled| compiled.prelude)
    }

    /// Calls a compiled function, after checking the stack has room for it: calls nested
    /// too deeply fail at the call, like the interpreter's, instead of crashing. `prelude`
    /// says it's a standard library function.
    fn raw_call(
        &mut self,
        id: cranelift_module::FuncId,
        values: &[Value],
        prelude: bool,
        span: Span,
    ) -> Vec<Value> {
        // Fail cleanly, at the call, before the stack runs out.
        let stack = self.b.ins().get_stack_pointer(self.pointer);
        let too_deep =
            self.b.ins().icmp(cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThan, stack, self.limit);
        self.fail_if(too_deep, runtime::TOO_DEEP, span);
        let callee = self.module.declare_func_in_func(id, self.b.func);
        let call = self.b.ins().call(callee, values);
        let mut results = self.b.inst_results(call).to_vec();
        if let Some(failed) = results.pop() {
            if prelude {
                self.leave_at_call_if(failed, span);
            } else {
                self.leave_if(failed);
            }
        }
        results
    }

    /// Leaves the function if the call at `span` failed. A failure inside the standard
    /// library is reported at the program's call to it.
    fn leave_at_call_if(&mut self, failed: Value, span: Span) {
        let site = self.site(span);
        let at_call = self.shared.helpers.get(&Helper::AtCall).copied();
        let (Some(id), false) = (at_call, site.in_prelude()) else {
            self.leave_if(failed);
            return;
        };
        let fail = self.b.create_block();
        self.b.set_cold_block(fail);
        let ok = self.b.create_block();
        self.b.ins().brif(failed, fail, &[], ok, &[]);
        self.b.switch_to_block(fail);
        self.b.seal_block(fail);
        let [start, end] = self.span_args(site);
        let callee = self.module.declare_func_in_func(id, self.b.func);
        self.b.ins().call(callee, &[start, end]);
        self.b.ins().jump(self.bail, &[]);
        self.b.switch_to_block(ok);
        self.b.seal_block(ok);
    }

    /// Struct arguments in memory, as a real call takes them. Values are in the order the
    /// call passes them, with nothing for capabilities, and a caller's name after a
    /// `mut` object.
    fn addresses_for(
        &mut self,
        params: &[(String, Ty, bool)],
        values: Vec<Value>,
    ) -> Result<Vec<Value>, Diagnostic> {
        let mut kinds = Vec::new();
        for (_, ty, is_mut) in params {
            match ty {
                Ty::Cap(_) => {}
                Ty::Obj if *is_mut => kinds.extend([Ty::Obj, Ty::Int]),
                ty => kinds.push(*ty),
            }
        }
        let mut converted = Vec::with_capacity(values.len());
        for (at, value) in values.into_iter().enumerate() {
            converted.push(match kinds.get(at) {
                Some(Ty::Struct(id)) => self.address_of(value, *id)?,
                _ => value,
            });
        }
        Ok(converted)
    }

    /// Memory for the plain struct a function gives back, when it gives one.
    fn dest_for(&mut self, ret: Ty) -> Result<Option<Value>, Diagnostic> {
        match ret {
            Ty::Struct(id) => self.new_struct(id).map(Some),
            _ => Ok(None),
        }
    }

    /// `value.method(args)` on a plain struct: a direct native call to its type's method,
    /// with `self` the struct's address, so a method that changes `self` changes it in place.
    fn call_struct_method(
        &mut self,
        expr: &Expr,
        object: &Expr,
        id: u32,
        method: &str,
        args: &[Arg],
    ) -> Result<Option<Lowered>, Diagnostic> {
        let key = format!("{}.{method}", self.layout(id)?.name);
        let Some(compiled) = self.shared.compiled.get(&key) else { return Ok(None) };
        let params = compiled.params.clone();
        if !compiled.takes_self
            || params.len() != args.len() + 1
            || args.iter().any(|arg| arg.name.is_some() || arg.is_mut)
            || params.iter().skip(1).any(|(_, _, is_mut)| *is_mut)
        {
            return Ok(None);
        }
        let changes_self = params.first().is_some_and(|(_, _, is_mut)| *is_mut);
        let place = self.struct_place(object)?;
        let receiver = if let Some((place, _)) = &place {
            self.read_place(place)
        } else {
            let (value, ty) = self.expr(object)?;
            self.convert(value, ty, Ty::Struct(id), object.span)?
        };
        let mut values = vec![receiver];
        for (arg, (_, to, _)) in args.iter().zip(params.iter().skip(1)) {
            let (value, from) = self.expr(&arg.value)?;
            if !matches!(to, Ty::Cap(_)) {
                values.push(self.convert(value, from, *to, arg.value.span)?);
            }
        }
        let self_place = place.filter(|_| changes_self).map(|(place, _)| place);
        if let Some(body) = self.inlinable(&key) {
            return self.inline_call(&key, &body, values, self_place, expr.span).map(Some);
        }
        // A real call changes `self` through its address, then the name reads it back.
        let reload = match &self_place {
            Some(place) => {
                let (address, vars) = self.place_address(place, id)?;
                if let Some(first) = values.first_mut() {
                    *first = address;
                }
                Some((address, vars))
            }
            None => None,
        };
        let result = self.call_key(&key, values, expr.span)?;
        if let Some((address, vars)) = reload {
            self.reload_place(address, vars, id)?;
        }
        Ok(Some(result))
    }

    /// Builtins on numbers that compile to a few instructions. `callee` is the name as
    /// written, like `sqrt` or `math.sqrt`.
    fn builtin(
        &mut self,
        name: &str,
        callee: &Expr,
        args: &[Arg],
        span: Span,
    ) -> Result<Option<Lowered>, Diagnostic> {
        let known = matches!(
            name,
            "abs" | "min" | "max" | "float" | "int" | "sqrt" | "floor" | "ceil" | "sin" | "cos"
        );
        if !known || self.shared.compiled.contains_key(name) {
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
            ("int", [(Some(x), Ty::Float)]) => (self.float_to_int(*x, span), Ty::Int),
            ("sqrt" | "floor" | "ceil" | "sin" | "cos", [(Some(x), ty @ (Ty::Int | Ty::Float))]) => {
                let x = if *ty == Ty::Int { self.b.ins().fcvt_from_sint(types::F64, *x) } else { *x };
                self.math(name, x, span)?
            }
            _ => {
                let call = Expr {
                    kind: ExprKind::Call { callee: Box::new(callee.clone()), args: args.to_vec() },
                    span,
                };
                return self.interpret_call(&call, &values).map(Some);
            }
        };
        Ok(Some((Some(result.0), result.1)))
    }

    /// `int(x)` on a float: toward zero, failing like the interpreter for a float outside an
    /// int's range (at least -2^63 and below 2^63) or not finite.
    fn float_to_int(&mut self, x: Value, span: Span) -> Value {
        use cranelift_codegen::ir::condcodes::FloatCC;
        let low = self.b.ins().f64const(-9_223_372_036_854_775_808.0);
        let high = self.b.ins().f64const(9_223_372_036_854_775_808.0);
        // Below the range, which is also true of NaN, or at or above it.
        let too_low = self.b.ins().fcmp(FloatCC::UnorderedOrLessThan, x, low);
        let too_high = self.b.ins().fcmp(FloatCC::GreaterThanOrEqual, x, high);
        let outside = self.b.ins().bor(too_low, too_high);
        self.fail_if(outside, runtime::NOT_AN_INT, span);
        self.b.ins().fcvt_to_sint_sat(types::I64, x)
    }

    /// The math builtins on a float.
    fn math(&mut self, name: &str, x: Value, span: Span) -> Result<(Value, Ty), Diagnostic> {
        Ok(match name {
            "floor" => {
                let floored = self.b.ins().floor(x);
                (self.float_to_int(floored, span), Ty::Int)
            }
            "ceil" => {
                let raised = self.b.ins().ceil(x);
                (self.float_to_int(raised, span), Ty::Int)
            }
            "sqrt" => {
                let zero = self.b.ins().f64const(0.0);
                let negative =
                    self.b.ins().fcmp(cranelift_codegen::ir::condcodes::FloatCC::LessThan, x, zero);
                self.fail_if(negative, runtime::NEGATIVE_SQRT, span);
                (self.b.ins().sqrt(x), Ty::Float)
            }
            _ => {
                let helper = if name == "sin" { Helper::Sin } else { Helper::Cos };
                (self.help(helper, &[x])?, Ty::Float)
            }
        })
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
            Ty::Struct(_) => {
                let handle = self.handle_of(value, ty, expr.span)?;
                return self.call_helper(Helper::PrintValue, &[handle]).map(|_| ());
            }
            Ty::Cap(_) => {
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
