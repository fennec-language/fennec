//! Writing functions as LLVM's text. Each local is a stack slot and every read and write goes
//! through it, which LLVM's first pass turns into proper SSA.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;

use fennec_ir::{FormatPart, Function, GeneratorDef, InstKind, LocalId, Operand, Program, Terminator};
use fennec_layout::Layouts;
use fennec_rt::embed::layout::COUNT;
use fennec_syntax::{BinOp, Span, UnaryOp};

use crate::analysis::{Analysis, Whole, callees, visit, whole};
use crate::kinds::{Kind, struct_definitions};
use crate::uniqueness::{FieldPath, Tracked, coming_in, unsettled, written_order};
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
!12 = !{!11, !11, i64 0}\n";

/// Where a string block's bytes start.
pub(crate) const TEXT_START: u32 = fennec_rt::embed::layout::TEXT.unsigned_abs();

/// A list's header, and what's in it, never overlap its items, and a length is far from any limit.
pub(crate) const HEADER_ACCESS: &str = ", !tbaa !4";
pub(crate) const ITEM_ACCESS: &str = ", !tbaa !6";
/// A boxed struct's or enum's fields never overlap a block's header or a list's items.
pub(crate) const FIELD_ACCESS: &str = ", !tbaa !10";
pub(crate) const LENGTH_ACCESS: &str = ", !tbaa !4, !range !7";
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
    pub text: String,
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

pub(crate) fn emit_with(
    program: &Program,
    cpu: Cpu,
    mut shared: Shared,
) -> Result<(Vec<Unit>, Shared, Layouts), String> {
    let header = crate::header()?;
    let attributes = attributes(cpu);
    if PROOFS_CHECKED.load(std::sync::atomic::Ordering::Relaxed) {
        shared.proofs = crate::Proofs::Checked;
    }
    shared.native_blocks = if NATIVE_TRUSTED.load(std::sync::atomic::Ordering::Relaxed)
        && shared.proofs != crate::Proofs::Checked
    {
        crate::NativeBlocks::Trusted
    } else {
        crate::NativeBlocks::Watched
    };
    let everything = whole(program);
    shared.can_fail = everything.can_fail.iter().map(|name| (*name).to_owned()).collect();
    let layouts = crate::layouts_of(program);
    let mut units = Vec::with_capacity(program.functions.len());
    // Every module of a program agrees, since what it declares has to match what's defined.
    let threads = crate::parallel::uses_parallel(program);
    shared.native = crate::native::native_types(program, &layouts);
    let waits = crate::waits::may_wait(program, true);
    let closures =
        program.functions.iter().flat_map(|function| &function.blocks).flat_map(|block| &block.insts);
    shared.waiting_closures = closures
        .filter_map(|inst| match &inst.kind {
            InstKind::MakeClosure { function, .. } if waits.marked.contains_key(function) => {
                Some(function.clone())
            }
            _ => None,
        })
        .collect();
    shared.waiting = waits.marked.into_keys().collect();
    // Every function it can't take yet is named, not just the first.
    let mut refused = Vec::new();
    for function in program.functions.iter().chain(&program.constants) {
        match unit(function, (&everything, threads), &layouts, &mut shared, (header, &attributes)) {
            Ok(text) => units.push(Unit { function: function.name.clone(), text }),
            Err(why) => refused.push(format!("`{}`: {why}", function.name)),
        }
    }
    if !refused.is_empty() {
        return Err(refused.join("\n"));
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
    Ok((units, shared, layouts))
}

fn size(function: &Function) -> usize {
    function.blocks.iter().map(|block| block.insts.len() + 1).sum()
}

/// Whether a function is small enough to copy into its callers, with everything it calls, which
/// must be too, `depth` calls deep. A recursive one isn't.
fn copyable(whole: &Whole<'_>, function: &Function, depth: usize) -> bool {
    size(function) <= COPIED
        && !whole.constants.contains(function.name.as_str())
        && callees(function).all(|name| {
            depth > 0
                && name != function.name
                && whole.functions.get(name).is_some_and(|callee| copyable(whole, callee, depth - 1))
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
    pub(crate) whole: &'w Whole<'p>,
    pub(crate) layouts: &'w Layouts,
    pub(crate) shared: &'w mut Shared,
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
}

fn unit<'p>(
    function: &'p Function,
    (whole, threads): (&Whole<'p>, bool),
    layouts: &Layouts,
    shared: &mut Shared,
    (header, attributes): (&str, &str),
) -> Result<String, String> {
    let mut module = Module {
        whole,
        layouts,
        shared,
        threads,
        cells: threads && whole.cells,
        declares: BTreeSet::new(),
        defined: HashSet::new(),
        bodies: String::new(),
        helpers: BTreeMap::new(),
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
        let Some(callee) = whole.functions.get(name).copied() else { continue };
        if name == function.name
            || copies.iter().any(|copy| copy.name == name)
            || !copyable(whole, callee, COPY_DEPTH)
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
    let small = size(function) <= ALWAYS_INLINE && callees(function).next().is_none();
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
    Ok(out)
}

impl<'p> Module<'_, 'p> {
    pub(crate) fn add_helper(&mut self, name: &str, text: String) {
        self.helpers.entry(name.to_owned()).or_insert(text);
    }

    pub(crate) fn function_named(&self, name: &str) -> Result<&'p Function, String> {
        self.whole
            .functions
            .get(name)
            .copied()
            .ok_or_else(|| format!("it uses `{name}`, which isn't lowered"))
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
        self.whole.generators.get(name).copied()
    }

    pub(crate) fn flagged(&self, name: &str) -> bool {
        self.whole.can_fail.contains(name)
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
        if returns.in_memory() {
            params.push(if named { "ptr noalias captures(none) %ret".to_owned() } else { "ptr".to_owned() });
        }
        for (at, local) in function.locals.iter().take(function.params).enumerate() {
            let kind = self.kind(&local.ty)?;
            let by_address = function.mut_params.contains(&at);
            let ty = if by_address || kind.in_memory() { "ptr".to_owned() } else { kind.slot(self.layouts) };
            params.push(match (named, by_address) {
                (false, _) => ty,
                (true, true) => format!("ptr noalias captures(none) %p{at}"),
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
        self.shared.starts.insert(start_symbol.clone(), start);
        self.declare(&format!("@\"{start_symbol}\" = external dso_local global i64"));
        let failed = if self.flagged(&function.name) {
            let leave = if ret == "void" { "ret void".to_owned() } else { format!("ret {ret} poison") };
            format!("store ptr inttoptr (i64 1 to ptr), ptr %err\n  {leave}")
        } else {
            "unreachable".to_owned()
        };
        let known = self.whole.analyses.get(function.name.as_str()).ok_or("no analysis")?;
        let mut kinds = Vec::with_capacity(function.locals.len());
        let joined = self.whole.builders.get(function.name.as_str());
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
            failed,
            leaves: std::cell::Cell::new(false),
            needs_prop: false,
            start,
            start_symbol,
            self_target,
            taken_at: BTreeMap::new(),
            found_entries: Vec::new(),
            alone_slots: HashSet::new(),
            alone_fields: Vec::new(),
            taken_items: HashMap::new(),
            place_homes: HashMap::new(),
            places_out: HashMap::new(),
            at: (usize::MAX, usize::MAX),
            check_kind: None,
            inside_items: Vec::new(),
            inside_slots: HashMap::new(),
            pair_slots: HashMap::new(),
            walks: HashMap::new(),
            saving: crate::rollback::saving_handles(function),
            on_crash: crate::rollback::given_back_on_crash(function),
            saving_slots: HashMap::new(),
            rolled_back: HashSet::new(),
            wrote_prop: false,
            traced_calls: Vec::new(),
            leave_after_crash: None,
        };
        // A function with locks a crash gives back, or that owns anything, leaves on every failure
        // through `prop`, where a crash puts back what the blocks it's inside changed, gives their
        // locks back and lets go of what the function owns, as a return does.
        let owns_any =
            (0..emit.kinds.len()).any(|at| emit.owns(LocalId(at)) || emit.owns_inline(LocalId(at)));
        let leave_after_crash = ((!emit.on_crash.is_empty() || owns_any) && emit.failed != "unreachable")
            .then(|| std::mem::replace(&mut emit.failed, "br label %prop".to_owned()));
        emit.leave_after_crash = leave_after_crash;
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

/// One function being written.
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
    failed: String,
    /// Whether a way out for a failure was written.
    leaves: std::cell::Cell<bool>,
    needs_prop: bool,
    start: u32,
    start_symbol: String,
    self_target: Option<&'e str>,
    /// Where each item taken out of a list to change was, to put it back without looking again.
    pub(crate) taken_at: BTreeMap<usize, String>,
    /// Where lookups since the last change found their keys: the dict's local, the key, and the
    /// entry or null.
    pub(crate) found_entries: Vec<(LocalId, Operand, String)>,
    /// Slots of `mut` parameters made alone on the way in, that nothing here can share since.
    alone_slots: HashSet<String>,
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
    /// Items of lists, by field path and index, known to be inside their lists here.
    pub(crate) inside_items: Vec<FieldPath>,
    /// Where the position each was found at is kept, negative indexes counted from the end.
    pub(crate) inside_slots: HashMap<FieldPath, String>,
    /// Where each pair from an `enumerate` never made points in its list, kept for its parts.
    pub(crate) pair_slots: HashMap<usize, String>,
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

    /// Adds a block away from the hot path.
    pub(crate) fn cold_block(&mut self, text: &str) {
        self.cold.push_str(text);
    }

    pub(crate) fn declare(&mut self, line: &str) {
        self.module.declare(line);
    }

    /// Whether a slot's block was made alone on the way in and can't have been shared since.
    pub(crate) fn alone_slots_contains(&self, place: &str) -> bool {
        self.alone_slots.contains(place)
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

    fn is_mut_param(&self, local: LocalId) -> bool {
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
        if self.keeps_its_value() {
            self.give_back_what_it_kept();
        }
        if function.name == "main" && self.module.threads {
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
        // What callers see to before calling.
        let whole = self.module.whole;
        if let Some(needs) = whole.needs.get(function.name.as_str()) {
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
        // Every caller passes the flag clear, so saying so lets a check after an inlined call
        // that didn't fail go away. An assumption, as a store would be inlined into loops.
        if self.module.flagged(&function.name) {
            self.declare("declare void @llvm.assume(i1)");
            self.line("%err_in = load ptr, ptr %err");
            self.line("%err_clear = icmp eq ptr %err_in, null");
            self.line("call void @llvm.assume(i1 %err_clear)");
        }
        if self.ticks() {
            self.start_ticks();
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
        let settles = (&whole.settles, Tracked::Owners);
        let lengths = (&whole.lengths, Tracked::Lengths);
        // Each block after every one leading to it, but for loops, so what's known carries on.
        let mut done = HashSet::new();
        for at in written_order(function) {
            let Some(block) = function.blocks.get(at) else { continue };
            self.start_block(&format!("bb{at}"));
            self.write_kept_lengths(at);
            let ways_in = before.get(&at).map_or(&[][..], Vec::as_slice);
            self.alone_fields =
                coming_in(function, self.known, settles, (at, &before, &done), (&on_entry, &alone_left));
            self.inside_items =
                coming_in(function, self.known, lengths, (at, &before, &done), (&[], &inside_left));
            self.found_entries = match ways_in.split_first() {
                Some((first, rest)) if ways_in.iter().all(|from| done.contains(from)) => {
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
                self.forget_entries(&inst.kind);
                self.take_waiting_places(&inst.kind, inst.span)?;
                if let InstKind::Call { function: callee, args, .. } = &inst.kind {
                    for (param, path) in whole.needs.get(callee.as_str()).map_or(&[][..], Vec::as_slice) {
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
            }
            left_with.insert(at, std::mem::take(&mut self.found_entries));
            alone_left.insert(at, std::mem::take(&mut self.alone_fields));
            inside_left.insert(at, std::mem::take(&mut self.inside_items));
            done.insert(at);
            if let Some((list, limit)) = self.known.reservations.get(&at).cloned()
                && matches!(self.kinds.get(list), Some(Kind::List(_)))
            {
                self.reserve(LocalId(list), &limit)?;
            }
            // Every loop goes back to a block written no later than where it goes from, so that's
            // where a task counts a turn.
            if self.ticks() && block.end.targets().iter().any(|next| next.0 <= at) {
                self.tick();
            }
            self.terminator(&block.end)?;
        }
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
            self.release_locals(None);
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

    /// One owner fewer for a value of this kind.
    pub(crate) fn release_kind(&mut self, value: &str, kind: &Kind) {
        if (*kind == Kind::Str && self.headers()) || *kind == Kind::MaybeText {
            self.release_text(value);
        } else if matches!(kind, Kind::Closure(..)) {
            self.release_closure(value);
        } else {
            self.release(value);
        }
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
        self.module.threads && self.module.whole.on_workers.contains(self.function.name.as_str())
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

    /// One owner fewer for a block, if there's a block, freeing it when that was the last.
    pub(crate) fn release(&mut self, block: &str) {
        debug_assert_eq!(COUNT, 0);
        // A program's native types are layouts it has, so finding their free can't fail.
        let free = self.free_function().unwrap_or("@fennec_free");
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

    /// Where a span is, from the symbol for where this function starts, for failing with.
    pub(crate) fn span_values(&mut self, fail: &str, span: Span) -> (String, String) {
        let symbol = self.start_symbol.clone();
        let (from, to) = (span.start.saturating_sub(self.start), span.end.saturating_sub(self.start));
        let text = format!(
            "  %s{fail} = load i64, ptr @\"{symbol}\"\n  %a{fail} = add i64 %s{fail}, {from}\n  %b{fail} = add i64 %s{fail}, {to}\n"
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
        self.failed.clone()
    }

    /// Says what the next way out on a failure is for, or with `None` that it isn't a check.
    pub(crate) fn next_check(&mut self, span: Span, what: Option<CheckKind>) {
        self.check_kind = Some((span, what));
    }

    /// Leaves the function, the failure already recorded, when `failed` holds.
    pub(crate) fn leave_if(&mut self, failed: &str) {
        let ok = self.label("ok");
        self.needs_prop = true;
        self.line(&format!("br i1 {failed}, label %prop, label %{ok}, !prof !0"));
        self.start_block(&ok);
    }

    /// As [`Self::leave_if`], after a call into a function of the program: a failure out of it
    /// says this call is in its call chain on the way, away from the path that doesn't fail.
    /// Every such call leaves by one block, which a phi tells which call it was: a branch per call
    /// and nothing more, so the calls' code is what it would be without it.
    pub(crate) fn leave_traced(&mut self, failed: &str, span: Span) {
        let ok = self.label("ok");
        self.needs_prop = true;
        self.traced_calls.push((self.current.clone(), span.start.saturating_sub(self.start)));
        self.line(&format!("br i1 {failed}, label %left_traced, label %{ok}, !prof !0"));
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
        let symbol = self.start_symbol.clone();
        let _ = writeln!(
            self.cold,
            "left_traced:\n  %left_from = phi i64 {}\n  %left_start = load i64, ptr @\"{symbol}\"\n  %left_at = add i64 %left_start, %left_from\n  call void @fennec_trace_frame(i64 %left_at, i64 %left_at)\n  br label %prop",
            arms.join(", ")
        );
    }

    /// As [`Self::leave_if`], after a call from the program into the standard library: a failure
    /// inside it is said to be here, where the program called it, as the interpreter says it.
    fn leave_pointing_here(&mut self, failed: &str, span: Span) {
        let (here, ok) = (self.label("from_std"), self.label("ok"));
        self.needs_prop = true;
        self.line(&format!("br i1 {failed}, label %{here}, label %{ok}, !prof !0"));
        self.start_block(&ok);
        self.declare("declare void @fennec_at_call(i64, i64) nounwind");
        let (place, args) = self.span_values(&here, span);
        let _ = writeln!(self.cold, "{here}:\n{place}  call void @fennec_at_call({args})\n  br label %prop");
    }

    /// Where the instruction being written is in the source.
    pub(crate) fn here_span(&self) -> Span {
        let block = self.function.blocks.get(self.at.0);
        block.and_then(|block| block.insts.get(self.at.1)).map_or_else(Span::default, |inst| inst.span)
    }

    /// Whether checks left out because of a proof are written anyway, as assertions.
    pub(crate) fn checks_proofs(&self) -> bool {
        self.module.shared.proofs == crate::Proofs::Checked
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
        if self.module.shared.noting_checks {
            let function = self.function.name.clone();
            self.module.shared.checks.push(CheckLeft { function, span, what });
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
        if !self.module.shared.noting_checks {
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

    /// Whether a local holds an inline struct or enum with blocks in it, and owns them.
    /// Whether `dest = lhs + more` is a text growing, from a local that holds its only owner or
    /// took it from a field.
    fn grows_in_place(&self, op: BinOp, lhs: &Operand, dest: LocalId) -> bool {
        op == BinOp::Add && *lhs == Operand::Local(dest) && (self.owns(dest) || self.known.is_taken(dest))
    }

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
    fn within_width(&mut self, value: &str, num: fennec_check::Num) -> String {
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
            Kind::Boxed(_) | Kind::Bytes | Kind::List(_) | Kind::Dict(..) | Kind::Maybe(_)
                if matches!(op, BinOp::Eq | BinOp::NotEq) =>
            {
                self.same_blocks(op, (&a, &b))
            }
            Kind::Closure(..) if matches!(op, BinOp::Eq | BinOp::NotEq) => self.same_code(op, (&a, &b)),
            Kind::List(_) if op == BinOp::Add => self.joined_lists(&a, &b),
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
                let (from, to) = (i64::from(span.start), i64::from(span.end));
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
            // By a positive number, the remainder is always from 0 up to it: said to LLVM, a value
            // worked out from it again, as in `x = x * k % m`, needs no fix-up for a sign it can't
            // have, and is the machine's remainder alone.
            if let Operand::Int(divisor @ 1..) = rhs {
                self.declare("declare void @llvm.assume(i1)");
                let (low, high) = (self.temp(), self.temp());
                self.line(&format!("{low} = icmp sge i64 {result}, 0"));
                self.line(&format!("call void @llvm.assume(i1 {low})"));
                self.line(&format!("{high} = icmp slt i64 {result}, {divisor}"));
                self.line(&format!("call void @llvm.assume(i1 {high})"));
            }
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
        let callee = *self
            .module
            .whole
            .functions
            .get(name)
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
                lent.push(value.clone());
            }
            // Checking proofs, a borrowed block lent to a call is held for it too, to see after
            // that what it was borrowed from still holds it.
            if self.checks_proofs()
                && matches!(arg, Operand::Local(local) if self.known.is_borrowed(*local))
                && matches!(kind, Kind::List(_) | Kind::Dict(..) | Kind::Boxed(_) | Kind::TextBlock)
            {
                borrows_lent.push(value.clone());
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
        for borrowed in &borrows_lent {
            self.when_made(borrowed, |emit| emit.retain(borrowed));
        }
        let result = self.temp();
        if ret == "void" {
            self.line(&format!("call swiftcc void {target}({shown})"));
        } else {
            self.line(&format!("{result} = call swiftcc {ret} {target}({shown})"));
        }
        for borrowed in &borrows_lent {
            self.when_made(borrowed, |emit| {
                // Its one owner now is the one taken for the call: what it was borrowed from let
                // go of it while the call ran, and the borrow was read after.
                let alone = emit.alone(borrowed);
                emit.proof_held(
                    &alone,
                    "what a value lent to a call was borrowed from holds it till the call ends",
                    span,
                );
                emit.release(borrowed);
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
        for block in lent {
            self.release(&block);
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

    fn builtin(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
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
                let (list, _) = self.operand(counted.as_ref().or(first).ok_or("no argument")?)?;
                let length = self.list_length(&list);
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
            | "intrinsics.table_tag"
            | "intrinsics.new_tag"
            | "intrinsics.table_tagged"
            | "intrinsics.table_slots"
            | "intrinsics.table_of_slots"
            | "intrinsics.id_of_key" => self.id_intrinsic(dest, name, args),
            "intrinsics.bytes_text" => self.bytes_text(dest.ok_or("no result")?, args),
            "intrinsics.bytes_builder"
            | "intrinsics.bytes_push"
            | "intrinsics.bytes_push_word"
            | "intrinsics.bytes_push_text"
            | "intrinsics.bytes_push_bytes"
            | "intrinsics.bytes_set" => self.byte_builder(dest, name, args),
            "intrinsics.text_plain_end" | "intrinsics.text_escape_at" => {
                self.text_scan(dest.ok_or("no result")?, name, args)
            }
            // A float's bits as an int, and back, which a binary format saves.
            "intrinsics.float_bits" | "intrinsics.float_of_bits" => {
                let (value, _) = self.operand(first.ok_or("no argument")?)?;
                let (from, to) =
                    if name == "intrinsics.float_bits" { ("double", "i64") } else { ("i64", "double") };
                let cast = self.temp();
                self.line(&format!("{cast} = bitcast {from} {value} to {to}"));
                self.assign(dest.ok_or("no result")?, &cast)
            }
            "intrinsics.stop" => {
                let (value, kind) = self.operand(first.ok_or("no argument")?)?;
                let block =
                    if kind == Kind::Str && self.headers() { self.owned_text_block(&value) } else { value };
                self.declare("declare ptr @fennec_block_text(ptr) nounwind");
                let text = self.temp();
                self.line(&format!("{text} = call ptr @fennec_block_text(ptr {block})"));
                self.stop_with(&text, span);
                Ok(())
            }
            "random.random_int" | "random.random_float" | "random_choice" | "random.random_choice" => {
                self.random_builtin(dest, name, args, span)
            }
            parallel if parallel.starts_with("parallel.") => self.parallel_builtin(dest, name, args, span),
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
            "str.len" | "str" | "str.code" | "str.join" | "str.contains" | "str.upper" | "str.lower"
            | "str.replace" | "str.find" | "str.split" => self.text_builtin(dest, name, args, span),
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
                self.declare("declare double @llvm.sqrt.f64(double)");
                let temp = self.temp();
                self.line(&format!("{temp} = call double @llvm.sqrt.f64(double {value})"));
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
            // A capability's method, run by the runtime as the interpreter runs it.
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
            if self.is_optional(&wanted) {
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
        // A borrowing local takes no owner, so its copy needs none either.
        if kind.counted() && !self.known.is_borrowed(dest) {
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
        if !self.module.shared.tracing_costs {
            return;
        }
        let Some(what) = what_can_cost(inst) else { return };
        self.module.shared.sites.entry(span.start).or_insert(what);
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
            InstKind::GetOk { dest, value } => self.get_case_field(*dest, value, "Ok", "value"),
            InstKind::GetErr { dest, value } => self.get_case_field(*dest, value, "Err", "error"),
            InstKind::IsError { dest, value, type_name } => self.is_case(*dest, value, type_name),
            InstKind::AsError { dest, value } => self.as_error(*dest, value),
            InstKind::GetField { dest, object, field } => self.get_field(*dest, object, field),
            InstKind::SetField { object, path, value } => self.set_field(*object, path, value),
            InstKind::MakeTuple { dest, items } => self.make(*dest, items),
            InstKind::GetTupleItem { dest, value: Operand::Local(pair), at }
                if self.known.walk_items.contains_key(&pair.0) =>
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
                self.builtin(*dest, name, args, span)
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
        self.found_entries.retain(|(dict, key, _)| {
            !written.contains(dict) && !matches!(key, Operand::Local(local) if written.contains(local))
        });
    }

    /// A local read once lets go of its value right after that, rather than when the function
    /// returns, so it doesn't still count as an owner when the next line changes what it read.
    fn let_go_after(&mut self, inst: &InstKind) {
        // A slice never made leaves what it's of to the lookups that read it in its place.
        if let InstKind::Slice { dest, .. } | InstKind::Copy { dest, .. } = inst
            && self.known.unmade.contains(&dest.0)
        {
            return;
        }
        // Nor does an `enumerate` never made, whose list its pairs read in its place.
        if let InstKind::Builtin { dest: Some(dest), .. } | InstKind::Copy { dest, .. } = inst
            && (self.known.enumerated.contains_key(&dest.0) || self.known.map_walks.contains_key(&dest.0))
        {
            return;
        }
        let (mut reads, mut writes) = (Vec::new(), Vec::new());
        visit(inst, |operand| reads.push(operand.clone()), |local| writes.push(local));
        // What's made from it may only be borrowing, or may stand for it.
        let leans = writes.iter().any(|dest| {
            self.known.is_borrowed(*dest) || self.known.is_taken(*dest) || self.known.alias(*dest).is_some()
        });
        if leans {
            return;
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
        for local in done_with {
            if self.owns(local) && self.known.moves(local) && !writes.contains(&local) {
                let kind = self.kinds.get(local.0).cloned().unwrap_or(Kind::Unit);
                let (held, slot, text) = (self.temp(), self.slot(local), self.text_of(&kind));
                self.line(&format!("{held} = load {text}, ptr {slot}"));
                self.line(&format!("store {text} zeroinitializer, ptr {slot}"));
                self.release_kind(&held, &kind);
            }
        }
    }

    /// Everything local lets go of what it owns, except what's leaving.
    fn release_locals(&mut self, except: Option<LocalId>) {
        for at in 0..self.kinds.len() {
            let local = LocalId(at);
            if Some(local) != except && self.owns(local) {
                let kind = self.kinds.get(at).cloned().unwrap_or(Kind::Unit);
                let (held, slot, text) = (self.temp(), self.slot(local), self.text_of(&kind));
                self.line(&format!("{held} = load {text}, ptr {slot}"));
                self.release_kind(&held, &kind);
            } else if Some(local) != except && self.owns_inline(local) {
                let kind = self.kinds.get(at).cloned().unwrap_or(Kind::Unit);
                let slot = self.slot(local);
                self.release_inline(&kind, &slot);
            }
        }
    }

    /// Whether this function gives a constant's value, which is worked out once and then kept. In
    /// a program that uses `parallel`, one that can fail is worked out at each read instead, since
    /// `main` works out the kept ones before any worker starts, whether or not they're ever read.
    fn keeps_its_value(&mut self) -> bool {
        let returns = self.returns.clone();
        let name = self.function.name.as_str();
        self.module.whole.constants.contains(name)
            && (!self.module.threads || !self.module.flagged(name))
            && self.text_of(&returns) != "void"
    }

    /// Starts `main` in a program that uses `parallel`: every constant that's kept gets kept here,
    /// so a worker only ever reads it, and two can't race to be the first.
    fn work_out_constants(&mut self) -> Result<(), String> {
        let mut constants: Vec<&str> =
            self.module.whole.constants.iter().copied().filter(|name| !self.module.flagged(name)).collect();
        constants.sort_unstable();
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
        } else if returns.counted() {
            self.retain_value(held, &returns);
        }
        // A constant held in a block lives for the whole run, so a task anywhere can read it.
        if matches!(returns, Kind::List(_) | Kind::Dict(..) | Kind::Boxed(_) | Kind::Bytes) {
            self.declare("declare void @fennec_constant_kept(ptr) nounwind");
            self.line(&format!("call void @fennec_constant_kept(ptr {held})"));
        }
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
            self.line("ret void");
            return Ok(());
        }
        if text == "void" {
            self.release_locals(None);
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
        self.line(&format!("ret {text} {held}"));
        Ok(())
    }

    fn terminator(&mut self, end: &Terminator) -> Result<(), String> {
        match end {
            Terminator::Jump(target) => self.line(&format!("br label %bb{}", target.0)),
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

// Windows has no target here yet, as `fennec build` doesn't link there.
#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    fn lowered(source: &str) -> Result<Program, String> {
        let module = fennec_syntax::parse(source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check(&module).map_err(|err| format!("{err:?}"))?;
        Ok(fennec_ir::lower(&module, &checked))
    }

    fn text_of(source: &str, function: &str) -> Result<String, String> {
        let (units, _, _) = emit(&lowered(source)?)?;
        Ok(units
            .into_iter()
            .find(|unit| unit.function == function)
            .map_or_else(String::new, |unit| unit.text))
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
    fn arguments_and_the_clock_need_no_interpreter() -> Result<(), String> {
        let source = "\
from std.time import Clock, Duration

def main(stdout: Stdout, args: Args, clock: Clock):
    started = clock.now()
    clock.sleep(Duration.millis(1))
    stdout.print(args.len(), args.program, started.nanos)
    for arg in args:
        stdout.print(arg, args[0])
    stdout.write(args.len())
";
        let text = text_of(source, "main")?;
        assert!(text.contains("@fennec_args_list("), "{text}");
        assert!(!text.contains("fennec_capability_call"), "{text}");
        Ok(())
    }

    #[test]
    fn looking_for_a_part_of_a_text_makes_no_block_for_either() -> Result<(), String> {
        let source = "\
def has_x(text: str) -> bool:
    \"x\" in text

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
                "{gap}def add(a: int, b: int) -> int:\n    a + b\n\ndef main(stdout: Stdout):\n    stdout.print(\"sum\", add(1, 2))\n"
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
                      for ref count in parallel(counts):\n        count += WORDS.len() + FIRST.len()\n    \
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
        let workers = main.rfind("@fennec_parallel_for(").ok_or("`main` starts no workers")?;
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
            "{types}def grow(ref units: list[Unit], at: int, v: int):\n    units[at].tags.append(v)\n\ndef grow_field(ref world: World, at: int, v: int):\n    world.units[at].tags.append(v)\n\ndef read(world: World, at: int) -> int:\n    world.units[at].hp\n\n{main}"
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
    text

def anew(note: Note, more: str) -> str:
    note.text + more

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
    str(n)

def of_float(x: float) -> str:
    str(x)

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
        let source = "def half(a: float) -> float:\n    a * 0.5\n\ndef main(stdout: Stdout):\n    stdout.print(half(1.0))\n";
        assert!(text_of(source, "half")?.contains("define swiftcc double @\"f_half\"(double %p0)"));
        Ok(())
    }

    /// A build that checks proofs writes the checks they left out, each saying which proof broke:
    /// a loop's count can't overflow, and its index is inside the list it goes through.
    #[test]
    fn checking_proofs_writes_what_they_left_out() -> Result<(), String> {
        let source = "def total(xs: list[int]) -> int:\n    mut sum = 0\n    for i in range(xs.len()):\n        sum += xs[i] * 0\n    sum\n\ndef main(stdout: Stdout):\n    stdout.print(total([1, 2]))\n";
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

    /// Checking proofs covers a loop filling a list too: one made whole of zeros gets one append a
    /// turn, and one whose length is kept aside isn't changed by anything else meanwhile.
    #[test]
    fn checking_proofs_covers_a_list_a_loop_fills() -> Result<(), String> {
        let source = "def made(n: int) -> int:\n    zeros = [0 for i in range(n)]\n    doubled = [i * 2 for i in range(n)]\n    zeros.len() + doubled.len()\n\ndef main(stdout: Stdout):\n    stdout.print(made(5))\n";
        let program = lowered(source)?;
        let checked = Shared { proofs: crate::Proofs::Checked, ..Shared::default() };
        let (_, shared, _) = emit_with(&program, Cpu::Usual, checked)?;
        let said: Vec<&String> = shared.literals.values().collect();
        for proof in ["gets one a turn, from 0", "is as long as its loop", "whose length was kept aside"] {
            assert!(said.iter().any(|text| text.contains(proof)), "{proof}: {said:?}");
        }
        Ok(())
    }

    /// A `list[str]` a loop fills and that's then joined is kept as the string it becomes, which
    /// has no list length to keep aside and write back: writing one would write over the string's
    /// own length, with a value nothing set. The build checking proofs found it.
    #[test]
    fn a_string_built_by_a_loop_has_no_length_written_over() -> Result<(), String> {
        let source = "def repeated(text: str, times: int) -> str:\n    \"\".join([text for _ in range(times)])\n\ndef main(stdout: Stdout):\n    stdout.print(repeated(\"ab\", 3))\n";
        let text = text_of(source, "repeated")?;
        assert!(!text.contains("kept_length"), "{text}");
        Ok(())
    }

    /// An enum holding strings gets native blocks in a program with no other threads, and its
    /// drop lets go of each string's block, giving the runtime one with no owner left.
    #[test]
    fn an_enum_holding_strings_has_native_blocks() -> Result<(), String> {
        let source = "enum Named:\n    Leaf(name: str)\n    Pair(left: Named, right: Named, label: str)\n\ndef build(depth: int) -> Named:\n    if depth == 0: return .Leaf(name=\"leaf\")\n    .Pair(left=build(depth - 1), right=build(depth - 1), label=\"pair\")\n\ndef size(tree: Named) -> int:\n    match tree:\n        case .Leaf(name):\n            name.len()\n        case .Pair(left, right, label):\n            size(left) + size(right) + label.len()\n\ndef main(stdout: Stdout):\n    stdout.print(size(build(4)))\n";
        let (units, shared, _) = emit(&lowered(source)?)?;
        assert!(!shared.native.is_empty(), "Named's blocks are native");
        let text: String = units.iter().map(|unit| unit.text.as_str()).collect();
        assert!(text.contains("call void @fennec_free(ptr %text"), "{text}");
        Ok(())
    }

    /// An enum holding lists of strings and numbers gets native blocks in a program with no
    /// other threads, and its drop lets go of each list, giving the runtime one with no owner
    /// left.
    #[test]
    fn an_enum_holding_lists_has_native_blocks() -> Result<(), String> {
        let source = "enum Doc:\n    Words(names: list[str], counts: list[int])\n    Pair(left: Doc, right: Doc)\n\ndef build(depth: int) -> Doc:\n    if depth == 0: return .Words(names=[\"a\", \"b\"], counts=[1, 2])\n    .Pair(left=build(depth - 1), right=build(depth - 1))\n\ndef size(doc: Doc) -> int:\n    match doc:\n        case .Words(names, counts):\n            names.len() + sum(counts)\n        case .Pair(left, right):\n            size(left) + size(right)\n\ndef main(stdout: Stdout):\n    stdout.print(size(build(4)))\n";
        let (units, shared, _) = emit(&lowered(source)?)?;
        assert!(!shared.native.is_empty(), "Doc's blocks are native");
        let text: String = units.iter().map(|unit| unit.text.as_str()).collect();
        assert!(text.contains("call void @fennec_free(ptr %list"), "{text}");
        Ok(())
    }

    /// A build that watches native blocks, as a dev build does, tells the runtime of each one it
    /// makes and frees, for it to stop if it's ever given one.
    #[test]
    fn a_watched_build_tells_the_runtime_of_its_native_blocks() -> Result<(), String> {
        let source = "enum Twig:\n    Leaf\n    Node(left: Twig, right: Twig)\n\ndef build(depth: int) -> Twig:\n    if depth == 0: return .Leaf\n    .Node(left=build(depth - 1), right=build(depth - 1))\n\ndef count(twig: Twig) -> int:\n    match twig:\n        case .Leaf:\n            1\n        case .Node(left, right):\n            1 + count(left) + count(right)\n\ndef main(stdout: Stdout):\n    stdout.print(count(build(4)))\n";
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
        let source = "enum Twig:\n    Leaf\n    Node(left: Twig, right: Twig)\n\ndef build(depth: int) -> Twig:\n    if depth == 0: return .Leaf\n    .Node(left=build(depth - 1), right=build(depth - 1))\n\ndef count(twig: Twig) -> int:\n    match twig:\n        case .Leaf:\n            1\n        case .Node(left, right):\n            1 + count(left) + count(right)\n\ndef main(stdout: Stdout):\n    small: u8 = 3\n    wide = i32(7)\n    stdout.print(small, wide, \"tree\", count(build(4)))\n";
        let (_, shared, _) = emit(&lowered(source)?)?;
        assert!(!shared.native.is_empty(), "Twig's blocks are native");
        Ok(())
    }

    /// What fails two ways holds only what its errors hold, so a function or an arrow failing
    /// `Refused | Shut` elsewhere in a program leaves a tree's blocks native.
    #[test]
    fn failing_two_ways_leaves_blocks_native() -> Result<(), String> {
        let source = "enum Twig:\n    Leaf\n    Node(left: Twig, right: Twig)\n\nerror Refused:\n    code: int\n\nerror Shut:\n    code: int\n\ndef build(depth: int) -> Twig:\n    if depth == 0: return .Leaf\n    .Node(left=build(depth - 1), right=build(depth - 1))\n\ndef count(twig: Twig) -> int:\n    match twig:\n        case .Leaf:\n            1\n        case .Node(left, right):\n            1 + count(left) + count(right)\n\ndef both(a: int) -> str fails Refused | Shut:\n    if a < 0: fail Refused(code=a)\n    if a == 0: fail Shut(code=a)\n    \"open\"\n\ndef either(run: () -> str fails Refused | Shut) -> str:\n    run() else \"failed\"\n\ndef main(stdout: Stdout):\n    stdout.print(both(-1) else \"failed\", either(() => both(1)?), count(build(4)))\n";
        let (_, shared, _) = emit(&lowered(source)?)?;
        assert!(!shared.native.is_empty(), "Twig's blocks are native");
        Ok(())
    }

    /// A new struct key compared by its words goes into a dict that's alone, with room, by
    /// compiled code itself, and only to the runtime otherwise.
    #[test]
    fn a_new_struct_key_is_added_natively() -> Result<(), String> {
        let source = "struct Cell:\n    x: int\n    y: int\n\ndef main(stdout: Stdout):\n    mut seen: dict[Cell, int] = {}\n    for i in range(10):\n        seen[Cell(x=i, y=i)] = i\n    stdout.print(seen.len())\n";
        let text = text_of(source, "main")?;
        assert!(text.contains("call i1 @fen_add_plain("), "{text}");
        assert!(text.contains("call ptr @fennec_dict_set("), "{text}");
        Ok(())
    }

    /// A case with no fields is one block made the first time and kept in a global: put there
    /// with release and read with acquire, so another thread that sees it sees its count and tag.
    #[test]
    fn a_shared_case_is_published_with_release_and_read_with_acquire() -> Result<(), String> {
        let source = "enum Twig:\n    Leaf\n    Node(left: Twig, right: Twig, tag: str)\n\ndef leaf() -> Twig:\n    .Leaf\n\ndef main(stdout: Stdout):\n    stdout.print(leaf() is Twig.Leaf)\n";
        let text = text_of(source, "leaf")?;
        assert!(
            text.contains("load atomic ptr, ptr @\"fen_case_") && text.contains("acquire, align 8"),
            "{text}"
        );
        assert!(text.contains("release, align 8"), "{text}");
        assert!(!text.contains("unordered"), "{text}");
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
        let source = "struct Inner:\n    name: list[int]\n\nstruct Wrap:\n    r: Inner\n\ndef grow(ref xs: list[int], at: int):\n    xs.append(at)\n    empty: list[int] = []\n    xs[0] = empty[at]\n\ndef handle(at: int) -> int:\n    mut w = Wrap(r=Inner(name=[1]))\n    grow(ref w.r.name, at=at)\n    w.r.name.len()\n\ndef main(stdout: Stdout):\n    stdout.print(handle(0))\n";
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
        let source = "def f() -> int:\n    xs = sorted([[2], [1]])\n    xs.len()\n\ndef main(stdout: Stdout):\n    stdout.print(f())\n";
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
