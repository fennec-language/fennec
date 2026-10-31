//! Setting up the module compiled code goes into: the JIT itself, the runtime helpers and
//! every function's declaration, each function's machine signature, and the entry point
//! that calls `main`.

use std::collections::HashSet;

use cranelift_codegen::ir::{AbiParam, InstBuilder, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};
use fennec_syntax::{Diagnostic, Span};

use super::{Compiled, Failure, Shared, inline, internal};
use crate::helpers::{self, Helper};
use crate::types::{Function, Program, Ty};

/// The machine type a value of `ty` is passed as, if it's passed at all.
pub(super) fn abi(ty: Ty) -> Option<types::Type> {
    match ty {
        Ty::Int | Ty::Obj | Ty::Struct(_) | Ty::Boxed(_) | Ty::Str | Ty::Dict(..) | Ty::List(_) => {
            Some(types::I64)
        }
        Ty::Float => Some(types::F64),
        Ty::Bool => Some(types::I8),
        Ty::Unit | Ty::Cap(_) | Ty::Known(_) => None,
    }
}

pub(super) fn new_module(span: Span) -> Result<JITModule, Diagnostic> {
    let mut flags = settings::builder();
    flags.set("opt_level", "speed").map_err(|err| internal(err, span))?;
    // The stack limit lives in a register of its own for the whole run, so checking it
    // before a call is one comparison. The register is one compiled Rust code preserves.
    flags.set("enable_pinned_reg", "true").map_err(|err| internal(err, span))?;
    // Results past a target's return registers go through memory: x86-64 has only two.
    flags.set("enable_multi_ret_implicit_sret", "true").map_err(|err| internal(err, span))?;
    // Cranelift's own checks of the code the JIT gives it find bugs in the JIT, not in the
    // program, so only test builds pay for them.
    let verify = if cfg!(debug_assertions) { "true" } else { "false" };
    flags.set("enable_verifier", verify).map_err(|err| internal(err, span))?;
    let isa = cranelift_native::builder()
        .map_err(|err| internal(err, span))?
        .finish(settings::Flags::new(flags))
        .map_err(|err| internal(err, span))?;
    let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    for helper in helpers::ALL {
        builder.symbol(helper.shape().symbol, helper.address());
    }
    // The failure flag is the run's thread's own, and the program runs on this thread.
    let failed = std::ptr::with_exposed_provenance::<u8>(fennec_interp::embed::failed_flag());
    builder.symbol(super::symbols::FAILED, failed);
    Ok(JITModule::new(builder))
}

/// The entry point: calls `main`, and fails the way the interpreter does if `main` gives
/// back a failure.
pub(super) fn define_entry(
    module: &mut dyn Module,
    ctx: &mut cranelift_codegen::Context,
    builder_ctx: &mut FunctionBuilderContext,
    shared: &Shared,
    (name, (main, ret)): (&str, (FuncId, Ty)),
    span: Span,
) -> Result<FuncId, Diagnostic> {
    let sig = module.make_signature();
    let id = module.declare_function(name, Linkage::Local, &sig).map_err(|err| internal(err, span))?;
    ctx.func.signature = sig;
    let mut b = FunctionBuilder::new(&mut ctx.func, builder_ctx);
    let block = b.create_block();
    b.switch_to_block(block);
    b.seal_block(block);
    let callee = module.declare_func_in_func(main, b.func);
    // The pinned register belongs to whoever called this; keep its value to put back.
    let saved = b.ins().get_pinned_reg(types::I64);
    let limit = b.ins().iconst(types::I64, shared.stack_limit);
    b.ins().set_pinned_reg(limit);
    let call = b.ins().call(callee, &[]);
    let result = b.inst_results(call).first().copied();
    if let (Ty::Obj, Some(result), Some(check)) = (ret, result, shared.helpers.get(&Helper::MainResult)) {
        let check = module.declare_func_in_func(*check, b.func);
        let start = b.ins().iconst(types::I64, i64::from(span.start));
        let end = b.ins().iconst(types::I64, i64::from(span.end));
        b.ins().call(check, &[result, start, end]);
    }
    b.ins().set_pinned_reg(saved);
    b.ins().return_(&[]);
    let config = module.target_config();
    b.finalize(config);
    module.define_function(id, ctx).map_err(|err| internal(err, span))?;
    module.clear_context(ctx);
    Ok(id)
}

/// Declares the runtime helpers and every function being compiled.
pub(super) fn declare_all(
    module: &mut dyn Module,
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
    for function in program.functions.iter().filter(|function| !skipped.contains(&function.key)) {
        let sig = signature(module, function);
        // Named by the function, not its place, so adding a function elsewhere leaves the
        // machine code of everything that calls this one the same.
        let name = format!("fennec_user_{}", super::symbols::digest(&function.key));
        let id = module.declare_function(&name, Linkage::Local, &sig).map_err(|err| internal(err, span))?;
        let params =
            function.params.iter().map(|param| (param.name.clone(), param.ty, param.is_mut)).collect();
        let takes_self = function.def.params.first().is_some_and(|param| param.is_self);
        let inline = inline::inline_body(function);
        if takes_self && let Some((owner, method)) = function.key.rsplit_once('.') {
            shared
                .methods
                .entry(method.to_owned())
                .or_default()
                .push((owner.to_owned(), function.key.clone()));
        }
        shared.compiled.insert(
            function.key.clone(),
            Compiled {
                id,
                params,
                ret: function.ret,
                takes_self,
                inline,
                specialized: super::specialize::specializable(function),
                prelude: function.prelude,
            },
        );
    }
    Ok(())
}

/// A compiled function's machine signature: for a function giving back a plain struct,
/// where to put it; each parameter's value (the address of a struct, and for a `mut`
/// object, also the caller's name for it); then the result and each `mut` number's final
/// value.
pub(super) fn signature(module: &dyn Module, function: &Function<'_>) -> cranelift_codegen::ir::Signature {
    let mut sig = module.make_signature();
    if matches!(function.ret, Ty::Struct(_)) {
        sig.params.push(AbiParam::new(types::I64));
    }
    for param in &function.params {
        sig.params.extend(abi(param.ty).map(AbiParam::new));
        if param.is_mut && param.ty == Ty::Obj {
            sig.params.push(AbiParam::new(types::I64));
        }
    }
    if !matches!(function.ret, Ty::Struct(_)) {
        sig.returns.extend(abi(function.ret).map(AbiParam::new));
    }
    for param in &function.params {
        if param.is_mut && returns_changes(param.ty) {
            sig.returns.extend(abi(param.ty).map(AbiParam::new));
        }
    }
    // Last, whether it failed: a register the caller tests, cheaper than loading the flag.
    sig.returns.push(AbiParam::new(types::I8));
    sig
}

/// Whether a `mut` parameter of this kind gives its final value back as a result: numbers
/// do; objects and plain structs are changed where the caller holds them.
pub(super) fn returns_changes(ty: Ty) -> bool {
    !matches!(ty, Ty::Obj | Ty::Struct(_))
}

/// Tries compiling until it works, each time leaving out the function that failed last,
/// which then runs in the interpreter. Gives back the result, and the functions left out
/// with why each was, by name.
pub(super) fn retrying<T>(
    mut attempt: impl FnMut(&HashSet<String>) -> Result<T, Failure>,
) -> Result<(T, Vec<Failure>), Diagnostic> {
    let mut skipped = HashSet::new();
    let mut reasons = Vec::new();
    loop {
        match attempt(&skipped) {
            Ok(done) => {
                reasons.sort_by(|(a, _): &Failure, (b, _): &Failure| a.cmp(b));
                return Ok((done, reasons));
            }
            Err((name, diagnostic)) => {
                if name == "main" || !skipped.insert(name.clone()) {
                    return Err(diagnostic);
                }
                reasons.push((name, diagnostic));
            }
        }
    }
}
