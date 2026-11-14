//! Starting an unchanged kit from what it was cut down to last time, rather than parsing all of
//! its files to find that out again. So how long `fennec run` takes to start depends on what
//! `main` reaches, not on the size of the kit.
//!
//! It's kept with the size and changed time of every file, which are all that's looked at. A
//! program from here has no source text, so a failure loads the source to point at it.
//!
//! A single file is kept the same way, with the files it read: itself, what it imports by path,
//! and what `embed` read.

use fennec_build::{Cache, KitFile, KitState};
use fennec_kit::Program;
use fennec_rt::embed::codec::{Decode, Encode, Reader, Writer};
use fennec_syntax::{Module, Sources};

const BUILD: &str = env!("FENNEC_BUILD");

/// Where a kit's starting point is kept.
fn name_of(path: &str) -> String {
    format!("{}#run", crate::run::absolute(path))
}

fn image_name(path: &str) -> String {
    format!("{}.image", fennec_blake3::hash(name_of(path).as_bytes()).to_hex())
}

/// A kit's program as it was cut down last time, if none of its files have changed since, and a
/// hash of it as it was kept, which says it's the same program another run had: see `kept_hash`.
pub(crate) fn load(path: &str, cache: &Cache) -> Option<(Program, String)> {
    let state = cache.kit_state(&name_of(path), BUILD)?;
    // A single file's are the files it read last time, which it reads again unless one changed.
    let files = if std::path::Path::new(path).is_dir() {
        crate::verdict::kit_files(path, Some(&state))?
    } else {
        crate::verdict::noted_files(&state)?
    };
    if !crate::verdict::same_files(&state, &files) {
        return None;
    }
    let bytes = cache.read("run-image", &image_name(path))?;
    let module = Module::decode(&mut Reader::new(&bytes))?;
    let kept = kept_hash(path, &files, &bytes);
    let program = Program {
        module,
        sources: Sources::new(),
        manifest: None,
        embedded: Vec::new(),
        kit_src: None,
        dependency_files: std::collections::HashSet::new(),
    };
    Some((program, kept))
}

/// What says a kept image is the program another run built, so that run's executable can be run
/// again: the image, and what the executable shows of where it came from, which the image doesn't
/// hold. That's the path the program was given as and where that is, which name every file in a
/// failure's message, and each file's text, which a failure quotes. The same program text from
/// another folder, or a comment changed to one as long, is then another program, built again,
/// rather than one that names the other folder or quotes the old comment.
fn kept_hash(path: &str, files: &[KitFile], bytes: &[u8]) -> String {
    let mut hasher = fennec_blake3::Hasher::new_derive_key("fennec kept image 2026-11-14");
    hasher.update(format!("{path}\0{}\0", crate::run::absolute(path)).as_bytes());
    let mut each: Vec<(&str, String)> =
        files.iter().map(|file| (file.module.as_str(), file.text.to_hex())).collect();
    each.sort();
    for (module, text) in each {
        hasher.update(format!("{module}\0{text}\n").as_bytes());
    }
    hasher.update(bytes);
    hasher.finalize().to_hex()
}

/// Keeps a kit's program as it was cut down, before anything was checked. `state` is what the kit
/// looked like before it was read, so an edit made since is seen next time.
pub(crate) fn store(path: &str, cache: &Cache, files: &[KitFile], program: &Program) {
    let mut writer = Writer::default();
    program.module.encode(&mut writer);
    // A cache that can't be written only means reading the kit again next time.
    let image = cache.write("run-image", &image_name(path), &writer.into_bytes());
    if fennec_build::kept("keep the checked program for the next run", image).is_some() {
        let state =
            cache.store_kit_state(&name_of(path), BUILD, &crate::verdict::with_embedded(files, program));
        fennec_build::kept("keep the kit's state", state);
    }
}

/// What a kit's files are now, with what was last noted to save reading them.
pub(crate) fn files_now(path: &str, cache: &Cache) -> Option<Vec<KitFile>> {
    let state: Option<KitState> = cache.kit_state(&name_of(path), BUILD);
    crate::verdict::kit_files(path, state.as_ref())
}
