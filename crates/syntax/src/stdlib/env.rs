//! `std.env`: the `Env` capability, which reads the environment variables a program was started
//! with, and `Missing`, what reading a setting that has to be there fails with. `get`, `all` and a
//! test's `set` are written in Rust, and the rest in Fennec.

use super::Module;

pub(crate) const MODULE: Module =
    Module { name: "env", names: &["Env", "Missing"], source: include_str!("env.fen"), gives: &[] };
