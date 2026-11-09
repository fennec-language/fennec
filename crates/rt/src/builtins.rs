//! The few plain operations the runtime shares among its modules.

/// The most positions a leaf of a `parallel` reduction's tree holds, by the length alone, so
/// a reduction combines in the same order whatever its items' sizes (see `embed::parallel`).
pub(crate) const REDUCE_LEAF: usize = 1024;
