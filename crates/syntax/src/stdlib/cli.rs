//! `std.cli`: reading command-line arguments, written in Fennec.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "cli",
    names: &["Cli", "CliArgs", "CliError"],
    source: include_str!("cli.fen"),
    gives: &[],
};
