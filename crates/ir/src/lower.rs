//! Lowering a checked module to the IR, one function at a time. The types come from the
//! checker's typed tree, never from working them out again.

pub(crate) mod calls;
mod closures;
mod collections;
mod constants;
mod defaults;
mod failure;
mod generics;
mod imports;
mod lists;
mod matching;
mod operators;
mod places;
mod structs;

use std::collections::{HashMap, HashSet};

use fennec_check::{Body, Signatures, Type};
use fennec_syntax::{BinOp, Expr, ExprKind, FnDef, Item, Module, Span, Stmt, StmtKind, UnaryOp};

use crate::{
    Block, BlockId, EnumDef, Function, Inst, InstKind, Local, LocalId, NotLowered, Operand, Program,
    Terminator,
};

/// Lowers a module's functions and methods, and the standard library's that they reach.
pub fn lower(module: &Module, signatures: &Signatures) -> Program {
    let library = fennec_syntax::prelude_modules().unwrap_or_default();
    let modules: Vec<&Module> = std::iter::once(module).chain(library).collect();
    let functions = definitions(module);
    let every: HashMap<String, &FnDef> = library
        .iter()
        .flat_map(definitions)
        .chain(functions.iter().map(|(key, def)| (key.clone(), *def)))
        .collect();
    let reachable_later: HashMap<String, &FnDef> = library
        .iter()
        .flat_map(definitions)
        .filter(|(key, _)| !functions.iter().any(|(own, _)| own == key))
        .collect();
    let defs = structs::definitions(&modules, signatures);
    let enums = matching::definitions(&modules, signatures);
    let errors = failure::definitions(signatures);
    let context = Context {
        signatures,
        errors: errors.iter().map(|def| def.name.as_str()).collect(),
        enums: enums.iter().map(|def| (def.name.as_str(), def)).collect(),
        functions: functions
            .iter()
            .map(|(key, _)| key.as_str())
            .chain(reachable_later.keys().map(String::as_str))
            .collect(),
        structs: defs.iter().map(|def| def.name.as_str()).collect(),
        constants: constants::written(module),
        defaults: defaults::written(module),
        rules: defaults::rules(module),
        imports: imports::written(module),
        library_imports: imports::of_library(library),
        library: reachable_later.keys().map(String::as_str).collect(),
        generic_types: generic_types(&modules),
        instances: generics::Instances::default(),
    };
    let mut program = Program::default();
    for (key, def) in &functions {
        // Something generic is only lowered as the copies a program uses.
        let generic = signatures.get(key).is_some_and(generics::is_generic);
        if !generic && def.generics.is_empty() {
            lower_into(&mut program, &generics::FunctionInstance::plain(key), def, &context);
        }
    }
    constants::lower_all(&context, &mut program);
    // Copies of generics, and the standard library's functions, come in as they're reached.
    loop {
        let waiting = context.instances.waiting.borrow_mut().pop();
        let next = waiting.or_else(|| {
            calls::external(&program)
                .into_iter()
                .find(|key| reachable_later.contains_key(key))
                .map(|key| generics::FunctionInstance::plain(&key))
        });
        let Some(next) = next else { break };
        if let Some(def) = every.get(&next.base) {
            lower_into(&mut program, &next, def, &context);
            constants::lower_all(&context, &mut program);
        } else {
            let why = format!("it has no definition: `{}`", next.base);
            program.not_lowered.push(NotLowered { function: next.name, why });
        }
    }
    program.external = calls::external(&program);
    let (mut defs, mut enums) = (defs.clone(), enums.iter().map(|def| (*def).clone()).collect::<Vec<_>>());
    structs::add_copies(&context, &mut defs, &mut enums);
    program.structs = defs;
    program.enums = enums;
    program.errors = errors;
    program
}

fn lower_into(
    program: &mut Program,
    instance: &generics::FunctionInstance,
    def: &FnDef,
    context: &Context<'_>,
) {
    match Lowering::function(instance, def, context, (&[], None)) {
        Ok(functions) => program.functions.extend(functions),
        Err(why) => program.not_lowered.push(NotLowered { function: instance.name.clone(), why }),
    }
}

/// The generic structs and enums of a module and the standard library.
fn generic_types<'a>(modules: &[&'a Module]) -> HashMap<&'a str, &'a fennec_syntax::TypeDef> {
    let mut found = HashMap::new();
    for item in modules.iter().rev().flat_map(|module| &module.items) {
        if let Item::Struct(def) | Item::Enum(def) = item
            && !def.generics.is_empty()
        {
            found.insert(def.name.as_str(), def);
        }
    }
    found
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
    /// How many things to undo on leaving the function there were outside the loop.
    depth: usize,
}

/// The capabilities, whose methods the engines provide.
const CAPABILITIES: [&str; 5] = ["Stdout", "Fs", "Clock", "Args", "Stdin"];

/// What's known about the whole program.
struct Context<'a> {
    signatures: &'a Signatures,
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
    /// The `where` rules of each struct, the standard library's included, in field order.
    rules: HashMap<&'a str, Vec<(&'a str, &'a Expr)>>,
    /// What the module imports from the standard library.
    imports: imports::Imports,
    /// What the standard library's own code imports.
    library_imports: imports::Imports,
    /// The keys of the standard library's functions and methods.
    library: HashSet<&'a str>,
    /// The generic structs and enums, the standard library's included.
    generic_types: HashMap<&'a str, &'a fennec_syntax::TypeDef>,
    /// The copies of generics a program uses.
    instances: generics::Instances,
}

impl Context<'_> {
    /// The type of a function as a value, when the IR holds it.
    fn arrow_of(&self, key: &str) -> Option<Type> {
        let signature = self.signatures.get(key)?;
        let params = signature.params.iter().map(|param| param.ty.clone()).collect();
        let ty = Type::Arrow(params, Box::new(signature.returns.clone()));
        self.holds(&ty).then_some(ty)
    }

    /// Whether the IR holds values of this type yet.
    fn holds(&self, ty: &Type) -> bool {
        match ty {
            Type::Int | Type::Float | Type::Bool | Type::Str | Type::Bytes | Type::None => true,
            Type::List(item)
            | Type::Set(item)
            | Type::Optional(item)
            | Type::IdMap(item)
            | Type::Id(item) => self.holds(item),
            Type::Dict(key, value) => self.holds(key) && self.holds(value),
            Type::Tuple(items) => items.iter().all(|item| self.holds(item)),
            Type::Arrow(params, returns) => {
                params.iter().all(|param| self.holds(param))
                    && (**returns == Type::Unit || self.holds(returns))
            }
            Type::Function(key) => self.arrow_of(key).is_some(),
            Type::Result(ok, error) => (**ok == Type::Unit || self.holds(ok)) && self.holds(error),
            Type::Errors(names) => names.iter().all(|name| self.errors.contains(name.as_str())),
            Type::Named(name) => {
                self.structs.contains(name.as_str())
                    || self.instances.types.borrow().contains_key(name.as_str())
                    || self.enums.contains_key(name.as_str())
                    || self.errors.contains(name.as_str())
                    || CAPABILITIES.contains(&name.as_str())
            }
            _ => false,
        }
    }
}

struct Lowering<'a> {
    /// The function's name in the IR.
    name: String,
    /// The function's key in the typed tree.
    base: String,
    /// What its type parameters stand for.
    subst: generics::Subst,
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
    /// The closures' functions lifted out of this one.
    lifted: Vec<Function>,
    /// What a generator has yielded so far.
    yielded: Option<LocalId>,
}

impl<'a> Lowering<'a> {
    fn new(
        instance: &generics::FunctionInstance,
        context: &'a Context<'a>,
        body: &'a Body,
        returns: Type,
    ) -> Self {
        Lowering {
            name: instance.name.clone(),
            base: instance.base.clone(),
            subst: instance.subst.clone(),
            context,
            body,
            returns,
            locals: Vec::new(),
            blocks: vec![Open::default()],
            current: BlockId(0),
            scopes: vec![HashMap::new()],
            loops: Vec::new(),
            leaving: Vec::new(),
            lifted: Vec::new(),
            yielded: None,
        }
    }

    /// A function, and those lifted out of it. A nested function's first parameters are what
    /// it captures, and it's given its own type if it names itself.
    fn function(
        instance: &generics::FunctionInstance,
        def: &FnDef,
        context: &'a Context<'a>,
        (captures, itself): (&[(String, Type)], Option<&Type>),
    ) -> Lowered<Vec<Function>> {
        let key = instance.name.as_str();
        let raw = context.signatures.get(&instance.base).ok_or("it has no signature")?;
        let signature = context.instantiate(raw, &instance.subst);
        let body = context.signatures.types().body(&instance.base).ok_or("it has no types")?;
        if signature.returns != Type::Unit && !context.holds(&signature.returns) {
            return Err(format!("it gives back {}", signature.returns));
        }
        let mut lowering = Lowering::new(instance, context, body, signature.returns.clone());
        for (name, ty) in captures {
            lowering.declare(name, ty.clone());
        }
        for param in &signature.params {
            if !context.holds(&param.ty) {
                return Err(format!("`{}` is {}", param.name, param.ty));
            }
            lowering.declare(&param.name, param.ty.clone());
        }
        if let Some(ty) = itself {
            lowering.bind_itself(key, &def.name, ty, captures.len(), def.span);
        }
        if signature.generator {
            let items = lowering.generator_items()?;
            lowering.yielded = Some(items);
        }
        let plain = lowering.returns == Type::Unit || signature.generator;
        lowering.stmts(&def.body, if plain { Tail::Drop } else { Tail::Return })?;
        lowering.end_function(def.span)?;
        let mut_params = signature
            .params
            .iter()
            .enumerate()
            .filter(|(_, param)| param.is_mut)
            .map(|(at, _)| at + captures.len())
            .collect();
        Ok(lowering.finished(captures.len() + signature.params.len(), mut_params))
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
        let ty = self.type_at(expr.span).ok_or("an expression has no type")?;
        self.held(&ty).ok_or_else(|| format!("it uses a value of type {ty}"))
    }

    /// A type as the IR holds it, when it does: a function as a value is a closure.
    fn held(&self, ty: &Type) -> Option<Type> {
        match ty {
            Type::Function(key) => self.context.arrow_of(key),
            _ => self.context.holds(ty).then(|| ty.clone()),
        }
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
                let depth = target.depth;
                let target = if matches!(stmt.kind, StmtKind::Break) { target.exit } else { target.next };
                self.undo_since(depth, stmt.span)?;
                self.finish(Terminator::Jump(target));
            }
            StmtKind::Pass => {}
            StmtKind::For { names, is_mut, iter, body } => {
                self.for_stmt((names, *is_mut), iter, body, stmt.span)?;
            }
            StmtKind::Fail(error) => self.fail(error, stmt.span)?,
            StmtKind::Yield(value) => self.yield_value(value, stmt.span)?,
            StmtKind::Assert(cond) => self.assert(cond, stmt.span)?,
            StmtKind::With { value, name, is_mut: _, body } => {
                self.with_block(value, name.as_deref(), body, tail, stmt.span)?;
            }
            StmtKind::Def(def) => self.nested(def, stmt.span)?,
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
        let ty = self.concrete(ty);
        let held = self.held(&ty).ok_or_else(|| format!("`{name}` is {ty}"))?;
        Ok(self.declare(name, held))
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
        self.loops.push(Loop { next: head, exit, depth: self.leaving.len() });
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
            && self.constructs(callee, expr.span).is_none()
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
            ExprKind::Attr { object, name, optional: true } => self.optional_field(expr, object, name),
            ExprKind::Match { subject, arms } => {
                let dest = self.temporary(self.ty(expr)?);
                self.match_expr(subject, arms, Tail::Into(dest))?;
                Ok(Operand::Local(dest))
            }
            ExprKind::Call { callee, args } => {
                if let Some(type_name) = self.constructs(callee, span) {
                    return self.make(&type_name, args, span);
                }
                if matches!(&callee.kind, ExprKind::Name(name) if name == "replace" && self.lookup(name).is_none())
                    && !self.context.functions.contains("replace")
                {
                    return self.replace(args, span);
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
            ExprKind::Lambda { params, body } => self.arrow(expr, params, body, false),
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
