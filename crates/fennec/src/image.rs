//! Starting an unchanged kit from what it was cut down to last time, rather than parsing all of
//! its files to find that out again. So how long `fennec run` takes to start depends on what
//! `main` reaches, not on the size of the kit.
//!
//! It's kept with the size and changed time of every file, which are all that's looked at. A
//! program from here has no source text, so a failure loads the source to point at it.

use fennec_build::{Cache, KitFile, KitState};
use fennec_interp::embed::codec::{Decode, Encode, Reader, Writer};
use fennec_kit::Program;
use fennec_syntax::{Module, Sources};

const BUILD: &str = env!("FENNEC_BUILD");

/// Where a kit's starting point is kept.
fn name_of(path: &str) -> String {
    format!("{}#run", crate::run::absolute(path))
}

fn image_name(path: &str) -> String {
    format!("{}.image", fennec_blake3::hash(name_of(path).as_bytes()).to_hex())
}

/// A kit's program as it was cut down last time, if none of its files have changed since.
pub(crate) fn load(path: &str, cache: &Cache) -> Option<Program> {
    let state = cache.kit_state(&name_of(path), BUILD)?;
    let files = crate::verdict::kit_files(path, Some(&state))?;
    if !crate::verdict::same_files(&state, &files) {
        return None;
    }
    let bytes = cache.read("run-image", &image_name(path))?;
    let module = Module::decode(&mut Reader::new(&bytes))?;
    Some(Program { module, sources: Sources::new(), manifest: None })
}

/// Keeps a kit's program as it was cut down, before anything was checked. `state` is what the kit
/// looked like before it was read, so an edit made since is seen next time.
pub(crate) fn store(path: &str, cache: &Cache, files: &[KitFile], module: &Module) {
    let mut writer = Writer::default();
    module.encode(&mut writer);
    // A cache that can't be written only means reading the kit again next time.
    if cache.write("run-image", &image_name(path), &writer.into_bytes()).is_ok() {
        let _ = cache.store_kit_state(&name_of(path), BUILD, files);
    }
}

/// What a kit's files are now, with what was last noted to save reading them.
pub(crate) fn files_now(path: &str, cache: &Cache) -> Option<Vec<KitFile>> {
    let state: Option<KitState> = cache.kit_state(&name_of(path), BUILD);
    crate::verdict::kit_files(path, state.as_ref())
}
