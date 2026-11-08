//! Fennec's first engine: a tree-walking interpreter that runs a parsed module directly.
//! It exists to run real programs while the typed bytecode interpreter and native
//! compilers are built.

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
mod stdlib;
mod testing;
mod unicode;
mod unicode_tables;
mod value;

pub use format::float as format_float;
pub use inputs::Inputs;
pub use stdlib::http::use_tls;
pub use testing::{PROPERTY_CASES, TestOutcome, property_seed};
