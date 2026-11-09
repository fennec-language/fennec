//! The parts of the standard library written in the runtime, one file per module. The rest of
//! each module is Fennec code in `fennec_syntax`.

pub(crate) mod compress;
pub(crate) mod crypto;
pub(crate) mod fs;
pub(crate) mod http;
pub(crate) mod math;
pub(crate) mod net;
pub(crate) mod process;
pub(crate) mod random;
pub(crate) mod re;
pub(crate) mod sqlite;
pub(crate) mod unicode;
