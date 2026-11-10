//! The code of the runtime library Fennec's compiled programs link against: values, the heap,
//! capabilities and the parts of the standard library written here. `fennec-runtime` exports it.

mod buffer;
mod builtins;
mod collections;
mod compress;
pub mod embed;
mod entropy;
mod env;
mod fakes;
mod format;
mod inputs;
mod limits;
mod methods;
mod ops;
pub mod packed;
mod pairs;
pub mod replay;
mod scan;
mod signals;
mod stderr;
mod stdlib;
mod testing;
mod unicode;
mod unicode_tables;
mod value;

pub use format::float as format_float;
pub use inputs::Inputs;
pub use stdlib::http::use_tls;
pub use testing::{PROPERTY_CASES, TestOutcome, property_seed};
