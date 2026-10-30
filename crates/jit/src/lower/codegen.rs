//! Turning lowered functions into machine code on every core at once. Lowering is quick and
//! shares the program's tables, so it runs in order; code generation is most of the work
//! and needs nothing shared, so each core takes its own share of the functions. The
//! results go into the module in order.

use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::ir::Function as Ir;
use cranelift_codegen::isa::TargetIsa;
use cranelift_module::{FuncId, Module, ModuleReloc};
use fennec_syntax::Diagnostic;

use super::internal;
use crate::types::Function;

/// A function's machine code, ready to go into the module.
pub(super) struct MachineCode {
    bytes: Vec<u8>,
    alignment: u64,
    relocs: Vec<ModuleReloc>,
}

impl MachineCode {
    pub(super) fn install(&self, module: &mut dyn Module, id: FuncId) -> Result<(), (String, Diagnostic)> {
        module
            .define_function_bytes(id, self.alignment, &self.bytes, &self.relocs)
            .map_err(|err| ("main".to_owned(), internal(err, fennec_syntax::Span::default())))
    }
}

type Failure = (String, Diagnostic);

/// Generates machine code for every lowered function, spread over the machine's cores.
pub(super) fn compile_all(
    isa: &dyn TargetIsa,
    lowered: Vec<(FuncId, Ir, &Function<'_>)>,
) -> Result<Vec<(FuncId, MachineCode)>, Failure> {
    let workers =
        std::thread::available_parallelism().map_or(1, std::num::NonZero::get).min(lowered.len().max(1));
    let mut shares: Vec<Vec<(FuncId, Ir, &Function<'_>)>> = (0..workers).map(|_| Vec::new()).collect();
    for (at, item) in lowered.into_iter().enumerate() {
        if let Some(share) = shares.get_mut(at % workers) {
            share.push(item);
        }
    }
    let results: Vec<Result<Vec<(FuncId, MachineCode)>, Failure>> = std::thread::scope(|scope| {
        let handles: Vec<_> = shares
            .into_iter()
            .map(|share| {
                scope.spawn(move || {
                    share.into_iter().map(|(id, ir, function)| compile_one(isa, id, ir, function)).collect()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle.join().unwrap_or_else(|_| {
                    Err((
                        "main".to_owned(),
                        internal("code generation stopped", fennec_syntax::Span::default()),
                    ))
                })
            })
            .collect()
    });
    let mut all = Vec::new();
    for result in results {
        all.extend(result?);
    }
    Ok(all)
}

fn compile_one(
    isa: &dyn TargetIsa,
    id: FuncId,
    ir: Ir,
    function: &Function<'_>,
) -> Result<(FuncId, MachineCode), Failure> {
    let failed =
        |err: &dyn std::fmt::Debug| (function.key.clone(), internal(format!("{err:?}"), function.def.span));
    let mut ctx = cranelift_codegen::Context::for_function(ir);
    let alignment = ctx
        .compile(isa, &mut ControlPlane::default())
        .map_err(|err| failed(&err.inner))?
        .buffer
        .min_alignment;
    let alignment = u64::from(alignment);
    let compiled = ctx.compiled_code().ok_or_else(|| failed(&"no machine code"))?;
    let bytes = compiled.code_buffer().to_vec();
    let relocs = compiled
        .buffer
        .relocs()
        .iter()
        .map(|reloc| ModuleReloc::from_mach_reloc(reloc, &ctx.func, id))
        .collect();
    Ok((id, MachineCode { bytes, alignment, relocs }))
}
