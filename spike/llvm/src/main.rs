//! Throwaway spike: lowers Fennec's typed IR to LLVM IR text, as one whole-program module and as
//! one module per function, to measure whether LLVM could replace Cranelift for development.
//!
//! `llvm-spike <file.fen> <out dir> [--abi checked|plain] [--import N] [--dump]`
//!
//! Covers ints, floats, bools, structs, tuples, lists, direct calls, printing and f-strings:
//! enough for fib, sieve, mandelbrot, nbody, spectral_norm, fannkuch and the generated program.
//! No reference counting or freeing, so a list copy shares its items. That's only right for
//! programs that never change a list after copying it, which holds for those above; their output
//! is compared with the Rust twins' to be sure.
//!
//! `checked` does the work the JIT does on a call: a stack check and a failure flag handed back
//! from every call. `plain` has neither, and a failure ends the program on the spot.

mod facts;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use fennec_check::Type;
use fennec_ir::{BlockId, FormatPart, Function, InstKind, LocalId, Operand, Program, StructDef, Terminator};
use fennec_syntax::{BinOp, UnaryOp};

use facts::Facts;

const HEADER: &str = "target datalayout = \"e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32\"\n\
target triple = \"arm64-apple-macosx14.0.0\"\n\n";
// What the IR knows and LLVM can't work out: a list's header and its items never overlap, and a
// list's length is far from overflowing anything.
const ATTRS: &str = "attributes #0 = { nounwind \"frame-pointer\"=\"non-leaf\" }\nattributes #1 = { cold nounwind }\nattributes #2 = { alwaysinline nounwind \"frame-pointer\"=\"non-leaf\" }\n\
!0 = !{!\"fen\"}\n!1 = !{!\"header\", !0, i64 0}\n!2 = !{!\"item\", !0, i64 0}\n\
!3 = !{!1, !1, i64 0}\n!4 = !{!2, !2, i64 0}\n!5 = !{i64 0, i64 281474976710656}\n\
!6 = !{!\"branch_weights\", i32 1, i32 100000}\n";

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, PartialEq)]
enum Abi {
    Checked,
    Plain,
    /// The failure flag in a register no one else uses: nothing to write on the way out.
    Reg,
}

/// What to try on calls, each by itself, to see what it's worth.
#[derive(Default)]
struct Options {
    /// A guard page and a signal handler in place of checking the stack on every call.
    guard: bool,
    /// Tells LLVM a call's failure path is cold, as well as a check's.
    weights: bool,
    /// No failure flag on a function that can't fail, or anything it calls.
    nofail: bool,
    /// The calling convention, with its trailing space.
    cc: String,
}

static OPTIONS: OnceLock<Options> = OnceLock::new();
static CAN_FAIL: OnceLock<HashSet<String>> = OnceLock::new();

fn options() -> &'static Options {
    OPTIONS.get_or_init(Options::default)
}

/// Which functions can fail: those with a check that can't be shown to pass, and their callers.
fn can_fail(program: &Program) -> HashSet<String> {
    let by_name: HashMap<&str, &Function> = program.functions.iter().map(|f| (f.name.as_str(), f)).collect();
    let mut failing = HashSet::new();
    for f in &program.functions {
        let facts = Facts::of(f, &|callee, at| by_name.get(callee).is_some_and(|c| c.mut_params.contains(&at)));
        let unnamed = |dest: LocalId| f.locals.get(dest.0).is_some_and(|local| local.name.is_none());
        let inst_fails = |inst: &InstKind| match inst {
            InstKind::Binary { dest, op, lhs, rhs } => match (f.type_of(lhs), op) {
                (Some(Type::Int), BinOp::Add | BinOp::Sub | BinOp::Mul) => {
                    let steps = *op == BinOp::Add && unnamed(*dest) && *lhs == Operand::Local(*dest) && *rhs == Operand::Int(1);
                    !steps && !facts.safe.contains(&dest.0)
                }
                (Some(Type::Int), BinOp::FloorDiv | BinOp::Mod) => !matches!(rhs, Operand::Int(d) if *d > 0),
                (Some(Type::Float), BinOp::Div) => !facts.nonzero(rhs),
                _ => false,
            },
            InstKind::Unary { op: UnaryOp::Neg, value, .. } => f.type_of(value) == Some(Type::Int),
            InstKind::GetIndex { object, index, .. } => !facts.inside(object, index),
            InstKind::SetIndex { object, path, index, .. } => {
                !path.is_empty() || !facts.inside(&Operand::Local(*object), index)
            }
            _ => false,
        };
        let fails = f.blocks.iter().any(|block| {
            matches!(block.end, Terminator::Stop { .. } | Terminator::NoMatch { .. })
                || block.insts.iter().any(|inst| inst_fails(&inst.kind))
        });
        if fails {
            failing.insert(f.name.clone());
        }
    }
    loop {
        let before = failing.len();
        for f in &program.functions {
            if callees(f).iter().any(|callee| failing.contains(*callee)) {
                failing.insert(f.name.clone());
            }
        }
        if failing.len() == before {
            return failing;
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Linkage {
    /// The module is the whole program, so everything but `main` can go.
    Internal,
    External,
    /// A copy of a function compiled elsewhere, there only to be inlined.
    Imported,
}

struct Module<'a> {
    program: &'a Program,
    abi: Abi,
    structs: std::rc::Rc<HashMap<&'a str, &'a StructDef>>,
    functions: std::rc::Rc<HashMap<&'a str, &'a Function>>,
    declares: BTreeSet<String>,
    strings: Vec<String>,
    bodies: String,
    defined: BTreeSet<String>,
    /// Calls to other modules go through a slot, so a function can be swapped while it runs.
    indirect: bool,
}

/// One function being emitted.
struct Emit<'m, 'a> {
    m: &'m mut Module<'a>,
    f: &'a Function,
    out: String,
    cold: String,
    next: usize,
    failed_ret: String,
    needs_prop: bool,
    facts: Facts,
    /// Who a call to itself goes to, when that isn't the function's own name.
    self_target: Option<String>,
}

fn quote(name: &str) -> String {
    format!("@\"f_{name}\"")
}

fn float_literal(value: f64) -> String {
    format!("0x{:016X}", value.to_bits())
}

fn escape(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b" .,:;_-+=()[]{}<>!?/%".contains(&byte) {
            out.push(byte as char);
        } else {
            let _ = write!(out, "\\{byte:02X}");
        }
    }
    out
}

impl<'a> Module<'a> {
    fn new(program: &'a Program, abi: Abi) -> Self {
        Module {
            program,
            abi,
            structs: std::rc::Rc::new(program.structs.iter().map(|def| (def.name.as_str(), def)).collect()),
            functions: std::rc::Rc::new(program.functions.iter().map(|def| (def.name.as_str(), def)).collect()),
            declares: BTreeSet::new(),
            strings: Vec::new(),
            bodies: String::new(),
            defined: BTreeSet::new(),
            indirect: false,
        }
    }

    fn new_sharing(shared: &Module<'a>, abi: Abi) -> Self {
        Module {
            program: shared.program,
            abi,
            structs: std::rc::Rc::clone(&shared.structs),
            functions: std::rc::Rc::clone(&shared.functions),
            declares: BTreeSet::new(),
            strings: Vec::new(),
            bodies: String::new(),
            defined: BTreeSet::new(),
            indirect: false,
        }
    }

    fn ty(&self, ty: &Type) -> Result<String> {
        Ok(match ty {
            Type::Int => "i64".into(),
            Type::Float => "double".into(),
            Type::Bool => "i1".into(),
            Type::Unit => "void".into(),
            Type::Str | Type::List(_) => "ptr".into(),
            Type::Tuple(items) => {
                let items: Result<Vec<String>> = items.iter().map(|item| self.ty(item)).collect();
                format!("{{ {} }}", items?.join(", "))
            }
            Type::Named(name) if self.structs.contains_key(name.as_str()) => format!("%\"{name}\""),
            Type::Named(name) if name == "Stdout" => "ptr".into(),
            other => return Err(format!("type not covered: {other:?}")),
        })
    }

    /// How a function hands back failure: not at all, if it can't fail.
    fn abi_of(&self, name: &str) -> Abi {
        let no_failure = options().nofail && options().guard && !CAN_FAIL.get().is_some_and(|set| set.contains(name));
        if no_failure { Abi::Plain } else { self.abi }
    }

    fn cc(&self) -> &'static str {
        options().cc.as_str()
    }

    fn ret_ty(&self, returns: &Type, name: &str) -> Result<String> {
        let ty = self.ty(returns)?;
        Ok(match (self.abi_of(name), ty.as_str()) {
            (Abi::Plain | Abi::Reg, _) => ty,
            (Abi::Checked, "void") => "i1".into(),
            (Abi::Checked, _) => format!("{{ {ty}, i1 }}"),
        })
    }

    fn signature(&self, f: &Function) -> Result<(String, String)> {
        let mut params = Vec::new();
        for (at, local) in f.locals.iter().take(f.params).enumerate() {
            params.push(format!("{} %p{at}", self.ty(&local.ty)?));
        }
        if self.abi_of(&f.name) == Abi::Reg {
            params.push("ptr swifterror %err".to_owned());
        }
        Ok((self.ret_ty(&f.returns, &f.name)?, params.join(", ")))
    }

    fn declare_function(&mut self, name: &str) -> Result<()> {
        if self.defined.contains(name) {
            return Ok(());
        }
        if self.indirect {
            self.declares.insert(format!("@\"s_{name}\" = external dso_local global ptr\n"));
            return Ok(());
        }
        let f = self.functions.get(name).ok_or_else(|| format!("no function {name}"))?;
        let mut params = Vec::new();
        for local in f.locals.iter().take(f.params) {
            params.push(self.ty(&local.ty)?);
        }
        if self.abi_of(name) == Abi::Reg {
            params.push("ptr swifterror".to_owned());
        }
        let line = format!("declare {}{} {}({}) #0\n", self.cc(), self.ret_ty(&f.returns, name)?, quote(name), params.join(", "));
        self.declares.insert(line);
        Ok(())
    }

    fn type_defs(&self) -> Result<String> {
        let mut out = String::new();
        for def in &self.program.structs {
            if let Ok(fields) = def.fields.iter().map(|(_, ty)| self.ty(ty)).collect::<Result<Vec<_>>>() {
                let _ = writeln!(out, "%\"{}\" = type {{ {} }}", def.name, fields.join(", "));
            }
        }
        Ok(out)
    }

    fn finish(self) -> Result<String> {
        let mut out = String::from(HEADER);
        out += &self.type_defs()?;
        out.push('\n');
        for (at, text) in self.strings.iter().enumerate() {
            let _ = writeln!(
                out,
                "@.s{at} = private unnamed_addr constant [{} x i8] c\"{}\\00\"",
                text.len() + 1,
                escape(text)
            );
        }
        for line in &self.declares {
            out += line;
        }
        out.push('\n');
        out += &self.bodies;
        if options().guard {
            // A frame bigger than a page mustn't step over the guard page.
            out += &ATTRS.replace("nounwind \"frame-pointer\"", "nounwind \"probe-stack\"=\"inline-asm\" \"frame-pointer\"");
        } else {
            out += ATTRS;
        }
        Ok(out)
    }

    fn function(&mut self, f: &'a Function, linkage: Linkage) -> Result<()> {
        let word = match linkage {
            Linkage::Internal if f.name != "main" => "internal ",
            Linkage::Imported => "available_externally ",
            _ => "",
        };
        // The JIT inlines what's small by its own rule, and so does this: a small function that
        // calls nothing. LLVM can't see its failure paths are cold and would leave it.
        let imported_here = linkage == Linkage::Imported && self.indirect;
        let small_leaf = size(f) <= 16 && callees(f).is_empty() && f.name != "main";
        let attrs = if small_leaf || imported_here { 2 } else { 0 };
        // A small recursive function gets one level of itself inlined, as in the JIT: it calls a
        // copy that's always inlined and calls back. The copy needn't check the stack again.
        let recursive = linkage != Linkage::Imported && size(f) <= 24 && callees(f).contains(&f.name.as_str());
        let copy = format!("@\"f_{}$inl\"", f.name);
        let self_target = recursive.then(|| copy.clone());
        self.emit_one(f, word, &quote(&f.name), attrs, true, self_target)?;
        if recursive {
            self.emit_one(f, "internal ", &copy, 2, false, None)?;
        }
        if f.name == "main" {
            self.main_wrapper(f)?;
        }
        Ok(())
    }

    fn emit_one(
        &mut self,
        f: &'a Function,
        word: &str,
        symbol: &str,
        attrs: u32,
        stack_check: bool,
        self_target: Option<String>,
    ) -> Result<()> {
        let (ret, params) = self.signature(f)?;
        let failed_ret = match (self.abi_of(&f.name), ret.as_str()) {
            (Abi::Reg, "void") => "store ptr inttoptr (i64 1 to ptr), ptr %err\n  ret void".to_owned(),
            (Abi::Reg, _) => format!("store ptr inttoptr (i64 1 to ptr), ptr %err\n  ret {ret} poison"),
            (Abi::Checked, "i1") => "ret i1 true".to_owned(),
            (Abi::Checked, _) => {
                let inner = self.ty(&f.returns)?;
                format!("ret {ret} {{ {inner} poison, i1 true }}")
            }
            (Abi::Plain, _) => "unreachable".to_owned(),
        };
        let abi = self.abi_of(&f.name);
        let functions = &self.functions;
        let facts = Facts::of(f, &|callee, at| {
            functions.get(callee).is_some_and(|callee| callee.mut_params.contains(&at))
        });
        let mut emit = Emit {
            m: self,
            f,
            out: String::new(),
            cold: String::new(),
            next: 0,
            failed_ret,
            needs_prop: false,
            facts,
            self_target,
        };
        let _ = writeln!(emit.out, "define {word}{}{ret} {symbol}({params}) #{attrs} {{\nentry:", options().cc);
        emit.body(abi, stack_check)?;
        let (out, cold, prop) = (emit.out, emit.cold, emit.needs_prop);
        let text = emit.failed_ret;
        self.bodies += &out;
        self.bodies += &cold;
        if prop {
            let _ = writeln!(self.bodies, "prop:\n  {text}");
        }
        self.bodies += "}\n\n";
        Ok(())
    }

    /// The program's real `main`, which sets up and calls Fennec's.
    fn main_wrapper(&mut self, f: &Function) -> Result<()> {
        self.declares.insert("declare void @fen_init()\n".into());
        self.declares.insert("declare i32 @fen_finish(i1)\n".into());
        let args = vec!["ptr null"; f.params].join(", ");
        let cc = self.cc();
        let body = match self.abi_of(&f.name) {
            Abi::Reg => format!(
                "  %e = alloca swifterror ptr\n  store ptr null, ptr %e\n  call void @fen_init()\n  call {cc}void {}({}ptr swifterror %e)\n  %v = load ptr, ptr %e\n  %r = icmp ne ptr %v, null\n  %c = call i32 @fen_finish(i1 %r)\n  ret i32 %c\n",
                quote(&f.name),
                if args.is_empty() { String::new() } else { format!("{args}, ") },
            ),
            Abi::Checked => format!(
                "  call void @fen_init()\n  %r = call {cc}i1 {}({args})\n  %c = call i32 @fen_finish(i1 %r)\n  ret i32 %c\n",
                quote(&f.name)
            ),
            Abi::Plain => format!(
                "  call void @fen_init()\n  call {cc}void {}({args})\n  %c = call i32 @fen_finish(i1 false)\n  ret i32 %c\n",
                quote(&f.name)
            ),
        };
        let _ = write!(self.bodies, "define i32 @main() #0 {{\n{body}}}\n\n");
        Ok(())
    }

    fn string(&mut self, text: &str) -> String {
        let at = self.strings.iter().position(|held| held == text).unwrap_or_else(|| {
            self.strings.push(text.to_owned());
            self.strings.len() - 1
        });
        format!("@.s{at}")
    }
}

impl<'a> Emit<'_, 'a> {
    fn temp(&mut self) -> String {
        self.next += 1;
        format!("%t{}", self.next)
    }

    fn label(&mut self, word: &str) -> String {
        self.next += 1;
        format!("{word}{}", self.next)
    }

    fn line(&mut self, text: &str) {
        self.out.push_str("  ");
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn local_ty(&self, id: LocalId) -> Result<&'a Type> {
        self.f.locals.get(id.0).map(|local| &local.ty).ok_or_else(|| "no such local".to_owned())
    }

    fn body(&mut self, abi: Abi, stack_check: bool) -> Result<()> {
        for (at, local) in self.f.locals.iter().enumerate() {
            let ty = self.m.ty(&local.ty)?;
            if ty != "void" {
                self.line(&format!("%l{at} = alloca {ty}"));
            }
        }
        for (at, local) in self.f.locals.iter().take(self.f.params).enumerate() {
            let ty = self.m.ty(&local.ty)?;
            self.line(&format!("store {ty} %p{at}, ptr %l{at}"));
        }
        // A function that calls nothing has a known, small frame: the margin covers it.
        if stack_check && abi != Abi::Plain && !options().guard && !callees(self.f).is_empty() {
            self.m.declares.insert("declare ptr @llvm.frameaddress.p0(i32)\n".into());
            self.m.declares.insert("@fen_stack_limit = external dso_local global i64\n".into());
            let (sp, limit) = (self.temp(), self.temp());
            self.line(&format!("{sp} = call ptr @llvm.frameaddress.p0(i32 0)"));
            self.line(&format!("{limit} = load i64, ptr @fen_stack_limit"));
            let cmp = self.temp();
            let sp_int = self.temp();
            self.line(&format!("{sp_int} = ptrtoint ptr {sp} to i64"));
            self.line(&format!("{cmp} = icmp ult i64 {sp_int}, {limit}"));
            self.check(&cmp, 9, 0);
        }
        self.line("br label %bb0");
        for (at, block) in self.f.blocks.iter().enumerate() {
            let _ = writeln!(self.out, "bb{at}:");
            for inst in &block.insts {
                self.inst(&inst.kind, inst.span.start as u64)?;
            }
            if let Some((list, limit)) = self.facts.reserve.get(&at).cloned() {
                self.reserve(list, &limit)?;
            }
            self.terminator(&block.end)?;
        }
        Ok(())
    }

    /// Branches away to fail when `bad` holds.
    fn check(&mut self, bad: &str, kind: u32, span: u64) {
        let fail = self.label("fail");
        let ok = self.label("ok");
        let weights = if options().weights { ", !prof !6" } else { "" };
        self.line(&format!("br i1 {bad}, label %{fail}, label %{ok}{weights}"));
        let _ = writeln!(self.out, "{ok}:");
        let died = self.m.abi_of(&self.f.name) == Abi::Plain;
        let callee = if died { "fen_die" } else { "fen_fail" };
        self.m.declares.insert(format!("declare void @{callee}(i32, i64) #1\n"));
        let end = self.failed_ret.clone();
        let _ = writeln!(self.cold, "{fail}:\n  call void @{callee}(i32 {kind}, i64 {span})\n  {end}");
    }

    fn reserve(&mut self, list: usize, limit: &Operand) -> Result<()> {
        let Type::List(elem) = self.local_ty(LocalId(list))? else { return Err("a list that isn't".to_owned()) };
        let elem = self.m.ty(elem)?;
        let size = self.elem_size(&elem);
        let (count, _) = self.operand(limit)?;
        let held = self.temp();
        self.m.declares.insert("declare void @fen_list_reserve(ptr, i64, i64)\n".into());
        self.line(&format!("{held} = load ptr, ptr %l{list}"));
        self.line(&format!("call void @fen_list_reserve(ptr {held}, i64 {size}, i64 {count})"));
        Ok(())
    }

    fn operand(&mut self, operand: &Operand) -> Result<(String, Type)> {
        Ok(match operand {
            Operand::Local(id) => {
                let ty = self.local_ty(*id)?;
                let text = self.m.ty(ty)?;
                let temp = self.temp();
                self.line(&format!("{temp} = load {text}, ptr %l{}", id.0));
                (temp, ty.clone())
            }
            Operand::Int(value) => (value.to_string(), Type::Int),
            Operand::Float(value) => (float_literal(*value), Type::Float),
            Operand::Bool(value) => (value.to_string(), Type::Bool),
            Operand::Str(text) => (self.m.string(text), Type::Str),
            Operand::None => return Err("None isn't covered".to_owned()),
        })
    }

    fn store(&mut self, dest: LocalId, value: &str) -> Result<()> {
        let ty = self.m.ty(self.local_ty(dest)?)?;
        if ty != "void" {
            self.line(&format!("store {ty} {value}, ptr %l{}", dest.0));
        }
        Ok(())
    }

    fn field_ptr(&mut self, base: &str, owner: &Type, field: &str) -> Result<(String, Type)> {
        let Type::Named(name) = owner else { return Err("a field of a non-struct".to_owned()) };
        let def = *self.m.structs.get(name.as_str()).ok_or("no such struct")?;
        let at = def.fields.iter().position(|(held, _)| held == field).ok_or("no such field")?;
        let temp = self.temp();
        self.line(&format!("{temp} = getelementptr inbounds %\"{name}\", ptr {base}, i32 0, i32 {at}"));
        Ok((temp, def.fields[at].1.clone()))
    }

    fn walk(&mut self, object: LocalId, path: &[String]) -> Result<(String, Type)> {
        let mut base = format!("%l{}", object.0);
        let mut ty = self.local_ty(object)?.clone();
        for field in path {
            (base, ty) = self.field_ptr(&base, &ty, field)?;
        }
        Ok((base, ty))
    }

    fn elem_size(&mut self, elem: &str) -> String {
        format!("ptrtoint (ptr getelementptr ({elem}, ptr null, i32 1) to i64)")
    }

    /// The address of a list's item, after its bounds check.
    fn item_ptr(&mut self, list: &str, elem: &str, index: &str, span: u64, inside: bool) -> String {
        let (items_at, items, at) = (self.temp(), self.temp(), self.temp());
        if !inside {
            let (len, bad) = (self.temp(), self.temp());
            self.line(&format!("{len} = load i64, ptr {list}, !tbaa !3, !range !5"));
            self.line(&format!("{bad} = icmp uge i64 {index}, {len}"));
            self.check(&bad, 3, span);
        }
        self.line(&format!("{items_at} = getelementptr inbounds i8, ptr {list}, i64 16"));
        self.line(&format!("{items} = load ptr, ptr {items_at}, !tbaa !3"));
        self.line(&format!("{at} = getelementptr inbounds {elem}, ptr {items}, i64 {index}"));
        at
    }

    fn append(&mut self, list: &str, elem: &str, value: &str) {
        let (len, cap_at, cap, full) = (self.temp(), self.temp(), self.temp(), self.temp());
        let (grow, room) = (self.label("grow"), self.label("room"));
        let size = self.elem_size(elem);
        self.m.declares.insert("declare void @fen_list_grow(ptr, i64)\n".into());
        self.line(&format!("{len} = load i64, ptr {list}, !tbaa !3, !range !5"));
        self.line(&format!("{cap_at} = getelementptr inbounds i8, ptr {list}, i64 8"));
        self.line(&format!("{cap} = load i64, ptr {cap_at}, !tbaa !3"));
        self.line(&format!("{full} = icmp eq i64 {len}, {cap}"));
        self.line(&format!("br i1 {full}, label %{grow}, label %{room}"));
        let _ = write!(self.out, "{grow}:\n  call void @fen_list_grow(ptr {list}, i64 {size})\n  br label %{room}\n{room}:\n");
        let (items_at, items, at, next) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{items_at} = getelementptr inbounds i8, ptr {list}, i64 16"));
        self.line(&format!("{items} = load ptr, ptr {items_at}, !tbaa !3"));
        self.line(&format!("{at} = getelementptr inbounds {elem}, ptr {items}, i64 {len}"));
        self.line(&format!("store {elem} {value}, ptr {at}, !tbaa !4"));
        self.line(&format!("{next} = add i64 {len}, 1"));
        self.line(&format!("store i64 {next}, ptr {list}, !tbaa !3"));
    }

    fn new_list(&mut self, elem: &str, capacity: usize) -> String {
        let temp = self.temp();
        let size = self.elem_size(elem);
        self.m.declares.insert("declare ptr @fen_list_new(i64, i64)\n".into());
        self.line(&format!("{temp} = call ptr @fen_list_new(i64 {size}, i64 {capacity})"));
        temp
    }

    fn overflow(&mut self, name: &str, lhs: &str, rhs: &str, span: u64) -> String {
        let intrinsic = format!("@llvm.{name}.with.overflow.i64");
        self.m.declares.insert(format!("declare {{ i64, i1 }} {intrinsic}(i64, i64)\n"));
        let (pair, value, bad) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{pair} = call {{ i64, i1 }} {intrinsic}(i64 {lhs}, i64 {rhs})"));
        self.line(&format!("{value} = extractvalue {{ i64, i1 }} {pair}, 0"));
        self.line(&format!("{bad} = extractvalue {{ i64, i1 }} {pair}, 1"));
        self.check(&bad, 1, span);
        value
    }

    fn binary(&mut self, dest: LocalId, op: BinOp, lhs: &Operand, rhs: &Operand, span: u64) -> Result<()> {
        let (a, ty) = self.operand(lhs)?;
        let (b, _) = self.operand(rhs)?;
        if self.facts.safe.contains(&dest.0) && ty == Type::Int {
            let word = match op {
                BinOp::Add => "add",
                BinOp::Sub => "sub",
                _ => "mul",
            };
            let temp = self.temp();
            self.line(&format!("{temp} = {word} nsw i64 {a}, {b}"));
            return self.store(dest, &temp);
        }
        let nonzero = self.facts.nonzero(rhs);
        // A `for` loop's count, an unnamed local stepping itself, stops below a limit that fits.
        let unnamed = self.f.locals.get(dest.0).is_some_and(|local| local.name.is_none());
        if op == BinOp::Add && unnamed && *lhs == Operand::Local(dest) && *rhs == Operand::Int(1) {
            let temp = self.temp();
            self.line(&format!("{temp} = add nsw i64 {a}, 1"));
            return self.store(dest, &temp);
        }
        let result = match ty {
            Type::Int => self.int_binary(op, &a, &b, span)?,
            Type::Float => self.float_binary(op, &a, &b, span, nonzero)?,
            Type::Bool => {
                let cc = match op {
                    BinOp::Eq => "eq",
                    BinOp::NotEq => "ne",
                    other => return Err(format!("bool operator {other:?}")),
                };
                let temp = self.temp();
                self.line(&format!("{temp} = icmp {cc} i1 {a}, {b}"));
                temp
            }
            other => return Err(format!("operator on {other:?}")),
        };
        self.store(dest, &result)
    }

    fn int_binary(&mut self, op: BinOp, a: &str, b: &str, span: u64) -> Result<String> {
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
        Ok(match op {
            BinOp::Add => self.overflow("sadd", a, b, span),
            BinOp::Sub => self.overflow("ssub", a, b, span),
            BinOp::Mul => self.overflow("smul", a, b, span),
            BinOp::FloorDiv | BinOp::Mod => {
                let zero = self.temp();
                self.line(&format!("{zero} = icmp eq i64 {b}, 0"));
                self.check(&zero, 2, span);
                let (m1, min, both) = (self.temp(), self.temp(), self.temp());
                self.line(&format!("{m1} = icmp eq i64 {b}, -1"));
                self.line(&format!("{min} = icmp eq i64 {a}, -9223372036854775808"));
                self.line(&format!("{both} = and i1 {m1}, {min}"));
                self.check(&both, 1, span);
                let (quotient, remainder, signs, nonzero, adjust) =
                    (self.temp(), self.temp(), self.temp(), self.temp(), self.temp());
                self.line(&format!("{quotient} = sdiv i64 {a}, {b}"));
                self.line(&format!("{remainder} = srem i64 {a}, {b}"));
                let xor = self.temp();
                self.line(&format!("{xor} = xor i64 {a}, {b}"));
                self.line(&format!("{signs} = icmp slt i64 {xor}, 0"));
                self.line(&format!("{nonzero} = icmp ne i64 {remainder}, 0"));
                self.line(&format!("{adjust} = and i1 {signs}, {nonzero}"));
                let result = self.temp();
                if op == BinOp::FloorDiv {
                    let wide = self.temp();
                    self.line(&format!("{wide} = zext i1 {adjust} to i64"));
                    self.line(&format!("{result} = sub i64 {quotient}, {wide}"));
                } else {
                    let plus = self.temp();
                    self.line(&format!("{plus} = add i64 {remainder}, {b}"));
                    self.line(&format!("{result} = select i1 {adjust}, i64 {plus}, i64 {remainder}"));
                }
                result
            }
            other => return Err(format!("int operator {other:?}")),
        })
    }

    fn float_binary(&mut self, op: BinOp, a: &str, b: &str, span: u64, nonzero: bool) -> Result<String> {
        let compare = match op {
            BinOp::Eq => Some("oeq"),
            BinOp::NotEq => Some("une"),
            BinOp::Lt => Some("olt"),
            BinOp::LtEq => Some("ole"),
            BinOp::Gt => Some("ogt"),
            BinOp::GtEq => Some("oge"),
            _ => None,
        };
        let temp;
        if let Some(cc) = compare {
            temp = self.temp();
            self.line(&format!("{temp} = fcmp {cc} double {a}, {b}"));
            return Ok(temp);
        }
        let word = match op {
            BinOp::Add => "fadd",
            BinOp::Sub => "fsub",
            BinOp::Mul => "fmul",
            BinOp::Div => {
                if !nonzero {
                    let zero = self.temp();
                    self.line(&format!("{zero} = fcmp oeq double {b}, 0x0000000000000000"));
                    self.check(&zero, 2, span);
                }
                "fdiv"
            }
            other => return Err(format!("float operator {other:?}")),
        };
        temp = self.temp();
        self.line(&format!("{temp} = {word} double {a}, {b}"));
        Ok(temp)
    }

    fn call(&mut self, dest: Option<LocalId>, name: &str, args: &[Operand]) -> Result<()> {
        self.m.declare_function(name)?;
        let callee = *self.m.functions.get(name).ok_or_else(|| format!("no function {name}"))?;
        for (at, local) in callee.locals.iter().take(callee.params).enumerate() {
            if callee.mut_params.contains(&at) && !matches!(local.ty, Type::List(_)) {
                return Err(format!("a `mut` parameter of {name} isn't a list"));
            }
        }
        let mut shown = Vec::new();
        for arg in args {
            let (value, ty) = self.operand(arg)?;
            shown.push(format!("{} {value}", self.m.ty(&ty)?));
        }
        let ret = self.m.ret_ty(&callee.returns, name)?;
        let callee_abi = self.m.abi_of(name);
        if callee_abi == Abi::Reg {
            shown.push("ptr swifterror %err".to_owned());
        }
        let shown = shown.join(", ");
        let cc = self.m.cc();
        let weights = if options().weights { ", !prof !6" } else { "" };
        let target = if name == self.f.name {
            self.self_target.clone().unwrap_or_else(|| quote(name))
        } else if self.m.indirect && !self.m.defined.contains(name) {
            // Read every time, and in a way LLVM won't move out of a loop, so a swap is seen.
            let slot = self.temp();
            self.line(&format!("{slot} = load atomic ptr, ptr @\"s_{name}\" monotonic, align 8"));
            slot
        } else {
            quote(name)
        };
        let result = self.temp();
        if ret == "void" {
            self.line(&format!("call {cc}void {target}({shown})"));
        } else {
            self.line(&format!("{result} = call {cc}{ret} {target}({shown})"));
        }
        let value = match callee_abi {
            Abi::Plain if ret == "void" => return Ok(()),
            Abi::Plain => result,
            Abi::Reg => {
                let (held, flag) = (self.temp(), self.temp());
                let ok = self.label("ok");
                self.line(&format!("{held} = load ptr, ptr %err"));
                self.line(&format!("{flag} = icmp ne ptr {held}, null"));
                self.needs_prop = true;
                self.line(&format!("br i1 {flag}, label %prop, label %{ok}{weights}"));
                let _ = writeln!(self.out, "{ok}:");
                if ret == "void" {
                    return Ok(());
                }
                result
            }
            Abi::Checked => {
                let flag = if ret == "i1" {
                    result.clone()
                } else {
                    let flag = self.temp();
                    self.line(&format!("{flag} = extractvalue {ret} {result}, 1"));
                    flag
                };
                let ok = self.label("ok");
                self.needs_prop = true;
                self.line(&format!("br i1 {flag}, label %prop, label %{ok}{weights}"));
                let _ = writeln!(self.out, "{ok}:");
                if ret == "i1" {
                    return Ok(());
                }
                let value = self.temp();
                self.line(&format!("{value} = extractvalue {ret} {result}, 0"));
                value
            }
        };
        if let Some(dest) = dest {
            self.store(dest, &value)?;
        }
        Ok(())
    }

    fn print(&mut self, args: &[Operand]) -> Result<()> {
        for (at, arg) in args.iter().enumerate() {
            if at > 0 {
                self.m.declares.insert("declare void @fen_print_sep()\n".into());
                self.line("call void @fen_print_sep()");
            }
            let (value, ty) = self.operand(arg)?;
            let (callee, text) = match ty {
                Type::Int => ("fen_print_int", "i64"),
                Type::Float => ("fen_print_float", "double"),
                Type::Bool => ("fen_print_bool", "i1"),
                Type::Str => ("fen_print_str", "ptr"),
                other => return Err(format!("printing {other:?}")),
            };
            self.m.declares.insert(format!("declare void @{callee}({text})\n"));
            self.line(&format!("call void @{callee}({text} {value})"));
        }
        self.m.declares.insert("declare void @fen_print_nl()\n".into());
        self.line("call void @fen_print_nl()");
        Ok(())
    }

    fn builtin(&mut self, dest: Option<LocalId>, name: &str, args: &[Operand]) -> Result<()> {
        let list_of = |ty: &Type| match ty {
            Type::List(item) => Ok((**item).clone()),
            other => Err(format!("{name} on {other:?}")),
        };
        match name {
            "Stdout.print" => self.print(args.get(1..).unwrap_or_default()),
            "list.len" => {
                let (list, _) = self.operand(args.first().ok_or("no argument")?)?;
                let len = self.temp();
                self.line(&format!("{len} = load i64, ptr {list}, !tbaa !3, !range !5"));
                self.store(dest.ok_or("no dest")?, &len)
            }
            "list.append" => {
                let (list, ty) = self.operand(args.first().ok_or("no argument")?)?;
                let elem = self.m.ty(&list_of(&ty)?)?;
                let (value, _) = self.operand(args.get(1).ok_or("no argument")?)?;
                self.append(&list, &elem, &value);
                Ok(())
            }
            "float" => {
                let (value, ty) = self.operand(args.first().ok_or("no argument")?)?;
                if ty != Type::Int {
                    return Err("float() of a non-int".to_owned());
                }
                let temp = self.temp();
                self.line(&format!("{temp} = sitofp i64 {value} to double"));
                self.store(dest.ok_or("no dest")?, &temp)
            }
            "math.sqrt" => {
                let (value, _) = self.operand(args.first().ok_or("no argument")?)?;
                self.m.declares.insert("declare double @llvm.sqrt.f64(double)\n".into());
                let temp = self.temp();
                self.line(&format!("{temp} = call double @llvm.sqrt.f64(double {value})"));
                self.store(dest.ok_or("no dest")?, &temp)
            }
            other => Err(format!("builtin not covered: {other}")),
        }
    }

    fn format(&mut self, dest: LocalId, parts: &[FormatPart]) -> Result<()> {
        self.m.declares.insert("declare void @fen_fmt_begin()\n".into());
        self.m.declares.insert("declare ptr @fen_fmt_end()\n".into());
        self.line("call void @fen_fmt_begin()");
        for part in parts {
            match part {
                FormatPart::Text(text) => {
                    let text = self.m.string(text);
                    self.m.declares.insert("declare void @fen_fmt_text(ptr)\n".into());
                    self.line(&format!("call void @fen_fmt_text(ptr {text})"));
                }
                FormatPart::Value { value, spec } => {
                    let (value, ty) = self.operand(value)?;
                    let spec = self.m.string(spec.as_deref().unwrap_or(""));
                    let (callee, text) = match ty {
                        Type::Int => ("fen_fmt_int", "i64"),
                        Type::Float => ("fen_fmt_float", "double"),
                        other => return Err(format!("formatting {other:?}")),
                    };
                    self.m.declares.insert(format!("declare void @{callee}({text}, ptr)\n"));
                    self.line(&format!("call void @{callee}({text} {value}, ptr {spec})"));
                }
            }
        }
        let temp = self.temp();
        self.line(&format!("{temp} = call ptr @fen_fmt_end()"));
        self.store(dest, &temp)
    }

    fn inst(&mut self, inst: &InstKind, span: u64) -> Result<()> {
        match inst {
            InstKind::Copy { dest, value } => {
                let (value, _) = self.operand(value)?;
                if let Some((low, high)) = self.facts.assumes.get(&dest.0).copied() {
                    self.m.declares.insert("declare void @llvm.assume(i1)\n".into());
                    let (above, under) = (self.temp(), self.temp());
                    self.line(&format!("{above} = icmp sge i64 {value}, {low}"));
                    self.line(&format!("call void @llvm.assume(i1 {above})"));
                    self.line(&format!("{under} = icmp sle i64 {value}, {high}"));
                    self.line(&format!("call void @llvm.assume(i1 {under})"));
                }
                self.store(*dest, &value)
            }
            InstKind::Unary { dest, op, value } => {
                let (value, ty) = self.operand(value)?;
                let result = match (op, ty) {
                    (UnaryOp::Neg, Type::Int) => self.overflow("ssub", "0", &value, span),
                    (UnaryOp::Neg, Type::Float) => {
                        let temp = self.temp();
                        self.line(&format!("{temp} = fneg double {value}"));
                        temp
                    }
                    (UnaryOp::Not, Type::Bool) => {
                        let temp = self.temp();
                        self.line(&format!("{temp} = xor i1 {value}, true"));
                        temp
                    }
                    (_, other) => return Err(format!("unary operator on {other:?}")),
                };
                self.store(*dest, &result)
            }
            InstKind::Binary { dest, op, lhs, rhs } => self.binary(*dest, *op, lhs, rhs, span),
            InstKind::Make { dest, type_name, fields } => {
                let def = *self.m.structs.get(type_name.as_str()).ok_or("no such struct")?;
                for (at, value) in fields.iter().enumerate() {
                    let (value, _) = self.operand(value)?;
                    let field_ty = self.m.ty(&def.fields[at].1)?;
                    let ptr = self.temp();
                    self.line(&format!(
                        "{ptr} = getelementptr inbounds %\"{type_name}\", ptr %l{}, i32 0, i32 {at}",
                        dest.0
                    ));
                    self.line(&format!("store {field_ty} {value}, ptr {ptr}"));
                }
                Ok(())
            }
            InstKind::GetField { dest, object, field } => {
                let Operand::Local(id) = object else { return Err("a field of a literal".to_owned()) };
                let owner = self.local_ty(*id)?.clone();
                let (ptr, ty) = self.field_ptr(&format!("%l{}", id.0), &owner, field)?;
                let text = self.m.ty(&ty)?;
                let temp = self.temp();
                self.line(&format!("{temp} = load {text}, ptr {ptr}"));
                self.store(*dest, &temp)
            }
            InstKind::SetField { object, path, value } => {
                let (value, _) = self.operand(value)?;
                let (ptr, ty) = self.walk(*object, path)?;
                let text = self.m.ty(&ty)?;
                self.line(&format!("store {text} {value}, ptr {ptr}"));
                Ok(())
            }
            InstKind::MakeList { dest, items } => {
                let Type::List(elem) = self.local_ty(*dest)? else { return Err("a list that isn't".to_owned()) };
                let elem = self.m.ty(elem)?;
                let list = self.new_list(&elem, items.len());
                for item in items {
                    let (value, _) = self.operand(item)?;
                    self.append(&list, &elem, &value);
                }
                self.store(*dest, &list)
            }
            InstKind::MakeTuple { dest, items } => {
                let ty = self.m.ty(self.local_ty(*dest)?)?;
                for (at, item) in items.iter().enumerate() {
                    let (value, item_ty) = self.operand(item)?;
                    let text = self.m.ty(&item_ty)?;
                    let ptr = self.temp();
                    self.line(&format!("{ptr} = getelementptr inbounds {ty}, ptr %l{}, i32 0, i32 {at}", dest.0));
                    self.line(&format!("store {text} {value}, ptr {ptr}"));
                }
                Ok(())
            }
            InstKind::GetTupleItem { dest, value, at } => {
                let Operand::Local(id) = value else { return Err("an item of a literal".to_owned()) };
                let ty = self.m.ty(self.local_ty(*id)?)?;
                let item = self.m.ty(self.local_ty(*dest)?)?;
                let (ptr, temp) = (self.temp(), self.temp());
                self.line(&format!("{ptr} = getelementptr inbounds {ty}, ptr %l{}, i32 0, i32 {at}", id.0));
                self.line(&format!("{temp} = load {item}, ptr {ptr}"));
                self.store(*dest, &temp)
            }
            InstKind::GetIndex { dest, object, index } => {
                let inside = self.facts.inside(object, index);
                let (list, ty) = self.operand(object)?;
                let Type::List(elem) = ty else { return Err(format!("indexing {ty:?}")) };
                let elem = self.m.ty(&elem)?;
                let (index, _) = self.operand(index)?;
                let at = self.item_ptr(&list, &elem, &index, span, inside);
                let temp = self.temp();
                self.line(&format!("{temp} = load {elem}, ptr {at}, !tbaa !4"));
                self.store(*dest, &temp)
            }
            InstKind::SetIndex { object, path, index, value } => {
                let inside = path.is_empty() && self.facts.inside(&Operand::Local(*object), index);
                let (ptr, ty) = self.walk(*object, path)?;
                let Type::List(elem) = ty else { return Err(format!("indexing {ty:?}")) };
                let elem = self.m.ty(&elem)?;
                let list = self.temp();
                self.line(&format!("{list} = load ptr, ptr {ptr}"));
                let (index, _) = self.operand(index)?;
                let (value, _) = self.operand(value)?;
                let at = self.item_ptr(&list, &elem, &index, span, inside);
                self.line(&format!("store {elem} {value}, ptr {at}, !tbaa !4"));
                Ok(())
            }
            InstKind::Format { dest, parts } => self.format(*dest, parts),
            InstKind::Builtin { dest, name, args, .. } => self.builtin(*dest, name, args),
            InstKind::Call { dest, function, args } => self.call(*dest, function, args),
            other => Err(format!("instruction not covered: {other}")),
        }
    }

    fn terminator(&mut self, end: &Terminator) -> Result<()> {
        match end {
            Terminator::Jump(BlockId(to)) => self.line(&format!("br label %bb{to}")),
            Terminator::Branch { cond, then, otherwise } => {
                let (cond, _) = self.operand(cond)?;
                self.line(&format!("br i1 {cond}, label %bb{}, label %bb{}", then.0, otherwise.0));
            }
            Terminator::Return(value) => {
                let value = value.as_ref().map(|value| self.operand(value)).transpose()?;
                let ret = self.m.ret_ty(&self.f.returns, &self.f.name)?;
                match (self.m.abi_of(&self.f.name), value, ret.as_str()) {
                    (Abi::Plain | Abi::Reg, _, "void") => self.line("ret void"),
                    (Abi::Plain | Abi::Reg, Some((value, _)), _) => self.line(&format!("ret {ret} {value}")),
                    (Abi::Checked, _, "i1") => self.line("ret i1 false"),
                    (Abi::Checked, Some((value, _)), _) => {
                        let inner = self.m.ty(&self.f.returns)?;
                        let temp = self.temp();
                        self.line(&format!(
                            "{temp} = insertvalue {ret} {{ {inner} poison, i1 false }}, {inner} {value}, 0"
                        ));
                        self.line(&format!("ret {ret} {temp}"));
                    }
                    _ => return Err("a return with no value".to_owned()),
                }
            }
            Terminator::Stop { span, .. } | Terminator::NoMatch { span, .. } => {
                let done = "true";
                let (fail, ok) = (self.label("stop"), self.label("dead"));
                self.line(&format!("br i1 {done}, label %{fail}, label %{ok}"));
                let _ = writeln!(self.out, "{ok}:\n  unreachable");
                let died = self.m.abi_of(&self.f.name) == Abi::Plain;
                let callee = if died { "fen_die" } else { "fen_fail" };
                self.m.declares.insert(format!("declare void @{callee}(i32, i64) #1\n"));
                let text = self.failed_ret.clone();
                let _ = writeln!(self.cold, "{fail}:\n  call void @{callee}(i32 4, i64 {})\n  {text}", span.start);
            }
            Terminator::Unreachable => self.line("unreachable"),
        }
        Ok(())
    }
}

/// How much a function is, for choosing what's small enough to copy for inlining.
fn size(f: &Function) -> usize {
    f.blocks.iter().map(|block| block.insts.len() + 1).sum()
}

fn callees(f: &Function) -> Vec<&str> {
    f.blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match &inst.kind {
            InstKind::Call { function, .. } => Some(function.as_str()),
            _ => None,
        })
        .collect()
}

/// One module holding one function, plus copies of small functions it calls, up to two calls deep.
fn single<'a>(shared: &Module<'a>, f: &'a Function, abi: Abi, import: usize, indirect: bool) -> Result<String> {
    let mut module = Module::new_sharing(shared, abi);
    module.indirect = indirect;
    module.defined.insert(f.name.clone());
    let mut imports: Vec<&Function> = Vec::new();
    if import > 0 {
        let mut level: Vec<&Function> = vec![f];
        for _ in 0..2 {
            let mut next = Vec::new();
            for caller in level {
                for name in callees(caller) {
                    let Some(callee) = module.functions.get(name).copied() else { continue };
                    let fit = size(callee) <= import && (!indirect || callees(callee).is_empty());
                    if fit && module.defined.insert(callee.name.clone()) {
                        imports.push(callee);
                        next.push(callee);
                    }
                }
            }
            level = next;
        }
    }
    module.function(f, Linkage::External)?;
    for callee in imports {
        module.function(callee, Linkage::Imported)?;
    }
    module.finish()
}

fn whole(program: &Program, abi: Abi) -> Result<String> {
    let mut module = Module::new(program, abi);
    for f in &program.functions {
        module.defined.insert(f.name.clone());
    }
    for f in &program.functions {
        module.function(f, Linkage::Internal)?;
    }
    module.finish()
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path, out_dir, rest @ ..] = args.as_slice() else {
        return Err("usage: llvm-spike <file.fen> <out dir> [--abi checked|plain] [--import N] [--dump]".to_owned());
    };
    let mut abi = Abi::Checked;
    let mut choices = Options::default();
    let mut import = 0;
    let mut dump = false;
    let mut indirect = false;
    let mut flags = rest.iter();
    while let Some(flag) = flags.next() {
        match flag.as_str() {
            "--abi" => {
                abi = match flags.next().map(String::as_str) {
                    Some("plain") => Abi::Plain,
                    Some("reg") => Abi::Reg,
                    _ => Abi::Checked,
                }
            }
            "--guard" => choices.guard = true,
            "--weights" => choices.weights = true,
            "--nofail" => choices.nofail = true,
            "--cc" => choices.cc = format!("{} ", flags.next().ok_or("--cc needs a name")?),
            "--import" => import = flags.next().and_then(|word| word.parse().ok()).ok_or("--import needs a number")?,
            "--dump" => dump = true,
            "--indirect" => indirect = true,
            other => return Err(format!("unknown flag {other}")),
        }
    }

    let started = Instant::now();
    let mut loaded =
        fennec_kit::load(Path::new(path), path, fennec_kit::Entry::Main).map_err(|err| format!("load: {err}"))?;
    let load_ms = started.elapsed().as_secs_f64() * 1e3;
    let at = Instant::now();
    fennec_check::check(&loaded.module).map_err(|_| "check failed")?;
    let check_ms = at.elapsed().as_secs_f64() * 1e3;
    let at = Instant::now();
    fennec_build::keep_what_main_reaches(&mut loaded.module);
    let checked = fennec_check::check(&loaded.module).map_err(|_| "check failed")?;
    let recheck_ms = at.elapsed().as_secs_f64() * 1e3;
    let at = Instant::now();
    let program = fennec_ir::lower(&loaded.module, &checked);
    if abi == Abi::Reg {
        choices.cc = "swiftcc ".to_owned();
    }
    let _ = CAN_FAIL.set(can_fail(&program));
    let _ = OPTIONS.set(choices);
    let lower_ms = at.elapsed().as_secs_f64() * 1e3;
    let front = started.elapsed();
    // What finding the changed functions costs: printing and hashing every one's IR.
    let at = Instant::now();
    let mut hashed = 0_u64;
    for f in &program.functions {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&f.to_string(), &mut hasher);
        hashed ^= std::hash::Hasher::finish(&hasher);
    }
    let hash_all_ms = at.elapsed().as_secs_f64() * 1e3;
    if dump {
        println!("{program}");
    }
    if !program.not_lowered.is_empty() {
        return Err(format!("not lowered: {:?}", program.not_lowered));
    }

    let started = Instant::now();
    let dir = Path::new(out_dir);
    let functions_dir = dir.join("fn");
    std::fs::create_dir_all(&functions_dir).map_err(|err| err.to_string())?;
    let mut bytes = 0;
    let mut names = String::new();
    let mut slots = String::from(HEADER);
    let mut emit_one_ms = 0.0_f64;
    let shared = Module::new(&program, abi);
    for (at, f) in program.functions.iter().enumerate() {
        let timer = Instant::now();
        let text = single(&shared, f, abi, import, indirect).map_err(|err| format!("{}: {err}", f.name))?;
        emit_one_ms = emit_one_ms.max(timer.elapsed().as_secs_f64() * 1e3);
        bytes += text.len();
        let _ = writeln!(names, "{}", f.name);
        let _ = writeln!(slots, "@\"s_{}\" = dso_local global ptr null", f.name);
        std::fs::write(functions_dir.join(format!("f{at:05}.ll")), text).map_err(|err| err.to_string())?;
    }
    std::fs::write(dir.join("names.txt"), names).map_err(|err| err.to_string())?;
    std::fs::write(dir.join("slots.ll"), slots).map_err(|err| err.to_string())?;
    let text = whole(&program, abi)?;
    std::fs::write(dir.join("whole.ll"), &text).map_err(|err| err.to_string())?;
    let emit = started.elapsed();
    let insts: usize = program.functions.iter().map(size).sum();
    println!(
        "{{\"load_ms\":{load_ms:.3},\"check_ms\":{check_ms:.3},\"recheck_ms\":{recheck_ms:.3},\"lower_ms\":{lower_ms:.3},\"hash_all_ms\":{hash_all_ms:.3},\"hash_check\":{},\"emit_one_max_ms\":{emit_one_ms:.3},\"functions\":{},\"ir_insts\":{insts},\"front_ms\":{:.3},\"emit_all_ms\":{:.3},\"whole_ll\":{},\"split_ll\":{bytes}}}",
        hashed & 1,
        program.functions.len(),
        front.as_secs_f64() * 1e3,
        emit.as_secs_f64() * 1e3,
        text.len(),
    );
    Ok(())
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
