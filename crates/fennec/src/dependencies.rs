//! `fennec update`: pins a kit's dependencies from git in `kit.lock`. See `fennec-deps`.

use std::path::Path;
use std::process::ExitCode;

/// The store of fetched kits, which fetches nothing with `FENNEC_OFFLINE` set (as the launcher sets
/// it for `--offline`).
pub(crate) fn kits() -> Option<fennec_deps::Kits> {
    let home = fennec_build::fennec_home()?;
    Some(fennec_deps::Kits::new(&home, std::env::var_os("FENNEC_OFFLINE").is_some()))
}

/// `fennec update`, in the kit around `path`.
pub(crate) fn update(path: &str) -> ExitCode {
    let Some(root) = Path::new(path)
        .canonicalize()
        .ok()
        .and_then(|from| from.ancestors().find(|dir| dir.join("kit.toml").is_file()).map(Path::to_path_buf))
    else {
        eprintln!(
            "error: there's no kit.toml in {path} or any folder above it, so there's nothing to update"
        );
        return ExitCode::FAILURE;
    };
    let Some(kits) = kits() else {
        eprintln!("error: there's no home folder to keep fetched kits in; set FENNEC_HOME");
        return ExitCode::FAILURE;
    };
    match kits.update(&root) {
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
