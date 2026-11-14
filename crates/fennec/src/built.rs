//! `fennec build` of a program none of whose files have changed since a build of it with the same
//! settings: the program that build stored is put where it goes, with nothing loaded, checked or
//! emitted, as `fennec run` starts one from its kept image. So building an unchanged kit costs
//! what reading its files' sizes and times does.
//!
//! What a build was made from is noted with the same kit state a run's image keeps, under a name
//! of its own: a kit's files, read before it's built, so an edit made while it builds is seen next
//! time; or a single file and everything it read, noted after it's loaded, each only if its text is
//! still what was parsed. A key over those files and every setting that decides what's built
//! names the program the build stored, its root.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use fennec_build::{Cache, KitFile};

const BUILD: &str = env!("FENNEC_BUILD");

/// Where a build's root is kept, by the key of what it was built from.
const BUILD_ROOTS: &str = "build-roots";

/// Where what a program's last build was made from is noted.
fn name_of(path: &str) -> String {
    format!("{}#build", crate::run::absolute(path))
}

/// What a build of the program at `path` makes from `files`, as a key:
/// - the path as given and made absolute, which name every file in a failure's message;
/// - each file by name and text, which a failure quotes;
/// - this build of Fennec, which covers its standard library and runtimes;
/// - the machine it builds for, and release or dev;
/// - the dev build's settings: the runtime, linking on its own, `--cpu` and every output switch.
///
/// With all the same, a build makes the same program.
fn key(path: &str, files: &[KitFile], release: bool) -> String {
    let mut hasher = fennec_blake3::Hasher::new_derive_key("fennec build of unchanged files 2026-11-14");
    hasher.update(format!("{path}\0{}\0", crate::run::absolute(path)).as_bytes());
    hasher.update(
        format!(
            "{BUILD}\n{}\n{}\n{}\nrelease={release}\n{}\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::ARCH,
            std::env::consts::OS,
            crate::aot::dev_build_settings()
        )
        .as_bytes(),
    );
    let mut each: Vec<(&str, String)> =
        files.iter().map(|file| (file.module.as_str(), file.text.to_hex())).collect();
    each.sort();
    for (module, text) in each {
        hasher.update(format!("{module}\0{text}\n").as_bytes());
    }
    hasher.finalize().to_hex()
}

/// A program's files as they are now, as its last build noted them.
fn files_now(path: &str, state: &fennec_build::KitState) -> Option<Vec<KitFile>> {
    if Path::new(path).is_dir() {
        crate::verdict::kit_files(path, Some(state))
    } else {
        crate::verdict::noted_files(state)
    }
}

/// `fennec build`: the program the store has for these files and settings, when it has one, or
/// else a build, noted for next time.
pub(crate) fn build(path: &str, output: Option<&str>, release: bool) -> ExitCode {
    let Some(cache) = crate::run::cache() else { return crate::aot::build(path, output, release) };
    let name = name_of(path);
    let state = cache.kit_state(&name, BUILD);
    if let Some(state) = &state
        && let Some(files) = files_now(path, state)
        && crate::verdict::same_files(state, &files)
        && stored(&cache, &key(path, &files, release), output)
    {
        return ExitCode::SUCCESS;
    }
    // A kit's files as they are before it's read.
    let before =
        if Path::new(path).is_dir() { crate::verdict::kit_files(path, state.as_ref()) } else { None };
    match crate::aot::build_kept(path, output, release) {
        Err(code) => code,
        Ok((program, built)) => {
            let files = before.or_else(|| {
                if Path::new(path).is_dir() { None } else { crate::verdict::loaded_files(&program) }
            });
            if let (Some(files), Some(root)) = (files, &built.root) {
                note(
                    &cache,
                    path,
                    (&files, &program),
                    release,
                    (root, built.total, &crate::aot::default_output(path, &program)),
                );
            }
            ExitCode::SUCCESS
        }
    }
}

/// Puts the program the store has under `key` where the build goes, saying so as a build does.
/// False when there's none, or it isn't whole any more.
fn stored(cache: &Cache, key: &str, output: Option<&str>) -> bool {
    let Some(kept) = cache.read(BUILD_ROOTS, key).and_then(|kept| String::from_utf8(kept).ok()) else {
        return false;
    };
    let mut lines = kept.lines();
    let (Some(root), Some(total), Some(named)) = (lines.next(), lines.next(), lines.next()) else {
        return false;
    };
    let Ok(total) = total.parse() else { return false };
    let output = output.map_or_else(|| PathBuf::from(named), PathBuf::from);
    if crate::aot::stored_executable(cache, root, &output).is_none() {
        return false;
    }
    let none = std::time::Duration::ZERO;
    crate::aot::said_built(
        &output,
        &crate::aot::Built { total, fresh: 0, compiled: none, linked: none, root: Some(root.to_owned()) },
    );
    true
}

/// Notes what a build was made from, and the root of what it made, with how many functions it
/// has and the name it's built to when no `-o` says.
fn note(
    cache: &Cache,
    path: &str,
    (files, program): (&[KitFile], &fennec_kit::Program),
    release: bool,
    (root, total, named): (&str, usize, &Path),
) {
    let files = crate::verdict::with_embedded(files, program);
    let kept = format!("{root}\n{total}\n{}\n", named.display());
    // A store that can't be written only means building it again next time.
    if fennec_build::kept(
        "keep which program a build made",
        cache.write(BUILD_ROOTS, &key(path, &files, release), kept.as_bytes()),
    )
    .is_some()
    {
        fennec_build::kept(
            "keep what a build was made from",
            cache.store_kit_state(&name_of(path), BUILD, &files),
        );
    }
}
