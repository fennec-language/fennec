//! Writing functions as LLVM's text. Each local is a stack slot and every read and write goes
//! through it, which LLVM's first pass turns into proper SSA.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;

use fennec_interp::embed::layout::COUNT;
use fennec_ir::{Function, GeneratorDef, InstKind, LocalId, Operand, Program, Terminator};
use fennec_layout::Layouts;
use fennec_syntax::{BinOp, Span, UnaryOp};

use crate::Shared;
use crate::analysis::{Analysis, Whole, callees, visit, whole};
use crate::kinds::{Kind, struct_definitions};

const ATTRIBUTES: &str = "attributes #0 = { nounwind \"frame-pointer\"=\"non-leaf\" }\n\
attributes #1 = { cold nounwind }\n\
attributes #2 = { alwaysinline nounwind \"frame-pointer\"=\"non-leaf\" }\n\
!0 = !{!\"branch_weights\", i32 1, i32 100000}\n\
!1 = !{}\n\
!2 = !{!\"fennec\"}\n\
!3 = !{!\"header\", !2, i64 0}\n\
!4 = !{!3, !3, i64 0}\n\
!5 = !{!\"item\", !2, i64 0}\n\
!6 = !{!5, !5, i64 0}\n\
!7 = !{i64 0, i64 281474976710657}\n\
!8 = !{!\"branch_weights\", i32 100000, i32 1}\n";

/// Where a string block's bytes start.
pub(crate) const TEXT_START: u32 = fennec_interp::embed::layout::TEXT.unsigned_abs();

/// A list's header, and what's in it, never overlap its items, and a length is far from any limit.
pub(crate) const HEADER_ACCESS: &str = ", !tbaa !4";
pub(crate) const ITEM_ACCESS: &str = ", !tbaa !6";
pub(crate) const LENGTH_ACCESS: &str = ", !tbaa !4, !range !7";

/// Below this many instructions, a function that calls nothing is always inlined.
const ALWAYS_INLINE: usize = 16;
/// Below this many, a function is copied to its callers to be inlined.
const COPIED: usize = 64;
/// How many calls deep what's copied may call other small functions.
const COPY_DEPTH: usize = 2;
/// Below this many, a recursive function gets one level of itself inlined, as in the JIT.
const SELF_INLINED: usize = 24;

const OVERFLOW: i64 = 1;
const DIVISION_BY_ZERO: i64 = 2;
const TOO_DEEP: i64 = 4;
const NEGATIVE_SQRT: i64 = 5;

/// One function's module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub function: String,
    pub text: String,
}

/// What can't be emitted yet says so, by function.
pub fn emit(program: &Program) -> Result<(Vec<Unit>, Shared), String> {
    let header = crate::header()?;
    let everything = whole(program);
    let layouts = crate::layouts_of(program);
    let mut shared = Shared::default();
    let mut units = Vec::with_capacity(program.functions.len());
    for function in program.functions.iter().chain(&program.constants) {
        let text = unit(function, &everything, &layouts, &mut shared, header)
            .map_err(|why| format!("`{}`: {why}", function.name))?;
        units.push(Unit { function: function.name.clone(), text });
    }
    Ok((units, shared))
}

fn size(function: &Function) -> usize {
    function.blocks.iter().map(|block| block.insts.len() + 1).sum()
}

/// Whether a function is small enough to copy into its callers, with everything it calls, which
/// must be too, `depth` calls deep. A recursive one isn't.
fn copyable(whole: &Whole<'_>, function: &Function, depth: usize) -> bool {
    size(function) <= COPIED
        && callees(function).all(|name| {
            depth > 0
                && name != function.name
                && whole.functions.get(name).is_some_and(|callee| copyable(whole, callee, depth - 1))
        })
}

pub(crate) fn symbol(name: &str) -> String {
    format!("@\"f_{name}\"")
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
    whole: &'w Whole<'p>,
    pub(crate) layouts: &'w Layouts,
    pub(crate) shared: &'w mut Shared,
    declares: BTreeSet<String>,
    defined: HashSet<&'p str>,
    bodies: String,
    /// Functions a module only needs once, however many things use them.
    helpers: BTreeMap<String, String>,
}

fn unit<'p>(
    function: &'p Function,
    whole: &Whole<'p>,
    layouts: &Layouts,
    shared: &mut Shared,
    header: &str,
) -> Result<String, String> {
    let mut module = Module {
        whole,
        layouts,
        shared,
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
    out += ATTRIBUTES;
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
    fn define_types(&mut self, kind: &Kind) {
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
    fn params(&self, function: &Function, named: bool) -> Result<String, String> {
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
            Kind::Unit | Kind::Cap | Kind::Struct(_) | Kind::Enum(_) => "void".to_owned(),
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
        for local in &function.locals {
            let kind = self.kind(&local.ty)?;
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
            out: String::new(),
            cold: String::new(),
            next: 0,
            current: "entry".to_owned(),
            failed,
            needs_prop: false,
            start,
            start_symbol,
            self_target,
        };
        emit.body()?;
        let (allocas, out, cold, failed, needs_prop) =
            (emit.allocas, emit.out, emit.cold, emit.failed, emit.needs_prop);
        let _ =
            writeln!(self.bodies, "define {linkage}swiftcc {ret} {name}({params}) #{attributes} {{\nentry:");
        self.bodies += &allocas;
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
    allocas: String,
    out: String,
    cold: String,
    next: usize,
    /// The block being written.
    pub(crate) current: String,
    failed: String,
    needs_prop: bool,
    start: u32,
    start_symbol: String,
    self_target: Option<&'e str>,
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
    fn owns(&self, local: LocalId) -> bool {
        self.kinds.get(local.0).is_some_and(Kind::counted)
            && !self.is_mut_param(local)
            && !self.is_lent(local)
            && self.known.alias(local).is_none()
            && !self.known.is_borrowed(local)
            && !self.known.is_taken(local)
    }

    fn body(&mut self) -> Result<(), String> {
        let function = self.function;
        let kinds = self.kinds.clone();
        for (at, kind) in kinds.iter().enumerate() {
            let local = LocalId(at);
            let text = self.text_of(kind);
            if text == "void" || self.is_mut_param(local) || (self.is_lent(local) && kind.in_memory()) {
                continue;
            }
            let _ = writeln!(self.allocas, "  %l{at} = alloca {text}");
            if kind.counted() {
                let _ = writeln!(self.allocas, "  store ptr null, ptr %l{at}");
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
            } else if text != "void" {
                if self.owns(local) {
                    self.retain(&format!("%p{at}"));
                }
                self.line(&format!("store {text} %p{at}, ptr %l{at}"));
            }
        }
        for (at, kind) in kinds.iter().take(function.params).enumerate() {
            let local = LocalId(at);
            if matches!(kind, Kind::List(_)) && self.known.alone_once_checked(local) {
                self.check_alone(local);
            }
        }
        self.line("br label %bb0");
        for (at, block) in function.blocks.iter().enumerate() {
            self.start_block(&format!("bb{at}"));
            for inst in &block.insts {
                self.inst(&inst.kind, inst.span, at)?;
                self.let_go_after(&inst.kind);
            }
            if let Some((list, limit)) = self.known.reservations.get(&at).cloned() {
                self.reserve(LocalId(list), &limit)?;
            }
            self.terminator(&block.end)?;
        }
        Ok(())
    }

    /// A new slot on this function's stack.
    pub(crate) fn place_for(&mut self, kind: &Kind) -> String {
        let (at, text) = (self.label("%place"), self.text_of(kind));
        let _ = writeln!(self.allocas, "  {at} = alloca {text}");
        at
    }

    /// A slot holding this block's address, for code that may replace it.
    pub(crate) fn temp_slot(&mut self, block: &str) -> String {
        let at = self.place_for(&Kind::Str);
        self.line(&format!("store ptr {block}, ptr {at}"));
        at
    }

    pub(crate) fn is_optional(&self, kind: &Kind) -> bool {
        let (Kind::Enum(id) | Kind::Boxed(id)) = kind else { return false };
        self.module.layouts.get(*id).is_some_and(|layout| layout.name.starts_with("Option["))
    }

    /// One owner fewer for a value of this kind.
    pub(crate) fn release_kind(&mut self, value: &str, kind: &Kind) {
        if matches!(kind, Kind::Closure(..)) {
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
        let (missing, tag) = (self.temp(), self.temp());
        self.line(&format!("{missing} = icmp eq ptr {value}, null"));
        self.line(&format!("{tag} = select i1 {missing}, i64 {none}, i64 {some}"));
        let (place, fields_at) = if let Kind::Boxed(id) = kind {
            let block = self.new_block(*id)?;
            let fields_at = self.offset(&block, crate::lists::FIELDS_OFFSET);
            (block, fields_at)
        } else {
            let place = self.place_for(kind);
            let text = self.text_of(kind);
            self.line(&format!("store {text} zeroinitializer, ptr {place}"));
            (place.clone(), place)
        };
        self.line(&format!("store i64 {tag}, ptr {fields_at}"));
        let at = self.offset(&fields_at, offset);
        self.line(&format!("store ptr {value}, ptr {at}"));
        Ok((place, true))
    }

    /// A value to go where this kind goes: `None` needs telling what it's `None` of. The second
    /// half says the value is new, and already has an owner for its new home.
    pub(crate) fn value_for(&mut self, operand: &Operand, kind: &Kind) -> Result<(String, bool), String> {
        if *operand != Operand::None {
            let (value, held) = self.operand(operand)?;
            if matches!(held, Kind::Maybe(_)) && self.is_optional(kind) {
                return self.optional_as_enum(&value, &held, kind);
            }
            return Ok((value, false));
        }
        match kind {
            Kind::Maybe(_) => Ok(("null".to_owned(), false)),
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

    /// One more owner for a block.
    pub(crate) fn retain(&mut self, block: &str) {
        let (count, more) = (self.temp(), self.temp());
        self.line(&format!("{count} = load i64, ptr {block}{HEADER_ACCESS}"));
        self.line(&format!("{more} = add i64 {count}, 1"));
        self.line(&format!("store i64 {more}, ptr {block}{HEADER_ACCESS}"));
    }

    /// One owner fewer for a block, if there's a block, freeing it when that was the last.
    pub(crate) fn release(&mut self, block: &str) {
        debug_assert_eq!(COUNT, 0);
        self.declare("declare void @fennec_free(ptr) nounwind");
        let (none, held, last, done) =
            (self.temp(), self.label("held"), self.label("last"), self.label("done"));
        self.line(&format!("{none} = icmp eq ptr {block}, null"));
        self.line(&format!("br i1 {none}, label %{done}, label %{held}"));
        self.start_block(&held);
        let (count, fewer, gone) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{count} = load i64, ptr {block}{HEADER_ACCESS}"));
        self.line(&format!("{fewer} = add i64 {count}, -1"));
        self.line(&format!("store i64 {fewer}, ptr {block}{HEADER_ACCESS}"));
        self.line(&format!("{gone} = icmp eq i64 {fewer}, 0"));
        self.line(&format!("br i1 {gone}, label %{last}, label %{done}"));
        self.start_block(&last);
        self.line(&format!("call void @fennec_free(ptr {block})"));
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
    }

    /// Fails if a call here would go too deep. The limit never changes once the program's
    /// running and the frame stays put, so LLVM drops all but the first check on a path.
    pub(crate) fn check_stack(&mut self, span: Span) {
        self.declare("declare ptr @llvm.frameaddress.p0(i32)");
        self.declare("@fennec_stack_limit = external dso_local global i64");
        let (here, limit, number, low) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{here} = call ptr @llvm.frameaddress.p0(i32 0)"));
        self.line(&format!("{limit} = load i64, ptr @fennec_stack_limit, !invariant.load !1"));
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
    pub(crate) fn failed_exit(&self) -> String {
        self.failed.clone()
    }

    /// Leaves the function, the failure already recorded, when `failed` holds.
    pub(crate) fn leave_if(&mut self, failed: &str) {
        let ok = self.label("ok");
        self.needs_prop = true;
        self.line(&format!("br i1 {failed}, label %prop, label %{ok}, !prof !0"));
        self.start_block(&ok);
    }

    /// Calls the runtime's failure and leaves the function when `bad` holds.
    fn fail_if(&mut self, bad: &str, code: i64, span: Span) {
        let (fail, ok) = (self.label("fail"), self.label("ok"));
        self.line(&format!("br i1 {bad}, label %{fail}, label %{ok}, !prof !0"));
        self.start_block(&ok);
        self.declare("declare void @fennec_fail(i64, i64, i64) #1");
        let (place, args) = self.span_values(&fail, span);
        let failed = self.failed.clone();
        let _ =
            writeln!(self.cold, "{fail}:\n{place}  call void @fennec_fail(i64 {code}, {args})\n  {failed}");
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
            Operand::Str(text) => (self.literal(text), Kind::Str),
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
            self.line(&format!("{old} = load ptr, ptr {slot}"));
            self.line(&format!("store ptr {value}, ptr {slot}"));
            self.release_kind(&old, &kind);
        } else {
            self.line(&format!("store {text} {value}, ptr {slot}"));
        }
        Ok(())
    }

    /// Copies what's in memory at `from` into a local.
    pub(crate) fn assign_memory(&mut self, dest: LocalId, from: &str, access: &str) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        let text = self.text_of(&kind);
        let (held, slot) = (self.temp(), self.slot(dest));
        self.line(&format!("{held} = load {text}, ptr {from}{access}"));
        self.line(&format!("store {text} {held}, ptr {slot}"));
        Ok(())
    }

    /// Reads a value of this kind from inside something, as a number or a block's address.
    pub(crate) fn load_field(&mut self, kind: &Kind, at: &str, access: &str) -> String {
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
        let memory = kind.memory(self.module.layouts);
        if kind.in_memory() {
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

    pub(crate) fn checked_add(&mut self, a: &str, b: &str, span: Span) -> String {
        self.checked("sadd", a, b, span)
    }

    fn checked(&mut self, name: &str, a: &str, b: &str, span: Span) -> String {
        let intrinsic = format!("@llvm.{name}.with.overflow.i64");
        self.declare(&format!("declare {{ i64, i1 }} {intrinsic}(i64, i64)"));
        let (pair, value, bad) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{pair} = call {{ i64, i1 }} {intrinsic}(i64 {a}, i64 {b})"));
        self.line(&format!("{value} = extractvalue {{ i64, i1 }} {pair}, 0"));
        self.line(&format!("{bad} = extractvalue {{ i64, i1 }} {pair}, 1"));
        self.fail_if(&bad, OVERFLOW, span);
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
        let (a, kind) = self.operand(lhs)?;
        let (b, _) = self.operand(rhs)?;
        let value = match kind {
            Kind::Int => self.int_binary(dest, op, (&a, &b), rhs, span)?,
            Kind::Float => self.float_binary(op, (&a, &b), rhs, span)?,
            Kind::Bool => {
                let cc = match op {
                    BinOp::Eq => "eq",
                    BinOp::NotEq => "ne",
                    other => return Err(format!("it uses {other:?} on bools")),
                };
                let temp = self.temp();
                self.line(&format!("{temp} = icmp {cc} i1 {a}, {b}"));
                temp
            }
            Kind::Str if op == BinOp::Add && *lhs == Operand::Local(dest) && self.owns(dest) => {
                // `text += more` takes over the owner the local had, and grows in place if it can.
                self.declare("declare ptr @fennec_str_append(ptr, ptr) nounwind");
                let (joined, slot) = (self.temp(), self.slot(dest));
                self.line(&format!("{joined} = call ptr @fennec_str_append(ptr {a}, ptr {b})"));
                self.line(&format!("store ptr {joined}, ptr {slot}"));
                return Ok(());
            }
            Kind::Str => self.string_binary(op, (&a, &b))?,
            Kind::Boxed(_) if matches!(op, BinOp::Eq | BinOp::NotEq) => self.same_blocks(op, (&a, &b)),
            Kind::Enum(id) if matches!(op, BinOp::Eq | BinOp::NotEq) => self.same_enums(op, id, (&a, &b)),
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
        let safe = self.known.is_safe(dest);
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
                if !matches!(rhs, Operand::Int(1..)) {
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
        if !matches!(rhs, Operand::Int(1..)) {
            let zero = self.temp();
            self.line(&format!("{zero} = icmp eq i64 {b}, 0"));
            self.fail_if(&zero, DIVISION_BY_ZERO, span);
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
        }
        result
    }

    fn float_binary(
        &mut self,
        op: BinOp,
        (a, b): (&str, &str),
        rhs: &Operand,
        span: Span,
    ) -> Result<String, String> {
        let compare = match op {
            BinOp::Eq => Some("oeq"),
            BinOp::NotEq => Some("une"),
            BinOp::Lt => Some("olt"),
            BinOp::LtEq => Some("ole"),
            BinOp::Gt => Some("ogt"),
            BinOp::GtEq => Some("oge"),
            _ => None,
        };
        if let Some(cc) = compare {
            let temp = self.temp();
            self.line(&format!("{temp} = fcmp {cc} double {a}, {b}"));
            return Ok(temp);
        }
        let word = match op {
            BinOp::Add => "fadd",
            BinOp::Sub => "fsub",
            BinOp::Mul => "fmul",
            BinOp::Div | BinOp::FloorDiv | BinOp::Mod => {
                if !self.known.is_nonzero(rhs) {
                    let zero = self.temp();
                    self.line(&format!("{zero} = fcmp oeq double {b}, {}", float_text(0.0)));
                    self.fail_if(&zero, DIVISION_BY_ZERO, span);
                }
                if op != BinOp::Div {
                    return Ok(self.float_floor(op, (a, b)));
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

    /// `a // b` or `a % b` on floats, rounding toward negative infinity.
    fn float_floor(&mut self, op: BinOp, (a, b): (&str, &str)) -> String {
        self.declare("declare double @llvm.floor.f64(double)");
        let (ratio, floor) = (self.temp(), self.temp());
        self.line(&format!("{ratio} = fdiv double {a}, {b}"));
        self.line(&format!("{floor} = call double @llvm.floor.f64(double {ratio})"));
        if op == BinOp::FloorDiv {
            return floor;
        }
        let (times, rest) = (self.temp(), self.temp());
        self.line(&format!("{times} = fmul double {floor}, {b}"));
        self.line(&format!("{rest} = fsub double {a}, {times}"));
        rest
    }

    fn call(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
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
        let returned_at = if returns.in_memory() {
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
        self.check_stack(span);
        let result = self.temp();
        if ret == "void" {
            self.line(&format!("call swiftcc void {target}({shown})"));
        } else {
            self.line(&format!("{result} = call swiftcc {ret} {target}({shown})"));
        }
        if flagged {
            let (held, failed) = (self.temp(), self.temp());
            self.line(&format!("{held} = load ptr, ptr %err"));
            self.line(&format!("{failed} = icmp ne ptr {held}, null"));
            self.leave_if(&failed);
        }
        for block in lent {
            self.release(&block);
        }
        match (dest, returned_at) {
            (Some(dest), Some(at)) => self.assign_memory(dest, &at, "")?,
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
        if let Some(done) = self.loop_builtin(dest, name, args, span) {
            return done;
        }
        let first = args.first();
        match name {
            "Stdout.print" => self.print(args.get(1..).unwrap_or_default()),
            "list.len" => {
                let (list, _) = self.operand(first.ok_or("no argument")?)?;
                let length = self.list_length(&list);
                self.assign(dest.ok_or("no result")?, &length)
            }
            "list.append" => self.append(args),
            "result.expect" => self.expect(dest, args, span),
            "str.byte_len" | "str.char_at_byte" => self.text_by_bytes(dest, name, args),
            "is_regex" | "re.is_regex" | "Regex.replace" | "Regex.split" | "Regex.find_all"
            | "Regex.search" | "Regex.match" | "Regex.fullmatch" => {
                self.regex_builtin(dest, name, args, span)
            }
            "str.len" | "str" | "str.code" | "str.join" | "str.contains" => {
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
                let (value, _) = self.operand(first.ok_or("no argument")?)?;
                let negative = self.temp();
                self.line(&format!("{negative} = fcmp olt double {value}, {}", float_text(0.0)));
                self.fail_if(&negative, NEGATIVE_SQRT, span);
                self.declare("declare double @llvm.sqrt.f64(double)");
                let temp = self.temp();
                self.line(&format!("{temp} = call double @llvm.sqrt.f64(double {value})"));
                self.assign(dest.ok_or("no result")?, &temp)
            }
            "max" | "min" if args.len() == 2 => {
                let (a, kind) = self.operand(first.ok_or("no argument")?)?;
                let (b, _) = self.operand(args.get(1).ok_or("no argument")?)?;
                if kind != Kind::Int {
                    return Err(format!(
                        "it calls `{name}` on a {kind:?}, which this backend doesn't take yet"
                    ));
                }
                let cc = if name == "max" { "sgt" } else { "slt" };
                let (first_wins, temp) = (self.temp(), self.temp());
                self.line(&format!("{first_wins} = icmp {cc} i64 {a}, {b}"));
                self.line(&format!("{temp} = select i1 {first_wins}, i64 {a}, i64 {b}"));
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
            other => Err(format!("it calls `{other}`, which this backend doesn't take yet")),
        }
    }

    /// `dest = value`: a block gets another owner, unless nothing else ever reads `value`.
    pub(crate) fn copy(&mut self, dest: LocalId, value: &Operand) -> Result<(), String> {
        // The local is the list, so there's nothing to copy.
        if self.known.alias(dest).is_some() {
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
            Operand::None => wanted.clone(),
        };
        if from != wanted {
            if self.is_optional(&wanted) {
                return self.make_case(dest, "Some", std::slice::from_ref(value));
            }
            if self.is_optional(&from) {
                return self.get_case_field(dest, value, "Some", "value");
            }
        }
        let (held, kind) = self.operand(value)?;
        if kind.in_memory() {
            return self.assign_memory(dest, &held, "");
        }
        if kind.counted() {
            match value {
                Operand::Local(from) if self.owns(*from) && self.known.moves(*from) => {
                    let slot = self.slot(*from);
                    self.line(&format!("store ptr null, ptr {slot}"));
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

    fn inst(&mut self, inst: &InstKind, span: Span, block: usize) -> Result<(), String> {
        match inst {
            InstKind::Copy { dest, value }
            | InstKind::MakeSome { dest, value }
            | InstKind::GetSome { dest, value } => self.copy(*dest, value),
            InstKind::Unary { dest, op, value } => {
                let (value, kind) = self.operand(value)?;
                let result = match (op, kind) {
                    (UnaryOp::Neg, Kind::Int) if self.known.is_safe(*dest) => {
                        let temp = self.temp();
                        self.line(&format!("{temp} = sub nsw i64 0, {value}"));
                        temp
                    }
                    (UnaryOp::Neg, Kind::Int) => self.checked("ssub", "0", &value, span),
                    (UnaryOp::Neg, Kind::Float) => {
                        let temp = self.temp();
                        self.line(&format!("{temp} = fneg double {value}"));
                        temp
                    }
                    (UnaryOp::Not, Kind::Bool) => {
                        let temp = self.temp();
                        self.line(&format!("{temp} = xor i1 {value}, true"));
                        temp
                    }
                    (_, other) => return Err(format!("it uses a unary operator on {other:?}")),
                };
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
            InstKind::GetField { dest, object, field } => self.get_field(*dest, object, field),
            InstKind::SetField { object, path, value } => self.set_field(*object, path, value),
            InstKind::MakeTuple { dest, items } => self.make(*dest, items),
            InstKind::GetTupleItem { dest, value, at } => self.get_field(*dest, value, &format!("_{at}")),
            InstKind::MakeList { dest, items } => self.make_list(*dest, items, block),
            InstKind::GetIndex { dest, object, index } => self.get_index(*dest, object, index, span),
            InstKind::SetIndex { object, path, index, value } => {
                self.set_index(*object, path, index, value, span)
            }
            InstKind::Slice { dest, object, start, end } => {
                self.slice(*dest, object, start.as_ref(), end.as_ref(), span)
            }
            InstKind::Format { dest, parts } => self.format(*dest, parts, span),
            InstKind::Builtin { dest, name, args, .. } => self.builtin(*dest, name, args, span),
            InstKind::Call { dest, function, args } => self.call(*dest, function, args, span),
            InstKind::MakeGenerator { dest, generator, args } => self.make_generator(*dest, generator, args),
            InstKind::GetSlot { dest, generator, slot } => self.get_slot(*dest, generator, *slot),
            InstKind::SetSlot { generator, slot, value } => self.set_slot(generator, *slot, value),
            InstKind::Resume { dest, generator, known } => {
                self.resume(*dest, generator, known.as_deref(), span)
            }
            other => Err(format!("it uses `{other}`, which this backend doesn't take yet")),
        }
    }

    /// A local read once lets go of its value right after that, rather than when the function
    /// returns, so it doesn't still count as an owner when the next line changes what it read.
    fn let_go_after(&mut self, inst: &InstKind) {
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
                let (held, slot) = (self.temp(), self.slot(local));
                self.line(&format!("{held} = load ptr, ptr {slot}"));
                self.line(&format!("store ptr null, ptr {slot}"));
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
                let (held, slot) = (self.temp(), self.slot(local));
                self.line(&format!("{held} = load ptr, ptr {slot}"));
                self.release_kind(&held, &kind);
            }
        }
    }

    fn give_back(&mut self, value: Option<&Operand>) -> Result<(), String> {
        let returns = self.returns.clone();
        let text = self.text_of(&returns);
        if returns.in_memory() {
            let (at, _) =
                self.value_for(value.ok_or("it returns nothing where a value is due")?, &returns)?;
            let held = self.temp();
            self.line(&format!("{held} = load {text}, ptr {at}"));
            self.line(&format!("store {text} {held}, ptr %ret"));
            self.release_locals(None);
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
            Operand::Local(local) if returns.counted() && self.owns(*local) => Some(*local),
            _ => None,
        };
        if returns.counted() && moved.is_none() && !fresh {
            self.retain_value(&held, &returns);
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
        let (units, _) = emit(&lowered(source)?)?;
        Ok(units
            .into_iter()
            .find(|unit| unit.function == function)
            .map_or_else(String::new, |unit| unit.text))
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
    fn a_function_that_cant_fail_has_no_flag() -> Result<(), String> {
        let source = "def half(a: float) -> float:\n    a * 0.5\n\ndef main(stdout: Stdout):\n    stdout.print(half(1.0))\n";
        assert!(text_of(source, "half")?.contains("define swiftcc double @\"f_half\"(double %p0)"));
        Ok(())
    }

    #[test]
    fn a_loop_count_needs_no_overflow_check() -> Result<(), String> {
        let source = "def main(stdout: Stdout):\n    for i in range(3):\n        stdout.print(i)\n";
        assert!(!text_of(source, "main")?.contains("with.overflow"));
        Ok(())
    }

    #[test]
    fn what_it_cant_take_says_which_function() -> Result<(), String> {
        let source = "def f() -> list[list[int]]:\n    [[1]]\n\ndef main(stdout: Stdout):\n    stdout.print(f().len())\n";
        let error = emit(&lowered(source)?).err().unwrap_or_default();
        assert!(error.contains("`f`"), "{error}");
        Ok(())
    }
}
