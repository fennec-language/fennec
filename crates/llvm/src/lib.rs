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
// A hash map's order changes from one run to the next, so it mustn't reach what's emitted or
// what's proven: each loop over one says why its order can't matter, or goes over a sorted copy.
#![deny(clippy::iter_over_hash_type)]

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
mod copies_made;
mod dicts;
mod dying;
mod emit;
mod entry;
mod facts;
mod failure_checks;
mod generators;
mod headers;
mod holding;
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
mod signs;
mod sized;
mod text;
mod ticks;
mod uniqueness;
mod waits;

use std::collections::{BTreeMap, BTreeSet};

use fennec_ir::{EnumDef, Program};
use fennec_layout::{ListItem, Written, generator_definition};
use fennec_rt::embed::dicts::Word;

pub use build::{
    Built, Tools, audited, compile, compile_whole, huge_functions, output_switches, sanitize_flag,
};
pub use checks_left::{CheckKind, CheckLeft, checks_left};
pub use copies_made::{CopyMade, Why as CopyWhy, changed_in_place, copies_made};
pub use emit::{
    Cpu, Unit, check_proofs, emit, emit_for, emit_traced, emit_tracing_costs, strip_facts,
    trust_native_blocks,
};
pub use entry::{Image, entry, tests_entry};
pub use failure_checks::unchecked_failures;
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

/// Whether a build hands LLVM the facts it knows beside the code (see `facts`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Facts {
    /// Hands them over, for LLVM to optimize with.
    #[default]
    Given,
    /// Leaves them out, which `FENNEC_STRIP_FACTS=1` asks for: a build that should run the same,
    /// only slower, to catch a wrong one.
    Stripped,
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
    /// Each function's symbol for its table of places in the source, and the places: where each
    /// thing it may say failed starts. A function's text names a place by its number here, so
    /// text added above the function, or anywhere in a file one of its places is in, leaves the
    /// function's text as it was.
    pub places: BTreeMap<String, Vec<u32>>,
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
    /// The functions that may fail, which take the failure flag, as the whole program's analysis
    /// works them out: the entry calls `main`, tests and own texts' functions accordingly.
    pub can_fail: BTreeSet<String>,
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
    /// Whether LLVM is handed the facts known beside the code.
    pub facts: Facts,
    /// Whether native blocks are made known to the runtime, for it to stop if it's ever given one.
    pub native_blocks: NativeBlocks,
    /// The checks written, when they're noted: the same one more than once if its function is
    /// copied into callers.
    pub checks: Vec<CheckLeft>,
}

/// The layouts of everything a program defines, generic types' copies included, and of the
/// tuples, optionals and results it uses.
///
/// # Errors
/// When the first layout and the finished one disagree on which types are blocks, or on how an
/// optional the program mentions is held: a bug in the compiler, which would have a field and a
/// local hold one value two ways.
pub fn layouts_of(program: &Program) -> Result<Layouts, String> {
    // Which named types are blocks doesn't depend on how optionals are held, so a first layout
    // with every optional as an enum says which optionals can be one word.
    let first = laid_out(program, None);
    let finished = laid_out(program, Some(&first));
    agreed(program, &first, &finished)?;
    Ok(finished)
}

/// Whether the finished layout keeps what the first decided: each type a block in one is a block in
/// the other, and each optional the program mentions, however deep, is held the same way by both.
fn agreed(program: &Program, first: &Layouts, finished: &Layouts) -> Result<(), String> {
    let ids = (0..first.len()).filter_map(|id| u32::try_from(id).ok());
    for layout in ids.filter_map(|id| first.get(id)) {
        let other = finished.id(&layout.name).and_then(|id| finished.get(id));
        if other.is_some_and(|other| other.boxed != layout.boxed) {
            return Err(format!(
                "the layouts disagree on whether `{}` is a block, which is a bug in the compiler",
                layout.name
            ));
        }
    }
    let mut mentioned: Vec<&fennec_check::Type> = Vec::new();
    for def in program.structs.iter().chain(&program.errors) {
        mentioned.extend(def.fields.iter().map(|(_, ty)| ty));
    }
    for case in program.enums.iter().flat_map(|def| &def.cases) {
        mentioned.extend(case.fields.iter().map(|(_, ty)| ty));
    }
    for function in program.functions.iter().chain(&program.constants) {
        mentioned.push(&function.returns);
        mentioned.extend(function.locals.iter().map(|local| &local.ty));
    }
    for def in &program.generators {
        mentioned.push(&def.item);
        mentioned.extend(&def.slots);
    }
    mentioned.into_iter().try_for_each(|ty| optionals_agree(ty, first, finished))
}

/// Whether every optional in a type is one word in both layouts or in neither.
fn optionals_agree(ty: &fennec_check::Type, first: &Layouts, finished: &Layouts) -> Result<(), String> {
    use fennec_check::Type;
    let each = |types: &mut dyn Iterator<Item = &Type>| -> Result<(), String> {
        for ty in types {
            optionals_agree(ty, first, finished)?;
        }
        Ok(())
    };
    match ty {
        Type::Optional(inner) => {
            if kinds::one_word_optional(first, inner) != kinds::one_word_optional(finished, inner) {
                return Err(format!(
                    "the layouts disagree on whether `{ty}` is one word, which is a bug in the compiler"
                ));
            }
            optionals_agree(inner, first, finished)
        }
        Type::List(inner)
        | Type::Buffer(inner)
        | Type::Set(inner)
        | Type::Iterator(inner)
        | Type::Id(inner)
        | Type::Table(inner)
        | Type::Array(inner, _) => optionals_agree(inner, first, finished),
        Type::Dict(a, b) | Type::Result(a, b) => each(&mut [a.as_ref(), b.as_ref()].into_iter()),
        Type::Tuple(items) | Type::Union(items) | Type::Generic(_, items) => each(&mut items.iter()),
        Type::Arrow(params, returns) => each(&mut params.iter().chain(std::iter::once(returns.as_ref()))),
        _ => Ok(()),
    }
}

/// A `main` that gives back a result is run by one that doesn't, which fails the program if the
/// result is an error, at `main`'s own place in the source.
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
    // A generator's block is laid out like a boxed struct's, its header first. One that can fail
    // keeps what it failed with as a block of its own, whose address is in the header's failure
    // slot: the slot is laid out as an iterator's is, a block whose own shape says what it holds,
    // so the generator lets go of it when it's freed. Laid out as a number, it never was.
    let failure = fennec_check::Type::Iterator(Box::new(fennec_check::Type::Int));
    let generators = program.generators.iter().map(|def| {
        let slots: Vec<_> = def.slots.iter().map(|slot| kinds::canon_in(blocks, slot)).collect();
        let item = kinds::canon_in(blocks, &def.item);
        (def.name.clone(), generator_definition(&item, def.fails.then_some(&failure), &slots))
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

/// The module text for this machine starts with this, or says what's missing. Each module is
/// named `fennec`: unnamed, LLVM names it by the path of the file it's read from, which holds the
/// build's folder, and an object the same source makes would differ from one folder to the next.
fn header() -> Result<&'static str, String> {
    const MAC_ARM: &str = "source_filename = \"fennec\"\ntarget datalayout = \"e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32\"\ntarget triple = \"arm64-apple-macosx14.0.0\"\n\n";
    const LINUX_ARM: &str = "source_filename = \"fennec\"\ntarget datalayout = \"e-m:e-p270:32:32-p271:32:32-p272:64:64-i8:8-i16:16-i64:64-i128:128-n32:64-S128-Fn32\"\ntarget triple = \"aarch64-unknown-linux-gnu\"\n\n";
    const LINUX_X86: &str = "source_filename = \"fennec\"\ntarget datalayout = \"e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128\"\ntarget triple = \"x86_64-unknown-linux-gnu\"\n\n";
    const WINDOWS_X86: &str = "source_filename = \"fennec\"\ntarget datalayout = \"e-m:w-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128\"\ntarget triple = \"x86_64-w64-windows-gnu\"\n\n";
    const MAC_X86: &str = "source_filename = \"fennec\"\ntarget datalayout = \"e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128\"\ntarget triple = \"x86_64-apple-macosx14.0.0\"\n\n";
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => Ok(MAC_ARM),
        ("aarch64", "linux") => Ok(LINUX_ARM),
        ("x86_64", "linux") => Ok(LINUX_X86),
        ("x86_64", "macos") => Ok(MAC_X86),
        ("x86_64", "windows") => Ok(WINDOWS_X86),
        (arch, os) => Err(format!("the LLVM backend doesn't know {arch} on {os} yet")),
    }
}

#[cfg(test)]
mod tests {
    use fennec_check::Type;
    use fennec_layout::{Layouts, Written};

    /// A type a block in one layout and in place in the other, as a layout that changed its mind
    /// would give: an optional of it would be one word in a field and two in a local, so the
    /// build stops, naming it.
    #[test]
    fn layouts_that_disagree_on_an_optional_stop_the_build() {
        let node = Type::Named("Node".to_owned());
        let field = |name: &str, ty: Type| (name.to_owned(), ty, None);
        let held_by_itself = vec![(
            "Node".to_owned(),
            Written::Struct(vec![field("value", Type::Int), field("next", node.clone())]),
        )];
        let plain = vec![("Node".to_owned(), Written::Struct(vec![field("value", Type::Int)]))];
        let (boxed, in_place) =
            (Layouts::from_definitions(&held_by_itself), Layouts::from_definitions(&plain));
        let optional = Type::List(Box::new(Type::Optional(Box::new(node))));
        let found = super::optionals_agree(&optional, &boxed, &in_place);
        assert_eq!(
            found,
            Err("the layouts disagree on whether `Node?` is one word, which is a bug in the compiler"
                .to_owned())
        );
        assert_eq!(super::optionals_agree(&optional, &boxed, &boxed), Ok(()));
    }

    /// An error and a struct holding a `str?`: the first layout held the `str?` as an enum, so it
    /// laid each out in place, and the finished one held it as one word and laid each out as a
    /// block. Both layouts now hold them the same way.
    #[test]
    fn an_error_and_a_struct_holding_an_optional_string_are_laid_out_once() -> Result<(), String> {
        let mut module = fennec_syntax::parse(OPTIONAL_TEXTS).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let program = fennec_ir::lower(&module, &checked);
        let first = super::laid_out(&program, None);
        let finished = super::laid_out(&program, Some(&first));
        for name in ["Inner", "Lost", "Holder"] {
            let boxed = |layouts: &Layouts| {
                layouts.id(name).and_then(|id| layouts.get(id)).map(|layout| layout.boxed)
            };
            assert_eq!(boxed(&first), boxed(&finished), "{name}");
        }
        super::agreed(&program, &first, &finished)
    }

    const OPTIONAL_TEXTS: &str = r#"error Lost:
    place: str
    hint: str?

struct Inner:
    label: str
    note: str?

struct Holder:
    maybe: Inner?
    why: Lost?
    count: int

def made(n: int) -> Holder:
    inner = Inner(label=f"a label long enough for a block {n}", note=f"a note long enough for a block {n}" if n % 2 == 0 else None)
    lost = Lost(place=f"a place long enough for a block {n}", hint=None if n % 2 == 0 else f"a hint long enough for a block {n}")
    return Holder(maybe=inner, why=lost, count=n)

def put(out: shared list[Holder], holder: Holder):
    lock out as ref held:
        held.append(holder)
    return

def main(stdout: Stdout):
    noise = [made(n + 10) for n in range(3)]
    holders = [made(n) for n in range(4)]
    first = holders[1].maybe else return
    stdout.print(first, first == made(1).maybe, holders[2].why, holders[2].why == made(2).why)
    shared out = list[Holder]()
    for at in [3, 2]:
        given = holders[at]
        with launch(() => put(out, given)):
            pass
    lock out as held:
        kept = held[0].maybe else return
        stdout.print(kept, kept.note, held[1].why, held[0] == holders[3], held[1] == holders[2], len(noise))
    return
"#;
}
