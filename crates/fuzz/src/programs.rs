//! The `programs` target: random, well-typed programs, each built as a dev build and with
//! `--release`, which have to print the same and fail the same way, and run by a reference
//! evaluator written here, which has to agree with them where it's sure. A program prints every
//! value it makes when its block ends, so what it prints is a digest of everything it did.
//!
//! The programs come from a tree (`ast`) that the bytes choose (`generate`), written out as
//! Fennec (`source`) and run by the evaluator (`eval`) without the compiler. They hunt what the
//! audits found by hand: checks taken out on a proof that's wrong, copies that share what they
//! shouldn't, and the order assignments go in.

mod ast;
mod eval;
mod generate;
mod source;

/// Prints the program an input writes, for `fennec-fuzz show programs <file>`.
pub(crate) fn written(input: &[u8]) -> String {
    source::source(&generate::program(input))
}

/// The target: the two builds agree with each other and with the evaluator.
pub(crate) fn programs(input: &[u8]) -> Result<(), String> {
    if crate::llvm::ready().is_err() {
        return Ok(());
    }
    let program = generate::program(input);
    let source = source::source(&program);
    // A task needs the runtime with threads, as a release has beside `fennec`.
    if source.contains("match tally(") && !crate::llvm::threads_runtime_here() {
        return Ok(());
    }
    let with_program = |wrong: String| format!("{wrong}\n--- program\n{source}");
    let module =
        fennec_syntax::parse(&source).map_err(|err| with_program(format!("it wrote bad code: {err:?}")))?;
    fennec_check::check(&module)
        .map_err(|err| with_program(format!("it wrote code the checker refuses: {err:?}")))?;
    let dev = crate::llvm::built_and_run(&source, "programs", false).map_err(with_program)?;
    let release = crate::llvm::built_and_run(&source, "programs", true).map_err(with_program)?;
    let shown = |(printed, said, succeeded): &(Vec<u8>, String, bool)| {
        let ended = if *succeeded { "ran to the end".to_owned() } else { format!("failed: {said}") };
        format!("{}({ended})", String::from_utf8_lossy(printed))
    };
    if dev != release {
        return Err(with_program(format!(
            "the dev build and the --release build disagree:\n--- dev\n{}\n--- release\n{}",
            shown(&dev),
            shown(&release)
        )));
    }
    if let Some((printed, succeeded)) = eval::run(&program)
        && (printed.as_bytes() != dev.0.as_slice() || succeeded != dev.2)
    {
        let ended = if succeeded { "ran to the end" } else { "failed" };
        return Err(with_program(format!(
            "the builds disagree with the evaluator:\n--- built\n{}\n--- evaluator\n{printed}({ended})",
            shown(&dev)
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{generate, source};

    #[test]
    fn what_it_writes_is_a_program_that_builds() {
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> =
                (0_u8..96).map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(3))).collect();
            let source = source::source(&generate::program(&bytes));
            let module =
                fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
            let checked = fennec_check::check(&module);
            assert!(checked.is_ok(), "{source}\n{:?}", checked.err());
            // `fennec build` refuses positional arguments that are easy to get backwards.
            let mut sources = fennec_syntax::Sources::new();
            sources.add("program.fen", source.clone());
            let found = fennec_lint::positional_arguments(&module, &sources);
            let said: Vec<String> =
                found.iter().map(|lint| format!("{}: {}", lint.rule, lint.message())).collect();
            assert!(said.is_empty(), "{source}\n{said:#?}");
        }
    }
}
