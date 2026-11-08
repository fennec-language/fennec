//! The few plain operations the runtime shares among its modules.

/// The most positions a leaf of a `parallel` reduction's tree holds, by the length alone, so
/// the interpreter and native code combine in the same order (see `embed::parallel`).
pub(crate) const REDUCE_LEAF: usize = 1024;
