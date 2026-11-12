//! `fennec update`: pins a kit's dependencies from git in `kit.lock`, see `fennec-deps`, and
//! adds the layouts of what the kit saves in `std.pack` to `pack.lock`. `fennec fetch`: fetches
//! what `kit.lock` pins and this machine hasn't got. They're the only commands that reach the
//! network: loading a program fetches nothing.

use std::path::Path;
use std::process::ExitCode;

/// `fennec update` or `fennec fetch`, as `which` names, in the kit around `path`.
pub(crate) fn named(which: &str, path: &str) -> ExitCode {
    if which == "fetch" { fetch(path) } else { update(path) }
}

/// `fennec update`, in the kit around `path`.
pub(crate) fn update(path: &str) -> ExitCode {
    // Reached only through folders that are trusted: see `fennec_dist::kit_root`.
    let found = match fennec_dist::kit_root(Path::new(path)) {
        Ok(found) => found,
        Err(why) => {
            eprintln!("error: {why}");
            return ExitCode::FAILURE;
        }
    };
    let Some(root) = found else {
        eprintln!(
            "error: there's no kit.toml in {path} or any folder above it, so there's nothing to update"
        );
        return ExitCode::FAILURE;
    };
    let Some(kits) = fennec_deps::Kits::of_this_machine() else {
        eprintln!("error: there's no home folder to keep fetched kits in; set FENNEC_HOME");
        return ExitCode::FAILURE;
    };
    match kits.update(&root) {
        Ok(said) => {
            print!("{said}");
            match lock_layouts(&root) {
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

/// Adds the layout of each type the kit's program saves in `std.pack` to `pack.lock`, unless it's
/// there already. Nothing is ever taken out: data written with an old layout still needs it.
fn lock_layouts(root: &Path) -> Result<String, String> {
    let shown = root.to_string_lossy();
    let mut program = fennec_kit::load(root, &shown, fennec_kit::Entry::Main)?;
    let signatures = fennec_check::check_and_resolve(&mut program.module)
        .map_err(|problems| program.sources.render_all(&problems))?;
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
    let Some(root) = Path::new(path)
        .canonicalize()
        .ok()
        .and_then(|from| from.ancestors().find(|dir| dir.join("kit.toml").is_file()).map(Path::to_path_buf))
    else {
        eprintln!("error: there's no kit.toml in {path} or any folder above it, so there's nothing to fetch");
        return ExitCode::FAILURE;
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
