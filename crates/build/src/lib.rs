//! Fennec's build engine. This is its first layer: content hashes.
//!
//! Each file is hashed by its bytes with BLAKE3, and each definition by its syntax tree with
//! positions and comments left out. A run's hashes are kept as a snapshot, so the next run
//! knows what changed, and doesn't parse a file whose bytes are the same. Reformatting a
//! file, editing its comments or moving code around changes no definition's hash.

mod cache;
mod changes;
mod locals;
mod reach;
mod reached;
mod snapshot;
mod store;
mod traced;
mod tree;
mod verdict;

pub use cache::{CLEANUP_EVERY, Cache, Cleanup, DEFAULT_CAP, FOLDER, UNUSED_BUILD, VERSION};
pub use changes::{Change, Kind, changes};
pub use reach::test_keys;
pub use reached::{
    Defined, keep_what_functions_reach, keep_what_main_reaches, names_used, reached_by_main, reaches_into,
    what_main_reaches,
};
pub use snapshot::{FileRecord, Run, Snapshot};
pub use store::{WORK_KEPT, fennec_home, load, save, sweep_work, work_folder};
pub use traced::{TestGraph, TestRecord};
pub use tree::{definitions, tree_hash};
pub use verdict::{KitFile, KitState, Row, changed_bodies, item_span, rows};
