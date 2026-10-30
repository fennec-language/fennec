//! Turning lowered functions into machine code on every core at once. Lowering is quick and
//! shares the program's tables, so it runs in order; code generation is most of the work
//! and needs nothing shared, so each core takes its own share of the functions. The
//! results go into the module in order.

use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::ir::{Function as Ir, UserExternalName};
use cranelift_codegen::isa::TargetIsa;
use cranelift_module::{DataId, FuncId, Module, ModuleReloc};
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

/// A function's IR as text, with each function and data symbol it refers to written by name
/// rather than by its number in this program. A function lowered to the same code gives the
/// same text in any run of any program, so this text is what its machine code depends on.
pub(super) fn canonical_ir(module: &dyn Module, ir: &Ir) -> String {
    let mut ir = ir.clone();
    let declarations = module.declarations();
    let refs: Vec<_> = ir.params.user_named_funcs().iter().map(|(at, name)| (at, name.clone())).collect();
    let mut names = Vec::with_capacity(refs.len());
    for (index, (at, name)) in (0_u32..).zip(refs) {
        let declared = match name.namespace {
            0 => declarations.get_function_decl(FuncId::from_u32(name.index)).name.clone(),
            1 => declarations.get_data_decl(DataId::from_u32(name.index)).name.clone(),
            _ => None,
        };
        names.push(format!("{index} = {}", declared.unwrap_or_default()));
        ir.params.reset_user_func_name(at, UserExternalName::new(name.namespace, index));
    }
    format!("{}\n{}\n", ir.display(), names.join("\n"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::{Shared, compile_functions, module::new_module};
    use crate::types::Program;

    /// Each function's canonical IR, lowered from `source` as `fennec run --jit` would.
    fn lowered(source: &str) -> HashMap<String, String> {
        let module = fennec_syntax::parse(source).unwrap();
        let signatures = fennec_check::check(&module).unwrap();
        let prelude: Vec<_> =
            fennec_syntax::prelude().filter_map(|source| fennec_syntax::parse_prelude(source).ok()).collect();
        let program = Program::from_module(&module, &prelude, &signatures).unwrap();
        let mut jit = new_module(program.main_span).unwrap();
        let mut shared = Shared::new(&program, 0);
        shared.captured = Some(Vec::new());
        compile_functions(&mut jit, &mut shared, &program, &signatures, &std::collections::HashSet::new())
            .unwrap();
        shared.captured.unwrap().into_iter().collect()
    }

    /// Uses a string literal, names and strings the runtime numbers, a dict the interpreter
    /// handles, and a match: everything the program numbers as a whole, or this run does.
    const TARGET: &str = "def target(name: str) -> int:\n    counts = {name: 1, \"fox\": 2}\n    \
        if name == \"fennec\": return counts[name]\n    match len(name):\n        case 3:\n            7\n        \
        case _:\n            counts.get(\"fox\", 0)\n";

    #[test]
    fn a_function_compiles_the_same_in_every_run_and_program() {
        let one = format!("{TARGET}\ndef main(stdout: Stdout):\n    stdout.print(target(\"kit\"))\n");
        let other = format!(
            "def first(text: str) -> int:\n    seen = {{\"zebra\": 3, text: 4}}\n    \
            if text == \"vixen\": return 1\n    seen.get(\"zebra\", 9)\n\n{TARGET}\n\
            def main(stdout: Stdout):\n    stdout.print(first(\"x\"), target(\"kit\"), \"another literal\")\n"
        );
        let lowered_once = lowered(&one);
        assert_eq!(lowered_once, lowered(&one), "two runs of one program lower the same");
        let in_one = lowered_once.get("target").unwrap();
        let in_other = lowered(&other).get("target").cloned().unwrap();
        assert_eq!(in_one, &in_other, "the same function lowers the same in another program");
        assert!(in_one.contains("fennec.literal."), "literals go through symbols:\n{in_one}");
    }
}
