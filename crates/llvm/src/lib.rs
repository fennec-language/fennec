//! The LLVM backend, behind `fennec build --llvm`: the typed IR as LLVM's text format, one module
//! a function, each optimized and compiled by `opt` and `llc` and cached by what's in its text.
//! Editing a function recompiles it and what copied it to inline, and nothing else.
//!
//! So that a change elsewhere leaves a function's text alone, nothing that depends on the rest
//! of the program is written in it. Where a function starts in its file and the numbers strings
//! and kinds of list item have are read from symbols, all defined in one small module made fresh
//! each build.
//!
//! So far this takes ints, floats, bools, strings, structs and enums, plain and boxed, tuples,
//! lists of those, optionals of blocks, and f-strings, laid out as `fennec_layout` says, on the
//! same runtime as the JIT. It says which function it can't take rather than running it some
//! other way.
//!
//! A function that can fail hands that back in a register kept for it, so that succeeding costs
//! no write: LLVM's `swifterror`. One that can't fail, knowing nothing it does can, doesn't.
//!
//! Owners are counted as the runtime's blocks ask. A local holding a list or string owns it,
//! except a parameter nothing writes, which the caller lends. A copy from a local nothing else
//! reads takes over its owner, and a list with other owners is copied before it changes. A
//! failure ends the program, so the way out on failing releases nothing.

mod analysis;
mod build;
mod emit;
mod entry;
mod kinds;
mod lists;
mod text;

use std::collections::BTreeMap;

use fennec_ir::{EnumDef, Program};
use fennec_layout::{Layouts, ListItem, Written};

pub use build::{Built, Tools, compile};
pub use emit::{Unit, emit};
pub use entry::{Image, entry};

/// What functions' text leaves to the module made fresh each build.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shared {
    /// Each function's symbol for where it starts in its file, and that place.
    pub starts: BTreeMap<String, u32>,
    /// Each string's symbol for its number, and its text.
    pub strings: BTreeMap<String, String>,
    /// Each literal's symbol for its block's address, and its text.
    pub literals: BTreeMap<String, String>,
    /// Each kind of list item's symbol for its number, and the kind.
    pub elements: BTreeMap<String, ListItem>,
    /// Each boxed type's symbol for its shape's number, and its layout's number.
    pub shapes: BTreeMap<String, u32>,
}

/// The layouts of everything a program defines, generic types' copies included.
pub fn layouts_of(program: &Program) -> Layouts {
    let fields = |fields: &[(String, fennec_check::Type)]| {
        fields.iter().map(|(name, ty)| (name.clone(), ty.clone(), None)).collect::<Vec<_>>()
    };
    let enums = |def: &EnumDef| {
        let cases = def.cases.iter().map(|case| (case.name.clone(), fields(&case.fields))).collect();
        (def.name.clone(), Written::Enum(cases))
    };
    let definitions: Vec<(String, Written)> = program
        .structs
        .iter()
        .map(|def| (def.name.clone(), Written::Struct(fields(&def.fields))))
        .chain(program.enums.iter().map(enums))
        .chain(program.errors.iter().map(|def| (def.name.clone(), Written::Struct(fields(&def.fields)))))
        .collect();
    Layouts::from_definitions(&definitions)
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
