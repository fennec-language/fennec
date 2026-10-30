//! Lowers Fennec functions to Cranelift IR and compiles them to machine code. Statements are
//! here; expressions are in `expr`. Failures (overflow, division by zero) call into the
//! runtime and leave the function early; every call checks whether its callee failed.

mod expr;

use std::collections::HashMap;

use cranelift_codegen::ir::{AbiParam, Block, InstBuilder, MemFlagsData, Value, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};
use fennec_syntax::{BinOp, Diagnostic, Expr, ExprKind, Span, Stmt, StmtKind};

use crate::runtime::{self, Entry};
use crate::types::{Function, Program, Ty, unsupported};

/// The runtime functions compiled code calls.
pub(crate) struct Helpers {
    fail: FuncId,
    print_int: FuncId,
    print_float: FuncId,
    print_bool: FuncId,
    print_str: FuncId,
}

/// A compiled function's id and types.
#[derive(Clone)]
struct Compiled {
    id: FuncId,
    params: Vec<Ty>,
    ret: Ty,
}

fn internal(err: impl std::fmt::Debug, span: Span) -> Diagnostic {
    Diagnostic::new(format!("the JIT hit an internal error: {err:?}"), span)
        .with_help("this is a bug in Fennec; run without `--jit` and please report it")
}

pub(crate) fn compile(program: &Program<'_>) -> Result<Entry, Diagnostic> {
    let span = program.main_span;
    let mut flags = settings::builder();
    flags.set("opt_level", "speed").map_err(|err| internal(err, span))?;
    let isa = cranelift_native::builder()
        .map_err(|err| internal(err, span))?
        .finish(settings::Flags::new(flags))
        .map_err(|err| internal(err, span))?;
    let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    let helper_fns: [(&str, *const u8); 5] = [
        ("fennec_fail", runtime::fail as *const u8),
        ("fennec_print_int", runtime::print_int as *const u8),
        ("fennec_print_float", runtime::print_float as *const u8),
        ("fennec_print_bool", runtime::print_bool as *const u8),
        ("fennec_print_str", runtime::print_str as *const u8),
    ];
    for (name, address) in helper_fns {
        builder.symbol(name, address);
    }
    let mut module = JITModule::new(builder);
    let helpers = declare_helpers(&mut module, span)?;
    let mut compiled: HashMap<String, Compiled> = HashMap::new();
    for function in &program.functions {
        if let Some(ret) = function.ret {
            let id = declare(&mut module, function, ret)?;
            compiled.insert(function.def.name.clone(), Compiled { id, params: function.params.clone(), ret });
        }
    }
    let mut strings = Vec::new();
    let mut ctx = module.make_context();
    let mut builder_ctx = FunctionBuilderContext::new();
    for function in &program.functions {
        let env = Env { compiled: &compiled, helpers: &helpers, strings: &mut strings };
        let ret = lower_function(&mut module, &mut ctx, &mut builder_ctx, function, env)?;
        let id = match compiled.get(&function.def.name) {
            Some(existing) => existing.id,
            None => declare(&mut module, function, ret)?,
        };
        module.define_function(id, &mut ctx).map_err(|err| internal(err, function.def.span))?;
        module.clear_context(&mut ctx);
        compiled.insert(function.def.name.clone(), Compiled { id, params: function.params.clone(), ret });
    }
    module.finalize_definitions().map_err(|err| internal(err, span))?;
    let main =
        compiled.get("main").ok_or_else(|| Diagnostic::new("this program has no `main` function", span))?;
    if main.ret != Ty::Unit || !main.params.iter().all(|ty| *ty == Ty::Stdout) {
        return Err(unsupported("a `main` that takes anything but `stdout` or returns a value", span));
    }
    let code = module.get_finalized_function(main.id);
    Ok(Entry { code, strings, _module: module })
}

fn abi(ty: Ty) -> Option<types::Type> {
    match ty {
        Ty::Int => Some(types::I64),
        Ty::Float => Some(types::F64),
        Ty::Bool => Some(types::I8),
        Ty::Unit | Ty::Stdout => None,
    }
}

fn signature(module: &JITModule, params: &[Ty], ret: Ty) -> cranelift_codegen::ir::Signature {
    let mut sig = module.make_signature();
    sig.params.extend(params.iter().filter_map(|ty| abi(*ty)).map(AbiParam::new));
    sig.returns.extend(abi(ret).map(AbiParam::new));
    sig
}

fn declare(module: &mut JITModule, function: &Function<'_>, ret: Ty) -> Result<FuncId, Diagnostic> {
    let sig = signature(module, &function.params, ret);
    module
        .declare_function(&format!("fennec_user_{}", function.def.name), Linkage::Local, &sig)
        .map_err(|err| internal(err, function.def.span))
}

fn declare_helpers(module: &mut JITModule, span: Span) -> Result<Helpers, Diagnostic> {
    let mut helper = |name: &str, params: &[types::Type]| {
        let mut sig = module.make_signature();
        sig.params.extend(params.iter().copied().map(AbiParam::new));
        module.declare_function(name, Linkage::Import, &sig).map_err(|err| internal(err, span))
    };
    Ok(Helpers {
        fail: helper("fennec_fail", &[types::I64, types::I64, types::I64])?,
        print_int: helper("fennec_print_int", &[types::I64])?,
        print_float: helper("fennec_print_float", &[types::F64])?,
        print_bool: helper("fennec_print_bool", &[types::I8])?,
        print_str: helper("fennec_print_str", &[types::I64])?,
    })
}

/// What every function being compiled can see.
struct Env<'a> {
    compiled: &'a HashMap<String, Compiled>,
    helpers: &'a Helpers,
    /// String literals, by index, for the runtime to print.
    strings: &'a mut Vec<String>,
}

/// Compiles one function into `ctx`, returning its return type.
fn lower_function(
    module: &mut JITModule,
    ctx: &mut cranelift_codegen::Context,
    builder_ctx: &mut FunctionBuilderContext,
    function: &Function<'_>,
    env: Env<'_>,
) -> Result<Ty, Diagnostic> {
    let Env { compiled, helpers, strings } = env;
    ctx.func.signature = signature(module, &function.params, function.ret.unwrap_or(Ty::Unit));
    let mut b = FunctionBuilder::new(&mut ctx.func, builder_ctx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let bail = b.create_block();
    let mut lowerer = Lowerer {
        b,
        module,
        compiled,
        helpers,
        strings,
        scopes: vec![HashMap::new()],
        loops: Vec::new(),
        ret: function.ret,
        bail,
    };
    let mut values = lowerer.b.block_params(entry).to_vec().into_iter();
    for (param, ty) in function.def.params.iter().zip(&function.params) {
        let value = if abi(*ty).is_some() { values.next() } else { None };
        lowerer.bind(&param.name, *ty, value, false);
    }
    let (tail, tail_ty) = lowerer.tail(&function.def.body)?;
    if !lowerer.is_dead() {
        let ret = lowerer.returns(tail_ty, function.def.span)?;
        lowerer.emit_return(tail, ret);
    }
    Ok(lowerer.finish())
}

pub(crate) struct Local {
    var: Option<Variable>,
    ty: Ty,
    is_mut: bool,
}

pub(crate) struct Lowerer<'a, 'f> {
    b: FunctionBuilder<'f>,
    module: &'a mut JITModule,
    compiled: &'a HashMap<String, Compiled>,
    helpers: &'a Helpers,
    strings: &'a mut Vec<String>,
    scopes: Vec<HashMap<String, Local>>,
    /// (continue target, break target) for each enclosing loop.
    loops: Vec<(Block, Block)>,
    ret: Option<Ty>,
    /// Where a failure leaves the function from.
    bail: Block,
}

impl Lowerer<'_, '_> {
    fn finish(mut self) -> Ty {
        let ret = self.ret.unwrap_or(Ty::Unit);
        self.b.switch_to_block(self.bail);
        let zero = self.zero(ret);
        self.emit_return(zero, ret);
        self.b.seal_all_blocks();
        let config = self.module.target_config();
        self.b.finalize(config);
        ret
    }

    fn is_dead(&self) -> bool {
        self.b.current_block().is_none_or(|block| self.b.is_unreachable() && block != self.bail)
    }

    /// Starts a fresh block after a jump or return, for any code that follows it.
    fn after_terminator(&mut self) {
        let dead = self.b.create_block();
        self.b.switch_to_block(dead);
        // Nothing jumps here, so sealing it now marks it unreachable.
        self.b.seal_block(dead);
    }

    fn zero(&mut self, ty: Ty) -> Option<Value> {
        match ty {
            Ty::Int => Some(self.b.ins().iconst(types::I64, 0)),
            Ty::Float => Some(self.b.ins().f64const(0.0)),
            Ty::Bool => Some(self.b.ins().iconst(types::I8, 0)),
            Ty::Unit | Ty::Stdout => None,
        }
    }

    /// Records the function's return type the first time it returns, and checks later ones match.
    fn returns(&mut self, ty: Ty, span: Span) -> Result<Ty, Diagnostic> {
        match self.ret {
            None => {
                self.ret = Some(ty);
                self.b.func.signature.returns.extend(abi(ty).map(AbiParam::new));
                Ok(ty)
            }
            Some(ret) if ret == ty => Ok(ret),
            Some(ret) => Err(unsupported(
                &format!("a function that returns both {} and {}", ret.name(), ty.name()),
                span,
            )),
        }
    }

    fn emit_return(&mut self, value: Option<Value>, ty: Ty) {
        match (value, abi(ty)) {
            (Some(value), Some(_)) => self.b.ins().return_(&[value]),
            _ => self.b.ins().return_(&[]),
        };
    }

    fn bind(&mut self, name: &str, ty: Ty, value: Option<Value>, is_mut: bool) {
        let var = abi(ty).map(|abi_ty| {
            let var = self.b.declare_var(abi_ty);
            if let Some(value) = value {
                self.b.def_var(var, value);
            }
            var
        });
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_owned(), Local { var, ty, is_mut });
        }
    }

    fn local(&self, name: &str) -> Option<&Local> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }

    /// Lowers a block's statements, returning the value of its last expression, if any.
    fn tail(&mut self, stmts: &[Stmt]) -> Result<(Option<Value>, Ty), Diagnostic> {
        let Some((last, rest)) = stmts.split_last() else { return Ok((None, Ty::Unit)) };
        for stmt in rest {
            self.stmt(stmt)?;
        }
        match &last.kind {
            StmtKind::Expr(expr) => self.expr(expr),
            StmtKind::If { branches, otherwise: Some(otherwise) } => {
                self.tail_if(branches, otherwise, last.span)
            }
            _ => {
                self.stmt(last)?;
                Ok((None, Ty::Unit))
            }
        }
    }

    fn block_tail(&mut self, stmts: &[Stmt]) -> Result<(Option<Value>, Ty), Diagnostic> {
        self.scopes.push(HashMap::new());
        let result = self.tail(stmts);
        self.scopes.pop();
        result
    }

    /// An `if` whose branches all end in a value: the value of whichever branch runs.
    fn tail_if(
        &mut self,
        branches: &[(Expr, Vec<Stmt>)],
        otherwise: &[Stmt],
        span: Span,
    ) -> Result<(Option<Value>, Ty), Diagnostic> {
        let merge = self.b.create_block();
        let mut merged: Option<Ty> = None;
        for (cond, body) in branches {
            let test = self.condition(cond)?;
            let then = self.b.create_block();
            let next = self.b.create_block();
            self.b.ins().brif(test, then, &[], next, &[]);
            self.b.switch_to_block(then);
            self.merge_into(merge, &mut merged, body, span)?;
            self.b.switch_to_block(next);
        }
        self.merge_into(merge, &mut merged, otherwise, span)?;
        self.b.switch_to_block(merge);
        let ty = merged.unwrap_or(Ty::Unit);
        let value = self.b.block_params(merge).first().copied();
        Ok((value, ty))
    }

    fn merge_into(
        &mut self,
        merge: Block,
        merged: &mut Option<Ty>,
        body: &[Stmt],
        span: Span,
    ) -> Result<(), Diagnostic> {
        let (value, ty) = self.block_tail(body)?;
        if self.is_dead() {
            return Ok(());
        }
        match *merged {
            None => {
                *merged = Some(ty);
                if let Some(abi_ty) = abi(ty) {
                    self.b.append_block_param(merge, abi_ty);
                }
            }
            Some(existing) if existing != ty => {
                return Err(unsupported(
                    &format!("branches that give both {} and {}", existing.name(), ty.name()),
                    span,
                ));
            }
            Some(_) => {}
        }
        let args: Vec<_> = value.into_iter().map(Into::into).collect();
        self.b.ins().jump(merge, &args);
        Ok(())
    }

    fn stmt(&mut self, stmt: &Stmt) -> Result<(), Diagnostic> {
        match &stmt.kind {
            StmtKind::Expr(expr) => self.expr(expr).map(|_| ()),
            StmtKind::Assign { target, op, value, declare_mut } => {
                self.assign(target, *op, value, *declare_mut)
            }
            StmtKind::If { branches, otherwise } => self.if_stmt(branches, otherwise.as_deref()),
            StmtKind::While { cond, body } => self.while_loop(cond, body),
            StmtKind::For { names, is_mut: false, iter, body } if names.len() == 1 => {
                let name = names.first().map_or("", String::as_str);
                self.for_range(name, iter, body)
            }
            StmtKind::Return(value) => {
                let (value, ty) = match value {
                    Some(value) => self.expr(value)?,
                    None => (None, Ty::Unit),
                };
                let ret = self.returns(ty, stmt.span)?;
                self.emit_return(value, ret);
                self.after_terminator();
                Ok(())
            }
            StmtKind::Break | StmtKind::Continue => {
                let &(continue_to, break_to) = self
                    .loops
                    .last()
                    .ok_or_else(|| Diagnostic::new("`break` or `continue` outside a loop", stmt.span))?;
                let target = if matches!(stmt.kind, StmtKind::Break) { break_to } else { continue_to };
                self.b.ins().jump(target, &[]);
                self.after_terminator();
                Ok(())
            }
            StmtKind::Pass => Ok(()),
            _ => Err(unsupported("this kind of statement", stmt.span)),
        }
    }

    fn assign(
        &mut self,
        target: &Expr,
        op: Option<BinOp>,
        value: &Expr,
        declare_mut: bool,
    ) -> Result<(), Diagnostic> {
        let ExprKind::Name(name) = &target.kind else {
            return Err(unsupported("assigning to anything but a name", target.span));
        };
        let (new_value, new_ty) = self.scalar(value)?;
        let existing = self.local(name).map(|local| (local.var, local.ty, local.is_mut));
        match (existing, declare_mut, op) {
            (None, _, None) | (Some(_), true, None) => {
                self.bind(name, new_ty, Some(new_value), declare_mut);
                Ok(())
            }
            (Some((Some(var), ty, true)), false, op) if ty == new_ty || op.is_some() => {
                let result = match op {
                    Some(op) => {
                        let old = self.b.use_var(var);
                        let (result, result_ty) =
                            self.binary(op, (old, ty), (new_value, new_ty), target.span.to(value.span))?;
                        if result_ty != ty {
                            return Err(unsupported("changing a name's type", target.span));
                        }
                        result
                    }
                    None => new_value,
                };
                self.b.def_var(var, result);
                Ok(())
            }
            _ => Err(unsupported("this assignment", target.span)),
        }
    }

    fn if_stmt(
        &mut self,
        branches: &[(Expr, Vec<Stmt>)],
        otherwise: Option<&[Stmt]>,
    ) -> Result<(), Diagnostic> {
        let done = self.b.create_block();
        for (cond, body) in branches {
            let test = self.condition(cond)?;
            let then = self.b.create_block();
            let next = self.b.create_block();
            self.b.ins().brif(test, then, &[], next, &[]);
            self.b.switch_to_block(then);
            self.block_tail(body)?;
            if !self.is_dead() {
                self.b.ins().jump(done, &[]);
            }
            self.b.switch_to_block(next);
        }
        if let Some(body) = otherwise {
            self.block_tail(body)?;
        }
        if !self.is_dead() {
            self.b.ins().jump(done, &[]);
        }
        self.b.switch_to_block(done);
        Ok(())
    }

    fn while_loop(&mut self, cond: &Expr, body: &[Stmt]) -> Result<(), Diagnostic> {
        let header = self.b.create_block();
        let inside = self.b.create_block();
        let exit = self.b.create_block();
        self.b.ins().jump(header, &[]);
        self.b.switch_to_block(header);
        let test = self.condition(cond)?;
        self.b.ins().brif(test, inside, &[], exit, &[]);
        self.b.switch_to_block(inside);
        self.loops.push((header, exit));
        let lowered = self.block_tail(body);
        self.loops.pop();
        lowered?;
        if !self.is_dead() {
            self.b.ins().jump(header, &[]);
        }
        self.b.switch_to_block(exit);
        Ok(())
    }

    /// `for name in range(...)`, the only loop over a collection the JIT compiles so far.
    fn for_range(&mut self, name: &str, iter: &Expr, body: &[Stmt]) -> Result<(), Diagnostic> {
        let ExprKind::Call { callee, args } = &iter.kind else {
            return Err(unsupported("loops over anything but `range`", iter.span));
        };
        if !matches!(&callee.kind, ExprKind::Name(callee) if callee == "range")
            || args.iter().any(|arg| arg.name.is_some() || arg.is_mut)
        {
            return Err(unsupported("loops over anything but `range`", iter.span));
        }
        let mut bounds = Vec::new();
        for arg in args {
            match self.scalar(&arg.value)? {
                (value, Ty::Int) => bounds.push(value),
                _ => return Err(unsupported("`range` with anything but ints", arg.value.span)),
            }
        }
        let zero = self.b.ins().iconst(types::I64, 0);
        let one = self.b.ins().iconst(types::I64, 1);
        let (start, end, step) = match bounds.as_slice() {
            [end] => (zero, *end, one),
            [start, end] => (*start, *end, one),
            [start, end, step] => (*start, *end, *step),
            _ => return Err(unsupported("`range` with these arguments", iter.span)),
        };
        let zero_step = self.b.ins().icmp_imm_s(cranelift_codegen::ir::condcodes::IntCC::Equal, step, 0);
        self.fail_if(zero_step, runtime::ZERO_STEP, iter.span);
        let counter = self.b.declare_var(types::I64);
        self.b.def_var(counter, start);
        let header = self.b.create_block();
        let inside = self.b.create_block();
        let advance = self.b.create_block();
        let exit = self.b.create_block();
        self.b.ins().jump(header, &[]);
        self.b.switch_to_block(header);
        let at = self.b.use_var(counter);
        let test = self.range_test(at, end, step);
        self.b.ins().brif(test, inside, &[], exit, &[]);
        self.b.switch_to_block(inside);
        self.scopes.push(HashMap::new());
        self.bind(name, Ty::Int, Some(at), false);
        self.loops.push((advance, exit));
        let lowered = self.tail(body);
        self.loops.pop();
        self.scopes.pop();
        lowered?;
        if !self.is_dead() {
            self.b.ins().jump(advance, &[]);
        }
        self.b.switch_to_block(advance);
        let at = self.b.use_var(counter);
        let next = self.b.ins().iadd(at, step);
        self.b.def_var(counter, next);
        self.b.ins().jump(header, &[]);
        self.b.switch_to_block(exit);
        Ok(())
    }

    /// Whether a range is still going: `at < end` going up, `at > end` going down.
    fn range_test(&mut self, at: Value, end: Value, step: Value) -> Value {
        use cranelift_codegen::ir::condcodes::IntCC;
        let up = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, step, 0);
        let below = self.b.ins().icmp(IntCC::SignedLessThan, at, end);
        let above = self.b.ins().icmp(IntCC::SignedGreaterThan, at, end);
        self.b.ins().select(up, below, above)
    }

    /// Calls the runtime's `fail` and leaves the function if `failed` is true.
    pub(crate) fn fail_if(&mut self, failed: Value, code: i64, span: Span) {
        let failing = self.b.create_block();
        let fine = self.b.create_block();
        self.b.ins().brif(failed, failing, &[], fine, &[]);
        self.b.switch_to_block(failing);
        let fail = self.module.declare_func_in_func(self.helpers.fail, self.b.func);
        let args =
            [code, i64::from(span.start), i64::from(span.end)].map(|n| self.b.ins().iconst(types::I64, n));
        self.b.ins().call(fail, &args);
        self.b.ins().jump(self.bail, &[]);
        self.b.switch_to_block(fine);
    }

    /// After a call, leaves the function if the callee failed.
    pub(crate) fn check_failed(&mut self) {
        let pointer = self.module.target_config().pointer_type();
        let address = self.b.ins().iconst(pointer, i64::try_from(runtime::failed_flag()).unwrap_or(0));
        let failed = self.b.ins().load(types::I8, MemFlagsData::trusted(), address, 0);
        let fine = self.b.create_block();
        self.b.ins().brif(failed, self.bail, &[], fine, &[]);
        self.b.switch_to_block(fine);
    }
}
