//! Arrow functions, nested functions and functions as values. Each is lifted to a function
//! of its own, whose first parameters are what it captures. Capturing copies, when the
//! closure is made, so it sees the values as they were then.

use fennec_check::Type;
use fennec_syntax::{Arg, Expr, ExprKind, FnDef, Span, visit};

use super::{Lowered, Lowering, Tail, reachable};
use crate::{Function, InstKind, LocalId, Operand};

/// What a closure captures: a local of the function making it, under its name.
type Captures = Vec<(String, LocalId)>;

impl Lowering<'_> {
    /// The locals an arrow function or nested function reads from here, in the order it
    /// first names them.
    fn captures(&self, mut names: Vec<String>, own: &[&str]) -> Captures {
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
    fn make_closure(&mut self, function: String, captures: &Captures, ty: Type, span: Span) -> LocalId {
        let dest = self.temporary(ty);
        let captures = captures.iter().map(|(_, local)| Operand::Local(*local)).collect();
        self.emit(InstKind::MakeClosure { dest, function, captures }, span);
        dest
    }

    /// `(x) => body`.
    pub(super) fn arrow(&mut self, expr: &Expr, params: &[String], body: &Expr) -> Lowered<Operand> {
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
        let key = format!("{}.arrow@{}", self.name, expr.span.start);
        let mut lifted = Lowering::new(&key, self.context, self.body, *returns.clone());
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
        self.lifted.extend(lifted.finished(count, Vec::new()));
        Ok(Operand::Local(self.make_closure(key, &captures, ty, expr.span)))
    }

    /// `def inner(...)` inside a function: its name holds a closure from here on.
    pub(super) fn nested(&mut self, def: &FnDef, span: Span) -> Lowered<()> {
        let key = format!("{}.{}", self.name, def.name);
        let context = self.context;
        let signature = context.checked.signatures.get(&key).ok_or("a nested function has no signature")?;
        if signature.params.iter().any(|param| param.is_mut) {
            return Err("it has a nested function with a `mut` parameter".to_owned());
        }
        let ty = context.arrow_of(&key).ok_or("a nested function takes what the IR doesn't hold")?;
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
        self.lifted.extend(Lowering::function(&key, def, context, (&held, itself))?);
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
            ExprKind::Name(name) => self.local_type(self.lookup(name)?),
            _ => self.body.expr(callee.span)?.clone(),
        };
        matches!(ty, Type::Arrow(..)).then_some(ty)
    }

    /// A call to a closure. Its arguments are positional, since its type has no names.
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
        let closure = self.expr(callee)?;
        let mut values = Vec::with_capacity(args.len());
        for (arg, param) in args.iter().zip(params) {
            if arg.name.is_some() || arg.is_mut {
                return Err("its call to a closure has a named or `mut` argument".to_owned());
            }
            let value = self.expr(&arg.value)?;
            values.push(self.coerce(value, param, arg.value.span)?);
        }
        let dest = keep.filter(|ty| *ty != Type::Unit).map(|ty| self.temporary(ty));
        self.emit(InstKind::CallClosure { dest, closure, args: values }, span);
        Ok(dest)
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
        }];
        functions.extend(self.lifted);
        functions
    }
}
