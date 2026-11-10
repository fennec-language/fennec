//! Closures: the address of an owner count, a word for the runtime's shape, the code to call, the
//! code that lets go of it, then the values it captured. The code takes that address first, then
//! the arguments, and hands failure back in the usual register whether or not the function
//! underneath can fail, so that any closure can be called the same way.
//!
//! A program that only ever holds closures in locals keeps them outside the runtime's heap, made
//! and freed by generated code. One that keeps a closure anywhere the runtime reads, as a list's
//! item, a field, or in a generator, has every closure in the runtime's table instead, laid out
//! as a block whose shape names what it captured, so the runtime can copy and free it like any
//! other. See [`runtime_holds_closures`].

use std::fmt::Write as _;

use fennec_check::Type;
use fennec_ir::{InstKind, LocalId, Operand, Program};
use fennec_layout::{Layouts, Written, closure_definition};
use fennec_syntax::Span;

use crate::emit::{Emit, symbol};
use crate::kinds::Kind;

/// Where a closure's code and captured values are.
pub(crate) const CODE: u32 = 16;
const LET_GO: u32 = 24;
const CAPTURES: u32 = 32;

/// Whether a program keeps a closure anywhere the runtime reads: inside a collection, an
/// optional, a tuple or another value, in a field, in a generator's block, or as a launched task,
/// which the runtime copies. A closure only ever
/// held by a local, given as an argument or returned stays with generated code.
pub(crate) fn runtime_holds_closures(program: &Program) -> bool {
    let functions = program.functions.iter().chain(&program.constants);
    let locals = functions.clone().flat_map(|function| function.locals.iter().map(|local| &local.ty));
    let returns = functions.map(|function| &function.returns);
    if locals.chain(returns).any(holds_one) {
        return true;
    }
    let structs = program.structs.iter().chain(&program.errors).flat_map(|def| &def.fields);
    let cases = program.enums.iter().flat_map(|def| &def.cases).flat_map(|case| &case.fields);
    if structs.chain(cases).any(|(_, ty)| has_one(ty)) {
        return true;
    }
    // A launched task's closure is copied for it by the runtime, by its block's shape.
    let launches = program.functions.iter().flat_map(|function| &function.blocks).any(|block| {
        block
            .insts
            .iter()
            .any(|inst| matches!(&inst.kind, InstKind::Builtin { name, .. } if name == "intrinsics.launch"))
    });
    launches || program.generators.iter().any(|def| has_one(&def.item) || def.slots.iter().any(has_one))
}

/// Whether a value of this type holds a closure inside it, rather than being one.
fn holds_one(ty: &Type) -> bool {
    match ty {
        Type::Arrow(params, returns) => params.iter().any(holds_one) || holds_one(returns),
        other => inside(other).iter().any(|inner| has_one(inner)),
    }
}

/// Whether this type is a closure or holds one.
fn has_one(ty: &Type) -> bool {
    matches!(ty, Type::Arrow(..) | Type::Function(_)) || inside(ty).iter().any(|inner| has_one(inner))
}

/// The types directly inside a type.
fn inside(ty: &Type) -> Vec<&Type> {
    match ty {
        Type::List(inner)
        | Type::Buffer(inner)
        | Type::Array(inner, _)
        | Type::Set(inner)
        | Type::Iterator(inner)
        | Type::Optional(inner)
        | Type::Id(inner)
        | Type::Table(inner) => vec![inner],
        Type::Dict(key, value) | Type::Result(key, value) => vec![key, value],
        Type::Tuple(items) | Type::Generic(_, items) | Type::Union(items) => items.iter().collect(),
        _ => Vec::new(),
    }
}

/// The name of the layout of a closure's block, when the runtime holds closures: one for each
/// function and how many values it captures.
pub(crate) fn layout_name(function: &str, captured: usize) -> String {
    format!("closure.{function}.{captured}")
}

/// The layouts of every closure a program makes, by what it captures, for when the runtime holds
/// closures.
pub(crate) fn closure_layouts(program: &Program, blocks: Option<&Layouts>) -> Vec<(String, Written)> {
    let functions: std::collections::HashMap<&str, &fennec_ir::Function> =
        program.functions.iter().map(|function| (function.name.as_str(), function)).collect();
    let mut made = std::collections::BTreeMap::new();
    let insts = program.functions.iter().flat_map(|function| &function.blocks).flat_map(|block| &block.insts);
    for inst in insts {
        let InstKind::MakeClosure { function, captures, .. } = &inst.kind else { continue };
        let Some(callee) = functions.get(function.as_str()) else { continue };
        let held: Vec<Type> = callee
            .locals
            .iter()
            .take(captures.len().min(callee.params))
            .map(|local| crate::kinds::canon_in(blocks, &local.ty))
            .collect();
        made.entry(layout_name(function, captures.len())).or_insert_with(|| closure_definition(&held));
    }
    made.into_iter().collect()
}

impl Emit<'_, '_, '_> {
    /// One owner fewer for a closure, freeing it when that was the last.
    pub(crate) fn release_closure(&mut self, closure: &str) {
        // A block in the runtime's table goes as any other does, with what it captured.
        if self.module.whole.runtime_closures {
            self.release(closure);
            return;
        }
        let (none, held, last, done) =
            (self.temp(), self.label("held"), self.label("last"), self.label("done"));
        self.line(&format!("{none} = icmp eq ptr {closure}, null"));
        self.line(&format!("br i1 {none}, label %{done}, label %{held}"));
        self.start_block(&held);
        let gone = self.closure_gone(closure);
        self.line(&format!("br i1 {gone}, label %{last}, label %{done}"));
        self.start_block(&last);
        let (at, code) = (self.temp(), self.temp());
        self.line(&format!("{at} = getelementptr inbounds i8, ptr {closure}, i64 {LET_GO}"));
        self.line(&format!("{code} = load ptr, ptr {at}"));
        self.line(&format!("call void {code}(ptr {closure})"));
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
    }

    /// `==` on functions: each is equal only to itself, however many times it's been named.
    pub(crate) fn same_code(&mut self, op: fennec_syntax::BinOp, (a, b): (&str, &str)) -> String {
        let (a_at, b_at) = (self.offset(a, CODE), self.offset(b, CODE));
        let (x, y, same) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{x} = load ptr, ptr {a_at}"));
        self.line(&format!("{y} = load ptr, ptr {b_at}"));
        let cc = if op == fennec_syntax::BinOp::Eq { "eq" } else { "ne" };
        self.line(&format!("{same} = icmp {cc} ptr {x}, {y}"));
        same
    }

    pub(crate) fn make_closure(
        &mut self,
        dest: LocalId,
        function: &str,
        captures: &[Operand],
    ) -> Result<(), String> {
        let (code, let_go, kinds) = self.closure_code(function, captures.len())?;
        let layouts = self.module.layouts;
        let size = u64::from(CAPTURES) + kinds.iter().map(|kind| u64::from(kind.size(layouts))).sum::<u64>();
        let closure = if self.module.whole.runtime_closures {
            self.table_closure(function, kinds.get(..captures.len()).ok_or("a closure captures too much")?)?
        } else {
            self.declare("declare noalias ptr @malloc(i64) nounwind");
            let closure = self.temp();
            self.line(&format!("{closure} = call ptr @malloc(i64 {size})"));
            if self.module.shared.tracing_costs {
                self.declare("declare void @fennec_trace_allocated(i64) nounwind");
                self.line(&format!("call void @fennec_trace_allocated(i64 {size})"));
            }
            self.line(&format!("store i64 1, ptr {closure}"));
            closure
        };
        let (code_at, let_go_at) = (self.temp(), self.temp());
        self.line(&format!("{code_at} = getelementptr inbounds i8, ptr {closure}, i64 {CODE}"));
        self.line(&format!("store ptr {code}, ptr {code_at}"));
        self.line(&format!("{let_go_at} = getelementptr inbounds i8, ptr {closure}, i64 {LET_GO}"));
        self.line(&format!("store ptr {let_go}, ptr {let_go_at}"));
        let mut offset = CAPTURES;
        for (kind, value) in kinds.iter().zip(captures) {
            let at = self.temp();
            self.line(&format!("{at} = getelementptr inbounds i8, ptr {closure}, i64 {offset}"));
            self.fill(kind, value, &at, "")?;
            offset += kind.size(layouts);
        }
        self.assign(dest, &closure)
    }

    /// A new closure's block in the runtime's table, owned once, whose layout puts its captured
    /// values where its code reads them.
    fn table_closure(&mut self, function: &str, kinds: &[Kind]) -> Result<String, String> {
        use fennec_rt::embed::layout::FIELDS;
        let layouts = self.module.layouts;
        let id = layouts.id(&layout_name(function, kinds.len())).ok_or("a closure with no layout")?;
        let layout = layouts.get(id).ok_or("a closure with no layout")?;
        let start = FIELDS.unsigned_abs();
        let mut offset = CAPTURES;
        for (at, kind) in kinds.iter().enumerate() {
            let field = layout.fields.iter().find(|field| field.name == format!("\0capture{at}"));
            if field.is_none_or(|field| field.offset + start != offset) {
                return Err("a closure's block has its captured values somewhere else".to_owned());
            }
            offset += kind.size(layouts);
        }
        self.new_block(id)
    }

    /// The code for a closure of a function, written once a module: its captured values come
    /// from the closure, and the rest are what it's called with.
    fn closure_code(&mut self, name: &str, captured: usize) -> Result<(String, String, Vec<Kind>), String> {
        let callee = self.module.function_named(name)?;
        self.module.declare_call(callee)?;
        let (code, let_go) = (format!("@\"c_{name}.{captured}\""), format!("@\"g_{name}.{captured}\""));
        let mut kinds = Vec::with_capacity(callee.params);
        for local in callee.locals.iter().take(callee.params) {
            kinds.push(self.module.kind(&local.ty)?);
        }
        let (held, rest) = kinds.split_at(captured.min(kinds.len()));
        let returns = self.module.kind(&callee.returns)?;
        let ret = self.module.ret_text(callee)?;
        let layouts = self.module.layouts;
        let mut params = Vec::new();
        let mut args = Vec::new();
        if returns.in_memory() {
            params.push("ptr %ret".to_owned());
            args.push("ptr %ret".to_owned());
        }
        params.push("ptr %closure".to_owned());
        let mut body = String::new();
        let mut release = String::new();
        let mut offset = CAPTURES;
        for (at, kind) in held.iter().enumerate() {
            let _ = writeln!(body, "  %c{at}_at = getelementptr inbounds i8, ptr %closure, i64 {offset}");
            let _ = writeln!(release, "  %c{at}_at = getelementptr inbounds i8, ptr %closure, i64 {offset}");
            if kind.in_memory() {
                args.push(format!("ptr %c{at}_at"));
            } else if *kind == Kind::Bool {
                let _ =
                    writeln!(body, "  %w{at} = load i64, ptr %c{at}_at\n  %c{at} = trunc i64 %w{at} to i1");
                args.push(format!("i1 %c{at}"));
            } else {
                let text = kind.slot(layouts);
                let _ = writeln!(body, "  %c{at} = load {text}, ptr %c{at}_at");
                args.push(format!("{text} %c{at}"));
            }
            if kind.counted() {
                let free = if matches!(kind, Kind::Closure(..)) {
                    format!(
                        "  %f{at}_at = getelementptr inbounds i8, ptr %h{at}, i64 {LET_GO}\n  %f{at} = load ptr, ptr %f{at}_at\n  call void %f{at}(ptr %h{at})\n"
                    )
                } else {
                    format!("  call void @fennec_free(ptr %h{at})\n")
                };
                // A header holds a block only when byte 23's top bit is clear, and may hold none.
                let block = if *kind == Kind::Str && layouts.text_headers() {
                    format!(
                        "  %l{at}_at = getelementptr inbounds i64, ptr %c{at}_at, i64 2\n  %l{at} = load i64, ptr %l{at}_at\n  %b{at} = icmp sgt i64 %l{at}, -1\n  %a{at} = load i64, ptr %c{at}_at\n  %p{at} = select i1 %b{at}, i64 %a{at}, i64 0\n  %h{at} = inttoptr i64 %p{at} to ptr\n  %e{at} = icmp eq ptr %h{at}, null\n  br i1 %e{at}, label %kept{at}, label %held{at}\nheld{at}:\n"
                    )
                } else {
                    format!("  %h{at} = load ptr, ptr %c{at}_at\n")
                };
                let gone = if matches!(kind, Kind::Closure(..)) && self.module.threads {
                    "@fen_closure_gone"
                } else {
                    "@fen_owner_gone"
                };
                let _ = write!(
                    release,
                    "{block}  %z{at} = call i1 {gone}(ptr %h{at})\n  br i1 %z{at}, label %free{at}, label %kept{at}\nfree{at}:\n{free}  br label %kept{at}\nkept{at}:\n"
                );
            }
            offset += kind.size(layouts);
        }
        for (at, kind) in rest.iter().enumerate() {
            // A `mut` parameter is where the value is, whatever it is.
            let changed = callee.mut_params.contains(&(captured + at));
            let text = if kind.in_memory() || changed { "ptr".to_owned() } else { kind.slot(layouts) };
            params.push(format!("{text} %a{at}"));
            args.push(format!("{text} %a{at}"));
        }
        params.push("ptr swifterror %err".to_owned());
        if self.module.flagged(name) {
            args.push("ptr swifterror %err".to_owned());
        }
        let (assigned, leave) = if ret == "void" {
            (String::new(), "ret void".to_owned())
        } else {
            ("%r = ".to_owned(), format!("ret {ret} %r"))
        };
        let target = symbol(name);
        let thunk = format!(
            "define internal swiftcc {ret} {code}({}) #0 {{\nentry:\n{body}  {assigned}call swiftcc {ret} {target}({})\n  {leave}\n}}\n\n",
            params.join(", "),
            args.join(", ")
        );
        self.module.add_helper(&code, thunk);
        if held.iter().any(Kind::counted) {
            self.declare("declare void @fennec_free(ptr) nounwind");
            self.owner_gone_helper();
            self.closure_helpers();
        }
        self.declare("declare void @free(ptr) nounwind");
        let drop = format!(
            "define internal void {let_go}(ptr %closure) #0 {{\nentry:\n{release}  call void @free(ptr %closure)\n  ret void\n}}\n\n"
        );
        self.module.add_helper(&let_go, drop);
        Ok((code, let_go, kinds))
    }

    /// Calls a closure with arguments already in LLVM's terms, giving back what it gave back: a
    /// value, or the place it's in if it's kept in memory.
    pub(crate) fn call_closure_with(
        &mut self,
        closure: &str,
        returns: &Kind,
        args: &[String],
        span: Span,
    ) -> String {
        self.check_stack(span);
        let (result, failed) = self.call_closure_unchecked(closure, returns, args);
        self.leave_traced(&failed, span);
        result
    }

    /// As [`Self::call_closure_with`], with the stack checked already and a failure left for the
    /// caller to leave by: what it gave back, and an `i1` that holds when it failed.
    pub(crate) fn call_closure_unchecked(
        &mut self,
        closure: &str,
        returns: &Kind,
        args: &[String],
    ) -> (String, String) {
        let mut shown = Vec::with_capacity(args.len() + 3);
        let place = if returns.in_memory() {
            let at = self.place_for(returns);
            shown.push(format!("ptr {at}"));
            Some(at)
        } else {
            None
        };
        shown.push(format!("ptr {closure}"));
        shown.extend_from_slice(args);
        shown.push("ptr swifterror %err".to_owned());
        let ret = if returns.in_memory() || matches!(returns, Kind::Unit) {
            "void".to_owned()
        } else {
            self.text_of(returns)
        };
        let (at, code, result) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{at} = getelementptr inbounds i8, ptr {closure}, i64 {CODE}"));
        // A closure's code never changes while it lives, so a call through one LLVM can see made
        // becomes a direct call, and then inlined.
        self.line(&format!("{code} = load ptr, ptr {at}, !invariant.load !1"));
        if ret == "void" {
            self.line(&format!("call swiftcc void {code}({})", shown.join(", ")));
        } else {
            self.line(&format!("{result} = call swiftcc {ret} {code}({})", shown.join(", ")));
        }
        let (held, failed) = (self.temp(), self.temp());
        self.line(&format!("{held} = load ptr, ptr %err"));
        self.line(&format!("{failed} = icmp ne ptr {held}, null"));
        (place.unwrap_or(result), failed)
    }

    pub(crate) fn call_closure(
        &mut self,
        dest: Option<LocalId>,
        closure: &Operand,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        let (held, kind) = self.operand(closure)?;
        let Kind::Closure(params, returns) = kind else {
            return Err("it calls something that isn't a closure".to_owned());
        };
        let mut shown = Vec::with_capacity(args.len());
        for (param, arg) in params.iter().zip(args) {
            let (value, _) = self.value_for(arg, param)?;
            let text = if param.in_memory() { "ptr".to_owned() } else { self.text_of(param) };
            shown.push(format!("{text} {value}"));
        }
        let result = self.call_closure_with(&held, &returns, &shown, span);
        match dest {
            Some(dest) if returns.in_memory() => self.assign_memory(dest, &result, ""),
            Some(dest) if !matches!(*returns, Kind::Unit) => self.assign(dest, &result),
            None if returns.counted() => {
                self.release_kind(&result, &returns);
                Ok(())
            }
            _ => Ok(()),
        }
    }
}
