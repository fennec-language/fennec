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

/// Whether `Named` is made of native blocks in a program with no other threads, which it is once
/// native blocks take enums with `str` and `list[str]` fields (the LLVM lane's steps 1 and 2).
/// `FENNEC_FUZZ_NAMED_NATIVE` turns the check on before then, to run against a branch that has them.
const NAMED_GOES_NATIVE: bool = true;

fn named_goes_native() -> bool {
    NAMED_GOES_NATIVE || std::env::var_os("FENNEC_FUZZ_NAMED_NATIVE").is_some()
}

/// Whether `Named` stays native blocks in a program calling a function that fails two ways, as
/// `either`'s arrow, `fails Refused | Shut`, does. Today it doesn't: one such call, from an arrow or a
/// `def`, and the whole program makes none. Until the LLVM lane turns this on, the check leaves
/// those programs out, while everything else about them is still checked.
const TWO_WAY_FAILURES_KEEP_NAMED_NATIVE: bool = false;

/// Whether the dev build just made calls `fennec_native_made`, as code making a native block
/// does. The runtime defines it, so only a call counts, `bl` on arm64 and `call` on x86-64.
fn makes_native_blocks() -> Result<bool, String> {
    let built = crate::llvm::built_executable("programs");
    let listed = std::process::Command::new("objdump")
        .args(["-d", "--no-show-raw-insn"])
        .arg(&built)
        .output()
        .map_err(|err| format!("couldn't run objdump: {err}"))?;
    Ok(String::from_utf8_lossy(&listed.stdout).lines().any(|line| {
        let mut words = line.split_whitespace().skip(1);
        let call = matches!(words.next(), Some("bl" | "call" | "callq"));
        call && (line.ends_with("<fennec_native_made>") || line.ends_with("<_fennec_native_made>"))
    }))
}

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
    // A task needs the runtime with threads, as a release has beside `fennec`. Where there's
    // none, a program that launches one, through `tally` or a `sent`, can't be built.
    let launches = source.contains("match tally(") || source.contains("match sent");
    if launches && !crate::llvm::threads_runtime_here() {
        return Ok(());
    }
    let with_program = |wrong: String| format!("{wrong}\n--- program\n{source}");
    let module =
        fennec_syntax::parse(&source).map_err(|err| with_program(format!("it wrote bad code: {err:?}")))?;
    fennec_check::check(&module)
        .map_err(|err| with_program(format!("it wrote code the checker refuses: {err:?}")))?;
    let dev = crate::llvm::built_and_run(&source, "programs", false).map_err(with_program)?;
    // In a program with no other threads, `Named` is made of native blocks, which a dev build
    // says it made by calling `fennec_native_made`. Only one that printed a `Named`'s digest, whose
    // leaves read `l(...)`, surely made one: code that never runs, like everything after a
    // failure the compiler can see, makes none and can be left out.
    let printed_named = dev.0.windows(2).any(|pair| pair == b"l(");
    let two_ways = source.contains("either(() =>");
    let checked_named = TWO_WAY_FAILURES_KEEP_NAMED_NATIVE || !two_ways;
    if named_goes_native() && program.threadless && printed_named && checked_named && !makes_native_blocks()?
    {
        return Err(with_program(
            "`Named` isn't made of native blocks, in a program with no other threads".to_owned(),
        ));
    }
    let release = crate::llvm::built_and_run(&source, "programs", true).map_err(with_program)?;
    let shown = |(printed, said, succeeded): &(Vec<u8>, String, bool)| {
        let ended = if *succeeded { "ran to the end".to_owned() } else { format!("failed: {said}") };
        format!("{}({ended})", String::from_utf8_lossy(printed))
    };
    // Built checking proofs (`FENNEC_CHECK_PROOFS=1`), a stop on one is a false proof, whether or
    // not both builds stop the same way. Built with AddressSanitizer (`FENNEC_SANITIZE=address`),
    // a report is memory touched that the program doesn't own, which both builds can share. A
    // dev build stops when a native block reaches the runtime.
    for (how, (_, said, _)) in [("dev", &dev), ("--release", &release)] {
        if said.contains("handed a native block") {
            return Err(with_program(format!(
                "the {how} build handed a native block to the runtime: {said}"
            )));
        }
        if said.contains("proof broken") {
            return Err(with_program(format!(
                "a proof the compiler made is false, in the {how} build: {said}"
            )));
        }
        if said.contains("AddressSanitizer") {
            return Err(with_program(format!("the {how} build touched memory it doesn't own: {said}")));
        }
    }
    // A program that runs `parallel` does the same on one thread as on several.
    if source.contains("parallel(") {
        for threads in [1, 4, 8] {
            let ran = crate::llvm::ran_again_on("programs", threads).map_err(with_program)?;
            if ran != release {
                return Err(with_program(format!(
                    "the --release build does something else on {threads} threads:\n--- on {threads}\n{}\n--- as it ran first\n{}",
                    shown(&ran),
                    shown(&release)
                )));
            }
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

    /// Trees, `Node` and `Named`, are built, changed through `ref`, copied while they change,
    /// built deep, and built in loops that fail partway, often enough to matter.
    #[test]
    fn it_writes_trees_of_every_kind() {
        let mut found = [0_usize; 9];
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> = (0_u8..160)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
                .collect();
            let source = source::source(&generate::program(&bytes));
            let body = source.split("def main").last().unwrap_or_default();
            let marks = [
                "case .Branch(ref label",
                "deep(20000",
                "100 // (",
                "Node.Tagged(tags=[",
                "digest_all(",
                "named_digest(.Pair(",
                "case .Pair(ref left",
                "chain(20000",
                "f\"<{(100 // (",
            ];
            for (count, mark) in found.iter_mut().zip(marks) {
                *count += usize::from(body.contains(mark));
            }
        }
        assert!(found.iter().all(|count| *count >= 3), "{found:?}");
    }

    /// Local `ref`s name list items, fields, a holder's items, its list and its tag, and items
    /// under a lock, and pass themselves on as `ref` arguments, often enough to matter.
    #[test]
    fn it_writes_local_refs_of_every_kind() {
        let mut found = [0_usize; 6];
        // Two lengths of input, since some kinds are rare.
        for (seed, length) in (0_u8..=255).flat_map(|seed| [(seed, 160_u8), (seed, 96)]) {
            let bytes: Vec<u8> = (0_u8..length)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
                .collect();
            let source = source::source(&generate::program(&bytes));
            let body = source.split("def main").last().unwrap_or_default();
            let refs: Vec<&str> =
                source.lines().map(str::trim).filter(|line| line.starts_with("ref ")).collect();
            let field = |names: &[&str]| {
                refs.iter().any(|line| line.rsplit_once('.').is_some_and(|(_, field)| names.contains(&field)))
            };
            let kinds = [
                field(&["tag"]),
                field(&["items"]),
                field(&["x", "y"]),
                refs.iter().any(|line| line.contains(".items[") && !line.starts_with("ref slot")),
                refs.iter().any(|line| line.starts_with("ref slot")),
                body.contains("bump(ref v"),
            ];
            for (count, kind) in found.iter_mut().zip(kinds) {
                *count += usize::from(kind);
            }
        }
        assert!(found.iter().all(|count| *count >= 3), "{found:?}");
    }

    /// One program in four has no other threads anywhere, which `Named` needs to be made of
    /// native blocks, and builds `Named` values.
    #[test]
    fn some_programs_have_no_other_threads() {
        let (mut threadless, mut named) = (0, 0);
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> = (0_u8..160)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
                .collect();
            let program = generate::program(&bytes);
            let source = source::source(&program);
            if program.threadless {
                assert!(!source.contains("launch(") && !source.contains("parallel("), "{source}");
                threadless += 1;
                named += usize::from(source.contains("named_digest(.Pair("));
            }
        }
        assert!(threadless >= 32 && named >= 3, "{threadless} with no threads, {named} making Named");
    }

    /// A holder is passed a list read from its own field by a `ref self` method, in `main` and on
    /// `parallel`'s threads, and a copy of a container of containers is changed deep inside: the
    /// shapes of use-after-frees native code once had.
    #[test]
    fn it_passes_a_holder_its_own_list() {
        let (mut plain, mut threaded, mut nested) = (0, 0, 0);
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> = (0_u8..160)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
                .collect();
            let source = source::source(&generate::program(&bytes));
            let body = source.split("def main").last().unwrap_or_default();
            plain += usize::from(body.contains(".grown_by(v"));
            nested += usize::from(body.contains("[\"a\"][0].tags.append("));
            threaded += usize::from(body.contains("sum(regrown("));
        }
        assert!(plain >= 3 && threaded >= 3, "{plain} in main, {threaded} in parallel");
        // And a copy of a container of containers is changed deep inside.
        assert!(nested >= 1, "{nested} dicts of lists of units copied and changed");
    }

    /// A union's members are called on through the union in every way, two and three of them,
    /// and on what's left of three after `is`.
    #[test]
    fn it_calls_on_unions_every_way() {
        let calls = [
            "matched2(",
            "matched3(",
            "checked2(",
            "checked3(",
            "direct2(",
            "direct3(",
            "rest3(",
            "plain2(",
            "plain3(",
        ];
        let mut found = [0_usize; 9];
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> = (0_u8..160)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
                .collect();
            let source = source::source(&generate::program(&bytes));
            let body = source.split("def main").last().unwrap_or_default();
            for (count, call) in found.iter_mut().zip(calls) {
                *count += usize::from(body.contains(call));
            }
        }
        assert!(found.iter().all(|count| *count >= 2), "{found:?}");
    }

    /// Lists are lent to calls through `ref` beside values read from them, values are assigned to
    /// themselves, dicts are changed through copies, and under locks too, where it's undone.
    #[test]
    fn it_lends_copies_and_assigns_to_itself() {
        let shapes = [
            "appended_all(ref v",
            "[0], ",
            ".items = ",
            "held.counts = kept",
            "held.counts = held.counts",
            "held.counts.pop(",
            "].hp += ",
            "\"].append(",
            "\"] = Unit(",
        ];
        let mut found = [0_usize; 9];
        let mut selves = 0_usize;
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> = (0_u8..160)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
                .collect();
            let source = source::source(&generate::program(&bytes));
            let body = source.split("def main").last().unwrap_or_default();
            let made: String = source
                .split("\ndef ")
                .filter(|def| def.starts_with("locked") || def.starts_with("worker"))
                .collect();
            for (count, shape) in found.iter_mut().zip(shapes) {
                *count += usize::from(body.contains(shape) || made.contains(shape));
            }
            // A local assigned to itself: `name = name`.
            selves += usize::from(body.lines().any(|line| {
                line.trim().split_once(" = ").is_some_and(|(left, right)| {
                    left == right && left.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
            }));
        }
        assert!(
            found.iter().all(|count| *count >= 2) && selves >= 2,
            "{found:?}, {selves} assigned to themselves"
        );
    }

    /// Arrow functions that fail pass it up with `?`, and keys hold floats and `f32`s.
    #[test]
    fn it_fails_in_arrows_and_keys_by_floats() {
        let shapes = [
            "print(tried(() => capped(",
            "print(tried_twice(() => capped(",
            "print(tried_each(",
            "print(ran_fine(() => capped(",
            "print(either(() => paired(capped(",
            "dict[Gauge, int]()",
            "dict[Gauge32, int]()",
            "dict[Level, int]()",
            "dict[Level32, int]()",
            ": set[Marked] = {",
            "nan()",
            "-0.0",
        ];
        let mut found = [0_usize; 12];
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> = (0_u8..160)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
                .collect();
            let source = source::source(&generate::program(&bytes));
            let body = source.split("def main").last().unwrap_or_default();
            for (count, shape) in found.iter_mut().zip(shapes) {
                *count += usize::from(body.contains(shape));
            }
        }
        assert!(found.iter().all(|count| *count >= 1), "{found:?}");
    }

    /// Changes are planned on `parallel`'s threads and applied in a plain loop afterwards.
    #[test]
    fn it_plans_on_threads_then_applies() {
        let mut found = 0;
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> = (0_u8..160)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
                .collect();
            let source = source::source(&generate::program(&bytes));
            found += usize::from(source.contains("for change in v"));
        }
        assert!(found >= 3, "{found} programs plan and apply");
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
            if let Err(err) = fennec_check::check(&module) {
                unreachable!("{source}\n{err:?}");
            }
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
