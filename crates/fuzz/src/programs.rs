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
mod eval;
mod generate;
mod source;

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

/// The target: the two builds agree with each other and with the evaluator.
pub(crate) fn programs(input: &[u8]) -> Result<(), String> {
    if crate::llvm::ready().is_err() {
        return Ok(());
    }
    let program = generate::program(input);
    let source = source::source(&program);
    // A task needs the runtime with threads, as a release has beside `fennec`. Where there's
    // none, a program that may launch one, through `tally`, `guard_all` or a `sent`, can't be
    // built.
    let launches = !program.threadless;
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
    // failure the compiler can see, makes none and can be left out. A program built for a heap
    // audit keeps no native blocks at all, so there's nothing to look for.
    let printed_named = dev.0.windows(2).any(|pair| pair == b"l(");
    if program.threadless && printed_named && !crate::llvm::audited() && !makes_native_blocks()? {
        return Err(with_program(
            "`Named` isn't made of native blocks, in a program with no other threads".to_owned(),
        ));
    }
    let release = crate::llvm::built_and_run(&source, "programs", true).map_err(with_program)?;
    // Built checking proofs (`FENNEC_CHECK_PROOFS=1`), a stop on one is a false proof, whether or
    // not both builds stop the same way. Built with AddressSanitizer (`FENNEC_SANITIZE=address`),
    // a report is memory touched that the program doesn't own, which both builds can share. A
    // dev build stops when a native block reaches the runtime. Built for a heap audit
    // (`FENNEC_AUDIT=1`), a line of the audit's is a block left with no holder or counted
    // otherwise than its holders, which both builds can share too.
    for (how, (_, said, _)) in [("dev", &dev), ("--release", &release)] {
        if said.contains(crate::llvm::AUDIT) {
            return Err(with_program(format!(
                "the {how} build's heap audit found a block left or miscounted:\n{said}"
            )));
        }
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
                    if gives.starts_with("with ") || gives.starts_with("Iterator") {
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
        for seed in 0_u8..=255 {
            let bytes: Vec<u8> = (0_u8..160)
                .map(|at| at.wrapping_mul(seed).wrapping_add(seed.rotate_left(5)) ^ at.rotate_left(2))
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
            assert_eq!(said.as_deref(), Some("this Id refers to something that's been removed"));
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
