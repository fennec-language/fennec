//! Fennec's first engine: a tree-walking interpreter that runs a parsed module directly.
//! It exists to run real programs while the typed bytecode interpreter and native
//! compilers are built.

#![cfg_attr(
    feature = "lean",
    expect(dead_code, unused_imports, reason = "what only the interpreter uses is left unused")
)]
mod access;
mod buffer;
mod builtins;
mod call;
mod collections;
mod compress;
mod constants;
pub mod embed;
mod entropy;
mod env;
mod equality;
mod eval;
mod exec;
mod fakes;
mod format;
mod formats;
mod generators;
mod inputs;
mod interp;
mod intrinsics;
mod limits;
mod matching;
mod methods;
mod ops;
pub mod packed;
mod pairs;
mod place;
mod prelude;
pub mod replay;
mod scan;
mod scope;
mod stdlib;
mod testing;
mod types;
mod unicode;
mod unicode_tables;
mod value;

pub use format::float as format_float;
pub use inputs::Inputs;
pub use interp::{Interp, run};
#[cfg(not(target_arch = "wasm32"))]
pub use stdlib::http::use_tls;
pub use testing::{PROPERTY_CASES, TestOutcome, property_seed};
