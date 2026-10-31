//! Writing functions as LLVM's text. Each local is a stack slot and every read and write goes
//! through it, which LLVM's first pass turns into proper SSA.

use std::collections::{BTreeSet, HashSet};
use std::fmt::Write as _;

use fennec_check::Type;
use fennec_ir::{Function, InstKind, LocalId, Operand, Program, Terminator};
use fennec_syntax::{BinOp, Span, UnaryOp};

use crate::Shared;
use crate::analysis::{Analysis, Whole, callees, whole};

const ATTRIBUTES: &str = "attributes #0 = { nounwind \"frame-pointer\"=\"non-leaf\" }\n\
attributes #1 = { cold nounwind }\n\
attributes #2 = { alwaysinline nounwind \"frame-pointer\"=\"non-leaf\" }\n\
!0 = !{!\"branch_weights\", i32 1, i32 100000}\n\
!1 = !{}\n";

/// Below this many instructions, a function that calls nothing is always inlined.
const ALWAYS_INLINE: usize = 16;
/// Below this many, a function that calls nothing is copied to its callers to be inlined.
const COPIED: usize = 40;
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
    let mut shared = Shared::default();
    let mut units = Vec::with_capacity(program.functions.len());
    for function in &program.functions {
        let text = unit(function, &everything, &mut shared, header)
            .map_err(|why| format!("`{}`: {why}", function.name))?;
        units.push(Unit { function: function.name.clone(), text });
    }
    Ok((units, shared))
}

fn size(function: &Function) -> usize {
    function.blocks.iter().map(|block| block.insts.len() + 1).sum()
}

fn symbol(name: &str) -> String {
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
struct Module<'w, 'p> {
    whole: &'w Whole<'p>,
    shared: &'w mut Shared,
    declares: BTreeSet<String>,
    defined: HashSet<&'p str>,
    bodies: String,
}

fn unit<'p>(
    function: &'p Function,
    whole: &Whole<'p>,
    shared: &mut Shared,
    header: &str,
) -> Result<String, String> {
    let mut module =
        Module { whole, shared, declares: BTreeSet::new(), defined: HashSet::new(), bodies: String::new() };
    module.defined.insert(&function.name);
    // Small functions that call nothing come along, to be inlined and thrown away.
    let copies: Vec<&Function> = callees(function)
        .filter(|name| *name != function.name)
        .filter_map(|name| whole.functions.get(name).copied())
        .filter(|callee| size(callee) <= COPIED && callees(callee).next().is_none())
        .collect();
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
    out += ATTRIBUTES;
    Ok(out)
}

fn type_text(ty: &Type) -> Result<&'static str, String> {
    Ok(match ty {
        Type::Int => "i64",
        Type::Float => "double",
        Type::Bool => "i1",
        Type::Unit => "void",
        Type::Named(name) if name == "Stdout" => "ptr",
        other => return Err(format!("it uses a {other:?}, which this backend doesn't take yet")),
    })
}

impl<'p> Module<'_, 'p> {
    fn flagged(&self, name: &str) -> bool {
        self.whole.can_fail.contains(name)
    }

    fn params(&self, function: &Function, named: bool) -> Result<String, String> {
        let mut params = Vec::with_capacity(function.params + 1);
        for (at, local) in function.locals.iter().take(function.params).enumerate() {
            let ty = type_text(&local.ty)?;
            params.push(if named { format!("{ty} %p{at}") } else { ty.to_owned() });
        }
        if self.flagged(&function.name) {
            params.push(if named { "ptr swifterror %err".to_owned() } else { "ptr swifterror".to_owned() });
        }
        Ok(params.join(", "))
    }

    fn define(&mut self, function: &'p Function, how: &Definition<'_>) -> Result<(), String> {
        let Definition { linkage, name, always_inline, self_target } = *how;
        let ret = type_text(&function.returns)?;
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
        self.declares.insert(format!("@\"{start_symbol}\" = external dso_local global i64\n"));
        let failed = if self.flagged(&function.name) {
            let leave = if ret == "void" { "ret void".to_owned() } else { format!("ret {ret} poison") };
            format!("store ptr inttoptr (i64 1 to ptr), ptr %err\n  {leave}")
        } else {
            "unreachable".to_owned()
        };
        let known = self.whole.analyses.get(function.name.as_str()).ok_or("no analysis")?;
        let mut emit = Emit {
            module: self,
            function,
            known,
            out: String::new(),
            cold: String::new(),
            next: 0,
            failed,
            needs_prop: false,
            start,
            start_symbol,
            self_target,
        };
        let _ = writeln!(emit.out, "define {linkage}swiftcc {ret} {name}({params}) #{attributes} {{\nentry:");
        emit.body()?;
        let (out, cold, failed, needs_prop) = (emit.out, emit.cold, emit.failed, emit.needs_prop);
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
struct Emit<'e, 'w, 'p> {
    module: &'e mut Module<'w, 'p>,
    function: &'p Function,
    known: &'w Analysis,
    out: String,
    cold: String,
    next: usize,
    failed: String,
    needs_prop: bool,
    start: u32,
    start_symbol: String,
    self_target: Option<&'e str>,
}

fn float_text(value: f64) -> String {
    format!("0x{:016X}", value.to_bits())
}

impl Emit<'_, '_, '_> {
    fn temp(&mut self) -> String {
        self.next += 1;
        format!("%t{}", self.next)
    }

    fn label(&mut self, word: &str) -> String {
        self.next += 1;
        format!("{word}{}", self.next)
    }

    fn line(&mut self, text: &str) {
        let _ = writeln!(self.out, "  {text}");
    }

    fn declare(&mut self, line: &str) {
        self.module.declares.insert(format!("{line}\n"));
    }

    fn local_type(&self, local: LocalId) -> Result<&'static str, String> {
        type_text(&self.function.locals.get(local.0).ok_or("no such local")?.ty)
    }

    fn body(&mut self) -> Result<(), String> {
        let function = self.function;
        for (at, local) in function.locals.iter().enumerate() {
            let ty = type_text(&local.ty)?;
            if ty != "void" {
                self.line(&format!("%l{at} = alloca {ty}"));
            }
        }
        for (at, local) in function.locals.iter().take(function.params).enumerate() {
            let ty = type_text(&local.ty)?;
            self.line(&format!("store {ty} %p{at}, ptr %l{at}"));
        }
        self.line("br label %bb0");
        for (at, block) in function.blocks.iter().enumerate() {
            let _ = writeln!(self.out, "bb{at}:");
            for inst in &block.insts {
                self.inst(&inst.kind, inst.span)?;
            }
            self.terminator(&block.end)?;
        }
        Ok(())
    }

    /// Fails if a call here would go too deep. The limit never changes once the program's
    /// running and the frame stays put, so LLVM drops all but the first check on a path.
    fn check_stack(&mut self, span: Span) {
        self.declare("declare ptr @llvm.frameaddress.p0(i32)");
        self.declare("@fennec_stack_limit = external dso_local global i64");
        let (here, limit, number, low) = (self.temp(), self.temp(), self.temp(), self.temp());
        self.line(&format!("{here} = call ptr @llvm.frameaddress.p0(i32 0)"));
        self.line(&format!("{limit} = load i64, ptr @fennec_stack_limit, !invariant.load !1"));
        self.line(&format!("{number} = ptrtoint ptr {here} to i64"));
        self.line(&format!("{low} = icmp ult i64 {number}, {limit}"));
        self.fail_if(&low, TOO_DEEP, span);
    }

    /// Calls the runtime's failure and leaves the function when `bad` holds.
    fn fail_if(&mut self, bad: &str, code: i64, span: Span) {
        let (fail, ok) = (self.label("fail"), self.label("ok"));
        self.line(&format!("br i1 {bad}, label %{fail}, label %{ok}, !prof !0"));
        let _ = writeln!(self.out, "{ok}:");
        self.declare("declare void @fennec_fail(i64, i64, i64) #1");
        let symbol = self.start_symbol.clone();
        let (from, to) = (span.start.saturating_sub(self.start), span.end.saturating_sub(self.start));
        let failed = self.failed.clone();
        let _ = writeln!(
            self.cold,
            "{fail}:\n  %s{fail} = load i64, ptr @\"{symbol}\"\n  %a{fail} = add i64 %s{fail}, {from}\n  %b{fail} = add i64 %s{fail}, {to}\n  call void @fennec_fail(i64 {code}, i64 %a{fail}, i64 %b{fail})\n  {failed}"
        );
    }

    fn operand(&mut self, operand: &Operand) -> Result<(String, Type), String> {
        Ok(match operand {
            Operand::Local(local) => {
                let held = self.function.locals.get(local.0).ok_or("no such local")?;
                let ty = type_text(&held.ty)?;
                let temp = self.temp();
                self.line(&format!("{temp} = load {ty}, ptr %l{}", local.0));
                (temp, held.ty.clone())
            }
            Operand::Int(value) => (value.to_string(), Type::Int),
            Operand::Float(value) => (float_text(*value), Type::Float),
            Operand::Bool(value) => (value.to_string(), Type::Bool),
            Operand::Str(_) => {
                return Err("it holds a string, which this backend doesn't take yet".to_owned());
            }
            Operand::None => return Err("it holds `None`, which this backend doesn't take yet".to_owned()),
        })
    }

    fn store(&mut self, dest: LocalId, value: &str) -> Result<(), String> {
        let ty = self.local_type(dest)?;
        if ty != "void" {
            self.line(&format!("store {ty} {value}, ptr %l{}", dest.0));
        }
        Ok(())
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
        let (a, ty) = self.operand(lhs)?;
        let (b, _) = self.operand(rhs)?;
        let value = match ty {
            Type::Int => self.int_binary(dest, op, (&a, &b), rhs, span)?,
            Type::Float => self.float_binary(op, (&a, &b), rhs, span)?,
            Type::Bool => {
                let cc = match op {
                    BinOp::Eq => "eq",
                    BinOp::NotEq => "ne",
                    other => return Err(format!("it uses {other:?} on bools")),
                };
                let temp = self.temp();
                self.line(&format!("{temp} = icmp {cc} i1 {a}, {b}"));
                temp
            }
            other => return Err(format!("it uses an operator on {other:?}")),
        };
        self.store(dest, &value)
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
            BinOp::Div => {
                if !self.known.is_nonzero(rhs) {
                    let zero = self.temp();
                    self.line(&format!("{zero} = fcmp oeq double {b}, {}", float_text(0.0)));
                    self.fail_if(&zero, DIVISION_BY_ZERO, span);
                }
                "fdiv"
            }
            other => return Err(format!("it uses {other:?} on floats, which this backend doesn't take yet")),
        };
        let temp = self.temp();
        self.line(&format!("{temp} = {word} double {a}, {b}"));
        Ok(temp)
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
        if !callee.mut_params.is_empty() {
            return Err(format!("`{name}` takes a `mut` parameter, which this backend doesn't take yet"));
        }
        let ret = type_text(&callee.returns)?;
        if !self.module.defined.contains(name) {
            let params = self.module.params(callee, false)?;
            self.declare(&format!("declare swiftcc {ret} {}({params}) #0", symbol(name)));
        }
        let mut shown = Vec::with_capacity(args.len() + 1);
        for arg in args {
            let (value, ty) = self.operand(arg)?;
            shown.push(format!("{} {value}", type_text(&ty)?));
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
            let (held, failed, ok) = (self.temp(), self.temp(), self.label("ok"));
            self.line(&format!("{held} = load ptr, ptr %err"));
            self.line(&format!("{failed} = icmp ne ptr {held}, null"));
            self.needs_prop = true;
            self.line(&format!("br i1 {failed}, label %prop, label %{ok}, !prof !0"));
            let _ = writeln!(self.out, "{ok}:");
        }
        if let (Some(dest), true) = (dest, ret != "void") {
            self.store(dest, &result)?;
        }
        Ok(())
    }

    /// The number of a string, read from its symbol, since it depends on the whole program.
    fn string_number(&mut self, text: &str) -> String {
        let hash = fennec_blake3::hash(text.as_bytes()).to_hex();
        let name = format!("s_{hash}");
        self.declare(&format!("@\"{name}\" = external dso_local global i64"));
        self.module.shared.strings.insert(name.clone(), text.to_owned());
        let temp = self.temp();
        self.line(&format!("{temp} = load i64, ptr @\"{name}\""));
        temp
    }

    fn print_text(&mut self, text: &str) {
        let number = self.string_number(text);
        self.declare("declare void @fennec_print_str(i64)");
        self.line(&format!("call void @fennec_print_str(i64 {number})"));
    }

    fn print(&mut self, args: &[Operand]) -> Result<(), String> {
        // Everything is worked out before anything prints.
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(match arg {
                Operand::Str(text) => (text.clone(), Type::Str),
                other => self.operand(other)?,
            });
        }
        for (at, (value, ty)) in values.iter().enumerate() {
            if at > 0 {
                self.print_text(" ");
            }
            let (helper, text) = match ty {
                Type::Str => {
                    self.print_text(value);
                    continue;
                }
                Type::Int => ("fennec_print_int", "i64"),
                Type::Float => ("fennec_print_float", "double"),
                Type::Bool => ("fennec_print_bool", "i8"),
                other => return Err(format!("it prints a {other:?}, which this backend doesn't take yet")),
            };
            self.declare(&format!("declare void @{helper}({text})"));
            if *ty == Type::Bool {
                let wide = self.temp();
                self.line(&format!("{wide} = zext i1 {value} to i8"));
                self.line(&format!("call void @{helper}(i8 {wide})"));
            } else {
                self.line(&format!("call void @{helper}({text} {value})"));
            }
        }
        self.print_text("\n");
        Ok(())
    }

    fn builtin(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        let first = args.first();
        match name {
            "Stdout.print" => self.print(args.get(1..).unwrap_or_default()),
            "float" => {
                let (value, ty) = self.operand(first.ok_or("no argument")?)?;
                let temp = self.temp();
                if ty == Type::Int {
                    self.line(&format!("{temp} = sitofp i64 {value} to double"));
                } else {
                    return Err("it calls `float` on something other than an int".to_owned());
                }
                self.store(dest.ok_or("no result")?, &temp)
            }
            "math.sqrt" => {
                let (value, _) = self.operand(first.ok_or("no argument")?)?;
                let negative = self.temp();
                self.line(&format!("{negative} = fcmp olt double {value}, {}", float_text(0.0)));
                self.fail_if(&negative, NEGATIVE_SQRT, span);
                self.declare("declare double @llvm.sqrt.f64(double)");
                let temp = self.temp();
                self.line(&format!("{temp} = call double @llvm.sqrt.f64(double {value})"));
                self.store(dest.ok_or("no result")?, &temp)
            }
            other => Err(format!("it calls `{other}`, which this backend doesn't take yet")),
        }
    }

    fn inst(&mut self, inst: &InstKind, span: Span) -> Result<(), String> {
        match inst {
            InstKind::Copy { dest, value } => {
                let (value, _) = self.operand(value)?;
                self.store(*dest, &value)
            }
            InstKind::Unary { dest, op, value } => {
                let (value, ty) = self.operand(value)?;
                let result = match (op, ty) {
                    (UnaryOp::Neg, Type::Int) if self.known.is_safe(*dest) => {
                        let temp = self.temp();
                        self.line(&format!("{temp} = sub nsw i64 0, {value}"));
                        temp
                    }
                    (UnaryOp::Neg, Type::Int) => self.checked("ssub", "0", &value, span),
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
                    (_, other) => return Err(format!("it uses a unary operator on {other:?}")),
                };
                self.store(*dest, &result)
            }
            InstKind::Binary { dest, op, lhs, rhs } => self.binary(*dest, *op, lhs, rhs, span),
            InstKind::Builtin { dest, name, args, .. } => self.builtin(*dest, name, args, span),
            InstKind::Call { dest, function, args } => self.call(*dest, function, args, span),
            other => Err(format!("it uses `{other}`, which this backend doesn't take yet")),
        }
    }

    fn terminator(&mut self, end: &Terminator) -> Result<(), String> {
        match end {
            Terminator::Jump(target) => self.line(&format!("br label %bb{}", target.0)),
            Terminator::Branch { cond, then, otherwise } => {
                let (cond, _) = self.operand(cond)?;
                self.line(&format!("br i1 {cond}, label %bb{}, label %bb{}", then.0, otherwise.0));
            }
            Terminator::Return(value) => {
                let ret = type_text(&self.function.returns)?;
                match (value, ret) {
                    (_, "void") => self.line("ret void"),
                    (Some(value), _) => {
                        let (value, _) = self.operand(value)?;
                        self.line(&format!("ret {ret} {value}"));
                    }
                    (None, _) => return Err("it returns nothing where a value is due".to_owned()),
                }
            }
            Terminator::Unreachable => self.line("unreachable"),
            Terminator::NoMatch { .. } | Terminator::Stop { .. } => {
                return Err(format!("it ends with `{end}`, which this backend doesn't take yet"));
            }
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
        let source =
            "def f() -> list[int]:\n    [1]\n\ndef main(stdout: Stdout):\n    stdout.print(f().len())\n";
        let error = emit(&lowered(source)?).err().unwrap_or_default();
        assert!(error.contains("`f`"), "{error}");
        Ok(())
    }
}
