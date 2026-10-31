//! Lowering a checked module to the IR, one function at a time. The types come from the
//! checker's typed tree, never from working them out again.

mod matching;
mod structs;

use std::collections::{HashMap, HashSet};

use fennec_check::{Body, Checked, Type};
use fennec_syntax::{BinOp, Expr, ExprKind, FnDef, Item, Module, Span, Stmt, StmtKind, UnaryOp};

use crate::{
    Block, BlockId, EnumDef, Function, Inst, InstKind, Local, LocalId, NotLowered, Operand, Program,
    Terminator, type_in,
};

/// Lowers a module's functions and methods. The standard library's aren't included.
pub fn lower(module: &Module, checked: &Checked) -> Program {
    let functions = definitions(module);
    let defs = structs::definitions(module, checked);
    let enums = matching::definitions(module, checked);
    let context = Context {
        checked,
        enums: enums.iter().map(|def| (def.name.as_str(), def)).collect(),
        functions: functions.iter().map(|(key, _)| key.as_str()).collect(),
        structs: defs.iter().map(|def| def.name.as_str()).collect(),
    };
    let mut program = Program::default();
    for (key, def) in &functions {
        match Lowering::function(key, def, &context) {
            Ok(function) => program.functions.push(function),
            Err(why) => program.not_lowered.push(NotLowered { function: key.clone(), why }),
        }
    }
    program.structs = defs;
    program.enums = enums;
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

/// What becomes of the value of the last statement of a block.
#[derive(Clone, Copy)]
enum Tail {
    /// Nothing keeps it.
    Drop,
    /// The function gives it back.
    Return,
    /// It's the value of a `match`, kept in this local.
    Into(LocalId),
}

/// Where `break` and `continue` go.
struct Loop {
    next: BlockId,
    exit: BlockId,
}

/// What's known about the whole program.
struct Context<'a> {
    checked: &'a Checked,
    /// The program's enums that the IR can hold.
    enums: HashMap<&'a str, &'a EnumDef>,
    /// The keys of the program's functions and methods.
    functions: HashSet<&'a str>,
    /// The names of the program's structs that the IR can hold.
    structs: HashSet<&'a str>,
}

impl Context<'_> {
    /// Whether the IR holds values of this type yet.
    fn holds(&self, ty: &Type) -> bool {
        match ty {
            Type::Int | Type::Float | Type::Bool | Type::Str => true,
            Type::Named(name) => {
                self.structs.contains(name.as_str()) || self.enums.contains_key(name.as_str())
            }
            _ => false,
        }
    }
}

struct Lowering<'a> {
    context: &'a Context<'a>,
    body: &'a Body,
    returns: Type,
    locals: Vec<Local>,
    blocks: Vec<Open>,
    current: BlockId,
    /// The names in each block being lowered, innermost last.
    scopes: Vec<HashMap<String, LocalId>>,
    loops: Vec<Loop>,
}

impl<'a> Lowering<'a> {
    fn function(key: &str, def: &FnDef, context: &'a Context<'a>) -> Lowered<Function> {
        let signature = context.checked.signatures.get(key).ok_or("it has no signature")?;
        let body = context.checked.types.body(key).ok_or("it has no types")?;
        if !def.generics.is_empty() {
            return Err("it's generic".to_owned());
        }
        if signature.generator {
            return Err("it's a generator".to_owned());
        }
        if signature.can_fail {
            return Err("it can fail".to_owned());
        }
        if signature.returns != Type::Unit && !context.holds(&signature.returns) {
            return Err(format!("it gives back {}", signature.returns));
        }
        let mut lowering = Lowering {
            context,
            body,
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
            if !context.holds(&param.ty) {
                return Err(format!("`{}` is {}", param.name, param.ty));
            }
            lowering.declare(&param.name, param.ty.clone());
        }
        let tail = if lowering.returns == Type::Unit { Tail::Drop } else { Tail::Return };
        lowering.stmts(&def.body, tail)?;
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

    /// The type the typed tree has for an expression, when the IR can hold it.
    fn ty(&self, expr: &Expr) -> Lowered<Type> {
        let ty = self.body.expr(expr.span).ok_or("an expression has no type")?;
        if self.context.holds(ty) { Ok(ty.clone()) } else { Err(format!("it uses a value of type {ty}")) }
    }

    /// Lowers statements in order. Code after leaving early never runs, so it's skipped.
    /// `tail` says what becomes of the value of the last one.
    fn stmts(&mut self, stmts: &[Stmt], tail: Tail) -> Lowered<()> {
        for (at, stmt) in stmts.iter().enumerate() {
            if !self.is_open() {
                break;
            }
            self.stmt(stmt, if at + 1 == stmts.len() { tail } else { Tail::Drop })?;
        }
        Ok(())
    }

    fn block(&mut self, stmts: &[Stmt], tail: Tail) -> Lowered<()> {
        self.scopes.push(HashMap::new());
        let lowered = self.stmts(stmts, tail);
        self.scopes.pop();
        lowered
    }

    fn stmt(&mut self, stmt: &Stmt, tail: Tail) -> Lowered<()> {
        match &stmt.kind {
            StmtKind::Expr(expr) => self.tail_expr(expr, tail)?,
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
                let target = if matches!(stmt.kind, StmtKind::Break) { target.exit } else { target.next };
                self.finish(Terminator::Jump(target));
            }
            StmtKind::Pass => {}
            StmtKind::For { names, iter, body, .. } => self.for_stmt(names, iter, body, stmt.span)?,
            StmtKind::Fail(_) => return Err("it has `fail`".to_owned()),
            StmtKind::Yield(_) => return Err("it has `yield`".to_owned()),
            StmtKind::Assert(_) => return Err("it has `assert`".to_owned()),
            StmtKind::With { .. } => return Err("it has `with`".to_owned()),
            StmtKind::Def(_) => return Err("it has a nested function".to_owned()),
        }
        Ok(())
    }

    /// An expression statement, with what becomes of its value. Each case of a `match`
    /// passes its own value on the same way.
    fn tail_expr(&mut self, expr: &Expr, tail: Tail) -> Lowered<()> {
        if let ExprKind::Match { subject, arms } = &expr.kind {
            return self.match_expr(subject, arms, tail);
        }
        match tail {
            Tail::Drop => self.effect(expr)?,
            Tail::Return => {
                let value = self.expr(expr)?;
                self.finish(Terminator::Return(Some(value)));
            }
            Tail::Into(dest) => {
                let value = self.expr(expr)?;
                self.emit(InstKind::Copy { dest, value }, expr.span);
            }
        }
        Ok(())
    }

    fn assign(&mut self, target: &Expr, op: Option<BinOp>, value: &Expr, span: Span) -> Lowered<()> {
        let ExprKind::Name(name) = &target.kind else {
            return self.assign_field(target, op, value, span);
        };
        let value = self.expr(value)?;
        let dest = if let Some(dest) = self.lookup(name) {
            dest
        } else {
            let ty = self.body.name(target.span, name).ok_or_else(|| format!("`{name}` has no type"))?;
            if !self.context.holds(ty) {
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
        tail: Tail,
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
        self.loops.push(Loop { next: head, exit });
        let lowered = self.block(body, Tail::Drop);
        self.loops.pop();
        lowered?;
        self.finish(Terminator::Jump(head));
        self.current = exit;
        Ok(())
    }

    /// `for name in range(...)`, counting in a local of its own, so the loop's name is a
    /// fresh value each time around. The limit is read once. Going one step at a time, the
    /// count can't overflow, since it's short of the limit before each step.
    fn for_stmt(&mut self, names: &[String], iter: &Expr, body: &[Stmt], span: Span) -> Lowered<()> {
        const OVER: &str = "it has a `for` loop over something other than a `range` stepping by 1 or -1";
        let ([name], Some((args, down))) = (names, self.range_args(iter)) else {
            return Err(OVER.to_owned());
        };
        let (first, limit) = match args.as_slice() {
            [limit] => (Operand::Int(0), self.expr(limit)?),
            [first, limit] => (self.expr(first)?, self.expr(limit)?),
            _ => return Err(OVER.to_owned()),
        };
        let count = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: count, value: first }, span);
        let stop = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: stop, value: limit }, span);
        let (head, then, next, exit) =
            (self.new_block(), self.new_block(), self.new_block(), self.new_block());
        self.finish(Terminator::Jump(head));
        self.current = head;
        let going = self.temporary(Type::Bool);
        let (lhs, rhs) = (Operand::Local(count), Operand::Local(stop));
        let op = if down { BinOp::Gt } else { BinOp::Lt };
        self.emit(InstKind::Binary { dest: going, op, lhs, rhs }, span);
        self.finish(Terminator::Branch { cond: Operand::Local(going), then, otherwise: exit });
        self.current = then;
        self.scopes.push(HashMap::new());
        let item = self.declare(name, Type::Int);
        self.emit(InstKind::Copy { dest: item, value: Operand::Local(count) }, span);
        self.loops.push(Loop { next, exit });
        let lowered = self.stmts(body, Tail::Drop);
        self.loops.pop();
        self.scopes.pop();
        lowered?;
        self.finish(Terminator::Jump(next));
        self.current = next;
        let op = if down { BinOp::Sub } else { BinOp::Add };
        let lhs = Operand::Local(count);
        self.emit(InstKind::Binary { dest: count, op, lhs, rhs: Operand::Int(1) }, span);
        self.finish(Terminator::Jump(head));
        self.current = exit;
        Ok(())
    }

    /// The first and limit arguments of a call to the built-in `range` with a step of 1 or
    /// -1, and whether it counts down.
    fn range_args<'e>(&self, iter: &'e Expr) -> Option<(Vec<&'e Expr>, bool)> {
        let ExprKind::Call { callee, args } = &iter.kind else { return None };
        let ExprKind::Name(name) = &callee.kind else { return None };
        let plain = args.iter().all(|arg| arg.name.is_none() && !arg.is_mut);
        if name != "range"
            || !plain
            || self.lookup(name).is_some()
            || self.context.functions.contains("range")
        {
            return None;
        }
        let values: Vec<&Expr> = args.iter().map(|arg| &arg.value).collect();
        match values.as_slice() {
            [first, limit, step] => {
                let down = match &step.kind {
                    ExprKind::Int(1) => false,
                    ExprKind::Int(-1) => true,
                    ExprKind::Unary { op: UnaryOp::Neg, expr } if matches!(expr.kind, ExprKind::Int(1)) => {
                        true
                    }
                    _ => return None,
                };
                Some((vec![first, limit], down))
            }
            _ => Some((values, false)),
        }
    }

    /// An expression whose value nothing keeps.
    fn effect(&mut self, expr: &Expr) -> Lowered<()> {
        if let ExprKind::Call { callee, args } = &expr.kind
            && self.constructs(callee).is_none()
            && self.built_case(expr).is_none()
        {
            let (function, args) = self.call_parts(callee, args)?;
            self.emit(InstKind::Call { dest: None, function, args }, expr.span);
            return Ok(());
        }
        self.expr(expr).map(|_| ())
    }

    fn expr(&mut self, expr: &Expr) -> Lowered<Operand> {
        let span = expr.span;
        if let Some((owner, case, args)) = self.built_case(expr) {
            return self.make_case(&owner, case, args, span);
        }
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
            ExprKind::Attr { object, name, optional: false } => self.get_field(expr, object, name),
            ExprKind::Match { subject, arms } => {
                let dest = self.temporary(self.ty(expr)?);
                self.match_expr(subject, arms, Tail::Into(dest))?;
                Ok(Operand::Local(dest))
            }
            ExprKind::Call { callee, args } => {
                if let Some(type_name) = self.constructs(callee) {
                    return self.make(type_name, args, span);
                }
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

    /// Only operators on plain values of one type are lowered: anything else, like `in` or
    /// a type's own operator, isn't yet.
    fn check_operands(&self, op: BinOp, lhs: &Operand, rhs: &Operand) -> Lowered<()> {
        let (left, right) = (type_in(&self.locals, lhs), type_in(&self.locals, rhs));
        let fits = left == right
            && match op {
                BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                    matches!(left, Some(Type::Int | Type::Float | Type::Bool | Type::Str))
                }
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
