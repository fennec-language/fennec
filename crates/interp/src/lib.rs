//! Fennec's first engine: a tree-walking interpreter that runs a parsed module directly.
//! It exists to run real programs while the typed bytecode interpreter and native
//! compilers are built.

mod access;
mod builtins;
mod call;
mod collections;
mod constants;
pub mod embed;
mod equality;
mod eval;
mod exec;
mod fakes;
mod format;
mod formats;
mod generators;
mod inputs;
mod interp;
mod matching;
mod methods;
mod ops;
pub mod packed;
mod pairs;
mod place;
mod prelude;
mod scan;
mod scope;
mod stdlib;
mod testing;
mod types;
mod value;

pub use format::float as format_float;
pub use inputs::Inputs;
pub use interp::{Interp, run};
pub use testing::TestOutcome;
