//! Closures, which live outside the runtime's heap: the address of an owner count, the code to
//! call, the code that lets go of it, then the values it captured. The code takes that address
//! first, then the arguments, and hands failure back in the usual register whether or not the
//! function underneath can fail, so that any closure can be called the same way.

use std::fmt::Write as _;

use fennec_ir::{LocalId, Operand};
use fennec_syntax::Span;

use crate::emit::{Emit, symbol};
use crate::kinds::Kind;

/// Where a closure's code and captured values are.
const CODE: u32 = 8;
const LET_GO: u32 = 16;
const CAPTURES: u32 = 24;

impl Emit<'_, '_, '_> {
    /// One owner fewer for a closure, freeing it when that was the last.
    pub(crate) fn release_closure(&mut self, closure: &str) {
        let (none, held, last, done) =
            (self.temp(), self.label("held"), self.label("last"), self.label("done"));
        self.line(&format!("{none} = icmp eq ptr {closure}, null"));
        self.line(&format!("br i1 {none}, label %{done}, label %{held}"));
        self.start_block(&held);
        let (count, fewer, gone) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{count} = load i64, ptr {closure}"));
        self.line(&format!("{fewer} = add i64 {count}, -1"));
        self.line(&format!("store i64 {fewer}, ptr {closure}"));
        self.line(&format!("{gone} = icmp eq i64 {fewer}, 0"));
        self.line(&format!("br i1 {gone}, label %{last}, label %{done}"));
        self.start_block(&last);
        let (at, code) = (self.temp(), self.temp());
        self.line(&format!("{at} = getelementptr inbounds i8, ptr {closure}, i64 {LET_GO}"));
        self.line(&format!("{code} = load ptr, ptr {at}"));
        self.line(&format!("call void {code}(ptr {closure})"));
        self.line(&format!("br label %{done}"));
        self.start_block(&done);
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
        self.declare("declare noalias ptr @malloc(i64) nounwind");
        let (closure, code_at, let_go_at) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{closure} = call ptr @malloc(i64 {size})"));
        self.line(&format!("store i64 1, ptr {closure}"));
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

    /// The code for a closure of a function, written once a module: its captured values come
    /// from the closure, and the rest are what it's called with.
    fn closure_code(&mut self, name: &str, captured: usize) -> Result<(String, String, Vec<Kind>), String> {
        let callee = self.module.function_named(name)?;
        if !callee.mut_params.is_empty() {
            return Err(format!(
                "a closure of `{name}` takes a `mut` parameter, which this backend doesn't take yet"
            ));
        }
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
                let _ = write!(
                    release,
                    "  %h{at} = load ptr, ptr %c{at}_at\n  %o{at} = load i64, ptr %h{at}\n  %n{at} = add i64 %o{at}, -1\n  store i64 %n{at}, ptr %h{at}\n  %z{at} = icmp eq i64 %n{at}, 0\n  br i1 %z{at}, label %free{at}, label %kept{at}\nfree{at}:\n{free}  br label %kept{at}\nkept{at}:\n"
                );
            }
            offset += kind.size(layouts);
        }
        for (at, kind) in rest.iter().enumerate() {
            let text = if kind.in_memory() { "ptr".to_owned() } else { kind.slot(layouts) };
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
        let ret = if returns.in_memory() || matches!(returns, Kind::Unit | Kind::Cap) {
            "void".to_owned()
        } else {
            self.text_of(returns)
        };
        let (at, code, result) = (self.temp(), self.temp(), self.temp());
        self.line(&format!("{at} = getelementptr inbounds i8, ptr {closure}, i64 {CODE}"));
        self.line(&format!("{code} = load ptr, ptr {at}"));
        if ret == "void" {
            self.line(&format!("call swiftcc void {code}({})", shown.join(", ")));
        } else {
            self.line(&format!("{result} = call swiftcc {ret} {code}({})", shown.join(", ")));
        }
        let (held, failed) = (self.temp(), self.temp());
        self.line(&format!("{held} = load ptr, ptr %err"));
        self.line(&format!("{failed} = icmp ne ptr {held}, null"));
        self.leave_if(&failed);
        place.unwrap_or(result)
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
            Some(dest) if !matches!(*returns, Kind::Unit | Kind::Cap) => self.assign(dest, &result),
            None if returns.counted() => {
                self.release_kind(&result, &returns);
                Ok(())
            }
            _ => Ok(()),
        }
    }
}
