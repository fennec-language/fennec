//! Fennec's build engine. This is its first layer: content hashes.
//!
//! Each file is hashed by its bytes with BLAKE3, and each definition by its syntax tree with
//! positions and comments left out. A run's hashes are kept as a snapshot, so the next run
//! knows what changed, and doesn't parse a file whose bytes are the same. Reformatting a
//! file, editing its comments or moving code around changes no definition's hash.

mod cache;
mod changes;
mod snapshot;
mod store;
mod tree;

pub use cache::{Cache, Cleanup, DEFAULT_CAP, VERSION};
pub use changes::{Change, Kind, changes};
pub use snapshot::{FileRecord, Run, Snapshot};
pub use store::{fennec_home, load, save};
pub use tree::{definitions, tree_hash};
