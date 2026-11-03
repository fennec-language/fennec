//! `parallel` for native code: the builtins the IR lifts `parallel` loops, comprehensions and
//! sums into (see `fennec_ir`'s `lower::parallel`), run through the runtime's
//! `fennec_parallel_for`, `fennec_parallel_map` and `fennec_parallel_reduce`.
//!
//! The IR gives each a closure of an ordinary function that does the work for a range of the
//! list's positions, compiled like any other. The runtime calls a body as a C function of
//! `(env, from, to, thread)`, so one small shim for each shape of range calls the closure
//! `env` names, its code `swiftcc` as closures are, and says whether it failed: the failure is
//! already in the runtime's state. The runtime decides how ranges are split and where they run.
//!
//! A program that runs `parallel` keeps its stack limit and failure flag for each thread, since a
//! worker's stack and runtime are its own. The program's thread sets them in `fennec_entry`, and
//! a worker in the first shim it runs.
//!
//! `parallel.put` and `parallel.put_back` write an item at a position only one range writes,
//! in place: into the list `fennec_parallel_map` made, or back into a `for mut` loop's list,
//! which `parallel.own` made the loop's own first.

use std::fmt::Write as _;

use fennec_ir::{InstKind, LocalId, Operand, Program};
use fennec_syntax::Span;

use crate::emit::{Emit, ITEM_ACCESS};
use crate::kinds::{Kind, element_symbol};
use crate::lists::list_item;

/// Where a closure's code is, as `closures` lays one out.
const CODE: u32 = 8;

/// Whether a program hands work to the runtime's `parallel` calls, and so keeps a stack limit
/// and failure flag for each thread.
pub fn uses_parallel(program: &Program) -> bool {
    program.functions.iter().chain(&program.constants).flat_map(|function| &function.blocks).any(|block| {
        block
            .insts
            .iter()
            .any(|inst| matches!(&inst.kind, InstKind::Builtin { name, .. } if name.starts_with("parallel.")))
    })
}

/// How a module declares the lowest address the stack may grow to, for this thread when there
/// are threads. Initial-exec, since `fennec_entry`'s module, in the executable, defines it.
pub(crate) fn stack_limit_global(threads: bool) -> &'static str {
    if threads {
        "@fennec_stack_limit = external dso_local thread_local(initialexec) global i64"
    } else {
        "@fennec_stack_limit = external dso_local global i64"
    }
}

/// How a module declares where the runtime's failure flag is, for this thread when there are
/// threads.
pub(crate) fn failed_global(threads: bool) -> &'static str {
    if threads {
        "@fennec_failed = external dso_local thread_local(initialexec) global ptr"
    } else {
        "@fennec_failed = external dso_local global ptr"
    }
}

/// The start of every shim: on a thread whose failure flag isn't known yet, a worker running
/// its first range, asks the runtime where this thread's flag is and how deep its stack goes.
/// The program's thread has both from `fennec_entry`.
const READY: &str = "entry:\n  %known = load ptr, ptr @fennec_failed\n  %fresh = icmp eq ptr %known, null\n  \
    br i1 %fresh, label %ready, label %run\nready:\n  \
    %flag = call i64 @fennec_aot_failed_flag()\n  %flag_at = inttoptr i64 %flag to ptr\n  \
    store ptr %flag_at, ptr @fennec_failed\n  %limit = call i64 @fennec_aot_stack_limit()\n  \
    store i64 %limit, ptr @fennec_stack_limit\n  br label %run\nrun:\n";

/// What a range's closure gives back, which says which shim runs it.
#[derive(Clone, Copy)]
enum Shape {
    /// `(from, to) -> ()`: a loop's range.
    Each,
    /// `(from, to, out) -> ()`: a comprehension's range, putting items into `out`.
    Into,
    /// `(from, to) -> int` or `-> float`: a sum's range, giving back its partial.
    Partial { float: bool },
}

impl Shape {
    fn name(self) -> &'static str {
        match self {
            Shape::Each => "@fen_parallel_range",
            Shape::Into => "@fen_parallel_range_into",
            Shape::Partial { float: false } => "@fen_parallel_partial_int",
            Shape::Partial { float: true } => "@fen_parallel_partial_float",
        }
    }

    /// The shim's text: the runtime's body, calling the closure `%env` names over the range.
    fn shim(self) -> String {
        let name = self.name();
        let (extra, args, call, keep) = match self {
            Shape::Each => ("", "", "call swiftcc void", String::new()),
            Shape::Into => (", i64 %out", ", ptr %into", "call swiftcc void", String::new()),
            Shape::Partial { float: false } => (
                ", ptr %partial",
                "",
                "%got = call swiftcc i64",
                "  store i64 %got, ptr %partial\n".to_owned(),
            ),
            Shape::Partial { float: true } => (
                ", ptr %partial",
                "",
                "%got = call swiftcc double",
                "  %bits = bitcast double %got to i64\n  store i64 %bits, ptr %partial\n".to_owned(),
            ),
        };
        let into = if matches!(self, Shape::Into) { "  %into = inttoptr i64 %out to ptr\n" } else { "" };
        format!(
            "define internal i64 {name}(i64 %env, i64 %from, i64 %to, i64 %thread{extra}) #0 {{\n{READY}  \
             %closure = inttoptr i64 %env to ptr\n{into}  %code_at = getelementptr inbounds i8, ptr %closure, i64 {CODE}\n  \
             %code = load ptr, ptr %code_at\n  %failed = alloca swifterror ptr\n  store ptr null, ptr %failed\n  \
             {call} %code(ptr %closure, i64 %from, i64 %to{args}, ptr swifterror %failed)\n{keep}  \
             %held = load ptr, ptr %failed\n  %bad = icmp ne ptr %held, null\n  %result = zext i1 %bad to i64\n  \
             ret i64 %result\n}}\n\n"
        )
    }
}

impl Emit<'_, '_, '_> {
    /// The builtins `parallel` is lowered to.
    pub(crate) fn parallel_builtin(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
        span: Span,
    ) -> Result<(), String> {
        match (name, args) {
            ("parallel.own", [Operand::Local(list)]) => {
                self.unshared(*list);
                Ok(())
            }
            ("parallel.for", [list, range]) => self.parallel_for(list, range),
            ("parallel.map", [list, range]) => {
                self.parallel_map(dest.ok_or("a map keeps nothing")?, list, range)
            }
            ("parallel.sum", [list, range, start]) => {
                self.parallel_sum(dest.ok_or("a sum keeps nothing")?, (list, range, start), span)
            }
            ("parallel.put" | "parallel.put_back", [list, at, value]) => {
                self.parallel_put(name == "parallel.put_back", (list, at, value))
            }
            _ => Err(format!("`{name}` with the wrong arguments")),
        }
    }

    /// The shim for a range's shape, written once a module.
    fn shim(&mut self, shape: Shape) -> &'static str {
        let name = shape.name();
        self.declare(stack_limit_global(true));
        self.declare(failed_global(true));
        self.declare("declare i64 @fennec_aot_failed_flag()");
        self.declare("declare i64 @fennec_aot_stack_limit()");
        self.module.add_helper(name, shape.shim());
        name
    }

    /// The list's address and the closure's, as the words the runtime takes.
    fn list_and_env(&mut self, list: &Operand, range: &Operand) -> Result<(String, String), String> {
        let ((list, _), (closure, _)) = (self.operand(list)?, self.operand(range)?);
        let env = self.temp();
        self.line(&format!("{env} = ptrtoint ptr {closure} to i64"));
        Ok((list, env))
    }

    /// `parallel.for(list, range)`: each range of the list's positions, as the runtime splits
    /// them.
    fn parallel_for(&mut self, list: &Operand, range: &Operand) -> Result<(), String> {
        let (list, env) = self.list_and_env(list, range)?;
        let shim = self.shim(Shape::Each);
        self.declare("declare i64 @fennec_parallel_for(ptr, ptr, i64) nounwind");
        let done = self.temp();
        self.line(&format!("{done} = call i64 @fennec_parallel_for(ptr {list}, ptr {shim}, i64 {env})"));
        self.leave_if_failed();
        Ok(())
    }

    /// `dest = parallel.map(list, range)`: a new list, each range putting its items' results
    /// in it.
    fn parallel_map(&mut self, dest: LocalId, list: &Operand, range: &Operand) -> Result<(), String> {
        let item = list_item(&self.kind_of(dest)?.clone())?;
        let (list, env) = self.list_and_env(list, range)?;
        let symbol = element_symbol(self.module.layouts, item);
        self.module.shared.elements.insert(symbol.clone(), item);
        self.declare(&format!("@\"{symbol}\" = external dso_local global i64"));
        let shim = self.shim(Shape::Into);
        self.declare("declare ptr @fennec_parallel_map(ptr, ptr, i64, i64) nounwind");
        let (element, made) = (self.temp(), self.temp());
        self.line(&format!("{element} = load i64, ptr @\"{symbol}\", !invariant.load !1"));
        self.line(&format!(
            "{made} = call ptr @fennec_parallel_map(ptr {list}, ptr {shim}, i64 {env}, i64 {element})"
        ));
        self.leave_if_failed();
        self.assign(dest, &made)
    }

    /// `dest = parallel.sum(list, range, start)`: each range's partial, added in the runtime's
    /// fixed tree, then the start, as the interpreter adds them. A list with nothing in it sums
    /// to the start.
    fn parallel_sum(
        &mut self,
        dest: LocalId,
        (list, range, start): (&Operand, &Operand, &Operand),
        span: Span,
    ) -> Result<(), String> {
        let float = match self.kind_of(dest)? {
            Kind::Int => false,
            Kind::Float => true,
            other => return Err(format!("it sums {other:?}s, which this backend doesn't take yet")),
        };
        let (start, _) = self.operand(start)?;
        let (held, env) = self.list_and_env(list, range)?;
        let shim = self.shim(Shape::Partial { float });
        let combine = self.combine(float, span);
        self.declare("declare i64 @fennec_parallel_reduce(ptr, ptr, ptr, i64, ptr) nounwind");
        let (slot, done, length, empty) = (self.temp(), self.temp(), self.list_length(&held), self.temp());
        self.line(&format!("{slot} = alloca i64"));
        self.line(&format!("store i64 0, ptr {slot}"));
        self.line(&format!(
            "{done} = call i64 @fennec_parallel_reduce(ptr {held}, ptr {shim}, ptr {combine}, i64 {env}, ptr {slot})"
        ));
        self.leave_if_failed();
        self.line(&format!("{empty} = icmp eq i64 {length}, 0"));
        let (bits, total) = (self.temp(), self.temp());
        self.line(&format!("{bits} = load i64, ptr {slot}"));
        let added = if float {
            let (tree, sum) = (self.temp(), self.temp());
            self.line(&format!("{tree} = bitcast i64 {bits} to double"));
            self.line(&format!("{sum} = fadd double {start}, {tree}"));
            self.line(&format!("{total} = select i1 {empty}, double {start}, double {sum}"));
            total
        } else {
            // An empty list's tree adds nothing, so its sum is the start, with nothing to check.
            let tree = self.temp();
            self.line(&format!("{tree} = select i1 {empty}, i64 0, i64 {bits}"));
            self.checked_add(&start, &tree, span)
        };
        self.assign(dest, &added)
    }

    /// The runtime's `combine` for a sum: two partials added, the earlier first. An int sum that
    /// overflows fails as the interpreter does, at the sum: where that is, worked out here as
    /// any failure's is, waits in a global of the sum's own for the combine to read.
    fn combine(&mut self, float: bool, span: Span) -> String {
        if float {
            let name = "@fen_parallel_add_float";
            self.module.add_helper(
                name,
                format!(
                    "define internal i64 {name}(i64 %env, i64 %a, i64 %b, ptr %out) #0 {{\nentry:\n  \
                     %x = bitcast i64 %a to double\n  %y = bitcast i64 %b to double\n  %sum = fadd double %x, %y\n  \
                     %bits = bitcast double %sum to i64\n  store i64 %bits, ptr %out\n  ret i64 0\n}}\n\n"
                ),
            );
            return name.to_owned();
        }
        let label = self.label("sum");
        let (text, _) = self.span_values(&label, span);
        for line in text.lines() {
            self.line(line.trim());
        }
        let (at, name) =
            (format!("@\"fen_parallel_span.{label}\""), format!("@\"fen_parallel_add_int.{label}\""));
        self.module.add_helper(&at, format!("{at} = internal global [2 x i64] zeroinitializer\n"));
        let end_at = self.temp();
        self.line(&format!("store i64 %a{label}, ptr {at}"));
        self.line(&format!("{end_at} = getelementptr inbounds i64, ptr {at}, i64 1"));
        self.line(&format!("store i64 %b{label}, ptr {end_at}"));
        self.declare("declare { i64, i1 } @llvm.sadd.with.overflow.i64(i64, i64)");
        self.declare("declare void @fennec_fail(i64, i64, i64) #1");
        let mut text =
            format!("define internal i64 {name}(i64 %env, i64 %a, i64 %b, ptr %out) #0 {{\nentry:\n");
        let _ = write!(
            text,
            "  %pair = call {{ i64, i1 }} @llvm.sadd.with.overflow.i64(i64 %a, i64 %b)\n  \
             %sum = extractvalue {{ i64, i1 }} %pair, 0\n  %bad = extractvalue {{ i64, i1 }} %pair, 1\n  \
             br i1 %bad, label %overflow, label %fine\nfine:\n  store i64 %sum, ptr %out\n  ret i64 0\n\
             overflow:\n  %start = load i64, ptr {at}\n  %end_at = getelementptr inbounds i64, ptr {at}, i64 1\n  \
             %end = load i64, ptr %end_at\n  call void @fennec_fail(i64 1, i64 %start, i64 %end)\n  ret i64 1\n}}\n\n"
        );
        self.module.add_helper(&name, text);
        name
    }

    /// `parallel.put(out, at, value)` or `parallel.put_back(list, at, value)`: an item written
    /// at a position only this range writes, in place. What was there goes, by its kind:
    /// nothing in a new list, the old item in a `for mut`'s.
    fn parallel_put(
        &mut self,
        back: bool,
        (list, at, value): (&Operand, &Operand, &Operand),
    ) -> Result<(), String> {
        let (held, kind) = self.operand(list)?;
        let item = self.item_kind(list_item(&kind)?)?;
        let ((place, _), (value, value_kind)) = (self.operand(at)?, self.operand(value)?);
        let memory = item.memory(self.module.layouts);
        let slot = self.item_address(&held, &memory, &place);
        if back {
            if item.in_memory() {
                self.release_inline(&item, &slot);
            } else if item.counted() {
                let old = self.load_field(&item, &slot, ITEM_ACCESS);
                self.release_kind(&old, &item);
            }
        }
        self.put_item(&item, &value_kind, &value, &slot);
        Ok(())
    }
}

// Windows has no target here yet, as `fennec build` doesn't link there.
#[cfg(all(test, not(windows)))]
mod tests {
    /// Every function's text, lowered with `parallel` lifted, as a backend that runs it asks.
    fn texts(source: &str) -> Result<String, String> {
        let mut module = fennec_syntax::parse(source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let options = fennec_ir::Options { outline_parallel: true };
        let (units, _, _) = crate::emit::emit(&fennec_ir::lower_with(&module, &checked, options))?;
        Ok(units.into_iter().map(|unit| unit.text).collect())
    }

    #[test]
    fn a_loop_a_comprehension_and_a_sum_over_parallel_run_through_the_runtime() -> Result<(), String> {
        let source = "\
def show(stdout: Stdout, xs: list[int]):
    for x in parallel(xs):
        stdout.print(x)

def squares(xs: list[int]) -> list[int]:
    [x * x for x in parallel(xs)]

def total(xs: list[int]) -> int:
    sum(x for x in parallel(xs))

def main(stdout: Stdout):
    show(stdout, [1])
    stdout.print(squares([2]), total([3]))
";
        let text = texts(source)?;
        for wanted in [
            "@fennec_parallel_for(ptr",
            "@fen_parallel_range,",
            "@fennec_parallel_map(ptr",
            "@fen_parallel_range_into,",
            "@fennec_parallel_reduce(ptr",
            "@fen_parallel_partial_int,",
            "@llvm.sadd.with.overflow.i64(i64 %a, i64 %b)",
        ] {
            assert!(text.contains(wanted), "no {wanted} in:\n{text}");
        }
        // Each shim calls the closure the runtime hands it, as closures are called.
        assert!(
            text.contains(
                "call swiftcc void %code(ptr %closure, i64 %from, i64 %to, ptr swifterror %failed)"
            )
        );
        Ok(())
    }

    #[test]
    fn a_for_mut_over_parallel_makes_the_list_its_own_and_puts_items_back_in_place() -> Result<(), String> {
        let source = "\
def grow(mut xs: list[int], by: int):
    for mut x in parallel(xs):
        x += by

def main(stdout: Stdout):
    mut xs = [1, 2]
    grow(mut xs, 3)
    stdout.print(xs)
";
        let text = texts(source)?;
        assert!(text.contains("@fennec_parallel_for(ptr"), "{text}");
        // The range writes in place: no copy of the list it holds, which would lose the change.
        assert!(!text.contains("fennec_list_copy"), "{text}");
        Ok(())
    }
}
