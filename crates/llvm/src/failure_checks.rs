//! A lint over emitted code: every call to a runtime function that can fail is followed by its
//! check. The runtime notes a failure in a flag and gives back a zero (`attempt`), so a call
//! whose flag nothing reads carries on with that zero as if it were a value. Which functions can
//! fail is the runtime's to say, in `fennec_runtime::CAN_FAIL`.
//!
//! From each such call, every way on must reach the check, or leave as a failure does, before it
//! calls anything else or gives back as a function that didn't fail. Letting go of what's held
//! comes first on some ways, which calls only what frees. And the call must be in a function that
//! can fail: one that can't leaves by `unreachable`, which a failure there would reach.

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

/// Each call in `text` to a function in `can_fail` that a way on from it doesn't check: the
/// function it's in, the callee, and the line.
pub fn unchecked_failures(text: &str, never_fail: &[&str]) -> Vec<String> {
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
                if !function.flagged {
                    found.push(format!(
                        "{}: `{}` can fail in a function that can't: {}",
                        function.name,
                        callee,
                        line.trim()
                    ));
                } else if let Err(why) = checked_after(&function, label, at + 1) {
                    found.push(format!(
                        "{}: `{}` isn't checked ({why}): {}",
                        function.name,
                        callee,
                        line.trim()
                    ));
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

    const CAN_FAIL: [&str; 2] = ["fennec_str_slice", "fennec_out_of_range"];

    /// A call followed by its check, after letting go of a temporary, passes; one followed by
    /// other work first, or that gives back without one, doesn't. Leaving as a failure does
    /// passes.
    #[test]
    fn a_call_that_can_fail_needs_its_check() {
        let checked = "define void @f(ptr %t, ptr swifterror %err) #0 {\nentry:\n  %s = call ptr @fennec_str_slice(ptr %t)\n  br i1 %gone, label %let, label %on\nlet:\n  call void @fennec_free(ptr %t)\n  br label %on\non:\n  %flag_at = load ptr, ptr @fennec_failed, !invariant.load !1\n  ret void\n}\n";
        assert_eq!(unchecked_failures(checked, &CAN_FAIL), Vec::<String>::new());
        let other = checked.replace("call void @fennec_free(ptr %t)", "call void @fennec_print_str(ptr %s)");
        assert_eq!(unchecked_failures(&other, &CAN_FAIL).len(), 1, "{other}");
        let back = "define void @f(ptr %t, ptr swifterror %err) #0 {\nentry:\n  %s = call ptr @fennec_str_slice(ptr %t)\n  ret void\n}\n";
        assert_eq!(unchecked_failures(back, &CAN_FAIL).len(), 1);
        let unflagged = back.replace(", ptr swifterror %err", "");
        assert!(unchecked_failures(&unflagged, &CAN_FAIL)[0].contains("can't"), "{unflagged}");
        let leaves = "define void @f(i64 %i, ptr swifterror %err) #0 {\nentry:\n  call void @fennec_out_of_range(i64 %i)\n  br label %prop\nprop:\n  ret void\n}\n";
        assert_eq!(unchecked_failures(leaves, &CAN_FAIL), Vec::<String>::new());
    }

    /// The runtime's own list of what can fail, read from its source.
    fn runtime_can_fail() -> Vec<&'static str> {
        let source = include_str!("../../runtime/src/lib.rs");
        let list = source.split("pub const NEVER_FAIL").nth(1).unwrap_or("").split("];").next().unwrap_or("");
        list.split('"').skip(1).step_by(2).collect()
    }

    /// Every example, test program and kernel checks the runtime's flag after each call that can
    /// set it, as emitted.
    #[test]
    fn every_program_checks_what_the_runtime_can_fail_at() {
        let can_fail = runtime_can_fail();
        assert!(can_fail.len() > 40 && can_fail.contains(&"fennec_str_slice"), "{can_fail:?}");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut found = Vec::new();
        let mut emitted = 0;
        for folder in ["examples", "crates/fennec/tests/fixtures", "bench/kernels"] {
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
                for unit in &units {
                    for missed in unchecked_failures(&unit.text, &can_fail) {
                        found.push(format!("{}: {missed}", path.display()));
                    }
                }
            }
        }
        assert!(emitted >= 150, "only {emitted} programs emitted");
        assert!(found.is_empty(), "{} unchecked:\n{}", found.len(), found.join("\n"));
    }
}
