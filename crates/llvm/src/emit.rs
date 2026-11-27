//! Writing functions as LLVM's text. Each local is a stack slot and every read and write goes
//! through it, which LLVM's first pass turns into proper SSA.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;

use fennec_ir::{FormatPart, Function, GeneratorDef, InstKind, LocalId, Operand, Program, Terminator};
use fennec_layout::{Layouts, ListItem, Word};
use fennec_rt::embed::layout::COUNT;
use fennec_syntax::{BinOp, Span, UnaryOp};

use crate::analysis::{Analysis, callees, visit};
use crate::holding::{Holding, filled, holding, step};
use crate::index::{Record, Using};
use crate::keys::Key;
use crate::kinds::{Kind, struct_definitions};
use crate::reads::Reads;
use crate::reads::What;
use crate::share::{Added, Kept, Place, Share, place};
use crate::uniqueness::{Callees, FieldPath, Tracked, coming_in, unsettled, written_order};
use crate::{CheckKind, CheckLeft, Shared};

/// The level of processor code is built for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cpu {
    /// On x86-64, `x86-64-v3`: AVX2, FMA and BMI, the level the twins are built for. On 64-bit Arm, the baseline already has what's wanted.
    #[default]
    Usual,
    /// The first of its architecture, for older machines.
    Baseline,
}

/// Each function's attributes, with the processor it's for, which LLVM's choices go by. Being
/// in the text, it keeps code for one level from being cached as code for the other.
fn attributes(cpu: Cpu) -> String {
    let target = match (std::env::consts::ARCH, cpu) {
        ("x86_64", Cpu::Usual) => " \"target-cpu\"=\"x86-64-v3\"",
        ("x86_64", Cpu::Baseline) => " \"target-cpu\"=\"x86-64\"",
        _ => "",
    };
    format!(
        "attributes #0 = {{ nounwind \"frame-pointer\"=\"non-leaf\"{target} }}\nattributes #1 = {{ cold nounwind{target} }}\nattributes #2 = {{ alwaysinline nounwind \"frame-pointer\"=\"non-leaf\"{target} }}\n{METADATA}"
    )
}

const METADATA: &str = "!0 = !{!\"branch_weights\", i32 1, i32 100000}\n\
!1 = !{}\n\
!2 = !{!\"fennec\"}\n\
!3 = !{!\"header\", !2, i64 0}\n\
!4 = !{!3, !3, i64 0}\n\
!5 = !{!\"item\", !11, i64 0}\n\
!6 = !{!5, !5, i64 0}\n\
!7 = !{i64 0, i64 281474976710657}\n\
!8 = !{!\"branch_weights\", i32 100000, i32 1}\n\
!9 = !{!\"field\", !11, i64 0}\n\
!10 = !{!9, !9, i64 0}\n\
!11 = !{!\"values\", !2, i64 0}\n\
!12 = !{!11, !11, i64 0}\n\
!13 = !{!\"llvm.loop.interleave.count\", i32 1}\n";

/// The dict locals no other owner can ever hold: each is made fresh, by `{...}`, every time it's
/// set, and is only ever read by looking things up in it, asking its length, or indexing into it
/// or putting into it in place. Copying it to another local, passing it to a call, keeping it in
/// another value, capturing it, walking it or giving it back are all ways another owner could
/// come to hold it, so any of them leaves it out, as does being a parameter, whose caller may
/// hold it too.
fn held_by_itself(function: &Function) -> Vec<usize> {
    let insts: Vec<&InstKind> =
        function.blocks.iter().flat_map(|block| &block.insts).map(|inst| &inst.kind).collect();
    let ends_read = |local: usize| {
        function.blocks.iter().any(|block| match &block.end {
            Terminator::Return(Some(Operand::Local(read)))
            | Terminator::Branch { cond: Operand::Local(read), .. }
            | Terminator::NoMatch { value: Operand::Local(read), .. } => read.0 == local,
            _ => false,
        })
    };
    let is_dict = |at: usize| {
        at >= function.params
            && function.locals.get(at).is_some_and(|local| matches!(local.ty, fennec_check::Type::Dict(..)))
    };
    // How many times each local is read, and whether it's only ever made fresh.
    let mut read_count = vec![0_usize; function.locals.len()];
    let mut only_made = vec![true; function.locals.len()];
    for inst in &insts {
        visit(
            inst,
            |operand| {
                if let Operand::Local(local) = operand
                    && let Some(count) = read_count.get_mut(local.0)
                {
                    *count += 1;
                }
            },
            |local| {
                if !matches!(inst, InstKind::MakeDict { dest, .. } if *dest == local)
                    && let Some(made) = only_made.get_mut(local.0)
                {
                    *made = false;
                }
            },
        );
    }
    // A temporary made fresh and read once, by the copy that names it, is moved there, not shared.
    let fresh_temporary = |from: LocalId| {
        is_dict(from.0)
            && !ends_read(from.0)
            && only_made.get(from.0).copied().unwrap_or(false)
            && read_count.get(from.0).copied() == Some(1)
    };
    (function.params..function.locals.len())
        .filter(|at| is_dict(*at))
        .filter(|at| {
            let me = Operand::Local(LocalId(*at));
            let mine = |operand: &Operand| *operand == me;
            !ends_read(*at)
                && insts.iter().all(|inst| {
                    let (mut reads, mut writes) = (Vec::new(), Vec::new());
                    visit(inst, |operand| reads.push(operand.clone()), |local| writes.push(local));
                    let writes_me = writes.iter().any(|local| local.0 == *at);
                    let reads_me = reads.iter().any(&mine);
                    if !writes_me && !reads_me {
                        return true;
                    }
                    match inst {
                        InstKind::MakeDict { dest, pairs } => {
                            dest.0 == *at && !pairs.iter().any(|(key, value)| mine(key) || mine(value))
                        }
                        InstKind::Copy { dest, value: Operand::Local(from) } => {
                            dest.0 == *at && *from != LocalId(*at) && fresh_temporary(*from)
                        }
                        InstKind::Builtin { name, args, .. } => {
                            matches!(name.as_str(), "dict.get" | "dict.contains" | "dict.len")
                                && args.first().is_some_and(mine)
                                && !args.iter().skip(1).any(&mine)
                                && !writes_me
                        }
                        InstKind::GetIndex { object, index, dest } => {
                            mine(object) && !mine(index) && dest.0 != *at
                        }
                        InstKind::SetIndex { object, path, index, value } => {
                            object.0 == *at && path.is_empty() && !mine(index) && !mine(value)
                        }
                        _ => false,
                    }
                })
        })
        .collect()
}

/// The first number a loop's metadata takes, past the unit's own (`METADATA`).
const FIRST_LOOP: usize = 100;

/// The jumps that go back to the start of a loop they're in, from block to block: their target
/// is still being walked, depth first from the entry, when they're reached.
pub(crate) fn back_edges(function: &Function) -> HashSet<(usize, usize)> {
    let blocks = function.blocks.len();
    let (mut walking, mut done) = (vec![false; blocks], vec![false; blocks]);
    let mut found = HashSet::new();
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    if let Some(first) = walking.first_mut() {
        *first = true;
    }
    while let Some((at, next)) = stack.last().copied() {
        let targets = function.blocks.get(at).map(|block| block.end.targets()).unwrap_or_default();
        if let Some(target) = targets.get(next) {
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            if walking.get(target.0).copied().unwrap_or(false) {
                found.insert((at, target.0));
            } else if !done.get(target.0).copied().unwrap_or(true) {
                if let Some(mark) = walking.get_mut(target.0) {
                    *mark = true;
                }
                stack.push((target.0, 0));
            }
        } else {
            stack.pop();
            if let (Some(mark), Some(over)) = (walking.get_mut(at), done.get_mut(at)) {
                *mark = false;
                *over = true;
            }
        }
    }
    found
}

/// Whether block `at` starts a counted loop, a `for`: its branch compares one of the function's
/// counts with its limit, which bounds how many turns it takes.
///
/// Any other loop's way back says not to interleave it. A `while` loop whose turns nothing bounds
/// was interleaved four ways once its indexes needed no way back from the end, and `sieve`, whose
/// inner loops are mostly a few turns, ran 13% more cycles for it.
fn counted_header(function: &Function, at: usize) -> bool {
    let Some(block) = function.blocks.get(at) else { return false };
    function.counts.iter().any(|count| {
        block.insts.iter().any(|inst| match &inst.kind {
            InstKind::Binary { lhs, rhs, .. } => {
                let (counting, stopping) = (Operand::Local(count.count), Operand::Local(count.stop));
                (*lhs == counting && *rhs == stopping) || (*lhs == stopping && *rhs == counting)
            }
            _ => false,
        })
    })
}

/// Where a string block's bytes start.
pub(crate) const TEXT_START: u32 = fennec_rt::embed::layout::TEXT.unsigned_abs();

/// A list's header, and what's in it, never overlap its items, and a length is far from any limit.
pub(crate) const HEADER_ACCESS: &str = ", !tbaa !4";
pub(crate) const ITEM_ACCESS: &str = ", !tbaa !6";
/// A boxed struct's or enum's fields never overlap a block's header or a list's items.
pub(crate) const FIELD_ACCESS: &str = ", !tbaa !10";
/// A length is always written, so it's never undefined or poison.
pub(crate) const LENGTH_ACCESS: &str = ", !tbaa !4, !range !7, !noundef !1";
/// A struct's field wherever the struct is, which could be an item or a boxed field, but is
/// never a block's header. So changing one, like a parser's place in its text, doesn't make a
/// string's lengths read again.
pub(crate) const VALUE_ACCESS: &str = ", !tbaa !12";

/// Below this many instructions, a function that calls nothing is always inlined.
const ALWAYS_INLINE: usize = 16;
/// Below this many, a function is copied to its callers to be inlined.
const COPIED: usize = 64;
/// How many calls deep what's copied may call other small functions.
const COPY_DEPTH: usize = 2;
/// Below this many, a recursive function gets one level of itself inlined.
const SELF_INLINED: usize = 24;

const OVERFLOW: i64 = 1;
pub(crate) const DIVISION_BY_ZERO: i64 = 2;
const TOO_DEEP: i64 = 4;
const NEGATIVE_SQRT: i64 = 5;

/// One function's module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub function: String,
    /// Its module's text, or nothing for a unit a build used again without writing it.
    pub text: String,
    /// The hash of its text, which names its object.
    pub hash: fennec_blake3::Hash,
    /// Whether it's too big to optimize, so it's compiled as written (see `huge_functions`).
    pub huge: bool,
}

impl Unit {
    /// A unit just written, with its text.
    #[must_use]
    pub fn written(function: String, text: String) -> Unit {
        let hash = fennec_blake3::hash(text.as_bytes());
        let huge = crate::build::is_huge(&text);
        Unit { function, text, hash, huge }
    }
}

/// What a dev build gives emission to use the units it kept last time again, without writing
/// them (edit builds, stage 1), and what it gets back to keep for next time.
pub struct Reuse<'r> {
    /// The bytes of the index the last build of this program kept, if there's one.
    pub kept: Option<Vec<u8>>,
    /// Whether the object a unit's text names is in the cache. A unit whose object is gone is
    /// written again, so it can be compiled.
    pub has_object: &'r dyn Fn(&fennec_blake3::Hash) -> bool,
    /// Whether every unit used again is written anyway, into what's thrown away, and checked to
    /// come out the same: `FENNEC_CACHE_AUDIT=1`.
    pub audit: bool,
    /// The index to keep for the next build, once emission has made it.
    pub keep: Option<Vec<u8>>,
}

/// What can't be emitted yet says so, by function.
/// The layouts come back too: they number the blocks lists hold as they meet them, so the
/// program's image has to be made from the same ones.
pub fn emit(program: &Program) -> Result<(Vec<Unit>, Shared, Layouts), String> {
    emit_for(program, Cpu::Usual)
}

/// `emit`, for a level of processor.
pub fn emit_for(program: &Program, cpu: Cpu) -> Result<(Vec<Unit>, Shared, Layouts), String> {
    emit_with(program, cpu, Shared::default())
}

/// `emit_for`, with each function setting a flag that says it ran, for a test to say what it ran.
pub fn emit_traced(program: &Program, cpu: Cpu) -> Result<(Vec<Unit>, Shared, Layouts), String> {
    emit_with(program, cpu, Shared { traced: true, ..Shared::default() })
}

/// `emit_for`, with code saying where it is before anything there can allocate or copy, so a run
/// can say what each place cost.
pub fn emit_tracing_costs(program: &Program, cpu: Cpu) -> Result<(Vec<Unit>, Shared, Layouts), String> {
    emit_with(program, cpu, Shared { tracing_costs: true, ..Shared::default() })
}

/// Whether every build from here on checks proofs: see [`crate::Proofs::Checked`].
static PROOFS_CHECKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Whether builds leave out the facts they would hand LLVM, as `FENNEC_STRIP_FACTS=1` asks.
static FACTS_STRIPPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Makes every build from here on check proofs, or not: each check left out because of a proof is
/// written anyway, as an assertion that it held. `fennec` does when `FENNEC_CHECK_PROOFS=1`.
/// Whether builds from here on leave native blocks unwatched: see [`crate::NativeBlocks`].
static NATIVE_TRUSTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Makes builds from here on leave native blocks unwatched, as a release build does, or watch
/// them. One checking proofs watches them whatever this says.
pub fn trust_native_blocks(on: bool) {
    NATIVE_TRUSTED.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn check_proofs(on: bool) {
    PROOFS_CHECKED.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether builds leave out the facts they'd hand LLVM (see `facts`).
pub fn strip_facts(on: bool) {
    FACTS_STRIPPED.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether builds check proofs, as [`check_proofs`] last said.
pub(crate) fn proofs_checked() -> bool {
    PROOFS_CHECKED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether builds leave out the facts, as [`strip_facts`] last said.
pub(crate) fn facts_stripped() -> bool {
    FACTS_STRIPPED.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn emit_with(
    program: &Program,
    cpu: Cpu,
    shared: Shared,
) -> Result<(Vec<Unit>, Shared, Layouts), String> {
    // A dev build of `fennec` keeps what each unit read and shares, to check it's all it shares.
    let (units, shared, layouts, _) = emit_keeping(program, cpu, shared, (cfg!(debug_assertions), None))?;
    Ok((units, shared, layouts))
}

/// `emit_for`, using again each unit `reuse` kept last time whose function and every read it
/// made are as they were, and filling in what to keep for next time.
///
/// # Errors
/// What can't be emitted yet, as [`emit_for`] says it; and a unit kept last time that can't be
/// added again as it was kept, which is a bug.
pub fn emit_reusing(
    program: &Program,
    cpu: Cpu,
    reuse: &mut Reuse<'_>,
) -> Result<(Vec<Unit>, Shared, Layouts), String> {
    let (units, shared, layouts, _) =
        emit_keeping(program, cpu, Shared::default(), (cfg!(debug_assertions), Some(reuse)))?;
    Ok((units, shared, layouts))
}

/// What [`emit_with`] gives, and, when `keeping`, what each unit read of the whole program, what
/// it added to what units share and what it asked the layouts to number, checked to be all it
/// added (see `share::audit`).
pub(crate) fn emit_keeping(
    program: &Program,
    cpu: Cpu,
    mut shared: Shared,
    (checking, reuse): (bool, Option<&mut Reuse<'_>>),
) -> Result<(Vec<Unit>, Shared, Layouts, Vec<Kept>), String> {
    // A build that uses units again keeps what each one read and shares, for the next build.
    let keeping = checking || reuse.is_some();
    let header = crate::header()?;
    let attributes = attributes(cpu);
    if PROOFS_CHECKED.load(std::sync::atomic::Ordering::Relaxed) {
        shared.proofs = crate::Proofs::Checked;
    }
    if FACTS_STRIPPED.load(std::sync::atomic::Ordering::Relaxed) {
        shared.facts = crate::Facts::Stripped;
    }
    shared.native_blocks = if NATIVE_TRUSTED.load(std::sync::atomic::Ordering::Relaxed)
        && shared.proofs != crate::Proofs::Checked
    {
        crate::NativeBlocks::Trusted
    } else {
        crate::NativeBlocks::Watched
    };
    // Only a build uses and keeps analyses across builds: what notes the checks left looks at a
    // program beside one.
    let (everything, differ) = if shared.noting_checks {
        (crate::analysis::whole(program), Vec::new())
    } else {
        crate::analysis::whole_kept(program)
    };
    // An analysis used again from the build before that isn't what analyzing it again gives is
    // this backend's mistake: the build stops rather than use it (see `kept`).
    if !differ.is_empty() {
        return Err(format!(
            "the analysis kept from the build before of {} isn't what analyzing it again gives, which is a bug in the backend",
            differ.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ")
        ));
    }
    shared.can_fail = everything.can_fail.iter().map(|name| (*name).to_owned()).collect();
    let layouts = crate::layouts_of(program)?;
    let mut units = Vec::with_capacity(program.functions.len());
    // Every module of a program agrees, since what it declares has to match what's defined.
    let threads = crate::parallel::uses_parallel(program);
    let native = crate::native::native_types(program, &layouts);
    let waits = crate::waits::may_wait(program, true);
    let closures =
        program.functions.iter().flat_map(|function| &function.blocks).flat_map(|block| &block.insts);
    let waiting_closures: BTreeSet<String> = closures
        .filter_map(|inst| match &inst.kind {
            InstKind::MakeClosure { function, .. } if waits.marked.contains_key(function) => {
                Some(function.clone())
            }
            _ => None,
        })
        .collect();
    let waiting: BTreeSet<String> = waits.marked.into_keys().collect();
    // What units read of these is noted, so they're kept apart from what units write while
    // they're written, and put in `shared` after, for the entry.
    let reads = Reads::new(&everything, &native, (&waiting, !waiting_closures.is_empty()), keeping);
    let build = build_key(&shared, (header, &attributes), (threads, &reads), &layouts);
    // Each unit's IR key, worked out once, where units are used again: what their reuse checks
    // compare, and, together, which builds check the index they bring up to date, as an edit
    // changes them (see `index::compares_index`).
    let irs: Option<Vec<Key>> = reuse.is_some().then(|| {
        program
            .functions
            .iter()
            .chain(&program.constants)
            .map(|function| {
                everything
                    .ir_keys
                    .get(function.name.as_str())
                    .copied()
                    .unwrap_or_else(|| crate::keys::ir_key(function))
            })
            .collect()
    });
    let picked = irs
        .iter()
        .flatten()
        .fold(0_u64, |picked, ir| picked ^ u64::from_le_bytes(*ir.first_chunk::<8>().unwrap_or(&[0; 8])));
    let mut using = reuse.map(|reuse| Using::new(reuse, (build, picked), &reads, &layouts));
    let functions: HashMap<&str, &Function> = program
        .functions
        .iter()
        .chain(&program.constants)
        .map(|function| (function.name.as_str(), function))
        .collect();
    let anchor_of = |name: &str| functions.get(name).map(|function| crate::share::anchor(function));
    // Every function it can't take yet is named, not just the first.
    let mut refused = Vec::new();
    let mut kept = Vec::new();
    // Each written unit's IR key, by its place in `units`, for the index.
    let mut written = Vec::new();
    for (at, function) in program.functions.iter().chain(&program.constants).enumerate() {
        // A build asked to stop writes no more (`fennec_build::cancel`).
        if fennec_build::cancelled() {
            return Err(crate::STOPPED.to_owned());
        }
        let ir = irs.as_ref().and_then(|irs| irs.get(at)).copied();
        if let (Some(using), Some(ir)) = (&mut using, ir)
            && let Some(at) = using.usable(&function.name, &ir)
            && let Some(record) = using.record(at).cloned()
        {
            if using.audit {
                let kept_reads = using.reads_of(at);
                let unit_in = (&reads, threads, kept_reads.as_slice());
                audit_unit(function, &record, unit_in, &layouts, &shared, (header, &attributes))?;
            }
            // Asked again, then added into a `Shared` of its own, which goes into what units
            // share only once all of it is added. A unit kept with what can't be asked or added
            // again as it was is written instead, as if it hadn't been kept, rather than failing
            // this build and every one after it.
            let share = Share { added: Some(record.share.clone()) };
            let mut wrote = crate::share::settings_of(&shared);
            let added = layouts
                .ask_again(&record.asked)
                .and_then(|()| share.add_to(&mut wrote, &layouts, &anchor_of));
            if added.is_some() {
                crate::share::merge(&mut shared, &wrote);
                units.push(Unit {
                    function: function.name.clone(),
                    text: String::new(),
                    hash: fennec_blake3::Hash(record.text),
                    huge: record.huge,
                });
                if checking {
                    kept.push(Kept {
                        function: function.name.clone(),
                        reads: using.reads_of(at),
                        share,
                        asked: Some(record.asked.clone()),
                        wrote,
                    });
                }
                using.keep(at);
                continue;
            }
        }
        if keeping {
            layouts.note_asked();
            layouts.note_reads();
        }
        // A build that checks what units keep writes each into a `Shared` of its own, then adds
        // that to what units share, so the audit checks what each kept against exactly what it
        // wrote.
        let mut wrote = checking.then(|| crate::share::settings_of(&shared));
        let made = unit(
            function,
            (&reads, threads),
            &layouts,
            wrote.as_mut().unwrap_or(&mut shared),
            (header, &attributes),
        );
        let read = with_layouts_read(reads.take(), &layouts);
        let asked = layouts.asked();
        match made {
            Ok((text, share)) => {
                units.push(Unit::written(function.name.clone(), text));
                if keeping {
                    let reads = read;
                    if let (Some(ir), Some(_)) = (ir, &using) {
                        written.push((units.len() - 1, ir, reads.clone(), share.clone(), asked.clone()));
                    }
                    if let Some(wrote) = wrote {
                        crate::share::merge(&mut shared, &wrote);
                        kept.push(Kept { function: function.name.clone(), reads, share, asked, wrote });
                    }
                }
            }
            Err(why) => refused.push(format!("`{}`: {why}", function.name)),
        }
    }
    if !refused.is_empty() {
        return Err(refused.join("\n"));
    }
    if checking {
        crate::share::audit(program, &layouts, &kept)?;
    }
    for own in &program.own_texts {
        let (id, boxed) = match Kind::of(&layouts, &own.ty)? {
            Kind::Struct(id) | Kind::Enum(id) => (id, false),
            Kind::Boxed(id) => (id, true),
            other => return Err(format!("`{}` writes its own text, as a {other:?}", own.ty)),
        };
        let name = layouts.get(id).ok_or("no such layout")?.name.clone();
        let symbol = format!("l_{}", fennec_blake3::hash(name.as_bytes()).to_hex());
        shared.literals.insert(symbol.clone(), name);
        shared.own_texts.push((symbol, own.function.clone(), boxed));
    }
    if shared.facts == crate::Facts::Stripped {
        for unit in units.iter_mut().filter(|unit| !unit.text.is_empty()) {
            *unit = Unit::written(std::mem::take(&mut unit.function), crate::facts::stripped(&unit.text));
        }
    }
    if let Some(mut using) = using {
        // A written unit is kept with its text's hash as it's compiled, after facts are stripped.
        for (at, ir, unit_reads, share, asked) in written {
            let Some(unit) = units.get(at) else { continue };
            using.keep_written(unit, ir, &unit_reads, share, asked);
        }
        using.finish()?;
    }
    shared.native = native;
    shared.waiting_closures = waiting_closures;
    shared.waiting = waiting;
    Ok((units, shared, layouts, kept))
}

/// The settings every unit of a build is written with, and how its layouts lay everything out:
/// a kept unit is used again only by a build with the same. Everything here is the same for
/// every unit, so it's one key for the build rather than a read each unit notes. Each layout's
/// own fields and size aren't in it: a unit notes reading each layout it uses (`What::Layout`),
/// so a type's edit writes again only the units that read it.
fn build_key(
    shared: &Shared,
    (header, attributes): (&str, &str),
    (threads, reads): (bool, &Reads<'_, '_>),
    layouts: &Layouts,
) -> Key {
    let Shared { traced, tracing_costs, noting_checks, proofs, facts, native_blocks, .. } = shared;
    let settings = format!(
        "{header}\n{attributes}\n{}\n{traced} {tracing_costs} {noting_checks} {proofs:?} {facts:?} {native_blocks:?}\n{threads} {} {} {}\n",
        crate::output_switches(),
        reads.cells(),
        reads.runtime_closures(),
        reads.tested()
    );
    crate::keys::key_of("fennec unit build 2026-11-19", &(settings, layouts.whole_fingerprint()))
}

/// The reads a unit made of the whole program, by name, with each layout it read: what `Reads`
/// noted, then what the layouts noted since `note_reads`.
fn with_layouts_read(read: BTreeSet<crate::reads::Read<'_>>, layouts: &Layouts) -> Vec<(What, String)> {
    let mut reads: Vec<(What, String)> =
        read.into_iter().map(|(what, name)| (what, name.into_owned())).collect();
    reads.extend(layouts.layouts_read().unwrap_or_default().into_iter().map(|name| (What::Layout, name)));
    reads
}

/// Writes a unit a build is using again, into what's then thrown away, and checks it comes out as
/// it was kept: its text, what it shares, every kind it asks the layouts for, in order, and every
/// read it makes of the whole program. `FENNEC_CACHE_AUDIT=1` has every unit used again checked.
fn audit_unit<'p>(
    function: &'p Function,
    record: &Record,
    (reads, threads, kept_reads): (&Reads<'_, 'p>, bool, &[(What, String)]),
    layouts: &Layouts,
    shared: &Shared,
    (header, attributes): (&str, &str),
) -> Result<(), String> {
    layouts.ask_again(&record.asked).ok_or("asking again failed")?;
    let mut scratch = crate::share::settings_of(shared);
    layouts.note_asked();
    layouts.note_reads();
    let made = unit(function, (reads, threads), layouts, &mut scratch, (header, attributes));
    let read: BTreeSet<(What, String)> = with_layouts_read(reads.take(), layouts).into_iter().collect();
    let asked = layouts.asked();
    let (text, share) = made?;
    let text = if shared.facts == crate::Facts::Stripped { crate::facts::stripped(&text) } else { text };
    let name = &function.name;
    if *fennec_blake3::hash(text.as_bytes()).as_bytes() != record.text {
        return Err(format!("the audit wrote `{name}` again and its text isn't the one kept"));
    }
    if share.added.as_ref() != Some(&record.share) {
        return Err(format!("the audit wrote `{name}` again and what it shares isn't what was kept"));
    }
    // Every kind, in the order it was first asked for, whether something before had numbered it
    // or not: a kind the kept asking leaves out could be one it later can't find.
    if asked.as_ref() != Some(&record.asked) {
        return Err(format!(
            "the audit wrote `{name}` again and it asked for other kinds than it kept, or in another order"
        ));
    }
    if read != kept_reads.iter().cloned().collect::<BTreeSet<_>>() {
        return Err(format!("the audit wrote `{name}` again and it read other things than it kept"));
    }
    Ok(())
}

/// Whether a function only hands its work on: one block, a few instructions, and one call, to
/// another function. Inlined where it's called, it costs nothing, and what it calls is reached
/// with one call fewer, as `field(name, x)` reaches `x.serialize(ref self)`.
fn forwards(function: &Function) -> bool {
    let [block] = function.blocks.as_slice() else { return false };
    let calls = block.insts.iter().filter(|inst| matches!(inst.kind, InstKind::Call { .. })).count();
    block.insts.len() <= 4 && calls == 1 && callees(function).all(|name| name != function.name)
}

fn size(function: &Function) -> usize {
    function.blocks.iter().map(|block| block.insts.len() + 1).sum()
}

/// Whether a function is small enough to copy into its callers, with everything it calls, which
/// must be too, `depth` calls deep. A recursive one isn't.
fn copyable(reads: &Reads<'_, '_>, function: &Function, depth: usize) -> bool {
    size(function) <= COPIED
        && !reads.constant(function.name.as_str())
        && callees(function).all(|name| {
            depth > 0
                && name != function.name
                && reads.function(name).is_some_and(|callee| copyable(reads, callee, depth - 1))
        })
}

pub(crate) fn symbol(name: &str) -> String {
    format!("@\"f_{name}\"")
}

/// What an instruction does, in a few words, if it can allocate or copy. Reading what's already
/// there can't, and a call says for itself what's in the function it calls.
fn what_can_cost(inst: &InstKind) -> Option<String> {
    Some(match inst {
        InstKind::Builtin { name, .. } => format!("`{name}`"),
        InstKind::Binary { .. } => "an operator".to_owned(),
        InstKind::Make { .. } | InstKind::MakeCase { .. } | InstKind::MakeTuple { .. } => {
            "making a value".to_owned()
        }
        InstKind::MakeList { .. } | InstKind::ConstantList { .. } => "making a list".to_owned(),
        InstKind::MakeSet { .. } => "making a set".to_owned(),
        InstKind::MakeDict { .. } => "making a dict".to_owned(),
        InstKind::Format { .. } => "making a string".to_owned(),
        InstKind::Slice { .. } => "a slice".to_owned(),
        InstKind::SetField { .. } => "setting a field".to_owned(),
        InstKind::SetIndex { .. } => "setting an item".to_owned(),
        InstKind::GetIndex { .. } => "reading an item".to_owned(),
        _ => return None,
    })
}

/// How a function's written into a module.
#[derive(Clone, Copy)]
struct Definition<'d> {
    linkage: &'d str,
    name: &'d str,
    always_inline: bool,
    /// Who a call to itself goes to, when that isn't the function's own name.
    self_target: Option<&'d str>,
}

/// What one module holds so far.
pub(crate) struct Module<'w, 'p> {
    /// What the whole program's analysis worked out, every read of it noted.
    pub(crate) reads: &'w Reads<'w, 'p>,
    pub(crate) layouts: &'w Layouts,
    /// What units share, which a unit adds to only through methods that keep what it adds in
    /// `share`.
    shared: &'w mut Shared,
    share: Share,
    /// Where each function whose places this unit keeps starts (see `share::anchor`).
    anchors: HashMap<&'p str, Option<u32>>,
    /// Whether the program runs `parallel`'s ranges on other threads, which keep their own stack
    /// limit and failure flag.
    pub(crate) threads: bool,
    /// Whether it also has `shared` values, whose blocks that needs more from counting.
    pub(crate) cells: bool,
    declares: BTreeSet<String>,
    pub(crate) defined: HashSet<&'p str>,
    bodies: String,
    /// Functions a module only needs once, however many things use them.
    helpers: BTreeMap<String, String>,
    /// How many loops the unit has numbered for their metadata, from [`FIRST_LOOP`].
    loops: usize,
}

fn unit<'p>(
    function: &'p Function,
    (reads, threads): (&Reads<'_, 'p>, bool),
    layouts: &Layouts,
    shared: &mut Shared,
    (header, attributes): (&str, &str),
) -> Result<(String, Share), String> {
    let mut module = Module {
        reads,
        layouts,
        shared,
        share: Share::new(reads.noting()),
        anchors: HashMap::new(),
        threads,
        cells: threads && reads.cells(),
        declares: BTreeSet::new(),
        defined: HashSet::new(),
        bodies: String::new(),
        helpers: BTreeMap::new(),
        loops: 0,
    };
    module.defined.insert(&function.name);
    // Small functions come along, with the small functions they call, to be inlined and thrown
    // away.
    let made = |function: &'p Function| {
        function.blocks.iter().flat_map(|block| &block.insts).filter_map(|inst| match &inst.kind {
            InstKind::MakeClosure { function, .. } => Some(function.as_str()),
            _ => None,
        })
    };
    let mut copies: Vec<&Function> = Vec::new();
    let mut waiting: Vec<&str> = callees(function).chain(made(function)).collect();
    while let Some(name) = waiting.pop() {
        let Some(callee) = reads.function(name) else { continue };
        if name == function.name
            || copies.iter().any(|copy| copy.name == name)
            || !copyable(reads, callee, COPY_DEPTH)
        {
            continue;
        }
        copies.push(callee);
        waiting.extend(callees(callee).chain(made(callee)));
    }
    for copy in &copies {
        module.defined.insert(&copy.name);
    }
    let recursive = size(function) <= SELF_INLINED && callees(function).any(|name| name == function.name);
    let inlined = format!("@\"f_{}$inl\"", function.name);
    let small = (size(function) <= ALWAYS_INLINE && callees(function).next().is_none()) || forwards(function);
    module.define(
        function,
        &Definition {
            linkage: "",
            name: &symbol(&function.name),
            always_inline: small,
            self_target: recursive.then_some(inlined.as_str()),
        },
    )?;
    if recursive {
        let how = Definition { linkage: "internal ", name: &inlined, always_inline: true, self_target: None };
        module.define(function, &how)?;
    }
    let mut seen = HashSet::new();
    for copy in copies {
        if seen.insert(&copy.name) {
            let how = Definition {
                linkage: "available_externally ",
                name: &symbol(&copy.name),
                always_inline: true,
                self_target: None,
            };
            module.define(copy, &how)?;
        }
    }
    let mut out = String::from(header);
    for line in &module.declares {
        out += line;
    }
    out.push('\n');
    out += &module.bodies;
    out.extend(module.helpers.values().map(String::as_str));
    out += attributes;
    Ok((out, module.share))
}

impl<'p> Module<'_, 'p> {
    /// Whether checks left out because of a proof are, or are written anyway: one of the build's
    /// settings.
    pub(crate) fn proofs(&self) -> crate::Proofs {
        self.shared.proofs
    }

    /// Whether native blocks are made known to the runtime: one of the build's settings.
    pub(crate) fn native_blocks(&self) -> crate::NativeBlocks {
        self.shared.native_blocks
    }

    /// Whether code says where it is for a run to say what each place cost: one of the build's
    /// settings.
    pub(crate) fn tracing_costs(&self) -> bool {
        self.shared.tracing_costs
    }

    /// Whether each check written is noted: one of the build's settings.
    pub(crate) fn noting_checks(&self) -> bool {
        self.shared.noting_checks
    }

    /// Where `function` starts, for its places to be kept from.
    fn anchor(&mut self, function: &'p Function) -> Option<u32> {
        *self.anchors.entry(&function.name).or_insert_with(|| crate::share::anchor(function))
    }

    pub(crate) fn add_string(&mut self, symbol: &str, text: &str) {
        self.shared.strings.insert(symbol.to_owned(), text.to_owned());
        if self.share.keeping() {
            self.share.add(Some(Added::String(symbol.to_owned(), text.to_owned())));
        }
    }

    pub(crate) fn add_literal(&mut self, symbol: &str, text: &str) {
        self.shared.literals.insert(symbol.to_owned(), text.to_owned());
        if self.share.keeping() {
            self.share.add(Some(Added::Literal(symbol.to_owned(), text.to_owned())));
        }
    }

    pub(crate) fn add_element(&mut self, symbol: &str, item: ListItem) {
        self.shared.elements.insert(symbol.to_owned(), item);
        if self.share.keeping() {
            self.share.add(crate::share::element(self.layouts, symbol, item));
        }
    }

    pub(crate) fn add_shape(&mut self, symbol: &str, id: u32) {
        self.shared.shapes.insert(symbol.to_owned(), id);
        if self.share.keeping() {
            self.share.add(crate::share::shape(self.layouts, symbol, id));
        }
    }

    pub(crate) fn add_dict(&mut self, symbol: &str, words: (Word, Word)) {
        self.shared.dicts.insert(symbol.to_owned(), words);
        if self.share.keeping() {
            self.share.add(crate::share::dict(self.layouts, symbol, words));
        }
    }

    /// A function's table of places as it stands: each new place adds to it, so what's kept is
    /// the last, where the first was.
    pub(crate) fn add_places(&mut self, symbol: &str, function: &'p Function, sites: &[u32]) {
        self.shared.places.insert(symbol.to_owned(), sites.to_vec());
        if !self.share.keeping() {
            return;
        }
        let anchor = self.anchor(function);
        let kept: Vec<Place> = sites.iter().map(|at| place(*at, anchor)).collect();
        let table = Added::Places(symbol.to_owned(), function.name.clone(), kept);
        let earlier = self.share.added.as_mut().and_then(|added| {
            added.iter_mut().find(|added| matches!(added, Added::Places(known, ..) if known == symbol))
        });
        match earlier {
            Some(earlier) => *earlier = table,
            None => self.share.add(Some(table)),
        }
    }

    pub(crate) fn add_check(&mut self, function: &'p Function, span: Span, what: CheckKind) {
        if self.share.keeping() {
            let start = place(span.start, self.anchor(function));
            let length = span.end.saturating_sub(span.start);
            self.share.add(Some(Added::Check(function.name.clone(), start, length, what.clone())));
        }
        self.shared.checks.push(CheckLeft { function: function.name.clone(), span, what });
    }

    pub(crate) fn add_site(&mut self, at: u32, what: String) {
        if self.share.keeping() {
            self.share.add(Some(Added::Site(at, what.clone())));
        }
        self.shared.sites.entry(at).or_insert(what);
    }

    pub(crate) fn add_helper(&mut self, name: &str, text: String) {
        self.helpers.entry(name.to_owned()).or_insert(text);
    }

    pub(crate) fn function_named(&self, name: &str) -> Result<&'p Function, String> {
        self.reads.function(name).ok_or_else(|| format!("it uses `{name}`, which isn't lowered"))
    }

    /// Has a function declared if it isn't in this module.
    pub(crate) fn declare_call(&mut self, callee: &Function) -> Result<(), String> {
        if !self.defined.contains(callee.name.as_str()) {
            let (ret, params) = (self.ret_text(callee)?, self.params(callee, false)?);
            self.declare(&format!("declare swiftcc {ret} {}({params}) #0", symbol(&callee.name)));
        }
        Ok(())
    }

    /// A generator, by its name or the name of one of its functions.
    pub(crate) fn generator(&self, name: &str) -> Option<&'p GeneratorDef> {
        self.reads.generator(name)
    }

    pub(crate) fn flagged(&self, name: &str) -> bool {
        self.reads.can_fail(name)
    }

    pub(crate) fn declare(&mut self, line: &str) {
        self.declares.insert(format!("{line}\n"));
    }

    /// Has the types a kind needs defined.
    pub(crate) fn define_types(&mut self, kind: &Kind) {
        match kind {
            Kind::Struct(id) => {
                let mut lines = Vec::new();
                struct_definitions(self.layouts, *id, &mut lines);
                self.declares.extend(lines);
            }
            Kind::Enum(id) => {
                let mut lines = Vec::new();
                crate::kinds::case_definitions(self.layouts, *id, &mut lines);
                self.declares.extend(lines);
            }
            Kind::List(item) => {
                if let Some(kind) = Kind::from_ty(self.layouts, item.ty()) {
                    self.define_types(&kind);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn kind(&self, ty: &fennec_check::Type) -> Result<Kind, String> {
        Kind::of(self.layouts, ty)
    }

    /// A function's parameters, with their names and attributes or just their types.
    pub(crate) fn params(&self, function: &Function, named: bool) -> Result<String, String> {
        let mut params = Vec::with_capacity(function.params + 2);
        let returns = self.kind(&function.returns)?;
        // An address only this parameter has: told to LLVM, or checked on the way in by a build
        // that checks proofs (see `Emit::addresses_apart`).
        let alone = if self.proofs() == crate::Proofs::Checked { "" } else { "noalias " };
        if returns.in_memory() {
            params.push(if named { format!("ptr {alone}captures(none) %ret") } else { "ptr".to_owned() });
        }
        for (at, local) in function.locals.iter().take(function.params).enumerate() {
            let kind = self.kind(&local.ty)?;
            let by_address = function.mut_params.contains(&at);
            let ty = if by_address || kind.in_memory() { "ptr".to_owned() } else { kind.slot(self.layouts) };
            params.push(match (named, by_address) {
                (false, _) => ty,
                (true, true) => format!("ptr {alone}captures(none) %p{at}"),
                (true, false) => format!("{ty} %p{at}"),
            });
        }
        if self.flagged(&function.name) {
            params.push(if named { "ptr swifterror %err".to_owned() } else { "ptr swifterror".to_owned() });
        }
        Ok(params.join(", "))
    }

    pub(crate) fn ret_text(&self, function: &Function) -> Result<String, String> {
        let kind = self.kind(&function.returns)?;
        Ok(match kind {
            Kind::Unit | Kind::Struct(_) | Kind::Enum(_) => "void".to_owned(),
            other => other.slot(self.layouts),
        })
    }

    fn define(&mut self, function: &'p Function, how: &Definition<'_>) -> Result<(), String> {
        let Definition { linkage, name, always_inline, self_target } = *how;
        let ret = self.ret_text(function)?;
        let params = self.params(function, true)?;
        let attributes = if always_inline { 2 } else { 0 };
        let start_symbol = format!("p_{}", function.name);
        let start = function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .map(|inst| inst.span.start)
            .min()
            .unwrap_or(0);
        // Whether any of its code is written in the program's own files, whose places move when
        // text is added above them. The standard library's never do.
        let moves = function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|inst| inst.span != Span::default() && !inst.span.in_prelude());
        self.shared.starts.insert(start_symbol.clone(), start);
        if self.share.keeping() {
            let kept = place(start, self.anchor(function));
            self.share.add(Some(Added::Start(start_symbol.clone(), function.name.clone(), kept)));
        }
        self.declare(&format!("@\"{start_symbol}\" = external dso_local global i64"));
        let failed = if self.flagged(&function.name) {
            let leave = if ret == "void" { "ret void".to_owned() } else { format!("ret {ret} poison") };
            format!("store ptr inttoptr (i64 1 to ptr), ptr %err\n  {leave}")
        } else {
            "unreachable".to_owned()
        };
        let known = self.reads.analysis(function.name.as_str()).ok_or("no analysis")?;
        let mut kinds = Vec::with_capacity(function.locals.len());
        let joined = self.reads.builders(function.name.as_str());
        let builder = if self.layouts.text_headers() { Kind::TextBlock } else { Kind::Str };
        for (at, local) in function.locals.iter().enumerate() {
            // Pieces only ever joined are the string they're joined into, a block that grows.
            let kind = if joined.is_some_and(|locals| locals.contains(&at)) {
                builder.clone()
            } else {
                self.kind(&local.ty)?
            };
            self.define_types(&kind);
            kinds.push(kind);
        }
        let returns = self.kind(&function.returns)?;
        self.define_types(&returns);
        let mut emit = Emit {
            module: self,
            function,
            known,
            kinds,
            returns,
            allocas: String::new(),
            kept_lengths: HashMap::new(),
            zeros_counted: HashMap::new(),
            out: String::new(),
            cold: String::new(),
            next: 0,
            current: "entry".to_owned(),
            runs: Runs::AsItsFunction,
            failed,
            leaves: std::cell::Cell::new(false),
            needs_prop: false,
            free_owed: false,
            named: if moves { Places::Numbered } else { Places::FromStart(start) },
            start_symbol,
            sites: Vec::new(),
            made_for_body: Vec::new(),
            self_target,
            taken_at: BTreeMap::new(),
            found_entries: Vec::new(),
            searched: HashMap::new(),
            alone_slots: HashSet::new(),
            alone_by_block: HashMap::new(),
            alone_in_block: HashSet::new(),
            alone_before_loops: HashMap::new(),
            alone_dicts: HashSet::new(),
            alone_fields: Vec::new(),
            taken_items: HashMap::new(),
            place_homes: HashMap::new(),
            places_out: HashMap::new(),
            out_of_dicts: BTreeSet::new(),
            at: (usize::MAX, usize::MAX),
            check_kind: None,
            inside_items: Vec::new(),
            inside_slots: HashMap::new(),
            ranged_flags: HashMap::new(),
            ranged_made: HashMap::new(),
            copied_text: HashMap::new(),
            pair_slots: HashMap::new(),
            taken_places: HashMap::new(),
            walks: HashMap::new(),
            saving: crate::rollback::saving_handles(function),
            on_crash: crate::rollback::given_back_on_crash(function),
            saving_slots: HashMap::new(),
            rolled_back: HashSet::new(),
            lock_record: None,
            clean_blocks: HashMap::new(),
            locks_held: None,
            dropped_saves: None,
            wrote_prop: false,
            traced_calls: Vec::new(),
            leave_after_crash: None,
            holding: Holding::default(),
            holding_now: BTreeSet::new(),
            empties: None,
            dying: crate::dying::Dying::new(),
            back_edges: HashSet::new(),
            filling: Vec::new(),
            split: false,
        };
        // A function with locks a crash gives back, or that owns anything, leaves on every failure
        // through `prop`, where a crash puts back what the blocks it's inside changed, gives their
        // locks back and lets go of what the function owns, as a return does.
        let owns_any =
            (0..emit.kinds.len()).any(|at| emit.owns(LocalId(at)) || emit.owns_inline(LocalId(at)));
        // So does one that may be left while a value is out of a dict or a list, or a place is
        // out for a call, owning nothing or not: it goes back first, for whatever holds it to find
        // it whole.
        let left_out = emit.values_left_out();
        let out_for_calls = function
            .places
            .iter()
            .any(|place| emit.kind_of(place.value).is_ok_and(|kind| emit.holds_a_block(kind)));
        let leave_after_crash =
            ((!emit.on_crash.is_empty() || owns_any || !left_out.is_empty() || out_for_calls)
                && emit.failed != "unreachable")
                .then(|| std::mem::replace(&mut emit.failed, "br label %prop".to_owned()));
        emit.leave_after_crash = leave_after_crash;
        // A failure can let go of what's short-lived before `prop` when `prop` doesn't first put
        // back or roll back what's in a local's blocks. Locals whose blocks it does put things in,
        // and those it fills itself, are let go of at `prop` and at a return, as before.
        emit.split = emit.leave_after_crash.is_some() && emit.on_crash.is_empty();
        let mut untracked: BTreeSet<usize> = emit.known.length_kept.keys().copied().collect();
        for place in &function.places {
            untracked.extend([place.root.0, place.value.0]);
        }
        // What a value may be out of when a crash leaves is let go of only after it's back.
        untracked.extend(left_out);
        emit.dying = emit.last_reads(&untracked);
        emit.back_edges = back_edges(function);
        emit.holding = holding(&emit, &untracked, emit.split);
        emit.body()?;
        emit.write_traced_exit();
        let (allocas, out, cold, failed, needs_prop) =
            (emit.allocas, emit.out, emit.cold, emit.failed, emit.needs_prop && !emit.wrote_prop);
        // Leaving a function that says it can't fail would be undefined, so it's refused here.
        if failed == "unreachable" && (needs_prop || emit.leaves.get()) {
            return Err(format!(
                "`{}` fails where the backend worked out that it can't, which is a bug in it",
                function.name
            ));
        }
        let _ =
            writeln!(self.bodies, "define {linkage}swiftcc {ret} {name}({params}) #{attributes} {{\nentry:");
        self.bodies += &allocas;
        if self.shared.traced {
            let flag = format!("@\"ran_{}\"", function.name);
            self.declares.insert(format!("{flag} = external dso_local global i8"));
            let _ = writeln!(self.bodies, "  store i8 1, ptr {flag}");
            self.shared.ran.insert(function.name.clone());
            if self.share.keeping() {
                self.share.add(Some(Added::Ran(function.name.clone())));
            }
        }
        self.bodies += &out;
        self.bodies += &cold;
        if needs_prop {
            let _ = writeln!(self.bodies, "prop:\n  {failed}");
        }
        self.bodies += "}\n\n";
        Ok(())
    }
}

/// Where the code being written runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Runs {
    /// Where its function runs.
    AsItsFunction,
    /// On any thread.
    Anywhere,
}

/// How a function's text names the places in the source it may say failed.
#[derive(Clone, Copy)]
enum Places {
    /// By how far each is from where the function starts, for a function written wholly in the
    /// standard library. None of its places moves when the program's text does, so its text is
    /// what it always was.
    FromStart(u32),
    /// By its number in the function's table of places, for a function with any code in the
    /// program's own files. The table is written fresh each build, so text added above a place
    /// moves the table's entry and leaves the function's text alone.
    Numbered,
}

/// Where a block of a loop written twice is in the text: its part of `out`, then of `cold`.
type CopiedText = ((usize, usize), (usize, usize));

/// One function being written.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each says something of its own about the function being written"
)]
pub(crate) struct Emit<'e, 'w, 'p> {
    pub(crate) module: &'e mut Module<'w, 'p>,
    pub(crate) function: &'p Function,
    pub(crate) known: &'w Analysis,
    kinds: Vec<Kind>,
    returns: Kind,
    pub(crate) allocas: String,
    /// Where a list a loop fills keeps its length while the loop runs, by the list's local: in
    /// a local of its own, which LLVM holds in a register, written to the list as the loop ends.
    pub(crate) kept_lengths: HashMap<usize, String>,
    /// In a build that checks proofs, where each list made whole of zeros counts the appends it
    /// leaves out.
    pub(crate) zeros_counted: HashMap<usize, String>,
    out: String,
    cold: String,
    next: usize,
    /// The block being written.
    pub(crate) current: String,
    /// Where what's being written runs: a closure's drop may run on any thread, so it counts
    /// owners as code on the workers does.
    runs: Runs,
    failed: String,
    /// Whether a way out for a failure was written.
    leaves: std::cell::Cell<bool>,
    needs_prop: bool,
    /// Whether a free since the flag was last read may have run program code, which can fail
    /// (see [`Emit::owe_free_check`]).
    free_owed: bool,
    /// How this function's text names the places in the source it may say failed.
    named: Places,
    start_symbol: String,
    /// The places in the source this function's text names by number: where each starts.
    sites: Vec<u32>,
    /// Blocks made for the code being written now that no local holds, innermost last: each a
    /// block and whether it was made here. `on_blocks` notes the ones it makes for its body, and
    /// `max` and `min` their keys. A failure that leaves the function meanwhile lets go of them
    /// on its way out (`leave_if`); the code that noted them lets go of them otherwise.
    pub(crate) made_for_body: Vec<Vec<(String, String)>>,
    self_target: Option<&'e str>,
    /// Where each item taken out of a list to change was, to put it back without looking again.
    pub(crate) taken_at: BTreeMap<usize, String>,
    /// Where lookups since the last change found their keys: the dict's local, the key, and the
    /// entry or null.
    pub(crate) found_entries: Vec<(LocalId, Operand, String)>,
    /// The dict each of those lookups searched, by its entry: a put that reuses an entry compares
    /// it with the dict it's writing, and uses the entry only for that same block.
    pub(crate) searched: HashMap<String, String>,
    /// Slots of `mut` parameters made alone on the way in, that nothing here can share since.
    alone_slots: HashSet<String>,
    /// The slots made alone on the way into a loop the block being written is in (see
    /// `AloneInLoop`), by block, and those of the block being written.
    alone_by_block: HashMap<usize, HashSet<String>>,
    alone_in_block: HashSet<String>,
    /// The locals each block makes alone just before it goes into a loop.
    alone_before_loops: HashMap<usize, Vec<LocalId>>,
    /// Slots of dicts nothing else can hold, so a put into one needs no check that it's alone.
    pub(crate) alone_dicts: HashSet<String>,
    /// Field paths from locals whose blocks are known alone here.
    pub(crate) alone_fields: Vec<FieldPath>,
    /// Items taken out of a list to change: the list's local and field path, and the step to
    /// the item.
    pub(crate) taken_items: HashMap<usize, (LocalId, Vec<String>, String)>,
    /// Where a place taken out for a call goes back to: the address, what's held there, and how
    /// it's marked for LLVM.
    pub(crate) place_homes: HashMap<usize, (String, Kind, &'static str)>,
    /// For each place taken out to change, the stack slot saying where it goes back while it's out.
    pub(crate) places_out: HashMap<usize, String>,
    /// Those of them that are values out of a dict or a list, which go back after the others: a
    /// field out of such a value goes back into it first.
    pub(crate) out_of_dicts: BTreeSet<usize>,
    /// The instruction being written: its block, and its place in it.
    pub(crate) at: (usize, usize),
    /// The handles of the `lock ... as ref` blocks whose changes are saved, for a crash to put
    /// back, by local.
    pub(crate) saving: Vec<usize>,
    /// The handles a crash leaving this function gives back the locks of: those that save, and read locks.
    pub(crate) on_crash: Vec<usize>,
    /// For each of those handles, a flag that's set while its lock is held, for a crash to know
    /// which to put back.
    pub(crate) saving_slots: HashMap<usize, String>,
    /// The locks to write whose changes a failure has just put back, for their unlock to know.
    pub(crate) rolled_back: HashSet<usize>,
    /// The slot holding the address of this thread's record of plain saves, once a lock that saves
    /// its changes is taken, for its saves to go in without a call (see `rollback::save_plain`).
    pub(crate) lock_record: Option<String>,
    /// Which blocks reach a lock's end with nothing that can crash or fail on the way, worked out
    /// the first time a save asks (see `rollback::clean_blocks`), for each set of locks held.
    pub(crate) clean_blocks: HashMap<Vec<usize>, Vec<bool>>,
    /// The locks that save their changes held at each block's start (see `rollback::locks_held`).
    pub(crate) locks_held: Option<Vec<Vec<usize>>>,
    /// In a build checking proofs, the slot saying a save was left out since the last lock began,
    /// for a rollback to stop at: the proof that nothing after it could need it didn't hold.
    pub(crate) dropped_saves: Option<String>,
    /// Whether the way out on a failure, `prop`, is written already, with what a crash puts back.
    wrote_prop: bool,
    /// The calls a failure can leave by, as the block each is at the end of and where it is from
    /// the function's start: one way out says which, for the failure's call chain.
    traced_calls: Vec<(String, u32)>,
    /// How the function leaves on a failure once `prop` has put back what it changed and let go
    /// of what it owns, when every failure goes through `prop`.
    leave_after_crash: Option<String>,
    /// What the next way out on a failure is for, when what writes it says.
    check_kind: Option<(Span, Option<CheckKind>)>,
    /// Which locals may hold something where, worked out before writing.
    holding: Holding,
    /// The locals that may hold something as the instruction being written starts.
    holding_now: BTreeSet<usize>,
    /// In a build that checks proofs, the locals a function with many of them checks empty at
    /// its returns through one table (see [`Emit::check_all_empty`]), once it's written.
    empties: Option<EmptyTable>,
    /// The owned locals each instruction reads for the last time, let go of after it.
    dying: crate::dying::Dying,
    /// The jumps back to the start of a loop, from block to block (see [`back_edges`]).
    back_edges: HashSet<(usize, usize)>,
    /// What the instruction being written, and the one after, which it may be written with,
    /// may fill.
    filling: Vec<usize>,
    /// Whether a failure lets go of short-lived locals where it leaves, rather than at `prop`.
    split: bool,
    /// Items of lists, by field path and index, known to be inside their lists here.
    pub(crate) inside_items: Vec<FieldPath>,
    /// Where the position each was found at is kept, negative indexes counted from the end.
    pub(crate) inside_slots: HashMap<FieldPath, String>,
    /// For each check of item ranges (`range_scans`), the flag saying it held when last made
    /// (`check_item_ranges`): clear until it's made.
    pub(crate) ranged_flags: HashMap<usize, String>,
    /// For each check of item ranges, the flag saying it's been made in this call: neither list
    /// can change in the function, so what it found holds for the rest of the call.
    pub(crate) ranged_made: HashMap<usize, String>,
    /// For each loop written twice (`find_versioned_loops`), by its place in `versioned`, where the
    /// text of each of its blocks is in `out` and `cold`, for the second copy to be made from.
    copied_text: HashMap<usize, Vec<CopiedText>>,
    /// Where each pair from an `enumerate` never made points in its list, kept for its parts.
    pub(crate) pair_slots: HashMap<usize, String>,
    /// The slot holding where each item taken out of a `Table` to be put back is, by the local it
    /// was read into: its put-back goes there with no second lookup (see `id_map_store`).
    pub(crate) taken_places: HashMap<LocalId, String>,
    /// Where each map's walk has got to, and where the slot a walk is on and its generation are.
    pub(crate) walks: HashMap<usize, (String, String)>,
}

pub(crate) fn float_text(value: f64) -> String {
    format!("0x{:016X}", value.to_bits())
}

impl Emit<'_, '_, '_> {
    pub(crate) fn temp(&mut self) -> String {
        self.next += 1;
        format!("%t{}", self.next)
    }

    pub(crate) fn label(&mut self, word: &str) -> String {
        self.next += 1;
        format!("{word}{}", self.next)
    }

    pub(crate) fn line(&mut self, text: &str) {
        let _ = writeln!(self.out, "  {text}");
    }

    pub(crate) fn start_block(&mut self, label: &str) {
        let _ = writeln!(self.out, "{label}:");
        label.clone_into(&mut self.current);
    }

    /// What `write` writes, as text for a function of its own that may run on any thread,
    /// leaving the one being written as it was.
    pub(crate) fn written_apart(&mut self, write: impl FnOnce(&mut Self)) -> String {
        let (hot, current) = (std::mem::take(&mut self.out), self.current.clone());
        let runs = std::mem::replace(&mut self.runs, Runs::Anywhere);
        // What's written apart reads no flag of this function's.
        let owed = self.free_owed;
        write(self);
        self.free_owed = owed;
        self.runs = runs;
        self.current = current;
        std::mem::replace(&mut self.out, hot)
    }

    /// Adds a block away from the hot path.
    pub(crate) fn cold_block(&mut self, text: &str) {
        self.cold.push_str(text);
    }

    pub(crate) fn declare(&mut self, line: &str) {
        self.module.declare(line);
    }

    /// Whether a slot's block was made alone on the way in and can't have been shared since.
    pub(crate) fn alone_slots_contains(&self, place: &str) -> bool {
        self.alone_slots.contains(place) || self.alone_in_block.contains(place)
    }

    /// Whether a slot's block was made alone on the way into a loop this block is in, which
    /// gives it no other owner: a build that checks proofs checks it where it would have.
    pub(crate) fn alone_in_loop(&self, place: &str) -> bool {
        self.alone_in_block.contains(place)
    }

    /// Makes a local's list or `Table` alone on the way into a loop that changes it.
    fn alone_for_loop(&mut self, local: LocalId) -> Result<(), String> {
        let slot = self.slot(local);
        match self.kind_of(local)? {
            Kind::List(_) => {
                self.check_alone(local);
            }
            _ => {
                self.alone_at(&slot, "fennec_unshare");
            }
        }
        Ok(())
    }

    pub(crate) fn kind_of(&self, local: LocalId) -> Result<&Kind, String> {
        self.kinds.get(local.0).ok_or_else(|| "no such local".to_owned())
    }

    pub(crate) fn text_of(&self, kind: &Kind) -> String {
        kind.slot(self.module.layouts)
    }

    fn is_param(&self, local: LocalId) -> bool {
        local.0 < self.function.params
    }

    pub(crate) fn is_mut_param(&self, local: LocalId) -> bool {
        self.is_param(local) && self.function.mut_params.contains(&local.0)
    }

    /// A parameter the caller's slot or block is used for directly, since nothing writes it.
    fn is_lent(&self, local: LocalId) -> bool {
        self.is_param(local) && !self.is_mut_param(local) && !self.known.written(local)
    }

    /// Where a local is held.
    pub(crate) fn slot(&self, local: LocalId) -> String {
        let local = self.known.alias(local).unwrap_or(local);
        let in_memory = self.kinds.get(local.0).is_some_and(Kind::in_memory);
        if self.is_mut_param(local) || (self.is_lent(local) && in_memory) {
            format!("%p{}", local.0)
        } else {
            format!("%l{}", local.0)
        }
    }

    /// Whether this local's slot holds an owner, for it to release.
    pub(crate) fn owns(&self, local: LocalId) -> bool {
        self.kinds.get(local.0).is_some_and(Kind::counted)
            && !self.is_mut_param(local)
            && !self.is_lent(local)
            && self.known.alias(local).is_none()
            && !self.known.is_borrowed(local)
            && !self.known.is_taken(local)
    }

    fn body(&mut self) -> Result<(), String> {
        let function = self.function;
        // Where the function's code starts, for what's checked on the way in.
        let entry_span = function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .map(|inst| inst.span)
            .find(|span| *span != Span::default())
            .unwrap_or_default();
        if self.keeps_its_value() {
            self.give_back_what_it_kept();
        }
        // A test runs as a program's `main` does, so in a program that uses `parallel` it works out
        // the constants first too: a task it launches only reads them then. Worked out first in a
        // task instead, a constant's block stayed on that task's worker, where the test couldn't
        // find it.
        if (function.name == "main" || is_test(&function.name)) && self.module.threads {
            self.work_out_constants()?;
        }
        let kinds = self.kinds.clone();
        for (at, kind) in kinds.iter().enumerate() {
            let local = LocalId(at);
            let text = self.text_of(kind);
            if text == "void" || self.is_mut_param(local) || (self.is_lent(local) && kind.in_memory()) {
                continue;
            }
            let _ = writeln!(self.allocas, "  %l{at} = alloca {text}");
            if kind.counted() && text == "ptr" {
                let _ = writeln!(self.allocas, "  store ptr null, ptr %l{at}");
            } else if kind.counted() || self.holds_inline(kind) {
                let zero = self.zero_line(kind, &format!("%l{at}"));
                let _ = writeln!(self.allocas, "  {zero}");
            }
        }
        for (at, kind) in kinds.iter().take(function.params).enumerate() {
            let local = LocalId(at);
            if self.is_mut_param(local) || (self.is_lent(local) && kind.in_memory()) {
                continue;
            }
            let text = self.text_of(kind);
            if kind.in_memory() {
                let held = self.temp();
                self.line(&format!("{held} = load {text}, ptr %p{at}"));
                self.line(&format!("store {text} {held}, ptr %l{at}"));
                self.retain_inline(kind, &format!("%l{at}"));
            } else if text != "void" {
                if self.owns(local) {
                    // A string's header, a closure or an optional each counts its own way.
                    self.retain_value(&format!("%p{at}"), kind);
                }
                self.line(&format!("store {text} %p{at}, ptr %l{at}"));
            }
        }
        // Each list parameter whose length nothing here changes has its length read now and kept.
        let mut steady: Vec<usize> = self.known.steady_lengths.iter().copied().collect();
        steady.sort_unstable();
        for at in steady {
            let _ = writeln!(self.allocas, "  %sl{at} = alloca i64");
            self.keep_length(LocalId(at));
        }
        // Each check of item ranges (`find_ranged_reads`) has its flag, clear until it's made, and
        // a second saying whether it's been made: once a call is enough (`check_item_ranges`).
        for id in 0..self.known.range_scans.len() {
            let flag = self.flag_slot();
            self.ranged_flags.insert(id, flag);
            let made = self.flag_slot();
            self.ranged_made.insert(id, made);
        }
        // What every call gives a parameter, which what's proven from it leans on.
        if self.checks_proofs() && !self.known.param_ranges.is_empty() {
            let span = entry_span;
            for (at, (low, high)) in self.known.param_ranges.clone() {
                let (below, above, outside) = (self.temp(), self.temp(), self.temp());
                self.line(&format!("{below} = icmp slt i64 %p{at}, {low}"));
                self.line(&format!("{above} = icmp sgt i64 %p{at}, {high}"));
                self.line(&format!("{outside} = or i1 {below}, {above}"));
                self.proof_held(&outside, "a parameter is in the range every call gives it", span);
            }
        }
        if self.checks_proofs() {
            self.addresses_apart(entry_span)?;
        }
        // A dict nothing but its own lookups and puts ever holds (see `held_by_itself`).
        for local in held_by_itself(function) {
            let slot = self.slot(LocalId(local));
            self.alone_dicts.insert(slot);
        }
        // What callers see to before calling.
        let reads = self.module.reads;
        if let Some(needs) = reads.needs(function.name.as_str()) {
            self.alone_fields.clone_from(needs);
        }
        for (at, kind) in kinds.iter().take(function.params).enumerate() {
            let local = LocalId(at);
            if matches!(kind, Kind::List(_)) && self.known.alone_once_checked(local) {
                self.check_alone(local);
            }
        }
        // A `mut` parameter changed through more than once, which nothing here can share, is
        // made alone once, on the way in, rather than at every change.
        for at in 0..function.params {
            let local = LocalId(at);
            let changes = function
                .blocks
                .iter()
                .flat_map(|block| &block.insts)
                .filter(|inst| match &inst.kind {
                    InstKind::SetField { object, .. } => *object == local,
                    InstKind::SetIndex { object, path, .. } => *object == local && !path.is_empty(),
                    _ => false,
                })
                .count();
            if self.is_mut_param(local)
                && matches!(kinds.get(at), Some(Kind::Boxed(_)))
                && !kinds.get(at).is_some_and(|kind| self.is_cell(kind))
                && !self.known.is_shared(local)
                && !self.known.written(local)
                && changes > 1
            {
                let slot = self.slot(local);
                self.alone_along((local, &[]), &slot, "fennec_unshare");
                self.alone_slots.insert(slot);
            }
        }
        // Lists and `Table`s a loop changes and gives no other owner are made alone on the way
        // in, once, and not checked again inside it. A function that asks a lock's log keeps
        // every check: a marked block never reads as alone.
        if !self.asks_the_log() {
            for alone in &self.known.alone_in_loops {
                let slots: Vec<String> = alone.locals.iter().map(|local| self.slot(*local)).collect();
                for at in &alone.blocks {
                    self.alone_by_block.entry(*at).or_default().extend(slots.iter().cloned());
                }
                self.alone_before_loops.entry(alone.entry).or_default().extend(alone.locals.iter().copied());
            }
        }
        // Every caller passes the flag clear, so saying so lets a check after an inlined call
        // that didn't fail go away. An assumption, as a store would be inlined into loops.
        if self.module.flagged(&function.name) {
            self.line("%err_in = load ptr, ptr %err");
            self.line("%err_clear = icmp eq ptr %err_in, null");
            self.assumed("%err_clear", "every caller passes the failure flag clear", entry_span);
        }
        if self.ticks() {
            self.start_ticks();
            if self.module.reads.recurs(function.name.as_str()) {
                self.tick_call();
            }
        }
        self.line("br label %bb0");
        // Which blocks lead to each, so a lookup can carry on into a block every way in has
        // come through, once each of those is written.
        let mut before: HashMap<usize, Vec<usize>> = HashMap::new();
        for (at, block) in function.blocks.iter().enumerate() {
            for next in block.end.targets() {
                before.entry(next.0).or_default().push(at);
            }
        }
        let mut left_with: HashMap<usize, Vec<(LocalId, Operand, String)>> = HashMap::new();
        let mut alone_left: HashMap<usize, Vec<FieldPath>> = HashMap::new();
        let mut inside_left: HashMap<usize, Vec<FieldPath>> = HashMap::new();
        let on_entry = std::mem::take(&mut self.alone_fields);
        let (settled, lengthened) = (reads.settles(), reads.lengths());
        let settles: (&dyn Callees, Tracked) = (&settled, Tracked::Owners);
        let lengths: (&dyn Callees, Tracked) = (&lengthened, Tracked::Lengths);
        // Each block after every one leading to it, but for loops, so what's known carries on.
        let mut done = HashSet::new();
        for at in written_order(function) {
            let Some(block) = function.blocks.get(at) else { continue };
            let copied = self.known.versioned.iter().position(|loop_| loop_.blocks.contains(&at));
            let written_from = (self.out.len(), self.cold.len());
            self.start_block(&format!("bb{at}"));
            self.alone_in_block = self.alone_by_block.get(&at).cloned().unwrap_or_default();
            self.holding_now = self.holding.coming_in(at);
            self.filling.clear();
            self.write_kept_lengths(at);
            let ways_in = before.get(&at).map_or(&[][..], Vec::as_slice);
            self.alone_fields =
                coming_in(function, self.known, settles, (at, &before, &done), (&on_entry, &alone_left));
            self.inside_items =
                coming_in(function, self.known, lengths, (at, &before, &done), (&[], &inside_left));
            // A block a loop written twice goes to is reached from either copy, so nothing the
            // first copy found is known coming in (`write_second_copies`).
            let after_copies = copied.is_none()
                && ways_in
                    .iter()
                    .any(|from| self.known.versioned.iter().any(|loop_| loop_.blocks.contains(from)));
            self.found_entries = match ways_in.split_first() {
                Some((first, rest)) if !after_copies && ways_in.iter().all(|from| done.contains(from)) => {
                    let mut kept = left_with.get(first).cloned().unwrap_or_default();
                    for from in rest {
                        let theirs = left_with.get(from).map_or(&[][..], Vec::as_slice);
                        kept.retain(|entry| theirs.contains(entry));
                    }
                    kept
                }
                _ => Vec::new(),
            };
            let mut appended = false;
            for (index, inst) in block.insts.iter().enumerate() {
                self.at = (at, index);
                self.filling = filled(&self.holding, &inst.kind);
                if let Some(next) = block.insts.get(index + 1) {
                    self.filling.extend(filled(&self.holding, &next.kind));
                }
                self.forget_entries(&inst.kind);
                self.take_waiting_places(&inst.kind, inst.span)?;
                // A field that waited for this append is taken out now, with what's appended
                // already worked out.
                if let Some((field_local, item, field)) = self.known.path_due(self.at).cloned() {
                    self.get_field(field_local, &Operand::Local(item), &field)?;
                }
                if let InstKind::Call { function: callee, args, .. } = &inst.kind {
                    for (param, path) in reads.needs(callee.as_str()).map_or(&[][..], Vec::as_slice) {
                        if let Some(Operand::Local(given)) = args.get(param.0) {
                            self.see_to(*given, path)?;
                        }
                    }
                }
                // A string made only to go on the end of joined pieces goes there as it's made.
                let next = block.insts.get(index + 1).map(|next| &next.kind);
                if std::mem::take(&mut appended) {
                } else if let Some((joined, parts)) = self.appended_parts(&inst.kind, next) {
                    self.append_parts(joined, &parts, inst.span)?;
                    appended = true;
                } else if let Some((grown, made, parts)) = self.format_added(&inst.kind, next)? {
                    self.format_appended((grown, made), &parts, inst.span)?;
                    appended = true;
                } else if let Some((tested, op, wanted, items)) = self.literals_tested(&inst.kind, next) {
                    // A list or set written out of constants only to be looked in is never made.
                    self.in_literals(tested, op, &wanted, &items)?;
                    appended = true;
                } else {
                    self.inst(&inst.kind, inst.span, at)?;
                }
                if !self.alone_fields.is_empty() {
                    let hit = unsettled(function, self.known, &inst.kind, settles);
                    self.alone_fields.retain(|path| !hit.iter().any(|unsettled| unsettled.hits(path)));
                }
                if !self.inside_items.is_empty() {
                    let hit = unsettled(function, self.known, &inst.kind, lengths);
                    self.inside_items.retain(|path| !hit.iter().any(|unsettled| unsettled.hits(path)));
                }
                // A store may have grown the dict, moving every entry: any dict, through a path.
                if let InstKind::SetIndex { object, path, .. } = &inst.kind {
                    if path.is_empty() {
                        self.found_entries.retain(|(dict, _, _)| dict != object);
                    } else {
                        self.found_entries.clear();
                    }
                }
                self.let_go_after(&inst.kind);
                self.close_what_waits(&inst.kind);
                self.pay_free_check();
                let mut now = std::mem::take(&mut self.holding_now);
                step(self, &self.holding, (at, index), &inst.kind, &mut now);
                self.holding_now = now;
            }
            left_with.insert(at, std::mem::take(&mut self.found_entries));
            alone_left.insert(at, std::mem::take(&mut self.alone_fields));
            inside_left.insert(at, std::mem::take(&mut self.inside_items));
            done.insert(at);
            for reserved in self.reserved_at(at) {
                self.reserve(LocalId(reserved.list), &reserved.limit)?;
            }
            // Every loop has a jump back to its start ([`back_edges`]), which is where a task
            // counts a turn: once a turn. A block numbered before the one going to it is often
            // only further on in the same turn, as a `for` loop's step is after its body.
            if self.ticks() && block.end.targets().iter().any(|next| self.back_edges.contains(&(at, next.0)))
            {
                self.tick();
            }
            for local in self.alone_before_loops.get(&at).cloned().unwrap_or_default() {
                self.alone_for_loop(local)?;
            }
            self.check_item_ranges(at)?;
            self.terminator(&block.end)?;
            if let Some(loop_) = copied {
                let text = ((written_from.0, self.out.len()), (written_from.1, self.cold.len()));
                self.copied_text.entry(loop_).or_default().push(text);
            }
        }
        self.write_second_copies()?;
        // What follows runs on a crash from anywhere in the function, in no loop.
        self.alone_in_block.clear();
        // A crash inside a `lock ... as ref` block puts back what it changed before leaving, and a
        // crash anywhere lets go of what the function owns, so a server whose rounds crash keeps
        // nothing of theirs.
        if let Some(leave) = self.leave_after_crash.clone() {
            self.start_block("prop");
            self.put_places_back_after_crash()?;
            if !self.on_crash.is_empty() {
                self.roll_back_on_crash()?;
            }
            self.write_kept_lengths_after_crash();
            self.release_locals_at_prop();
            // Leaving as it failed already.
            self.free_owed = false;
            self.line(&leave);
            self.wrote_prop = true;
        }
        Ok(())
    }

    /// Whether a crash in this function lets go of what it owns, rather than leaving it.
    pub(crate) fn lets_go_after_crash(&self) -> bool {
        self.leave_after_crash.is_some()
    }

    /// A new slot on this function's stack.
    pub(crate) fn place_for(&mut self, kind: &Kind) -> String {
        let (at, text) = (self.label("%place"), self.text_of(kind));
        let _ = writeln!(self.allocas, "  {at} = alloca {text}");
        at
    }

    /// A new flag on this function's stack, clear when the function starts.
    pub(crate) fn flag_slot(&mut self) -> String {
        let at = self.label("%place");
        let _ = writeln!(self.allocas, "  {at} = alloca i1");
        let _ = writeln!(self.allocas, "  store i1 false, ptr {at}");
        at
    }

    /// A slot holding this block's address, for code that may replace it.
    pub(crate) fn temp_slot(&mut self, block: &str) -> String {
        let at = self.label("%place");
        let _ = writeln!(self.allocas, "  {at} = alloca ptr");
        self.line(&format!("store ptr {block}, ptr {at}"));
        at
    }

    pub(crate) fn is_optional(&self, kind: &Kind) -> bool {
        let (Kind::Enum(id) | Kind::Boxed(id)) = kind else { return false };
        self.module.layouts.get(*id).is_some_and(|layout| layout.name.starts_with("Option["))
    }

    /// What an optional held as an enum holds when it isn't `None`.
    pub(crate) fn some_kind(&self, kind: &Kind) -> Option<Kind> {
        if !self.is_optional(kind) {
            return None;
        }
        let (_, fields) = self.case_of(kind, "Some").ok()?;
        fields.into_iter().find(|(name, ..)| name == "value")?.2
    }

    /// One owner fewer for a value of this kind.
    pub(crate) fn release_kind(&mut self, value: &str, kind: &Kind) {
        if (*kind == Kind::Str && self.headers()) || *kind == Kind::MaybeText {
            self.release_text(value);
        } else if matches!(kind, Kind::Closure(..)) {
            self.release_closure(value);
        } else if self.lets_go_quietly(kind) {
            self.release_quietly(value);
        } else {
            self.release(value);
        }
    }

    /// Whether letting go of a value of this kind can't run program code: the program has no
    /// generator that closes something when it's let go partway, or the kind can't hold or reach
    /// one (see `dying::lets_go_quietly`). Such a value is freed with `fennec_free_plain`, which
    /// can't fail.
    pub(crate) fn lets_go_quietly(&self, kind: &Kind) -> bool {
        !self.module.reads.abandons() || crate::dying::lets_go_quietly(self.module.layouts, kind)
    }

    /// Notes that a free just written may have run program code, which can fail: the function
    /// reads the flag once what it's writing is whole (see [`Self::pay_free_check`]). A function
    /// that can't fail has nothing to read it with, and the analysis gives the flag to every one
    /// that may let go of such a value (`analysis::lets_go_loudly`).
    pub(crate) fn owe_free_check(&mut self) {
        if self.module.flagged(self.function.name.as_str()) {
            self.free_owed = true;
        }
    }

    /// Reads the failure flag, and leaves on a failure, when a free since it last did may have run
    /// what closes a generator let go partway, which can fail: so a launched task whose free fails
    /// stops at that line, as `main`'s thread does, and runs nothing after it. Written once an
    /// instruction is whole, where `prop` finds each local as it lets go of it.
    fn pay_free_check(&mut self) {
        if !std::mem::take(&mut self.free_owed) {
            return;
        }
        let failed = self.flag_set();
        self.leave_if(&failed);
    }

    /// Whether the failure flag is set, read.
    fn flag_set(&mut self) -> String {
        let flag_at = self.failure_flag();
        let (flag, failed) = (self.temp(), self.temp());
        self.line(&format!("{flag} = load i8, ptr {flag_at}"));
        self.line(&format!("{failed} = icmp ne i8 {flag}, 0"));
        failed
    }

    /// After an instruction that changes a list or a dict whose items may be generators that close
    /// something when they're let go partway: the runtime's change only queues those it lets go
    /// of, so they're closed here, at the line that let go of them, and a failure leaves from it.
    fn close_what_waits(&mut self, inst: &InstKind) {
        let local = match inst {
            InstKind::Builtin { changes: true, args, .. } => match args.first() {
                Some(Operand::Local(local)) => *local,
                _ => return,
            },
            InstKind::SetIndex { object, .. } => *object,
            _ => return,
        };
        let Some(kind) = self.kinds.get(local.0).cloned() else { return };
        if kind == Kind::Gen
            || self.lets_go_quietly(&kind)
            || !self.module.flagged(self.function.name.as_str())
        {
            return;
        }
        self.declare("declare void @fennec_close_waiting() nounwind");
        self.line("call void @fennec_close_waiting()");
        self.free_owed = true;
    }

    /// [`Self::pay_free_check`] at a return, once its locals are let go of: a failure leaves from
    /// here rather than by `prop`, which would let go of them again, and lets go of what was to be
    /// given back, which no caller takes.
    fn pay_free_check_at_return(&mut self, given: Option<(&str, &Kind)>) {
        if !std::mem::take(&mut self.free_owed) {
            return;
        }
        let failed = self.flag_set();
        let (leaving, on) = (self.label("free_failed"), self.label("giving_back"));
        self.line(&format!("br i1 {failed}, label %{leaving}, label %{on}, !prof !0"));
        self.start_block(&leaving);
        match given {
            Some((value, kind)) if kind.in_memory() => self.release_inline(kind, value),
            Some((value, kind)) if kind.counted() => self.release_kind(value, kind),
            _ => {}
        }
        // Already leaving as it failed.
        self.free_owed = false;
        self.leaves.set(true);
        let leave = self.leave_after_crash.clone().unwrap_or_else(|| self.failed.clone());
        for line in leave.lines() {
            self.line(line.trim());
        }
        self.start_block(&on);
    }

    /// One more owner for a value, unless it's the one that's nothing: an optional's may be.
    pub(crate) fn retain_value(&mut self, value: &str, kind: &Kind) {
        if value == "null" {
            return;
        }
        if (*kind == Kind::Str && self.headers()) || *kind == Kind::MaybeText {
            self.retain_text(value);
            return;
        }
        if matches!(kind, Kind::Closure(..)) {
            self.retain_closure(value);
            return;
        }
        if !matches!(kind, Kind::Maybe(_)) {
            self.retain(value);
            return;
        }
        let (none, held, done) = (self.temp(), self.label("some"), self.label("retained"));
        self.line(&format!("{none} = icmp eq ptr {value}, null"));
        self.line(&format!("br i1 {none}, label %{done}, label %{held}"));
        self.start_block(&held);
        self.retain(value);
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
    }

    /// A one-word optional as the enum a list's items hold it as, with an owner of its own.
    fn optional_as_enum(&mut self, value: &str, held: &Kind, kind: &Kind) -> Result<(String, bool), String> {
        let (some, fields) = self.case_of(kind, "Some")?;
        let (none, _) = self.case_of(kind, "None")?;
        let (_, offset, _) = fields.first().cloned().ok_or("an optional with nothing in it")?;
        self.retain_value(value, held);
        // A local holds an optional string as a header, all zero for `None`.
        let missing = if *held == Kind::Str {
            "false".to_owned()
        } else if *held == Kind::MaybeText {
            self.header_is_none(value)
        } else {
            let missing = self.temp();
            self.line(&format!("{missing} = icmp eq ptr {value}, null"));
            missing
        };
        let tag = self.temp();
        self.line(&format!("{tag} = select i1 {missing}, i64 {none}, i64 {some}"));
        let (place, fields_at) = if let Kind::Boxed(id) = kind {
            let block = self.new_block(*id)?;
            let fields_at = self.offset(&block, crate::lists::FIELDS_OFFSET);
            (block, fields_at)
        } else {
            let place = self.place_for(kind);
            let zero = self.zero_line(kind, &place);
            self.line(&zero);
            (place.clone(), place)
        };
        self.line(&format!("store i64 {tag}, ptr {fields_at}"));
        let at = self.offset(&fields_at, offset);
        let memory = held.memory(self.module.layouts);
        self.line(&format!("store {memory} {value}, ptr {at}"));
        Ok((place, true))
    }

    /// A value to go where this kind goes: `None` needs telling what it's `None` of. The second
    /// half says the value is new, and already has an owner for its new home.
    pub(crate) fn value_for(&mut self, operand: &Operand, kind: &Kind) -> Result<(String, bool), String> {
        if *operand != Operand::None {
            let (value, held) = self.operand(operand)?;
            if matches!(held, Kind::Maybe(_) | Kind::MaybeText | Kind::Str) && self.is_optional(kind) {
                return self.optional_as_enum(&value, &held, kind);
            }
            if self.widens(&held, kind) {
                return Ok((self.widened(&value, &held, kind)?, true));
            }
            return Ok((value, false));
        }
        match kind {
            Kind::Maybe(_) => Ok(("null".to_owned(), false)),
            Kind::MaybeText => Ok(("zeroinitializer".to_owned(), false)),
            Kind::Enum(_) => {
                // Word 0 is which case, and `None` comes first.
                let (at, text) = (self.label("%none"), self.text_of(kind));
                let _ = writeln!(
                    self.allocas,
                    "  {at} = alloca {text}\n  store {text} zeroinitializer, ptr {at}"
                );
                Ok((at, false))
            }
            Kind::Boxed(id) => Ok((self.new_block(*id)?, true)),
            other => Err(format!("it uses `None` as a {other:?}, which this backend doesn't take yet")),
        }
    }

    /// Whether this function can run on one of `parallel`'s threads, so a block it's handed may
    /// be another thread's.
    pub(crate) fn on_workers(&self) -> bool {
        self.module.threads
            && (self.runs == Runs::Anywhere || self.module.reads.on_workers(self.function.name.as_str()))
    }

    /// Writes `write` for a block that's made, leaving a null one be.
    fn when_made(&mut self, block: &str, write: impl FnOnce(&mut Self)) {
        let (none, made, after) = (self.temp(), self.label("made"), self.label("made_done"));
        self.line(&format!("{none} = icmp eq ptr {block}, null"));
        self.line(&format!("br i1 {none}, label %{after}, label %{made}"));
        self.start_block(&made);
        write(self);
        self.line(&format!("br label %{after}"));
        self.start_block(&after);
    }

    /// One more owner for a block.
    pub(crate) fn retain(&mut self, block: &str) {
        if self.on_workers() {
            self.retain_helper();
            self.line(&format!("call void @fen_retain(ptr {block})"));
            return;
        }
        let (count, more) = (self.temp(), self.temp());
        self.line(&format!("{count} = load i64, ptr {block}{HEADER_ACCESS}"));
        self.line(&format!("{more} = add i64 {count}, 1"));
        self.line(&format!("store i64 {more}, ptr {block}{HEADER_ACCESS}"));
    }

    /// Whether a block has one owner, so changing it in place changes nobody else's. Every
    /// such question native code asks of a block comes here.
    pub(crate) fn alone(&mut self, block: &str) -> String {
        if self.on_workers() {
            self.alone_helper();
            let alone = self.temp();
            self.line(&format!("{alone} = call i1 @fen_alone(ptr {block})"));
            return alone;
        }
        let (count, alone) = (self.temp(), self.temp());
        self.line(&format!("{count} = load i64, ptr {block}{HEADER_ACCESS}"));
        self.line(&format!("{alone} = icmp eq i64 {count}, 1"));
        alone
    }

    /// Whether a block has one owner and a lock's log marked it, for the lock to change it in place
    /// (see `lists::MARKED` in the runtime).
    pub(crate) fn marked_alone(&mut self, block: &str) -> String {
        let marked_one = fennec_rt::embed::lists::MARKED | 1;
        if self.on_workers() {
            self.marked_alone_helper(marked_one);
            let alone = self.temp();
            self.line(&format!("{alone} = call i1 @fen_marked_alone(ptr {block})"));
            return alone;
        }
        let (count, alone) = (self.temp(), self.temp());
        self.line(&format!("{count} = load i64, ptr {block}{HEADER_ACCESS}"));
        self.line(&format!("{alone} = icmp eq i64 {count}, {marked_one}"));
        alone
    }

    /// Takes one owner off a block that has one, giving back whether that was the last. With
    /// [`Self::retain`] and [`Self::alone`], everything native code does to a block's count.
    pub(crate) fn owner_gone(&mut self, block: &str) -> String {
        if self.on_workers() {
            self.owner_gone_helper();
            let gone = self.temp();
            self.line(&format!("{gone} = call i1 @fen_owner_gone(ptr {block})"));
            return gone;
        }
        let (count, fewer, gone) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{count} = load i64, ptr {block}{HEADER_ACCESS}"));
        self.line(&format!("{fewer} = add i64 {count}, -1"));
        self.line(&format!("store i64 {fewer}, ptr {block}{HEADER_ACCESS}"));
        self.line(&format!("{gone} = icmp eq i64 {fewer}, 0"));
        gone
    }

    /// Defines `@fen_retain`, which gives a block one more owner as [`Self::retain`] does, for
    /// helpers written as text to call.
    pub(crate) fn retain_helper(&mut self) {
        if self.module.threads {
            return self.counted_retain_helper();
        }
        self.module.add_helper(
            "fen_retain",
            format!(
                "define internal void @fen_retain(ptr %block) #0 {{\nentry:\n  %count = load i64, ptr %block{HEADER_ACCESS}\n  %more = add i64 %count, 1\n  store i64 %more, ptr %block{HEADER_ACCESS}\n  ret void\n}}\n\n"
            ),
        );
    }

    /// Defines `@fen_owner_gone`, which takes an owner off a block as [`Self::owner_gone`] does,
    /// for helpers written as text to call.
    pub(crate) fn owner_gone_helper(&mut self) {
        if self.module.threads {
            return self.counted_owner_gone_helper();
        }
        self.module.add_helper(
            "fen_owner_gone",
            format!(
                "define internal i1 @fen_owner_gone(ptr %block) #0 {{\nentry:\n  %count = load i64, ptr %block{HEADER_ACCESS}\n  %fewer = add i64 %count, -1\n  store i64 %fewer, ptr %block{HEADER_ACCESS}\n  %gone = icmp eq i64 %fewer, 0\n  ret i1 %gone\n}}\n\n"
            ),
        );
    }

    /// One owner fewer for a block, if there's a block, freeing it when that was the last, with
    /// `fennec_free`: what it frees may hold a generator let go partway that closes something,
    /// which can fail, so the function reads the flag after (see [`Self::owe_free_check`]). A
    /// block known to hold no such generator goes by [`Self::release_quietly`].
    pub(crate) fn release(&mut self, block: &str) {
        if !self.module.reads.abandons() {
            return self.release_quietly(block);
        }
        // A program's native types are layouts it has, so finding their free can't fail.
        let free = self.free_function().unwrap_or("@fennec_free");
        self.release_with(block, free);
        self.owe_free_check();
    }

    /// [`Self::release`], or [`Self::release_quietly`] for a block of a kind that lets go quietly.
    pub(crate) fn release_block(&mut self, block: &str, kind: &Kind) {
        if self.lets_go_quietly(kind) {
            self.release_quietly(block);
        } else {
            self.release(block);
        }
    }

    /// One owner fewer for a block whose type can't hold or reach a generator that closes
    /// something when it's let go partway, freeing it when that was the last, with
    /// `fennec_free_plain`, which runs no program code and can't fail.
    pub(crate) fn release_quietly(&mut self, block: &str) {
        let free = self.plain_free_function().unwrap_or("@fennec_free_plain");
        self.release_with(block, free);
    }

    /// One owner fewer for a block, if there's a block, freeing it with `free` when that was the
    /// last.
    fn release_with(&mut self, block: &str, free: &str) {
        debug_assert_eq!(COUNT, 0);
        let (none, held, last, done) =
            (self.temp(), self.label("held"), self.label("last"), self.label("done"));
        self.line(&format!("{none} = icmp eq ptr {block}, null"));
        self.line(&format!("br i1 {none}, label %{done}, label %{held}"));
        self.start_block(&held);
        let gone = self.owner_gone(block);
        self.line(&format!("br i1 {gone}, label %{last}, label %{done}"));
        self.start_block(&last);
        self.line(&format!("call void {free}(ptr {block})"));
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
    }

    /// Fails if a call here would go too deep. The limit never changes once the program's
    /// running and the frame stays put, so LLVM drops all but the first check on a path.
    pub(crate) fn check_stack(&mut self, span: Span) {
        self.declare("declare ptr @llvm.frameaddress.p0(i32)");
        let (declared, symbol) = if self.module.threads && !self.on_workers() {
            ("@fennec_stack_limit_main = external dso_local global i64", "@fennec_stack_limit_main")
        } else {
            (crate::parallel::stack_limit_global(self.module.threads), "@fennec_stack_limit")
        };
        self.declare(declared);
        let (here, limit, number, low) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{here} = call ptr @llvm.frameaddress.p0(i32 0)"));
        self.line(&format!("{limit} = load i64, ptr {symbol}, !invariant.load !1"));
        self.line(&format!("{number} = ptrtoint ptr {here} to i64"));
        self.line(&format!("{low} = icmp ult i64 {number}, {limit}"));
        self.fail_if(&low, TOO_DEEP, span);
    }

    /// This function's number for a place in the source, and the symbol of its table of places.
    /// The table is written fresh each build, in the module every build makes anyway, so the
    /// function's own text holds only the number and how long the span is: text added anywhere
    /// before the place moves the table's entry, and the function isn't compiled again.
    fn site(&mut self, at: u32) -> (usize, String) {
        let symbol = format!("{}.sites", self.start_symbol);
        let number = self.sites.iter().position(|known| *known == at).unwrap_or_else(|| {
            self.sites.push(at);
            self.sites.len() - 1
        });
        self.declare(&format!("@\"{symbol}\" = external dso_local global i64"));
        self.module.add_places(&symbol, self.function, &self.sites);
        (number, symbol)
    }

    /// Where a span is in the source, for a runtime call that says where it failed: its two ends,
    /// the first read from this function's table of places and the second that far on.
    pub(crate) fn place_in_source(&mut self, span: Span) -> (String, String) {
        // A place in the standard library never moves with the program's text, so it's written
        // as it is: std's own code, where most of these calls are, is what it was.
        if span.in_prelude() || matches!(self.named, Places::FromStart(_)) {
            return (span.start.to_string(), span.end.to_string());
        }
        let (number, symbol) = self.site(span.start);
        let length = span.end.saturating_sub(span.start);
        let (place, first, last) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{place} = getelementptr inbounds i64, ptr @\"{symbol}\", i64 {number}"));
        self.line(&format!("{first} = load i64, ptr {place}, !invariant.load !1"));
        self.line(&format!("{last} = add i64 {first}, {length}"));
        (first, last)
    }

    /// Where a span is, from this function's table of places, for failing with.
    pub(crate) fn span_values(&mut self, fail: &str, span: Span) -> (String, String) {
        if let Places::FromStart(start) = self.named {
            let symbol = self.start_symbol.clone();
            let (from, to) = (span.start.saturating_sub(start), span.end.saturating_sub(start));
            let text = format!(
                "  %s{fail} = load i64, ptr @\"{symbol}\"\n  %a{fail} = add i64 %s{fail}, {from}\n  %b{fail} = add i64 %s{fail}, {to}\n"
            );
            return (text, format!("i64 %a{fail}, i64 %b{fail}"));
        }
        let (number, symbol) = self.site(span.start);
        let length = span.end.saturating_sub(span.start);
        let text = format!(
            "  %s{fail} = getelementptr inbounds i64, ptr @\"{symbol}\", i64 {number}\n  %a{fail} = load i64, ptr %s{fail}\n  %b{fail} = add i64 %a{fail}, {length}\n"
        );
        (text, format!("i64 %a{fail}, i64 %b{fail}"))
    }

    /// How a function leaves when it has failed.
    pub(crate) fn failed_exit(&mut self) -> String {
        self.leaves.set(true);
        // Each way out on a failure is a check: what it's for, if what writes it said so.
        match self.check_kind.take() {
            Some((span, Some(what))) => self.note_check(span, what),
            Some((_, None)) => {}
            None => self.note_runtime_check(),
        }
        if self.failed == "br label %prop" {
            let out = self.way_out("prop");
            return format!("br label %{out}");
        }
        self.failed.clone()
    }

    /// Where a failure here goes on the way to `then`: a block away from the hot path that lets go
    /// of the short-lived locals that may hold something here, which `prop` leaves, or straight
    /// to `then` when there are none.
    fn way_out(&mut self, then: &str) -> String {
        self.ways_out(then).0
    }

    /// [`Self::way_out`], with the block that goes on to `then`, if it's one of its own.
    fn ways_out(&mut self, then: &str) -> (String, Option<String>) {
        if !self.split {
            return (then.to_owned(), None);
        }
        let mut short: Vec<usize> = self.holding_now.iter().chain(&self.filling).copied().collect();
        short.retain(|local| self.holding.short(*local));
        short.sort_unstable();
        short.dedup();
        if short.is_empty() {
            return (then.to_owned(), None);
        }
        let label = self.label("let_go");
        let (hot, block) = (std::mem::take(&mut self.out), self.current.clone());
        // What's let go of here is on a way out as the function fails already.
        let owed = self.free_owed;
        self.start_block(&label);
        for local in short {
            let kind = self.kinds.get(local).cloned().unwrap_or(Kind::Unit);
            if self.owns_inline(LocalId(local)) {
                let slot = self.slot(LocalId(local));
                self.release_inline(&kind, &slot);
                if self.checks_proofs() {
                    let zero = self.zero_line(&kind, &slot);
                    self.line(&zero);
                }
                continue;
            }
            let (held, slot, text) = (self.temp(), self.slot(LocalId(local)), self.text_of(&kind));
            self.line(&format!("{held} = load {text}, ptr {slot}"));
            self.release_kind(&held, &kind);
            // A build that checks proofs empties it too, for `prop` to check that every short-lived
            // local is empty once each way out has let go of what it held.
            if self.checks_proofs() && text == "ptr" {
                self.line(&format!("store ptr null, ptr {slot}"));
            }
        }
        self.line(&format!("br label %{then}"));
        self.free_owed = owed;
        let written = std::mem::replace(&mut self.out, hot);
        let last = std::mem::replace(&mut self.current, block);
        self.cold.push_str(&written);
        (label, Some(last))
    }

    /// Says what the next way out on a failure is for, or with `None` that it isn't a check.
    pub(crate) fn next_check(&mut self, span: Span, what: Option<CheckKind>) {
        self.check_kind = Some((span, what));
    }

    /// Leaves the function, the failure already recorded, when `failed` holds.
    pub(crate) fn leave_if(&mut self, failed: &str) {
        // Blocks made for the call that may have failed, which no local holds, go on the way out:
        // let go of only once the call's result is read, they'd be left for good.
        let made: Vec<(String, String)> = self.made_for_body.iter().flatten().cloned().collect();
        if !made.is_empty() {
            return self.leave_if_after(failed, |emit| {
                for (block, owned) in &made {
                    emit.let_go_text_block(block, owned);
                }
            });
        }
        let ok = self.label("ok");
        self.needs_prop = true;
        let out = self.way_out("prop");
        self.line(&format!("br i1 {failed}, label %{out}, label %{ok}, !prof !0"));
        self.start_block(&ok);
    }

    /// As [`Self::leave_if`], with `first` done on the way out and nowhere else: what a failed
    /// call leaves the function still holding is let go of there.
    pub(crate) fn leave_if_after(&mut self, failed: &str, first: impl FnOnce(&mut Self)) {
        let (leaving, ok) = (self.label("leaving"), self.label("ok"));
        self.needs_prop = true;
        let out = self.way_out("prop");
        self.line(&format!("br i1 {failed}, label %{leaving}, label %{ok}, !prof !0"));
        self.start_block(&leaving);
        first(self);
        self.line(&format!("br label %{out}"));
        self.start_block(&ok);
    }

    /// As [`Self::leave_if`], after a call into a function of the program: a failure out of it
    /// says this call is in its call chain on the way, away from the path that doesn't fail.
    /// Every such call leaves by one block, which a phi tells which call it was: a branch per call
    /// and nothing more, so the calls' code is what it would be without it.
    pub(crate) fn leave_traced(&mut self, failed: &str, span: Span) {
        let ok = self.label("ok");
        self.needs_prop = true;
        let (out, last) = self.ways_out("left_traced");
        let from = last.unwrap_or_else(|| self.current.clone());
        let place = match self.named {
            Places::FromStart(start) => span.start.saturating_sub(start),
            Places::Numbered => u32::try_from(self.site(span.start).0).unwrap_or(0),
        };
        self.traced_calls.push((from, place));
        self.line(&format!("br i1 {failed}, label %{out}, label %{ok}, !prof !0"));
        self.start_block(&ok);
    }

    /// The way out of the calls [`Self::leave_traced`] left by: it says which call this function
    /// failed at, then leaves as any failure does.
    fn write_traced_exit(&mut self) {
        if self.traced_calls.is_empty() {
            return;
        }
        self.declare("declare void @fennec_trace_frame(i64, i64) nounwind cold");
        let arms: Vec<String> =
            self.traced_calls.iter().map(|(block, from)| format!("[ {from}, %{block} ]")).collect();
        if matches!(self.named, Places::FromStart(_)) {
            let symbol = self.start_symbol.clone();
            let _ = writeln!(
                self.cold,
                "left_traced:\n  %left_from = phi i64 {}\n  %left_start = load i64, ptr @\"{symbol}\"\n  %left_at = add i64 %left_start, %left_from\n  call void @fennec_trace_frame(i64 %left_at, i64 %left_at)\n  br label %prop",
                arms.join(", ")
            );
            return;
        }
        let symbol = format!("{}.sites", self.start_symbol);
        let _ = writeln!(
            self.cold,
            "left_traced:\n  %left_from = phi i64 {}\n  %left_place = getelementptr inbounds i64, ptr @\"{symbol}\", i64 %left_from\n  %left_at = load i64, ptr %left_place\n  call void @fennec_trace_frame(i64 %left_at, i64 %left_at)\n  br label %prop",
            arms.join(", ")
        );
    }

    /// As [`Self::leave_if`], after a call from the program into the standard library: a failure
    /// inside it is said to be here, where the program called it.
    fn leave_pointing_here(&mut self, failed: &str, span: Span) {
        let (here, ok) = (self.label("from_std"), self.label("ok"));
        self.needs_prop = true;
        self.line(&format!("br i1 {failed}, label %{here}, label %{ok}, !prof !0"));
        self.start_block(&ok);
        self.declare("declare void @fennec_at_call(i64, i64) nounwind");
        let (place, args) = self.span_values(&here, span);
        let out = self.way_out("prop");
        let _ = writeln!(self.cold, "{here}:\n{place}  call void @fennec_at_call({args})\n  br label %{out}");
    }

    /// Where the instruction being written is in the source.
    pub(crate) fn here_span(&self) -> Span {
        let block = self.function.blocks.get(self.at.0);
        block.and_then(|block| block.insts.get(self.at.1)).map_or_else(Span::default, |inst| inst.span)
    }

    /// Whether checks left out because of a proof are written anyway, as assertions.
    pub(crate) fn checks_proofs(&self) -> bool {
        self.module.proofs() == crate::Proofs::Checked
    }

    /// In a build that checks proofs, ends the program if `broken`, an `i1`, is true: what the
    /// compiler proved, and left a check out for, didn't hold, which is a bug in the compiler.
    /// It says which proof, and where.
    pub(crate) fn proof_held(&mut self, broken: &str, proof: &str, span: Span) {
        let (stop, carry_on) = (self.label("proof_broken"), self.label("proof_held"));
        self.line(&format!("br i1 {broken}, label %{stop}, label %{carry_on}, !prof !8"));
        self.start_block(&stop);
        // It ends the program where it is, so the function needn't be one that can fail.
        let text = self.literal(&format!("proof broken: {proof}"));
        self.declare("declare void @fennec_proof_broken(ptr, i64, i64) #1");
        let (place, args) = self.span_values(&stop, span);
        for line in place.lines() {
            self.line(line.trim_start());
        }
        self.line(&format!("call void @fennec_proof_broken(ptr {text}, {args})"));
        self.line("unreachable");
        self.start_block(&carry_on);
    }

    /// In a build that checks proofs, ends the program if `broken` is true, as `proof_held` does,
    /// but through a call LLVM copies in, so the block this is in carries on: for a fact checked
    /// where what comes after leans on the block it's written in, like a phi.
    /// In a build that checks proofs, that an item copied out and put back at `at` goes back
    /// where it was taken from: the place still holds what was taken, which the local it was
    /// first read into still has. A put back anywhere else would leave that block with no owner.
    /// For a struct kept in place, the place still holds the words that were copied out.
    pub(crate) fn proof_goes_back(&mut self, value: &Operand, at: &str) {
        if !self.checks_proofs() {
            return;
        }
        let Operand::Local(item) = value else { return };
        let Some(read) = self.known.put_back_read(*item) else { return };
        let Ok(kind) = self.kind_of(read).cloned() else { return };
        let slot = self.slot(read);
        let words = if kind.in_memory() { kind.size(self.module.layouts) / 8 } else { 1 };
        let mut elsewhere = "false".to_owned();
        for word in 0..words {
            let (there_at, taken_at, there, taken, differs, either) =
                (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
            self.line(&format!("{there_at} = getelementptr inbounds i64, ptr {at}, i64 {word}"));
            self.line(&format!("{taken_at} = getelementptr inbounds i64, ptr {slot}, i64 {word}"));
            self.line(&format!("{there} = load i64, ptr {there_at}"));
            self.line(&format!("{taken} = load i64, ptr {taken_at}"));
            self.line(&format!("{differs} = icmp ne i64 {there}, {taken}"));
            self.line(&format!("{either} = or i1 {elsewhere}, {differs}"));
            elsewhere = either;
        }
        let span = self.here_span();
        self.proof_held(&elsewhere, "an item copied out goes back to the place it was taken from", span);
    }

    pub(crate) fn proof_held_here(&mut self, broken: &str, proof: &str, span: Span) {
        let text = self.proof_text(proof);
        let here = self.label("proof_at");
        let (place, args) = self.span_values(&here, span);
        for line in place.lines() {
            self.line(line.trim_start());
        }
        self.line(&format!("call void @fen_proof_held(i1 {broken}, ptr {text}, {args})"));
    }

    /// What a broken proof says, for `@fen_proof_held`, which this defines for the module.
    pub(crate) fn proof_text(&mut self, proof: &str) -> String {
        let symbol = self.proof_symbol(proof);
        let text = self.temp();
        self.line(&format!("{text} = load ptr, ptr {symbol}, !invariant.load !1"));
        text
    }

    /// The global holding what a broken proof says, for a helper to load and give
    /// `@fen_proof_held`, which this defines for the module.
    pub(crate) fn proof_symbol(&mut self, proof: &str) -> String {
        self.declare("declare void @fennec_proof_broken(ptr, i64, i64) #1");
        self.module.add_helper(
            "fen_proof_held",
            "define internal void @fen_proof_held(i1 %broken, ptr %proof, i64 %from, i64 %to) #2 {\nentry:\n  br i1 %broken, label %stop, label %held, !prof !8\nstop:\n  call void @fennec_proof_broken(ptr %proof, i64 %from, i64 %to)\n  unreachable\nheld:\n  ret void\n}\n\n".to_owned(),
        );
        self.literal_symbol(&format!("proof broken: {proof}"))
    }

    /// Checks that each address a parameter has alone, which a build that doesn't check proofs
    /// tells LLVM is `noalias` (the place a value is given back in, and each `ref` parameter), is
    /// no other parameter's address.
    fn addresses_apart(&mut self, span: Span) -> Result<(), String> {
        let function = self.function;
        let mut alone = Vec::new();
        let mut addresses = Vec::new();
        if self.module.kind(&function.returns)?.in_memory() {
            alone.push("%ret".to_owned());
            addresses.push("%ret".to_owned());
        }
        for (at, local) in function.locals.iter().take(function.params).enumerate() {
            let kind = self.module.kind(&local.ty)?;
            let by_address = function.mut_params.contains(&at);
            if by_address {
                alone.push(format!("%p{at}"));
            }
            if by_address || kind.in_memory() || kind.slot(self.module.layouts) == "ptr" {
                addresses.push(format!("%p{at}"));
            }
        }
        for (at, one) in alone.iter().enumerate() {
            for other in addresses.iter().filter(|other| *other != one) {
                if alone.iter().take(at).any(|before| before == other) {
                    continue;
                }
                let same = self.temp();
                self.line(&format!("{same} = icmp eq ptr {one}, {other}"));
                self.proof_held_here(&same, "a `ref` parameter's address is no other parameter's", span);
            }
        }
        Ok(())
    }

    /// Says `holds`, an `i1`, is true: to LLVM, as an assumption, or in a build that checks proofs,
    /// as a check that stops the program with `proof` if it isn't.
    pub(crate) fn assumed(&mut self, holds: &str, proof: &str, span: Span) {
        if self.checks_proofs() {
            let broken = self.temp();
            self.line(&format!("{broken} = xor i1 {holds}, true"));
            self.proof_held_here(&broken, proof, span);
        } else {
            self.declare("declare void @llvm.assume(i1)");
            self.line(&format!("call void @llvm.assume(i1 {holds})"));
        }
    }

    /// Reads the length of a list parameter whose length nothing in this function changes into
    /// the place it's kept (`%sl`, see `steady_lengths`), at the function's start.
    ///
    /// A length is loaded as `!noundef` (`LENGTH_ACCESS`): a kept length LLVM had to take as
    /// maybe poison was frozen where a loop was split on it, which stopped it splitting the loop
    /// further, as it does on a length read in the loop, and fannkuch ran 6% more instructions.
    fn keep_length(&mut self, local: LocalId) {
        let (list, slot) = (self.temp(), self.slot(local));
        self.line(&format!("{list} = load ptr, ptr {slot}"));
        let length = self.list_length(&list);
        self.line(&format!("store i64 {length}, ptr %sl{}", local.0));
    }

    /// The length of `list`, read from the local `from`: the length kept for it if nothing in this
    /// function changes its length, which LLVM can see doesn't change in a loop and so can check
    /// once, or else the one in the list. A build that checks proofs checks the two are the same.
    pub(crate) fn length_of_list(&mut self, from: Option<LocalId>, list: &str) -> String {
        let Some(local) = from.filter(|local| self.known.steady_lengths.contains(&local.0)) else {
            return self.list_length(list);
        };
        let length = self.temp();
        self.line(&format!("{length} = load i64, ptr %sl{}", local.0));
        if self.checks_proofs() {
            let (own, differs) = (self.list_length(list), self.temp());
            self.line(&format!("{differs} = icmp ne i64 {length}, {own}"));
            self.proof_held_here(&differs, "a list's length is the one kept for it", Span::default());
        }
        length
    }

    /// Loads a length from `at` into `dest`. LLVM is told it's from 0 to the most items anything
    /// can have, or a build that checks proofs checks that.
    pub(crate) fn load_length(&mut self, dest: &str, at: &str) {
        if self.checks_proofs() {
            self.line(&format!("{dest} = load i64, ptr {at}{HEADER_ACCESS}, !noundef !1"));
            let broken = self.temp();
            self.line(&format!("{broken} = icmp ugt i64 {dest}, {}", crate::analysis::MOST_ITEMS));
            self.proof_held_here(
                &broken,
                "a length is at most the most items anything can have",
                Span::default(),
            );
        } else {
            self.line(&format!("{dest} = load i64, ptr {at}{LENGTH_ACCESS}"));
        }
    }

    /// In a build that checks proofs, ends the program if the block at `block` doesn't have one
    /// owner, as a proof says it does.
    pub(crate) fn proof_alone(&mut self, block: &str, proof: &str, span: Span) {
        if !self.checks_proofs() {
            return;
        }
        let alone = self.alone(block);
        let shared = self.temp();
        self.line(&format!("{shared} = xor i1 {alone}, true"));
        self.proof_held(&shared, proof, span);
    }

    /// Calls the runtime's failure and leaves the function when `bad` holds.
    pub(crate) fn fail_if(&mut self, bad: &str, code: i64, span: Span) {
        self.fail_noted(bad, code, span, None);
    }

    /// `fail_if`, with what the check is for when the code alone doesn't say.
    fn fail_noted(&mut self, bad: &str, code: i64, span: Span, noted: Option<CheckKind>) {
        let what = noted.or_else(|| match code {
            OVERFLOW => Some(CheckKind::Overflow),
            DIVISION_BY_ZERO => Some(CheckKind::DivideByZero),
            fennec_rt::embed::checks::BAD_SHIFT => Some(CheckKind::ShiftRange),
            // How deep the calls go is checked on the way into a function, not by what it does.
            TOO_DEEP => None,
            _ => Some(CheckKind::Builtin(self.failing_here())),
        });
        self.next_check(span, what);
        let (fail, ok) = (self.label("fail"), self.label("ok"));
        self.line(&format!("br i1 {bad}, label %{fail}, label %{ok}, !prof !0"));
        self.start_block(&ok);
        self.declare("declare void @fennec_fail(i64, i64, i64) #1");
        let (place, args) = self.span_values(&fail, span);
        let failed = self.failed_exit();
        let _ =
            writeln!(self.cold, "{fail}:\n{place}  call void @fennec_fail(i64 {code}, {args})\n  {failed}");
    }

    /// The case a union's member is held under: the member's name, by its place among them.
    fn member_case(&self, union: &Operand, member: usize) -> Result<String, String> {
        match self.function.type_of(union) {
            Some(fennec_check::Type::Union(members)) => members
                .get(member)
                .map(ToString::to_string)
                .ok_or_else(|| format!("a union has no member {member}")),
            other => Err(format!("it reads a union's member from a {other:?}")),
        }
    }

    /// Notes a check as it's written, when checks are being noted.
    pub(crate) fn note_check(&mut self, span: Span, what: CheckKind) {
        if self.module.noting_checks() {
            self.module.add_check(self.function, span, what);
        }
    }

    /// What the instruction being written is, for a check the runtime makes in it.
    fn failing_here(&self) -> String {
        let function = self.function;
        let inst = function.blocks.get(self.at.0).and_then(|block| block.insts.get(self.at.1));
        match inst.map(|inst| &inst.kind) {
            Some(InstKind::Builtin { name, .. }) => name.clone(),
            Some(InstKind::Format { .. }) => "a format".to_owned(),
            Some(InstKind::Slice { .. }) => "a slice".to_owned(),
            Some(InstKind::Constant { .. }) => "a constant".to_owned(),
            Some(InstKind::Lock { .. } | InstKind::LockAll { .. }) => "a lock".to_owned(),
            _ => "a check".to_owned(),
        }
    }

    /// Notes the check the runtime made in what was just called, which leaves when it failed.
    /// What a call to a function fails with is noted in that function, and a call through a
    /// closure or into a generator in what runs there.
    pub(crate) fn note_runtime_check(&mut self) {
        if !self.module.noting_checks() {
            return;
        }
        let function = self.function;
        let Some(inst) = function.blocks.get(self.at.0).and_then(|block| block.insts.get(self.at.1)) else {
            return;
        };
        let what = match &inst.kind {
            InstKind::Call { .. } | InstKind::CallClosure { .. } | InstKind::Resume { .. } => return,
            InstKind::Binary { op: BinOp::Pow, .. } => CheckKind::Overflow,
            InstKind::GetIndex { object, .. } => match function.type_of(object) {
                Some(
                    fennec_check::Type::Str
                    | fennec_check::Type::Bytes
                    | fennec_check::Type::List(_)
                    | fennec_check::Type::Array(..),
                ) => CheckKind::Bounds,
                _ => CheckKind::MissingKey,
            },
            InstKind::SetIndex { .. } => CheckKind::MissingKey,
            _ => CheckKind::Builtin(self.failing_here()),
        };
        self.note_check(inst.span, what);
    }

    /// A value: a number as itself, a block's address, or the slot of something in memory.
    pub(crate) fn operand(&mut self, operand: &Operand) -> Result<(String, Kind), String> {
        Ok(match operand {
            Operand::Local(local) => {
                let kind = self.kind_of(*local)?.clone();
                let slot = self.slot(*local);
                if kind.in_memory() {
                    return Ok((slot, kind));
                }
                let text = self.text_of(&kind);
                let temp = self.temp();
                self.line(&format!("{temp} = load {text}, ptr {slot}"));
                (temp, kind)
            }
            Operand::Int(value) => (value.to_string(), Kind::Int),
            Operand::Float(value) => (float_text(*value), Kind::Float),
            Operand::Bool(value) => (value.to_string(), Kind::Bool),
            Operand::Str(text) => (self.literal_value(text), Kind::Str),
            Operand::Bytes(data) => (self.bytes_literal(&data.0), Kind::Bytes),
            Operand::None => ("null".to_owned(), Kind::Unit),
        })
    }

    /// Puts a value in a local. A local that owns a block lets go of what it held.
    pub(crate) fn assign(&mut self, dest: LocalId, value: &str) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let text = self.text_of(&kind);
        if text == "void" {
            return Ok(());
        }
        let slot = self.slot(dest);
        if self.owns(dest) || (kind.counted() && self.is_mut_param(dest)) {
            let old = self.temp();
            self.line(&format!("{old} = load {text}, ptr {slot}"));
            self.line(&format!("store {text} {value}, ptr {slot}"));
            self.release_kind(&old, &kind);
        } else {
            self.line(&format!("store {text} {value}, ptr {slot}"));
        }
        Ok(())
    }

    /// Copies what's in memory at `from` into a local: an inline struct's blocks get another
    /// owner, and let go of what the local held, if the local owns what it holds.
    pub(crate) fn assign_memory(&mut self, dest: LocalId, from: &str, access: &str) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let text = self.text_of(&kind);
        let (held, slot) = (self.temp(), self.slot(dest));
        let owner = self.keeps_inline(dest);
        if owner {
            self.release_inline(&kind, &slot);
        }
        if !self.copied_as_bytes(&kind, from, &slot) {
            self.line(&format!("{held} = load {text}, ptr {from}{access}"));
            self.line(&format!("store {text} {held}, ptr {slot}"));
        }
        if owner {
            self.retain_inline(&kind, &slot);
        }
        Ok(())
    }

    /// Moves an inline struct whose blocks are already owned, like a call's result, into a
    /// local, letting go of what the local held.
    pub(crate) fn assign_memory_owned(&mut self, dest: LocalId, from: &str) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let text = self.text_of(&kind);
        let (held, slot) = (self.temp(), self.slot(dest));
        if self.keeps_inline(dest) {
            self.release_inline(&kind, &slot);
        } else {
            // Nothing here owns them, so they're let go of now.
            if !self.copied_as_bytes(&kind, from, &slot) {
                self.line(&format!("{held} = load {text}, ptr {from}"));
                self.line(&format!("store {text} {held}, ptr {slot}"));
            }
            self.release_inline(&kind, from);
            return Ok(());
        }
        if !self.copied_as_bytes(&kind, from, &slot) {
            self.line(&format!("{held} = load {text}, ptr {from}"));
            self.line(&format!("store {text} {held}, ptr {slot}"));
        }
        Ok(())
    }

    /// Lets go of what a local's inline struct or enum holds, before it's written over in place.
    pub(crate) fn release_held(&mut self, dest: LocalId) -> Result<(), String> {
        if self.keeps_inline(dest) {
            let (kind, slot) = (self.kind_of(dest)?.clone(), self.slot(dest));
            self.release_inline(&kind, &slot);
        }
        Ok(())
    }

    /// Whether what's written to a local stays owned there: by the local, or for a `mut`
    /// parameter by whatever it was called with.
    fn keeps_inline(&self, local: LocalId) -> bool {
        self.owns_inline(local)
            || (self.is_mut_param(local)
                && self.kinds.get(local.0).is_some_and(|kind| self.holds_inline(kind)))
    }

    /// Whether `dest = lhs + more` is a text, a list or bytes growing, from a local that holds its
    /// only owner or took it from a field, or from a `ref` parameter. A parameter's slot is the caller's, which
    /// holds its owner, so growing it there is the caller's text growing, as [`Self::assign`]
    /// writes there too. Not where a change asks a lock's log first: the text may be in what the
    /// lock holds, which has to be put back as it was if the lock's block fails.
    fn grows_in_place(&self, op: BinOp, lhs: &Operand, dest: LocalId) -> bool {
        let handed = || {
            self.is_mut_param(dest)
                && !self.asks_the_log()
                && self.known.alias(dest).is_none()
                && !self.known.is_borrowed(dest)
        };
        op == BinOp::Add
            && *lhs == Operand::Local(dest)
            && (self.owns(dest) || self.known.is_taken(dest) || handed())
    }

    /// Whether a local holds an inline struct or enum with blocks in it, and owns them.
    pub(crate) fn owns_inline(&self, local: LocalId) -> bool {
        self.kinds.get(local.0).is_some_and(|kind| self.holds_inline(kind))
            && !self.is_mut_param(local)
            && !self.is_lent(local)
            && self.known.alias(local).is_none()
            && !self.known.is_borrowed(local)
            && !self.known.is_taken(local)
    }

    /// Reads a value of this kind from inside something, as a number or a block's address.
    pub(crate) fn load_field(&mut self, kind: &Kind, at: &str, access: &str) -> String {
        // A struct held in place inside something no local holds may be new to this module.
        self.module.define_types(kind);
        let memory = kind.memory(self.module.layouts);
        let held = self.temp();
        self.line(&format!("{held} = load {memory}, ptr {at}{access}"));
        if *kind == Kind::Bool {
            let bit = self.temp();
            self.line(&format!("{bit} = trunc i64 {held} to i1"));
            return bit;
        }
        held
    }

    /// Writes a value inside something: a number as a word, or what's at a slot, in memory.
    pub(crate) fn store_field(&mut self, kind: &Kind, value: &str, at: &str, access: &str) {
        self.module.define_types(kind);
        let memory = kind.memory(self.module.layouts);
        if kind.in_memory() {
            if self.copied_as_bytes(kind, value, at) {
                return;
            }
            let held = self.temp();
            self.line(&format!("{held} = load {memory}, ptr {value}"));
            self.line(&format!("store {memory} {held}, ptr {at}{access}"));
        } else if *kind == Kind::Bool {
            let word = self.temp();
            self.line(&format!("{word} = zext i1 {value} to i64"));
            self.line(&format!("store i64 {word}, ptr {at}{access}"));
        } else {
            self.line(&format!("store {memory} {value}, ptr {at}{access}"));
        }
    }

    /// The sized number type a local holds, if it's one.
    pub(crate) fn sized(&self, local: LocalId) -> Option<fennec_check::Num> {
        match self.function.locals.get(local.0).map(|local| &local.ty) {
            Some(fennec_check::Type::Sized(num)) => Some(*num),
            _ => None,
        }
    }

    /// A word cut to a size's bits, as that size holds them: kept unsigned, or with the sign of its
    /// top bit.
    pub(crate) fn within_width(&mut self, value: &str, num: fennec_check::Num) -> String {
        let bits = num.bits();
        if bits >= 64 {
            return value.to_owned();
        }
        let (narrow, wide) = (self.temp(), self.temp());
        self.line(&format!("{narrow} = trunc i64 {value} to i{bits}"));
        let extend = if num.is_unsigned() { "zext" } else { "sext" };
        self.line(&format!("{wide} = {extend} i{bits} {narrow} to i64"));
        wide
    }

    /// Stops the program as an overflow, when an integer worked out for a local of a sized type
    /// doesn't fit it.
    pub(crate) fn check_size(&mut self, dest: LocalId, value: &str, span: Span) {
        let Some(num) = self.sized(dest) else { return };
        // A `u64` fills its word, so its operations check for themselves.
        if num == fennec_check::Num::U64 {
            return;
        }
        let Some((least, most)) = num.range() else { return };
        // One unsigned comparison: the value less the least is in range only when it's below
        // the range's width.
        let (shifted, bad) = (self.temp(), self.temp());
        self.line(&format!("{shifted} = sub i64 {value}, {least}"));
        self.line(&format!("{bad} = icmp ugt i64 {shifted}, {}", most - least));
        self.size_overflow(&bad, num, span);
    }

    /// Stops the program, when `bad` holds, saying that a calculation overflowed this size.
    pub(crate) fn size_overflow(&mut self, bad: &str, num: fennec_check::Num, span: Span) {
        let (least, most) = num.range().unwrap_or((0, 0));
        let ty = fennec_check::Type::Sized(num);
        let said = format!("this calculation overflows {}, which holds {least} to {most}", article_of(&ty));
        let text = self.literal(&said);
        self.declare("declare void @fennec_stop(ptr, i64, i64) #1");
        let (over, fits) = (self.label("over"), self.label("fits"));
        self.line(&format!("br i1 {bad}, label %{over}, label %{fits}, !prof !0"));
        self.start_block(&over);
        let (place, args) = self.span_values(&over, span);
        for line in place.lines() {
            self.line(line.trim_start());
        }
        self.line(&format!("call void @fennec_stop(ptr {text}, {args})"));
        self.next_check(span, Some(CheckKind::Overflow));
        let failed = self.failed_exit();
        self.line(&failed);
        self.start_block(&fits);
    }

    pub(crate) fn checked_add(&mut self, a: &str, b: &str, span: Span) -> String {
        self.checked("sadd", a, b, span)
    }

    /// What an arithmetic proven not to overflow gives, in a build that checks proofs: worked out
    /// with the check, which says the proof broke if it fails.
    fn proven_unchecked(&mut self, name: &str, (a, b): (&str, &str), span: Span) -> String {
        let intrinsic = format!("@llvm.{name}.with.overflow.i64");
        self.declare(&format!("declare {{ i64, i1 }} {intrinsic}(i64, i64)"));
        let (pair, value, bad) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{pair} = call {{ i64, i1 }} {intrinsic}(i64 {a}, i64 {b})"));
        self.line(&format!("{bad} = extractvalue {{ i64, i1 }} {pair}, 1"));
        self.proof_held(&bad, "no overflow", span);
        self.line(&format!("{value} = extractvalue {{ i64, i1 }} {pair}, 0"));
        value
    }

    /// Whether the instruction being written multiplies two ints proven not negative (see `signs`).
    fn operands_not_negative(&self) -> bool {
        self.module
            .reads
            .non_negative(self.function.name.as_str())
            .is_some_and(|found| found.contains(&self.at))
    }

    /// A product of two ints proven not negative, which overflows when it passes `i64::MAX`: an
    /// unsigned multiply says when it passes `u64::MAX`, and the product's top bit when it's past
    /// `i64::MAX` short of that. That's the check Rust makes on a `usize`, one multiply fewer than
    /// a signed one, which works the product out twice. A build that checks proofs checks that
    /// neither is negative.
    fn checked_not_negative_product(&mut self, a: &str, b: &str, span: Span) -> String {
        if self.checks_proofs() {
            let (either, negative) = (self.temp(), self.temp());
            self.line(&format!("{either} = or i64 {a}, {b}"));
            self.line(&format!("{negative} = icmp slt i64 {either}, 0"));
            self.proof_held(&negative, "a product's ints aren't negative", span);
        }
        self.declare("declare { i64, i1 } @llvm.umul.with.overflow.i64(i64, i64)");
        let (pair, value, wrapped, past, bad) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{pair} = call {{ i64, i1 }} @llvm.umul.with.overflow.i64(i64 {a}, i64 {b})"));
        self.line(&format!("{wrapped} = extractvalue {{ i64, i1 }} {pair}, 1"));
        self.line(&format!("{value} = extractvalue {{ i64, i1 }} {pair}, 0"));
        self.line(&format!("{past} = icmp slt i64 {value}, 0"));
        self.line(&format!("{bad} = or i1 {wrapped}, {past}"));
        self.fail_if(&bad, OVERFLOW, span);
        value
    }

    fn checked(&mut self, name: &str, a: &str, b: &str, span: Span) -> String {
        let intrinsic = format!("@llvm.{name}.with.overflow.i64");
        self.declare(&format!("declare {{ i64, i1 }} {intrinsic}(i64, i64)"));
        let (pair, value, bad) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{pair} = call {{ i64, i1 }} {intrinsic}(i64 {a}, i64 {b})"));
        self.line(&format!("{bad} = extractvalue {{ i64, i1 }} {pair}, 1"));
        self.fail_if(&bad, OVERFLOW, span);
        // A product past the check is the plain multiply's, which says it didn't overflow: so LLVM
        // knows one of numbers that aren't negative isn't either, and can drop checks after it
        // that can't fail. A sum or difference stays as it was, as saying the same of a counter
        // lets LLVM unroll short loops in ways that cost more than they save.
        if name == "smul" {
            self.line(&format!("{value} = mul nsw i64 {a}, {b}"));
        } else {
            self.line(&format!("{value} = extractvalue {{ i64, i1 }} {pair}, 0"));
        }
        value
    }

    fn binary(
        &mut self,
        dest: LocalId,
        op: BinOp,
        lhs: &Operand,
        rhs: &Operand,
        span: Span,
    ) -> Result<(), String> {
        if let (BinOp::In | BinOp::NotIn, Operand::Local(right)) = (op, rhs)
            && matches!(self.kind_of(*right)?, Kind::Dict(..))
        {
            let found = self.dict_contains(op, lhs, rhs)?;
            return self.assign(dest, &found);
        }
        let (a, mut kind) = self.operand(lhs)?;
        let (mut b, mut b_kind) = self.operand(rhs)?;
        // A number written out takes the size of what's beside it, so the arithmetic is that size's.
        if matches!(lhs, Operand::Int(_) | Operand::Float(_)) && matches!(b_kind, Kind::U64 | Kind::F32) {
            kind = b_kind.clone();
        }
        // `None` beside an optional string is that string's `None`: a header all zero.
        if *rhs == Operand::None && kind == Kind::MaybeText {
            (b, b_kind) = ("zeroinitializer".to_owned(), Kind::MaybeText);
        }
        let optional = (kind == Kind::MaybeText, b_kind == Kind::MaybeText);
        let value = match kind {
            Kind::U64 => self.u64_binary(op, (&a, &b), rhs, span)?,
            Kind::F32 => self.f32_binary(op, (&a, &b), rhs, span)?,
            Kind::Int => {
                let value = self.int_binary(dest, op, (&a, &b), rhs, span)?;
                match (op, self.sized(dest)) {
                    // Bits shifted past a size's top are dropped, as on every integer type.
                    (BinOp::ShiftLeft, Some(num)) => self.within_width(&value, num),
                    // What's worked out in a word has to fit its size; the bits of two that do, do.
                    (BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor, _) => value,
                    _ => {
                        self.check_size(dest, &value, span);
                        value
                    }
                }
            }
            Kind::Str | Kind::MaybeText
                if matches!(op, BinOp::Eq | BinOp::NotEq) && (optional.0 || optional.1) =>
            {
                self.maybe_text_binary(op, (&a, optional.0), (&b, optional.1))
            }
            Kind::Float => self.float_binary(op, (&a, &b), rhs, span)?,
            Kind::Bool => {
                let cc = match op {
                    BinOp::Eq => "eq",
                    BinOp::NotEq => "ne",
                    BinOp::Lt => "ult",
                    BinOp::LtEq => "ule",
                    BinOp::Gt => "ugt",
                    BinOp::GtEq => "uge",
                    other => return Err(format!("it uses {other:?} on bools")),
                };
                let temp = self.temp();
                self.line(&format!("{temp} = icmp {cc} i1 {a}, {b}"));
                temp
            }
            Kind::Str if self.headers() && self.grows_in_place(op, lhs, dest) => {
                return self.header_append(dest, &a, &b);
            }
            Kind::Str if self.headers() => self.header_binary(op, (&a, &b))?,
            Kind::Str if self.grows_in_place(op, lhs, dest) => {
                // `text += more` takes over the owner the local had, and grows in place if it can.
                let joined = self.append_text(&a, &b);
                let slot = self.slot(dest);
                self.line(&format!("store ptr {joined}, ptr {slot}"));
                return Ok(());
            }
            Kind::Str if matches!(op, BinOp::Eq | BinOp::NotEq) => match (lhs, rhs) {
                (_, Operand::Str(literal)) if literal.len() <= 8 => self.same_as_short(op, &a, literal),
                (Operand::Str(literal), _) if literal.len() <= 8 => self.same_as_short(op, &b, literal),
                _ => self.string_binary(op, (&a, &b))?,
            },
            Kind::Str => self.string_binary(op, (&a, &b))?,
            // Two `shared` values are the same only if they're one value.
            Kind::Boxed(_) if self.is_cell(&kind) && matches!(op, BinOp::Eq | BinOp::NotEq) => {
                let (same, cc) = (self.temp(), if op == BinOp::Eq { "eq" } else { "ne" });
                self.line(&format!("{same} = icmp {cc} ptr {a}, {b}"));
                same
            }
            Kind::Boxed(id) if self.is_boxed_enum(id) && matches!(op, BinOp::Eq | BinOp::NotEq) => {
                self.same_boxed_enums(op, (&a, &b))
            }
            Kind::Boxed(_) | Kind::Bytes | Kind::List(_) | Kind::Dict(..) | Kind::Maybe(_)
                if matches!(op, BinOp::Eq | BinOp::NotEq) =>
            {
                self.same_blocks(op, (&a, &b))
            }
            Kind::Closure(..) if matches!(op, BinOp::Eq | BinOp::NotEq) => self.same_code(op, (&a, &b)),
            Kind::List(_) if self.grows_in_place(op, lhs, dest) => {
                // `items += more` takes over the owner the local had, and grows in place if it can.
                let grown = self.extended_list(&a, &b);
                let slot = self.slot(dest);
                self.line(&format!("store ptr {grown}, ptr {slot}"));
                return Ok(());
            }
            Kind::List(_) if op == BinOp::Add => self.joined_lists(&a, &b),
            Kind::Bytes if self.grows_in_place(op, lhs, dest) => {
                self.bytes_grown(dest, &b);
                return Ok(());
            }
            Kind::Bytes => self.bytes_binary(op, (&a, &b))?,
            Kind::List(item) if matches!(op, BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq) => {
                self.lists_in_order(op, item, (&a, &b), span)?
            }
            Kind::Enum(id) if matches!(op, BinOp::Eq | BinOp::NotEq) => self.same_enums(op, id, (&a, &b))?,
            Kind::Struct(id) => self.compare_structs(op, id, (&a, &b))?,
            Kind::Boxed(id) if matches!(op, BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq) => {
                self.compare_boxed(op, id, (&a, &b))?
            }
            other => {
                return Err(format!("it uses an operator on {other:?}, which this backend doesn't take yet"));
            }
        };
        self.assign(dest, &value)
    }

    fn int_binary(
        &mut self,
        dest: LocalId,
        op: BinOp,
        (a, b): (&str, &str),
        rhs: &Operand,
        span: Span,
    ) -> Result<String, String> {
        let compare = match op {
            BinOp::Eq => Some("eq"),
            BinOp::NotEq => Some("ne"),
            BinOp::Lt => Some("slt"),
            BinOp::LtEq => Some("sle"),
            BinOp::Gt => Some("sgt"),
            BinOp::GtEq => Some("sge"),
            _ => None,
        };
        if let Some(cc) = compare {
            let temp = self.temp();
            self.line(&format!("{temp} = icmp {cc} i64 {a}, {b}"));
            return Ok(temp);
        }
        // A build that checks proofs keeps the check, saying a proof broke if it fails.
        let safe = self.known.is_safe(dest);
        if safe && self.checks_proofs() && matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) {
            let name = match op {
                BinOp::Add => "sadd",
                BinOp::Sub => "ssub",
                _ => "smul",
            };
            return Ok(self.proven_unchecked(name, (a, b), span));
        }
        Ok(match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul if safe => {
                let word = match op {
                    BinOp::Add => "add",
                    BinOp::Sub => "sub",
                    _ => "mul",
                };
                let temp = self.temp();
                self.line(&format!("{temp} = {word} nsw i64 {a}, {b}"));
                temp
            }
            BinOp::Add => self.checked("sadd", a, b, span),
            BinOp::Sub => self.checked("ssub", a, b, span),
            BinOp::Mul if self.operands_not_negative() => self.checked_not_negative_product(a, b, span),
            BinOp::Mul => self.checked("smul", a, b, span),
            BinOp::FloorDiv | BinOp::Mod => self.division(op, (a, b), rhs, span),
            BinOp::Div => {
                // A number written out that isn't zero needs no check.
                if matches!(rhs, Operand::Local(_) | Operand::Int(0)) {
                    let zero = self.temp();
                    self.line(&format!("{zero} = icmp eq i64 {b}, 0"));
                    self.fail_if(&zero, DIVISION_BY_ZERO, span);
                }
                let (x, y, ratio) = (self.temp(), self.temp(), self.temp());
                self.line(&format!("{x} = sitofp i64 {a} to double"));
                self.line(&format!("{y} = sitofp i64 {b} to double"));
                self.line(&format!("{ratio} = fdiv double {x}, {y}"));
                ratio
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                let word = match op {
                    BinOp::BitAnd => "and",
                    BinOp::BitOr => "or",
                    _ => "xor",
                };
                let temp = self.temp();
                self.line(&format!("{temp} = {word} i64 {a}, {b}"));
                temp
            }
            BinOp::ShiftLeft | BinOp::ShiftRight => {
                // A shift by as many bits as the type has, or more, stops the program.
                let sized = self.sized(dest);
                let width = sized.map_or(64, |num| i64::from(num.bits()));
                if !matches!(rhs, Operand::Int(count) if (0..width).contains(count)) {
                    let outside = self.temp();
                    self.line(&format!("{outside} = icmp uge i64 {b}, {width}"));
                    match sized {
                        Some(num) => {
                            let said =
                                format!("can't shift {} by a count outside 0 to {}", num.name(), width - 1);
                            self.stop_if(&outside, &said, span);
                        }
                        None => self.fail_if(&outside, fennec_rt::embed::checks::BAD_SHIFT, span),
                    }
                }
                let word = if op == BinOp::ShiftLeft { "shl" } else { "ashr" };
                let temp = self.temp();
                self.line(&format!("{temp} = {word} i64 {a}, {b}"));
                temp
            }
            BinOp::Pow => {
                self.declare("declare i64 @fennec_int_power(i64, i64, i64, i64) nounwind");
                let (from, to) = self.place_in_source(span);
                let temp = self.temp();
                self.line(&format!(
                    "{temp} = call i64 @fennec_int_power(i64 {a}, i64 {b}, i64 {from}, i64 {to})"
                ));
                self.leave_if_failed();
                temp
            }
            other => return Err(format!("it uses {other:?} on ints, which this backend doesn't take yet")),
        })
    }

    /// Rounds toward negative infinity, unlike the machine's division.
    fn division(&mut self, op: BinOp, (a, b): (&str, &str), rhs: &Operand, span: Span) -> String {
        // By a power of two, rounding toward negative infinity is what shifting does, and the
        // remainder that goes with it is the low bits, whatever the sign: `-7 // 2` is `-7 >> 1`,
        // -4, and `-7 % 2` is `-7 & 1`, 1. So nothing's left to fix up, and LLVM can see that
        // `n % 2 == 0` leaves `n // 2` exact.
        if let Operand::Int(divisor @ 1..) = rhs
            && divisor.count_ones() == 1
        {
            let result = self.temp();
            if op == BinOp::FloorDiv {
                self.line(&format!("{result} = ashr i64 {a}, {}", divisor.trailing_zeros()));
            } else {
                self.line(&format!("{result} = and i64 {a}, {}", divisor - 1));
            }
            return result;
        }
        // A number written out is checked only for what it can be: zero, or -1 with the lowest int.
        if matches!(rhs, Operand::Local(_) | Operand::Int(0)) {
            let zero = self.temp();
            self.line(&format!("{zero} = icmp eq i64 {b}, 0"));
            self.fail_if(&zero, DIVISION_BY_ZERO, span);
        }
        if matches!(rhs, Operand::Local(_) | Operand::Int(-1)) {
            let (minus_one, lowest, both) = (self.temp(), self.temp(), self.temp());
            self.line(&format!("{minus_one} = icmp eq i64 {b}, -1"));
            self.line(&format!("{lowest} = icmp eq i64 {a}, {}", i64::MIN));
            self.line(&format!("{both} = and i1 {minus_one}, {lowest}"));
            self.fail_if(&both, OVERFLOW, span);
        }
        // By a positive number written out, the machine's remainder has the sign of what's divided,
        // so it's below 0 exactly when the quotient rounded toward zero is one too many and the
        // remainder `b` too few: a shift of the remainder's sign fixes both, with no compare.
        if let Operand::Int(divisor @ 1..) = rhs {
            let (quotient, remainder, sign) = (self.temp(), self.temp(), self.temp());
            self.line(&format!("{quotient} = sdiv i64 {a}, {b}"));
            self.line(&format!("{remainder} = srem i64 {a}, {b}"));
            self.line(&format!("{sign} = ashr i64 {remainder}, 63"));
            let result = self.temp();
            if op == BinOp::FloorDiv {
                self.line(&format!("{result} = add i64 {quotient}, {sign}"));
            } else {
                let more = self.temp();
                self.line(&format!("{more} = and i64 {sign}, {divisor}"));
                self.line(&format!("{result} = add i64 {remainder}, {more}"));
                // The remainder is always from 0 up to the divisor: said to LLVM, a value worked
                // out from it again, as in `x = x * k % m`, needs no fix-up for a sign it can't have.
                let (low, high) = (self.temp(), self.temp());
                self.line(&format!("{low} = icmp sge i64 {result}, 0"));
                self.assumed(&low, "a remainder by a positive number isn't negative", span);
                self.line(&format!("{high} = icmp slt i64 {result}, {divisor}"));
                self.assumed(&high, "a remainder is below what it's by", span);
            }
            return result;
        }
        let (quotient, remainder, signs, differ, nonzero, adjust) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{quotient} = sdiv i64 {a}, {b}"));
        self.line(&format!("{remainder} = srem i64 {a}, {b}"));
        self.line(&format!("{signs} = xor i64 {a}, {b}"));
        self.line(&format!("{differ} = icmp slt i64 {signs}, 0"));
        self.line(&format!("{nonzero} = icmp ne i64 {remainder}, 0"));
        self.line(&format!("{adjust} = and i1 {differ}, {nonzero}"));
        let result = self.temp();
        if op == BinOp::FloorDiv {
            let one = self.temp();
            self.line(&format!("{one} = zext i1 {adjust} to i64"));
            self.line(&format!("{result} = sub i64 {quotient}, {one}"));
        } else {
            let more = self.temp();
            self.line(&format!("{more} = add i64 {remainder}, {b}"));
            self.line(&format!("{result} = select i1 {adjust}, i64 {more}, i64 {remainder}"));
        }
        result
    }

    /// A comparison of two floats, which have one equality and order everywhere: `0.0` is the same
    /// as `-0.0`, and NaN comes after everything and is the same as any other NaN. LLVM's own
    /// comparisons do this with a literal on one side, which is never NaN, and take a second look
    /// otherwise.
    pub(crate) fn float_compare(&mut self, op: BinOp, (a, b): (&str, &str)) -> String {
        let plain = |value: &str| {
            value
                .strip_prefix("0x")
                .and_then(|bits| u64::from_str_radix(bits, 16).ok())
                .is_some_and(|bits| !f64::from_bits(bits).is_nan())
        };
        let single = if plain(b) {
            Some(match op {
                BinOp::Eq => "oeq",
                BinOp::NotEq => "une",
                BinOp::Lt => "olt",
                BinOp::LtEq => "ole",
                BinOp::Gt => "ugt",
                _ => "uge",
            })
        } else if plain(a) {
            Some(match op {
                BinOp::Eq => "oeq",
                BinOp::NotEq => "une",
                BinOp::Lt => "ult",
                BinOp::LtEq => "ule",
                BinOp::Gt => "ogt",
                _ => "oge",
            })
        } else {
            None
        };
        if let Some(cc) = single {
            let temp = self.temp();
            self.line(&format!("{temp} = fcmp {cc} double {a}, {b}"));
            return temp;
        }
        let not = |emit: &mut Self, value: String| {
            let flipped = emit.temp();
            emit.line(&format!("{flipped} = xor i1 {value}, true"));
            flipped
        };
        match op {
            BinOp::Eq => self.floats_same(a, b),
            BinOp::NotEq => {
                let same = self.floats_same(a, b);
                not(self, same)
            }
            BinOp::Lt => self.float_less(a, b),
            BinOp::Gt => self.float_less(b, a),
            BinOp::LtEq => {
                let more = self.float_less(b, a);
                not(self, more)
            }
            _ => {
                let less = self.float_less(a, b);
                not(self, less)
            }
        }
    }

    /// Whether two floats are the same: equal as numbers, or both NaN.
    fn floats_same(&mut self, a: &str, b: &str) -> String {
        let (equal, a_nan, b_nan, both, same) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{equal} = fcmp oeq double {a}, {b}"));
        self.line(&format!("{a_nan} = fcmp uno double {a}, {a}"));
        self.line(&format!("{b_nan} = fcmp uno double {b}, {b}"));
        self.line(&format!("{both} = and i1 {a_nan}, {b_nan}"));
        self.line(&format!("{same} = or i1 {equal}, {both}"));
        same
    }

    /// Whether a float comes before another: less as a number, or before a NaN and not one itself.
    fn float_less(&mut self, a: &str, b: &str) -> String {
        let (less, a_number, both) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{less} = fcmp ult double {a}, {b}"));
        self.line(&format!("{a_number} = fcmp ord double {a}, {a}"));
        self.line(&format!("{both} = and i1 {less}, {a_number}"));
        both
    }

    pub(crate) fn float_binary(
        &mut self,
        op: BinOp,
        (a, b): (&str, &str),
        rhs: &Operand,
        span: Span,
    ) -> Result<String, String> {
        if matches!(op, BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq) {
            return Ok(self.float_compare(op, (a, b)));
        }
        let word = match op {
            BinOp::Add => "fadd",
            BinOp::Sub => "fsub",
            BinOp::Mul => "fmul",
            BinOp::Div | BinOp::FloorDiv | BinOp::Mod => {
                if !self.known.is_nonzero(rhs) {
                    let zero = self.temp();
                    self.line(&format!("{zero} = fcmp oeq double {b}, {}", float_text(0.0)));
                    self.fail_noted(&zero, DIVISION_BY_ZERO, span, Some(CheckKind::FloatDivideByZero));
                } else if self.checks_proofs() {
                    let zero = self.temp();
                    self.line(&format!("{zero} = fcmp oeq double {b}, {}", float_text(0.0)));
                    self.proof_held(&zero, "nonzero divisor", span);
                }
                if op == BinOp::Mod {
                    return Ok(self.float_remainder((a, b)));
                }
                if op == BinOp::FloorDiv {
                    return Ok(self.float_floor((a, b)));
                }
                "fdiv"
            }
            BinOp::Pow => {
                self.declare("declare double @fennec_float_power(double, double) nounwind");
                let temp = self.temp();
                self.line(&format!("{temp} = call double @fennec_float_power(double {a}, double {b})"));
                return Ok(temp);
            }
            other => return Err(format!("it uses {other:?} on floats, which this backend doesn't take yet")),
        };
        let temp = self.temp();
        self.line(&format!("{temp} = {word} double {a}, {b}"));
        Ok(temp)
    }

    /// `a // b` on floats, rounding toward negative infinity.
    fn float_floor(&mut self, (a, b): (&str, &str)) -> String {
        self.declare("declare double @llvm.floor.f64(double)");
        let (ratio, floor) = (self.temp(), self.temp());
        self.line(&format!("{ratio} = fdiv double {a}, {b}"));
        self.line(&format!("{floor} = call double @llvm.floor.f64(double {ratio})"));
        floor
    }

    /// `a % b` on floats: exact, with the sign of `b`. Worked out from `a / b` it would lose what's
    /// left once `a` is much bigger than `b`.
    fn float_remainder(&mut self, (a, b): (&str, &str)) -> String {
        let zero = float_text(0.0);
        let (left, some, left_below, b_below) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{left} = frem double {a}, {b}"));
        self.line(&format!("{some} = fcmp une double {left}, {zero}"));
        self.line(&format!("{left_below} = fcmp olt double {left}, {zero}"));
        self.line(&format!("{b_below} = fcmp olt double {b}, {zero}"));
        let (differ, shifts, moved, signed, rest) =
            (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{differ} = xor i1 {left_below}, {b_below}"));
        self.line(&format!("{shifts} = and i1 {some}, {differ}"));
        self.line(&format!("{moved} = fadd double {left}, {b}"));
        self.line(&format!("{signed} = select i1 {shifts}, double {moved}, double {left}"));
        // Nothing left is 0.0, whichever way it came.
        self.line(&format!("{rest} = select i1 {some}, double {signed}, double {zero}"));
        rest
    }

    fn call(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        if let ("str.isdigit", Some(dest), [text], true) = (name, dest, args, self.headers()) {
            return self.header_isdigit(dest, text);
        }
        if let (Some(dest), [a, b]) = (dest, args)
            && let Some(done) = self.sized_arithmetic(dest, name, (a, b))
        {
            return done;
        }
        if dest.is_none()
            && let Some(done) = self.moving_list_method(name, args, span)
        {
            return done;
        }
        // `int.parse(text)` and `float.parse(text)` are `int(text)?` and `float(text)?` given back:
        // the result is made where the caller keeps it, rather than made, read and made again.
        if let ("int.parse" | "float.parse", Some(dest), [text @ Operand::Local(held)]) = (name, dest, args)
            && self.function.locals.get(held.0).is_some_and(|local| local.ty == fennec_check::Type::Str)
            && self.kind_of(dest).is_ok_and(|kind| self.case_of(&kind.clone(), "Ok").is_ok())
        {
            let number = if name == "int.parse" { "int" } else { "float" };
            return self.number_of_text(dest, number, text);
        }
        if let ("str.startswith" | "str.endswith", Some(dest), [text, part], true) =
            (name, dest, args, self.headers())
        {
            return self.header_affix(dest, (text, part), name == "str.endswith");
        }
        // What `fennec test` calls before each input of a property test with capabilities: a new
        // world of fakes, with nothing left by the input before.
        if name == "test:world" && args.is_empty() {
            self.declare("declare void @fennec_aot_test_world(i64)");
            self.line("call void @fennec_aot_test_world(i64 1)");
            return Ok(());
        }
        // Whether the attempt of a property test's shrinking just made passed.
        if let ("test:passed", Some(dest), []) = (name, dest, args) {
            self.declare("declare i8 @fennec_aot_attempt_passed()");
            let (byte, passed) = (self.temp(), self.temp());
            self.line(&format!("{byte} = call i8 @fennec_aot_attempt_passed()"));
            self.line(&format!("{passed} = icmp ne i8 {byte}, 0"));
            return self.assign(dest, &passed);
        }
        // What the property test failing next was given, for its report.
        if let ("test:failed_for", [text]) = (name, args) {
            let (text, _) = self.operand(text)?;
            return self.on_blocks([&text], |emit, [block]| {
                emit.declare("declare void @fennec_aot_failed_for(ptr)");
                emit.line(&format!("call void @fennec_aot_failed_for(ptr {block})"));
                Ok(())
            });
        }
        let callee = self
            .module
            .reads
            .function(name)
            .ok_or_else(|| format!("it calls `{name}`, which isn't lowered"))?;
        let ret = self.module.ret_text(callee)?;
        if !self.module.defined.contains(name) {
            let params = self.module.params(callee, false)?;
            self.declare(&format!("declare swiftcc {ret} {}({params}) #0", symbol(name)));
        }
        let returns = self.module.kind(&callee.returns)?;
        let mut shown = Vec::with_capacity(args.len() + 2);
        let mut lent = Vec::new();
        let mut borrows_lent = Vec::new();
        let returned_at = if returns.in_memory() {
            // What nothing here keeps, like a constant `main` works out ahead in a program with
            // `parallel`, has no local to have its type defined for.
            self.module.define_types(&returns);
            let at = self.label("%ret");
            let text = self.text_of(&returns);
            let _ = writeln!(self.allocas, "  {at} = alloca {text}");
            shown.push(format!("ptr {at}"));
            Some(at)
        } else {
            None
        };
        for (at, arg) in args.iter().enumerate() {
            if callee.mut_params.contains(&at) {
                let Operand::Local(local) = arg else {
                    return Err("it passes a literal for a `mut` parameter".to_owned());
                };
                shown.push(format!("ptr {}", self.slot(*local)));
                continue;
            }
            let param = callee.locals.get(at).ok_or("too many arguments")?;
            let kind = self.module.kind(&param.ty)?;
            let (value, fresh) = self.value_for(arg, &kind)?;
            if fresh {
                lent.push((value.clone(), kind.clone()));
            }
            // Checking proofs, a borrowed block lent to a call is held for it too, to see after
            // that what it was borrowed from still holds it.
            if self.checks_proofs()
                && matches!(arg, Operand::Local(local) if self.known.is_borrowed(*local))
                && matches!(kind, Kind::List(_) | Kind::Dict(..) | Kind::Boxed(_) | Kind::TextBlock)
            {
                borrows_lent.push((value.clone(), kind.clone()));
            }
            if kind.in_memory() {
                shown.push(format!("ptr {value}"));
            } else {
                shown.push(format!("{} {value}", self.text_of(&kind)));
            }
        }
        let flagged = self.module.flagged(name);
        if flagged {
            shown.push("ptr swifterror %err".to_owned());
        }
        let shown = shown.join(", ");
        let target = if name == self.function.name {
            self.self_target.map_or_else(|| symbol(name), str::to_owned)
        } else {
            symbol(name)
        };
        // A copy is inlined, and calls only other copies, so it can't go deeper.
        if name == self.function.name || !self.module.defined.contains(name) {
            self.check_stack(span);
        }
        for (borrowed, _) in &borrows_lent {
            self.when_made(borrowed, |emit| emit.retain(borrowed));
        }
        let result = self.temp();
        if ret == "void" {
            self.line(&format!("call swiftcc void {target}({shown})"));
        } else {
            self.line(&format!("{result} = call swiftcc {ret} {target}({shown})"));
        }
        for (borrowed, kind) in &borrows_lent {
            self.when_made(borrowed, |emit| {
                // Its one owner now is the one taken for the call: what it was borrowed from let
                // go of it while the call ran, and the borrow was read after.
                let alone = emit.alone(borrowed);
                emit.proof_held(
                    &alone,
                    "what a value lent to a call was borrowed from holds it till the call ends",
                    span,
                );
                emit.release_block(borrowed, kind);
            });
        }
        if flagged && is_attempt(&self.function.name) {
            // An attempt of a property test's shrinking finds out whether the test fails, so a
            // failure is cleared, as one test's is before the next, and the attempt says it
            // failed at once. What the call gave back was never written, so nothing may look at
            // it, and the attempt owns nothing yet: the call is its first. Built in place of `with
            // launch`, which the test entry can use once it's emitted.
            let (held, failed) = (self.temp(), self.temp());
            self.line(&format!("{held} = load ptr, ptr %err"));
            self.line(&format!("{failed} = icmp ne ptr {held}, null"));
            let (caught, after) = (self.label("attempt_failed"), self.label("attempt_done"));
            self.line(&format!("br i1 {failed}, label %{caught}, label %{after}"));
            self.start_block(&caught);
            self.declare("declare void @fennec_aot_attempt_failed()");
            self.declare("declare i8 @fennec_aot_attempt_passed()");
            self.line("store ptr null, ptr %err");
            self.line("call void @fennec_aot_attempt_failed()");
            // Taken back, so the next attempt starts afresh, as `test:passed` would have.
            self.line("call i8 @fennec_aot_attempt_passed()");
            let ret = self.module.ret_text(self.function)?;
            self.line(&format!("ret {ret} 0"));
            self.start_block(&after);
        } else if flagged {
            let (held, failed) = (self.temp(), self.temp());
            self.line(&format!("{held} = load ptr, ptr %err"));
            self.line(&format!("{failed} = icmp ne ptr {held}, null"));
            if !span.in_prelude() && written_in_std(callee) {
                self.leave_pointing_here(&failed, span);
            } else {
                self.leave_traced(&failed, span);
            }
        }
        for (block, kind) in lent {
            self.release_block(&block, &kind);
        }
        match (dest, returned_at) {
            (Some(dest), Some(at)) => self.assign_memory_owned(dest, &at)?,
            (None, Some(at)) => self.release_inline(&returns, &at),
            (Some(dest), None) if ret != "void" => {
                self.assign(dest, &result)?;
                self.alone_from_here(dest);
            }
            (None, None) if returns.counted() => self.release_kind(&result, &returns),
            _ => {}
        }
        Ok(())
    }

    /// A builtin, and whether it changes what it's given first, as the IR says.
    fn builtin(
        &mut self,
        dest: Option<LocalId>,
        (name, changes): (&str, bool),
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        if let Some(done) = self.conversion(dest, name, args, span) {
            return done;
        }
        if let Some(done) = self.loop_builtin(dest, name, args, span) {
            return done;
        }
        let first = args.first();
        match name {
            // A test's `Stdout` keeps what's printed, which the runtime does as it prints.
            "Stdout.print" => self.print(args.get(1..).unwrap_or_default()),
            "Stdout.write" => self.write(args.get(1..).unwrap_or_default()),
            "array.new" => self.new_array(dest.ok_or("an array made for nothing")?, args),
            "list.len" => {
                if let Some(Operand::Local(local)) = first
                    && let Some(map) = self.known.map_walks.get(&local.0).copied()
                {
                    let live = self.walked_live(map)?;
                    return self.assign(dest.ok_or("no result")?, &live);
                }
                // An `enumerate` never made is as long as its list.
                let counted = match first {
                    Some(Operand::Local(local)) => {
                        self.known.enumerated.get(&local.0).map(|list| Operand::Local(*list))
                    }
                    _ => None,
                };
                let looked_at = counted.as_ref().or(first).ok_or("no argument")?;
                let (list, _) = self.operand(looked_at)?;
                let from = match looked_at {
                    Operand::Local(local) => Some(*local),
                    _ => None,
                };
                let length = self.length_of_list(from, &list);
                self.assign(dest.ok_or("no result")?, &length)
            }
            "list.append" => self.append(args),
            "result.expect" => self.expect(dest, args, span),
            "str.byte_len" | "str.char_at_byte" | "str.is_ascii" => self.text_by_bytes(dest, name, args),
            // A place in a string's bytes and a character's length there: the next place, which is
            // never past the text's end, so it can't overflow.
            "str.next_place" => {
                let (at, _) = self.operand(first.ok_or("no place")?)?;
                let (step, _) = self.operand(args.get(1).ok_or("no step")?)?;
                let next = self.temp();
                self.line(&format!("{next} = add nuw nsw i64 {at}, {step}"));
                self.assign(dest.ok_or("no result")?, &next)
            }
            "intrinsics.text_byte"
            | "intrinsics.text_size"
            | "intrinsics.text_part"
            | "intrinsics.text_of_code" => self.text_by_place(dest, name, args, span),
            "intrinsics.id_key"
            | "intrinsics.id_at"
            | "intrinsics.id_tag"
            | "intrinsics.new_tag"
            | "intrinsics.table_slots"
            | "intrinsics.table_slot_tags"
            | "intrinsics.table_of_slots"
            | "intrinsics.id_of_key" => self.id_intrinsic(dest, name, args),
            "intrinsics.bytes_text" => self.bytes_text(dest.ok_or("no result")?, args),
            "intrinsics.bytes_builder"
            | "intrinsics.bytes_push"
            | "intrinsics.bytes_push_word"
            | "intrinsics.bytes_push_varint"
            | "intrinsics.bytes_push_text"
            | "intrinsics.bytes_push_bytes"
            | "intrinsics.bytes_set" => self.byte_builder(dest, name, args),
            "intrinsics.text_plain_end" | "intrinsics.text_escape_at" => {
                self.text_scan(dest.ok_or("no result")?, name, args)
            }
            "intrinsics.text_same_at" => self.text_same_at(dest.ok_or("no result")?, args),
            // A float's bits as an int, and back, which a binary format saves.
            "intrinsics.float_bits" | "intrinsics.float_of_bits" => {
                let (value, _) = self.operand(first.ok_or("no argument")?)?;
                let (from, to) =
                    if name == "intrinsics.float_bits" { ("double", "i64") } else { ("i64", "double") };
                let cast = self.temp();
                self.line(&format!("{cast} = bitcast {from} {value} to {to}"));
                self.assign(dest.ok_or("no result")?, &cast)
            }
            // A line on standard error, which can't fail: the string's block is made for the call
            // when its text is held in its header, and let go of after.
            "intrinsics.report" => {
                let (value, kind) = self.operand(first.ok_or("no argument")?)?;
                let made = kind == Kind::Str && self.headers();
                let block = if made { self.owned_text_block(&value) } else { value };
                self.declare("declare void @fennec_report(ptr) nounwind");
                self.line(&format!("call void @fennec_report(ptr {block})"));
                if made {
                    self.release_quietly(&block);
                }
                Ok(())
            }
            "intrinsics.stop" => {
                let (value, kind) = self.operand(first.ok_or("no argument")?)?;
                let made = kind == Kind::Str && self.headers();
                let block = if made { self.owned_text_block(&value) } else { value };
                self.declare("declare ptr @fennec_block_text(ptr) nounwind");
                let text = self.temp();
                self.line(&format!("{text} = call ptr @fennec_block_text(ptr {block})"));
                // The runtime copies the message, so the blocks made for it are let go of before
                // leaving: a stop in a launched task ends only that task, and the program goes on.
                self.stop_letting_go(&text, span, |emit| {
                    emit.release_quietly(&text);
                    if made {
                        emit.release_quietly(&block);
                    }
                });
                Ok(())
            }
            "random.random_int" | "random.random_float" | "random_choice" | "random.random_choice" => {
                self.random_builtin(dest, name, args, span)
            }
            parallel if parallel.starts_with("parallel.") => {
                self.parallel_builtin(dest, (name, changes), args, span)
            }
            channel if channel.starts_with("intrinsics.channel_") => self.channel_builtin(dest, name, args),
            "intrinsics.launch" | "intrinsics.launch_take" => self.launch_builtin(dest, name, args),
            "error.add_note" | "error.notes" => self.notes_builtin(dest, name, args),
            "error.raised" | "error.passed" => self.error_frame(name, args, span),
            "lock.roll_back" => {
                let [handle] = args else { return Err("a lock to put back that isn't one".to_owned()) };
                self.roll_back_on_failure(handle)
            }
            // A list of a list's items is the list, shared until one changes.
            "parallel" | "list"
                if args.len() == 1
                    && matches!(args.first(), Some(Operand::Local(from)) if matches!(self.kinds.get(from.0), Some(Kind::List(_)))) =>
            {
                let from = args.first().ok_or("no argument")?;
                self.copy(dest.ok_or("no result")?, from)
            }
            "Table" | "Table.len" | "Table.contains" | "Table.get" | "Table.add" | "Table.remove"
            | "Table.values" | "Table.ids" | "Table.items" | "Table.remove_where" | "Table.clear"
            | "Table.modify" | "Table.slots" | "Table.slot_live" | "Table.at_slot" | "Table.slot_id" => {
                self.id_map_builtin(dest, name, args, span)
            }
            "is_regex" | "re.is_regex" | "re.pattern_problem" | "Regex.replace" | "Regex.split"
            | "Regex.find_all" | "Regex.search" | "Regex.match" | "Regex.fullmatch" => {
                self.regex_builtin(dest, name, args, span)
            }
            "json.dumps" | "json.loads" | "toml.dumps" | "toml.loads" => {
                Err(crate::json::not_saved_natively(name))
            }
            named
                if ["json.dumps.", "json.loads.", "toml.dumps.", "toml.loads."]
                    .iter()
                    .any(|format| named.starts_with(format)) =>
            {
                Err(crate::json::not_saved_natively(name))
            }
            pure if pure.starts_with("crypto.")
                || pure.starts_with("compress.")
                || pure.starts_with("unicode.") =>
            {
                self.crypto_call(dest, name, args, span)
            }
            "error.with_cause" => {
                // Nothing reads what caused an error yet, so the wrapper is all there is.
                let wrapper = first.ok_or("no argument")?;
                self.copy(dest.ok_or("no result")?, wrapper)
            }
            buffer if buffer.starts_with("Buffer.") => self.buffer_builtin(dest, name, args, span),
            "str.len" | "str" | "repr" | "str.code" | "str.join" | "str.contains" | "str.upper"
            | "str.lower" | "str.replace" | "str.find" | "str.split" | "str.split_at" => {
                self.text_builtin(dest, name, args, span)
            }
            "float" => {
                let (value, kind) = self.operand(first.ok_or("no argument")?)?;
                let temp = self.temp();
                if kind == Kind::Int {
                    self.line(&format!("{temp} = sitofp i64 {value} to double"));
                } else {
                    return Err("it calls `float` on something other than an int".to_owned());
                }
                self.assign(dest.ok_or("no result")?, &temp)
            }
            "math.sqrt" => {
                let (value, kind) = self.operand(first.ok_or("no argument")?)?;
                let value = if kind == Kind::Int {
                    let wide = self.temp();
                    self.line(&format!("{wide} = sitofp i64 {value} to double"));
                    wide
                } else {
                    value
                };
                let negative = self.temp();
                self.line(&format!("{negative} = fcmp olt double {value}, {}", float_text(0.0)));
                self.fail_if(&negative, NEGATIVE_SQRT, span);
                let temp = self.temp();
                if kind == Kind::F32 {
                    // An `f32`'s square root, which IEEE makes exact, as the `f64` holding it.
                    let (narrow, root) = (self.temp(), self.temp());
                    self.declare("declare float @llvm.sqrt.f32(float)");
                    self.line(&format!("{narrow} = fptrunc double {value} to float"));
                    self.line(&format!("{root} = call float @llvm.sqrt.f32(float {narrow})"));
                    self.line(&format!("{temp} = fpext float {root} to double"));
                } else {
                    self.declare("declare double @llvm.sqrt.f64(double)");
                    self.line(&format!("{temp} = call double @llvm.sqrt.f64(double {value})"));
                }
                self.assign(dest.ok_or("no result")?, &temp)
            }
            "int.abs" => {
                let (value, _) = self.operand(first.ok_or("no argument")?)?;
                let lowest = self.temp();
                self.line(&format!("{lowest} = icmp eq i64 {value}, {}", i64::MIN));
                self.fail_if(&lowest, OVERFLOW, span);
                let (negative, flipped, temp) = (self.temp(), self.temp(), self.temp());
                self.line(&format!("{negative} = icmp slt i64 {value}, 0"));
                self.line(&format!("{flipped} = sub i64 0, {value}"));
                self.line(&format!("{temp} = select i1 {negative}, i64 {flipped}, i64 {value}"));
                self.assign(dest.ok_or("no result")?, &temp)
            }
            // A capability's method, run by the runtime.
            other
                if other.contains('.')
                    && matches!(first, Some(Operand::Local(local)) if self.kind_of(*local).ok() == Some(&Kind::Cap)) =>
            {
                self.capability_call(dest, other, args, span)
            }
            "Response.text" => match args {
                [response] => self.response_text(dest.ok_or("no result")?, response),
                _ => Err("`text` takes nothing".to_owned()),
            },
            // In a test, the runtime hands a socket's method to the fake network it holds.
            other if crate::analysis::is_socket_method(other) => self.socket_call(dest, other, args, span),
            other => Err(format!("it calls `{other}`, which this backend doesn't take yet")),
        }
    }

    /// `dest = value`: a block gets another owner, unless nothing else ever reads `value`.
    pub(crate) fn copy(&mut self, dest: LocalId, value: &Operand) -> Result<(), String> {
        // `x = x` changes nothing. Written as a copy, it would let go of what `x` held before
        // taking it again, or hand it over and clear `x`.
        if *value == Operand::Local(dest) {
            return Ok(());
        }
        // The local is the list, so there's nothing to copy, or it's an `enumerate` never made.
        if self.known.alias(dest).is_some()
            || self.known.enumerated.contains_key(&dest.0)
            || self.known.map_walks.contains_key(&dest.0)
        {
            return Ok(());
        }
        if *value == Operand::None {
            let kind = self.kind_of(dest)?.clone();
            let (held, _) = self.value_for(value, &kind)?;
            if kind.in_memory() {
                return self.assign_memory(dest, &held, "");
            }
            return self.assign(dest, &held);
        }
        // Between an optional and what it holds, the IR can write a plain copy.
        let wanted = self.kind_of(dest)?.clone();
        let from = match value {
            Operand::Local(local) => self.kind_of(*local)?.clone(),
            Operand::Int(_) => Kind::Int,
            Operand::Float(_) => Kind::Float,
            Operand::Bool(_) => Kind::Bool,
            Operand::Str(_) => Kind::Str,
            Operand::Bytes(_) => Kind::Bytes,
            Operand::None => wanted.clone(),
        };
        if from != wanted && self.headers() {
            let as_block = |kind: &Kind| *kind == Kind::TextBlock;
            // A string held as a block, as a dict holds one, and as a header.
            if as_block(&from) && matches!(wanted, Kind::Str | Kind::MaybeText) {
                let (held, _) = self.operand(value)?;
                let header = self.text_from_borrowed_block(&held);
                return self.assign(dest, &header);
            }
            if from == Kind::Str && as_block(&wanted) {
                let (held, _) = self.operand(value)?;
                let block = self.owned_text_block(&held);
                return self.assign(dest, &block);
            }
        }
        if from != wanted {
            if self.widens(&from, &wanted) {
                let (held, _) = self.operand(value)?;
                let made = self.widened(&held, &from, &wanted)?;
                if wanted.in_memory() {
                    return self.assign_memory_owned(dest, &made);
                }
                return self.assign(dest, &made);
            }
            // An optional of an optional is optional both ways, so what it holds says which: read
            // out of `from` when that's what it holds, else made from what `wanted` holds.
            let unwraps = self.some_kind(&from).as_ref() == Some(&wanted);
            if self.is_optional(&wanted) && !unwraps {
                return self.make_case(dest, "Some", std::slice::from_ref(value));
            }
            if self.is_optional(&from) {
                return self.get_case_field(dest, value, "Some", "value");
            }
        }
        let (held, kind) = self.operand(value)?;
        if kind.in_memory() {
            // The last use of an inline struct's local hands its blocks over.
            if let Operand::Local(from) = value
                && self.owns_inline(*from)
                && (self.known.moves(*from) || self.known.is_last_copy(self.at.0, self.at.1))
            {
                self.assign_memory_owned(dest, &held)?;
                let zero = self.zero_line(&kind, &held);
                self.line(&zero);
                return Ok(());
            }
            return self.assign_memory(dest, &held, "");
        }
        // A borrowing local takes no owner, so its copy needs none either. Nor does an item taken
        // out, handed from the local that took it to the name it's changed under.
        let handed_over =
            self.known.is_taken(dest) && matches!(value, Operand::Local(from) if self.known.is_taken(*from));
        if kind.counted() && !self.known.is_borrowed(dest) && !handed_over {
            match value {
                Operand::Local(from)
                    if self.owns(*from)
                        && (self.known.moves(*from) || self.known.is_last_copy(self.at.0, self.at.1)) =>
                {
                    let (slot, text) = (self.slot(*from), self.text_of(&kind));
                    self.line(&format!("store {text} zeroinitializer, ptr {slot}"));
                }
                _ => self.retain_value(&held, &kind),
            }
        }
        self.assign(dest, &held)?;
        self.alone_from_here(dest);
        Ok(())
    }

    /// A list that came from somewhere else gets its one check here.
    fn alone_from_here(&mut self, dest: LocalId) {
        if self.kinds.get(dest.0).is_some_and(|kind| matches!(kind, Kind::List(_)))
            && self.known.alone_once_checked(dest)
        {
            self.check_alone(dest);
        }
    }

    /// Says where this is, in a build that traces what things cost, if what's here can cost.
    fn trace_site(&mut self, inst: &InstKind, span: Span) {
        if !self.module.tracing_costs() {
            return;
        }
        let Some(what) = what_can_cost(inst) else { return };
        self.module.add_site(span.start, what);
        self.declare("declare void @fennec_trace_at(i64) nounwind");
        self.line(&format!("call void @fennec_trace_at(i64 {})", span.start));
    }

    fn inst(&mut self, inst: &InstKind, span: Span, block: usize) -> Result<(), String> {
        self.trace_site(inst, span);
        if let Some(done) = self.place_inst(inst, span) {
            return done;
        }
        match inst {
            // A union is laid out as an enum with a case for each member, named for it.
            InstKind::MakeMember { dest, value, member } => {
                let case = self.member_case(&Operand::Local(*dest), *member)?;
                self.make_case(*dest, &case, std::slice::from_ref(value))
            }
            InstKind::IsMember { dest, value, member } => {
                let case = self.member_case(value, *member)?;
                self.is_case(*dest, value, &case)
            }
            InstKind::GetMember { dest, value, member } => {
                let case = self.member_case(value, *member)?;
                self.get_case_field(*dest, value, &case, "value")
            }
            // Used only as a key the runtime finds where it is.
            InstKind::Slice { dest, .. } | InstKind::Copy { dest, .. }
                if self.known.unmade.contains(&dest.0) =>
            {
                Ok(())
            }
            // An empty list copied into one a loop right after fills: that one is made at the
            // block's end, once the loop's count is known, and the empty one never is.
            InstKind::Copy { dest, value: Operand::Local(from) }
                if self
                    .reserved_at(self.at.0)
                    .iter()
                    .any(|one| one.list == dest.0 && one.copied_from == Some(from.0)) =>
            {
                Ok(())
            }
            InstKind::Copy { dest, value }
            | InstKind::MakeSome { dest, value }
            | InstKind::GetSome { dest, value } => self.copy(*dest, value),
            InstKind::Unary { dest, op, value } => {
                let (value, kind) = self.operand(value)?;
                let result = match (op, kind) {
                    (UnaryOp::Neg, Kind::Int) if self.known.is_safe(*dest) && self.checks_proofs() => {
                        self.proven_unchecked("ssub", ("0", &value), span)
                    }
                    (UnaryOp::Neg, Kind::Int) if self.known.is_safe(*dest) => {
                        let temp = self.temp();
                        self.line(&format!("{temp} = sub nsw i64 0, {value}"));
                        temp
                    }
                    (UnaryOp::Neg, Kind::Int) => self.checked("ssub", "0", &value, span),
                    // Negating an `f32` gives an `f32` exactly.
                    (UnaryOp::Neg, Kind::Float | Kind::F32) => {
                        let temp = self.temp();
                        self.line(&format!("{temp} = fneg double {value}"));
                        temp
                    }
                    // An unsigned size's bits flip within its width, so it stays in range.
                    (UnaryOp::BitNot, Kind::Int)
                        if let Some(num) = self.sized(*dest)
                            && num.is_unsigned() =>
                    {
                        let (_, most) = num.range().unwrap_or((0, 0));
                        let temp = self.temp();
                        self.line(&format!("{temp} = xor i64 {value}, {most}"));
                        temp
                    }
                    (UnaryOp::BitNot, Kind::Int | Kind::U64) => {
                        let temp = self.temp();
                        self.line(&format!("{temp} = xor i64 {value}, -1"));
                        temp
                    }
                    (UnaryOp::Not, Kind::Bool) => {
                        let temp = self.temp();
                        self.line(&format!("{temp} = xor i1 {value}, true"));
                        temp
                    }
                    (_, other) => return Err(format!("it uses a unary operator on {other:?}")),
                };
                if *op == UnaryOp::Neg {
                    self.check_size(*dest, &result, span);
                }
                self.assign(*dest, &result)
            }
            InstKind::Binary { dest, op, lhs, rhs } => self.binary(*dest, *op, lhs, rhs, span),
            InstKind::Make { dest, fields, .. } => self.make(*dest, fields),
            InstKind::MakeDict { dest, pairs } => self.make_dict(*dest, pairs),
            InstKind::MakeSet { dest, items } => self.make_set(*dest, items),
            InstKind::MakeClosure { dest, function, captures } => {
                self.make_closure(*dest, function, captures)
            }
            InstKind::CallClosure { dest, closure, args } => self.call_closure(*dest, closure, args, span),
            InstKind::Constant { dest, name } => self.call(Some(*dest), name, &[], span),
            InstKind::MakeCase { dest, case, fields, .. } => self.make_case(*dest, case, fields),
            InstKind::IsCase { dest, value, case } => self.is_case(*dest, value, case),
            InstKind::GetCaseField { dest, value, case, field } => {
                self.get_case_field(*dest, value, case, field)
            }

            InstKind::IsNone { dest, value } => self.is_case(*dest, value, "None"),
            InstKind::MakeOk { dest, value } => self.make_case(*dest, "Ok", value.as_slice()),
            InstKind::MakeErr { dest, error } => self.make_case(*dest, "Err", std::slice::from_ref(error)),
            InstKind::IsErr { dest, value } => self.is_case(*dest, value, "Err"),
            InstKind::GetOk { dest, value } => {
                self.get_case_field(*dest, value, "Ok", "value")?;
                self.let_go_after_last_read(*dest, value)
            }
            InstKind::GetErr { dest, value } => {
                self.get_case_field(*dest, value, "Err", "error")?;
                self.let_go_after_last_read(*dest, value)
            }
            InstKind::IsError { dest, value, type_name } => self.is_case(*dest, value, type_name),
            InstKind::AsError { dest, value } => self.as_error(*dest, value),
            InstKind::GetField { dest, object, field } => self.get_field(*dest, object, field),
            InstKind::SetField { object, path, value } => self.set_field(*object, path, value),
            InstKind::MakeTuple { dest, items } => self.make(*dest, items),
            // A pair a walk over a map's items never made: its parts are read from the entry. The
            // item of a walk over keys or values alone is a value like any other, and one that's
            // a tuple has parts of its own.
            InstKind::GetTupleItem { dest, value: Operand::Local(pair), at }
                if self.known.walk_items.get(&pair.0).is_some_and(|(_, pairs)| *pairs) =>
            {
                self.walked_part(*dest, *pair, *at)
            }
            // A pair never made: its count is the index, and its item is the list's there.
            InstKind::GetTupleItem { dest, value: Operand::Local(pair), at }
                if self.known.pairs.contains_key(&pair.0) =>
            {
                let (list, _) = self.known.pairs.get(&pair.0).cloned().ok_or("no pair")?;
                let slot = self.pair_slots.get(&pair.0).cloned().ok_or("a pair read before it's made")?;
                let position = self.temp();
                self.line(&format!("{position} = load i64, ptr {slot}"));
                if *at == 0 {
                    return self.assign(*dest, &position);
                }
                let (list, kind) = self.operand(&Operand::Local(list))?;
                let Kind::List(item) = kind else {
                    return Err("an `enumerate` of something else".to_owned());
                };
                let held = self.item_kind(item)?;
                let memory = held.memory(self.module.layouts);
                let place = self.item_address(&list, &memory, &position);
                self.read_into(*dest, &held, &place, ITEM_ACCESS)
            }
            InstKind::GetTupleItem { dest, value, at } => self.get_field(*dest, value, &format!("_{at}")),
            InstKind::MakeList { dest, items } => self.make_list(*dest, items, block),
            InstKind::ConstantList { dest, items } => self.constant_list(*dest, items, block),
            InstKind::GetIndex { dest, object, index } => self.get_index(*dest, object, index, span),
            InstKind::SetIndex { object, path, index, value } => {
                self.set_index(*object, path, index, value, span)
            }
            InstKind::Slice { dest, object, start, end } => {
                self.slice(*dest, object, start.as_ref(), end.as_ref(), span)
            }
            InstKind::Format { dest, parts } => self.format(*dest, parts, span),
            InstKind::Builtin { dest, name, args, changes } => {
                // A value of a `lock ... as ref` block changed in place is saved first.
                if *changes
                    && !self.saving.is_empty()
                    && let Some(changed) = args.first()
                {
                    self.save_before_change(changed)?;
                }
                self.builtin(*dest, (name, *changes), args, span)
            }
            InstKind::Call { dest, function, args } => self.call(*dest, function, args, span),
            InstKind::MakeGenerator { dest, generator, args } => self.make_generator(*dest, generator, args),
            InstKind::GetSlot { dest, generator, slot } => self.get_slot(*dest, generator, *slot),
            InstKind::SetSlot { generator, slot, value } => self.set_slot(generator, *slot, value),
            InstKind::Resume { dest, generator, known } => {
                self.resume(*dest, generator, known.as_deref(), span)
            }
            InstKind::Lock { handle, write } => self.lock(handle, *write, span),
            InstKind::Unlock { handle } => self.unlock(handle),
            // A crash doesn't run a `with` source's `else:` natively yet.
            InstKind::CrashCleanup { cleanup } => {
                self.crash_cleanup(cleanup, true);
                Ok(())
            }
            InstKind::CrashCleanupDone { cleanup } => {
                self.crash_cleanup(cleanup, false);
                Ok(())
            }
            InstKind::LockAll { handles } => self.lock_all(handles, span),
            InstKind::UnlockAll { handles } => self.unlock_all(handles),
        }
    }

    /// An f-string, or `str(value)`, whose one use is the append right after it onto pieces kept
    /// as the string they're joined into: those pieces, and the parts to put on.
    fn appended_parts(&self, inst: &InstKind, next: Option<&InstKind>) -> Option<(LocalId, Vec<FormatPart>)> {
        let (made, parts) = match inst {
            InstKind::Format { dest, parts } => (*dest, parts.clone()),
            InstKind::Builtin { dest: Some(dest), name, args, .. } if name == "str" => {
                let [value] = args.as_slice() else { return None };
                (*dest, vec![FormatPart::Value { value: value.clone(), spec: None }])
            }
            _ => return None,
        };
        let Some(InstKind::Builtin { name, args, .. }) = next else { return None };
        let [Operand::Local(joined), Operand::Local(piece)] = args.as_slice() else { return None };
        (name == "list.append"
            && *piece == made
            && self.known.used_once(made)
            && matches!(self.kinds.get(joined.0), Some(Kind::Str | Kind::TextBlock)))
        .then_some((*joined, parts))
    }

    /// An f-string whose one use is `text += ` it right after, onto a text that grows where it is
    /// (as [`Self::grows_in_place`] says): the text, the f-string's local, and its parts. Only
    /// parts that are read where they are, a literal or a string, an int or a float with no
    /// spec, so working them out can't fail with some already written on.
    fn format_added(
        &self,
        inst: &InstKind,
        next: Option<&InstKind>,
    ) -> Result<Option<(LocalId, LocalId, Vec<FormatPart>)>, String> {
        let InstKind::Format { dest: made, parts } = inst else { return Ok(None) };
        let Some(InstKind::Binary { dest, op: BinOp::Add, lhs, rhs: Operand::Local(added) }) = next else {
            return Ok(None);
        };
        // A field taken out only just before the `+=` isn't in its local yet.
        if added != made
            || self.known.path_due((self.at.0, self.at.1 + 1)).is_some()
            || !self.headers()
            || !self.known.used_once(*made)
            || !matches!(self.kinds.get(dest.0), Some(Kind::Str))
            || !self.grows_in_place(BinOp::Add, lhs, *dest)
        {
            return Ok(None);
        }
        for part in parts {
            if let FormatPart::Value { value, spec } = part
                && (spec.is_some()
                    || !matches!(self.operand_kind(value)?, Kind::Str | Kind::Int | Kind::Float))
            {
                return Ok(None);
            }
        }
        Ok(Some((*dest, *made, parts.clone())))
    }

    /// A list or set written out of constant strings or ints, which only the `in` right after reads
    /// (`not in` is that, then `not`): the `in`'s local, which way it asks, what it looks for, and
    /// the constants.
    fn literals_tested(
        &self,
        inst: &InstKind,
        next: Option<&InstKind>,
    ) -> Option<(LocalId, BinOp, Operand, Vec<Operand>)> {
        let (InstKind::MakeList { dest: made, items } | InstKind::MakeSet { dest: made, items }) = inst
        else {
            return None;
        };
        let Some(InstKind::Builtin { dest: Some(dest), name, args, .. }) = next else { return None };
        let [Operand::Local(looked_in), lhs @ Operand::Local(wanted)] = args.as_slice() else { return None };
        if !matches!(name.as_str(), "list.contains" | "set.contains")
            || looked_in != made
            || !self.known.used_once(*made)
        {
            return None;
        }
        let strings = items.iter().all(|item| matches!(item, Operand::Str(_)));
        let ints = items.iter().all(|item| matches!(item, Operand::Int(_)));
        let fits = match self.kinds.get(wanted.0) {
            Some(Kind::Str) => strings,
            Some(Kind::Int) => ints,
            _ => false,
        };
        (fits && (1..=64).contains(&items.len())).then(|| (*dest, BinOp::In, lhs.clone(), items.clone()))
    }

    /// `wanted in [constants]`, or `not in`, as a compare with each: no list or set is made.
    fn in_literals(
        &mut self,
        dest: LocalId,
        op: BinOp,
        wanted: &Operand,
        items: &[Operand],
    ) -> Result<(), String> {
        let (a, _) = self.operand(wanted)?;
        let mut found = "false".to_owned();
        for item in items {
            let same = match item {
                // As `==` on two strings is, by the way strings are held.
                Operand::Str(_) if self.headers() => {
                    let (b, _) = self.operand(item)?;
                    self.header_binary(BinOp::Eq, (&a, &b))?
                }
                Operand::Str(literal) if literal.len() <= 8 => self.same_as_short(BinOp::Eq, &a, literal),
                Operand::Str(_) => {
                    let (b, _) = self.operand(item)?;
                    self.string_binary(BinOp::Eq, (&a, &b))?
                }
                Operand::Int(number) => {
                    let same = self.temp();
                    self.line(&format!("{same} = icmp eq i64 {a}, {number}"));
                    same
                }
                _ => return Err("`in` on constants that aren't strings or ints".to_owned()),
            };
            let either = self.temp();
            self.line(&format!("{either} = or i1 {found}, {same}"));
            found = either;
        }
        if op == BinOp::NotIn {
            let missing = self.temp();
            self.line(&format!("{missing} = xor i1 {found}, true"));
            found = missing;
        }
        self.assign(dest, &found)
    }

    /// Lookups only stay good across instructions that leave dicts alone and don't change the
    /// dict's or the key's local.
    fn forget_entries(&mut self, inst: &InstKind) {
        if self.found_entries.is_empty() {
            return;
        }
        let harmless = matches!(
            inst,
            InstKind::Binary { .. }
                | InstKind::Unary { .. }
                | InstKind::Copy { .. }
                | InstKind::IsNone { .. }
                | InstKind::GetSome { .. }
                | InstKind::MakeSome { .. }
                | InstKind::SetIndex { .. }
        );
        let mut written = Vec::new();
        visit(inst, |_| {}, |local| written.push(local));
        if !harmless {
            self.found_entries.clear();
            return;
        }
        // A dict read out of a field, however deep, isn't the one that field holds once what it
        // was read from is written.
        let from_written = |dict: LocalId| {
            let mut read_from = self.known.field_of(dict);
            while let Some((from, _)) = read_from {
                if written.contains(from) {
                    return true;
                }
                read_from = self.known.field_of(*from);
            }
            false
        };
        let kept: Vec<bool> = self
            .found_entries
            .iter()
            .map(|(dict, key, _)| {
                !written.contains(dict)
                    && !from_written(*dict)
                    && !matches!(key, Operand::Local(local) if written.contains(local))
            })
            .collect();
        let mut kept = kept.into_iter();
        self.found_entries.retain(|_| kept.next().unwrap_or(false));
    }

    /// A local read once lets go of its value right after that, rather than when the function
    /// returns, so it doesn't still count as an owner when the next line changes what it read.
    fn let_go_after(&mut self, inst: &InstKind) {
        for local in self.let_go_of(self.at, inst) {
            let kind = self.kinds.get(local.0).cloned().unwrap_or(Kind::Unit);
            // A struct or an enum held in place lets go of each block it holds, where it is.
            if self.owns_inline(local) {
                let slot = self.slot(local);
                self.release_inline(&kind, &slot);
                let zero = self.zero_line(&kind, &slot);
                self.line(&zero);
                continue;
            }
            let (held, slot, text) = (self.temp(), self.slot(local), self.text_of(&kind));
            self.line(&format!("{held} = load {text}, ptr {slot}"));
            self.line(&format!("store {text} zeroinitializer, ptr {slot}"));
            self.release_kind(&held, &kind);
        }
    }

    /// The locals [`Self::let_go_after`] lets go of after `inst`, leaving their slots empty.
    pub(crate) fn let_go_of(&self, at: (usize, usize), inst: &InstKind) -> Vec<LocalId> {
        // A slice never made leaves what it's of to the lookups that read it in its place.
        if let InstKind::Slice { dest, .. } | InstKind::Copy { dest, .. } = inst
            && self.known.unmade.contains(&dest.0)
        {
            return Vec::new();
        }
        // Nor does an `enumerate` never made, whose list its pairs read in its place.
        if let InstKind::Builtin { dest: Some(dest), .. } | InstKind::Copy { dest, .. } = inst
            && (self.known.enumerated.contains_key(&dest.0) || self.known.map_walks.contains_key(&dest.0))
        {
            return Vec::new();
        }
        let (mut reads, mut writes) = (Vec::new(), Vec::new());
        visit(inst, |operand| reads.push(operand.clone()), |local| writes.push(local));
        // What's made from it may only be borrowing, or may stand for it.
        let leans = writes.iter().any(|dest| {
            self.known.is_borrowed(*dest) || self.known.is_taken(*dest) || self.known.alias(*dest).is_some()
        });
        if leans {
            return Vec::new();
        }
        let mut done_with: Vec<LocalId> = Vec::new();
        for operand in reads {
            let Operand::Local(mut local) = operand else { continue };
            done_with.push(local);
            // With a borrow finished, what it borrowed from is finished too.
            while let Some(lender) = self.known.lender(local) {
                done_with.push(lender);
                local = lender;
            }
        }
        let mut let_go = Vec::new();
        for local in done_with {
            if (self.owns(local) || self.owns_inline(local))
                && self.known.moves(local)
                && !writes.contains(&local)
            {
                let_go.push(local);
            }
        }
        // And what's read here for the last time on every way on.
        for local in self.dying.get(&at).into_iter().flatten() {
            if !let_go.contains(local) && !writes.contains(local) {
                let_go.push(*local);
            }
        }
        let_go
    }

    /// The owned locals this function reads for the last time at each instruction, which are let
    /// go of there: see `dying`. `untracked` are left to the function's end, as are any something
    /// else stands for, a crash reads, or a generator keeps across a pause.
    fn last_reads(&self, untracked: &BTreeSet<usize>) -> crate::dying::Dying {
        let function = self.function;
        let name = function.name.as_str();
        if self.module.reads.generator(name).is_some() {
            return crate::dying::Dying::new();
        }
        // What's read by an instruction whose result stands for it, borrows from it or takes from
        // it, and what anything borrows from.
        let mut leaned_on: BTreeSet<usize> = BTreeSet::new();
        for inst in function.blocks.iter().flat_map(|block| &block.insts) {
            let (mut reads, mut writes) = (Vec::new(), Vec::new());
            visit(&inst.kind, |operand| reads.push(operand.clone()), |local| writes.push(local));
            let leans = writes.iter().any(|dest| {
                self.known.is_borrowed(*dest)
                    || self.known.is_taken(*dest)
                    || self.known.alias(*dest).is_some()
                    || self.known.unmade.contains(&dest.0)
                    || self.known.enumerated.contains_key(&dest.0)
                    || self.known.map_walks.contains_key(&dest.0)
            });
            if leans {
                leaned_on.extend(reads.iter().filter_map(|read| match read {
                    Operand::Local(local) => Some(local.0),
                    _ => None,
                }));
            }
        }
        for at in 0..function.locals.len() {
            let mut local = LocalId(at);
            while let Some(lender) = self.known.lender(local) {
                leaned_on.insert(lender.0);
                local = lender;
            }
        }
        let eligible = |at: usize| {
            at >= function.params
                && (self.owns(LocalId(at)) || self.owns_inline(LocalId(at)))
                && !untracked.contains(&at)
                && !leaned_on.contains(&at)
                && !self.on_crash.contains(&at)
                && !self.saving.contains(&at)
                && self
                    .kinds
                    .get(at)
                    .is_some_and(|kind| crate::dying::lets_go_quietly(self.module.layouts, kind))
        };
        crate::dying::dying(function, &eligible)
    }

    /// Everything local lets go of what it owns, except what's leaving, on a return: what's known
    /// to be empty here has nothing to let go of. A build that checks proofs checks that it is.
    fn release_locals(&mut self, except: Option<LocalId>) {
        let empty = |emit: &Self, at: usize| {
            emit.holding.tracked(at) && !emit.holding_now.contains(&at) && Some(LocalId(at)) != except
        };
        // A function with many such locals checks them through one table, in one call.
        let together = self.checks_proofs() && self.checked_empty().len() > CHECKED_ONE_BY_ONE;
        if together {
            let all: Vec<usize> = (0..self.kinds.len()).filter(|at| empty(self, *at)).collect();
            self.check_all_empty(&all, "a local a return leaves is empty");
        }
        for at in 0..self.kinds.len() {
            if empty(self, at) {
                if !together {
                    self.check_empty(at, "a local a return leaves is empty");
                }
                continue;
            }
            self.release_local(at, except);
        }
    }

    /// The locals a build that checks proofs can check are empty, with how many words each is: a
    /// followed local that's one address, or held in place.
    fn checked_empty(&self) -> Vec<(usize, u32)> {
        let words = |at: usize| {
            let kind = self.kinds.get(at)?;
            if self.owns_inline(LocalId(at)) {
                Some(kind.size(self.module.layouts) / 8)
            } else {
                (self.text_of(kind) == "ptr").then_some(1)
            }
        };
        (0..self.kinds.len())
            .filter(|at| self.holding.tracked(*at))
            .filter_map(|at| Some((at, words(at)?)))
            .collect()
    }

    /// In a build that checks proofs, ends the program unless each of the locals `empty` is, as
    /// [`Self::check_empty`] does for one, in a single call: for a function with many locals to
    /// check, where a check written out for each local at each return grew with their product.
    /// `main` in the `chat_server` kernel has 9,600 such locals and hundreds of returns, and its
    /// text ran to gigabytes.
    ///
    /// The function keeps one table of where those locals are, filled as it starts, beside a
    /// constant of how many words each is. A return hands `fen_proof_empty` the table and a
    /// constant of its own, a bit for each local it wants empty.
    fn check_all_empty(&mut self, empty: &[usize], proof: &str) {
        if self.empties.is_none() {
            let checked = self.checked_empty();
            let name = self.function.name.clone();
            let _ = writeln!(self.allocas, "  %proof.slots = alloca [{} x ptr]", checked.len());
            for (place, (at, _)) in checked.iter().enumerate() {
                let slot = self.slot(LocalId(*at));
                let _ = writeln!(
                    self.allocas,
                    "  %proof.slot{place} = getelementptr inbounds ptr, ptr %proof.slots, i64 {place}\n  store ptr {slot}, ptr %proof.slot{place}"
                );
            }
            let words: Vec<String> = checked.iter().map(|(_, words)| format!("i32 {words}")).collect();
            let sizes = format!("@\"f_{name}.proof.words\"");
            self.declare(&format!(
                "{sizes} = private unnamed_addr constant [{} x i32] [{}]",
                checked.len(),
                words.join(", ")
            ));
            let places = checked.iter().enumerate().map(|(place, (at, _))| (*at, place)).collect();
            self.empties = Some(EmptyTable { places, sizes, masks: HashMap::new() });
        }
        let Some(table) = self.empties.as_mut() else { return };
        let count = table.places.len();
        let mut bits = vec![0_u64; count.div_ceil(64)];
        for place in empty.iter().filter_map(|at| table.places.get(at)) {
            if let Some(word) = bits.get_mut(place / 64) {
                *word |= 1 << (place % 64);
            }
        }
        if bits.iter().all(|word| *word == 0) {
            return;
        }
        let sizes = table.sizes.clone();
        // Returns that want the same locals empty share a constant.
        let made = table.masks.len();
        let fresh = format!("@\"f_{}.proof.empty.{made}\"", self.function.name);
        let mask = table.masks.entry(bits.clone()).or_insert(fresh).clone();
        // Written as LLVM reads an `i64`: the same bits, signed.
        let written: Vec<String> =
            bits.iter().map(|word| format!("i64 {}", i64::from_ne_bytes(word.to_ne_bytes()))).collect();
        self.declare(&format!(
            "{mask} = private unnamed_addr constant [{} x i64] [{}]",
            bits.len(),
            written.join(", ")
        ));
        self.declare("declare void @fennec_proof_broken(ptr, i64, i64) #1");
        self.module.add_helper("fen_proof_empty", PROOF_EMPTY.to_owned());
        let text = self.literal(&format!("proof broken: {proof}"));
        let here = self.label("proof_at");
        let span = self.here_span();
        let (place, args) = self.span_values(&here, span);
        for line in place.lines() {
            self.line(line.trim_start());
        }
        self.line(&format!(
            "call void @fen_proof_empty(ptr %proof.slots, ptr {sizes}, ptr {mask}, i64 {count}, ptr {text}, {args})"
        ));
    }

    /// Everything local lets go of what it owns at `prop`, but for what each failure let go of
    /// before getting there.
    fn release_locals_at_prop(&mut self) {
        for at in 0..self.kinds.len() {
            // Each way out let go of the short-lived locals it held, so a build that checks proofs
            // checks here that none holds anything: once for each local, rather than at each of
            // the function's ways out, which grew with their product.
            if self.split && self.holding.short(at) {
                self.check_empty(at, "a local a failure leaves is empty");
                continue;
            }
            self.release_local(at, None);
        }
    }

    /// In a build that checks proofs, ends the program unless a local thought empty is.
    fn check_empty(&mut self, at: usize, proof: &str) {
        let kind = self.kinds.get(at).cloned().unwrap_or(Kind::Unit);
        if !self.checks_proofs() {
            return;
        }
        // One held in place is empty when every word of it is zero, as letting go of it leaves it.
        if self.owns_inline(LocalId(at)) {
            let slot = self.slot(LocalId(at));
            let mut any = "0".to_owned();
            for word in 0..kind.size(self.module.layouts) / 8 {
                let (place, held, either) = (self.temp(), self.temp(), self.temp());
                self.line(&format!("{place} = getelementptr inbounds i8, ptr {slot}, i64 {}", word * 8));
                self.line(&format!("{held} = load i64, ptr {place}"));
                self.line(&format!("{either} = or i64 {any}, {held}"));
                any = either;
            }
            let full = self.temp();
            self.line(&format!("{full} = icmp ne i64 {any}, 0"));
            let span = self.here_span();
            self.proof_held(&full, proof, span);
            return;
        }
        if self.text_of(&kind) != "ptr" {
            return;
        }
        let (held, full, slot) = (self.temp(), self.temp(), self.slot(LocalId(at)));
        self.line(&format!("{held} = load ptr, ptr {slot}"));
        self.line(&format!("{full} = icmp ne ptr {held}, null"));
        let span = self.here_span();
        self.proof_held(&full, proof, span);
    }

    /// A local lets go of what it owns, unless it's what's leaving.
    fn release_local(&mut self, at: usize, except: Option<LocalId>) {
        let local = LocalId(at);
        if Some(local) == except {
            return;
        }
        let kind = self.kinds.get(at).cloned().unwrap_or(Kind::Unit);
        if self.owns(local) {
            let (held, slot, text) = (self.temp(), self.slot(local), self.text_of(&kind));
            self.line(&format!("{held} = load {text}, ptr {slot}"));
            self.release_kind(&held, &kind);
        } else if self.owns_inline(local) {
            let slot = self.slot(local);
            self.release_inline(&kind, &slot);
        }
    }

    /// Whether this function gives a constant's value, which is worked out once and then kept. In
    /// a program that uses `parallel`, one that can fail is worked out at each read instead, since
    /// `main` works out the kept ones before any worker starts, whether or not they're ever read.
    fn keeps_its_value(&mut self) -> bool {
        let returns = self.returns.clone();
        let name = self.function.name.as_str();
        self.module.reads.constant(name)
            && (!self.module.threads || !self.module.flagged(name))
            && self.text_of(&returns) != "void"
    }

    /// Starts `main` in a program that uses `parallel`: every constant that's kept gets kept here,
    /// so a worker only ever reads it, and two can't race to be the first.
    fn work_out_constants(&mut self) -> Result<(), String> {
        let constants: Vec<&str> =
            self.module.reads.constants().into_iter().filter(|name| !self.module.flagged(name)).collect();
        for name in constants {
            self.call(None, name, &[], Span::default())?;
        }
        Ok(())
    }

    /// Where a constant's value is kept, and whether it's there yet.
    fn kept_places(&mut self) -> (String, String) {
        let name = &self.function.name;
        let (kept, done) = (format!("@\"f_{name}.kept\""), format!("@\"f_{name}.done\""));
        let returns = self.returns.clone();
        let text = self.text_of(&returns);
        self.declare(&format!("{kept} = internal global {text} zeroinitializer"));
        self.declare(&format!("{done} = internal global i1 false"));
        (kept, done)
    }

    /// Starts a constant's function: if its value is kept, that's what it gives back, with one more
    /// owner.
    fn give_back_what_it_kept(&mut self) {
        let (kept, done) = self.kept_places();
        let returns = self.returns.clone();
        let text = self.text_of(&returns);
        let (ready, again, first) = (self.temp(), self.label("kept"), self.label("first"));
        self.line(&format!("{ready} = load i1, ptr {done}"));
        self.line(&format!("br i1 {ready}, label %{again}, label %{first}"));
        self.start_block(&again);
        let held = self.temp();
        self.line(&format!("{held} = load {text}, ptr {kept}"));
        if returns.in_memory() {
            self.line(&format!("store {text} {held}, ptr %ret"));
            self.retain_inline(&returns, "%ret");
            self.line("ret void");
        } else {
            if returns.counted() {
                self.retain_value(&held, &returns);
            }
            self.line(&format!("ret {text} {held}"));
        }
        self.start_block(&first);
    }

    /// Keeps a constant's value, with an owner of its own, the first time it's worked out.
    fn keep_value(&mut self, held: &str) {
        let (kept, done) = self.kept_places();
        let returns = self.returns.clone();
        let text = self.text_of(&returns);
        self.line(&format!("store {text} {held}, ptr {kept}"));
        self.line(&format!("store i1 true, ptr {done}"));
        if returns.in_memory() {
            self.retain_inline(&returns, &kept);
            self.keep_inline(&returns, &kept);
        } else if returns.counted() {
            self.retain_value(held, &returns);
        }
        // A constant held in a block lives for the whole run, so a task anywhere can read it, and
        // it's never counted as left over. That goes for a string whose text is in a block and
        // for an optional held as one word too, which a missing one has no block for.
        let block = match &returns {
            Kind::Str | Kind::MaybeText if self.headers() => self.header_block(held),
            Kind::List(_)
            | Kind::Dict(..)
            | Kind::Boxed(_)
            | Kind::Bytes
            | Kind::Maybe(_)
            | Kind::Str
            | Kind::TextBlock => held.to_owned(),
            _ => return,
        };
        let (none, mark, marked) = (self.temp(), self.label("kept_block"), self.label("kept_marked"));
        self.line(&format!("{none} = icmp eq ptr {block}, null"));
        self.line(&format!("br i1 {none}, label %{marked}, label %{mark}"));
        self.start_block(&mark);
        self.declare("declare void @fennec_constant_kept(ptr) nounwind");
        self.line(&format!("call void @fennec_constant_kept(ptr {block})"));
        self.line(&format!("br label %{marked}"));
        self.start_block(&marked);
    }

    fn give_back(&mut self, value: Option<&Operand>) -> Result<(), String> {
        let returns = self.returns.clone();
        let text = self.text_of(&returns);
        let keeps = self.keeps_its_value();
        if returns.in_memory() {
            let (at, fresh) =
                self.value_for(value.ok_or("it returns nothing where a value is due")?, &returns)?;
            let held = self.temp();
            self.line(&format!("{held} = load {text}, ptr {at}"));
            self.line(&format!("store {text} {held}, ptr %ret"));
            // Leaving, a local's blocks go with its value; others' get another owner.
            let moved = match value {
                Some(Operand::Local(local)) if !fresh && self.owns_inline(*local) => Some(*local),
                _ => None,
            };
            if moved.is_none() && !fresh {
                self.retain_inline(&returns, "%ret");
            }
            if keeps {
                self.keep_value(&held);
            }
            self.release_locals(moved);
            self.pay_free_check_at_return(Some(("%ret", &returns)));
            self.line("ret void");
            return Ok(());
        }
        if text == "void" {
            self.release_locals(None);
            self.pay_free_check_at_return(None);
            self.line("ret void");
            return Ok(());
        }
        let operand = value.ok_or("it returns nothing where a value is due")?;
        let (held, fresh) = self.value_for(operand, &returns)?;
        // Leaving, a local's owner goes with its value.
        let moved = match operand {
            Operand::Local(local) if !fresh && returns.counted() && self.owns(*local) => Some(*local),
            _ => None,
        };
        if returns.counted() && moved.is_none() && !fresh {
            self.retain_value(&held, &returns);
        }
        if keeps {
            self.keep_value(&held);
        }
        self.release_locals(moved);
        self.pay_free_check_at_return(Some((&held, &returns)));
        self.line(&format!("ret {text} {held}"));
        Ok(())
    }

    /// The second copy of each loop written twice (`find_versioned_loops`), made from the text of
    /// the first, its ways out and cold blocks included: everything they define is renamed, with a
    /// `v` after its name, so the way in can go to either, and each read that leans on the loop's
    /// check takes the way with no check (`Emit::ranged_once`). Made from the text, the copies
    /// can't differ in anything else, whatever the emitter knew writing the first.
    fn write_second_copies(&mut self) -> Result<(), String> {
        let mut loops: Vec<usize> = self.copied_text.keys().copied().collect();
        loops.sort_unstable();
        for loop_ in loops {
            let spans = self.copied_text.remove(&loop_).unwrap_or_default();
            let (mut text, mut cold) = (String::new(), String::new());
            for ((from, to), (cold_from, cold_to)) in spans {
                text.push_str(self.out.get(from..to).ok_or("a copied block's text is gone")?);
                cold.push_str(self.cold.get(cold_from..cold_to).ok_or("a copied cold block's text is gone")?);
            }
            let defined: HashSet<String> =
                text.lines().chain(cold.lines()).filter_map(defined_name).collect();
            // A failure out of a traced call in the second copy leaves through the same block as
            // one in the first, whose phi is told of both (`write_traced_exit`, written after).
            let traced: Vec<(String, u32)> = self
                .traced_calls
                .iter()
                .filter(|(block, _)| defined.contains(block))
                .map(|(block, place)| (format!("{block}v"), *place))
                .collect();
            self.traced_calls.extend(traced);
            let mut renamed = |part: &str| -> String {
                let mut written = String::with_capacity(part.len() + part.len() / 8);
                for line in part.lines() {
                    let mut line = renamed_line(line, &defined);
                    if line.ends_with(CHECKED_ONCE) {
                        line = line.replacen("br i1 false,", "br i1 true,", 1);
                    }
                    // A loop's own note for LLVM is its own, in each copy.
                    if let Some((head, _)) = line.split_once(", !llvm.loop !") {
                        let id = FIRST_LOOP + self.module.loops;
                        self.module.loops += 1;
                        self.declare(&format!("!{id} = distinct !{{!{id}, !13}}"));
                        line = format!("{head}, !llvm.loop !{id}");
                    }
                    written.push_str(&line);
                    written.push('\n');
                }
                written
            };
            let (text, cold) = (renamed(&text), renamed(&cold));
            self.out.push_str(&text);
            self.cold.push_str(&cold);
        }
        Ok(())
    }

    fn terminator(&mut self, end: &Terminator) -> Result<(), String> {
        match end {
            // The way into a loop written twice takes the copy with no checks on its reads where
            // their check held (`find_versioned_loops`).
            Terminator::Jump(target)
                if let Some(scan) = self
                    .known
                    .versioned
                    .iter()
                    .find(|loop_| loop_.header == target.0 && loop_.before == self.at.0)
                    .map(|loop_| loop_.scan) =>
            {
                let flag =
                    self.ranged_flags.get(&scan).cloned().ok_or("a loop written twice with no flag")?;
                let held = self.temp();
                self.line(&format!("{held} = load i1, ptr {flag}"));
                self.line(&format!("br i1 {held}, label %bb{}v, label %bb{}, !prof !8", target.0, target.0));
            }
            Terminator::Jump(target) => {
                let back = self.back_edges.contains(&(self.at.0, target.0))
                    && !counted_header(self.function, target.0);
                if back {
                    let id = FIRST_LOOP + self.module.loops;
                    self.module.loops += 1;
                    self.declare(&format!("!{id} = distinct !{{!{id}, !13}}"));
                    self.line(&format!("br label %bb{}, !llvm.loop !{id}", target.0));
                } else {
                    self.line(&format!("br label %bb{}", target.0));
                }
            }
            Terminator::Branch { cond, then, otherwise } => {
                let (cond, _) = self.operand(cond)?;
                self.line(&format!("br i1 {cond}, label %bb{}, label %bb{}", then.0, otherwise.0));
            }
            Terminator::Return(value) => self.give_back(value.as_ref())?,
            Terminator::Unreachable => self.line("unreachable"),
            Terminator::NoMatch { value, span } => self.no_match(value, *span)?,
            Terminator::Stop { message, span } => self.stop(message, *span),
        }
        Ok(())
    }
}

/// What ends the line of the branch a read covered by a loop's own check of item ranges takes:
/// `br i1 false` to its check in the first copy of the loop, which the second turns to `true`
/// (`Emit::write_second_copies`).
pub(crate) const CHECKED_ONCE: &str = "; checked once";

/// The name a line of LLVM text defines: a block's label, or a value.
fn defined_name(line: &str) -> Option<String> {
    if let Some(label) = line.strip_suffix(':')
        && !label.starts_with(' ')
        && !label.is_empty()
        && label.chars().all(llvm_name_char)
    {
        return Some(label.to_owned());
    }
    let (name, _) = line.trim_start().strip_prefix('%')?.split_once(" = ")?;
    name.chars().all(llvm_name_char).then(|| name.to_owned())
}

/// Whether a character can be part of an LLVM name written without quotes.
fn llvm_name_char(found: char) -> bool {
    found.is_ascii_alphanumeric() || matches!(found, '_' | '.' | '$' | '-')
}

/// A line with every name in `defined` renamed for the second copy: a value or label used, `%name`,
/// and a block's own label.
fn renamed_line(line: &str, defined: &HashSet<String>) -> String {
    if let Some(label) = line.strip_suffix(':')
        && defined.contains(label)
    {
        return format!("{label}v:");
    }
    let mut written = String::with_capacity(line.len() + 8);
    let mut chars = line.chars().peekable();
    while let Some(found) = chars.next() {
        written.push(found);
        if found != '%' {
            continue;
        }
        let mut name = String::new();
        while let Some(&next) = chars.peek() {
            if !llvm_name_char(next) {
                break;
            }
            name.push(next);
            chars.next();
        }
        written.push_str(&name);
        if defined.contains(&name) {
            written.push('v');
        }
    }
    written
}

// Windows has no target here yet, as `fennec build` doesn't link there.
#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    fn lowered(source: &str) -> Result<Program, String> {
        let mut module = fennec_syntax::parse(source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        Ok(fennec_ir::lower(&module, &checked))
    }

    fn text_of(source: &str, function: &str) -> Result<String, String> {
        let (units, _, _) = emit(&lowered(source)?)?;
        Ok(units
            .into_iter()
            .find(|unit| unit.function == function)
            .map_or_else(String::new, |unit| unit.text))
    }

    /// A list whose items hold no blocks, ints or a struct of plain fields, is cleared by setting
    /// its length to 0, with no call; one whose items hold blocks calls the runtime, which lets
    /// go of them.
    #[test]
    fn a_list_of_plain_items_is_cleared_by_its_length() -> Result<(), String> {
        let source = "struct Spot:\n    x: int\n    y: float\n\ndef ints(ref xs: list[int]):\n    xs.clear()\n    return\n\ndef spots(ref xs: list[Spot]):\n    xs.clear()\n    return\n\ndef names(ref xs: list[str]):\n    xs.clear()\n    return\n\ndef main(stdout: Stdout):\n    mut a = [1]\n    mut b = [Spot(x=1, y=2.0)]\n    mut c = [\"c\"]\n    ints(ref a)\n    spots(ref b)\n    names(ref c)\n    stdout.print(len(a), len(b), len(c))\n    return\n";
        for plain in ["ints", "spots"] {
            let text = text_of(source, plain)?;
            assert!(!text.contains("@fennec_list_clear(") && text.contains("store i64 0, ptr"), "{text}");
        }
        let text = text_of(source, "names")?;
        assert!(text.contains("call void @fennec_list_clear("), "{text}");
        Ok(())
    }

    /// An enum whose every case is flat crosses a channel as its words, sent and taken through
    /// this thread's buffer, as a flat struct does. One with a case holding a list still goes as a
    /// block.
    #[test]
    fn a_flat_enum_goes_as_its_words_and_one_holding_a_list_as_a_block() -> Result<(), String> {
        let source = "from std.concurrent import Closed, Crashed, Inbox\n\nenum Event:\n    Join(user: int, room: int)\n    Say(user: int, text: str)\n    Quiet\n\nenum Listing:\n    Empty\n    Items(items: list[str])\n\ndef events(inbox: Inbox[Event], stdout: Stdout):\n    for got in inbox:\n        stdout.print(got)\n\ndef listings(inbox: Inbox[Listing], stdout: Stdout):\n    for got in inbox:\n        stdout.print(got)\n\ndef sent(stdout: Stdout) fails Closed | Crashed:\n    with launch((inbox) => events(inbox, stdout))? as sends:\n        sends.send(.Say(user=1, text=\"hello\"))?\n    with launch((inbox) => listings(inbox, stdout))? as sends:\n        sends.send(.Empty())?\n    return\n\ndef main(stdout: Stdout):\n    sent(stdout) except why:\n        stdout.print(\"failed\", why)\n    return\n";
        let (units, _, _) = emit(&lowered(source)?)?;
        let calls = |of: &str, call: &str| {
            units.iter().filter(|unit| unit.function.contains(of) && unit.text.contains(call)).count()
        };
        // Sent and taken as words, never as a block.
        assert!(calls("[Event]", "call i64 @fennec_channel_send_words(") > 0, "no words sent");
        assert!(calls("[Event]", "call i64 @fennec_channel_receive_words(") > 0, "no words taken");
        assert_eq!(calls("[Event]", "call i64 @fennec_channel_send("), 0);
        assert_eq!(calls("[Event]", "call i64 @fennec_channel_receive("), 0);
        // A case holding a list isn't flat.
        assert!(calls("[Listing]", "call i64 @fennec_channel_send(") > 0, "no block sent");
        assert_eq!(calls("[Listing]", "call i64 @fennec_channel_send_words("), 0);
        Ok(())
    }

    /// A list parameter only read in a loop has its length read once, at the start, into `%sl`,
    /// and the loop's index check compares with that. One the loop appends to has its length
    /// read from the list each time, and keeps nothing.
    #[test]
    fn a_list_parameter_read_in_a_loop_has_its_length_kept() -> Result<(), String> {
        let source = "def summed(xs: list[int], n: int) -> int:\n    mut got = 0\n    for at in range(n):\n        got += xs[at * 2]\n    return got\n\ndef grown(ref xs: list[int], n: int) -> int:\n    mut got = 0\n    for at in range(n):\n        xs.append(at)\n        got += xs[at * 2]\n    return got\n\ndef main(stdout: Stdout):\n    mut ys = [1, 2, 3, 4, 5, 6]\n    stdout.print(summed(ys, n=3), grown(ref ys, n=2))\n    return\n";
        let summed = text_of(source, "summed")?;
        assert!(summed.contains("%sl0 = alloca i64") && summed.contains("load i64, ptr %sl0"), "{summed}");
        let grown = text_of(source, "grown")?;
        assert!(!grown.contains("%sl"), "{grown}");
        Ok(())
    }

    /// A program's image describes the shapes of what it makes and what those hold, not the
    /// standard library's other types, with strings' shape right after them.
    #[test]
    fn an_image_has_shapes_only_for_what_the_program_makes() -> Result<(), String> {
        let named = |image: &crate::Image, wanted: &str| {
            image.tables.shapes.iter().any(|shape| match shape.as_ref() {
                fennec_rt::packed::Element::Enum { name, .. }
                | fennec_rt::packed::Element::Struct { name, .. } => &**name == wanted,
                _ => false,
            })
        };
        let hello = "def main(stdout: Stdout):\n    stdout.print(1)\n";
        let (_, shared, layouts) = emit(&lowered(hello)?)?;
        let image = crate::Image::of(&layouts, &shared)?;
        // Types that hold themselves through lists keep their numbers as kinds of item, so they
        // and what they hold stay, and strings' shape comes last.
        let shapes = image.tables.shapes.len();
        assert!(shapes <= 2 * layouts.all_through_lists().len() + 1, "{shapes} of {} layouts", layouts.len());
        assert_eq!(image.tables.str_shape, u64::try_from(shapes - 1).unwrap_or(0));
        let points = "struct Point:\n    x: int\n    y: int\n\nstruct Unused:\n    x: int\n\ndef main(stdout: Stdout):\n    points = [Point(x=1, y=2), Point(x=3, y=4)]\n    stdout.print(points[1].x)\n";
        let (_, shared, layouts) = emit(&lowered(points)?)?;
        let image = crate::Image::of(&layouts, &shared)?;
        assert!(named(&image, "Point"), "{:?}", image.tables.shapes);
        assert!(!named(&image, "Unused"), "{:?}", image.tables.shapes);
        Ok(())
    }

    #[test]
    fn arguments_and_the_clock_are_native_calls() -> Result<(), String> {
        let source = "\
from std.time import Clock, Duration

def main(stdout: Stdout, args: Args, clock: Clock):
    started = clock.now()
    clock.sleep(Duration.millis(1))
    stdout.print(len(args), args.program, started.nanos)
    for arg in args:
        stdout.print(arg, args[0])
    stdout.write(len(args))
";
        let text = text_of(source, "main")?;
        assert!(text.contains("@fennec_args_list("), "{text}");
        assert!(!text.contains("fennec_capability_call"), "{text}");
        Ok(())
    }

    /// `in` on a set or list written out of constants, which nothing else reads, makes neither: it
    /// compares with each constant. One the function keeps, or of values worked out, is made.
    #[test]
    fn looking_in_constants_written_out_makes_no_collection() -> Result<(), String> {
        let source = "\
def is_keyword(word: str) -> bool:
    return word in {\"def\", \"if\", \"a keyword longer than eight bytes\"}

def small(n: int) -> bool:
    return n in [1, 2, 3]

def kept(word: str) -> bool:
    words = [\"def\", \"if\"]
    return word in words and len(words) == 2

def kept_set(word: str) -> bool:
    words = {\"def\", \"if\"}
    return word in words and len(words) == 2

def main(stdout: Stdout):
    stdout.print(is_keyword(\"if\"), small(2), kept(\"if\"), kept_set(\"if\"))
";
        for name in ["is_keyword", "small"] {
            let text = text_of(source, name)?;
            for made in ["@fennec_list_new(", "@fennec_dict_new(", "@fennec_list_contains("] {
                assert!(
                    !text.contains(&format!("call ptr {made}")) && !text.contains(&format!("call i8 {made}")),
                    "{name} calls {made}: {text}"
                );
            }
        }
        let text = text_of(source, "kept")?;
        assert!(text.contains("@fennec_list_contains("), "a list kept is made and looked in: {text}");
        let text = text_of(source, "kept_set")?;
        assert!(text.contains("call ptr @fennec_dict_new("), "a set kept is made: {text}");
        Ok(())
    }

    /// `==` on an enum held in a block compares the cases first, with no call: only the same case
    /// asks the runtime, `fennec_box_equal`, to compare its fields.
    #[test]
    fn an_enum_in_a_block_compares_its_case_before_asking_the_runtime() -> Result<(), String> {
        let source = "\
enum Tok:
    Name(text: str)
    Wrapped(inner: Tok)
    Eof

def is_eof(token: Tok) -> bool:
    return token == .Eof

def main(stdout: Stdout):
    stdout.print(is_eof(.Name(\"x\")))
";
        let text = text_of(source, "is_eof")?;
        let (before, after) = text.split_once("same_case").ok_or_else(|| text.clone())?;
        assert!(before.contains("icmp ne i64"), "the cases are compared first: {text}");
        assert!(!before.contains("call i8 @fennec_box_equal("), "{text}");
        assert!(after.contains("call i8 @fennec_box_equal("), "{text}");
        Ok(())
    }

    #[test]
    fn looking_for_a_part_of_a_text_makes_no_block_for_either() -> Result<(), String> {
        let source = "\
def has_x(text: str) -> bool:
    return \"x\" in text

def main(stdout: Stdout):
    stdout.print(has_x(\"a\"))
";
        let text = text_of(source, "has_x")?;
        assert!(text.contains("@fennec_text_contains("), "{text}");
        assert!(!text.contains("fennec_text_block"), "{text}");
        Ok(())
    }

    #[test]
    fn a_change_elsewhere_in_a_file_leaves_a_functions_text_alone() -> Result<(), String> {
        let later = |gap: &str| {
            let source = format!(
                "{gap}def add(a: int, b: int) -> int:\n    return a + b\n\ndef main(stdout: Stdout):\n    stdout.print(\"sum\", add(1, 2))\n"
            );
            text_of(&source, "add")
        };
        assert_eq!(later("")?, later("## moved down\n\n")?);
        Ok(())
    }

    #[test]
    fn a_constant_keeps_its_value_after_the_first_time() -> Result<(), String> {
        let source = "WORDS = [\"a\", \"b\"]\n\ndef main(stdout: Stdout):\n    stdout.print(WORDS)\n";
        let text = text_of(source, "WORDS")?;
        for written in [
            "@\"f_WORDS.kept\" = internal global ",
            "@\"f_WORDS.done\" = internal global i1 false",
            "= load i1, ptr @\"f_WORDS.done\"",
            ", ptr @\"f_WORDS.kept\"",
            "store i1 true, ptr @\"f_WORDS.done\"",
        ] {
            assert!(text.contains(written), "{written} isn't in:\n{text}");
        }
        assert!(text.contains("define swiftcc") && text.contains("@\"f_WORDS\""), "{text}");
        // What reads it calls it rather than holding a copy, and isn't kept itself.
        let main = text_of(source, "main")?;
        assert!(main.contains("call swiftcc") && main.contains("@\"f_WORDS\"("), "{main}");
        assert!(!main.contains(".kept"), "{main}");
        assert!(!main.contains("define internal") || !main.contains("f_WORDS.done"), "{main}");
        Ok(())
    }

    #[test]
    fn a_program_using_parallel_works_out_its_constants_before_any_worker_starts() -> Result<(), String> {
        let source = "WORDS = [\"a\", \"b\"]\nFIRST = WORDS[5]\n\ndef main(stdout: Stdout):\n    mut counts = [1, 2]\n    \
                      for ref count in parallel(counts):\n        count += len(WORDS) + len(FIRST)\n    \
                      stdout.print(counts)\n";
        let mut module = fennec_syntax::parse(source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let program = fennec_ir::lower_with(
            &module,
            &checked,
            fennec_ir::Options { outline_parallel: true, ..fennec_ir::Options::default() },
        );
        assert!(crate::parallel::uses_parallel(&program), "{:?}", program.not_lowered);
        let (units, _, _) = emit(&program)?;
        let text =
            |name: &str| units.iter().find(|unit| unit.function == name).map(|unit| unit.text.as_str());
        let kept = text("WORDS").ok_or("`WORDS` isn't lowered")?;
        assert!(kept.contains("@\"f_WORDS.kept\""), "{kept}");
        // One that can fail would fail a program that never read it, so it's left until a read.
        let failing = text("FIRST").ok_or("`FIRST` isn't lowered")?;
        assert!(!failing.contains(".kept"), "{failing}");
        let main = text("main").ok_or("`main` isn't lowered")?;
        let worked_out =
            main.find("call swiftcc ptr @\"f_WORDS\"").ok_or("`main` doesn't work out `WORDS`")?;
        // A `for ref` loop, which writes its items back where they are.
        let workers = main.rfind("@fennec_parallel_for_in_place(").ok_or("`main` starts no workers")?;
        assert!(worked_out < workers, "{main}");
        assert!(!main.contains("@\"f_FIRST\""), "{main}");
        Ok(())
    }

    /// A field of a local list's item, changed by `append`, is taken out for the change rather
    /// than given a second owner, whether the list is the local itself or a field of it.
    #[test]
    fn appending_to_a_field_of_an_item_takes_it_out_and_lets_go_of_nothing() -> Result<(), String> {
        let types =
            "struct Unit:\n    tags: list[int]\n    hp: int\n\nstruct World:\n    units: list[Unit]\n\n";
        let main = "def main(stdout: Stdout):\n    mut units = [Unit([1], 0)]\n    grow(ref units, 0, 1)\n    mut world = World(units)\n    grow_field(ref world, 0, 1)\n    stdout.print(units[0].hp, read(world, 0))\n";
        let source = format!(
            "{types}def grow(ref units: list[Unit], at: int, v: int):\n    units[at].tags.append(v)\n\ndef grow_field(ref world: World, at: int, v: int):\n    world.units[at].tags.append(v)\n\ndef read(world: World, at: int) -> int:\n    return world.units[at].hp\n\n{main}"
        );
        for function in ["grow", "grow_field"] {
            let text = text_of(&source, function)?;
            // Where the item is in the list, as `take_from_item` works it out.
            assert!(text.contains("getelementptr inbounds %\"s.Unit\""), "{function}: {text}");
            assert!(text.contains("fennec_list_grow"), "{function}: {text}");
            // Nothing here has an owner to let go of, as a field read with one would.
            assert!(!text.contains("call void @fennec_free"), "{function}: {text}");
        }
        // A plain read of a field reads it.
        assert!(text_of(&source, "read")?.contains("getelementptr"));
        Ok(())
    }

    /// `+=` grows the text where it is, in a local and in a field. A plain `+` makes a new one.
    #[test]
    fn a_text_grows_where_it_is_when_its_added_to() -> Result<(), String> {
        let source = "\
struct Note:
    text = \"\"

def in_field(ref note: Note, more: str):
    note.text += more

def in_local(more: str) -> str:
    mut text = \"\"
    text += more
    return text

def anew(note: Note, more: str) -> str:
    return note.text + more

def main(stdout: Stdout):
    mut note = Note()
    in_field(ref note, \"a\")
    stdout.print(in_local(\"b\"), anew(note, \"c\"))
";
        for function in ["in_field", "in_local"] {
            let text = text_of(source, function)?;
            assert!(text.contains("@fennec_str_append("), "{function}: {text}");
        }
        let text = text_of(source, "anew")?;
        assert!(!text.contains("@fennec_str_append("), "{text}");
        Ok(())
    }

    /// A number's text goes straight into a header, with no block made and let go of: an int's
    /// written natively, a float's by the runtime.
    #[test]
    fn a_numbers_text_makes_no_block() -> Result<(), String> {
        let source = "\
def of_int(n: int) -> str:
    return str(n)

def of_float(x: float) -> str:
    return str(x)

def main(stdout: Stdout):
    stdout.print(of_int(1), of_float(1.5))
";
        for (function, call) in [("of_int", "@fen_int_header("), ("of_float", "@fennec_float_header(")] {
            let text = text_of(source, function)?;
            assert!(text.contains(call), "{function}: {text}");
            assert!(!text.contains("_text("), "{function}: {text}");
        }
        Ok(())
    }

    #[test]
    fn a_function_that_cant_fail_has_no_flag() -> Result<(), String> {
        let source = "def half(a: float) -> float:\n    return a * 0.5\n\ndef main(stdout: Stdout):\n    stdout.print(half(1.0))\n";
        assert!(text_of(source, "half")?.contains("define swiftcc double @\"f_half\"(double %p0)"));
        Ok(())
    }

    /// A program whose code is handed every kind of fact: a `ref` parameter (`noalias`), lengths
    /// (`!range`), a remainder by a number and a list made alone (`llvm.assume`), and the failure
    /// flag a function that can fail is given clear (`llvm.assume`).
    const FACTFUL: &str = "struct Unit:\n    hp: int\n\ndef bump(ref unit: Unit, by: int, seen: list[int]) -> int:\n    unit.hp += by % 7 + len(seen)\n    return unit.hp\n\ndef fill(xs: list[int]) -> list[int]:\n    mut ys = xs\n    ys.append(len(xs))\n    return ys\n\ndef pick(xs: list[int], at: int) -> int:\n    return xs[at]\n\ndef main(stdout: Stdout):\n    mut unit = Unit(hp=1)\n    stdout.print(bump(ref unit, by=10, seen=[1]), fill([1, 2]), pick([4, 5], at=1))\n    return\n";

    /// Every module's text, as a build with these settings hands it to LLVM.
    fn handed(shared: Shared) -> Result<(String, Shared), String> {
        let (units, shared, _) = emit_with(&lowered(FACTFUL)?, Cpu::Usual, shared)?;
        Ok((units.iter().map(|unit| unit.text.as_str()).collect(), shared))
    }

    /// A build with facts stripped hands LLVM none, where a plain build hands it every kind.
    #[test]
    fn stripping_facts_hands_llvm_none() -> Result<(), String> {
        let facts =
            ["noalias", "captures(none)", "!range !", "!tbaa !", "inbounds", "call void @llvm.assume("];
        let (plain, _) = handed(Shared::default())?;
        for fact in facts {
            assert!(plain.contains(fact), "a plain build hands LLVM `{fact}`");
        }
        let (stripped, _) = handed(Shared { facts: crate::Facts::Stripped, ..Shared::default() })?;
        for fact in facts.iter().chain(&[" nsw ", " nuw ", "!invariant.load"]) {
            assert!(!stripped.contains(fact), "`{fact}` is left in:\n{stripped}");
        }
        Ok(())
    }

    /// A build that checks proofs checks the facts it can rather than handing them over: what's
    /// assumed, that a length is in range, and that a `ref` parameter's address is its alone. The
    /// TBAA tags, which it can't check, stay, for a build with facts stripped to be compared with.
    #[test]
    fn checking_proofs_checks_the_facts_it_can() -> Result<(), String> {
        let (text, shared) = handed(Shared { proofs: crate::Proofs::Checked, ..Shared::default() })?;
        for fact in ["noalias", "!range !", "call void @llvm.assume("] {
            assert!(!text.contains(fact), "`{fact}` is handed over:\n{text}");
        }
        assert!(text.contains("call void @fen_proof_held(") && text.contains("!tbaa !"), "{text}");
        let said: Vec<&String> = shared.literals.values().collect();
        for proof in [
            "a length is at most",
            "a `ref` parameter's address",
            "every caller passes the failure flag clear",
            "a remainder by a positive number",
        ] {
            assert!(said.iter().any(|text| text.contains(proof)), "no check that {proof}: {said:?}");
        }
        Ok(())
    }

    /// A build that checks proofs checks a string's cursor is inside it wherever it's taken as a
    /// place in its bytes: in finding a character, reading one, and walking to one.
    #[test]
    fn checking_proofs_checks_a_strings_cursor() -> Result<(), String> {
        let source = "def go(s: str) -> int:\n    mut n = 0\n    for i in range(len(s)):\n        if s[i] == \"\u{e9}\" or s[i - 1] == \"a\":\n            n += 1\n    return n\n\ndef main(stdout: Stdout):\n    stdout.print(go(\"caf\u{e9} na\u{ef}ve\"))\n    return\n";
        let program = lowered(source)?;
        let (plain, _, _) = emit(&program)?;
        let plain: String = plain.iter().map(|unit| unit.text.as_str()).collect();
        assert!(plain.contains("@fen_char_walk(") && !plain.contains("%cursor_wrong"), "{plain}");
        let checked = Shared { proofs: crate::Proofs::Checked, ..Shared::default() };
        let (units, shared, _) = emit_with(&program, Cpu::Usual, checked)?;
        let text: String = units.iter().map(|unit| unit.text.as_str()).collect();
        let helpers = text.matches("%cursor_wrong = or i1").count();
        assert!(helpers >= 2, "{helpers} helpers check the cursor:\n{text}");
        assert!(shared.literals.values().any(|said| said.contains("a string's cursor is inside it")));
        Ok(())
    }

    /// A slice of a block's text that isn't ASCII finds where its ends are only once it's known to
    /// be short enough to be inline. A longer one goes to the runtime with nothing walked, as
    /// `api_handler`'s body did. An end that's the text's own end is never looked for: it's where
    /// the bytes end.
    #[test]
    fn a_slice_finds_its_ends_only_when_it_could_be_inline() -> Result<(), String> {
        let source = "def go(s: str, a: int) -> str:\n    return s[a:]\n\ndef main(stdout: Stdout):\n    stdout.print(go(\"caf\u{e9} na\u{ef}ve\", 2))\n    return\n";
        let program = lowered(source)?;
        let (plain, _, _) = emit(&program)?;
        let plain: String = plain.iter().map(|unit| unit.text.as_str()).collect();
        let start = plain.find("@\"f_go\"(").ok_or_else(|| format!("no go:\n{plain}"))?;
        let go = plain.get(start..).and_then(|rest| rest.split("\n}\n").next()).unwrap_or_default();
        let short = go.find("\nslice_short").ok_or_else(|| format!("no short check:\n{go}"))?;
        let placed = go.find("call i64 @fen_char_place(").ok_or_else(|| format!("no place found:\n{go}"))?;
        assert!(short < placed, "a place is found before the length is checked:\n{go}");
        // Each place is found in a block of its own, which the text's own end goes around.
        let guarded =
            go.lines().filter(|line| line.starts_with("slice_place") && !line.starts_with("slice_placed"));
        assert_eq!(go.matches("call i64 @fen_char_place(").count(), guarded.count(), "{go}");
        Ok(())
    }

    /// A build that checks proofs checks a text joined into a block is past 23 bytes, as its
    /// header, made from the block with nothing looked up, takes it to be. A plain build doesn't.
    #[test]
    fn checking_proofs_checks_a_joined_text_is_past_a_header() -> Result<(), String> {
        let source = "def join(a: str, b: str) -> str:\n    return a + b\n\ndef main(stdout: Stdout):\n    stdout.print(join(\"a string long enough\", \" to need a block\"))\n    return\n";
        let program = lowered(source)?;
        let proof = "a text joined in a block is past 23 bytes";
        let (plain, plain_shared, _) = emit(&program)?;
        let plain: String = plain.iter().map(|unit| unit.text.as_str()).collect();
        assert!(!plain.contains(proof) && !plain_shared.literals.values().any(|said| said.contains(proof)));
        let checked = Shared { proofs: crate::Proofs::Checked, ..Shared::default() };
        let (units, shared, _) = emit_with(&program, Cpu::Usual, checked)?;
        let text: String = units.iter().map(|unit| unit.text.as_str()).collect();
        let said: Vec<&String> = shared.strings.values().chain(shared.literals.values()).collect();
        assert!(text.contains(proof) || said.iter().any(|said| said.contains(proof)), "{text}");
        Ok(())
    }

    /// An item read out of a `Table` by `Id`, changed and put back by the same `Id` is put back
    /// where it was read: one lookup, which fails if the `Id` finds nothing. A call given the
    /// `Table` in between could change it, so there the put-back looks it up again.
    #[test]
    fn an_item_put_back_by_the_id_it_was_read_by_is_looked_up_once() -> Result<(), String> {
        let source = "from std.collections import Id, Table\n\nstruct Unit:\n    hp: int\n    bag: dict[str, int]\n\ndef hurt(ref units: Table[Unit], ids: list[Id[Unit]], at: int):\n    mut unit = units[ids[at]]\n    unit.hp -= 1\n    if unit.hp <= 0:\n        unit.bag[\"bone\"] = 1\n    units[ids[at]] = unit\n    return\n\ndef heal(ref units: Table[Unit], id: Id[Unit]):\n    mut unit = units[id]\n    unit.hp += 1\n    units[id] = unit\n    return\n\ndef hurt_around_a_call(ref units: Table[Unit], id: Id[Unit]):\n    mut unit = units[id]\n    unit.hp -= 1\n    heal(ref units, id)\n    units[id] = unit\n    return\n\ndef main(stdout: Stdout):\n    mut units = Table[Unit]()\n    ids = [units.add(Unit(hp=1, bag={\"gold\": 1}))]\n    hurt(ref units, ids, at=0)\n    hurt_around_a_call(ref units, ids[0])\n    stdout.print(units[ids[0]].hp)\n";
        // Counted in the function's own definition, apart from the copies of others its unit has
        // for inlining.
        let lookups = |text: &str, function: &str| {
            let own = format!("@\"f_{function}\"(");
            text.split("\ndefine ")
                .filter(|definition| definition.lines().next().is_some_and(|first| first.contains(&own)))
                .flat_map(str::lines)
                .filter(|line| line.starts_with("stale") && line.ends_with(':'))
                .count()
        };
        let once = text_of(source, "hurt")?;
        assert_eq!(lookups(&once, "hurt"), 1, "{once}");
        let twice = text_of(source, "hurt_around_a_call")?;
        assert_eq!(lookups(&twice, "hurt_around_a_call"), 2, "{twice}");
        Ok(())
    }

    /// A build that checks proofs checks, at each return, that the locals let go of before it
    /// are empty. A function with many of them does it through one table and one call a return,
    /// so its text grows with its locals and its returns, not with their product: `main` in the
    /// `chat_server` kernel, 9,600 such locals and hundreds of returns, ran the compiler past
    /// 6 GB. A function with few still checks each where it is.
    #[test]
    fn a_function_with_many_locals_checks_them_empty_through_a_table() -> Result<(), String> {
        let (locals, returns) = (300, 40);
        let mut source = String::from("def many(n: int) -> int:\n");
        for at in 0..locals {
            source.push_str(&format!(
                "    held{at} = [n + {at}, n]\n    if len(held{at}) > 2:\n        return {at}\n"
            ));
        }
        for at in 0..returns {
            source.push_str(&format!("    if n == {at}:\n        return n + {at}\n"));
        }
        source.push_str("    return 0\n\n");
        source.push_str("def few(n: int) -> int:\n    held = [n, n]\n    if len(held) > 2:\n        return 1\n    return 0\n\n");
        source.push_str("def main(stdout: Stdout):\n    stdout.print(many(3), few(3))\n    return\n");
        let program = lowered(&source)?;
        let checked = Shared { proofs: crate::Proofs::Checked, ..Shared::default() };
        let (units, shared, _) = emit_with(&program, Cpu::Usual, checked)?;
        let text = |name: &str| {
            units.iter().find(|unit| unit.function == name).map_or("", |unit| unit.text.as_str())
        };
        let said: Vec<&String> = shared.literals.values().collect();
        assert!(said.iter().any(|said| said.contains("proof broken: a local a return leaves is empty")));
        let (many, few) = (text("many"), text("few"));
        let calls = many.matches("call void @fen_proof_empty(").count();
        assert!(calls >= returns, "{calls} calls for {returns} returns");
        assert!(many.contains("%proof.slots = alloca ["), "the table");
        // Written out for each local at each return, the returns alone had over 100,000 lines.
        assert!(many.lines().count() < 40_000, "{} lines", many.lines().count());
        assert!(!few.contains("@fen_proof_empty(") && few.contains("proof_broken"), "{few}");
        // A build that doesn't check proofs has none of it.
        let (plain, _, _) = emit(&program)?;
        assert!(plain.iter().all(|unit| !unit.text.contains("proof.slots")));
        Ok(())
    }

    /// A capability's method is called through its module's own export, so a program that reads
    /// a file links no SQLite and no sockets. A program with a test built into it calls the export
    /// that runs every module's, as a test's capabilities are fakes only that one asks.
    #[test]
    fn a_capability_method_goes_to_its_modules_export_unless_tests_are_built_in() -> Result<(), String> {
        let source = "def adds() -> int:\n    return 1 + 1\n\ndef main(fs: Fs, stdout: Stdout):\n    text = fs.read_text(\"notes.txt\") except why:\n        f\"none: {why}\"\n    stdout.print(text, adds())\n    return\n";
        let emitted = |program: &Program| -> Result<String, String> {
            let (units, _, _) = emit(program)?;
            Ok(units.iter().map(|unit| unit.text.as_str()).collect())
        };
        let mut program = lowered(source)?;
        let plain = emitted(&program)?;
        assert!(plain.contains("@fennec_fs_capability("), "{plain}");
        assert!(!plain.contains("@fennec_native_capability("), "{plain}");
        // A test is a function named `test:` and its number, as `fennec test` and `fennec run`
        // build one in.
        let mut test = program.functions.iter().find(|function| function.name == "adds").cloned();
        if let Some(test) = test.as_mut() {
            "test:0".clone_into(&mut test.name);
        }
        program.functions.extend(test);
        let tested = emitted(&program)?;
        assert!(tested.contains("@fennec_native_capability("), "{tested}");
        assert!(!tested.contains("@fennec_fs_capability("), "{tested}");
        Ok(())
    }

    /// `main` and a test are started by the runtime, which has a capability for each thing they
    /// take and nothing for anything else. A parameter of another type stops the build: handed a
    /// null in its place, `main` read a list at address 0 and the program died.
    #[test]
    fn what_the_runtime_starts_takes_capabilities_and_nothing_else() -> Result<(), String> {
        // Neither reads `args`, so the body builds whatever its type is and only the entry differs.
        let source = "def main(stdout: Stdout, args: Args):\n    stdout.print(1)\n    return\n";
        let entries = |program: &Program| -> (Result<String, String>, Result<String, String>) {
            let made = emit(program).and_then(|(_, shared, layouts)| {
                let image = crate::entry::Image::of(&layouts, &shared)?;
                Ok((shared, image))
            });
            match made {
                Ok((shared, image)) => (
                    crate::entry::entry(program, &shared, &image, &[]),
                    crate::entry::tests_entry(program, &shared, &image, &[], &["test:0".to_owned()]),
                ),
                Err(why) => (Err(why.clone()), Err(why)),
            }
        };
        // A test is a function named `test:` and its number, here one that takes `Args`.
        let mut program = lowered(source)?;
        let mut test = program.functions.iter().find(|function| function.name == "main").cloned();
        if let Some(test) = test.as_mut() {
            "test:0".clone_into(&mut test.name);
        }
        program.functions.extend(test);
        let (main, tests) = entries(&program);
        assert!(main?.contains("call i64 @fennec_capability_block(i64 3)"));
        assert!(tests?.contains("call i64 @fennec_capability_block(i64 3)"));
        // The same program as it would reach here had the checker let `args: list[str]` through.
        for function in &mut program.functions {
            if (function.name == "main" || function.name == "test:0")
                && let Some(args) = function.locals.get_mut(1)
            {
                args.ty = fennec_check::Type::List(Box::new(fennec_check::Type::Str));
            }
        }
        let (main, tests) = entries(&program);
        let main = main.err().ok_or("`main` was started with a list")?;
        assert!(main.contains("`main` takes `args`, a `list[str]`, which isn't a capability"), "{main}");
        let tests = tests.err().ok_or("a test was started with a list")?;
        assert!(tests.contains("`test:0` takes `args`, a `list[str]`, which isn't a capability"), "{tests}");
        Ok(())
    }

    /// A build that checks proofs writes the checks they left out, each saying which proof broke:
    /// a loop's count can't overflow, and its index is inside the list it goes through.
    #[test]
    fn checking_proofs_writes_what_they_left_out() -> Result<(), String> {
        let source = "def total(xs: list[int]) -> int:\n    mut subtotal = 0\n    for i in range(len(xs)):\n        subtotal += xs[i] * 0\n    return subtotal\n\ndef main(stdout: Stdout):\n    stdout.print(total([1, 2]))\n";
        let program = lowered(source)?;
        let checked = Shared { proofs: crate::Proofs::Checked, ..Shared::default() };
        let (units, shared, _) = emit_with(&program, Cpu::Usual, checked)?;
        let text: String = units.iter().map(|unit| unit.text.as_str()).collect();
        let said: Vec<&String> = shared.strings.values().chain(shared.literals.values()).collect();
        assert!(said.iter().any(|text| text.contains("proof broken: index inside its list")), "{said:?}");
        assert!(said.iter().any(|text| text.contains("proof broken: no overflow")), "{said:?}");
        let (plain, plain_shared, _) = emit(&program)?;
        let plain: String = plain.iter().map(|unit| unit.text.as_str()).collect();
        assert!(!plain_shared.literals.values().any(|text| text.contains("proof broken")));
        assert!(text.len() > plain.len());
        Ok(())
    }

    /// A product of two ints proven not negative is checked as an unsigned one, as Rust checks a
    /// `usize`'s: two lengths, which can be big enough to overflow. One whose ints may be negative
    /// keeps the signed check.
    #[test]
    fn only_a_product_of_ints_that_arent_negative_checks_unsigned() -> Result<(), String> {
        let source = "def area(w: int, h: int) -> int:\n    return w * h\n\ndef scaled(x: int) -> int:\n    return x * 3\n\ndef main(stdout: Stdout):\n    xs = [1, 2, 3]\n    stdout.print(area(len(xs), h=len(xs)))\n    stdout.print(scaled(xs[0] - 5))\n    return\n";
        let area = text_of(source, "area")?;
        assert!(area.contains("@llvm.umul.with.overflow.i64(") && !area.contains("@llvm.smul"), "{area}");
        let scaled = text_of(source, "scaled")?;
        assert!(
            scaled.contains("@llvm.smul.with.overflow.i64(") && !scaled.contains("@llvm.umul"),
            "{scaled}"
        );
        Ok(())
    }

    /// A dict the function makes and only looks things up in and puts into needs no check that
    /// it's alone before each put. One it copies, passes to a call, stores, or was given keeps it.
    #[test]
    fn a_put_into_a_dict_nothing_else_holds_checks_nothing() -> Result<(), String> {
        let program = |body: &str| {
            format!(
                "def keep(d: dict[int, int]) -> int:\n    return len(d)\n\ndef counted(n: int, given: dict[int, int]) -> int:\n{body}\n\ndef main(stdout: Stdout):\n    stdout.print(counted(10, given={{}}))\n    return\n"
            )
        };
        let checked = |body: &str| -> Result<(usize, usize), String> {
            let text = text_of(&program(body), "counted")?;
            Ok((text.matches("br i1 true, label %quick").count(), text.matches("label %quick").count()))
        };
        let alone = "    mut counts: dict[int, int] = {}\n    for i in range(n):\n        counts[i % 3] = (counts.get(i % 3) else 0) + 1\n    return len(counts) + counts[0]";
        let (proven, puts) = checked(alone)?;
        assert!(proven >= 1 && proven == puts, "{proven} of {puts}");
        // Each of these lets something else hold the dict, so every put checks.
        let shared = [
            "    mut counts: dict[int, int] = {}\n    for i in range(n):\n        counts[i] = keep(counts)\n    return len(counts)",
            "    mut counts: dict[int, int] = {}\n    mut kept = [counts]\n    for i in range(n):\n        counts[i] = 1\n    return len(counts) + len(kept[0])",
            "    mut counts: dict[int, int] = {}\n    other = counts\n    for i in range(n):\n        counts[i] = 1\n    return len(counts) + len(other)",
            "    mut counts = given\n    for i in range(n):\n        counts[i] = 1\n    return len(counts)",
        ];
        for body in shared {
            let (proven, puts) = checked(body)?;
            assert!(proven == 0 && puts >= 1, "{body}: {proven} of {puts}");
        }
        Ok(())
    }

    /// A `while` loop's way back says not to interleave it, as nothing bounds its turns; a `for`
    /// loop's doesn't, so LLVM still interleaves a counted loop where that pays.
    #[test]
    fn only_a_loop_nothing_counts_says_not_to_interleave() -> Result<(), String> {
        let source = "def marks(limit: int) -> int:\n    mut composite = [0 for _ in range(limit + 1)]\n    mut count = 0\n    for n in range(2, limit + 1):\n        if composite[n] == 0:\n            count += 1\n            mut multiple = n * n\n            while multiple <= limit:\n                composite[multiple] = 1\n                multiple += n\n    return count\n\ndef main(stdout: Stdout):\n    stdout.print(marks(100))\n";
        let text = text_of(source, "marks")?;
        let marked =
            text.lines().filter(|line| line.contains("br label") && line.contains("!llvm.loop")).count();
        assert_eq!(marked, 1, "only the while loop: {text}");
        assert!(text.contains("!{!\"llvm.loop.interleave.count\", i32 1}"), "{text}");
        Ok(())
    }

    /// Checking proofs covers a loop filling a list too: one made whole of zeros gets one append a
    /// turn, and one whose length is kept aside isn't changed by anything else meanwhile.
    #[test]
    fn checking_proofs_covers_a_list_a_loop_fills() -> Result<(), String> {
        let source = "def made(n: int) -> int:\n    zeros = [0 for i in range(n)]\n    doubled = [i * 2 for i in range(n)]\n    return len(zeros) + len(doubled)\n\ndef main(stdout: Stdout):\n    stdout.print(made(5))\n";
        let program = lowered(source)?;
        let checked = Shared { proofs: crate::Proofs::Checked, ..Shared::default() };
        let (_, shared, _) = emit_with(&program, Cpu::Usual, checked)?;
        let said: Vec<&String> = shared.literals.values().collect();
        for proof in ["gets one a turn, from 0", "is as long as its loop", "whose length was kept aside"] {
            assert!(said.iter().any(|text| text.contains(proof)), "{proof}: {said:?}");
        }
        Ok(())
    }

    /// A build that checks proofs checks each short-lived local is empty once, where the failures
    /// come together, not at each way out: the checks grew with the lines times the locals, and
    /// a 200-line `main` came to 239,800 of them and 143 MB of IR.
    #[test]
    fn checking_proofs_checks_each_local_once_however_many_ways_out() -> Result<(), String> {
        let checks = |lines: usize| -> Result<usize, String> {
            let line = "    stdout.print(area(.Square(at_or_zero([3, 1, 2], at_or_zero([1, 2], 1)))))\n";
            let source = format!(
                "enum Shape:\n    Circle(radius: int)\n    Square(side: int)\n    Empty\n\ndef area(shape: Shape) -> int:\n    match shape:\n        case .Circle(radius): return radius * radius * 3\n        case .Square(side): return side * side\n        case .Empty: return 0\n\ndef at_or_zero(items: list[int], at: int) -> int:\n    if len(items) == 0: return 0\n    return items[at % len(items)]\n\ndef main(stdout: Stdout):\n{}",
                line.repeat(lines)
            );
            let program = lowered(&source)?;
            let checked = Shared { proofs: crate::Proofs::Checked, ..Shared::default() };
            let (units, shared, _) = emit_with(&program, Cpu::Usual, checked)?;
            let said: Vec<&String> = shared.literals.values().collect();
            assert!(said.iter().any(|text| text.contains("a local a failure leaves is empty")), "{said:?}");
            let main = units.iter().find(|unit| unit.function == "main").ok_or("no main")?;
            Ok(main.text.matches("call void @fennec_proof_broken(").count())
        };
        let (fewer, more) = (checks(100)?, checks(200)?);
        assert!(fewer > 0);
        assert!(more <= fewer * 5 / 2, "{fewer} checks for 100 lines and {more} for 200");
        Ok(())
    }

    /// A `list[str]` a loop fills and that's then joined is kept as the string it becomes, which
    /// has no list length to keep aside and write back: writing one would write over the string's
    /// own length, with a value nothing set. The build checking proofs found it.
    #[test]
    fn a_string_built_by_a_loop_has_no_length_written_over() -> Result<(), String> {
        let source = "def repeated(text: str, times: int) -> str:\n    return \"\".join([text for _ in range(times)])\n\ndef main(stdout: Stdout):\n    stdout.print(repeated(\"ab\", 3))\n";
        let text = text_of(source, "repeated")?;
        assert!(!text.contains("kept_length"), "{text}");
        Ok(())
    }

    /// An enum holding strings gets native blocks in a program with no other threads, and its
    /// drop lets go of each string's block, giving the runtime one with no owner left, to free
    /// plainly: a string holds nothing.
    #[test]
    fn an_enum_holding_strings_has_native_blocks() -> Result<(), String> {
        let source = "enum Named:\n    Leaf(name: str)\n    Pair(left: Named, right: Named, label: str)\n\ndef build(depth: int) -> Named:\n    if depth == 0: return .Leaf(name=\"leaf\")\n    return .Pair(left=build(depth - 1), right=build(depth - 1), label=\"pair\")\n\ndef size(tree: Named) -> int:\n    match tree:\n        case .Leaf(name):\n            return len(name)\n        case .Pair(left, right, label):\n            return size(left) + size(right) + len(label)\n\ndef main(stdout: Stdout):\n    stdout.print(size(build(4)))\n";
        let (units, shared, _) = emit(&lowered(source)?)?;
        assert!(!shared.native.is_empty(), "Named's blocks are native");
        let text: String = units.iter().map(|unit| unit.text.as_str()).collect();
        assert!(text.contains("call void @fennec_free_plain(ptr %text"), "{text}");
        Ok(())
    }

    /// A program with no generator that closes something when it's let go partway frees every
    /// block plainly, with nothing to check after (#2127).
    #[test]
    fn a_program_whose_generators_close_nothing_frees_everything_plainly() -> Result<(), String> {
        let source = "def walk(n: int) -> Iterator[str]:\n    for at in range(n):\n        yield f\"a line long enough for a block {at}\"\n\ndef main(stdout: Stdout):\n    lines = [line for line in walk(3)]\n    keyed = {line: len(line) for line in lines}\n    stdout.print(len(lines), len(keyed))\n";
        let (units, _, _) = emit(&lowered(source)?)?;
        let text: String = units.iter().map(|unit| unit.text.as_str()).collect();
        assert!(text.contains("@fennec_free_plain"), "{text}");
        assert!(!text.contains("@fennec_free("), "{text}");
        Ok(())
    }

    /// In a program with a generator that closes something when it's let go partway, only what
    /// may be or hold one is freed with `fennec_free`, and the flag is read after each such free:
    /// a string's free is plain, and the generator's is checked, so nothing goes unchecked in any
    /// function of the program, the standard library's and the blocks a call into the runtime
    /// carries its arguments in included (#2127).
    #[test]
    fn only_what_may_hold_a_generator_that_closes_is_freed_checked() -> Result<(), String> {
        let source = "from std import crypto\n\nerror Shut:\n    Stuck(name: str)\n\nstruct Door:\n    name: str\n\ndef opened(name: str) -> with Door fails Shut:\n    door = Door(name=name)\n    lend door\n    fail .Stuck(name=name)\n\ndef held(name: str) -> Iterator[int] fails Shut:\n    with opened(name)? as door:\n        yield 1\n        yield 2\n\ndef kept(name: str) -> int fails Shut:\n    walk = held(name)?\n    for n in walk:\n        break\n    said = f\"{name}, said with more text than a header holds\"\n    return len(crypto.sha256(said))\n\ndef main(stdout: Stdout):\n    n = kept(\"kept\") except why:\n        stdout.print(why)\n        return\n    stdout.print(n)\n";
        let (units, _, _) = emit(&lowered(source)?)?;
        let kept = units.iter().find(|unit| unit.function == "kept").map_or("", |unit| unit.text.as_str());
        assert!(kept.contains("@fennec_free("), "{kept}");
        assert!(kept.contains("@fennec_free_plain"), "{kept}");
        for unit in &units {
            let unchecked: Vec<_> = crate::failure_checks::unchecked_failures(&unit.text, &[])
                .into_iter()
                .filter(|missed| missed.callee == "fennec_free")
                .collect();
            assert!(unchecked.is_empty(), "{}: {unchecked:?}\n{}", unit.function, unit.text);
        }
        Ok(())
    }

    /// An enum holding lists of strings and numbers gets native blocks in a program with no
    /// other threads, and its drop lets go of each list, giving the runtime one with no owner
    /// left.
    #[test]
    fn an_enum_holding_lists_has_native_blocks() -> Result<(), String> {
        let source = "enum Doc:\n    Words(names: list[str], counts: list[int])\n    Pair(left: Doc, right: Doc)\n\ndef build(depth: int) -> Doc:\n    if depth == 0: return .Words(names=[\"a\", \"b\"], counts=[1, 2])\n    return .Pair(left=build(depth - 1), right=build(depth - 1))\n\ndef size(doc: Doc) -> int:\n    match doc:\n        case .Words(names, counts):\n            return len(names) + sum(counts)\n        case .Pair(left, right):\n            return size(left) + size(right)\n\ndef main(stdout: Stdout):\n    stdout.print(size(build(4)))\n";
        let (units, shared, _) = emit(&lowered(source)?)?;
        assert!(!shared.native.is_empty(), "Doc's blocks are native");
        let text: String = units.iter().map(|unit| unit.text.as_str()).collect();
        assert!(text.contains("call void @fennec_free_plain(ptr %list"), "{text}");
        Ok(())
    }

    /// A regex op that fails leaves the function, and the blocks made for its pattern and text
    /// go on its way out, not only after the op. No op on a pattern that built can be made to
    /// fail today, so this reads the text: the failing way out of each op lets go of a block
    /// before it leaves.
    #[test]
    fn a_regex_op_that_fails_lets_go_of_its_blocks_on_the_way_out() -> Result<(), String> {
        let source = "from std.re import Regex\n\ndef swapped(text: str) -> str:\n    return Regex(r\"\\w+\").replace(text, \"x\")\n\ndef main(stdout: Stdout):\n    stdout.print(swapped(\"one two\"))\n";
        let text = text_of(source, "swapped")?;
        let leaving: Vec<&str> = text.split("\nleaving").skip(1).collect();
        assert!(!leaving.is_empty(), "{text}");
        for way_out in leaving {
            // The way out's first lines: whether the block was made here, and letting go of it.
            let first: Vec<&str> = way_out.lines().take(3).collect();
            assert!(first.iter().any(|line| line.contains("text_made_gone")), "{first:?}");
        }
        Ok(())
    }

    /// A byte builder's push reads its room from the block's header, the word the runtime writes
    /// whenever it makes or grows one, rather than working it out from the length: so a varint
    /// that grew a builder for 10 bytes and wrote fewer leaves the rest as room for the next push.
    #[test]
    fn a_byte_builders_push_reads_its_room_from_the_header() -> Result<(), String> {
        let source = "from std import pack\n\ndef main(stdout: Stdout):\n    numbers = [n * 1000003 for n in range(100)]\n    stdout.print(len(pack.dumps(numbers)) > 0)\n";
        let (units, _, _) = emit(&lowered(source)?)?;
        let growing: Vec<&str> = units
            .iter()
            .map(|unit| unit.text.as_str())
            .filter(|text| text.contains("call ptr @fennec_bytes_grow("))
            .collect();
        assert!(!growing.is_empty(), "nothing pushes into a builder");
        for text in growing {
            // `%at = getelementptr inbounds i8, ptr %block, i64 24`, `%room = load i64, ptr %at`,
            // then `icmp ule i64 %wanted, %room`: the room compared is the word read.
            let lines: Vec<&str> = text.lines().map(str::trim).collect();
            let read = lines.windows(2).filter_map(|pair| {
                let [address, loaded] = pair else { return None };
                let at =
                    address.strip_suffix(", i64 24")?.split_once(" = getelementptr inbounds i8, ptr ")?.0;
                loaded.strip_suffix(&format!(" = load i64, ptr {at}"))
            });
            let compared: Vec<&str> = read
                .filter(|room| {
                    lines
                        .iter()
                        .any(|line| line.contains("icmp ule i64 ") && line.ends_with(&format!(", {room}")))
                })
                .collect();
            assert!(!compared.is_empty(), "no room read from the header and compared: {text}");
        }
        Ok(())
    }

    /// A native block's drop never calls itself, whichever field a chain runs through: those of
    /// its own type it lets go of wait in a list in one loop. Recursing through `left`, a chain
    /// 200,000 deep once ran past the thread sanitizer's 64K-frame stack, which then said a list was
    /// freed twice and hung.
    #[test]
    fn a_native_drop_never_calls_itself() -> Result<(), String> {
        let source = "enum Named:\n    Leaf(name: str)\n    Pair(left: Named, right: Named, label: str, tags: list[str])\n\ndef chain(depth: int) -> Named:\n    mut named = Named.Leaf(name=\"leaf\")\n    for at in range(depth):\n        named = .Pair(left=named, right=.Leaf(name=\"r\"), label=\"pair\", tags=[\"t\"])\n    return named\n\ndef size(named: Named) -> int:\n    match named:\n        case .Leaf(name):\n            return len(name)\n        case .Pair(left, right, label, tags):\n            return size(left) + size(right) + len(label) + len(tags)\n\ndef main(stdout: Stdout):\n    stdout.print(size(chain(4)))\n";
        let (units, shared, _) = emit(&lowered(source)?)?;
        assert!(!shared.native.is_empty(), "Named's blocks are native");
        let text: String = units.iter().map(|unit| unit.text.as_str()).collect();
        let drops: Vec<&str> = text.split("define internal void @\"fen_drop_").skip(1).collect();
        assert!(!drops.is_empty(), "{text}");
        for drop in drops {
            let body = drop.split("\n}\n").next().unwrap_or_default();
            assert!(!body.contains("call void @\"fen_drop_"), "{body}");
            assert!(body.contains("%waiting"), "{body}");
        }
        Ok(())
    }

    /// A build that watches native blocks, as a dev build does, tells the runtime of each one it
    /// makes and frees, for it to stop if it's ever given one.
    #[test]
    fn a_watched_build_tells_the_runtime_of_its_native_blocks() -> Result<(), String> {
        let source = "enum Twig:\n    Leaf\n    Node(left: Twig, right: Twig)\n\ndef build(depth: int) -> Twig:\n    if depth == 0: return .Leaf\n    return .Node(left=build(depth - 1), right=build(depth - 1))\n\ndef count(twig: Twig) -> int:\n    match twig:\n        case .Leaf:\n            return 1\n        case .Node(left, right):\n            return 1 + count(left) + count(right)\n\ndef main(stdout: Stdout):\n    stdout.print(count(build(4)))\n";
        let program = lowered(source)?;
        let (units, shared, _) = emit(&program)?;
        assert!(!shared.native.is_empty(), "Twig's blocks are native");
        let text: String = units.iter().map(|unit| unit.text.as_str()).collect();
        assert!(text.contains("call void @fennec_native_made("), "{text}");
        assert!(text.contains("call void @fennec_native_freed(ptr %block)"), "{text}");
        Ok(())
    }

    /// A sized number or a literal string elsewhere in a program holds no enum, so it leaves a
    /// tree's blocks native.
    #[test]
    fn sized_numbers_and_literal_strings_leave_blocks_native() -> Result<(), String> {
        let source = "enum Twig:\n    Leaf\n    Node(left: Twig, right: Twig)\n\ndef build(depth: int) -> Twig:\n    if depth == 0: return .Leaf\n    return .Node(left=build(depth - 1), right=build(depth - 1))\n\ndef count(twig: Twig) -> int:\n    match twig:\n        case .Leaf:\n            return 1\n        case .Node(left, right):\n            return 1 + count(left) + count(right)\n\ndef main(stdout: Stdout):\n    small: u8 = 3\n    wide = i32(7)\n    stdout.print(small, wide, \"tree\", count(build(4)))\n";
        let (_, shared, _) = emit(&lowered(source)?)?;
        assert!(!shared.native.is_empty(), "Twig's blocks are native");
        Ok(())
    }

    /// What fails two ways holds only what its errors hold, so a function or an arrow failing
    /// `Refused | Shut` elsewhere in a program leaves a tree's blocks native.
    #[test]
    fn failing_two_ways_leaves_blocks_native() -> Result<(), String> {
        let source = "enum Twig:\n    Leaf\n    Node(left: Twig, right: Twig)\n\nerror Refused:\n    code: int\n\nerror Shut:\n    code: int\n\ndef build(depth: int) -> Twig:\n    if depth == 0: return .Leaf\n    return .Node(left=build(depth - 1), right=build(depth - 1))\n\ndef count(twig: Twig) -> int:\n    match twig:\n        case .Leaf:\n            return 1\n        case .Node(left, right):\n            return 1 + count(left) + count(right)\n\ndef both(a: int) -> str fails Refused | Shut:\n    if a < 0: fail Refused(code=a)\n    if a == 0: fail Shut(code=a)\n    return \"open\"\n\ndef either(run: () -> str fails Refused | Shut) -> str:\n    return run() except Refused() | Shut(): \"failed\"\n\ndef main(stdout: Stdout):\n    both_value = both(-1) except Refused() | Shut(): \"failed\"\n    stdout.print(both_value, either(() => both(1)?), count(build(4)))\n";
        let (_, shared, _) = emit(&lowered(source)?)?;
        assert!(!shared.native.is_empty(), "Twig's blocks are native");
        Ok(())
    }

    /// A function that does nothing but `stop` can fail, so it's built taking the failure flag.
    /// The stop was missing from what can fail, so a helper whose whole body was one was taken as
    /// unable to and refused. Only the standard library can write the call, so the program is
    /// given one once it's lowered.
    #[test]
    fn a_function_that_only_stops_can_fail() -> Result<(), String> {
        let source = "def halts(why: str):\n    return\n\ndef main(stdout: Stdout):\n    halts(\"nobody is left to send to\")\n    stdout.print(\"went on\")\n    return\n";
        let mut program = lowered(source)?;
        let halts = program.functions.iter_mut().find(|function| function.name == "halts");
        let block = halts.and_then(|halts| halts.blocks.first_mut()).ok_or("no `halts`")?;
        let stop = InstKind::Builtin {
            dest: None,
            name: "intrinsics.stop".to_owned(),
            args: vec![Operand::Local(LocalId(0))],
            changes: false,
        };
        block.insts.insert(0, fennec_ir::Inst { kind: stop, span: Span::default() });
        let (units, shared, _) = emit(&program)?;
        assert!(shared.can_fail.contains("halts"), "{:?}", shared.can_fail);
        let text = units.into_iter().find(|unit| unit.function == "halts").map(|unit| unit.text);
        let text = text.ok_or("`halts` wasn't built")?;
        assert!(text.contains("call void @fennec_stop("), "{text}");
        assert!(text.contains("ptr swifterror"), "{text}");
        Ok(())
    }

    /// A loop over a slice of a list goes through the list between the slice's bounds, slicing
    /// nothing, whether a bound is a number, from the end or a name; a slice kept as a list is
    /// still made.
    #[test]
    fn a_loop_over_a_slice_slices_nothing() -> Result<(), String> {
        let source = "def main(stdout: Stdout):\n    lines = [\"a\", \"b\", \"c\"]\n    mut joined = \"\"\n    for line in lines[1:]:\n        joined += line\n    stdout.print(joined)\n";
        for bounds in ["[1:]", "[-1:]", "[:-1]", "[len(joined):len(lines)]"] {
            let walked = source.replace("[1:]", bounds);
            let text = text_of(&walked, "main")?;
            assert!(!text.contains("@fennec_list_slice("), "{bounds}: {text}");
        }
        let kept = "def main(stdout: Stdout):\n    lines = [\"a\", \"b\", \"c\"]\n    rest = lines[1:]\n    stdout.print(rest)\n";
        let text = text_of(kept, "main")?;
        assert!(text.contains("@fennec_list_slice("), "{text}");
        Ok(())
    }

    /// A loop over a text's parts looks for each separator from where the last part ended, and
    /// makes no list of them; `split` used as a list still makes one.
    #[test]
    fn a_loop_over_a_split_makes_no_list() -> Result<(), String> {
        let source = "def walked(text: str, stdout: Stdout):\n    mut count = 0\n    for part in text.split(\",\"):\n        count += len(part)\n    stdout.print(count)\n\ndef listed(text: str, stdout: Stdout):\n    parts = text.split(\",\")\n    stdout.print(len(parts))\n\ndef main(stdout: Stdout):\n    walked(\"a,b\", stdout)\n    listed(\"a,b\", stdout)\n";
        let walked = text_of(source, "walked")?;
        assert!(walked.contains("@fennec_str_split_at_header("), "{walked}");
        assert!(!walked.contains("@fennec_str_split_header("), "{walked}");
        let listed = text_of(source, "listed")?;
        assert!(listed.contains("@fennec_str_split_header("), "{listed}");
        Ok(())
    }

    /// A new struct key compared by its words goes into a dict that's alone, with room, by
    /// compiled code itself, and only to the runtime otherwise.
    #[test]
    fn a_new_struct_key_is_added_natively() -> Result<(), String> {
        let source = "struct Cell:\n    x: int\n    y: int\n\ndef main(stdout: Stdout):\n    mut seen: dict[Cell, int] = {}\n    for i in range(10):\n        seen[Cell(x=i, y=i)] = i\n    stdout.print(len(seen))\n";
        let text = text_of(source, "main")?;
        assert!(text.contains("call i1 @fen_add_plain("), "{text}");
        assert!(text.contains("call ptr @fennec_dict_set("), "{text}");
        Ok(())
    }

    /// A new int key goes into a dict of ints that's alone and has room by compiled code itself,
    /// and the runtime adds it otherwise.
    #[test]
    fn a_new_int_key_is_added_natively() -> Result<(), String> {
        let source = "def main(stdout: Stdout):\n    mut seen: dict[int, int] = {}\n    for i in range(10):\n        seen[i * 7] = i\n    stdout.print(len(seen))\n";
        let text = text_of(source, "main")?;
        assert!(text.contains("call i1 @fen_add_plain("), "{text}");
        assert!(text.contains("call ptr @fennec_dict_set("), "{text}");
        Ok(())
    }

    /// A new int key goes into a dict that holds its values in its entries by compiled code too:
    /// the add writes the slot, hash and key and gives back where the value's two words go, and
    /// the runtime's add, with the value through its buffer, is what's left when it can't.
    #[test]
    fn a_new_int_key_with_its_value_in_the_entry_is_added_natively() -> Result<(), String> {
        let source = "struct Visit:\n    cost: int\n    came_from: int\n\ndef main(stdout: Stdout):\n    mut seen = dict[int, Visit]()\n    for i in range(10):\n        seen[i * 7] = Visit(cost=i, came_from=i - 1)\n    stdout.print(len(seen))\n";
        let text = text_of(source, "main")?;
        assert!(text.contains("call ptr @fen_add_entry_e4("), "{text}");
        assert!(text.contains("call ptr @fennec_dict_set_words("), "{text}");
        Ok(())
    }

    /// A case with no fields is one block made the first time and kept in a global: put there
    /// with release and read with acquire, so another thread that sees it sees its count and tag.
    #[test]
    fn a_shared_case_is_published_with_release_and_read_with_acquire() -> Result<(), String> {
        let source = "enum Twig:\n    Leaf\n    Node(left: Twig, right: Twig, tag: str)\n\ndef leaf() -> Twig:\n    return .Leaf\n\ndef main(stdout: Stdout):\n    stdout.print(leaf() is Twig.Leaf)\n";
        let text = text_of(source, "leaf")?;
        assert!(
            text.contains("load atomic ptr, ptr @\"fen_case_") && text.contains("acquire, align 8"),
            "{text}"
        );
        assert!(text.contains("release, align 8"), "{text}");
        assert!(!text.contains("unordered"), "{text}");
        Ok(())
    }

    /// The symbols modules share name types by what they are, so a function's text is the same
    /// after a type is added before its own, or another function asks for new kinds of blocks
    /// first: a dict's shape (plain struct keys, struct values in the entry, list values), a kind
    /// of list item that's a block (lists of lists of lists too), an enum's case with no fields,
    /// kept in a global, and a native block's drop. They named layouts and kinds of blocks by
    /// numbers that follow the order of definitions and of asking, so each such edit wrote these
    /// functions again, and a function written once and used after could name another type. Each
    /// symbol is checked to be in the text, so the test can't pass on a text without it.
    #[test]
    fn a_function_reads_the_same_after_types_are_added_before_it() -> Result<(), String> {
        let program = "struct Spot:\n    x: int\n    y: int\n\nstruct Info:\n    hits: int\n    seen: bool\n\nenum Twig:\n    Leaf\n    Node(left: Twig, right: Twig, tag: str)\n\ndef shapes(keys: list[Spot]) -> int:\n    mut by_spot = dict[Spot, int]()\n    mut infos = dict[str, Info]()\n    mut runs = dict[str, list[int]]()\n    for key in keys:\n        by_spot[key] = key.x\n        infos[f\"{key.x}\"] = Info(hits=key.y, seen=True)\n        runs[f\"{key.y}\"] = [key.x]\n    return len(by_spot) + len(infos) + len(runs)\n\ndef nested(count: int) -> int:\n    mut rows = list[list[list[int]]]()\n    for at in range(count):\n        rows.append([[at]])\n    return len(rows)\n\ndef leaf() -> Twig:\n    return .Leaf\n\ndef main(stdout: Stdout):\n    stdout.print(shapes([Spot(x=1, y=2)]) + nested(3))\n    stdout.print(leaf() is Twig.Leaf)\n";
        let before = "struct Early:\n    first: int\n    second: int\n\nstruct Kept:\n    count: int\n    flag: bool\n\ndef early(count: int) -> int:\n    mut grid = list[list[str]]()\n    mut kept = dict[str, Kept]()\n    mut spots = dict[Early, list[str]]()\n    for at in range(count):\n        grid.append([f\"{at}\"])\n        kept[f\"{at}\"] = Kept(count=at, flag=True)\n        spots[Early(first=at, second=at)] = [f\"{at}\"]\n    return len(grid) + len(kept) + len(spots)\n\n";
        let with_early = format!(
            "{before}{}",
            program.replace(
                "def main(stdout: Stdout):\n",
                "def main(stdout: Stdout):\n    stdout.print(early(2))\n"
            )
        );
        let symbols: [(&str, &[&str]); 3] = [
            (
                "shapes",
                &[
                    "@\"d_Plain(Spot)_Int\"",
                    "@\"d_Text_InPlace(Info)\"",
                    "@\"d_Text_Block(List(Int))\"",
                    "@\"fen_drop_Twig\"",
                ],
            ),
            ("nested", &["@\"e_k_List(Block(List(Int)))\"", "@\"e_k_List(Int)\""]),
            ("leaf", &["@\"fen_case_Twig_0\""]),
        ];
        for (function, named) in symbols {
            let (alone, after) = (text_of(program, function)?, text_of(&with_early, function)?);
            for symbol in named {
                assert!(alone.contains(symbol), "`{function}` doesn't use {symbol}:\n{alone}");
            }
            assert!(
                alone == after,
                "`{function}` reads differently with types added before it:\n{alone}\n---\n{after}"
            );
        }
        Ok(())
    }

    /// What each unit of `source` read, shared and asked to be numbered, as a build keeping
    /// units keeps it.
    fn kept_of(source: &str) -> Result<(Vec<Kept>, Program), String> {
        let program = lowered(source)?;
        let (_, _, _, kept) = emit_keeping(&program, Cpu::Usual, Shared::default(), (true, None))?;
        Ok((kept, program))
    }

    /// A unit notes what it read of the whole program: its own analysis, a small function it
    /// holds a copy of, whether what it calls can fail. It keeps what it added to what units
    /// share, a kind of list item by its type's name, and what it asked to be numbered.
    #[test]
    fn a_unit_notes_what_it_reads_and_keeps_what_it_shares() -> Result<(), String> {
        use crate::reads::What;
        use fennec_layout::{Asked, Named};
        let source = "error Short:\n    excess: int\n\ndef tiny(value: int) -> int:\n    return value + 1\n\ndef bound(value: int) -> int fails Short:\n    if value < 0:\n        fail Short(excess=value)\n    return value\n\ndef total(items: list[list[int]]) -> int:\n    mut summed = 0\n    for item in items:\n        summed += tiny(len(item))\n    return summed\n\ndef main(stdout: Stdout):\n    stdout.print(total([[1], [2, 3]]))\n    checked_value = bound(tiny(4)) except Short(): -1\n    stdout.print(checked_value)\n    return\n";
        let (kept, _) = kept_of(source)?;
        let of = |name: &str| kept.iter().find(|kept| kept.function == name).ok_or(format!("no unit {name}"));
        let read = |kept: &Kept, what: What, name: &str| {
            kept.reads.iter().any(|(was, read)| *was == what && read == name)
        };
        let total = of("total")?;
        assert!(
            read(total, What::Analysis, "total") && read(total, What::Function, "tiny"),
            "{:?}",
            total.reads
        );
        let main = of("main")?;
        assert!(read(main, What::CanFail, "bound"), "{:?}", main.reads);
        let lists = Named::Block(Box::new(Named::List(Box::new(Named::Int))));
        assert!(
            total
                .asked
                .as_ref()
                .is_some_and(|asked| asked.contains(&Asked::Block(Named::List(Box::new(Named::Int))))),
            "{:?}",
            total.asked
        );
        let shared = kept.iter().filter_map(|kept| kept.share.added.as_ref()).flatten();
        assert!(
            shared.into_iter().any(|added| matches!(added, Added::Element(_, item) if *item == lists)),
            "no unit kept the list of lists' kind of item"
        );
        Ok(())
    }

    /// The audit a dev build runs on what each unit kept passes what writing a program kept, and
    /// names the unit when what it kept isn't so: a share missing what it added, one with
    /// something more, a share or an asking that couldn't be kept, and layouts numbered another
    /// way than asking again numbers them.
    #[test]
    fn the_audit_names_a_unit_whose_kept_share_isnt_what_it_added() -> Result<(), String> {
        let source = "def total(items: list[list[int]]) -> int:\n    mut summed = 0\n    for item in items:\n        summed += len(item)\n    return summed\n\ndef main(stdout: Stdout):\n    stdout.print(total([[1], [2, 3]]), \"done\")\n    return\n";
        let program = lowered(source)?;
        let (_, _, layouts, kept) = emit_keeping(&program, Cpu::Usual, Shared::default(), (true, None))?;
        crate::share::audit(&program, &layouts, &kept)?;
        let sharing = kept
            .iter()
            .position(|unit| {
                unit.function == "total" && unit.share.added.as_ref().is_some_and(|added| !added.is_empty())
            })
            .ok_or("`total` kept nothing it shares")?;
        let changed = |change: &dyn Fn(&mut Kept)| -> Vec<Kept> {
            let mut kept: Vec<Kept> = kept
                .iter()
                .map(|unit| Kept {
                    function: unit.function.clone(),
                    reads: unit.reads.clone(),
                    share: unit.share.clone(),
                    asked: unit.asked.clone(),
                    wrote: unit.wrote.clone(),
                })
                .collect();
            change(&mut kept[sharing]);
            kept
        };
        let audited = |kept: &[Kept]| crate::share::audit(&program, &layouts, kept);
        let less = changed(&|unit| {
            if let Some(added) = unit.share.added.as_mut() {
                added.pop();
            }
        });
        assert_eq!(
            audited(&less),
            Err("what `total` kept isn't what it added to what units share".to_owned())
        );
        let more = changed(&|unit| {
            if let Some(added) = unit.share.added.as_mut() {
                added.push(Added::String("s_extra".to_owned(), "extra".to_owned()));
            }
        });
        assert_eq!(
            audited(&more),
            Err("what `total` kept isn't what it added to what units share".to_owned())
        );
        let unshared = changed(&|unit| unit.share.added = None);
        assert_eq!(audited(&unshared), Err("`total` shared a kind with no name".to_owned()));
        let unasked = changed(&|unit| unit.asked = None);
        assert_eq!(audited(&unasked), Err("`total` asked for a number of a kind with no name".to_owned()));
        let other = lowered("def main(stdout: Stdout):\n    stdout.print({\"a\": [1.5]})\n    return\n")?;
        let (_, _, other_layouts, _) = emit_keeping(&other, Cpu::Usual, Shared::default(), (true, None))?;
        assert_eq!(
            crate::share::audit(&program, &other_layouts, &kept),
            Err("asking again for what the units asked numbers the layouts another way".to_owned())
        );
        Ok(())
    }

    /// What a unit keeps of the places it shares moves with its function: kept from a program,
    /// then added again for the same program with lines added above every function, it gives
    /// what writing that program gives, every place moved.
    #[test]
    fn a_kept_share_moves_with_its_function() -> Result<(), String> {
        let source = "def picked(items: list[int], index: int) -> int:\n    return items[index] // (index + 1)\n\ndef main(stdout: Stdout):\n    stdout.print(picked([3, 5, 7], 1))\n    return\n";
        let (before, _) = kept_of(source)?;
        let (after, program) = kept_of(&format!("# Two lines\n# above everything.\n{source}"))?;
        let anchors: HashMap<&str, Option<u32>> = program
            .functions
            .iter()
            .chain(&program.constants)
            .map(|function| (function.name.as_str(), crate::share::anchor(function)))
            .collect();
        let anchor_of = |name: &str| anchors.get(name).copied();
        let layouts = crate::layouts_of(&program)?;
        let (mut moved, mut written) = (Shared::default(), Shared::default());
        for (before, after) in before.iter().zip(&after) {
            assert_eq!(before.function, after.function);
            // As a build using the unit again does: its asking asked again, then what it kept.
            layouts.ask_again(after.asked.as_deref().unwrap_or_default()).ok_or("didn't ask again")?;
            before.share.add_to(&mut moved, &layouts, &anchor_of).ok_or("didn't add again")?;
            after.share.add_to(&mut written, &layouts, &anchor_of).ok_or("didn't add")?;
        }
        assert!(!written.places.values().all(Vec::is_empty), "no places to move: {:?}", written.places);
        assert_eq!(moved.places, written.places);
        assert_eq!(moved.starts, written.starts);
        Ok(())
    }

    /// A place at the program's very first byte is kept as it is, and the anchor a function's other
    /// places are kept from leaves it out: so a `main` written on the file's first line, part of
    /// which goes by its whole definition, keeps its other places moving with it, and its key (see
    /// the next step's `keys`) changes when text above it moves rather than leaving a place behind.
    #[test]
    fn a_place_at_the_first_byte_is_kept_as_it_is() -> Result<(), String> {
        let source = "def main(stdout: Stdout) fails Oops:\n    picked_value = picked([3, 5, 7], 1)\n    if picked_value > 6: fail Oops(why=picked_value)\n    stdout.print(picked_value)\n    return\n\nerror Oops:\n    why: int\n\ndef picked(items: list[int], index: int) -> int:\n    return items[index]\n";
        // Lowered as a build lowers it, which has `main` stop on a failure it gives back.
        let mut module = fennec_syntax::parse(source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let options =
            fennec_ir::Options { outline_parallel: true, stop_on_failed_main: true, serve_in_order: false };
        let program = fennec_ir::lower_with(&module, &checked, options);
        let main = program.functions.iter().find(|function| function.name == "main").ok_or("no main")?;
        let starts: Vec<u32> =
            main.blocks.iter().flat_map(|block| &block.insts).map(|inst| inst.span.start).collect();
        assert!(starts.contains(&0), "nothing of main is at byte 0: {starts:?}");
        let anchor = crate::share::anchor(main);
        assert!(anchor.is_some_and(|at| at > 0), "the anchor is {anchor:?}");
        assert_eq!(place(0, anchor), Place::At(0));
        Ok(())
    }

    /// A unit's kinds of block, asked for again in its place among units written fresh, get the
    /// numbers writing it would give, though a unit before it asks for more than it did when
    /// it was kept: so the numbers every unit after it gets come out as writing them all would.
    #[test]
    fn asking_again_numbers_a_kind_as_writing_did_after_others_asked_more() -> Result<(), String> {
        let helper = "def helper() -> int:\n    nested = [[1, 2], [3]]\n    names = {\"a\": [\"b\"]}\n    return len(nested) + len(names)\n\n";
        let main = "def main(stdout: Stdout):\n    stdout.print(early(), helper())\n    return\n";
        let plain = "def early() -> int:\n    return 1\n\n";
        let asking = "def early() -> int:\n    places = {\"x\": [[1.5]]}\n    return len(places)\n\n";
        let (kept, _) = kept_of(&format!("{plain}{helper}{main}"))?;
        let (fresh, program) = kept_of(&format!("{asking}{helper}{main}"))?;
        let at = |kept: &[Kept], name: &str| kept.iter().position(|kept| kept.function == name);
        assert!(at(&fresh, "early") < at(&fresh, "helper"), "early is written first");
        let layouts = crate::layouts_of(&program)?;
        let _ = crate::native::native_types(&program, &layouts);
        let kept_helper = kept.iter().find(|kept| kept.function == "helper").ok_or("no helper")?;
        for unit in &fresh {
            let unit = if unit.function == "helper" { kept_helper } else { unit };
            layouts.ask_again(unit.asked.as_deref().ok_or("unnamed")?).ok_or("didn't ask again")?;
        }
        let (_, _, written, _) = emit_keeping(&program, Cpu::Usual, Shared::default(), (true, None))?;
        assert_eq!(layouts.fingerprint(), written.fingerprint());
        Ok(())
    }

    /// A function that only hands its work on to another, as a serializer's `field` hands `x` to
    /// its `serialize`, is inlined wherever it's called, so what it calls is reached with one call
    /// fewer. One that calls itself, or does more, isn't made to be.
    #[test]
    fn a_function_that_only_forwards_is_inlined() -> Result<(), String> {
        let source = "def work(n: int) -> int:\n    mut total = 0\n    for i in range(n):\n        total += i * n\n    return total\n\n\
                      def forward(n: int) -> int:\n    return work(n)\n\n\
                      def more(n: int) -> int:\n    a = work(n)\n    b = work(a)\n    return a + b\n\n\
                      def main(stdout: Stdout):\n    stdout.print(forward(3), more(2))\n";
        let forward = text_of(source, "forward")?;
        assert!(forward.contains("@\"f_forward\"(") && forward.contains(") #2 {"), "{forward}");
        let more = text_of(source, "more")?;
        assert!(more.contains("@\"f_more\"(") && !more.contains("@\"f_more\"(i64 %p0) #2 {"), "{more}");
        Ok(())
    }

    #[test]
    fn a_loop_count_needs_no_overflow_check() -> Result<(), String> {
        let source = "def main(stdout: Stdout):\n    for i in range(3):\n        stdout.print(i)\n";
        assert!(!text_of(source, "main")?.contains("with.overflow"));
        Ok(())
    }

    /// A place taken out for a `ref` call goes back where it was before a crash lets go of what
    /// holds it: what holds it has the address of what the call may have moved or freed.
    #[test]
    fn a_crash_puts_a_place_that_is_out_back_first() -> Result<(), String> {
        let source = "struct Inner:\n    name: list[int]\n\nstruct Wrap:\n    r: Inner\n\ndef grow(ref xs: list[int], at: int):\n    xs.append(at)\n    empty: list[int] = []\n    xs[0] = empty[at]\n\ndef handle(at: int) -> int:\n    mut w = Wrap(r=Inner(name=[1]))\n    grow(ref w.r.name, at=at)\n    return len(w.r.name)\n\ndef main(stdout: Stdout):\n    stdout.print(handle(0))\n";
        let text = text_of(source, "handle")?;
        let prop = text.find("prop:").ok_or("no way out on a crash")?;
        let put_back =
            text.get(prop..).and_then(|rest| rest.find("crash_place")).ok_or_else(|| text.clone())?;
        let let_go = text
            .get(prop..)
            .and_then(|rest| rest.find("@fen_owner_gone").or_else(|| rest.find("add i64 %")))
            .unwrap_or(usize::MAX);
        assert!(put_back < let_go, "{text}");
        Ok(())
    }

    #[test]
    fn what_it_cant_take_says_which_function() -> Result<(), String> {
        // Sorting lists of lists isn't built natively yet.
        let source = "def f() -> int:\n    xs = sorted([[2], [1]])\n    return len(xs)\n\ndef main(stdout: Stdout):\n    stdout.print(f())\n";
        let error = emit(&lowered(source)?).err().unwrap_or_default();
        assert!(error.contains("`f`"), "{error}");
        Ok(())
    }
}

/// Whether a function is the standard library's, by where its code is written.
fn written_in_std(function: &Function) -> bool {
    let mut spans = function.blocks.iter().flat_map(|block| &block.insts).map(|inst| inst.span);
    spans.find(|span| *span != Span::default()).is_some_and(Span::in_prelude)
}

/// Whether a function is one `fennec test` makes for a test, named `test:` and its number, or for
/// an attempt of a property test's shrinking.
/// How many locals a function checks empty one at a time at each return, in a build that checks
/// proofs. Past this it checks them through a table: see [`Emit::check_all_empty`].
const CHECKED_ONE_BY_ONE: usize = 256;

/// The locals a function checks empty through a table, in a build that checks proofs.
struct EmptyTable {
    /// Each local's place in the table.
    places: HashMap<usize, usize>,
    /// The constant holding how many words each is.
    sizes: String,
    /// The constants written so far, each by the bits it holds: a bit for each local a return
    /// wants empty.
    masks: HashMap<Vec<u64>, String>,
}

/// Ends the program, as a broken proof does, unless every local `%mask` has a bit for is empty:
/// each word of it zero. `%slots` holds where each of `%count` locals is, and `%words` how many
/// words it is. Kept out of line: it's called at every return of a function with many locals.
const PROOF_EMPTY: &str = "define internal void @fen_proof_empty(ptr nocapture readonly %slots, ptr nocapture readonly %words, ptr nocapture readonly %mask, i64 %count, ptr %proof, i64 %from, i64 %to) noinline #0 {
entry:
  br label %next
next:
  %at = phi i64 [ 0, %entry ], [ %after, %step ]
  %done = icmp eq i64 %at, %count
  br i1 %done, label %out, label %test
test:
  %mask_word = lshr i64 %at, 6
  %mask_at = getelementptr inbounds i64, ptr %mask, i64 %mask_word
  %bits = load i64, ptr %mask_at
  %bit = and i64 %at, 63
  %shifted = lshr i64 %bits, %bit
  %one = and i64 %shifted, 1
  %wanted = icmp ne i64 %one, 0
  br i1 %wanted, label %check, label %step
check:
  %slot_at = getelementptr inbounds ptr, ptr %slots, i64 %at
  %slot = load ptr, ptr %slot_at
  %size_at = getelementptr inbounds i32, ptr %words, i64 %at
  %size32 = load i32, ptr %size_at
  %size = zext i32 %size32 to i64
  br label %word
word:
  %which = phi i64 [ 0, %check ], [ %which_next, %word_step ]
  %words_done = icmp eq i64 %which, %size
  br i1 %words_done, label %step, label %read
read:
  %held_at = getelementptr inbounds i64, ptr %slot, i64 %which
  %held = load i64, ptr %held_at
  %full = icmp ne i64 %held, 0
  br i1 %full, label %broken, label %word_step
word_step:
  %which_next = add i64 %which, 1
  br label %word
step:
  %after = add i64 %at, 1
  br label %next
broken:
  call void @fennec_proof_broken(ptr %proof, i64 %from, i64 %to)
  unreachable
out:
  ret void
}

";

pub(crate) fn is_test(name: &str) -> bool {
    name.strip_prefix("test:")
        .is_some_and(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
        || is_attempt(name)
}

/// Whether a function is one `fennec test` makes for an attempt of a property test's shrinking,
/// named `test:`, the test's number and `_attempt`.
fn is_attempt(name: &str) -> bool {
    name.strip_prefix("test:")
        .and_then(|rest| rest.strip_suffix("_attempt"))
        .is_some_and(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
}

/// A sized number type's name with its article, read letter by letter: "a u8", "an i32".
fn article_of(ty: &fennec_check::Type) -> String {
    match ty {
        fennec_check::Type::Sized(num) if num.is_unsigned() => format!("a {ty}"),
        _ => format!("an {ty}"),
    }
}
