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
    // Built checking proofs (`FENNEC_CHECK_PROOFS=1`), a stop on one is a false proof, whether or
    // not both builds stop the same way. Built with AddressSanitizer (`FENNEC_SANITIZE=address`),
    // a report is memory touched that the program doesn't own, which both builds can share.
    for (how, (_, said, _)) in [("dev", &dev), ("--release", &release)] {
        if said.contains("proof broken") {
            return Err(with_program(format!(
                "a proof the compiler made is false, in the {how} build: {said}"
            )));
        }
        if said.contains("AddressSanitizer") {
            return Err(with_program(format!("the {how} build touched memory it doesn't own: {said}")));
        }
    }
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
    use super::{eval, generate, source};

    /// The evaluator is sure what nearly every program does, or the comparison with it would say
    /// little: it's unsure only where the language doesn't promise an outcome.
    #[test]
    fn the_evaluator_is_sure_of_nearly_every_program() {
        let mut sure = 0;
        let mut failed = 0;
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> = (0_u8..160)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
                .collect();
            if let Some((_, succeeded)) = eval::run(&generate::program(&bytes)) {
                sure += 1;
                failed += usize::from(!succeeded);
            }
        }
        assert!(sure > 240, "sure of only {sure} of 256");
        assert!(failed > 0, "no program failed, so failures are never compared");
    }

    /// Trees are built, changed through `ref`, copied while they change, built deep, and built in
    /// loops that fail partway, often enough to matter.
    #[test]
    fn it_writes_trees_of_every_kind() {
        let mut found = [0_usize; 5];
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> = (0_u8..160)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
                .collect();
            let source = source::source(&generate::program(&bytes));
            let body = source.split("def main").last().unwrap_or_default();
            let marks =
                ["case .Branch(ref label", "deep(20000", "100 // (", "Node.Tagged(tags=[", "digest_all("];
            for (count, mark) in found.iter_mut().zip(marks) {
                *count += usize::from(body.contains(mark));
            }
        }
        assert!(found.iter().all(|count| *count >= 3), "{found:?}");
    }

    #[test]
    fn what_it_writes_is_a_program_that_builds() {
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> =
                (0_u8..96).map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(3))).collect();
            let source = source::source(&generate::program(&bytes));
            // Small enough to build well inside a minute, however the bytes go.
            assert!(source.lines().count() < 2000, "{} lines", source.lines().count());
            let module =
                fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
            let signatures =
                fennec_check::check(&module).unwrap_or_else(|err| unreachable!("{source}\n{err:?}"));
            // `fennec build` refuses positional arguments that are easy to get backwards.
            let mut sources = fennec_syntax::Sources::new();
            sources.add("program.fen", source.clone());
            let found = fennec_lint::positional_arguments(&module, &sources);
            let said: Vec<String> =
                found.iter().map(|lint| format!("{}: {}", lint.rule, lint.message())).collect();
            assert!(said.is_empty(), "{source}\n{said:#?}");
            // It refuses a copy made each time round a loop that isn't said with `copy`, too.
            let copies =
                fennec_lint::implicit_copies(&fennec_ir::costs(&fennec_ir::lower(&module, &signatures)));
            let said: Vec<&str> = copies.iter().map(|copy| copy.message.as_str()).collect();
            assert!(said.is_empty(), "{source}\n{said:#?}");
        }
    }
}
