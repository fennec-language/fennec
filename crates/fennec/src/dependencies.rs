//! `fennec update`: pins a kit's dependencies from git in `kit.lock`. See `fennec-deps`.

use std::path::Path;
use std::process::ExitCode;

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
    let Some(kits) = fennec_deps::Kits::of_this_machine() else {
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
