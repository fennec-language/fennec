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

#![expect(
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::format_push_string,
    reason = "this writes text: its temporaries are named for what they hold, in long runs of lines"
)]

mod analysis;
mod build;
mod builtins;
mod closures;
mod dicts;
mod emit;
mod entry;
mod generators;
mod kinds;
mod lists;
mod regex;
mod text;

use std::collections::BTreeMap;

use fennec_interp::embed::dicts::Word;
use fennec_ir::{EnumDef, Program};
use fennec_layout::{Layouts, ListItem, Written, generator_definition};

pub use build::{Built, Tools, compile, compile_whole};
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
    /// Each kind of dict's symbol for its shape's number, and what it holds.
    pub dicts: BTreeMap<String, (Word, Word)>,
}

/// The layouts of everything a program defines, generic types' copies included, and of the
/// tuples, optionals and results it uses.
pub fn layouts_of(program: &Program) -> Layouts {
    // Which named types are blocks doesn't depend on how optionals are held, so a first layout
    // with every optional as an enum says which optionals can be one word.
    let first = laid_out(program, None);
    laid_out(program, Some(&first))
}

fn laid_out(program: &Program, blocks: Option<&Layouts>) -> Layouts {
    let fields = |fields: &[(String, fennec_check::Type)]| {
        fields.iter().map(|(name, ty)| (name.clone(), kinds::canon_in(blocks, ty), None)).collect::<Vec<_>>()
    };
    let enums = |def: &EnumDef| {
        let cases = def.cases.iter().map(|case| (case.name.clone(), fields(&case.fields))).collect();
        (def.name.clone(), Written::Enum(cases))
    };
    let mut named = BTreeMap::new();
    let structs = program.structs.iter().chain(&program.errors);
    for (_, ty) in structs.clone().flat_map(|def| &def.fields) {
        kinds::define_named(ty, blocks, &mut named);
    }
    for (_, ty) in program.enums.iter().flat_map(|def| &def.cases).flat_map(|case| &case.fields) {
        kinds::define_named(ty, blocks, &mut named);
    }
    for function in program.functions.iter().chain(&program.constants) {
        kinds::define_named(&function.returns, blocks, &mut named);
        function.locals.iter().for_each(|local| kinds::define_named(&local.ty, blocks, &mut named));
    }
    for def in &program.generators {
        kinds::define_named(&def.item, blocks, &mut named);
        def.slots.iter().for_each(|slot| kinds::define_named(slot, blocks, &mut named));
    }
    // A generator's block is laid out like a boxed struct's, its header first.
    let generators = program.generators.iter().map(|def| {
        let slots: Vec<_> = def.slots.iter().map(|slot| kinds::canon_in(blocks, slot)).collect();
        (def.name.clone(), generator_definition(&kinds::canon_in(blocks, &def.item), None, &slots))
    });
    let definitions: Vec<(String, Written)> = structs
        .map(|def| (def.name.clone(), Written::Struct(fields(&def.fields))))
        .chain(program.enums.iter().map(enums))
        .chain(named)
        .chain(generators)
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
