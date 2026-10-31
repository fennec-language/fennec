//! Lowering a checked module to the IR, one function at a time. The types come from the
//! checker's typed tree, never from working them out again.

mod calls;
mod collections;
mod constants;
mod defaults;
mod failure;
mod lists;
mod matching;
mod operators;
mod places;
mod structs;

use std::collections::{HashMap, HashSet};

use fennec_check::{Body, Checked, Type};
use fennec_syntax::{BinOp, Expr, ExprKind, FnDef, Item, Module, Span, Stmt, StmtKind, UnaryOp};

use crate::{
    Block, BlockId, EnumDef, Function, Inst, InstKind, Local, LocalId, NotLowered, Operand, Program,
    Terminator,
};

/// Lowers a module's functions and methods. The standard library's aren't included.
pub fn lower(module: &Module, checked: &Checked) -> Program {
    let functions = definitions(module);
    let defs = structs::definitions(module, checked);
    let enums = matching::definitions(module, checked);
    let errors = failure::definitions(checked);
    let context = Context {
        checked,
        errors: errors.iter().map(|def| def.name.as_str()).collect(),
        enums: enums.iter().map(|def| (def.name.as_str(), def)).collect(),
        functions: functions.iter().map(|(key, _)| key.as_str()).collect(),
        structs: defs.iter().map(|def| def.name.as_str()).collect(),
        constants: constants::written(module),
        defaults: defaults::written(module),
    };
    let mut program = Program::default();
    for (key, def) in &functions {
        match Lowering::function(key, def, &context) {
            Ok(function) => program.functions.push(function),
            Err(why) => program.not_lowered.push(NotLowered { function: key.clone(), why }),
        }
    }
    constants::lower_all(&context, &mut program);
    program.external = calls::external(&program);
    program.structs = defs;
    program.enums = enums;
    program.errors = errors;
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

/// The capabilities, whose methods the engines provide.
const CAPABILITIES: [&str; 5] = ["Stdout", "Fs", "Clock", "Args", "Stdin"];

/// What's known about the whole program.
struct Context<'a> {
    checked: &'a Checked,
    /// The program's enums that the IR can hold.
    enums: HashMap<&'a str, &'a EnumDef>,
    /// The error types that the IR can hold.
    errors: HashSet<&'a str>,
    /// The keys of the program's functions and methods.
    functions: HashSet<&'a str>,
    /// The names of the program's structs that the IR can hold.
    structs: HashSet<&'a str>,
    /// The values of the constants the program can read, the standard library's included.
    constants: HashMap<&'a str, constants::Written<'a>>,
    /// The fields with defaults, the standard library's included.
    defaults: HashMap<defaults::FieldKey<'a>, &'a Expr>,
}

impl Context<'_> {
    /// Whether the IR holds values of this type yet.
    fn holds(&self, ty: &Type) -> bool {
        match ty {
            Type::Int | Type::Float | Type::Bool | Type::Str | Type::Bytes | Type::None => true,
            Type::List(item) | Type::Set(item) | Type::Optional(item) => self.holds(item),
            Type::Dict(key, value) => self.holds(key) && self.holds(value),
            Type::Tuple(items) => items.iter().all(|item| self.holds(item)),
            Type::Result(ok, error) => (**ok == Type::Unit || self.holds(ok)) && self.holds(error),
            Type::Errors(names) => names.iter().all(|name| self.errors.contains(name.as_str())),
            Type::Named(name) => {
                self.structs.contains(name.as_str())
                    || self.enums.contains_key(name.as_str())
                    || self.errors.contains(name.as_str())
                    || CAPABILITIES.contains(&name.as_str())
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
    /// The `for mut` loops being lowered, innermost last.
    leaving: Vec<places::Leaving>,
}

impl<'a> Lowering<'a> {
    fn new(context: &'a Context<'a>, body: &'a Body, returns: Type) -> Self {
        Lowering {
            context,
            body,
            returns,
            locals: Vec::new(),
            blocks: vec![Open::default()],
            current: BlockId(0),
            scopes: vec![HashMap::new()],
            loops: Vec::new(),
            leaving: Vec::new(),
        }
    }

    fn function(key: &str, def: &FnDef, context: &'a Context<'a>) -> Lowered<Function> {
        let signature = context.checked.signatures.get(key).ok_or("it has no signature")?;
        let body = context.checked.types.body(key).ok_or("it has no types")?;
        if !def.generics.is_empty() {
            return Err("it's generic".to_owned());
        }
        if signature.generator {
            return Err("it's a generator".to_owned());
        }
        if signature.returns != Type::Unit && !context.holds(&signature.returns) {
            return Err(format!("it gives back {}", signature.returns));
        }
        let mut lowering = Lowering::new(context, body, signature.returns.clone());
        for param in &signature.params {
            if !context.holds(&param.ty) {
                return Err(format!("`{}` is {}", param.name, param.ty));
            }
            lowering.declare(&param.name, param.ty.clone());
        }
        let tail = if lowering.returns == Type::Unit { Tail::Drop } else { Tail::Return };
        lowering.stmts(&def.body, tail)?;
        lowering.end_function(def.span)?;
        Ok(Function {
            name: key.to_owned(),
            params: signature.params.len(),
            mut_params: signature
                .params
                .iter()
                .enumerate()
                .filter(|(_, param)| param.is_mut)
                .map(|(at, _)| at)
                .collect(),
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

    fn local_type(&self, local: LocalId) -> Type {
        self.locals.get(local.0).map_or(Type::Unknown, |held| held.ty.clone())
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
            StmtKind::Return(Some(value)) => self.return_value(value)?,
            StmtKind::Return(None) => self.return_nothing(stmt.span)?,
            StmtKind::Break | StmtKind::Continue => {
                let target = self.loops.last().ok_or("`break` outside a loop")?;
                let target = if matches!(stmt.kind, StmtKind::Break) { target.exit } else { target.next };
                self.finish(Terminator::Jump(target));
            }
            StmtKind::Pass => {}
            StmtKind::For { names, is_mut, iter, body } => {
                self.for_stmt((names, *is_mut), iter, body, stmt.span)?;
            }
            StmtKind::Fail(error) => self.fail(error, stmt.span)?,
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
            Tail::Return => self.return_value(expr)?,
            Tail::Into(dest) => {
                let value = self.expr(expr)?;
                let ty = self.local_type(dest);
                let value = self.coerce(value, &ty, expr.span)?;
                self.emit(InstKind::Copy { dest, value }, expr.span);
            }
        }
        Ok(())
    }

    fn assign(&mut self, target: &Expr, op: Option<BinOp>, value: &Expr, span: Span) -> Lowered<()> {
        if let ExprKind::Tuple(targets) = &target.kind {
            return self.assign_unpacking(targets, value, span);
        }
        let ExprKind::Name(name) = &target.kind else {
            return self.assign_place(target, op, value, span);
        };
        let value = self.expr(value)?;
        let dest = self.named_target(target, name)?;
        if let Some(op) = op {
            let ty = self.local_type(dest);
            if self.check_operands(op, &Operand::Local(dest), &value).is_ok() {
                let kind = InstKind::Binary { dest, op, lhs: Operand::Local(dest), rhs: value };
                self.emit(kind, span);
            } else {
                let value = self.operate(op, (Operand::Local(dest), value), &ty, span)?;
                self.emit(InstKind::Copy { dest, value }, span);
            }
        } else {
            let ty = self.local_type(dest);
            let value = self.coerce(value, &ty, span)?;
            self.emit(InstKind::Copy { dest, value }, span);
        }
        Ok(())
    }

    /// The local a name being assigned to holds its value in, declared if it's new.
    fn named_target(&mut self, target: &Expr, name: &str) -> Lowered<LocalId> {
        if let Some(dest) = self.lookup(name) {
            return Ok(dest);
        }
        let ty = self.body.name(target.span, name).ok_or_else(|| format!("`{name}` has no type"))?;
        if !self.context.holds(ty) {
            return Err(format!("`{name}` is {ty}"));
        }
        Ok(self.declare(name, ty.clone()))
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

    /// An expression whose value nothing keeps.
    fn effect(&mut self, expr: &Expr) -> Lowered<()> {
        if let ExprKind::Call { callee, args } = &expr.kind
            && self.constructs(callee).is_none()
            && self.built_case(expr).is_none()
        {
            return self.call(callee, args, None, expr.span).map(|_| ());
        }
        if let ExprKind::Try { value, context } = &expr.kind {
            return self.try_value(value, context.as_deref(), expr.span).map(|_| ());
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
            ExprKind::None => Ok(Operand::None),
            ExprKind::Name(name) => {
                let Some(local) = self.lookup(name) else { return self.read_constant(name, expr) };
                self.narrowed(Operand::Local(local), expr)
            }
            ExprKind::Binary { op: op @ (BinOp::Is | BinOp::IsNot), lhs, rhs } => {
                self.is_none(*op, lhs, rhs, span)
            }
            ExprKind::Else { value, default } => self.else_value(expr, value, default),
            ExprKind::Try { value, context } => self
                .try_value(value, context.as_deref(), span)?
                .ok_or_else(|| "it uses the nothing `?` gives back".to_owned()),
            ExprKind::Unary { op, expr: inner } => self.unary(*op, inner, expr),
            ExprKind::Binary { op: op @ (BinOp::And | BinOp::Or), lhs, rhs } => {
                self.short_circuit(*op, lhs, rhs, span)
            }
            ExprKind::Binary { op: op @ (BinOp::In | BinOp::NotIn), lhs, rhs } => {
                self.contains(*op, lhs, rhs, span)
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let ty = self.ty(expr)?;
                let lhs = self.expr(lhs)?;
                let rhs = self.expr(rhs)?;
                self.operate(*op, (lhs, rhs), &ty, span)
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
                let dest = self.call(callee, args, Some(self.ty(expr)?), span)?;
                dest.map(Operand::Local).ok_or_else(|| "a call kept nothing".to_owned())
            }
            ExprKind::List(items) => self.list(expr, items),
            ExprKind::Set(items) => self.set(expr, items),
            ExprKind::Dict(pairs) => self.dict(expr, pairs),
            ExprKind::Tuple(items) => self.tuple(expr, items),
            ExprKind::Comprehension { kind, elem, value, names, iter, cond } => {
                let parts = collections::Parts { elem, value: value.as_deref(), cond: cond.as_deref() };
                self.comprehension(expr, *kind, &parts, (names, iter))
            }
            ExprKind::Index { object, index } => self.get_index(expr, object, index),
            ExprKind::Slice { object, start, end } => {
                self.slice(expr, object, (start.as_deref(), end.as_deref()))
            }
            ExprKind::FStr(parts) => self.format(parts, span),
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
            let ty = self.local_type(dest);
            let value = self.coerce(value, &ty, side.span)?;
            self.emit(InstKind::Copy { dest, value }, side.span);
            self.finish(Terminator::Jump(join));
        }
        self.current = join;
        Ok(Operand::Local(dest))
    }
}

/// What an expression that isn't lowered yet is, for saying why.
fn describe(kind: &ExprKind) -> &'static str {
    match kind {
        ExprKind::FStr(_) => "an f-string",
        ExprKind::None => "`None`",
        ExprKind::Variant(_) => "an enum case",
        ExprKind::Else { .. } => "`else`",
        ExprKind::Try { .. } => "`?`",
        ExprKind::Attr { .. } => "a field or method",
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
