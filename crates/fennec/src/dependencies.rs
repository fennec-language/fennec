//! `fennec update`: pins a kit's dependencies from git in `kit.lock`, see `fennec-deps`, records
//! there what each kit the program depends on holds, which a check stops at any kit being handed
//! more than, and adds the layouts of what the kit saves in `std.pack` to `pack.lock`.
//! `fennec update --check` says whether `kit.lock`'s holds are what the program hands over, for CI,
//! and writes nothing. `fennec fetch`: fetches
//! what `kit.lock` pins and this machine hasn't got. They're the only commands that reach the
//! network: loading a program fetches nothing.

use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;

/// `fennec update`, `fennec update --check` or `fennec fetch`, as `which` and `rest` name, in the kit
/// around the current folder.
pub(crate) fn named(which: &str, rest: &[&str]) -> ExitCode {
    match (which, rest) {
        ("fetch", []) => fetch("."),
        ("update", []) => update("."),
        ("update", ["--check"]) => holds_up_to_date("."),
        _ => {
            eprintln!("error: `fennec {which}` takes no {}", rest.join(" "));
            ExitCode::FAILURE
        }
    }
}

/// `fennec update`, in the kit around `path`.
pub(crate) fn update(path: &str) -> ExitCode {
    let root = match kit_around(path, "update") {
        Ok(root) => root,
        Err(code) => return code,
    };
    let Some(kits) = fennec_deps::Kits::of_this_machine() else {
        eprintln!("error: there's no home folder to keep fetched kits in; set FENNEC_HOME");
        return ExitCode::FAILURE;
    };
    match kits.update(&root) {
        Ok(said) => {
            print!("{said}");
            match lock_holds(&root, path).and_then(|said| {
                print!("{said}");
                lock_layouts(&root)
            }) {
                Ok(said) => {
                    print!("{said}");
                    ExitCode::SUCCESS
                }
                Err(message) => {
                    eprintln!("error: {message}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// What each kit the program depends on holds now, as `kit.lock` would say it, and each change from
/// what `kit.lock` says: a kit holding something new, worded as "now holds" or "would now hold",
/// and one that no longer holds something.
struct HoldsNow {
    holds: Vec<(String, Vec<String>)>,
    changes: Vec<String>,
}

/// Works out what each kit the program hands anything to holds: from all of the kit's code, tests
/// included, since anything that's checked is held to `kit.lock`.
fn holds_now(root: &Path, shown: &str, (lock, will): (&str, &str)) -> Result<HoldsNow, String> {
    let program = fennec_kit::load(root, shown, fennec_kit::Entry::Tests)?;
    let worked =
        fennec_check::holds_worked_out(&program.module).map_err(|problems| program.render(&problems))?;
    let before = fennec_deps::holds(lock)?;
    let had = |kit: &str| -> Vec<&str> {
        before
            .iter()
            .filter(|(had, _)| had == kit)
            .flat_map(|(_, held)| held.iter().map(String::as_str))
            .collect()
    };
    let mut changes = Vec::new();
    for (kit, held) in &worked {
        for (capability, span) in held {
            if had(kit).contains(&capability.as_str()) {
                continue;
            }
            let at = program
                .sources
                .locate(*span)
                .map(|(path, line, column, _)| format!(", first handed over at {path}:{line}:{column}"))
                .unwrap_or_default();
            changes.push(format!("`{kit}` {will} `{capability}`{at}"));
        }
    }
    for (kit, held) in &before {
        for capability in held {
            if !worked.get(kit).is_some_and(|now| now.contains_key(capability)) {
                changes.push(format!("`{kit}` no longer holds `{capability}`"));
            }
        }
    }
    let holds = worked.into_iter().map(|(kit, held)| (kit, held.into_keys().collect())).collect();
    Ok(HoldsNow { holds, changes })
}

/// Records in `kit.lock` what each kit the program depends on holds, saying what changed. This is
/// the one place a kit's being handed more is accepted: a check stops at it until it's here.
fn lock_holds(root: &Path, shown: &str) -> Result<String, String> {
    let path = root.join("kit.lock");
    let lock = std::fs::read_to_string(&path).unwrap_or_default();
    let now = holds_now(root, shown, (&lock, "now holds"))?;
    if now.changes.is_empty() {
        return Ok(String::new());
    }
    fennec_dist::write_kit_file(&path, &fennec_deps::with_holds(&lock, &now.holds))?;
    let mut said = String::new();
    for change in &now.changes {
        let _ = writeln!(said, "kit.lock: {change}");
    }
    Ok(said)
}

/// `fennec update --check`, in the kit around `path`: whether `kit.lock`'s holds are what the program
/// hands over, both ways, for CI. It reaches no network and writes nothing.
pub(crate) fn holds_up_to_date(path: &str) -> ExitCode {
    let root = match kit_around(path, "check") {
        Ok(root) => root,
        Err(code) => return code,
    };
    let lock = std::fs::read_to_string(root.join("kit.lock")).unwrap_or_default();
    match holds_now(&root, path, (&lock, "would now hold")) {
        Ok(now) if now.changes.is_empty() => {
            println!("kit.lock's holds are what the program hands over");
            ExitCode::SUCCESS
        }
        Ok(now) => {
            eprintln!("error: kit.lock's holds aren't what the program hands over:");
            for change in &now.changes {
                eprintln!("  {change}");
            }
            eprintln!(
                "  = help: `fennec update` records what each kit holds, to be reviewed with the change that made it"
            );
            ExitCode::FAILURE
        }
        Err(message) => {
            eprint!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// Adds the layout of each type the kit's program saves in `std.pack` to `pack.lock`, unless it's
/// there already. Nothing is ever taken out: data written with an old layout still needs it.
fn lock_layouts(root: &Path) -> Result<String, String> {
    let shown = root.to_string_lossy();
    let mut program = fennec_kit::load(root, &shown, fennec_kit::Entry::Main)?;
    let signatures =
        fennec_check::check_and_resolve(&mut program.module).map_err(|problems| program.render(&problems))?;
    let lowered = fennec_ir::lower(&program.module, &signatures);
    if lowered.layouts.is_empty() {
        return Ok(String::new());
    }
    let path = root.join("pack.lock");
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut added = 0;
    for saved in &lowered.layouts {
        let key = format!("{:016x} ", saved.shape.cast_unsigned());
        if text.lines().any(|line| line.starts_with(&key)) {
            continue;
        }
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        for part in [key.as_str(), saved.name.as_str(), " ", saved.layout.as_str(), "\n"] {
            text.push_str(part);
        }
        added += 1;
    }
    if added == 0 {
        return Ok(String::new());
    }
    std::fs::write(&path, text).map_err(|err| format!("couldn't write pack.lock: {err}"))?;
    let what = if added == 1 { "1 layout".to_owned() } else { format!("{added} layouts") };
    Ok(format!("locked {what} in pack.lock\n"))
}

/// `fennec fetch`, in the kit around `path`: each kit `kit.lock` pins that the store hasn't got, or
/// holds changed, is fetched, and checked against its pin.
pub(crate) fn fetch(path: &str) -> ExitCode {
    let root = match kit_around(path, "fetch") {
        Ok(root) => root,
        Err(code) => return code,
    };
    let Some(kits) = fennec_deps::Kits::of_this_machine() else {
        eprintln!("error: there's no home folder to keep fetched kits in; set FENNEC_HOME");
        return ExitCode::FAILURE;
    };
    match kits.ensure(&root) {
        Ok(found) => {
            let pinned = found.iter().filter(|kit| kit.pin.is_some()).count();
            println!("every kit kit.lock pins is on this machine, as pinned: {pinned}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The kit around `path`, reached only through folders that are trusted: see
/// `fennec_dist::kit_root`. With none, or one that isn't trusted, it's said, for `doing`.
fn kit_around(path: &str, doing: &str) -> Result<std::path::PathBuf, ExitCode> {
    match fennec_dist::kit_root(Path::new(path)) {
        Ok(Some(root)) => Ok(root),
        Ok(None) => {
            eprintln!(
                "error: there's no kit.toml in {path} or any folder above it, so there's nothing to {doing}"
            );
            Err(ExitCode::FAILURE)
        }
        Err(why) => {
            eprintln!("error: {why}");
            Err(ExitCode::FAILURE)
        }
    }
}
