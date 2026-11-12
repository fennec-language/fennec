//! A lint over emitted code: every call to a runtime function that can fail is followed by its
//! check. The runtime notes a failure in a flag and gives back a zero (`attempt`), so a call
//! whose flag nothing reads carries on with that zero as if it were a value, and the failure is
//! said later, somewhere else, if at all. Every runtime function can fail but those in
//! `fennec_runtime::NEVER_FAIL`, so one left off that list costs a check, not a missed failure.
//!
//! From each such call, every way on must reach the check, or leave as a failure does, before it
//! calls anything else or gives back as a function that didn't fail. Letting go of what's held
//! comes first on some ways, which calls only what frees. And the call must be in a function that
//! can fail: one emitted as unable to has nowhere to send a failure, so nothing checks there.
//!
//! What emitted code doesn't check yet is held to a ratchet, `unchecked_runtime_calls.txt`: no
//! runtime function may go unchecked that isn't listed, and the bench kernels' counts may only
//! fall.

use std::collections::{BTreeMap, BTreeSet};

/// What reading the flag starts with: its address, for this thread or for `main`'s.
const FLAG_READS: [&str; 2] = ["load ptr, ptr @fennec_failed,", "load ptr, ptr @fennec_failed_main,"];

/// The blocks a failure leaves by.
const WAYS_OUT: [&str; 3] = ["prop", "left_traced", "let_go"];

/// Calls that only let go of what's held, which may come between a call and its check.
fn only_lets_go(callee: &str) -> bool {
    callee.starts_with("llvm.")
        || callee.starts_with("fen_owner_gone")
        || callee.starts_with("fen_release")
        || callee.starts_with("fen_free")
        || callee.contains("_free")
        || callee.contains("release")
        || callee.ends_with("_drop")
}

/// A call to a runtime function that can fail, which nothing checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unchecked {
    /// The function it's in.
    pub function: String,
    /// The runtime function it calls.
    pub callee: String,
    /// Whether that's because the function it's in was emitted as unable to fail, rather than
    /// because a way on from the call doesn't check.
    pub in_function_that_cant_fail: bool,
    /// Why not, and the line.
    pub said: String,
}

/// Each call in `text` to a runtime function not in `never_fail` that nothing checks.
pub fn unchecked_failures(text: &str, never_fail: &[&str]) -> Vec<Unchecked> {
    let never_fail: BTreeSet<&str> = never_fail.iter().copied().collect();
    let can_fail = |callee: &str| callee.starts_with("fennec_") && !never_fail.contains(callee);
    let mut found = Vec::new();
    for function in functions(text) {
        for (label, lines) in &function.blocks {
            for (at, line) in lines.iter().enumerate() {
                let Some(callee) = callee_of(line) else { continue };
                if !can_fail(callee) {
                    continue;
                }
                let missed = |in_function_that_cant_fail: bool, said: String| Unchecked {
                    function: function.name.to_owned(),
                    callee: callee.to_owned(),
                    in_function_that_cant_fail,
                    said,
                };
                if !function.flagged {
                    found.push(missed(true, format!("in a function that can't fail: {}", line.trim())));
                } else if let Err(why) = checked_after(&function, label, at + 1) {
                    found.push(missed(false, format!("{why}: {}", line.trim())));
                }
            }
        }
    }
    found
}

/// A function's name and its blocks' lines, by label.
struct Function<'t> {
    name: &'t str,
    /// Whether it's given the flag a failure leaves in: one that isn't can't fail, so its way out
    /// on a failure is `unreachable`.
    flagged: bool,
    blocks: BTreeMap<&'t str, Vec<&'t str>>,
    first: &'t str,
}

/// The functions defined in module text.
fn functions(text: &str) -> Vec<Function<'_>> {
    let mut found = Vec::new();
    let mut current: Option<Function<'_>> = None;
    let mut label = "";
    for line in text.lines() {
        if line.starts_with("define ") {
            let name = line.split('@').nth(1).and_then(|rest| rest.split('(').next()).unwrap_or("");
            let flagged = line.contains("swifterror");
            current = Some(Function { name, flagged, blocks: BTreeMap::new(), first: "" });
            label = "";
            continue;
        }
        let Some(function) = current.as_mut() else { continue };
        if line == "}" {
            found.extend(current.take());
            continue;
        }
        if let Some(name) = line.strip_suffix(':').filter(|name| !name.starts_with(' ')) {
            label = name;
            if function.first.is_empty() {
                function.first = name;
            }
            function.blocks.entry(name).or_default();
            continue;
        }
        if label.is_empty() {
            label = "entry";
            function.first = "entry";
        }
        function.blocks.entry(label).or_default().push(line);
    }
    found
}

/// The function a line calls by name, if it calls one.
fn callee_of(line: &str) -> Option<&str> {
    let after = line.split_once("call ")?.1;
    let name = after.split_once('@')?.1;
    let name = name.trim_start_matches('"');
    name.split(['(', '"']).next()
}

/// Whether every way on from line `at` of block `label` checks the flag or leaves as a failure,
/// or why not.
fn checked_after(function: &Function<'_>, label: &str, at: usize) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    let mut waiting = vec![(label, at)];
    while let Some((label, from)) = waiting.pop() {
        if WAYS_OUT.iter().any(|out| label.starts_with(out)) {
            continue;
        }
        let Some(lines) = function.blocks.get(label) else {
            return Err(format!("no block `{label}`"));
        };
        let mut next = Vec::new();
        let mut done = false;
        for line in lines.iter().skip(from) {
            let line = line.trim();
            if FLAG_READS.iter().any(|read| line.contains(read))
                || line.starts_with("store ptr inttoptr (i64 1 to ptr), ptr %err")
                || line == "unreachable"
            {
                done = true;
                break;
            }
            if let Some(callee) = callee_of(line)
                && !only_lets_go(callee)
            {
                return Err(format!("`{callee}` is called first"));
            }
            if line.starts_with("ret ") {
                return Err("it gives back first".to_owned());
            }
            if line.starts_with("br ") || line.starts_with("switch ") {
                next.extend(
                    line.split("label %").skip(1).filter_map(|rest| rest.split([',', ' ', ']']).next()),
                );
                done = true;
                break;
            }
        }
        if !done {
            return Err(format!("block `{label}` ends without a branch"));
        }
        for target in next {
            if seen.insert(target) {
                waiting.push((target, 0));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the unit tests take as never failing: letting go, as most of the runtime can't.
    const NEVER: [&str; 2] = ["fennec_free", "fennec_mark"];

    /// A call followed by its check, after letting go of a temporary, passes; one followed by
    /// other work first, or that gives back without one, doesn't, nor does one in a function
    /// that can't fail. Leaving as a failure does passes, and a call that never fails needs no
    /// check.
    #[test]
    fn a_call_that_can_fail_needs_its_check() {
        let checked = "define void @f(ptr %t, ptr swifterror %err) #0 {\nentry:\n  %s = call ptr @fennec_str_slice(ptr %t)\n  br i1 %gone, label %let, label %on\nlet:\n  call void @fennec_free(ptr %t)\n  br label %on\non:\n  %flag_at = load ptr, ptr @fennec_failed, !invariant.load !1\n  ret void\n}\n";
        assert_eq!(unchecked_failures(checked, &NEVER), []);
        let other = checked.replace(
            "call void @fennec_free(ptr %t)",
            "call void @fennec_mark()\n  call void @fennec_print_str(ptr %s)",
        );
        // The slice's way on calls another first; the print's reaches the check.
        let missed = unchecked_failures(&other, &NEVER);
        assert_eq!(missed.len(), 1, "{other}");
        assert!(
            missed[0].callee == "fennec_str_slice" && !missed[0].in_function_that_cant_fail,
            "{missed:?}"
        );
        let back = "define void @f(ptr %t, ptr swifterror %err) #0 {\nentry:\n  %s = call ptr @fennec_str_slice(ptr %t)\n  ret void\n}\n";
        assert_eq!(unchecked_failures(back, &NEVER).len(), 1);
        let unflagged = back.replace(", ptr swifterror %err", "");
        let missed = unchecked_failures(&unflagged, &NEVER);
        assert!(missed.len() == 1 && missed[0].in_function_that_cant_fail, "{missed:?}");
        let leaves = "define void @f(i64 %i, ptr swifterror %err) #0 {\nentry:\n  call void @fennec_out_of_range(i64 %i)\n  br label %prop\nprop:\n  ret void\n}\n";
        assert_eq!(unchecked_failures(leaves, &NEVER), []);
    }

    /// The runtime's list of what never fails, read from its source.
    fn never_fail() -> Vec<&'static str> {
        let source = include_str!("../../runtime/src/lib.rs");
        let list = source.split("pub const NEVER_FAIL").nth(1).unwrap_or("").split("];").next().unwrap_or("");
        list.split('"').skip(1).step_by(2).collect()
    }

    /// Which way a call goes unchecked, as the ratchet's file names it.
    fn kind(missed: &Unchecked) -> &'static str {
        if missed.in_function_that_cant_fail { "in-cant-fail" } else { "unchecked" }
    }

    type Key = (String, String);

    /// The ratchet's file: what's knowingly left unchecked, each with how many calls the bench
    /// kernels make that way.
    fn allowed(text: &str) -> BTreeMap<Key, usize> {
        text.lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                let (callee, kind, count) = (words.next()?, words.next()?, words.next()?.parse().ok()?);
                Some(((callee.to_owned(), kind.to_owned()), count))
            })
            .collect()
    }

    /// No runtime function goes unchecked in any example, test program or kernel that the
    /// ratchet's file doesn't list, and the kernels' unchecked calls are exactly as many as it
    /// says: a fix lowers them, and `FENNEC_UPDATE_UNCHECKED=1` writes the file as it now is.
    /// Counted over the kernels alone, so that adding an example never moves them.
    #[test]
    fn no_runtime_call_goes_unchecked_past_the_ratchet() {
        let never_fail = never_fail();
        assert!(never_fail.len() > 20 && never_fail.contains(&"fennec_sin"), "{never_fail:?}");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("unchecked_runtime_calls.txt");
        let allowed = allowed(&std::fs::read_to_string(&file).unwrap_or_default());
        let (mut anywhere, mut counted, mut new) =
            (BTreeSet::new(), BTreeMap::<Key, usize>::new(), Vec::new());
        let mut emitted = 0;
        for (folder, counts) in
            [("examples", false), ("crates/fennec/tests/fixtures", false), ("bench/kernels", true)]
        {
            let Ok(entries) = std::fs::read_dir(root.join(folder)) else { continue };
            let mut paths: Vec<_> = entries.filter_map(Result::ok).map(|entry| entry.path()).collect();
            paths.sort();
            for path in paths.iter().filter(|path| path.extension().is_some_and(|ext| ext == "fen")) {
                let Ok(source) = std::fs::read_to_string(path) else { continue };
                let Ok(module) = fennec_syntax::parse(&source) else { continue };
                let Ok(checked) = fennec_check::check(&module) else { continue };
                let program = fennec_ir::lower(&module, &checked);
                let Ok((units, _, _)) = crate::emit(&program) else { continue };
                emitted += 1;
                for missed in units.iter().flat_map(|unit| unchecked_failures(&unit.text, &never_fail)) {
                    let key = (missed.callee.clone(), kind(&missed).to_owned());
                    if !allowed.contains_key(&key) {
                        new.push(format!(
                            "{}: {} in {}, {}",
                            path.display(),
                            missed.callee,
                            missed.function,
                            missed.said
                        ));
                    }
                    if counts {
                        *counted.entry(key.clone()).or_default() += 1;
                    }
                    anywhere.insert(key);
                }
            }
        }
        assert!(emitted >= 150, "only {emitted} programs emitted");
        if std::env::var_os("FENNEC_UPDATE_UNCHECKED").is_some_and(|on| on == "1") {
            let mut text = String::from(HEADER);
            for key in &anywhere {
                let count = counted.get(key).copied().unwrap_or(0);
                text.push_str(&format!("{} {} {count}\n", key.0, key.1));
            }
            std::fs::write(&file, text)
                .unwrap_or_else(|err| panic!("couldn't write {}: {err}", file.display()));
            return;
        }
        assert!(
            new.is_empty(),
            "{} calls nothing checks, of runtime functions unchecked_runtime_calls.txt doesn't list. Check each, or list it, knowingly, with FENNEC_UPDATE_UNCHECKED=1:\n{}",
            new.len(),
            new.join("\n")
        );
        let moved: Vec<String> = allowed
            .iter()
            .filter(|(key, count)| {
                counted.get(*key).copied().unwrap_or(0) != **count || !anywhere.contains(*key)
            })
            .map(|(key, count)| {
                format!(
                    "{} {}: {count} listed, {} in the kernels",
                    key.0,
                    key.1,
                    counted.get(key).copied().unwrap_or(0)
                )
            })
            .collect();
        assert!(
            moved.is_empty(),
            "the kernels' unchecked calls aren't what unchecked_runtime_calls.txt says. More is a new unchecked call; fewer is a fix, to write down with FENNEC_UPDATE_UNCHECKED=1:\n{}",
            moved.join("\n")
        );
    }

    const HEADER: &str = "\
# Runtime calls emitted code doesn't check yet, held to a ratchet by
# crates/llvm/src/failure_checks.rs: every runtime function not in fennec_runtime::NEVER_FAIL can
# fail, so a call to one needs the failure flag read after it. Each line is a runtime function, how
# it goes unchecked (`unchecked`: a way on from the call doesn't read the flag; `in-cant-fail`: the
# function it's in was emitted as unable to fail, so nothing checks there), and how many such calls
# the bench kernels make. A function not listed here mustn't go unchecked anywhere, and the
# kernels' counts must match: a fix lowers them, written with FENNEC_UPDATE_UNCHECKED=1.
";
}
