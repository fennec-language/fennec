//! Calls: to compiled functions natively, a few builtins on numbers natively, and
//! `stdout.print`. Calls to methods, types and other builtins run in the interpreter.

use cranelift_codegen::ir::{InstBuilder, Value, types};
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
        if let (ExprKind::Attr { object, name, optional: false }, []) = (&callee.kind, args)
            && matches!(name.as_str(), "len" | "abs")
            && self.std_member(callee).is_none()
            && !matches!(self.static_ty(object), Some(Ty::Struct(_) | Ty::Boxed(_)))
        {
            return if name == "len" { self.length(expr, object) } else { self.absolute(expr, object) };
        }
        if let Some(total) = self.sum_call(expr, callee, args)? {
            return Ok(total);
        }
        if let Some(made) = self.call_known(callee, args)? {
            return Ok(made);
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
        if let Some(result) = self.call_typed_method(expr, callee, args)? {
            return Ok(result);
        }
        if let Some((id, at)) = self.named_case(callee)
            && let Some(result) = self.make_case(id, at, args, expr.span)?
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

    /// `value.method(args)` on a plain struct or boxed value whose type is known here: a
    /// direct native call to its type's method.
    fn call_typed_method(
        &mut self,
        expr: &Expr,
        callee: &Expr,
        args: &[Arg],
    ) -> Result<Option<Lowered>, Diagnostic> {
        let ExprKind::Attr { object, name, optional: false } = &callee.kind else { return Ok(None) };
        match self.static_ty(object) {
            Some(Ty::Struct(id)) => self.call_struct_method(expr, object, id, name, args),
            Some(Ty::Boxed(id)) => self.call_boxed_method(object, id, name, args, expr.span),
            Some(Ty::Dict(key, value)) => self.call_dict_method(object, (key, value), name, args),
            _ => Ok(None),
        }
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
        if let Some(made) = self.specialized_call(name, args, span)? {
            self.shared.specialized.insert(name.to_owned());
            return Ok(Some(made));
        }
        self.shared.called.insert(name.to_owned());
        let Some(compiled) = self.shared.compiled.get(name) else { return Ok(None) };
        let (id, params, ret) = (compiled.id, compiled.params.clone(), compiled.ret);
        let Some(arranged) = Self::arrange(&params, args) else { return Ok(None) };
        if !self.args_fit(&arranged, &params) {
            return Ok(None);
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
        // A list passed both ways must keep its items where it's passed without `mut`, so
        // that argument gets an owner of its own for the call, and a change copies the list.
        let changes = params.iter().any(|(_, ty, is_mut)| *is_mut && ty.counted());
        let mut values = Vec::new();
        let mut mut_natives = Vec::new();
        let mut reloads = Vec::new();
        for ((arg, (_, ty, is_mut)), lowered) in arranged.iter().zip(&params).zip(lowered) {
            match (lowered, is_mut, &arg.value.kind) {
                (Some((value, from)), _, _) => {
                    if !matches!(ty, Ty::Cap(_)) {
                        let converted = self.convert(value, from, *ty, arg.value.span)?;
                        values.push(if changes { self.own(converted, *ty) } else { converted });
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
        let used = Self::boxed_args(&params, &values);
        let mut values = self.addresses_for(&params, values)?;
        let dest = self.dest_for(ret)?;
        values.splice(0..0, dest);
        let mut results = self.raw_call(id, &values, self.is_prelude(name), span).into_iter();
        // The callee only borrowed its boxed arguments.
        for (value, ty) in used {
            self.consume(Some(value), ty)?;
        }
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

    /// Whether arranged arguments suit a native call: `mut` exactly where the parameter is,
    /// and capabilities passed by name.
    fn args_fit(&self, arranged: &[&Arg], params: &[(String, Ty, bool)]) -> bool {
        arranged.iter().zip(params).all(|(arg, (_, ty, is_mut))| match (&arg.value.kind, arg.is_mut) {
            (ExprKind::Name(local), true) => {
                *is_mut && self.local(local).is_some_and(|local| local.ty == *ty && local.is_mut)
            }
            (_, true) => false,
            (ExprKind::Name(local), false) if matches!(ty, Ty::Cap(_)) => {
                self.local(local).is_some_and(|local| local.ty == *ty)
            }
            (_, false) => !is_mut && !matches!(ty, Ty::Cap(_)),
        })
    }

    /// Calls a compiled function by its key with arguments already as its parameters take
    /// them, giving back its result.
    pub(super) fn call_key(
        &mut self,
        key: &str,
        values: Vec<Value>,
        span: Span,
    ) -> Result<Lowered, Diagnostic> {
        if let Some(body) = self.inlinable(key) {
            return self.inline_call(key, &body, values, None, span);
        }
        self.call_key_changing(key, values, span).map(|(result, _)| result)
    }

    /// Calls a compiled function by its key, natively, giving back its result and the final
    /// values of its `mut` parameters that it gives back.
    pub(super) fn call_key_changing(
        &mut self,
        key: &str,
        values: Vec<Value>,
        span: Span,
    ) -> Result<(Lowered, Vec<Value>), Diagnostic> {
        let compiled = self.shared.compiled.get(key).map(|compiled| (compiled.id, compiled.ret));
        let (id, ret) = compiled.ok_or_else(|| super::internal("unknown function", span))?;
        let params =
            self.shared.compiled.get(key).map(|compiled| compiled.params.clone()).unwrap_or_default();
        let dest = self.dest_for(ret)?;
        let used = Self::boxed_args(&params, &values);
        let mut values = self.addresses_for(&params, values)?;
        values.splice(0..0, dest);
        let mut results = self.raw_call(id, &values, self.is_prelude(key), span).into_iter();
        for (value, ty) in used {
            self.consume(Some(value), ty)?;
        }
        let value = if dest.is_some() {
            dest
        } else if super::abi(ret).is_some() {
            results.next()
        } else {
            None
        };
        if ret == Ty::Obj {
            self.dirty = true;
        }
        Ok(((value, ret), results.collect()))
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
        if self.stack_unchecked() {
            let stack = self.b.ins().get_stack_pointer(self.pointer);
            let limit = self.b.ins().get_pinned_reg(self.pointer);
            let too_deep =
                self.b.ins().icmp(cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThan, stack, limit);
            self.fail_if(too_deep, runtime::TOO_DEEP, span);
            self.note_stack_checked();
        }
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

    /// The boxed arguments among values in the order a call passes them, to let go of after
    /// the call.
    fn boxed_args(params: &[(String, Ty, bool)], values: &[Value]) -> Vec<(Value, Ty)> {
        let mut kinds = Vec::new();
        for (_, ty, is_mut) in params {
            match ty {
                Ty::Cap(_) => {}
                Ty::Obj if *is_mut => kinds.extend([(Ty::Obj, true), (Ty::Int, true)]),
                ty => kinds.push((*ty, *is_mut)),
            }
        }
        // A `mut` argument isn't let go: the callee gives it back.
        kinds
            .into_iter()
            .zip(values)
            .filter(|((ty, is_mut), _)| ty.counted() && !is_mut)
            .map(|((ty, _), value)| (*value, ty))
            .collect()
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
        let known =
            matches!(name, "min" | "max" | "float" | "int" | "sqrt" | "floor" | "ceil" | "sin" | "cos");
        if !known || self.shared.compiled.contains_key(name) {
            return Ok(None);
        }
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.expr(&arg.value)?);
        }
        let result = match (name, values.as_slice()) {
            ("min" | "max", [(Some(a), Ty::Int), (Some(b), Ty::Int)]) => {
                (if name == "min" { self.b.ins().smin(*a, *b) } else { self.b.ins().smax(*a, *b) }, Ty::Int)
            }
            ("min" | "max", [(Some(a), Ty::Float), (Some(b), Ty::Float)]) => {
                (if name == "min" { self.b.ins().fmin(*a, *b) } else { self.b.ins().fmax(*a, *b) }, Ty::Float)
            }
            ("float", [(Some(x), Ty::Int)]) => (self.int_to_float(*x), Ty::Float),
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

    /// `x.len()`: natively for a list, text and a dict, and in the interpreter for
    /// anything else.
    fn length(&mut self, expr: &Expr, object: &Expr) -> Result<Lowered, Diagnostic> {
        if let Some((list, item)) = self.packed(object) {
            return Ok((Some(self.list_length(&list, item)?), Ty::Int));
        }
        let (value, ty) = self.expr(object)?;
        match (value, ty) {
            (Some(text), Ty::Str) => Ok((Some(self.string_length(text)?), Ty::Int)),
            (Some(dict), Ty::Dict(..)) => {
                let length = self.dict_length(dict);
                self.consume(Some(dict), ty)?;
                Ok((Some(length), Ty::Int))
            }
            (Some(list), Ty::List(_)) => {
                let length = self.length_of_list(list);
                self.consume(Some(list), ty)?;
                Ok((Some(length), Ty::Int))
            }
            _ => {
                let receiver = self.handle_of(value, ty, object.span)?;
                self.interpret_method(expr, object, receiver, "len", &[])
            }
        }
    }

    /// `x.abs()`: natively for an int or a float, and in the interpreter for anything else.
    fn absolute(&mut self, expr: &Expr, object: &Expr) -> Result<Lowered, Diagnostic> {
        let (value, ty) = self.expr(object)?;
        match (value, ty) {
            (Some(x), Ty::Int) => {
                let zero = self.int(0);
                let (negated, overflow) = self.b.ins().ssub_overflow(zero, x);
                self.fail_if(overflow, runtime::OVERFLOW, expr.span);
                let negative =
                    self.b.ins().icmp_imm_s(cranelift_codegen::ir::condcodes::IntCC::SignedLessThan, x, 0);
                Ok((Some(self.b.ins().select(negative, negated, x)), Ty::Int))
            }
            (Some(x), Ty::Float) => Ok((Some(self.b.ins().fabs(x)), Ty::Float)),
            _ => {
                let receiver = self.handle_of(value, ty, object.span)?;
                self.interpret_method(expr, object, receiver, "abs", &[])
            }
        }
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
        // Everything is worked out before anything prints, as the interpreter does, so a
        // failure in one argument prints none of them.
        let mut exprs = Vec::new();
        for arg in args {
            match &arg.value.kind {
                ExprKind::Str(_) => {}
                ExprKind::FStr(parts) => exprs.extend(parts.iter().filter_map(|part| match part {
                    FStrPart::Text(_) => None,
                    FStrPart::Expr { expr, .. } => Some(expr),
                })),
                _ => exprs.push(&arg.value),
            }
        }
        let many = exprs.len() > 1;
        let mut inputs = Vec::with_capacity(exprs.len());
        for expr in exprs {
            inputs.push(self.print_input(expr, many)?);
        }
        let mut inputs = inputs.into_iter();
        let mut next = move || inputs.next().unwrap_or((None, Ty::Unit));
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
                            FStrPart::Expr { spec: None, .. } => self.print_value(next())?,
                            FStrPart::Expr { expr, spec: Some(spec) } => {
                                let (value, ty) = next();
                                let handle = self.handle_of(value, ty, expr.span)?;
                                let spec = self.string_id(spec);
                                let [start, end] = self.span_args(expr.span);
                                self.call_helper(Helper::PrintSpec, &[handle, spec, start, end])?;
                            }
                        }
                    }
                }
                _ => self.print_value(next())?,
            }
        }
        if newline {
            self.print_text("\n")?;
        }
        Ok(())
    }

    /// Works out something to print, in a form that a later argument can't change: when
    /// `many` are printed, a string gets an owner of its own, and a structure or collection
    /// becomes an object now.
    fn print_input(&mut self, expr: &Expr, many: bool) -> Result<Lowered, Diagnostic> {
        let (value, ty) = self.expr(expr)?;
        match ty {
            Ty::Str if many => Ok((value.map(|text| self.own(text, ty)), ty)),
            Ty::Struct(_) | Ty::Boxed(_) | Ty::Dict(..) | Ty::List(_) | Ty::Known(_) => {
                Ok((Some(self.handle_of(value, ty, expr.span)?), Ty::Obj))
            }
            Ty::Cap(_) => {
                let fallback = self.fallback(expr)?;
                Ok((Some(self.handle_of(fallback.0, fallback.1, expr.span)?), Ty::Obj))
            }
            _ => Ok((value, ty)),
        }
    }

    fn print_value(&mut self, (value, ty): Lowered) -> Result<(), Diagnostic> {
        let helper = match ty {
            Ty::Int => Helper::PrintInt,
            Ty::Float => Helper::PrintFloat,
            Ty::Bool => Helper::PrintBool,
            Ty::Str => {
                let address = value.ok_or_else(|| super::internal("a missing string", Span::default()))?;
                self.call_helper(Helper::StrPrint, &[address])?;
                return self.consume(Some(address), ty);
            }
            Ty::Unit => return Ok(()),
            _ => Helper::PrintValue,
        };
        let args: Vec<Value> = value.into_iter().collect();
        self.call_helper(helper, &args).map(|_| ())
    }

    fn print_text(&mut self, text: &str) -> Result<(), Diagnostic> {
        let id = self.string_id(text);
        self.call_helper(Helper::PrintStr, &[id]).map(|_| ())
    }
}
