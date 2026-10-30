//! Fennec's first engine: a tree-walking interpreter that runs a parsed module directly.
//! It exists to run real programs while the typed bytecode interpreter and native
//! compilers are built.

mod access;
mod builtins;
mod call;
mod collections;
pub mod embed;
mod eval;
mod exec;
mod format;
mod interp;
mod matching;
mod methods;
mod ops;
mod place;
mod prelude;
mod random;
mod scan;
mod scope;
mod testing;
mod types;
mod value;

pub use format::float as format_float;
pub use interp::{Interp, run};
pub use testing::TestOutcome;
