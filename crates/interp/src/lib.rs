//! Fennec's first engine: a tree-walking interpreter that runs a parsed module directly.
//! It exists to run real programs while the typed bytecode interpreter and native
//! compilers are built.

mod builtins;
mod call;
mod eval;
mod exec;
mod format;
mod interp;
mod methods;
mod ops;
mod scope;
mod value;

pub use interp::run;
