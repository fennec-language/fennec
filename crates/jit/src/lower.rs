//! Lowers Fennec functions to Cranelift IR and compiles them to machine code.
//!
//! Ints, floats and bools are machine values in Cranelift variables. Every other value is a
//! handle to a runtime temporary, and names holding one live in the runtime's frame, the
//! same frame the interpreter would use. A function that touches such values starts a
//! frame when it's called and drops its temporaries when it returns and after each
//! statement. Functions that don't, like most numeric code, compile to plain machine code.
//!
//! Statements are in `stmt`, expressions in `expr`, calls and printing in `call`, `match`
//! and errors in `flow`, and handing work to the interpreter in `fallback`.

mod call;
mod expr;
mod fallback;
mod flow;
mod objects;
mod stmt;

use std::collections::{HashMap, HashSet};

use cranelift_codegen::ir::{AbiParam, Block, InstBuilder, MemFlagsData, Value, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};
use fennec_interp::embed::{self, Tables};
use fennec_syntax::{Diagnostic, Span};

use crate::helpers::{self, Helper};
use crate::runtime::Entry;
use crate::types::{Function, Program, Ty, unsupported};

/// The message of the internal diagnostic that asks for a function to be lowered again,
/// this time with a frame.
const NEEDS_FRAME: &str = "\0needs a frame";

/// A compiled function's id and how to call it.
struct Compiled {
    id: FuncId,
    params: Vec<(String, Ty, bool)>,
    ret: Ty,
    /// Whether it's a method called on a value, rather than a function or named constructor.
    takes_self: bool,
}

/// How a program compiled.
#[derive(Debug, Default)]
pub(crate) struct Compilation {
    /// The functions and methods that couldn't be compiled, which run in the interpreter.
    pub(crate) interpreted: Vec<String>,
    /// How many of the program's compiled functions needed a frame in the runtime.
    pub(crate) with_frames: usize,
    /// How many operations the program's functions hand to the interpreter one at a time.
    pub(crate) handed_over: usize,
}

/// What every function being compiled shares.
struct Shared {
    compiled: HashMap<String, Compiled>,
    helpers: HashMap<Helper, FuncId>,
    tables: Tables,
    names: HashMap<String, i64>,
    strings: HashMap<String, i64>,
    with_frames: usize,
    handed_over: usize,
}

fn internal(err: impl std::fmt::Debug, span: Span) -> Diagnostic {
    Diagnostic::new(format!("the JIT hit an internal error: {err:?}"), span)
        .with_help("this is a bug in Fennec; run without `--jit` and please report it")
}

/// Compiles every function the JIT can. A function it can't compile runs in the
/// interpreter instead, so this retries without each one that fails until all compile.
pub(crate) fn compile(program: &Program<'_>) -> Result<(Entry, Tables, Compilation), Diagnostic> {
    let mut skipped = HashSet::new();
    loop {
        match attempt(program, &skipped) {
            Ok((entry, tables, (with_frames, handed_over))) => {
                let mut interpreted: Vec<String> = skipped.into_iter().collect();
                interpreted.sort();
                return Ok((entry, tables, Compilation { interpreted, with_frames, handed_over }));
            }
            Err((name, diagnostic)) => {
                if name == "main" || !skipped.insert(name) {
                    return Err(diagnostic);
                }
            }
        }
    }
}

fn new_module(span: Span) -> Result<JITModule, Diagnostic> {
    let mut flags = settings::builder();
    flags.set("opt_level", "speed").map_err(|err| internal(err, span))?;
    let isa = cranelift_native::builder()
        .map_err(|err| internal(err, span))?
        .finish(settings::Flags::new(flags))
        .map_err(|err| internal(err, span))?;
    let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    for helper in helpers::ALL {
        builder.symbol(helper.shape().symbol, helper.address());
    }
    Ok(JITModule::new(builder))
}

type Attempt = Result<(Entry, Tables, (usize, usize)), (String, Diagnostic)>;

/// One try at compiling the program, skipping some functions. On failure, the name of the
/// function that couldn't be compiled.
fn attempt(program: &Program<'_>, skipped: &HashSet<String>) -> Attempt {
    let span = program.main_span;
    let fatal = |diagnostic| ("main".to_owned(), diagnostic);
    let mut module = new_module(span).map_err(fatal)?;
    let mut shared = Shared {
        compiled: HashMap::new(),
        helpers: HashMap::new(),
        tables: Tables::default(),
        names: HashMap::new(),
        strings: HashMap::new(),
        with_frames: 0,
        handed_over: 0,
    };
    declare_all(&mut module, &mut shared, program, skipped).map_err(fatal)?;
    let mut ctx = module.make_context();
    let mut builder_ctx = FunctionBuilderContext::new();
    for function in program.functions.iter().filter(|function| !skipped.contains(&function.key)) {
        let failed = |diagnostic| (function.key.clone(), diagnostic);
        let id = shared.compiled.get(&function.key).map(|compiled| compiled.id);
        let id = id.ok_or_else(|| failed(internal("undeclared function", function.def.span)))?;
        let mut cx =
            Cx { module: &mut module, ctx: &mut ctx, builder_ctx: &mut builder_ctx, shared: &mut shared };
        lower_function(&mut cx, function).map_err(failed)?;
        module.define_function(id, &mut ctx).map_err(|err| failed(internal(err, function.def.span)))?;
        module.clear_context(&mut ctx);
    }
    let main = shared.compiled.get("main").ok_or_else(|| fatal(unsupported("this `main`", span)))?;
    if main.params.iter().any(|(_, ty, _)| !matches!(ty, Ty::Stdout | Ty::Fs)) {
        return Err(fatal(unsupported("a `main` that takes anything but `stdout`", span)));
    }
    let (main_id, main_ret) = (main.id, main.ret);
    let entry = define_entry(&mut module, &mut ctx, &mut builder_ctx, &shared, (main_id, main_ret), span)
        .map_err(fatal)?;
    module.finalize_definitions().map_err(|err| fatal(internal(err, span)))?;
    let code = module.get_finalized_function(entry);
    Ok((Entry { code, _module: module }, shared.tables, (shared.with_frames, shared.handed_over)))
}

/// The entry point: calls `main`, and fails the way the interpreter does if `main` gives
/// back a failure.
fn define_entry(
    module: &mut JITModule,
    ctx: &mut cranelift_codegen::Context,
    builder_ctx: &mut FunctionBuilderContext,
    shared: &Shared,
    (main, ret): (FuncId, Ty),
    span: Span,
) -> Result<FuncId, Diagnostic> {
    let sig = module.make_signature();
    let id =
        module.declare_function("fennec_entry", Linkage::Local, &sig).map_err(|err| internal(err, span))?;
    ctx.func.signature = sig;
    let mut b = FunctionBuilder::new(&mut ctx.func, builder_ctx);
    let block = b.create_block();
    b.switch_to_block(block);
    b.seal_block(block);
    let callee = module.declare_func_in_func(main, b.func);
    let depth = b.ins().iconst(types::I64, 1);
    let call = b.ins().call(callee, &[depth]);
    let result = b.inst_results(call).first().copied();
    if let (Ty::Obj, Some(result), Some(check)) = (ret, result, shared.helpers.get(&Helper::MainResult)) {
        let check = module.declare_func_in_func(*check, b.func);
        let start = b.ins().iconst(types::I64, i64::from(span.start));
        let end = b.ins().iconst(types::I64, i64::from(span.end));
        b.ins().call(check, &[result, start, end]);
    }
    b.ins().return_(&[]);
    let config = module.target_config();
    b.finalize(config);
    module.define_function(id, ctx).map_err(|err| internal(err, span))?;
    module.clear_context(ctx);
    Ok(id)
}

/// Declares the runtime helpers and every function being compiled.
fn declare_all(
    module: &mut JITModule,
    shared: &mut Shared,
    program: &Program<'_>,
    skipped: &HashSet<String>,
) -> Result<(), Diagnostic> {
    let span = program.main_span;
    for helper in helpers::ALL {
        let shape = helper.shape();
        let mut sig = module.make_signature();
        sig.params.extend(shape.params.iter().copied().map(AbiParam::new));
        sig.returns.extend(shape.results.iter().copied().map(AbiParam::new));
        let id = module
            .declare_function(shape.symbol, Linkage::Import, &sig)
            .map_err(|err| internal(err, span))?;
        shared.helpers.insert(helper, id);
    }
    for (at, function) in
        program.functions.iter().enumerate().filter(|(_, function)| !skipped.contains(&function.key))
    {
        let sig = signature(module, function);
        let name = format!("fennec_user_{at}");
        let id = module.declare_function(&name, Linkage::Local, &sig).map_err(|err| internal(err, span))?;
        let params =
            function.params.iter().map(|param| (param.name.clone(), param.ty, param.is_mut)).collect();
        let takes_self = function.def.params.first().is_some_and(|param| param.is_self);
        shared.compiled.insert(function.key.clone(), Compiled { id, params, ret: function.ret, takes_self });
    }
    Ok(())
}

fn abi(ty: Ty) -> Option<types::Type> {
    match ty {
        Ty::Int | Ty::Obj => Some(types::I64),
        Ty::Float => Some(types::F64),
        Ty::Bool => Some(types::I8),
        Ty::Unit | Ty::Stdout | Ty::Fs => None,
    }
}

/// A compiled function's machine signature: how deeply calls are nested, each parameter's
/// value (and, for a `mut` object, the caller's name for it), then the result and each
/// `mut` number's final value. The depth travels in a register, so counting it is cheap.
fn signature(module: &JITModule, function: &Function<'_>) -> cranelift_codegen::ir::Signature {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    for param in &function.params {
        sig.params.extend(abi(param.ty).map(AbiParam::new));
        if param.is_mut && param.ty == Ty::Obj {
            sig.params.push(AbiParam::new(types::I64));
        }
    }
    sig.returns.extend(abi(function.ret).map(AbiParam::new));
    for param in &function.params {
        if param.is_mut && param.ty != Ty::Obj {
            sig.returns.extend(abi(param.ty).map(AbiParam::new));
        }
    }
    sig
}

struct Cx<'a> {
    module: &'a mut JITModule,
    ctx: &'a mut cranelift_codegen::Context,
    builder_ctx: &'a mut FunctionBuilderContext,
    shared: &'a mut Shared,
}

/// Lowers a function, first without a frame, then with one if it turns out to need it.
fn lower_function(cx: &mut Cx<'_>, function: &Function<'_>) -> Result<(), Diagnostic> {
    let names = cx.shared.names.clone();
    let strings = cx.shared.strings.clone();
    let tables = &cx.shared.tables;
    let lengths = [
        tables.names.len(),
        tables.strings.len(),
        tables.exprs.len(),
        tables.stmts.len(),
        tables.patterns.len(),
    ];
    match lower_with(cx, function, false) {
        Err(diagnostic) if diagnostic.message == NEEDS_FRAME => {
            cx.ctx.clear();
            *cx.builder_ctx = FunctionBuilderContext::new();
            cx.ctx.func.signature = signature(cx.module, function);
            cx.shared.names = names;
            cx.shared.strings = strings;
            let [names, strings, exprs, stmts, patterns] = lengths;
            let tables = &mut cx.shared.tables;
            tables.names.truncate(names);
            tables.strings.truncate(strings);
            tables.exprs.truncate(exprs);
            tables.stmts.truncate(stmts);
            tables.patterns.truncate(patterns);
            if !function.prelude {
                cx.shared.with_frames += 1;
            }
            lower_with(cx, function, true)
        }
        other => other,
    }?;
    if !function.prelude {
        let tables = &cx.shared.tables;
        cx.shared.handed_over += tables.exprs.len() + tables.stmts.len() - lengths[2] - lengths[3];
    }
    Ok(())
}

fn lower_with(cx: &mut Cx<'_>, function: &Function<'_>, frame: bool) -> Result<(), Diagnostic> {
    cx.ctx.func.signature = signature(cx.module, function);
    let pointer = cx.module.target_config().pointer_type();
    let mut b = FunctionBuilder::new(&mut cx.ctx.func, cx.builder_ctx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);
    let params = b.block_params(entry).to_vec();
    let exit = b.create_block();
    let returns: Vec<_> = b.func.signature.returns.iter().map(|param| param.value_type).collect();
    if let Some(ret) = abi(function.ret).and(returns.first().copied()) {
        b.append_block_param(exit, ret);
    }
    let bail = b.create_block();
    b.set_cold_block(bail);
    let mut lowerer = Lowerer {
        b,
        module: &mut *cx.module,
        shared: &mut *cx.shared,
        function,
        scopes: vec![Scope::default()],
        loops: Vec::new(),
        frame,
        bases: Vec::new(),
        dirty: false,
        exit,
        bail,
        entry,
        pointer,
        mut_names: Vec::new(),
        depth: None,
        pending: Vec::new(),
    };
    lowerer.prologue(&params)?;
    let (tail, tail_ty) = lowerer.tail(&function.def.body)?;
    if !lowerer.is_dead() {
        lowerer.ret(tail, tail_ty, function.def.span)?;
    }
    lowerer.finish()
}

#[derive(Clone, Copy)]
pub(crate) struct Local {
    var: Option<Variable>,
    ty: Ty,
    is_mut: bool,
}

#[derive(Default)]
struct Scope {
    locals: HashMap<String, Local>,
    /// Whether this block has started a scope in the runtime's frame, for names holding
    /// objects. Blocks that don't create such names never do.
    pushed: bool,
}

struct Loop {
    next: Block,
    done: Block,
    /// How many scopes were open outside the loop's body.
    depth: usize,
    /// For `for mut`, the item to put back whenever the loop's body is left.
    put_back: Option<objects::PutBack>,
}

pub(crate) struct Lowerer<'a, 'f> {
    b: FunctionBuilder<'f>,
    module: &'a mut JITModule,
    shared: &'a mut Shared,
    function: &'a Function<'a>,
    scopes: Vec<Scope>,
    loops: Vec<Loop>,
    /// Whether this function has a runtime frame and temporaries.
    frame: bool,
    /// Where each statement drops temporaries back to: the function's mark, then one more
    /// for each `match` arm being lowered inside an expression.
    bases: Vec<Value>,
    /// Whether temporaries were made since the last drop.
    dirty: bool,
    /// Every return jumps here, to drop the frame and return.
    exit: Block,
    /// Where a failure leaves the function from.
    bail: Block,
    entry: Block,
    pointer: types::Type,
    /// For each `mut` object parameter: its name, and the variable holding the caller's
    /// name for it.
    mut_names: Vec<(String, Variable)>,
    /// How deeply this call is nested: 1 for `main`.
    depth: Option<Value>,
    /// The blocks for "the receiver is some other type", while calls on each compiled
    /// type are being lowered.
    pending: Vec<Block>,
}

impl Lowerer<'_, '_> {
    /// Counts the call, starts the frame and binds the parameters.
    fn prologue(&mut self, params: &[Value]) -> Result<(), Diagnostic> {
        let span = self.function.def.span;
        let (depth, params) = params.split_first().ok_or_else(|| internal("missing depth", span))?;
        self.depth = Some(*depth);
        if self.frame {
            let base = self.help(Helper::Mark, &[])?;
            self.bases.push(base);
            self.call_helper(Helper::Enter, &[])?;
        }
        let mut values = params.iter().copied();
        let function = self.function;
        for param in &function.params {
            let value = if abi(param.ty).is_some() { values.next() } else { None };
            if param.ty == Ty::Obj {
                let value = value.ok_or_else(|| internal("missing parameter", span))?;
                self.declare_object(&param.name, value, param.is_mut)?;
                if param.is_mut {
                    let caller = values.next().ok_or_else(|| internal("missing parameter", span))?;
                    let var = self.b.declare_var(types::I64);
                    self.b.def_var(var, caller);
                    self.mut_names.push((param.name.clone(), var));
                }
            } else {
                self.bind_native(&param.name, param.ty, value, param.is_mut);
            }
        }
        self.dirty = false;
        Ok(())
    }

    /// Fills the exit and bail blocks and finishes the function.
    fn finish(mut self) -> Result<(), Diagnostic> {
        let function = self.function;
        self.b.switch_to_block(self.exit);
        let mut results = self.b.block_params(self.exit).to_vec();
        if self.frame {
            for (name, var) in self.mut_names.clone() {
                let caller = self.b.use_var(var);
                let param = self.name_id(&name);
                self.call_helper(Helper::PutBack, &[caller, param])?;
            }
            let base = self.base()?;
            if function.ret == Ty::Obj {
                if let Some(first) = results.first_mut() {
                    *first = self.help(Helper::Keep, &[base, *first])?;
                }
            } else {
                self.call_helper(Helper::Truncate, &[base])?;
            }
            self.call_helper(Helper::Leave, &[])?;
        }
        for param in &function.params {
            if param.is_mut && param.ty != Ty::Obj {
                let local = self.local_param(&param.name)?;
                results.extend(local.var.map(|var| self.b.use_var(var)));
            }
        }
        self.b.ins().return_(&results);
        self.b.switch_to_block(self.bail);
        let returns: Vec<_> = self.b.func.signature.returns.iter().map(|param| param.value_type).collect();
        let zeros: Vec<Value> = returns.into_iter().map(|ty| self.zero_of(ty)).collect();
        self.b.ins().return_(&zeros);
        self.b.seal_all_blocks();
        let config = self.module.target_config();
        self.b.finalize(config);
        Ok(())
    }

    /// A parameter, which lives in the function's outermost scope.
    fn local_param(&self, name: &str) -> Result<Local, Diagnostic> {
        let scope = self.scopes.first().and_then(|scope| scope.locals.get(name)).copied();
        scope.ok_or_else(|| internal("lost parameter", self.function.def.span))
    }

    fn zero_of(&mut self, ty: types::Type) -> Value {
        if ty == types::F64 { self.b.ins().f64const(0.0) } else { self.b.ins().iconst(ty, 0) }
    }

    fn base(&self) -> Result<Value, Diagnostic> {
        self.bases.first().copied().ok_or_else(|| internal("no frame", self.function.def.span))
    }

    /// Whether code here can't run. Cranelift calls a sealed block with no predecessors
    /// unreachable, which the entry and bail blocks are not.
    fn is_dead(&self) -> bool {
        self.b
            .current_block()
            .is_none_or(|block| self.b.is_unreachable() && block != self.bail && block != self.entry)
    }

    /// Starts a fresh block after a jump or return, for any code that follows it.
    fn after_terminator(&mut self) {
        let dead = self.b.create_block();
        self.b.switch_to_block(dead);
        // Nothing jumps here, so sealing it now marks it unreachable.
        self.b.seal_block(dead);
    }

    /// Calls a runtime helper, checking the failure flag after it if it can fail. Asks for a
    /// frame if the function doesn't have one and the helper needs it.
    fn call_helper(&mut self, helper: Helper, args: &[Value]) -> Result<Option<Value>, Diagnostic> {
        if helper.needs_frame() && !self.frame {
            return Err(Diagnostic::new(NEEDS_FRAME, self.function.def.span));
        }
        let id = self.shared.helpers.get(&helper).copied();
        let id = id.ok_or_else(|| internal("undeclared helper", self.function.def.span))?;
        let callee = self.module.declare_func_in_func(id, self.b.func);
        let call = self.b.ins().call(callee, args);
        let result = self.b.inst_results(call).first().copied();
        if helper.can_fail() {
            self.check_failed();
        }
        if helper.shape().results == [types::I64] {
            self.dirty = true;
        }
        Ok(result)
    }

    /// Calls a helper that returns a value.
    fn help(&mut self, helper: Helper, args: &[Value]) -> Result<Value, Diagnostic> {
        let result = self.call_helper(helper, args)?;
        result.ok_or_else(|| internal("helper gave nothing back", self.function.def.span))
    }

    fn int(&mut self, n: i64) -> Value {
        self.b.ins().iconst(types::I64, n)
    }

    fn span_args(&mut self, span: Span) -> [Value; 2] {
        [self.int(i64::from(span.start)), self.int(i64::from(span.end))]
    }

    fn name_id(&mut self, name: &str) -> Value {
        let id = intern(&mut self.shared.names, &mut self.shared.tables.names, name);
        self.int(id)
    }

    fn string_id(&mut self, text: &str) -> Value {
        let id = intern(&mut self.shared.strings, &mut self.shared.tables.strings, text);
        self.int(id)
    }

    fn local(&self, name: &str) -> Option<Local> {
        self.scopes.iter().rev().find_map(|scope| scope.locals.get(name)).copied()
    }

    fn bind_native(&mut self, name: &str, ty: Ty, value: Option<Value>, is_mut: bool) {
        let var = abi(ty).map(|abi_ty| {
            let var = self.b.declare_var(abi_ty);
            if let Some(value) = value {
                self.b.def_var(var, value);
            }
            var
        });
        if let Some(scope) = self.scopes.last_mut() {
            scope.locals.insert(name.to_owned(), Local { var, ty, is_mut });
        }
    }

    /// Creates a name holding an object, in the runtime's frame.
    fn declare_object(&mut self, name: &str, handle: Value, is_mut: bool) -> Result<(), Diagnostic> {
        self.ensure_scope()?;
        let id = self.name_id(name);
        let is_mut_flag = self.b.ins().iconst(types::I8, i64::from(is_mut));
        self.call_helper(Helper::Declare, &[id, handle, is_mut_flag])?;
        if let Some(scope) = self.scopes.last_mut() {
            scope.locals.insert(name.to_owned(), Local { var: None, ty: Ty::Obj, is_mut });
        }
        Ok(())
    }

    /// Starts this block's scope in the runtime's frame, if it hasn't yet.
    fn ensure_scope(&mut self) -> Result<(), Diagnostic> {
        let nested = self.scopes.len() > 1;
        if nested && self.scopes.last().is_some_and(|scope| !scope.pushed) {
            self.call_helper(Helper::PushScope, &[])?;
            if let Some(scope) = self.scopes.last_mut() {
                scope.pushed = true;
            }
        }
        Ok(())
    }

    fn open_scope(&mut self) {
        self.scopes.push(Scope::default());
    }

    /// Ends a block's scope, and its scope in the runtime's frame if it started one.
    fn close_scope(&mut self) -> Result<(), Diagnostic> {
        let scope = self.scopes.pop();
        if scope.is_some_and(|scope| scope.pushed) && !self.is_dead() {
            self.call_helper(Helper::PopScope, &[])?;
        }
        Ok(())
    }

    /// Ends the runtime scopes of every block deeper than `depth`, before jumping out.
    fn unwind_scopes(&mut self, depth: usize) -> Result<(), Diagnostic> {
        let pushed = self.scopes.iter().skip(depth).filter(|scope| scope.pushed).count();
        for _ in 0..pushed {
            self.call_helper(Helper::PopScope, &[])?;
        }
        Ok(())
    }

    /// Drops the temporaries made by the statement just lowered.
    fn drop_temps(&mut self) -> Result<(), Diagnostic> {
        if self.dirty && self.frame && !self.is_dead() {
            let base =
                self.bases.last().copied().ok_or_else(|| internal("no mark", self.function.def.span))?;
            self.call_helper(Helper::Truncate, &[base])?;
        }
        self.dirty = false;
        Ok(())
    }

    /// Leaves the function from here with a value.
    fn ret(&mut self, value: Option<Value>, ty: Ty, span: Span) -> Result<(), Diagnostic> {
        self.put_back_all()?;
        let function = self.function;
        let value = if function.fallible {
            let handle = self.handle_of(value, ty, span)?;
            Some(self.help(Helper::Ok, &[handle])?)
        } else {
            match function.ret {
                Ty::Unit | Ty::Stdout | Ty::Fs => None,
                ret => Some(self.convert(value, ty, ret, span)?),
            }
        };
        self.jump_exit(value);
        Ok(())
    }

    /// Leaves the function with a value that's already what it returns, like a failure
    /// `?` passes up.
    fn pass_up(&mut self, handle: Value) -> Result<(), Diagnostic> {
        self.put_back_all()?;
        let value = abi(self.function.ret).map(|_| handle);
        self.jump_exit(value);
        Ok(())
    }

    fn jump_exit(&mut self, value: Option<Value>) {
        let args: Vec<_> = value.into_iter().map(Into::into).collect();
        self.b.ins().jump(self.exit, &args);
        self.after_terminator();
    }

    /// Calls the runtime's failure with one of the JIT's own codes when `failed` is set,
    /// then leaves the function.
    fn fail_if(&mut self, failed: Value, code: i64, span: Span) {
        let fail = self.b.create_block();
        self.b.set_cold_block(fail);
        let ok = self.b.create_block();
        self.b.ins().brif(failed, fail, &[], ok, &[]);
        self.b.switch_to_block(fail);
        self.b.seal_block(fail);
        let code = self.int(code);
        let [start, end] = self.span_args(span);
        if let Some(id) = self.shared.helpers.get(&Helper::Fail).copied() {
            let callee = self.module.declare_func_in_func(id, self.b.func);
            self.b.ins().call(callee, &[code, start, end]);
        }
        self.b.ins().jump(self.bail, &[]);
        self.b.switch_to_block(ok);
        self.b.seal_block(ok);
    }

    /// Leaves the function if the program has failed.
    fn check_failed(&mut self) {
        let address = i64::try_from(embed::failed_flag()).unwrap_or(0);
        let address = self.b.ins().iconst(self.pointer, address);
        let failed = self.b.ins().load(types::I8, MemFlagsData::trusted(), address, 0);
        let ok = self.b.create_block();
        self.b.ins().brif(failed, self.bail, &[], ok, &[]);
        self.b.switch_to_block(ok);
        self.b.seal_block(ok);
    }
}

fn intern(map: &mut HashMap<String, i64>, table: &mut Vec<String>, text: &str) -> i64 {
    if let Some(id) = map.get(text) {
        return *id;
    }
    let id = i64::try_from(table.len()).unwrap_or(0);
    table.push(text.to_owned());
    map.insert(text.to_owned(), id);
    id
}
