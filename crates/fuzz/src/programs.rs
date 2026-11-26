//! The `programs` target: random, well-typed programs, each built as a dev build and with
//! `--release`, which have to print the same and fail the same way, and run by a reference
//! evaluator written here, which has to agree with them where it's sure, and by the oracle
//! (`crates/oracle`), which runs its source and has to agree where it models all of it. A
//! program prints every value it makes when its block ends, so what it prints is a digest of
//! everything it did.
//!
//! The programs come from a tree (`ast`) that the bytes choose (`generate`), written out as
//! Fennec (`source`) and run by the evaluator (`eval`) without the compiler. They hunt what the
//! audits found by hand: checks taken out on a proof that's wrong, copies that share what they
//! shouldn't, and the order assignments go in.

mod ast;
mod crews;
mod eval;
mod generate;
mod messaging;
mod opening;
mod pools;
mod saving;
mod source;
mod storing;

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

/// How many random programs the oracle ran, and why it refused the rest. The builds are held to
/// the oracle only on the programs it runs, so this is how much of the target's comparison with it
/// says anything: a refusal skips it without a word.
#[derive(Default)]
pub(crate) struct OracleShare {
    pub(crate) programs: usize,
    pub(crate) ran: usize,
    /// How many it refused for each reason, with the numbers in a reason left out, so each kind is
    /// counted once.
    refused: std::collections::BTreeMap<String, usize>,
}

/// Runs the program each input writes through the oracle alone, and counts how many it ran and
/// why it refused the rest.
pub(crate) fn oracle_share(inputs: impl IntoIterator<Item = Vec<u8>>) -> OracleShare {
    let mut share = OracleShare::default();
    for input in inputs {
        let outcome = fennec_oracle::run_source(&written(&input), "programs.fen");
        share.programs += 1;
        match outcome.ended {
            fennec_oracle::Ended::Unsupported(why) => {
                let first = why.lines().next().unwrap_or_default();
                let kind: String = first.chars().filter(|c| !c.is_ascii_digit()).collect();
                *share.refused.entry(kind).or_default() += 1;
            }
            fennec_oracle::Ended::Exited(_) | fennec_oracle::Ended::Stopped(_) => share.ran += 1,
        }
    }
    share
}

impl std::fmt::Display for OracleShare {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let percent = self.ran * 100 / self.programs.max(1);
        write!(f, "the oracle ran {} of {} random programs ({percent}%)", self.ran, self.programs)?;
        let mut kinds: Vec<(&String, &usize)> = self.refused.iter().collect();
        kinds.sort_by(|(first, many), (second, more)| more.cmp(many).then(first.cmp(second)));
        let said: Vec<String> = kinds.iter().map(|(why, count)| format!("{count} {why}")).collect();
        if !said.is_empty() {
            write!(f, "; refused: {}", said.join("; "))?;
        }
        Ok(())
    }
}

/// What a watching build caught, as `said` says it, if it caught something: a block a heap audit
/// found left or miscounted (`FENNEC_AUDIT=1`), a native block handed to the runtime, a false
/// proof (`FENNEC_CHECK_PROOFS=1`) or memory touched that the program doesn't own
/// (`FENNEC_SANITIZE=address`). Each fails the program whether or not every build says the same.
fn caught(how: &str, said: &str) -> Option<String> {
    if said.contains(crate::llvm::AUDIT) {
        return Some(format!("the {how} build's heap audit found a block left or miscounted:\n{said}"));
    }
    if said.contains("handed a native block") {
        return Some(format!("the {how} build handed a native block to the runtime: {said}"));
    }
    if said.contains("proof broken") {
        return Some(format!("a proof the compiler made is false, in the {how} build: {said}"));
    }
    if said.contains("AddressSanitizer") {
        return Some(format!("the {how} build touched memory it doesn't own: {said}"));
    }
    None
}

/// Whether `input`'s program is built a third time with every type boxed: a quarter of them, by
/// their bytes.
fn boxed_too(input: &[u8]) -> bool {
    input.iter().fold(0_u8, |sum, byte| sum.wrapping_add(*byte)) % 4 == 1
}

/// The target: the two builds agree with each other and with the evaluator, and a boxed third
/// build, where there's one, with them.
pub(crate) fn programs(input: &[u8]) -> Result<(), String> {
    if !crate::llvm::can_build()? {
        return Ok(());
    }
    let program = generate::program(input);
    let source = source::source(&program);
    // A task needs the runtime with threads, as a release has beside `fennec`. Where there's
    // none, a program that may launch one, through `tally`, `guard_all` or a `sent`, can't be
    // built.
    let launches = !program.threadless;
    if launches && !crate::llvm::can_build_threads()? {
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
    // failure the compiler can see, makes none and can be left out. A program built for a heap
    // audit keeps no native blocks at all, so there's nothing to look for.
    let printed_named = dev.0.windows(2).any(|pair| pair == b"l(");
    if program.threadless && printed_named && !crate::llvm::audited() && !makes_native_blocks()? {
        return Err(with_program(
            "`Named` isn't made of native blocks, in a program with no other threads".to_owned(),
        ));
    }
    let release = crate::llvm::built_and_run(&source, "programs", true).map_err(with_program)?;
    // A quarter of the programs are built a third time with every type in a block of its own
    // (`FENNEC_BOXED_TYPES=1`), as the edit fuzzer's boxed versions are: a value taken out of
    // a list, a struct or an optional has to come out the same whichever way it's laid out, which
    // #2282 found it didn't for an optional popped out of a list.
    let boxed = if boxed_too(input) {
        Some(
            crate::llvm::built_and_run_laid_out(&source, "programs-boxed", false, true)
                .map_err(with_program)?,
        )
    } else {
        None
    };
    // Built checking proofs (`FENNEC_CHECK_PROOFS=1`), a stop on one is a false proof, whether or
    // not both builds stop the same way. Built with AddressSanitizer (`FENNEC_SANITIZE=address`),
    // a report is memory touched that the program doesn't own, which both builds can share. A
    // dev build stops when a native block reaches the runtime. Built for a heap audit
    // (`FENNEC_AUDIT=1`), a line of the audit's is a block left with no holder or counted
    // otherwise than its holders, which both builds can share too.
    let mut builds = vec![("dev", &dev), ("--release", &release)];
    if let Some(boxed) = &boxed {
        builds.push(("boxed dev", boxed));
    }
    for (how, (_, said, _)) in builds {
        if let Some(caught) = caught(how, said) {
            return Err(with_program(caught));
        }
    }
    // A program that runs `parallel` does the same on one thread as on several.
    if source.contains("parallel(") {
        for threads in [1, 4, 8, 16] {
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
    if let Some(boxed) = &boxed
        && *boxed != dev
    {
        return Err(with_program(format!(
            "the dev build with every type boxed (FENNEC_BOXED_TYPES=1) and the one laid out in place \
             disagree:\n--- boxed\n{}\n--- in place\n{}",
            shown(boxed),
            shown(&dev)
        )));
    }
    agrees_with_the_evaluators(&program, &source, &dev).map_err(with_program)
}

/// What a dev build printed and said, and whether it succeeded, as one text to show.
fn shown((printed, said, succeeded): &(Vec<u8>, String, bool)) -> String {
    let ended = if *succeeded { "ran to the end".to_owned() } else { format!("failed: {said}") };
    format!("{}({ended})", String::from_utf8_lossy(printed))
}

/// The dev build agrees with the generator's evaluator, where it's sure, and with the oracle,
/// where it models the program.
fn agrees_with_the_evaluators(
    program: &ast::Program,
    source: &str,
    dev: &(Vec<u8>, String, bool),
) -> Result<(), String> {
    let expected = eval::run_saying(program);
    // Where the evaluator knows what a failure says, as a stream's item does, the builds say it.
    if let Some((_, false, Some(said))) = &expected
        && !dev.2
        && !dev.1.contains(said.as_str())
    {
        return Err(format!(
            "the builds stop saying something else than the evaluator:\n--- built\n{}\n--- evaluator\n{said}",
            dev.1
        ));
    }
    if let Some((printed, succeeded, _)) = expected
        && (printed.as_bytes() != dev.0.as_slice() || succeeded != dev.2)
    {
        let ended = if succeeded { "ran to the end" } else { "failed" };
        return Err(format!(
            "the builds disagree with the evaluator:\n--- built\n{}\n--- evaluator\n{printed}({ended})",
            shown(dev)
        ));
    }
    // The oracle (crates/oracle) runs the program a second way, from its source rather than the
    // generator's tree. Where it models everything the program does, the builds print what it
    // prints and end as it ends; where it doesn't, it says so, and there's nothing to compare.
    let oracle = fennec_oracle::run_source(source, "programs.fen");
    if let Some(code) = oracle.exit_code()
        && (oracle.stdout != dev.0 || (code == 0) != dev.2)
    {
        let ended = if code == 0 { "ran to the end".to_owned() } else { format!("ended with {code}") };
        return Err(format!(
            "the builds disagree with the oracle:\n--- built\n{}\n--- oracle\n{}({ended})",
            shown(dev),
            String::from_utf8_lossy(&oracle.stdout)
        ));
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
            if let Some((_, succeeded, _)) = eval::run_saying(&generate::program(&bytes)) {
                sure += 1;
                failed += usize::from(!succeeded);
            }
        }
        assert!(sure > 240, "sure of only {sure} of 256");
        assert!(failed > 0, "no program failed, so failures are never compared");
    }

    /// The least share of random programs, in percent, the oracle has to run. It refuses the rest,
    /// and a refusal skips the target's comparison with it without a word. It goes up as what it
    /// refuses comes to be modeled, and never down: it runs all 64 since a listener left behind a
    /// channel to every listener, with nothing else able to read, falls behind as the runtime's does.
    const ORACLE_RUNS_AT_LEAST: usize = 100;

    /// The oracle runs at least `ORACLE_RUNS_AT_LEAST` percent of random programs, or holding the
    /// builds to it says little. What it refused, by why, is in the message.
    #[test]
    fn the_oracle_runs_its_share_of_programs() {
        let inputs = (0_u8..64).map(|seed| -> Vec<u8> {
            (0_u8..160)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
                .collect()
        });
        let share = super::oracle_share(inputs);
        assert!(share.ran * 100 >= share.programs * ORACLE_RUNS_AT_LEAST, "{share}");
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

    /// Inside a loop, no list of nodes is appended to or set with a node holding that same list,
    /// as in `nodes.append(.Branch(label="a", children=nodes))`, which doubles it every round:
    /// with loops inside loops, more than any machine holds. Outside one it still happens.
    #[test]
    fn no_list_of_nodes_takes_itself_in_a_loop() {
        let (mut in_loops, mut outside) = (0, 0);
        for (seed, length) in (0_u8..=255).flat_map(|seed| [(seed, 160_u8), (seed, 96)]) {
            let bytes: Vec<u8> = (0_u8..length)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
                .collect();
            let source = source::source(&generate::program(&bytes));
            // Each open block's indent, and whether it's a loop's.
            let mut open: Vec<(usize, bool)> = Vec::new();
            for line in source.lines().filter(|line| !line.trim().is_empty()) {
                let indent = line.len() - line.trim_start().len();
                let line = line.trim();
                open.retain(|(at, _)| *at < indent);
                let in_loop = open.iter().any(|(_, looping)| *looping);
                if line.ends_with(':') {
                    open.push((indent, line.starts_with("for ") || line.starts_with("while ")));
                }
                let target = line
                    .strip_prefix("if len(")
                    .and_then(|rest| rest.split_once(") > 0: "))
                    .map(|(name, _)| name)
                    .or_else(|| line.split_once(".append(").map(|(name, _)| name))
                    .filter(|name| {
                        name.strip_prefix('v').is_some_and(|digits| {
                            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
                        })
                    });
                let nodes = ["Leaf(", "Number(", "Branch(", "Tagged(", "node_or_leaf("];
                let Some(name) = target.filter(|_| nodes.iter().any(|node| line.contains(node))) else {
                    continue;
                };
                if line.contains(&format!("children={name})")) || line.contains(&format!("inner={name})")) {
                    assert!(!in_loop, "a list of nodes takes itself in a loop: {line}");
                    outside += 1;
                }
                in_loops += usize::from(in_loop);
            }
        }
        assert!(in_loops >= 50, "only {in_loops} lists of nodes appended to or set in loops");
        assert!(outside >= 3, "only {outside} lists of nodes take themselves outside loops");
    }

    /// Local `ref`s name list items, fields, a holder's items, its list and its tag, and items
    /// under a lock, and pass themselves on as `ref` arguments, often enough to matter. The
    /// rarest, a `ref` to a holder's item, comes up in about one program in fifty. So the inputs
    /// are drawn from the fuzzer's own random numbers, uniform as a run's are, and are enough for
    /// each kind to come up dozens of times. Patterned bytes drew that kind 2 to 6 times in 512,
    /// so any change to how the generator spends its bytes could tip it under the bar.
    #[test]
    fn it_writes_local_refs_of_every_kind() {
        let mut found = [0_usize; 6];
        for number in 0..2048 {
            let mut drawn = crate::mutate::Rng::new(1, number);
            let bytes: Vec<u8> = (0..20).flat_map(|_| drawn.next().to_le_bytes()).collect();
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
        assert!(found.iter().all(|count| *count >= 16), "{found:?}");
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
            "tried_twice(() => capped(",
            "print(fallback(() => capped(",
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
        let mut found = [0_usize; 13];
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

    /// A source's `lend x except:` cleanup runs on a failure and on a crash, in `main` and in a
    /// launched task.
    #[test]
    fn it_cleans_up_after_failures_and_crashes() {
        let shapes = [
            "refuse=True, crash=False, noted=True",
            "refuse=True, crash=False, noted=False",
            "refuse=False, crash=True",
            "refuse=False, crash=False",
            "guard_all([",
        ];
        let mut found = [0_usize; 5];
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

    /// `with` blocks over the program's own sources are left every way there is, nested and
    /// together, so that each cleanup is seen to run, innermost first.
    #[test]
    fn it_leaves_with_blocks_every_way() {
        let ways = [
            "got = capped(i, limit=",
            "total = total + capped(i, limit=",
            "match i % 2:",
            "        return\n",
            "        break\n",
            "        continue\n",
            "_ = [1, 2][i + 2]",
            "with ward(\"a\", stdout) as a, ",
            "capped(capped(i, limit=1000)?, limit=",
            "?, limit=1000)? * 2",
            "except Refused(): -1",
        ];
        let mut found = [0_usize; 11];
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> = (0_u8..160)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
                .collect();
            let source = source::source(&generate::program(&bytes));
            for left in source.split("\ndef left").skip(1) {
                let left = left.split("\n\n").next().unwrap_or_default();
                for (count, way) in found.iter_mut().zip(ways) {
                    *count += usize::from(left.contains(way));
                }
            }
        }
        assert!(found.iter().all(|count| *count >= 1), "{found:?}");
    }

    /// Every `def` that gives a value gives it with `return`, as the language will require: its
    /// last line returns or fails. A `with` source or a generator gives its values otherwise.
    #[test]
    fn it_returns_from_every_def_explicitly() {
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> = (0_u8..160)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
                .collect();
            for source in [source::source(&generate::program(&bytes)), crate::generate::program(&bytes)] {
                let lines: Vec<&str> = source.lines().collect();
                for (at, line) in lines.iter().enumerate() {
                    let trimmed = line.trim_start();
                    let Some(gives) = trimmed.strip_prefix("def ").and_then(def_gives) else {
                        continue;
                    };
                    // A source and a generator give with `lend` and `yield`, and an interface's
                    // method has no body to end.
                    if gives.starts_with("with ") || gives.starts_with("Iterator") || !trimmed.ends_with(':')
                    {
                        continue;
                    }
                    let indent = line.len() - trimmed.len();
                    let last = lines
                        .iter()
                        .skip(at + 1)
                        .take_while(|next| {
                            next.trim().is_empty() || next.len() - next.trim_start().len() > indent
                        })
                        .filter(|next| !next.trim().is_empty())
                        .last()
                        .copied()
                        .unwrap_or_default();
                    let value = last
                        .trim()
                        .split_once(": ")
                        .filter(|_| last.trim().starts_with("case "))
                        .map_or(last.trim(), |(_, value)| value);
                    assert!(
                        value.starts_with("return ") || value.starts_with("fail "),
                        "a def that gives a value ends without `return`: {line}\n{last}"
                    );
                }
            }
        }
    }

    /// What a `def` line says its function gives, written after the parenthesis that closes its
    /// parameters, so a parameter's own `() -> int` isn't taken for it.
    fn def_gives(def: &str) -> Option<&str> {
        let mut depth = 0_usize;
        for (at, written) in def.char_indices() {
            match written {
                '(' => depth += 1,
                ')' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return def.get(at + 1..)?.strip_prefix(" -> ");
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Programs are written only in the forms locked on 2026-11-11: no failure is handled with
    /// `else`, every catch-all `except why:` either has a `case` for each error or reads `why`,
    /// and `len(x)` and `abs(x)` stand where the methods did.
    #[test]
    fn it_handles_failures_with_except() {
        let wanted = [
            "except why:",
            "        case Refused():",
            "lend name except:",
            "    case Refused(code=code):",
            "except Refused(): -1",
            "except Refused(): \"refused\"",
            "            case _:",
        ];
        let mut found = [0_usize; 7];
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> = (0_u8..160)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
                .collect();
            let program = source::source(&generate::program(&bytes));
            let lines: Vec<&str> = program.lines().collect();
            for (at, line) in lines.iter().enumerate() {
                let trimmed = line.trim_start();
                assert!(!trimmed.ends_with(" else as why:") && trimmed != "match why:", "{line}");
                assert!(!(trimmed.starts_with("lend ") && trimmed.ends_with(" else:")), "{line}");
                let opens = ["if ", "elif ", "while ", "for ", "else:", "with ", "lock ", "def ", "case "];
                assert!(
                    !trimmed.ends_with(") else:") || opens.iter().any(|open| trimmed.starts_with(open)),
                    "{line}"
                );
                if trimmed.ends_with(" except why:") {
                    let indent = line.len() - trimmed.len();
                    let block: Vec<&str> = lines
                        .iter()
                        .skip(at + 1)
                        .take_while(|next| {
                            next.trim().is_empty() || next.len() - next.trim_start().len() > indent
                        })
                        .copied()
                        .collect();
                    let arms = block.first().is_some_and(|first| first.trim_start().starts_with("case "));
                    assert!(arms || block.iter().any(|held| held.contains("why")), "`why` unused: {line}");
                }
            }
            for (count, want) in found.iter_mut().zip(wanted) {
                *count += usize::from(program.contains(want));
            }
            assert!(!program.contains(") else \"refused\""), "a failing call inside a print");
            assert!(
                !program.contains(".len()") && !program.contains(".abs()"),
                "a method where a builtin goes"
            );
        }
        assert!(found.iter().all(|count| *count >= 1), "{found:?}");
    }

    /// A generator over `parallel(xs)` is made every way an item can be worked out, and read every
    /// way a stream is: a loop, one that breaks, `sum` and a list comprehension.
    #[test]
    fn it_reads_streams_every_way() {
        let ways = [
            " + ",
            "100 // (item - ",
            "ys[item] for item in parallel(xs)",
            "        if seen == ",
            "sum(y for y in stream)",
            "[y + 1 for y in stream]",
        ];
        let mut found = [0_usize; 6];
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> = (0_u8..160)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
                .collect();
            let source = source::source(&generate::program(&bytes));
            for streamed in source.split("\ndef streamed").skip(1) {
                let streamed = streamed.split("\n\n").next().unwrap_or_default();
                assert!(streamed.contains(" for item in parallel(xs))"), "{streamed}");
                for (count, way) in found.iter_mut().zip(ways) {
                    *count += usize::from(streamed.contains(way));
                }
            }
        }
        assert!(found.iter().all(|count| *count >= 1), "{found:?}");
    }

    /// A loop's int is at least 0 on its first turn and changes sign later, every way: stepped
    /// down past 0, flipped each turn, and reset below 0 partway through.
    #[test]
    fn it_turns_ints_negative_after_the_first_turn() {
        let ways = ["        at -= ", "        sign = -sign\n", "            n = -"];
        let mut found = [0_usize; 3];
        // Hashed bytes, as the shapes' own tests use: bytes linear in the seed come out alike
        // from program to program, so one way can go unpicked by chance of the layout.
        for seed in 0_u32..256 {
            let bytes: Vec<u8> = (0_u32..160)
                .map(|at| {
                    u8::try_from(seed.wrapping_mul(2_654_435_761).wrapping_add(at * 40_503) >> 13 & 0xff)
                        .unwrap_or(0)
                })
                .collect();
            let source = source::source(&generate::program(&bytes));
            for turned in source.split("\ndef turned").skip(1) {
                let turned = turned.split("\n\n").next().unwrap_or_default();
                assert!(turned.contains("xs[") && turned.contains("-9223372036854775804 + "), "{turned}");
                for (count, way) in found.iter_mut().zip(ways) {
                    *count += usize::from(turned.contains(way));
                }
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

    /// The two shapes the review of #1785 found main wrong on, off in random programs until the
    /// fixes land (`LOOKUPS_SWAPPED`, `PART_CRASHES`): every way of writing them checks, and the
    /// evaluator prints what the oracle prints.
    #[test]
    fn the_review_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{Program, SWAP_KEYS, ShelfPlace, Shelve, Shelving, Stmt, SwapField, Swapping};
        let mut body = Vec::new();
        let mut id = 0;
        for field in [SwapField::Counts, SwapField::Lists] {
            for key in 0..SWAP_KEYS.len() {
                for ways in 0..8_u8 {
                    id += 1;
                    body.push(Stmt::Swapped(Swapping {
                        id,
                        base: 3,
                        other_base: 101,
                        field,
                        key,
                        fresh: 40,
                        called: ways & 1 != 0,
                        kept: ways & 2 != 0,
                        owned: ways & 4 != 0,
                        rounds: 2,
                    }));
                }
            }
        }
        for (place, rounds) in [(ShelfPlace::Locked, 1), (ShelfPlace::Owned, 4), (ShelfPlace::Bare, 6)] {
            for in_list in [false, true] {
                id += 1;
                let steps = vec![
                    (9, Shelve::Fill),
                    (0, Shelve::Fill),
                    (1, Shelve::ByHand),
                    (7, Shelve::ByHand),
                    (0, Shelve::Direct),
                    (8, Shelve::Direct),
                    (1, Shelve::Handed),
                    (4, Shelve::Handed),
                    (0, Shelve::HandedByHand),
                    (6, Shelve::HandedByHand),
                ];
                body.push(Stmt::Shelved(Shelving { id, in_list, seen: 2, place, rounds, steps }));
            }
        }
        let program = Program { body, threadless: false };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed);
    }

    /// Every way of writing a name another of its name and type could capture (`CaptureWay`)
    /// checks, and the evaluator prints what the oracle prints: each name means what it does where
    /// it's written. `LockDef` is left out while the checker refuses a nested function's
    /// parameter named as a lock's name, as taking the lock: that's #2011, lock order treating a
    /// nested function as its outermost one, not name capture. It joins the rest once #2011 is
    /// fixed.
    #[test]
    fn the_name_capture_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{CAPTURE_WAYS, CaptureWay, Capturing, Program, Stmt};
        let mut body = Vec::new();
        let mut id = 0;
        for way in CAPTURE_WAYS.into_iter().filter(|way| *way != CaptureWay::LockDef) {
            for n in [0, 7, 19] {
                id += 1;
                body.push(Stmt::Captured(Capturing { id, way, n }));
            }
        }
        let program = Program { body, threadless: true };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed);
    }

    /// Every loop over `enumerate` or `zip` of a generator made right there (`ZipSumming`), the one
    /// lending from a source and the one failing partway, let go partway, run out, or failing,
    /// checks, and the evaluator prints what the oracle prints.
    #[test]
    fn the_zip_generator_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{Program, Stmt, ZIP_WAYS, ZipSumming};
        let mut body = Vec::new();
        for way in ZIP_WAYS {
            for failing in [false, true] {
                for (limit, stop, words) in [(0, 0, 0), (3, 4, 2), (3, 9, 5), (4, 1, 0), (2, 0, 3), (5, 5, 1)]
                {
                    body.push(Stmt::ZipSummed(ZipSumming { way, failing, limit, stop, words }));
                }
            }
        }
        let program = Program { body, threadless: true };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed);
    }

    /// Every way of writing a `match` with `ref` the compiled program once got wrong (`RefWay`),
    /// from every start and with every text, checks, and the evaluator prints what the oracle
    /// prints: each `ref` name's change put back into the field it names.
    #[test]
    fn the_ref_case_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{Program, REF_STARTS, REF_TEXTS, REF_WAYS, RefCasing, Stmt};
        let mut body = Vec::new();
        let mut id = 0;
        for way in REF_WAYS {
            for start in 0..REF_STARTS {
                for text in 0..REF_TEXTS.len() {
                    id += 1;
                    body.push(Stmt::RefCased(RefCasing { id, way, start, text }));
                }
            }
        }
        let program = Program { body, threadless: true };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed);
    }

    /// Every way of narrowing a union and changing it (`NarrowWay`), starting as each member,
    /// checks, and the evaluator prints what the oracle prints: each `grow` changes the member
    /// the union holds, and a `ref` call taking the union leaves it the other one.
    #[test]
    fn the_narrowed_union_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{NARROW_WAYS, Narrowing, Program, Stmt};
        let mut body = Vec::new();
        let mut id = 0;
        for way in NARROW_WAYS {
            for crate_first in [true, false] {
                for n in [0, 4, 9] {
                    id += 1;
                    body.push(Stmt::Narrowed(Narrowing { id, way, crate_first, n }));
                }
            }
        }
        let program = Program { body, threadless: true };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed);
    }

    /// Every way of a `Filed` step checks, and the evaluator prints what the oracle prints: a write
    /// through `with fs.create(...)` that fails, passes a failure up or crashes partway leaves the
    /// file as it was, byte for byte, one that ends replaces it, and a narrowed `Fs` refuses.
    #[test]
    fn the_file_write_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{FILE_PIECES, FILE_WAYS, FileStep, Filing, Program, Stmt};
        let body = (0..3_usize)
            .map(|round| {
                let steps = FILE_WAYS
                    .iter()
                    .enumerate()
                    .map(|(at, way)| {
                        let pieces: Vec<usize> = (0..=(at + round) % 4)
                            .map(|piece| (piece + at + round) % FILE_PIECES.len())
                            .collect();
                        let stop_at = (at * 3 + round) % (pieces.len() + 1);
                        FileStep { way: *way, second: (at + round) % 2 == 1, pieces, stop_at }
                    })
                    .collect();
                Stmt::Filed(Filing { id: round + 1, steps })
            })
            .collect();
        let program = Program { body, threadless: false };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        assert!(printed.contains("crashed\n") && printed.contains("failed Halt"), "{printed}");
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed);
    }

    /// Every step of an `Owned` statement checks, and the evaluator prints what the oracle prints:
    /// each change through a type's own `def []` written back once through its `def []=`, nested,
    /// failing partway and given by `ref`, the built-in methods that change a list and those that
    /// only read it, and the `ref` form's places reached by `=`, `ref`, `for ref` and `parallel`.
    #[test]
    fn the_own_index_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{OWN_STEPS, OwnIndexing, Program, Stmt};
        let body = (0..3_i64)
            .map(|round| {
                let steps = OWN_STEPS.iter().enumerate().map(|(at, step)| {
                    let at = i64::try_from(at).unwrap_or(0);
                    (*step, usize::try_from((at + round) % 3).unwrap_or(0), (at + round * 4) % 10)
                });
                OwnIndexing {
                    id: usize::try_from(round).unwrap_or(0) + 1,
                    base: round * 3,
                    steps: steps.collect(),
                }
            })
            .map(Stmt::Owned)
            .collect();
        let program = Program { body, threadless: false };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        assert!(printed.contains("failed 9\n"), "{printed}");
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed);
    }

    /// Every step of a `Generic` statement, for each type, checks, and the evaluator prints what
    /// the oracle prints: copies made in generic code change apart, and each `ref` changes the
    /// place it names.
    #[test]
    fn the_generic_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{GENERIC_STEPS, GENERIC_TYS, Genericking, Program, Stmt};
        let body = GENERIC_TYS
            .iter()
            .enumerate()
            .map(|(round, ty)| {
                let steps = GENERIC_STEPS
                    .iter()
                    .chain(GENERIC_STEPS.iter())
                    .enumerate()
                    .map(|(at, step)| (*step, (at + round) % 3, (at * 2 + round) % 3))
                    .collect();
                Stmt::Generic(Genericking { id: round + 1, ty: *ty, steps })
            })
            .collect();
        let program = Program { body, threadless: true };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
    }

    /// Every way of a `Stuck` statement checks, and the evaluator prints what the oracle prints:
    /// a wait with a time limit times out and the program goes on, and each deadlock stops it with
    /// exit code 1 after what it printed, saying so.
    #[test]
    fn the_deadlock_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{Program, STUCK_WAYS, Sticking, Stmt, StuckWay};
        let run = |body: Vec<Stmt>| {
            let program = Program { body, threadless: false };
            let source = source::source(&program);
            let module =
                fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
            if let Err(err) = fennec_check::check(&module) {
                unreachable!("{source}\n{err:?}");
            }
            let Some((printed, succeeded, said)) = eval::run_saying(&program) else {
                unreachable!("the evaluator isn't sure:\n{source}")
            };
            let oracle = fennec_oracle::run_source(&source, "programs.fen");
            assert_eq!(oracle.exit_code(), Some(u8::from(!succeeded)), "{source}\n{:?}", oracle.ended);
            assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
            (printed, said)
        };
        let timed = [StuckWay::TimedReceive, StuckWay::TimedSend, StuckWay::TimedReceive]
            .iter()
            .enumerate()
            .map(|(at, way)| Stmt::Stuck(Sticking { id: at + 1, way: *way, n: 3 }))
            .collect();
        let (printed, said) = run(timed);
        assert!(
            printed.ends_with("send timed out\nstuck3 3\nreceived -1\ndone\n") && said.is_none(),
            "{printed}"
        );
        for way in STUCK_WAYS.iter().skip(2) {
            let n = if *way == StuckWay::Rounds { 4 } else { 5 };
            let (_, said) = run(vec![Stmt::Stuck(Sticking { id: 1, way: *way, n })]);
            assert_eq!(
                said.as_deref(),
                Some("a deadlock: every task is waiting for another, and nothing can wake them")
            );
        }
    }

    /// Every step of an `Interfaced` statement, on each type, checks, and the evaluator prints
    /// what the oracle prints: each generic call runs the method of the type it's given, changes
    /// the place a `ref` names and leaves a copy apart, and a failing method passed up stops
    /// where it fails.
    #[test]
    fn the_interface_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{IFACE_STEPS, Interfacing, Program, Stmt};
        let body = (0..3_i64)
            .map(|round| {
                let steps = IFACE_STEPS
                    .iter()
                    .chain(IFACE_STEPS.iter())
                    .enumerate()
                    .map(|(at, step)| {
                        let at = i64::try_from(at).unwrap_or(0);
                        (*step, (at + round) % 2 == 0, (at * 3 + round) % 10)
                    })
                    .collect();
                Stmt::Interfaced(Interfacing { id: usize::try_from(round).unwrap_or(0) + 1, steps })
            })
            .collect();
        let program = Program { body, threadless: true };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        assert!(printed.contains("failed Nope"), "{printed}");
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
    }

    /// Every way of a `LockedTogether` step, on each pair and each way round, with the `lock`
    /// naming its values in the order they were made and against it, checks, and the evaluator
    /// prints what the oracle prints: a `fail` or a crash under one `lock` puts both values back,
    /// and a step that ends keeps both changes.
    #[test]
    fn the_locked_together_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{LOCK_WAYS, LockStep, LockingTogether, Program, Stmt};
        let body = (0..3_i64)
            .map(|round| {
                let mut steps = Vec::new();
                for (at, way) in LOCK_WAYS.iter().chain(LOCK_WAYS.iter()).chain(LOCK_WAYS.iter()).enumerate()
                {
                    let pairs = [(0, 1), (0, 2), (1, 2)];
                    let pair =
                        pairs.get((at + usize::try_from(round).unwrap_or(0)) % 3).copied().unwrap_or((0, 1));
                    let amount = (i64::try_from(at).unwrap_or(0) * 3 + round) % 10;
                    let against = (at + usize::try_from(round).unwrap_or(0)) % 3 == 2;
                    steps.push(LockStep { way: *way, pair, amount, backward: at % 2 == 1, against });
                }
                Stmt::LockedTogether(LockingTogether {
                    id: usize::try_from(round).unwrap_or(0) + 1,
                    base: round * 2,
                    steps,
                })
            })
            .collect();
        let program = Program { body, threadless: false };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        assert!(printed.contains("crashed\n") && printed.contains("failed Short"), "{printed}");
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
    }

    /// Every way of a `Ruled` step that keeps to its rules, three times over with shifting values,
    /// checks, and the evaluator prints what the oracle prints. Then each way that can break one
    /// does, on its own, and stops the program where the evaluator says, making a value too
    /// (#2434): a field lent alone by `ref` stops it as it comes back (#2270).
    #[test]
    fn the_where_rule_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{Program, RULE_WAYS, RuleStep, RuleWay, Ruling, Stmt};
        let run = |steps: Vec<RuleStep>| {
            let program = Program { body: vec![Stmt::Ruled(Ruling { id: 1, steps })], threadless: true };
            let source = source::source(&program);
            let module =
                fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
            if let Err(err) = fennec_check::check(&module) {
                unreachable!("{source}\n{err:?}");
            }
            let Some(ran) = eval::run_saying(&program) else {
                unreachable!("the evaluator isn't sure:\n{source}")
            };
            (source, ran)
        };
        let steps = (0..3_i64)
            .flat_map(|round| {
                RULE_WAYS.iter().enumerate().map(move |(at, way)| RuleStep {
                    way: *way,
                    value: (i64::try_from(at).unwrap_or(0) * 3 + round) % 10,
                    breaks: false,
                })
            })
            .collect();
        let (source, (printed, ended, _)) = run(steps);
        assert!(ended && printed.contains("counted 1\n") && printed.contains("made Gauge1"), "{printed}");
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
        for way in RULE_WAYS {
            let first = RuleStep { way: RuleWay::Raised, value: 2, breaks: false };
            let (source, (printed, ended, said)) = run(vec![first, RuleStep { way, value: 4, breaks: true }]);
            let oracle = fennec_oracle::run_source(&source, "programs.fen");
            assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
            if matches!(way, RuleWay::Raised | RuleWay::Dipped) {
                assert!(ended, "{way:?}: {printed}");
                assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
            } else {
                let said = said.unwrap_or_default();
                assert!(!ended && said.contains("breaks the `where` rule"), "{way:?}: {said}");
                let stopped =
                    matches!(&oracle.ended, fennec_oracle::Ended::Stopped(why) if why.contains(&said));
                assert!(stopped, "{way:?}: {:?}\n{source}", oracle.ended);
            }
        }
    }

    /// Every way of a `Popped` step, twice over, for each type, in a local and in a field, checks,
    /// and the evaluator prints what the oracle prints: an optional of a heap type taken out of a
    /// list is the item it was, and a missing one, or one that's `None`, is what `else` gives.
    #[test]
    fn the_popped_optional_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{POP_TYS, POP_WAYS, Popping, Program, Stmt};
        let body = POP_TYS
            .iter()
            .enumerate()
            .flat_map(|(round, ty)| {
                [false, true].into_iter().map(move |in_field| {
                    let steps = POP_WAYS
                        .iter()
                        .chain(POP_WAYS.iter())
                        .enumerate()
                        .map(|(at, way)| (*way, (at + round) % 3))
                        .collect();
                    let id = round * 2 + usize::from(in_field) + 1;
                    Stmt::Popped(Popping { id, ty: *ty, in_field, steps })
                })
            })
            .collect();
        let program = Program { body, threadless: true };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
    }

    /// Every way of a `Saved` step checks, and the evaluator prints what the oracle prints: a box
    /// through JSON, `std.pack` in order and exact, and TOML, with items taken out of both its
    /// `Table`s before, so each format's `Id`s show, an item added after an exact load taking the
    /// slot that was free, and every refused text and packed gauge.
    #[test]
    fn the_saved_value_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{Program, Stmt};
        use super::saving::{BROKEN_COUNT, PACKED, SaveFormat, SaveStep, Saving};
        let mut steps = vec![
            SaveStep::Hired(None),
            SaveStep::Hired(Some(0)),
            SaveStep::Hired(None),
            SaveStep::Looked(1),
            SaveStep::Fired(2),
            SaveStep::Fired(2),
            SaveStep::Picked(Some(1)),
            SaveStep::Noted(Some(1)),
            SaveStep::Marked(2),
            SaveStep::Marks(1),
            SaveStep::Marks(3),
            SaveStep::Marks(4),
            SaveStep::Moved(Some(2)),
            SaveStep::Moved(None),
            SaveStep::Gauged(4),
            SaveStep::Tagged(Vec::new()),
            SaveStep::Tagged(vec![0]),
            SaveStep::Tagged(Vec::new()),
            SaveStep::Untagged(2),
            SaveStep::Best(1),
            SaveStep::Shown,
            SaveStep::Trip(SaveFormat::Exact),
            SaveStep::Hired(Some(0)),
            SaveStep::Looked(2),
            SaveStep::Shown,
            SaveStep::Fired(2),
            SaveStep::Trip(SaveFormat::Json),
            SaveStep::Trip(SaveFormat::Pack),
            SaveStep::Tagged(vec![1]),
            SaveStep::Trip(SaveFormat::Toml),
            SaveStep::Shown,
        ];
        steps.extend((0..BROKEN_COUNT).map(SaveStep::Broken));
        steps.extend((0..PACKED.len()).map(SaveStep::Packed));
        steps.extend([
            SaveStep::Noted(None),
            SaveStep::Picked(None),
            SaveStep::Noted(Some(2)),
            SaveStep::Marked(0),
            SaveStep::Trip(SaveFormat::Json),
            SaveStep::Shown,
        ]);
        let program = Program { body: vec![Stmt::Saved(Saving { id: 1, steps })], threadless: true };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}\n{source}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
    }

    /// A `Stored` statement checks and the evaluator prints what the oracle prints, its table's
    /// rules each broken, alone and two at once, and transactions kept and undone.
    #[test]
    fn the_stored_row_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{Program, Stmt};
        use super::storing::{Step, Storing};
        let insert = |key, name, size| Step::Insert { key, name, size };
        let steps = vec![
            Step::Sum,
            Step::Rows,
            insert(2, Some(0), 3),
            insert(4, Some(2), 0),
            insert(1, Some(1), 5),
            insert(2, Some(1), 1),
            insert(3, None, 1),
            insert(5, Some(0), -1),
            insert(2, None, -1),
            insert(2, Some(0), -1),
            Step::Rows,
            Step::Update { by: 2, from: 2 },
            Step::Update { by: -3, from: 1 },
            Step::Sizes { above: 1 },
            Step::Transaction {
                steps: vec![insert(3, Some(1), 4), Step::Count, Step::Rows, Step::Delete { below: 4 }],
                fails: true,
            },
            Step::Rows,
            Step::Transaction {
                steps: vec![
                    insert(3, Some(1), 4),
                    insert(3, Some(1), 4),
                    Step::Update { by: 1, from: 3 },
                    Step::Count,
                ],
                fails: false,
            },
            Step::Count,
            Step::Delete { below: 4 },
            Step::Sum,
            Step::Delete { below: 9 },
            Step::Sum,
            Step::Sizes { above: 0 },
        ];
        let program = Program { body: vec![Stmt::Stored(Storing { id: 1, steps })], threadless: true };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}\n{source}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
    }

    /// The pools `keeper_pools_check_and_the_evaluator_agrees_with_the_oracle_and_the_builds`
    /// starts with, before those picked at random.
    fn fixed_pools() -> Vec<super::ast::Stmt> {
        use super::ast::Stmt;
        use super::pools::{Pooling, Step};
        let take = |inner: Vec<Step>, breaks| Step::Take { inner, breaks };
        let pool =
            |id, size, resets, refuse, steps| Stmt::Pooled(Pooling { id, size, resets, refuse, steps });
        vec![
            // One link: a failed block's link reset and lent again, a crash with it, a short wait.
            pool(1, 1, true, 0, vec![take(vec![Step::Waits], true), Step::Crashes, take(vec![], false)]),
            // One link that can't be reset: replaced after each failure.
            pool(2, 1, false, 0, vec![take(vec![], true), Step::Crashes, take(vec![Step::Waits], false)]),
            // Two links lent back in the order they were given back, the inner one first.
            pool(3, 2, true, 0, vec![take(vec![take(vec![Step::Waits], false)], false), take(vec![], false)]),
            // Three links that can't be reset, the third open refused.
            pool(
                4,
                3,
                false,
                3,
                vec![
                    take(vec![take(vec![Step::Crashes], true), take(vec![], false)], false),
                    take(vec![take(vec![take(vec![Step::Waits], true)], false)], true),
                    take(vec![], false),
                ],
            ),
            // The first link refused: the pool fails at its `with`.
            pool(5, 2, true, 1, vec![take(vec![], false), take(vec![], false)]),
        ]
    }

    /// `Pooled` statements check, and the evaluator prints what the oracle prints, and what a dev
    /// and a release build print where they can be built here: a link reset and lent again, one
    /// replaced, the order links come back in, an open refused, a pool failing at its `with`, a
    /// task crashing with a link and a short wait timing out, then 20 pools picked at random.
    ///
    /// Run with `FENNEC_CHECK_PROOFS=1` or `FENNEC_AUDIT=1`, a false proof or a block left fails it.
    #[test]
    fn keeper_pools_check_and_the_evaluator_agrees_with_the_oracle_and_the_builds() {
        use super::ast::{Program, Stmt};
        let mut body = fixed_pools();
        let mut state: u64 = 18;
        let mut choose = |n: usize| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(state >> 33).unwrap_or(0) % n.max(1)
        };
        body.extend((6..26).map(|id| Stmt::Pooled(super::pools::picked(id, &mut choose))));
        let mut refused = 0;
        // The oracle meets each pool alone, as some of them it refuses.
        for stmt in &body {
            let program = Program { body: vec![stmt.clone()], threadless: false };
            let source = source::source(&program);
            let module =
                fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
            if let Err(err) = fennec_check::check(&module) {
                unreachable!("{source}\n{err:?}");
            }
            let Some((printed, true, _)) = eval::run_saying(&program) else {
                unreachable!("the evaluator isn't sure, or it failed:\n{source}")
            };
            // Two keepers opening links bump one `shared` tally from a task and the block it was
            // launched from, which the oracle refuses: it can't see that `main` takes one at a time.
            // A pool with many tasks may have more orders of their messages than it tries.
            let oracle = fennec_oracle::run_source(&source, "programs.fen");
            match &oracle.ended {
                fennec_oracle::Ended::Unsupported(why)
                    if why.contains("a launched task or its `with` block")
                        || why.contains("orders of its tasks' messages") =>
                {
                    refused += 1;
                }
                _ => {
                    assert_eq!(oracle.exit_code(), Some(0), "{:?}\n{source}", oracle.ended);
                    assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
                }
            }
        }
        assert!(refused <= 10, "the oracle refused {refused} of 25 pools");
        // The builds meet them all in one program, as two builds take far less than fifty.
        if !(crate::llvm::ready().is_ok() && crate::llvm::threads_runtime_here()) {
            return;
        }
        let program = Program { body, threadless: false };
        let source = source::source(&program);
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        for release in [false, true] {
            let built = crate::llvm::built_and_run(&source, "programs-pooled", release);
            let Ok((said, told, _)) = built else { unreachable!("{built:?}\n{source}") };
            if let Some(wrong) = super::caught(if release { "release" } else { "dev" }, &told) {
                unreachable!("{wrong}\n{source}");
            }
            assert_eq!(String::from_utf8_lossy(&said), printed, "release {release}\n{source}");
        }
    }

    /// The crews `locked_crews_check_and_the_evaluator_agrees_with_the_oracle_and_the_builds` starts
    /// with, before those picked at random.
    fn fixed_crews() -> Vec<super::ast::Stmt> {
        use super::ast::Stmt;
        use super::crews::{Arg, Change, Crew};
        let crew =
            |id, items: &[i64], counts: &[(usize, i64)], locks: Vec<Vec<Change>>, tasks: Vec<Vec<i64>>| {
                Crew {
                    id,
                    items: items.to_vec(),
                    counts: counts.to_vec(),
                    locks,
                    tasks,
                    by: i64::try_from(id % 3).unwrap_or(0),
                }
            };
        vec![
            Stmt::Crewed(crew(
                1,
                &[0, 100],
                &[(0, 8), (1, 5)],
                vec![vec![Change::PopCount(0), Change::AddTwice(Arg::Times(1))], vec![Change::Pop]],
                vec![vec![-1, 2, 0], vec![3]],
            )),
            Stmt::Crewed(crew(
                2,
                &[1],
                &[(2, 4)],
                vec![
                    vec![Change::AddTwice(Arg::LengthLess), Change::Remove(0)],
                    vec![Change::Append(Arg::Plus(-2)), Change::PopCount(2)],
                ],
                vec![vec![5, 6], vec![1, 1], vec![]],
            )),
            Stmt::Crewed(crew(
                3,
                &[],
                &[],
                vec![
                    vec![Change::AddTwice(Arg::Length(2))],
                    vec![Change::Bump(3, Arg::Times(3)), Change::SetCount(1, Arg::LengthLess)],
                    vec![Change::Bump(-1, Arg::Literal(i64::MAX))],
                ],
                vec![vec![2, 4, 6], vec![0]],
            )),
            Stmt::Crewed(crew(
                4,
                &[2, -1, 7],
                &[(0, 1), (1, 2), (2, 3)],
                vec![
                    vec![Change::SetCount(0, Arg::Times(2)), Change::Pop],
                    vec![Change::AddTwice(Arg::Plus(1))],
                ],
                vec![vec![1, 2, 3, 4]],
            )),
        ]
    }

    /// `Crewed` statements check, and the evaluator prints what the oracle prints, and what a dev and
    /// a release build print where they can be built here. Among them:
    /// - #2516's program, a key popped and `add_twice` called with `(number * 1)` in one lock, whose
    ///   task crashes on its second number;
    /// - a crash in a later lock after an earlier one's `ref self` call with a computed argument;
    /// - a place outside the list, an overflow, and a task with nothing to crash on;
    /// - 20 crews picked at random.
    ///
    /// Run with `FENNEC_CHECK_PROOFS=1` or `FENNEC_AUDIT=1`, a false proof or a block left fails it.
    #[test]
    fn locked_crews_check_and_the_evaluator_agrees_with_the_oracle_and_the_builds() {
        use super::ast::{Program, Stmt};
        let mut body = fixed_crews();
        let mut state: u64 = 2516;
        let mut choose = |n: usize| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(state >> 33).unwrap_or(0) % n.max(1)
        };
        body.extend((5..25).map(|id| Stmt::Crewed(super::crews::picked(id, &mut choose))));
        let native = crate::llvm::can_build_threads().unwrap_or_else(|why| unreachable!("{why}"));
        let mut crashed = 0;
        for stmt in body {
            let program = Program { body: vec![stmt], threadless: false };
            let source = source::source(&program);
            let module =
                fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
            if let Err(err) = fennec_check::check(&module) {
                unreachable!("{source}\n{err:?}");
            }
            let Some((printed, true, _)) = eval::run_saying(&program) else {
                unreachable!("the evaluator isn't sure, or it failed:\n{source}")
            };
            crashed += usize::from(printed.contains("crashed"));
            let oracle = fennec_oracle::run_source(&source, "programs.fen");
            assert_eq!(oracle.exit_code(), Some(0), "{:?}\n{source}", oracle.ended);
            assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
            if !native {
                continue;
            }
            for release in [false, true] {
                let built = crate::llvm::built_and_run(&source, "programs-crewed", release);
                let Ok((said, told, _)) = built else { unreachable!("{built:?}\n{source}") };
                // Built checking proofs or for a heap audit, what either catches fails it too.
                if let Some(wrong) = super::caught(if release { "release" } else { "dev" }, &told) {
                    unreachable!("{wrong}\n{source}");
                }
                assert_eq!(String::from_utf8_lossy(&said), printed, "release {release}\n{source}");
            }
        }
        assert!(crashed >= 10, "only {crashed} of 24 crews crashed");
    }

    /// A `Messaged` statement of every shape, flat and not, each case sent with texts short, long,
    /// empty and the sender's own as it changes, checks, and the evaluator prints what the oracle
    /// prints, and what a dev and a release build print where they can be built here.
    #[test]
    fn every_enum_message_shape_checks_and_arrives_as_it_was_sent() {
        use super::ast::{Program, Stmt};
        use super::messaging::{Event, Message, Messaging, Shape, Text, Tree};
        let texts = [Text::Short(1), Text::Long(2), Text::Empty, Text::Kept];
        let events =
            [Event::Join(3, -4), Event::Say(5, Text::Long(6)), Event::Say(7, Text::Kept), Event::Quiet];
        let leaf = |value| Box::new(Tree::Leaf(value));
        let shapes = [
            (Shape::Names, vec![Message::Name(1), Message::Name(0), Message::Name(2)]),
            (Shape::Events, events.iter().map(|event| Message::Event(*event)).collect()),
            (
                Shape::Wrapped,
                vec![
                    Message::Inner(Event::Say(1, Text::Kept), Text::Long(3)),
                    Message::Plain(-2),
                    Message::Inner(Event::Quiet, Text::Empty),
                ],
            ),
            (Shape::Said, events.iter().zip(0..).map(|(event, seq)| Message::Said(seq, *event)).collect()),
            (Shape::Listing, vec![Message::Items(texts.to_vec()), Message::Empty, Message::Items(vec![])]),
            (
                Shape::Tally,
                vec![Message::Counts(vec![(0, 1), (2, -3)]), Message::Nothing, Message::Counts(vec![])],
            ),
            (
                Shape::Failure,
                vec![Message::Bad(-5), Message::Worse(Text::Kept), Message::Worse(Text::Long(4))],
            ),
            (
                Shape::Tree,
                vec![
                    Message::Tree(Tree::Pair(leaf(1), Box::new(Tree::Pair(leaf(2), leaf(-3))))),
                    Message::Tree(Tree::Leaf(4)),
                ],
            ),
        ];
        let body = shapes
            .into_iter()
            .zip(1..)
            .map(|((shape, messages), id)| {
                Stmt::Messaged(Messaging { id, shape, messages, changes: id % 3 != 0 })
            })
            .collect();
        let program = Program { body, threadless: false };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}\n{source}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
        if !crate::llvm::can_build_threads().unwrap_or_else(|why| unreachable!("{why}")) {
            return;
        }
        for release in [false, true] {
            let built = crate::llvm::built_and_run(&source, "programs-messaged", release);
            let Ok((said, _, _)) = built else { unreachable!("{built:?}\n{source}") };
            assert_eq!(String::from_utf8_lossy(&said), printed, "release {release}\n{source}");
        }
    }

    /// 24 `Messaged` statements of shapes and messages picked at random, in one program, check, and
    /// the evaluator prints what the oracle prints, and what a dev and a release build print where
    /// they can be built here.
    #[test]
    fn random_enum_messages_arrive_as_they_were_sent() {
        use super::ast::{Program, Stmt};
        let mut state: u64 = 0x5eed_0005;
        let mut choose = |n: usize| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(state >> 33).unwrap_or(0) % n.max(1)
        };
        let body = (1..=24).map(|id| Stmt::Messaged(super::messaging::picked(id, &mut choose))).collect();
        let program = Program { body, threadless: false };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}\n{source}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
        if !crate::llvm::can_build_threads().unwrap_or_else(|why| unreachable!("{why}")) {
            return;
        }
        for release in [false, true] {
            let built = crate::llvm::built_and_run(&source, "programs-messaged-random", release);
            let Ok((said, _, _)) = built else { unreachable!("{built:?}\n{source}") };
            assert_eq!(String::from_utf8_lossy(&said), printed, "release {release}\n{source}");
        }
    }

    /// Every way an `Opened` handle opens and closes, in each place, with and without a lent pair,
    /// and each way the block ends, checks, and the evaluator prints what the oracle prints: a
    /// handle failing to open runs the earlier ones' cleanups, a cleanup that crashes lets the
    /// outer ones run, and a crash stops the program in both.
    #[test]
    fn the_opened_handle_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{Program, Stmt};
        use super::opening::{BlockEnd, Gate, Opening};
        use Gate::{CrashesClose, CrashesUndo, FailsOpen, Plain};
        let cases = [
            (vec![Plain, Plain, Plain], true, BlockEnd::Ends),
            (vec![Plain, FailsOpen, Plain], true, BlockEnd::Ends),
            (vec![Plain, Plain, FailsOpen], false, BlockEnd::Ends),
            (vec![FailsOpen, Plain], false, BlockEnd::Ends),
            (vec![Plain, Plain], true, BlockEnd::Fails),
            (vec![Plain, CrashesUndo, Plain], true, BlockEnd::Fails),
            (vec![Plain, CrashesClose, Plain], true, BlockEnd::Ends),
            (vec![CrashesUndo, Plain, Plain], true, BlockEnd::Crashes),
            (vec![CrashesUndo, FailsOpen], false, BlockEnd::Ends),
            (vec![CrashesClose, CrashesUndo], false, BlockEnd::Fails),
            (vec![Plain, Plain, CrashesClose], false, BlockEnd::Ends),
            (vec![Plain, Plain], false, BlockEnd::Crashes),
        ];
        for (gates, pair, end) in cases {
            let opening = Opening { id: 1, gates, pair, end };
            let (_, stops) = super::opening::printed(&opening);
            let program = Program { body: vec![Stmt::Opened(opening.clone())], threadless: true };
            let source = source::source(&program);
            let module =
                fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
            if let Err(err) = fennec_check::check(&module) {
                unreachable!("{source}\n{err:?}");
            }
            let Some((printed, ran_through, _)) = eval::run_saying(&program) else {
                unreachable!("the evaluator isn't sure:\n{source}")
            };
            assert_eq!(ran_through, !stops, "{opening:?}");
            let oracle = fennec_oracle::run_source(&source, "programs.fen");
            assert_eq!(oracle.exit_code(), Some(u8::from(stops)), "{:?}\n{source}", oracle.ended);
            assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
        }
    }

    /// A sum over `parallel` checks and the evaluator prints what the oracle prints: floats added in
    /// the runtime's tree and, with an `if`, in order into eight totals, over short lists, an empty
    /// one and ones longer than a leaf, and an int sum over a long list. An int sum the tree
    /// overflows stops the program in both.
    #[test]
    fn the_parallel_sum_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{Expr, Program, Stmt, Ty};
        let run = |printed: Vec<Expr>| {
            let body = vec![Stmt::Print(printed.into_iter().map(|expr| (expr, Ty::Float)).collect())];
            let program = Program { body, threadless: false };
            let source = source::source(&program);
            let module =
                fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
            if let Err(err) = fennec_check::check(&module) {
                unreachable!("{source}\n{err:?}");
            }
            let oracle = fennec_oracle::run_source(&source, "programs.fen");
            (eval::run_saying(&program), oracle, source)
        };
        let floats = |list: Expr, scale: f64, filtered: bool| {
            Expr::FloatSum(Box::new(list), Box::new(Expr::Float(scale)), filtered)
        };
        let short = || Expr::Ints(vec![Expr::Int(3), Expr::Int(-7), Expr::Int(10), Expr::Int(4)]);
        let (ran, oracle, source) = run(vec![
            floats(Expr::Upto(3000), 0.1, false),
            floats(Expr::Upto(3000), 0.1, true),
            floats(Expr::Upto(1025), 1e300, false),
            floats(short(), 0.1, false),
            floats(short(), 0.1, true),
            floats(Expr::Ints(Vec::new()), 2.5, false),
            floats(Expr::Ints(Vec::new()), 2.5, true),
        ]);
        let Some((printed, true, _)) = ran else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        assert_eq!(oracle.exit_code(), Some(0), "{:?}\n{source}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
        // The tree's bits and the eight totals' differ.
        let lines: Vec<&str> = printed.lines().collect();
        assert_ne!(lines.first(), lines.get(1), "{printed}");
        let ints = |by: i64| Expr::ParallelSum(Box::new(Expr::Upto(3000)), Box::new(Expr::Int(by)));
        let (ran, oracle, source) = run(vec![Expr::FloatOf(Box::new(ints(1_000_000_007)))]);
        let Some((printed, true, _)) = ran else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
        let (ran, oracle, source) = run(vec![Expr::FloatOf(Box::new(ints(i64::MAX / 3000)))]);
        assert!(matches!(ran, Some((_, false, _))), "{ran:?}\n{source}");
        assert_eq!(oracle.exit_code(), Some(1), "{:?}\n{source}", oracle.ended);
    }

    /// Every way of a `Sized` step, for each of the eight sizes, from each start, checks, and the
    /// evaluator prints what the oracle prints. Then each way that can stop the program does, for
    /// each size, saying in compiled code's words what the evaluator says.
    #[test]
    fn the_sized_number_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{
            Program, SIZE_NUMS, SIZED_WAYS, SizeNum, SizedWay, Sizing, Stmt, sized_choice, sized_step,
        };
        let written = |num: SizeNum, start: i128, ways: &[(SizedWay, bool, usize)]| {
            let mut x = start;
            let mut steps = Vec::new();
            for &(way, breaks, pick) in ways {
                let step = sized_choice(num, x, way, breaks, pick);
                steps.push(step);
                match sized_step(num, x, step.way, step.operand) {
                    Ok(_) if matches!(step.way, SizedWay::Summed | SizedWay::Widened | SizedWay::Parsed) => {}
                    Ok(after) => x = after,
                    Err(_) => break,
                }
            }
            (steps, x)
        };
        let run = |body: Vec<Stmt>| {
            let program = Program { body, threadless: true };
            let source = source::source(&program);
            let module =
                fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
            if let Err(err) = fennec_check::check(&module) {
                unreachable!("{source}\n{err:?}");
            }
            let Some(ran) = eval::run_saying(&program) else {
                unreachable!("the evaluator isn't sure:\n{source}")
            };
            let oracle = fennec_oracle::run_source(&source, "programs.fen");
            assert_eq!(String::from_utf8_lossy(&oracle.stdout), ran.0, "{:?}\n{source}", oracle.ended);
            (source, ran, oracle)
        };
        let mut body = Vec::new();
        for (at, num) in SIZE_NUMS.iter().enumerate() {
            let (least, most) = num.range();
            let low = if num.unsigned() { 3 } else { least + 4 };
            for (round, start) in [7, (most - 4).min(i128::from(i64::MAX)), low].into_iter().enumerate() {
                let ways: Vec<(SizedWay, bool, usize)> = SIZED_WAYS
                    .iter()
                    .filter(|way| {
                        *num != SizeNum::I64
                            || !format!("{way:?}").starts_with("Wrapping")
                                && !format!("{way:?}").starts_with("Saturating")
                    })
                    .enumerate()
                    .map(|(step, way)| (*way, false, (step + round + at) % 4))
                    .collect();
                let (steps, _) = written(*num, start, &ways);
                body.push(Stmt::Sized(Sizing { id: body.len() + 1, num: *num, start, steps }));
            }
        }
        let (_, (printed, ended, _), oracle) = run(body);
        assert!(ended, "{printed}");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
        // Each way that can stop the program, on two sizes each: a program each, since each
        // stops, and each is checked whole, so one a size and way would take a minute.
        for (at, num) in SIZE_NUMS.into_iter().enumerate() {
            let (_, most) = num.range();
            let near_most = (most - 4).min(i128::from(i64::MAX));
            let ways = [
                (SizedWay::Added, near_most),
                (SizedWay::Halved, 7),
                (SizedWay::Shifted, 7),
                (SizedWay::Looped, near_most),
            ];
            for (way, start) in ways.into_iter().skip(at % 4).take(1) {
                // A `u64` starts below the largest int, too far below its most for a loop to
                // reach, so it's taken to its most first, as `saturating_add` does.
                let up = (SizedWay::SaturatingAdd, false, 3);
                let ways = if num == SizeNum::U64 && way == SizedWay::Looped {
                    vec![up, (way, true, 0)]
                } else {
                    vec![(way, true, 0)]
                };
                let (steps, _) = written(num, start, &ways);
                let (source, (_, ended, said), oracle) =
                    run(vec![Stmt::Sized(Sizing { id: 1, num, start, steps })]);
                let said = said.unwrap_or_default();
                assert!(!ended && !said.is_empty(), "{num:?} {way:?}: {source}");
                let stopped =
                    matches!(&oracle.ended, fennec_oracle::Ended::Stopped(why) if why.contains(&said));
                assert!(stopped, "{num:?} {way:?}: {:?} against {said}\n{source}", oracle.ended);
            }
        }
    }

    /// Every way of an `Arrayed` step, at each place and with values of both signs, checks, and the
    /// evaluator prints what the oracle prints: arrays held inline, copied by `=` and changed apart,
    /// wherever the struct holding them is. Then an index past either end of `counts`, reached by
    /// a loop whose bound the proofs can't see and by a call, stops the program as the evaluator
    /// says.
    #[test]
    fn the_array_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{ARRAY_WAYS, ArrayStep, ArrayWay, Arraying, Program, Stmt};
        let ways: Vec<ArrayWay> = ARRAY_WAYS
            .iter()
            .copied()
            .filter(|way| *way != ArrayWay::ForRefSlots || generate::NARROWED_FIELD_WRITES)
            .collect();
        let run = |steps: Vec<ArrayStep>| {
            let program = Program { body: vec![Stmt::Arrayed(Arraying { id: 1, steps })], threadless: true };
            let source = source::source(&program);
            let module =
                fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
            if let Err(err) = fennec_check::check(&module) {
                unreachable!("{source}\n{err:?}");
            }
            let Some(ran) = eval::run_saying(&program) else {
                unreachable!("the evaluator isn't sure:\n{source}")
            };
            (source, ran)
        };
        let steps = (0..3_i64)
            .flat_map(|round| {
                ways.iter().enumerate().map(move |(at, way)| {
                    let at = at + usize::try_from(round).unwrap_or(0);
                    let value = (i64::try_from(at).unwrap_or(0) * 7 + round) % 19 - 9;
                    ArrayStep { way: *way, at, value, past: false }
                })
            })
            .collect();
        let (source, (printed, ended, _)) = run(steps);
        assert!(ended, "{printed}");
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
        for (way, at, says) in [
            (ArrayWay::Looped, 1, "index 4 "),
            (ArrayWay::Picked, 0, "index 4 "),
            (ArrayWay::Picked, 1, "index -5 "),
        ] {
            let first = ArrayStep { way: ArrayWay::Set, at: 2, value: 6, past: false };
            let (source, (printed, ended, said)) =
                run(vec![first, ArrayStep { way, at, value: 3, past: true }]);
            assert!(!ended && said.as_deref().is_some_and(|said| said.starts_with(says)), "{said:?}");
            let oracle = fennec_oracle::run_source(&source, "programs.fen");
            assert_eq!(oracle.exit_code(), Some(1), "{:?}", oracle.ended);
            assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
            let stopped = matches!(&oracle.ended, fennec_oracle::Ended::Stopped(why) if why.contains(says));
            assert!(stopped, "{:?}\n{source}", oracle.ended);
        }
    }

    /// Every way of a `Generated` step, with its generators failing before, partway and not at
    /// all, checks, and the evaluator prints what the oracle prints: each failure wrapped as the
    /// `? from` of the generator that failed says, `zip` asking its first side first.
    #[test]
    fn the_failing_generator_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{GEN_WAYS, GenStep, Generating, Program, Stmt};
        let body = (0..3_i64)
            .map(|round| {
                let steps = GEN_WAYS
                    .iter()
                    .chain(GEN_WAYS.iter())
                    .enumerate()
                    .map(|(at, way)| {
                        let at = i64::try_from(at).unwrap_or(0) + round;
                        GenStep {
                            way: *way,
                            limit: at % 5 + 1,
                            fail_at: (at * 3) % 7,
                            other_at: (at * 5 + 2) % 7,
                        }
                    })
                    .collect();
                Stmt::Generated(Generating { id: usize::try_from(round).unwrap_or(0) + 1, steps })
            })
            .collect();
        let program = Program { body, threadless: true };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        for said in ["failed Left", "failed Right", "failed Lost", "failed Shut", "end "] {
            assert!(printed.contains(said), "{said}: {printed}");
        }
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
    }

    /// Every step of an `Ordered` statement checks, and the evaluator prints what the oracle
    /// prints, each line read left to right: a receiver, a field's receiver, a list's and a dict's
    /// method, an index's base, arguments, operators, a list, a text, and writes, each changed by
    /// a call to its right (#546).
    #[test]
    fn the_evaluation_order_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{InOrder, ORDER_STEPS, Program, Stmt};
        let body = (0..3)
            .map(|round| {
                let steps = ORDER_STEPS
                    .iter()
                    .enumerate()
                    .map(|(at, step)| (*step, (i64::try_from(at).unwrap_or(0) + round * 4) % 10));
                Stmt::Ordered(InOrder {
                    id: usize::try_from(round).unwrap_or(0) + 1,
                    base: round * 3,
                    steps: steps.collect(),
                })
            })
            .collect();
        let program = Program { body, threadless: true };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        assert!(printed.contains("receiver [0, 1, 2, 3] 0 1\n"), "{printed}");
        let oracle = fennec_oracle::run_source(&source, "programs.fen");
        assert_eq!(oracle.exit_code(), Some(0), "{:?}", oracle.ended);
        assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed);
    }

    /// Every way of writing a `Looped` step checks, and the evaluator prints what the oracle
    /// prints: an item taken out of a list, a dict, a table, a ledger's table field or rooms, or
    /// its `items`, put back in a `while True:` loop whose exit comes after the put-back, read once
    /// before the loop or on each turn, with the item changed, what holds it grown, or the item
    /// taken out between turns. A table's item taken out stops the program on the next turn.
    #[test]
    fn the_put_back_loop_shapes_check_and_the_evaluator_agrees_with_the_oracle() {
        use super::ast::{Alias, Aliasing, Between, LEDGER, Looping, Program, Stmt};
        let program = |steps: Vec<Alias>| {
            let program =
                Program { body: vec![Stmt::Aliased(Aliasing { id: 1, base: 4, steps })], threadless: true };
            let source = source::source(&program);
            let module =
                fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
            if let Err(err) = fennec_check::check(&module) {
                unreachable!("{source}\n{err:?}");
            }
            let Some((printed, succeeded, said)) = eval::run_saying(&program) else {
                unreachable!("the evaluator isn't sure:\n{source}")
            };
            let oracle = fennec_oracle::run_source(&source, "programs.fen");
            assert_eq!(oracle.exit_code() == Some(0), succeeded, "{source}\n{:?}", oracle.ended);
            assert_eq!(String::from_utf8_lossy(&oracle.stdout), printed, "{source}");
            (printed, said)
        };
        // The starting holders, lists, spots, table, keepers, ledger and rooms, by their names'
        // places, then the ledger's table taken out by the first step.
        let tables = [4, 7, 9, 10];
        let mut steps = vec![Alias::Part { from: LEDGER, at: 0 }];
        let mut stopping = Vec::new();
        let mut made = 0_i64;
        for to in [1, 2, 3, 4, 6, 7, 9, 10] {
            for between in [Between::Changed, Between::Grown(2), Between::Grown(20), Between::Removed] {
                for (inside, items) in [(false, false), (true, false), (false, true), (true, true)] {
                    if items && !matches!(to, 1 | 6) {
                        continue;
                    }
                    made += 1;
                    let at = usize::from(made % 2 == 0);
                    let looping =
                        Looping { to, at, n: made % 7 - 3, turns: 2 + made % 2, between, inside, items };
                    if between == Between::Removed && tables.contains(&to) {
                        stopping.push(looping);
                    } else {
                        steps.push(Alias::Looped(looping));
                    }
                }
            }
        }
        let (printed, _) = program(steps);
        assert!(printed.contains("g2x19"), "{printed}");
        assert_eq!(stopping.len(), 8);
        for looping in stopping {
            let (_, said) = program(vec![Alias::Part { from: LEDGER, at: 0 }, Alias::Looped(looping)]);
            assert_eq!(
                said.as_deref(),
                Some(
                    "this Id finds nothing in this Table: its item was taken out, or it's from another Table"
                )
            );
        }
    }

    /// Every way of writing a `Listened` statement checks, and the evaluator is sure what it
    /// prints: inboxes kept up or behind, read one at a time or in a loop, and a loop dropping
    /// the oldest, with a message sent before any of them listens or not.
    #[test]
    fn listeners_of_a_channel_to_every_listener_check_and_the_evaluator_is_sure() {
        use super::ast::{Listening, Program, Stmt};
        let mut body = Vec::new();
        let mut id = 0;
        for size in 1..=4 {
            for sent in [1, size, size + 1, 6] {
                for ways in 0..4_u8 {
                    id += 1;
                    let (early, dropping) = (ways & 1 != 0, ways & 2 != 0);
                    let inboxes = if dropping { vec![true] } else { vec![false, true] };
                    body.push(Stmt::Listened(Listening { id, size, early, sent, dropping, inboxes }));
                }
            }
        }
        let program = Program { body, threadless: false };
        let source = source::source(&program);
        let module = fennec_syntax::parse(&source).unwrap_or_else(|err| unreachable!("{err:?}\n{source}"));
        if let Err(err) = fennec_check::check(&module) {
            unreachable!("{source}\n{err:?}");
        }
        let Some((printed, true, _)) = eval::run_saying(&program) else {
            unreachable!("the evaluator isn't sure, or it failed:\n{source}")
        };
        assert!(printed.contains("listened [3, 4, 5, 6]\n"), "{printed}");
        assert!(printed.contains("behind\nbehind\n"), "{printed}");
    }

    /// A quarter of the inputs are built boxed as well: 64 of the 256 inputs of one byte.
    #[test]
    fn a_quarter_of_the_programs_are_built_boxed_too() {
        let boxed = (0_u8..=255).filter(|byte| super::boxed_too(&[*byte])).count();
        assert_eq!(boxed, 64);
        assert!(super::boxed_too(&[200, 5]) && !super::boxed_too(&[200, 6]));
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
