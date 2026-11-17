//! Arrow functions, nested functions and functions as values. Each is lifted to a function
//! of its own, whose first parameters are what it captures. Capturing copies, when the
//! closure is made, so it sees the values as they were then.

use fennec_check::Type;
use fennec_syntax::{Arg, Expr, ExprKind, FnDef, Span, visit};

use super::generics::FunctionInstance;
use super::{Lowered, Lowering, Tail, reachable};
use crate::{Function, InstKind, LocalId, Operand};

/// What a closure captures: a local of the function making it, under its name.
type Captures = Vec<(String, LocalId)>;

impl Lowering<'_> {
    /// The locals an arrow function or nested function reads from here, in the order it
    /// first names them.
    pub(super) fn captures(&self, mut names: Vec<String>, own: &[&str]) -> Captures {
        let mut found: Captures = Vec::new();
        for name in names.drain(..) {
            if own.contains(&name.as_str()) || found.iter().any(|(seen, _)| *seen == name) {
                continue;
            }
            if let Some(local) = self.lookup(&name) {
                found.push((name, local));
            }
        }
        found
    }

    /// A closure of `function` over what it captures, as a value of type `ty`.
    pub(super) fn make_closure(
        &mut self,
        function: String,
        captures: &Captures,
        ty: Type,
        span: Span,
    ) -> LocalId {
        let dest = self.temporary(ty);
        let captures = captures.iter().map(|(_, local)| Operand::Local(*local)).collect();
        self.emit(InstKind::MakeClosure { dest, function, captures }, span);
        dest
    }

    /// `(x) => body`.
    /// `changes` says whoever calls it gets its first parameter back, changed.
    pub(super) fn arrow(
        &mut self,
        expr: &Expr,
        params: &[String],
        body: &Expr,
        changes: bool,
    ) -> Lowered<Operand> {
        let ty = self.ty(expr)?;
        let Type::Arrow(types, returns) = &ty else {
            return Err("an arrow function has another type".to_owned());
        };
        let mut names = Vec::new();
        let mut written = body.clone();
        visit::in_expr(&mut written, &mut |seen| {
            if let ExprKind::Name(name) = &seen.kind {
                names.push(name.clone());
            }
        });
        let own: Vec<&str> = params.iter().map(String::as_str).collect();
        let captures = self.captures(names, &own);
        let key = self.lifted_name("arrow", expr.span.start);
        let instance =
            FunctionInstance { base: self.base.clone(), name: key.clone(), subst: self.subst.clone() };
        let mut lifted = Lowering::new(&instance, self.context, self.body, *returns.clone());
        for (name, local) in &captures {
            lifted.declare(name, self.local_type(*local));
        }
        for (param, ty) in params.iter().zip(types) {
            lifted.declare(param, ty.clone());
        }
        lifted.scopes.push(std::collections::HashMap::new());
        lifted.tail_expr(body, if **returns == Type::Unit { Tail::Drop } else { Tail::Return })?;
        lifted.end_function(expr.span)?;
        let count = captures.len() + params.len();
        let mut_params = if changes { vec![captures.len()] } else { Vec::new() };
        self.lifted.extend(lifted.finished(count, mut_params));
        Ok(Operand::Local(self.make_closure(key, &captures, ty, expr.span)))
    }

    /// `def inner(...)` inside a function: its name holds a closure from here on.
    pub(super) fn nested(&mut self, def: &FnDef, span: Span) -> Lowered<()> {
        let key = format!("{}.{}", self.name, def.name);
        let base = format!("{}.{}", self.base, def.name);
        let context = self.context;
        let raw = context.signatures.get(&base).ok_or("a nested function has no signature")?;
        let signature = context.instantiate(raw, &self.subst);
        if signature.params.iter().any(|param| param.is_mut) {
            return Err("it has a nested function with a `mut` parameter".to_owned());
        }
        let params = signature.params.iter().map(|param| param.ty.clone()).collect();
        let ty = Type::Arrow(params, Box::new(signature.returns.clone()));
        if !context.holds(&ty) {
            return Err("a nested function takes what the IR doesn't hold".to_owned());
        }
        let mut names = Vec::new();
        let mut written = def.clone();
        visit::in_function(&mut written, &mut |seen| {
            if let ExprKind::Name(name) = &seen.kind {
                names.push(name.clone());
            }
        });
        let mentions_itself = names.contains(&def.name);
        let mut own: Vec<&str> = signature.params.iter().map(|param| param.name.as_str()).collect();
        own.push(def.name.as_str());
        let captures = self.captures(names, &own);
        let held: Vec<(String, Type)> =
            captures.iter().map(|(name, local)| (name.clone(), self.local_type(*local))).collect();
        let itself = mentions_itself.then_some(&ty);
        let instance = FunctionInstance { base, name: key.clone(), subst: self.subst.clone() };
        self.lifted.extend(Lowering::function(&instance, def, context, (&held, itself))?);
        let closure = self.make_closure(key, &captures, ty.clone(), span);
        let local = self.declare(&def.name, ty);
        self.emit(InstKind::Copy { dest: local, value: Operand::Local(closure) }, span);
        Ok(())
    }

    /// Inside a nested function that names itself, its name holds a closure of itself.
    pub(super) fn bind_itself(&mut self, key: &str, name: &str, ty: &Type, count: usize, span: Span) {
        let captures = (0..count).map(|at| Operand::Local(LocalId(at))).collect();
        let dest = self.declare(name, ty.clone());
        self.emit(InstKind::MakeClosure { dest, function: key.to_owned(), captures }, span);
    }

    /// A function named as a value.
    pub(super) fn function_value(&mut self, key: &str, expr: &Expr) -> Lowered<Operand> {
        let ty = self
            .context
            .arrow_of(key)
            .ok_or_else(|| format!("it passes `{key}`, which takes what the IR doesn't hold"))?;
        Ok(Operand::Local(self.make_closure(key.to_owned(), &Vec::new(), ty, expr.span)))
    }

    /// The type of a callee that's a value rather than a function by name.
    pub(super) fn closure_callee(&self, callee: &Expr) -> Option<Type> {
        let ty = match &callee.kind {
            // An optional closure known not to be `None` here is read as the closure, and so is a
            // union a `match` or an `is` narrowed to its function member.
            ExprKind::Name(name) => match self.local_type(self.lookup(name)?) {
                Type::Optional(_) | Type::Union(_) => self.type_at(callee.span)?,
                held => held,
            },
            _ => self.type_at(callee.span)?,
        };
        matches!(ty, Type::Arrow(..)).then_some(ty)
    }

    /// A closure where one of another type goes, which the checker has shown fits: it takes what
    /// the other takes, and what it gives back fits what the other gives, as an `Answer` where
    /// `(int) -> Answer?` goes. It's held by a closure of the wanted type, which hands on what
    /// it's given, calls it, and makes what it gives back fit.
    pub(super) fn fitted_closure(
        &mut self,
        value: Operand,
        from: &Type,
        to: &Type,
        span: Span,
    ) -> Lowered<Operand> {
        let (Type::Arrow(held, gives), Type::Arrow(wanted, returns)) = (from, to) else {
            return Err(format!("it gives {from} where {to} goes"));
        };
        // Numbered among this function's, as its arrows are, so text added above it renames nothing.
        let prefix = format!("{}.fitted#", self.name);
        let number = self.lifted.iter().filter(|function| function.name.starts_with(&prefix)).count();
        let key = format!("{prefix}{number}");
        let instance =
            FunctionInstance { base: self.base.clone(), name: key.clone(), subst: self.subst.clone() };
        let mut lifted = Lowering::new(&instance, self.context, self.body, *returns.clone());
        // No name a program can write, so nothing it's handed is hidden by one.
        let closure = lifted.declare("\0fitted", from.clone());
        let mut args = Vec::with_capacity(wanted.len());
        for (at, (wanted, held)) in wanted.iter().zip(held).enumerate() {
            let handed = lifted.declare(&format!("\0handed {at}"), wanted.clone());
            args.push(lifted.coerce(Operand::Local(handed), held, span)?);
        }
        lifted.scopes.push(std::collections::HashMap::new());
        let dest = (**gives != Type::Unit).then(|| lifted.temporary(*gives.clone()));
        lifted.emit(InstKind::CallClosure { dest, closure: Operand::Local(closure), args }, span);
        match dest {
            Some(back) => {
                let fitted = lifted.coerce(Operand::Local(back), returns, span)?;
                lifted.leave(Some(fitted), span)?;
            }
            None => lifted.end_function(span)?,
        }
        self.lifted.extend(lifted.finished(1 + wanted.len(), Vec::new()));
        // What's captured is a local: a function named where a closure goes is one already.
        let held = match value {
            Operand::Local(local) => local,
            other => {
                let local = self.temporary(from.clone());
                self.emit(InstKind::Copy { dest: local, value: other }, span);
                local
            }
        };
        let captures = vec![("\0fitted".to_owned(), held)];
        Ok(Operand::Local(self.make_closure(key, &captures, to.clone(), span)))
    }

    /// An argument where a parameter of type `wanted` goes. An arrow function written right
    /// there, giving back something narrower than the parameter's does, is lowered as giving the
    /// wider thing, so it's the closure wanted with nothing between: what its body gives is made
    /// to fit as it leaves.
    pub(super) fn expr_where(&mut self, expr: &Expr, wanted: &Type) -> Lowered<Operand> {
        let own = self.type_at(expr.span).and_then(|ty| self.held(&ty));
        let retype = matches!(expr.kind, ExprKind::Lambda { .. })
            && matches!(
                (&own, wanted),
                (Some(Type::Arrow(held, gives)), Type::Arrow(params, returns))
                    if held == params && gives != returns && **gives != Type::Unit
            )
            && self.context.holds(wanted);
        if !retype {
            return self.expr(expr);
        }
        self.retyped.push((expr.span, wanted.clone()));
        let lowered = self.expr(expr);
        self.retyped.pop();
        lowered
    }

    /// A call to a closure. A closure's type has no names, but a nested function's call may name
    /// its arguments, as `scaled(5, by=3)`: each runs in the order it's written and lands in the
    /// parameter it names, by the nested function's signature.
    pub(super) fn call_closure(
        &mut self,
        callee: &Expr,
        ty: &Type,
        args: &[Arg],
        keep: Option<Type>,
        span: Span,
    ) -> Lowered<Option<LocalId>> {
        let Type::Arrow(params, _) = ty else {
            return Err("it calls something other than a function".to_owned());
        };
        let slots = self.closure_slots(callee, args, params.len())?;
        let closure = self.expr(callee)?;
        let mut reads = super::order::Reads::default();
        for (arg, at) in args.iter().zip(&slots) {
            // A nested function with a `mut` parameter isn't lowered, and a closure's type takes
            // none, so nothing passes one here.
            if arg.is_mut {
                return Err("its call to a closure passes an argument with `mut`".to_owned());
            }
            let param = params.get(*at).ok_or("an argument to a closure matches no parameter")?;
            let value = self.expr_where(&arg.value, param)?;
            let value = self.coerce(value, param, arg.value.span)?;
            reads.add(self, value);
        }
        let read = reads.finish(self, span);
        let mut values: Vec<Option<Operand>> = vec![None; params.len()];
        for (at, value) in slots.into_iter().zip(read) {
            if let Some(slot) = values.get_mut(at) {
                *slot = Some(value);
            }
        }
        let values = values
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or("its call to a closure leaves a parameter without a value")?;
        let dest = keep.filter(|ty| *ty != Type::Unit).map(|ty| self.temporary(ty));
        self.emit(InstKind::CallClosure { dest, closure, args: values }, span);
        Ok(dest)
    }

    /// The parameter each of a closure call's arguments lands in: in order, or, for one that names
    /// it, the nested function's parameter of that name, which the checker typed the callee as.
    fn closure_slots(&self, callee: &Expr, args: &[Arg], count: usize) -> Lowered<Vec<usize>> {
        if args.iter().all(|arg| arg.name.is_none()) {
            return Ok((0..args.len()).collect());
        }
        let Some(Type::Function(key)) = self.type_at(callee.span) else {
            return Err("its call to a closure names an argument, and it isn't a nested function".to_owned());
        };
        let signature = self.context.signatures.get(&key).ok_or("a nested function has no signature")?;
        let names: Vec<&str> = signature.params.iter().map(|param| param.name.as_str()).collect();
        if names.len() != count {
            return Err("a nested function takes other parameters than its closure".to_owned());
        }
        args.iter()
            .enumerate()
            .map(|(position, arg)| match &arg.name {
                Some(name) => names.iter().position(|param| param == name).ok_or_else(|| {
                    format!("its call to a closure names `{name}`, which it has no parameter for")
                }),
                None => Ok(position),
            })
            .collect()
    }

    /// The finished function, and those lifted out of it. `params` counts its leading
    /// locals that are parameters.
    pub(super) fn finished(self, params: usize, mut_params: Vec<usize>) -> Vec<Function> {
        let mut functions = vec![Function {
            name: self.name.clone(),
            params,
            mut_params,
            returns: self.returns,
            locals: self.locals,
            blocks: reachable(self.blocks),
            counts: self.counts,
            places: self.places,
        }];
        functions.extend(self.lifted);
        functions
    }
}
