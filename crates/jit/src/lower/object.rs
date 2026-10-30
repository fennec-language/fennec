//! Executables: the same functions as the JIT compiles, written to an object file instead,
//! with a C `main` that links against the runtime library.
//!
//! The object carries the program as bytes (`embed::codec`). Its `main` hands them to the
//! runtime word by word, then has the runtime run `fennec_entry` on a thread with a big
//! stack. `fennec_entry` asks the runtime for the addresses compiled code needs (the
//! failure flag, string literals) and writes them into a table every function reads,
//! then calls the program's `main`.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{AbiParam, InstBuilder, MemFlagsData, Value, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use fennec_interp::embed::codec::Image;
use fennec_syntax::{Diagnostic, Sources, Span};

use super::module::retrying;
use super::{Compilation, Shared, compile_functions, internal};
use crate::helpers::Helper;
use crate::types::Ty;

/// The program an executable carries, as written.
pub(crate) struct Written<'a> {
    pub(crate) module: &'a fennec_syntax::Module,
    pub(crate) sources: &'a Sources,
}

/// Compiles a program into an object file for an executable, which links against the
/// runtime library. Gives back its bytes, what its code refers to by number, and how it
/// compiled.
pub(crate) fn compile_object(
    program: &crate::types::Program<'_>,
    signatures: &fennec_check::Signatures,
    image: &Written<'_>,
) -> Result<(Vec<u8>, Compilation), Diagnostic> {
    let (bytes, skipped) = retrying(|skipped| {
        let span = program.main_span;
        let fatal = |diagnostic| ("main".to_owned(), diagnostic);
        let mut module = new_module(span).map_err(fatal)?;
        let mut shared = Shared::new(program, 0);
        shared.slots = Some(slots(&mut module, span).map_err(fatal)?);
        let main = compile_functions(&mut module, &mut shared, program, signatures, skipped)?;
        let counts = (shared.with_frames, shared.handed_over);
        let bytes = finish(module, &shared, main, image, span).map_err(fatal)?;
        Ok((bytes, counts))
    })?;
    let (bytes, (with_frames, handed_over)) = bytes;
    let (interpreted, reasons) = skipped.into_iter().unzip();
    Ok((bytes, Compilation { interpreted, reasons, with_frames, handed_over, notes: Vec::new(), cached: 0 }))
}

/// An executable's table of runtime addresses: the failure flag's, the ASCII table's, then
/// each string literal's, filled in when it starts.
pub(crate) struct Slots {
    pub(super) data: DataId,
    /// Each literal's text, by its place in the table after the first two.
    pub(super) literals: Vec<String>,
}

impl Slots {
    /// Where a literal's address is in the table, adding it the first time.
    pub(super) fn literal(&mut self, text: &str) -> usize {
        let at = self.literals.iter().position(|known| known == text).unwrap_or(self.literals.len());
        if at == self.literals.len() {
            self.literals.push(text.to_owned());
        }
        at + 2
    }
}

pub(super) fn new_module(span: Span) -> Result<ObjectModule, Diagnostic> {
    let mut flags = settings::builder();
    flags.set("opt_level", "speed").map_err(|err| internal(err, span))?;
    flags.set("enable_pinned_reg", "true").map_err(|err| internal(err, span))?;
    // Results past a target's return registers go through memory: x86-64 has only two.
    flags.set("enable_multi_ret_implicit_sret", "true").map_err(|err| internal(err, span))?;
    flags.set("is_pic", "true").map_err(|err| internal(err, span))?;
    let isa = cranelift_native::builder()
        .map_err(|err| internal(err, span))?
        .finish(settings::Flags::new(flags))
        .map_err(|err| internal(err, span))?;
    let builder = ObjectBuilder::new(isa, "fennec", cranelift_module::default_libcall_names())
        .map_err(|err| internal(err, span))?;
    Ok(ObjectModule::new(builder))
}

/// Declares the table of runtime addresses, filled in when the executable starts.
pub(super) fn slots(module: &mut ObjectModule, span: Span) -> Result<Slots, Diagnostic> {
    let data = module
        .declare_data("fennec_slots", Linkage::Local, true, false)
        .map_err(|err| internal(err, span))?;
    Ok(Slots { data, literals: Vec::new() })
}

/// Writes the entry point, `main`, the table and the program's bytes, and gives back the
/// object file.
pub(super) fn finish(
    mut module: ObjectModule,
    shared: &Shared,
    main: (FuncId, Ty),
    program: &Written<'_>,
    span: Span,
) -> Result<Vec<u8>, Diagnostic> {
    let slots = shared.slots.as_ref().ok_or_else(|| internal("no table of addresses", span))?;
    let count = slots.literals.len();
    let mut table = DataDescription::new();
    table.define_zeroinit(8 * (2 + count));
    table.set_align(8);
    module.define_data(slots.data, &table).map_err(|err| internal(err, span))?;
    let image = Image {
        module: program.module.clone(),
        sources: program.sources.clone(),
        tables: shared.tables.clone_for_image(),
        literals: slots.literals.clone(),
    }
    .to_bytes();
    let length = i64::try_from(image.len()).unwrap_or(0);
    let mut words = image;
    words.resize(words.len().div_ceil(8) * 8, 0);
    let image_data = module
        .declare_data("fennec_image", Linkage::Local, false, false)
        .map_err(|err| internal(err, span))?;
    let mut description = DataDescription::new();
    let word_count = i64::try_from(words.len() / 8).unwrap_or(0);
    description.define(words.into_boxed_slice());
    description.set_align(8);
    module.define_data(image_data, &description).map_err(|err| internal(err, span))?;
    let mut ctx = module.make_context();
    let mut builder_ctx = FunctionBuilderContext::new();
    let entry = define_entry(&mut module, (&mut ctx, &mut builder_ctx), shared, main, span)?;
    define_main(&mut module, (&mut ctx, &mut builder_ctx), entry, (image_data, word_count, length), span)?;
    let product = module.finish();
    product.emit().map_err(|err| internal(err, span))
}

/// Declares one of the runtime's functions for starting an executable.
fn import(
    module: &mut ObjectModule,
    name: &str,
    params: &[types::Type],
    results: &[types::Type],
    span: Span,
) -> Result<FuncId, Diagnostic> {
    let mut sig = module.make_signature();
    sig.params.extend(params.iter().copied().map(AbiParam::new));
    sig.returns.extend(results.iter().copied().map(AbiParam::new));
    module.declare_function(name, Linkage::Import, &sig).map_err(|err| internal(err, span))
}

type Contexts<'c> = (&'c mut cranelift_codegen::Context, &'c mut FunctionBuilderContext);

/// `fennec_entry(limit)`: fills the table of runtime addresses, then calls the program's
/// `main` with `limit` as the lowest the stack may grow to, and fails like the interpreter
/// if `main` gives back a failure.
fn define_entry(
    module: &mut ObjectModule,
    (ctx, builder_ctx): Contexts<'_>,
    shared: &Shared,
    (main, ret): (FuncId, Ty),
    span: Span,
) -> Result<FuncId, Diagnostic> {
    let slots = shared.slots.as_ref().ok_or_else(|| internal("no table of addresses", span))?;
    let flag = import(module, "fennec_aot_failed_flag", &[], &[types::I64], span)?;
    let ascii = import(module, "fennec_aot_ascii", &[], &[types::I64], span)?;
    let literal = import(module, "fennec_aot_literal", &[types::I64], &[types::I64], span)?;
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    let id =
        module.declare_function("fennec_entry", Linkage::Local, &sig).map_err(|err| internal(err, span))?;
    ctx.func.signature = sig;
    let mut b = FunctionBuilder::new(&mut ctx.func, builder_ctx);
    let block = b.create_block();
    b.append_block_params_for_function_params(block);
    b.switch_to_block(block);
    b.seal_block(block);
    let limit = b.block_params(block).first().copied().ok_or_else(|| internal("no stack limit", span))?;
    let table = module.declare_data_in_func(slots.data, b.func);
    let base = b.ins().symbol_value(types::I64, table);
    for (at, helper) in [(0, flag), (8, ascii)] {
        let callee = module.declare_func_in_func(helper, b.func);
        let call = b.ins().call(callee, &[]);
        let address = b.inst_results(call).first().copied().ok_or_else(|| internal("no address", span))?;
        b.ins().store(MemFlagsData::trusted(), address, base, at);
    }
    let count = i64::try_from(slots.literals.len()).unwrap_or(0);
    let literal = module.declare_func_in_func(literal, b.func);
    counted_loop(&mut b, count, |b, at| {
        let call = b.ins().call(literal, &[at]);
        if let Some(address) = b.inst_results(call).first().copied() {
            let offset = b.ins().imul_imm_s(at, 8);
            let place = b.ins().iadd(base, offset);
            b.ins().store(MemFlagsData::trusted(), address, place, 16);
        }
    });
    let saved = b.ins().get_pinned_reg(types::I64);
    b.ins().set_pinned_reg(limit);
    let callee = module.declare_func_in_func(main, b.func);
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
    b.finalize(module.target_config());
    module.define_function(id, ctx).map_err(|err| internal(err, span))?;
    module.clear_context(ctx);
    Ok(id)
}

/// `main(argc, argv)`: hands the program's bytes to the runtime, then has it run
/// `fennec_entry`, giving back the process's exit code.
fn define_main(
    module: &mut ObjectModule,
    (ctx, builder_ctx): Contexts<'_>,
    entry: FuncId,
    (image, words, length): (DataId, i64, i64),
    span: Span,
) -> Result<(), Diagnostic> {
    let pointer = module.target_config().pointer_type();
    let word = import(module, "fennec_aot_image", &[types::I64], &[], span)?;
    let run = import(module, "fennec_aot_run", &[pointer, types::I64], &[types::I32], span)?;
    let mut sig = module.make_signature();
    sig.params.extend([AbiParam::new(types::I32), AbiParam::new(pointer)]);
    sig.returns.push(AbiParam::new(types::I32));
    let id = module.declare_function("main", Linkage::Export, &sig).map_err(|err| internal(err, span))?;
    ctx.func.signature = sig;
    let mut b = FunctionBuilder::new(&mut ctx.func, builder_ctx);
    let block = b.create_block();
    b.append_block_params_for_function_params(block);
    b.switch_to_block(block);
    b.seal_block(block);
    let data = module.declare_data_in_func(image, b.func);
    let base = b.ins().symbol_value(pointer, data);
    let word = module.declare_func_in_func(word, b.func);
    counted_loop(&mut b, words, |b, at| {
        let offset = b.ins().imul_imm_s(at, 8);
        let place = b.ins().iadd(base, offset);
        let value = b.ins().load(types::I64, MemFlagsData::trusted(), place, 0);
        b.ins().call(word, &[value]);
    });
    let entry = module.declare_func_in_func(entry, b.func);
    let entry = b.ins().func_addr(pointer, entry);
    let length = b.ins().iconst(types::I64, length);
    let run = module.declare_func_in_func(run, b.func);
    let call = b.ins().call(run, &[entry, length]);
    let code: Vec<Value> = b.inst_results(call).to_vec();
    b.ins().return_(&code);
    b.finalize(module.target_config());
    module.define_function(id, ctx).map_err(|err| internal(err, span))?;
    module.clear_context(ctx);
    Ok(())
}

/// Runs `body` with each count from 0 up to `count`, as a loop.
fn counted_loop(
    b: &mut FunctionBuilder<'_>,
    count: i64,
    mut body: impl FnMut(&mut FunctionBuilder<'_>, Value),
) {
    let (header, inside, done) = (b.create_block(), b.create_block(), b.create_block());
    b.append_block_param(header, types::I64);
    let zero = b.ins().iconst(types::I64, 0);
    b.ins().jump(header, &[zero.into()]);
    b.switch_to_block(header);
    let at = b.block_params(header).first().copied().unwrap_or(zero);
    let more = b.ins().icmp_imm_s(IntCC::SignedLessThan, at, count);
    b.ins().brif(more, inside, &[], done, &[]);
    b.switch_to_block(inside);
    b.seal_block(inside);
    body(b, at);
    let next = b.ins().iadd_imm_s(at, 1);
    b.ins().jump(header, &[next.into()]);
    b.seal_block(header);
    b.switch_to_block(done);
    b.seal_block(done);
}
