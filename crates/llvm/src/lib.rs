//! The LLVM backend, behind `fennec build --llvm`: the typed IR as LLVM's text format, one module
//! a function, each optimized and compiled by `opt` and `llc` and cached by what's in its text.
//! Editing a function recompiles it and what copied it to inline, and nothing else.
//!
//! So that a change elsewhere leaves a function's text alone, nothing that depends on the rest
//! of the program is written in it. Where a function starts in its file and which number a string
//! has are read from symbols, all defined in one small module made fresh each build.
//!
//! So far this takes functions that only hold ints, floats and bools, and says which it can't
//! take rather than running them some other way.
//!
//! A function that can fail hands that back in a register kept for it, so that succeeding costs
//! no write: LLVM's `swifterror`. One that can't fail, knowing nothing it does can, doesn't.

mod analysis;
mod build;
mod emit;
mod entry;

use std::collections::BTreeMap;

pub use build::{Built, Tools, compile};
pub use emit::{Unit, emit};
pub use entry::entry;

/// What the rest of the program's text depends on, for the module made fresh each build.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shared {
    /// Each function's symbol for where it starts in its file, and that place.
    pub starts: BTreeMap<String, u32>,
    /// Each string's symbol for its number, and its text.
    pub strings: BTreeMap<String, String>,
}

/// The module text for this machine starts with this, or says what's missing.
fn header() -> Result<&'static str, String> {
    const MAC_ARM: &str = "target datalayout = \"e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32\"\ntarget triple = \"arm64-apple-macosx14.0.0\"\n\n";
    const LINUX_ARM: &str = "target datalayout = \"e-m:e-p270:32:32-p271:32:32-p272:64:64-i8:8-i16:16-i64:64-i128:128-n32:64-S128-Fn32\"\ntarget triple = \"aarch64-unknown-linux-gnu\"\n\n";
    const LINUX_X86: &str = "target datalayout = \"e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128\"\ntarget triple = \"x86_64-unknown-linux-gnu\"\n\n";
    const MAC_X86: &str = "target datalayout = \"e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128\"\ntarget triple = \"x86_64-apple-macosx14.0.0\"\n\n";
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => Ok(MAC_ARM),
        ("aarch64", "linux") => Ok(LINUX_ARM),
        ("x86_64", "linux") => Ok(LINUX_X86),
        ("x86_64", "macos") => Ok(MAC_X86),
        (arch, os) => Err(format!("the LLVM backend doesn't know {arch} on {os} yet")),
    }
}
