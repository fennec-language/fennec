//! Lowering a checked module to the IR, one function at a time. The types come from the
//! checker's typed tree, never from working them out again.

use std::collections::{HashMap, HashSet};

use fennec_check::{Body, Checked, Type};
use fennec_syntax::{Arg, BinOp, Expr, ExprKind, FnDef, Item, Module, Span, Stmt, StmtKind, UnaryOp};

use crate::{
    Block, BlockId, Function, Inst, InstKind, Local, LocalId, NotLowered, Operand, Program, Terminator,
    type_in,
};

/// Lowers a module's functions and methods. The standard library's aren't included.
pub fn lower(module: &Module, checked: &Checked) -> Program {
    let functions = definitions(module);
    let known: HashSet<&str> = functions.iter().map(|(key, _)| key.as_str()).collect();
    let mut program = Program::default();
    for (key, def) in &functions {
        match Lowering::function(key, def, checked, &known) {
            Ok(function) => program.functions.push(function),
            Err(why) => program.not_lowered.push(NotLowered { function: key.clone(), why }),
        }
    }
    program
}

/// Every function and method in a module, by its key in the typed tree.
fn definitions(module: &Module) -> Vec<(String, &FnDef)> {
    let mut found = Vec::new();
    let methods = |owner: &str, defs: &'_ [FnDef]| -> Vec<String> {
        defs.iter().map(|def| format!("{owner}.{}", def.name)).collect()
    };
    for item in &module.items {
        match item {
            Item::Function(def) => found.push((def.name.clone(), def)),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                found.extend(methods(&def.name, &def.methods).into_iter().zip(&def.methods));
            }
            Item::Extend(extension) => {
                found
                    .extend(methods(&extension.name, &extension.methods).into_iter().zip(&extension.methods));
            }
            _ => {}
        }
    }
    found
}

/// What stopped a function being lowered.
type Lowered<T> = Result<T, String>;

/// A block being built. It has no terminator until it's finished.
#[derive(Default)]
struct Open {
    insts: Vec<Inst>,
    end: Option<Terminator>,
}

/// Where `break` and `continue` go.
struct Loop {
    head: BlockId,
    exit: BlockId,
}

struct Lowering<'a> {
    checked: &'a Checked,
    body: &'a Body,
    known: &'a HashSet<&'a str>,
    returns: Type,
    locals: Vec<Local>,
    blocks: Vec<Open>,
    current: BlockId,
    /// The names in each block being lowered, innermost last.
    scopes: Vec<HashMap<String, LocalId>>,
    loops: Vec<Loop>,
}

/// Whether the IR holds values of this type yet.
fn scalar(ty: &Type) -> bool {
    matches!(ty, Type::Int | Type::Float | Type::Bool | Type::Str)
}

impl<'a> Lowering<'a> {
    fn function(
        key: &str,
        def: &FnDef,
        checked: &'a Checked,
        known: &'a HashSet<&'a str>,
    ) -> Lowered<Function> {
        let signature = checked.signatures.get(key).ok_or("it has no signature")?;
        let body = checked.types.body(key).ok_or("it has no types")?;
        if !def.generics.is_empty() {
            return Err("it's generic".to_owned());
        }
        if signature.generator {
            return Err("it's a generator".to_owned());
        }
        if signature.can_fail {
            return Err("it can fail".to_owned());
        }
        if signature.returns != Type::Unit && !scalar(&signature.returns) {
            return Err(format!("it gives back {}", signature.returns));
        }
        let mut lowering = Lowering {
            checked,
            body,
            known,
            returns: signature.returns.clone(),
            locals: Vec::new(),
            blocks: vec![Open::default()],
            current: BlockId(0),
            scopes: vec![HashMap::new()],
            loops: Vec::new(),
        };
        for param in &signature.params {
            if param.is_mut {
                return Err(format!("`{}` is a `mut` parameter", param.name));
            }
            if !scalar(&param.ty) {
                return Err(format!("`{}` is {}", param.name, param.ty));
            }
            lowering.declare(&param.name, param.ty.clone());
        }
        lowering.stmts(&def.body, true)?;
        // A function with a return type has already returned, or the checker would object.
        let end =
            if lowering.returns == Type::Unit { Terminator::Return(None) } else { Terminator::Unreachable };
        lowering.finish(end);
        Ok(Function {
            name: key.to_owned(),
            params: signature.params.len(),
            returns: lowering.returns,
            locals: lowering.locals,
            blocks: reachable(lowering.blocks),
        })
    }

    fn declare(&mut self, name: &str, ty: Type) -> LocalId {
        let id = LocalId(self.locals.len());
        self.locals.push(Local { name: Some(name.to_owned()), ty });
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_owned(), id);
        }
        id
    }

    fn temporary(&mut self, ty: Type) -> LocalId {
        self.locals.push(Local { name: None, ty });
        LocalId(self.locals.len() - 1)
    }

    fn lookup(&self, name: &str) -> Option<LocalId> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name).copied())
    }

    fn new_block(&mut self) -> BlockId {
        self.blocks.push(Open::default());
        BlockId(self.blocks.len() - 1)
    }

    /// Whether the current block can still take instructions.
    fn is_open(&self) -> bool {
        self.blocks.get(self.current.0).is_some_and(|block| block.end.is_none())
    }

    /// Ends the current block, unless it's ended already.
    fn finish(&mut self, end: Terminator) {
        if let Some(block) = self.blocks.get_mut(self.current.0)
            && block.end.is_none()
        {
            block.end = Some(end);
        }
    }

    fn emit(&mut self, kind: InstKind, span: Span) {
        if let Some(block) = self.blocks.get_mut(self.current.0) {
            block.insts.push(Inst { kind, span });
        }
    }

    /// The scalar type the typed tree has for an expression.
    fn ty(&self, expr: &Expr) -> Lowered<Type> {
        let ty = self.body.expr(expr.span).ok_or("an expression has no type")?;
        if scalar(ty) { Ok(ty.clone()) } else { Err(format!("it uses a value of type {ty}")) }
    }

    /// Lowers statements in order. Code after leaving early never runs, so it's skipped.
    /// `tail` says the value of the last one is what the function gives back.
    fn stmts(&mut self, stmts: &[Stmt], tail: bool) -> Lowered<()> {
        for (at, stmt) in stmts.iter().enumerate() {
            if !self.is_open() {
                break;
            }
            self.stmt(stmt, tail && at + 1 == stmts.len())?;
        }
        Ok(())
    }

    fn block(&mut self, stmts: &[Stmt], tail: bool) -> Lowered<()> {
        self.scopes.push(HashMap::new());
        let lowered = self.stmts(stmts, tail);
        self.scopes.pop();
        lowered
    }

    fn stmt(&mut self, stmt: &Stmt, tail: bool) -> Lowered<()> {
        match &stmt.kind {
            StmtKind::Expr(expr) if tail && self.returns != Type::Unit => {
                let value = self.expr(expr)?;
                self.finish(Terminator::Return(Some(value)));
            }
            StmtKind::Expr(expr) => self.effect(expr)?,
            StmtKind::Assign { target, op, value, ty: _, declare_mut: _ } => {
                self.assign(target, *op, value, stmt.span)?;
            }
            StmtKind::If { branches, otherwise } => self.if_stmt(branches, otherwise.as_deref(), tail)?,
            StmtKind::While { cond, body } => self.while_stmt(cond, body)?,
            StmtKind::Return(value) => {
                let value = value.as_ref().map(|value| self.expr(value)).transpose()?;
                self.finish(Terminator::Return(value));
            }
            StmtKind::Break | StmtKind::Continue => {
                let target = self.loops.last().ok_or("`break` outside a loop")?;
                let target = if matches!(stmt.kind, StmtKind::Break) { target.exit } else { target.head };
                self.finish(Terminator::Jump(target));
            }
            StmtKind::Pass => {}
            StmtKind::For { .. } => return Err("it has a `for` loop".to_owned()),
            StmtKind::Fail(_) => return Err("it has `fail`".to_owned()),
            StmtKind::Yield(_) => return Err("it has `yield`".to_owned()),
            StmtKind::Assert(_) => return Err("it has `assert`".to_owned()),
            StmtKind::With { .. } => return Err("it has `with`".to_owned()),
            StmtKind::Def(_) => return Err("it has a nested function".to_owned()),
        }
        Ok(())
    }

    fn assign(&mut self, target: &Expr, op: Option<BinOp>, value: &Expr, span: Span) -> Lowered<()> {
        let ExprKind::Name(name) = &target.kind else {
            return Err("it assigns to something other than a name".to_owned());
        };
        let value = self.expr(value)?;
        let dest = if let Some(dest) = self.lookup(name) {
            dest
        } else {
            let ty = self.body.name(target.span, name).ok_or_else(|| format!("`{name}` has no type"))?;
            if !scalar(ty) {
                return Err(format!("`{name}` is {ty}"));
            }
            self.declare(name, ty.clone())
        };
        match op {
            Some(op) => {
                self.check_operands(op, &Operand::Local(dest), &value)?;
                let kind = InstKind::Binary { dest, op, lhs: Operand::Local(dest), rhs: value };
                self.emit(kind, span);
            }
            None => self.emit(InstKind::Copy { dest, value }, span),
        }
        Ok(())
    }

    fn if_stmt(
        &mut self,
        branches: &[(Expr, Vec<Stmt>)],
        otherwise: Option<&[Stmt]>,
        tail: bool,
    ) -> Lowered<()> {
        let join = self.new_block();
        for (cond, body) in branches {
            let cond = self.expr(cond)?;
            let then = self.new_block();
            let next = self.new_block();
            self.finish(Terminator::Branch { cond, then, otherwise: next });
            self.current = then;
            self.block(body, tail)?;
            self.finish(Terminator::Jump(join));
            self.current = next;
        }
        self.block(otherwise.unwrap_or_default(), tail)?;
        self.finish(Terminator::Jump(join));
        self.current = join;
        Ok(())
    }

    fn while_stmt(&mut self, cond: &Expr, body: &[Stmt]) -> Lowered<()> {
        let head = self.new_block();
        let then = self.new_block();
        let exit = self.new_block();
        self.finish(Terminator::Jump(head));
        self.current = head;
        // `while True:` leaves only by `break`.
        if matches!(cond.kind, ExprKind::Bool(true)) {
            self.finish(Terminator::Jump(then));
        } else {
            let cond = self.expr(cond)?;
            self.finish(Terminator::Branch { cond, then, otherwise: exit });
        }
        self.current = then;
        self.loops.push(Loop { head, exit });
        let lowered = self.block(body, false);
        self.loops.pop();
        lowered?;
        self.finish(Terminator::Jump(head));
        self.current = exit;
        Ok(())
    }

    /// An expression whose value nothing keeps.
    fn effect(&mut self, expr: &Expr) -> Lowered<()> {
        if let ExprKind::Call { callee, args } = &expr.kind {
            let (function, args) = self.call_parts(callee, args)?;
            self.emit(InstKind::Call { dest: None, function, args }, expr.span);
            return Ok(());
        }
        self.expr(expr).map(|_| ())
    }

    fn expr(&mut self, expr: &Expr) -> Lowered<Operand> {
        let span = expr.span;
        match &expr.kind {
            ExprKind::Int(value) => Ok(Operand::Int(*value)),
            ExprKind::Float(value) => Ok(Operand::Float(*value)),
            ExprKind::Bool(value) => Ok(Operand::Bool(*value)),
            ExprKind::Str(value) => Ok(Operand::Str(value.clone())),
            ExprKind::Name(name) => self
                .lookup(name)
                .map(Operand::Local)
                .ok_or_else(|| format!("it reads `{name}`, which isn't a local")),
            ExprKind::Unary { op, expr: inner } => self.unary(*op, inner, expr),
            ExprKind::Binary { op: op @ (BinOp::And | BinOp::Or), lhs, rhs } => {
                self.short_circuit(*op, lhs, rhs, span)
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let ty = self.ty(expr)?;
                let lhs = self.expr(lhs)?;
                let rhs = self.expr(rhs)?;
                self.check_operands(*op, &lhs, &rhs)?;
                let dest = self.temporary(ty);
                self.emit(InstKind::Binary { dest, op: *op, lhs, rhs }, span);
                Ok(Operand::Local(dest))
            }
            ExprKind::Cond { then, cond, otherwise } => self.conditional(expr, (then, cond, otherwise)),
            ExprKind::Call { callee, args } => {
                let ty = self.ty(expr)?;
                let (function, args) = self.call_parts(callee, args)?;
                let dest = self.temporary(ty);
                self.emit(InstKind::Call { dest: Some(dest), function, args }, span);
                Ok(Operand::Local(dest))
            }
            other => Err(format!("it uses {}", describe(other))),
        }
    }

    fn unary(&mut self, op: UnaryOp, inner: &Expr, expr: &Expr) -> Lowered<Operand> {
        let ty = self.ty(expr)?;
        let value = self.expr(inner)?;
        let dest = self.temporary(ty);
        self.emit(InstKind::Unary { dest, op, value }, expr.span);
        Ok(Operand::Local(dest))
    }

    /// `and` and `or`: the right side only runs when the left doesn't settle it.
    fn short_circuit(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span) -> Lowered<Operand> {
        let dest = self.temporary(Type::Bool);
        let value = self.expr(lhs)?;
        self.emit(InstKind::Copy { dest, value }, span);
        let right = self.new_block();
        let join = self.new_block();
        let cond = Operand::Local(dest);
        self.finish(if op == BinOp::And {
            Terminator::Branch { cond, then: right, otherwise: join }
        } else {
            Terminator::Branch { cond, then: join, otherwise: right }
        });
        self.current = right;
        let value = self.expr(rhs)?;
        self.emit(InstKind::Copy { dest, value }, span);
        self.finish(Terminator::Jump(join));
        self.current = join;
        Ok(Operand::Local(dest))
    }

    /// `then if cond else otherwise`.
    fn conditional(
        &mut self,
        expr: &Expr,
        (then, cond, otherwise): (&Expr, &Expr, &Expr),
    ) -> Lowered<Operand> {
        let dest = self.temporary(self.ty(expr)?);
        let cond = self.expr(cond)?;
        let (yes, no, join) = (self.new_block(), self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond, then: yes, otherwise: no });
        for (block, side) in [(yes, then), (no, otherwise)] {
            self.current = block;
            let value = self.expr(side)?;
            self.emit(InstKind::Copy { dest, value }, side.span);
            self.finish(Terminator::Jump(join));
        }
        self.current = join;
        Ok(Operand::Local(dest))
    }

    /// The callee and arguments of a direct call to a function of this module, with one
    /// plain value given for each parameter.
    fn call_parts(&mut self, callee: &Expr, args: &[Arg]) -> Lowered<(String, Vec<Operand>)> {
        let ExprKind::Name(name) = &callee.kind else {
            return Err("it calls something other than a function by name".to_owned());
        };
        if self.lookup(name).is_some() || !self.known.contains(name.as_str()) {
            return Err(format!("it calls `{name}`, which isn't a function of this program"));
        }
        let signature = self.checked.signatures.get(name).ok_or("a callee has no signature")?;
        if signature.can_fail || signature.generator || args.len() != signature.params.len() {
            return Err(format!("its call to `{name}` isn't a plain call"));
        }
        if !signature.params.iter().all(|param| scalar(&param.ty)) {
            return Err(format!("`{name}` takes something other than plain values"));
        }
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            if arg.name.is_some() || arg.is_mut {
                return Err(format!("its call to `{name}` has a named or `mut` argument"));
            }
            values.push(self.expr(&arg.value)?);
        }
        Ok((name.clone(), values))
    }

    /// Only operators on plain values of one type are lowered: anything else, like `in` or
    /// a type's own operator, isn't yet.
    fn check_operands(&self, op: BinOp, lhs: &Operand, rhs: &Operand) -> Lowered<()> {
        let (left, right) = (type_in(&self.locals, lhs), type_in(&self.locals, rhs));
        let fits = left == right
            && match op {
                BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => true,
                BinOp::Add => matches!(left, Some(Type::Int | Type::Float | Type::Str)),
                BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::FloorDiv | BinOp::Mod | BinOp::Pow => {
                    matches!(left, Some(Type::Int | Type::Float))
                }
                BinOp::And | BinOp::Or | BinOp::In | BinOp::NotIn | BinOp::Is | BinOp::IsNot => false,
            };
        if fits { Ok(()) } else { Err(format!("it uses `{}` in a way that isn't lowered yet", op.symbol())) }
    }
}

/// What an expression that isn't lowered yet is, for saying why.
fn describe(kind: &ExprKind) -> &'static str {
    match kind {
        ExprKind::FStr(_) => "an f-string",
        ExprKind::None => "`None`",
        ExprKind::Variant(_) => "an enum case",
        ExprKind::List(_) | ExprKind::Tuple(_) | ExprKind::Dict(_) | ExprKind::Set(_) => "a collection",
        ExprKind::Comprehension { .. } => "a comprehension",
        ExprKind::Else { .. } => "`else`",
        ExprKind::Try { .. } => "`?`",
        ExprKind::Attr { .. } => "a field or method",
        ExprKind::Index { .. } | ExprKind::Slice { .. } => "indexing",
        ExprKind::Lambda { .. } => "an arrow function",
        ExprKind::Match { .. } => "`match`",
        _ => "something that isn't lowered yet",
    }
}

/// The blocks control can reach, in order, with their targets renumbered. A join that every
/// branch left early never gets used, so it goes.
fn reachable(blocks: Vec<Open>) -> Vec<Block> {
    let ends = |block: &Open| block.end.as_ref().map_or_else(Vec::new, Terminator::targets);
    let mut seen = vec![false; blocks.len()];
    let mut waiting = vec![BlockId(0)];
    while let Some(block) = waiting.pop() {
        if let Some(slot) = seen.get_mut(block.0)
            && !*slot
        {
            *slot = true;
            waiting.extend(blocks.get(block.0).map(ends).unwrap_or_default());
        }
    }
    let mut renumbered = HashMap::new();
    for (at, _) in seen.iter().enumerate().filter(|(_, seen)| **seen) {
        renumbered.insert(at, BlockId(renumbered.len()));
    }
    let moved = |block: BlockId| renumbered.get(&block.0).copied().unwrap_or(block);
    blocks
        .into_iter()
        .zip(seen)
        .filter(|(_, seen)| *seen)
        .map(|(block, _)| Block {
            insts: block.insts,
            end: match block.end {
                Some(Terminator::Jump(target)) => Terminator::Jump(moved(target)),
                Some(Terminator::Branch { cond, then, otherwise }) => {
                    Terminator::Branch { cond, then: moved(then), otherwise: moved(otherwise) }
                }
                Some(other) => other,
                None => Terminator::Unreachable,
            },
        })
        .collect()
}
