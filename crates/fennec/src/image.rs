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

/// The image a kit's program was kept as last time, if none of its files have changed since: the
/// hash it was written with, which `program` reads it by, and a hash of it as it was kept, which
/// says it's the same program another run had (see `kept_hash`). Nothing of the image is read but
/// its hash, so a run whose program is stored already never reads or decodes it: on a big kit
/// that's 16 million instructions a run.
pub(crate) fn kept(path: &str, cache: &Cache) -> Option<(fennec_blake3::Hash, String)> {
    // The build daemon holds it for files the same as when it was found.
    if let Some(held) = crate::held::image(path) {
        return Some(held);
    }
    let state = cache.kit_state(&name_of(path), BUILD)?;
    let files = kept_files(path, &state)?;
    if !crate::verdict::same_files(&state, &files) {
        return None;
    }
    let image = cache.written_hash("run-image", &image_name(path))?;
    let kept = kept_hash(path, &files, image);
    crate::held::note_image(path, &files, (image, kept.clone()));
    Some((image, kept))
}

/// What the last run that kept the program at `path` noted of its files, which says which of them
/// to read again: those whose size or changed time isn't as noted.
pub(crate) fn noted(path: &str) -> Option<KitState> {
    crate::run::cache()?.kit_state(&name_of(path), BUILD)
}

/// The program a kit's image holds, as `kept` found it: None if it isn't the image `kept` named
/// any more, as when another run kept it again since, or it doesn't read back whole.
pub(crate) fn program(path: &str, cache: &Cache, image: fennec_blake3::Hash) -> Option<Program> {
    let (bytes, read) = cache.read_hashed("run-image", &image_name(path))?;
    if read != image {
        return None;
    }
    let module = Module::decode(&mut Reader::new(&bytes))?;
    let program = Program {
        module,
        sources: Sources::new(),
        manifest: None,
        embedded: Vec::new(),
        kit_src: None,
        dependency_files: std::collections::HashSet::new(),
    };
    Some(program)
}

/// The files a kept image is held to, by the state it was kept with: a kit's files, or a single
/// file's, which are the files it read last time and reads again unless one changed.
fn kept_files(path: &str, state: &KitState) -> Option<Vec<KitFile>> {
    if std::path::Path::new(path).is_dir() {
        crate::verdict::kit_files(path, Some(state))
    } else {
        crate::verdict::noted_files(state)
    }
}

/// What says a kept image is the program another run built, so that run's executable can be run
/// again: the image, and what the executable shows of where it came from, which the image doesn't
/// hold. That's the path the program was given as and where that is, which name every file in a
/// failure's message, and each file's text, which a failure quotes. The same program text from
/// another folder, or a comment changed to one as long, is then another program, built again,
/// rather than one that names the other folder or quotes the old comment.
///
/// The image goes in by its BLAKE3 hash, which reading it checked already, rather than hashed
/// again: on a big kit that's 6 million instructions a run.
fn kept_hash(path: &str, files: &[KitFile], image: fennec_blake3::Hash) -> String {
    let mut hasher = fennec_blake3::Hasher::new_derive_key("fennec kept image 2026-11-24");
    hasher.update(format!("{path}\0{}\0", crate::run::absolute(path)).as_bytes());
    let mut each: Vec<(&str, String)> =
        files.iter().map(|file| (file.module.as_str(), file.text.to_hex())).collect();
    each.sort();
    for (module, text) in each {
        hasher.update(format!("{module}\0{text}\n").as_bytes());
    }
    hasher.update(image.as_bytes());
    hasher.finalize().to_hex()
}

/// Keeps a kit's program as it was cut down, before anything was checked. `state` is what the kit
/// looked like before it was read, so an edit made since is seen next time. Gives back the hash
/// the next run's `load` will find it by, so the run keeping it can note what it builds from it
/// for that one: without it, the run after a change checked and built the program again, though
/// the run before had just built it.
pub(crate) fn store(path: &str, cache: &Cache, files: &[KitFile], program: &Program) -> Option<String> {
    let mut writer = Writer::default();
    program.module.encode(&mut writer);
    let bytes = writer.into_bytes();
    // A cache that can't be written only means reading the kit again next time.
    let image = cache.write("run-image", &image_name(path), &bytes);
    fennec_build::kept("keep the checked program for the next run", image)?;
    let noted = crate::verdict::with_embedded(files, program);
    fennec_build::kept("keep the kit's state", cache.store_kit_state(&name_of(path), BUILD, &noted))?;
    let state = cache.kit_state(&name_of(path), BUILD)?;
    let files = kept_files(path, &state)?;
    let image = fennec_blake3::hash(&bytes);
    let kept = kept_hash(path, &files, image);
    // The build daemon holds it for the next run of these same files, which then reads nothing.
    crate::held::note_image(path, &files, (image, kept.clone()));
    Some(kept)
}

/// What a kit's files are now, with what was last noted to save reading them.
pub(crate) fn files_now(path: &str, cache: &Cache) -> Option<Vec<KitFile>> {
    let state: Option<KitState> = cache.kit_state(&name_of(path), BUILD);
    crate::verdict::kit_files(path, state.as_ref())
}
