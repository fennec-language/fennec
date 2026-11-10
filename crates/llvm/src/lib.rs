//! The LLVM backend, behind `fennec build` and `fennec run`: the typed IR as LLVM's text format, one module
//! a function, each optimized and compiled by `opt` and `llc` and cached by what's in its text.
//! Editing a function recompiles it and what copied it to inline, and little else.
//!
//! So that a change elsewhere leaves a function's text alone, nothing that depends on the rest
//! of the program is written in it. Where a function starts in its file and the numbers strings
//! and kinds of list item have are read from symbols, all defined in one small module made fresh
//! each build. What a function's callers make alone before calling it, and what the functions
//! it calls leave alone, are written in, so editing one can recompile those it calls or is
//! called by.
//!
//! So far this takes ints, floats, bools, strings, structs and enums, plain and boxed, tuples,
//! lists of those, optionals of blocks, and f-strings, laid out as `fennec_layout` says, on the
//! runtime. It says which function it can't take rather than running it some
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
mod arrays;
mod build;
mod builders;
mod builtins;
mod bytes;
mod capabilities;
mod channels;
mod checks_left;
mod closures;
mod conversions;
mod dicts;
mod emit;
mod entry;
mod generators;
mod headers;
mod idmaps;
mod json;
mod kinds;
mod lists;
mod locks;
mod native;
mod notes;
mod parallel;
mod random;
mod regex;
mod rollback;
mod sized;
mod text;
mod ticks;
mod uniqueness;
mod waits;

use std::collections::{BTreeMap, BTreeSet};

use fennec_ir::{EnumDef, Program};
use fennec_layout::{ListItem, Written, generator_definition};
use fennec_rt::embed::dicts::Word;

pub use build::{Built, Tools, compile, compile_whole, sanitize_flag};
pub use checks_left::{CheckKind, CheckLeft, checks_left};
pub use emit::{
    Cpu, Unit, check_proofs, emit, emit_for, emit_traced, emit_tracing_costs, trust_native_blocks,
};
pub use entry::{Image, entry, tests_entry};
pub use fennec_layout::Layouts;
pub use parallel::{uses_launch, uses_parallel, uses_tls};
pub use waits::{Waits, Why, may_wait};

/// What a build does with a check the compiler proved can't fail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Proofs {
    /// Leaves it out.
    #[default]
    Trusted,
    /// Writes it anyway, as an assertion that the proof held: a build to find false proofs with,
    /// which `FENNEC_CHECK_PROOFS=1` asks for. One that didn't hold says "proof broken" and which.
    Checked,
}

/// Whether a build tells the runtime of each native block it makes and frees (see `native`). The
/// runtime must never be given one, and does nothing with a block it doesn't hold, so a mistake
/// there would otherwise leak or corrupt what it holds, silently; told, it stops the program
/// saying so instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NativeBlocks {
    /// Says nothing of them: a release build, which pays nothing for the watching.
    Trusted,
    /// Tells the runtime of each: a dev build, and one checking proofs.
    #[default]
    Watched,
}

/// What functions' text leaves to the module made fresh each build.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shared {
    /// Each function's symbol for where it starts in its file, and that place.
    pub starts: BTreeMap<String, u32>,
    /// Each string's symbol for its number, and its text.
    pub strings: BTreeMap<String, String>,
    /// Each literal's symbol for its block's address, and its text.
    pub literals: BTreeMap<String, String>,
    /// Each type that writes its own text: its layout name's literal, the function giving the
    /// text, and whether a value of it is a block rather than in place.
    pub own_texts: Vec<(String, String, bool)>,
    /// Each kind of list item's symbol for its number, and the kind.
    pub elements: BTreeMap<String, ListItem>,
    /// Each boxed type's symbol for its shape's number, and its layout's number.
    pub shapes: BTreeMap<String, u32>,
    /// The boxed types whose blocks generated code makes and frees itself, outside the runtime's
    /// table: see `native`.
    pub native: BTreeSet<u32>,
    /// The functions that may wait, as `waits` works them out, blocking reads counted: a
    /// `parallel` call whose body is one of them goes to the runtime's `_waiting` entry points.
    pub waiting: BTreeSet<String>,
    /// The closures the program makes that may wait: any of them could be a body that isn't
    /// known where it's run.
    pub waiting_closures: BTreeSet<String>,
    /// Each kind of dict's symbol for its shape's number, and what it holds.
    pub dicts: BTreeMap<String, (Word, Word)>,
    /// Whether each function sets a flag saying it ran, for a test to say what it ran. A function
    /// copied into its caller sets it too, since the flag comes along with its body.
    pub traced: bool,
    /// The functions that have a flag.
    pub ran: BTreeSet<String>,
    /// Whether code says where it is before anything there can allocate or copy, for a run to say
    /// what each place cost.
    pub tracing_costs: bool,
    /// What's at each place that says so, by where it starts in its file.
    pub sites: BTreeMap<u32, String>,
    /// Whether each check the code is written with is noted in `checks`, as it's written.
    pub noting_checks: bool,
    /// Whether checks left out because of a proof are, or are written anyway as assertions.
    pub proofs: Proofs,
    /// Whether native blocks are made known to the runtime, for it to stop if it's ever given one.
    pub native_blocks: NativeBlocks,
    /// The checks written, when they're noted: the same one more than once if its function is
    /// copied into callers.
    pub checks: Vec<CheckLeft>,
}

/// The layouts of everything a program defines, generic types' copies included, and of the
/// tuples, optionals and results it uses.
pub fn layouts_of(program: &Program) -> Layouts {
    // Which named types are blocks doesn't depend on how optionals are held, so a first layout
    // with every optional as an enum says which optionals can be one word.
    let first = laid_out(program, None);
    laid_out(program, Some(&first))
}

/// A `main` that gives back a result is run by one that doesn't, which fails the program if the
/// result is an error, as the interpreter does at `main`'s own place in the source.
pub fn wrap_main(program: &mut Program, span: fennec_syntax::Span) {
    let Some(main) = program.functions.iter_mut().find(|function| function.name == "main") else { return };
    if main.returns == fennec_check::Type::Unit {
        return;
    }
    "main.given_back".clone_into(&mut main.name);
    let (returns, params) = (main.returns.clone(), main.params);
    let mut locals: Vec<fennec_ir::Local> = main.locals.iter().take(params).cloned().collect();
    let result = fennec_ir::LocalId(params);
    locals.push(fennec_ir::Local { name: None, ty: returns, copied: false });
    let args = (0..params).map(|at| fennec_ir::Operand::Local(fennec_ir::LocalId(at))).collect();
    let call = fennec_ir::InstKind::Call { dest: Some(result), function: "main.given_back".to_owned(), args };
    let check = fennec_ir::InstKind::Builtin {
        dest: None,
        name: "main.result".to_owned(),
        args: vec![fennec_ir::Operand::Local(result)],
        changes: false,
    };
    program.functions.push(fennec_ir::Function {
        name: "main".to_owned(),
        params,
        mut_params: Vec::new(),
        returns: fennec_check::Type::Unit,
        locals,
        blocks: vec![fennec_ir::Block {
            insts: vec![fennec_ir::Inst { kind: call, span }, fennec_ir::Inst { kind: check, span }],
            end: fennec_ir::Terminator::Return(None),
        }],
        counts: Vec::new(),
        places: Vec::new(),
    });
}

fn laid_out(program: &Program, blocks: Option<&Layouts>) -> Layouts {
    let fields = |fields: &[(String, fennec_check::Type)]| {
        fields.iter().map(|(name, ty)| (name.clone(), kinds::canon_in(blocks, ty), None)).collect::<Vec<_>>()
    };
    // An error's notes are a word of its own: last in an error struct, and first in each case of an
    // error with cases, so it's at one place whatever the case. It's 0, as `None`, until one's added.
    let notes = || {
        let ty = fennec_check::Type::Optional(Box::new(fennec_check::Type::List(Box::new(
            fennec_check::Type::Str,
        ))));
        (fennec_rt::packed::NOTES.to_owned(), kinds::canon_in(blocks, &ty), None)
    };
    let enums = |def: &EnumDef| {
        let cases = def
            .cases
            .iter()
            .map(|case| {
                let mut written = fields(&case.fields);
                if def.error {
                    written.insert(0, notes());
                }
                (case.name.clone(), written)
            })
            .collect();
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
    // When the runtime holds closures, each closure's block is laid out like a boxed struct's.
    let closures = if closures::runtime_holds_closures(program) {
        closures::closure_layouts(program, blocks)
    } else {
        Vec::new()
    };
    // A generator's block is laid out like a boxed struct's, its header first.
    let generators = program.generators.iter().map(|def| {
        let slots: Vec<_> = def.slots.iter().map(|slot| kinds::canon_in(blocks, slot)).collect();
        (def.name.clone(), generator_definition(&kinds::canon_in(blocks, &def.item), None, &slots))
    });
    let errors = program.errors.iter().map(|def| {
        let mut written = fields(&def.fields);
        written.push(notes());
        (def.name.clone(), Written::Struct(written))
    });
    let definitions: Vec<(String, Written)> = program
        .structs
        .iter()
        .map(|def| {
            let written = if def.shared {
                Written::Cell(fields(&def.fields))
            } else {
                Written::Struct(fields(&def.fields))
            };
            (def.name.clone(), written)
        })
        .chain(errors)
        .chain(program.enums.iter().map(enums))
        .chain(named)
        .chain(generators)
        .chain(closures)
        .collect();
    Layouts::from_definitions(&definitions)
}

/// The module text for this machine starts with this, or says what's missing.
fn header() -> Result<&'static str, String> {
    const MAC_ARM: &str = "target datalayout = \"e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32\"\ntarget triple = \"arm64-apple-macosx14.0.0\"\n\n";
    const LINUX_ARM: &str = "target datalayout = \"e-m:e-p270:32:32-p271:32:32-p272:64:64-i8:8-i16:16-i64:64-i128:128-n32:64-S128-Fn32\"\ntarget triple = \"aarch64-unknown-linux-gnu\"\n\n";
    const LINUX_X86: &str = "target datalayout = \"e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128\"\ntarget triple = \"x86_64-unknown-linux-gnu\"\n\n";
    const WINDOWS_X86: &str = "target datalayout = \"e-m:w-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128\"\ntarget triple = \"x86_64-w64-windows-gnu\"\n\n";
    const MAC_X86: &str = "target datalayout = \"e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128\"\ntarget triple = \"x86_64-apple-macosx14.0.0\"\n\n";
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => Ok(MAC_ARM),
        ("aarch64", "linux") => Ok(LINUX_ARM),
        ("x86_64", "linux") => Ok(LINUX_X86),
        ("x86_64", "macos") => Ok(MAC_X86),
        ("x86_64", "windows") => Ok(WINDOWS_X86),
        (arch, os) => Err(format!("the LLVM backend doesn't know {arch} on {os} yet")),
    }
}
