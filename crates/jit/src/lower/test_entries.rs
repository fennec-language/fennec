//! Compiling tests to run natively: each test is a function with an entry point of its own, in
//! one module, so a failure ends only that test.

use std::collections::HashMap;

use cranelift_frontend::FunctionBuilderContext;
use cranelift_module::Module;
use fennec_interp::embed::Tables;
use fennec_syntax::Diagnostic;

use super::module::{define_entry, new_module, retrying};
use super::{Shared, cached, compile_bodies, internal, symbols};
use crate::types::Program;

/// Tests in machine code, by name.
pub(crate) struct CompiledTests {
    pub(crate) entries: Vec<(String, *const u8)>,
    // Keeps the machine code alive while it runs.
    pub(crate) _module: cranelift_jit::JITModule,
}

/// Compiles the tests in `names`, a map from each function's key to its test's name. A test
/// that can't be compiled has no entry, and is left to the interpreter.
pub(crate) fn compile_tests(
    program: &Program<'_>,
    signatures: &fennec_check::Signatures,
    stack_limit: usize,
    cache: Option<(&fennec_build::Cache, &str)>,
    names: &HashMap<String, String>,
) -> Result<(CompiledTests, Tables), Diagnostic> {
    let stack_limit = i64::try_from(stack_limit).unwrap_or(i64::MAX);
    let span = program.main_span;
    let ((tests, shared), _) = retrying(|skipped| {
        let fatal = |diagnostic| ("main".to_owned(), diagnostic);
        let mut module = new_module(span).map_err(fatal)?;
        let mut shared = Shared::new(program, stack_limit);
        shared.cache = cached::CacheUse::of(cache, true);
        compile_bodies(&mut module, &mut shared, program, signatures, skipped)?;
        symbols::define(&mut module, &shared).map_err(fatal)?;
        let mut ctx = module.make_context();
        let mut builder_ctx = FunctionBuilderContext::new();
        let mut ids = Vec::new();
        for function in &program.functions {
            let (Some(name), Some(compiled)) = (names.get(&function.key), shared.compiled.get(&function.key))
            else {
                continue;
            };
            let entry_name = format!("fennec_entry_{}", symbols::digest(&function.key));
            let target = (entry_name.as_str(), (compiled.id, compiled.ret));
            let id =
                define_entry(&mut module, &mut ctx, &mut builder_ctx, &shared, target, function.def.span)
                    .map_err(fatal)?;
            ids.push((name.clone(), id));
        }
        module.finalize_definitions().map_err(|err| fatal(internal(err, span)))?;
        let entries = ids.into_iter().map(|(name, id)| (name, module.get_finalized_function(id))).collect();
        Ok((CompiledTests { entries, _module: module }, shared))
    })?;
    Ok((tests, shared.tables))
}
