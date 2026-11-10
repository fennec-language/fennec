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
//! All of such a program's counting checks whose block it is, wherever it runs, since a closure
//! made anywhere can be called in a range. A block's own thread counts it with plain arithmetic
//! as ever. Any other thread leaves that to the runtime (see its `threads`), and a block isn't
//! alone to any thread but its own. A closure isn't a block: it has no owner to check, and any
//! thread that holds one counts it atomically.
//!
//! `parallel.put` and `parallel.put_back` write an item at a position only one range writes,
//! in place: into the list `fennec_parallel_map` made, or back into a `for mut` loop's list,
//! which `parallel.own` made the loop's own first.

use std::collections::{HashMap, HashSet};

use std::fmt::Write as _;

use fennec_ir::{Function, InstKind, LocalId, Operand, Program};
use fennec_syntax::Span;

use crate::closures::CODE;
use crate::emit::{Emit, HEADER_ACCESS, ITEM_ACCESS};
use crate::kinds::{Kind, element_symbol};
use crate::lists::list_item;

/// Whether a builtin needs TLS: an HTTP request, a raw socket asked for it with `tls=True`, or a
/// plain connection upgraded to it.
pub(crate) fn is_request(name: &str) -> bool {
    ["Net.get", "Net.post", "Net.request", "Net._connect.tls", "Net._listen.tls", "Connection.start_tls"]
        .iter()
        .any(|request| name.starts_with(request))
}

/// Whether a program makes HTTP requests.
pub fn uses_tls(program: &Program) -> bool {
    program.functions.iter().chain(&program.constants).flat_map(|function| &function.blocks).any(|block| {
        block
            .insts
            .iter()
            .any(|inst| matches!(&inst.kind, InstKind::Builtin { name, .. } if is_request(name)))
    })
}

/// Whether a program hands work to the runtime's `parallel` calls or launches a task, and so keeps a stack limit
/// and failure flag for each thread.
pub fn uses_parallel(program: &Program) -> bool {
    program.functions.iter().chain(&program.constants).flat_map(|function| &function.blocks).any(|block| {
        block
            .insts
            .iter()
            .any(|inst| matches!(&inst.kind, InstKind::Builtin { name, .. } if runs_beside(name)))
    })
}

/// Whether a program starts a task with `with launch`, which only the threads runtime can run:
/// unlike `parallel`'s loops, it has no way to run in order on one thread.
pub fn uses_launch(program: &Program) -> bool {
    program.functions.iter().chain(&program.constants).flat_map(|function| &function.blocks).any(|block| {
        block
            .insts
            .iter()
            .any(|inst| matches!(&inst.kind, InstKind::Builtin { name, .. } if name == "intrinsics.launch"))
    })
}

/// Whether a builtin runs code on another of the runtime's threads: `parallel`'s, and
/// `with launch`'s task.
fn runs_beside(name: &str) -> bool {
    name.starts_with("parallel.") || name == "intrinsics.launch"
}

/// Whether a program has any `shared` type.
pub(crate) fn has_cells(program: &Program) -> bool {
    program.structs.iter().any(|def| def.shared)
}

/// The functions that can run on a thread `parallel` uses. A range is a closure, and anything
/// running on one can call any other closure, resume any generator, or let go of one, so all of
/// those start this, with what writes a value's own text, and it takes in all they call. Every
/// other function only ever runs on the program's own thread, between ranges, and needs nothing
/// a thread of its own would need. Starting from them keeps a program's plain code as fast as it
/// would be without `parallel`.
pub(crate) fn run_on_workers(program: &Program) -> HashSet<&str> {
    let mut found = HashSet::new();
    if !uses_parallel(program) {
        return found;
    }
    let every = || program.functions.iter().chain(&program.constants);
    // A `shared` value's handle is only ever counted by the runtime, and a thread under its lock
    // has another number, so nothing can count without asking who it is.
    if has_cells(program) {
        return every().map(|function| function.name.as_str()).collect();
    }
    let functions: HashMap<&str, &Function> =
        every().map(|function| (function.name.as_str(), function)).collect();
    let closures =
        every().flat_map(|function| &function.blocks).flat_map(|block| &block.insts).filter_map(|inst| {
            match &inst.kind {
                InstKind::MakeClosure { function, .. } => Some(function.as_str()),
                _ => None,
            }
        });
    let generators = program
        .generators
        .iter()
        .flat_map(|def| std::iter::once(def.resume.as_str()).chain(def.abandon.as_deref()));
    let own_texts = program.own_texts.iter().map(|own| own.function.as_str());
    let mut waiting: Vec<&str> = closures.chain(generators).chain(own_texts).collect();
    while let Some(name) = waiting.pop() {
        if found.insert(name)
            && let Some(function) = functions.get(name)
        {
            waiting.extend(crate::analysis::callees(function));
        }
    }
    found
}

/// In a program that uses `parallel`, the program's own thread's stack limit and failure flag,
/// for what only ever runs on it to read without asking which thread this is.
pub(crate) const MAIN_THREAD_GLOBALS: &str =
    "@fennec_stack_limit_main = dso_local global i64 0\n@fennec_failed_main = dso_local global ptr null\n";

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

/// How a module declares this thread's number, which is what a block's owner bits hold when
/// this thread made it. It's 0 on the program's thread, and a worker's first shim sets it.
const THREAD_GLOBAL: &str = "@fennec_thread = external dso_local thread_local(initialexec) global i64";

/// How a function declares `@fennec_thread`, to change it.
pub(crate) fn thread_global() -> &'static str {
    THREAD_GLOBAL
}

/// Where a block's shape word is, whose top 16 bits say which thread made it.
const SHAPE: u32 = 8;
const OWNER_SHIFT: u32 = 48;

/// The start of a function about a block's owners: `%owner` is the thread that made `%block`, and
/// `%mine` says whether that's this one.
fn whose_block(changes: bool) -> String {
    // Under a `shared` value's write lock a thread goes by another number.
    let invariant = if changes { "" } else { ", !invariant.load !1" };
    format!(
        "  %shape_at = getelementptr inbounds i8, ptr %block, i64 {SHAPE}\n  %shape = load i64, ptr %shape_at\n  \
         %owner = lshr i64 %shape, {OWNER_SHIFT}\n  %me = load i64, ptr @fennec_thread{invariant}\n  \
         %mine = icmp eq i64 %owner, %me\n"
    )
}

/// In a function about another thread's block, whether it's one that's never freed, like a
/// string literal, so a thread needn't count it at all, and a body using one on every iteration
/// costs nothing here. `%immortal` says so. It's read from the owner in the shape word, which
/// `whose_block` has, never from the count: only a block's own thread may touch its count, as it
/// changes it without atomics, and a launched task runs beside the thread that made what it holds.
fn immortal() -> String {
    format!("  %immortal = icmp eq i64 %owner, {}\n", fennec_rt::embed::heap::NEVER_FREED_OWNER)
}

/// The start of every shim: on a thread whose failure flag isn't known yet, a worker running
/// its first range, asks the runtime where this thread's flag is, and notes its number. The
/// program's thread has both from `fennec_entry`. Then, for every range, it asks how deep its stack
/// may go: a range whose body may wait runs as a parked task, on a stack of its own, so the
/// worker's stack limit isn't its. Everything else reads these as never changing, so the range
/// runs in a function that's never inlined here: no such read can move above these stores.
const READY: &str = "entry:\n  %known = load ptr, ptr @fennec_failed\n  %fresh = icmp eq ptr %known, null\n  \
    br i1 %fresh, label %ready, label %set_limit\nready:\n  \
    %flag = call i64 @fennec_aot_failed_flag()\n  %flag_at = inttoptr i64 %flag to ptr\n  \
    store ptr %flag_at, ptr @fennec_failed\n  store i64 %thread, ptr @fennec_thread\n  br label %set_limit\nset_limit:\n  \
    %stack_limit = call i64 @fennec_aot_stack_limit()\n  store i64 %stack_limit, ptr @fennec_stack_limit\n  br label %run\nrun:\n";

/// What a range's closure gives back, which says which shim runs it.
#[derive(Clone, Copy)]
enum Shape {
    /// `(from, to) -> ()`: a loop's range.
    Each,
    /// `(from, to, out) -> ()`: a comprehension's range, putting items into `out`.
    Into,
    /// `(from, to, out) -> int`: a comprehension's range whose items can fail, giving back 0, or
    /// 2 when it handed a failure over.
    IntoFailing,
    /// `(from, to) -> int` or `-> float`: a sum's range, giving back its partial.
    Partial { float: bool },
    /// `(conn) -> ()`: a server's task for a connection, on a stack of its own.
    Task,
    /// `() -> int`: a launched task, giving back 0, or 2 when it handed a failure over.
    Launch,
}

impl Shape {
    fn name(self) -> &'static str {
        match self {
            Shape::Each => "@fen_parallel_range",
            Shape::Into => "@fen_parallel_range_into",
            Shape::IntoFailing => "@fen_parallel_range_into_failing",
            Shape::Partial { float: false } => "@fen_parallel_partial_int",
            Shape::Partial { float: true } => "@fen_parallel_partial_float",
            Shape::Task => "@fen_parallel_task",
            Shape::Launch => "@fen_launched_task",
        }
    }

    /// The shim's text: the runtime's body, calling the closure `%env` names over the range.
    fn shim(self) -> String {
        if matches!(self, Shape::Task) {
            return task_shim();
        }
        if matches!(self, Shape::Launch) {
            return launch_shim();
        }
        let name = self.name();
        let (extra, args, call, keep) = match self {
            Shape::Each | Shape::Task | Shape::Launch => ("", "", "call swiftcc void", String::new()),
            Shape::Into => (", i64 %out", ", ptr %into", "call swiftcc void", String::new()),
            Shape::IntoFailing => (", i64 %out", ", ptr %into", "%ended = call swiftcc i64", String::new()),
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
        let into = if matches!(self, Shape::Into | Shape::IntoFailing) {
            "  %into = inttoptr i64 %out to ptr\n"
        } else {
            ""
        };
        // A crash is 1, with the failure flag set; otherwise what the range gave back, if it says.
        let result = if matches!(self, Shape::IntoFailing) {
            "%result = select i1 %bad, i64 1, i64 %ended"
        } else {
            "%result = zext i1 %bad to i64"
        };
        let params = format!("i64 %env, i64 %from, i64 %to, i64 %thread{extra}");
        let running = format!("@\"{}.run\"", name.trim_start_matches('@'));
        format!(
            "define internal i64 {running}({params}) noinline #0 {{\nentry:\n  \
             %closure = inttoptr i64 %env to ptr\n{into}  %code_at = getelementptr inbounds i8, ptr %closure, i64 {CODE}\n  \
             %code = load ptr, ptr %code_at\n  %failed = alloca swifterror ptr\n  store ptr null, ptr %failed\n  \
             {call} %code(ptr %closure, i64 %from, i64 %to{args}, ptr swifterror %failed)\n{keep}  \
             %held = load ptr, ptr %failed\n  %bad = icmp ne ptr %held, null\n  {result}\n  \
             ret i64 %result\n}}\n\n\
             define internal i64 {name}({params}) #0 {{\n{READY}  \
             %result = call i64 {running}(i64 %env, i64 %from, i64 %to, i64 %thread{extra})\n  ret i64 %result\n}}\n\n"
        )
    }
}

/// The shim for a server's task: as a range's, but each task has a stack of its own, so its limit
/// is set every time, whatever this thread has run.
fn task_shim() -> String {
    let name = Shape::Task.name();
    let running = format!("@\"{}.run\"", name.trim_start_matches('@'));
    format!(
        "define internal i64 {running}(i64 %env, i64 %conn) noinline #0 {{\nentry:\n  \
         %closure = inttoptr i64 %env to ptr\n  %code_at = getelementptr inbounds i8, ptr %closure, i64 {CODE}\n  \
         %code = load ptr, ptr %code_at\n  %failed = alloca swifterror ptr\n  store ptr null, ptr %failed\n  \
         %ended = call swiftcc i64 %code(ptr %closure, i64 %conn, ptr swifterror %failed)\n  \
         %held = load ptr, ptr %failed\n  %bad = icmp ne ptr %held, null\n  %result = select i1 %bad, i64 1, i64 %ended\n  \
         ret i64 %result\n}}\n\n\
         define internal i64 {name}(i64 %env, i64 %conn, i64 %thread) #0 {{\n{READY}  \
         %task_limit = call i64 @fennec_aot_stack_limit()\n  store i64 %task_limit, ptr @fennec_stack_limit\n  \
         %result = call i64 {running}(i64 %env, i64 %conn)\n  ret i64 %result\n}}\n\n"
    )
}

/// The shim for a launched task: its closure takes nothing, and its stack is its own, which the
/// prelude reads the limit of. A crash is 1, with the failure flag set; otherwise what it gave back.
fn launch_shim() -> String {
    let name = Shape::Launch.name();
    let running = format!("@\"{}.run\"", name.trim_start_matches('@'));
    format!(
        "define internal i64 {running}(i64 %env) noinline #0 {{\nentry:\n  \
         %closure = inttoptr i64 %env to ptr\n  %code_at = getelementptr inbounds i8, ptr %closure, i64 {CODE}\n  \
         %code = load ptr, ptr %code_at\n  %failed = alloca swifterror ptr\n  store ptr null, ptr %failed\n  \
         %ended = call swiftcc i64 %code(ptr %closure, ptr swifterror %failed)\n  \
         %held = load ptr, ptr %failed\n  %bad = icmp ne ptr %held, null\n  %result = select i1 %bad, i64 1, i64 %ended\n  \
         ret i64 %result\n}}\n\n\
         define internal i64 {name}(i64 %env, i64 %thread) #0 {{\n{READY}  \
         %result = call i64 {running}(i64 %env)\n  ret i64 %result\n}}\n\n"
    )
}

impl Emit<'_, '_, '_> {
    /// `intrinsics.launch(task)`: starts the task, a closure the IR made that gives back how it
    /// ended, on the runtime's threads, which copy it for the task. `intrinsics.launch_take`, what
    /// the IR makes of `launch_end`: waits for it, sets `why` to how it ended, and takes its
    /// failure into `dest` when that's 2, leaving it empty otherwise for the IR to make `None`.
    pub(crate) fn launch_builtin(
        &mut self,
        dest: Option<LocalId>,
        name: &str,
        args: &[Operand],
    ) -> Result<(), String> {
        match (name, args) {
            ("intrinsics.launch", [task]) => {
                let (closure, _) = self.operand(task)?;
                let env = self.temp();
                self.line(&format!("{env} = ptrtoint ptr {closure} to i64"));
                let shim = self.shim(Shape::Launch);
                self.declare("declare i64 @fennec_launch(ptr, i64) nounwind");
                let started = self.temp();
                self.line(&format!("{started} = call i64 @fennec_launch(ptr {shim}, i64 {env})"));
                // With no thread to run it on, it fails.
                self.leave_if_failed();
                self.assign(dest.ok_or("a task no one waits for")?, &started)
            }
            ("intrinsics.launch_take", [task, nanos, failed, Operand::Local(why)]) => {
                let dest = dest.ok_or("a failure no one keeps")?;
                let ((task, _), (nanos, _), (failed, _)) =
                    (self.operand(task)?, self.operand(nanos)?, self.operand(failed)?);
                let failed_word = self.temp();
                self.line(&format!("{failed_word} = zext i1 {failed} to i64"));
                let failure_at = self.place_for(&Kind::Int);
                self.declare("declare i64 @fennec_launch_end(i64, i64, i64, ptr) nounwind");
                let how = self.temp();
                self.line(&format!(
                    "{how} = call i64 @fennec_launch_end(i64 {task}, i64 {nanos}, i64 {failed_word}, ptr {failure_at})"
                ));
                // A failure this thread had before waiting is its own again.
                self.leave_if_failed();
                self.assign(*why, &how)?;
                let (failed_task, take, nothing, done) = (
                    self.temp(),
                    self.label("task_failed"),
                    self.label("task_ended"),
                    self.label("task_waited"),
                );
                self.line(&format!("{failed_task} = icmp eq i64 {how}, 2"));
                self.line(&format!("br i1 {failed_task}, label %{take}, label %{nothing}"));
                self.start_block(&take);
                let (word, block) = (self.temp(), self.temp());
                self.line(&format!("{word} = load i64, ptr {failure_at}"));
                self.line(&format!("{block} = inttoptr i64 {word} to ptr"));
                self.take_handed_failure(dest, &block)?;
                self.line(&format!("br label %{done}"));
                self.start_block(&nothing);
                let kind = self.kind_of(dest)?.clone();
                let slot = self.slot(dest);
                let zero = self.zero_line(&kind, &slot);
                self.line(&zero);
                self.line(&format!("br label %{done}"));
                self.start_block(&done);
                Ok(())
            }
            _ => Err(format!("it calls `{name}` in a way this backend doesn't take yet")),
        }
    }

    /// Defines `@fen_retain` for a program that runs `parallel`: one more owner of a block, by
    /// whichever way this thread counts it.
    pub(crate) fn counted_retain_helper(&mut self) {
        self.declare(THREAD_GLOBAL);
        self.declare("declare void @fennec_retain_shared(i64, i64) nounwind");
        let (start, immortal) = (whose_block(self.module.cells), immortal());
        self.module.add_helper(
            "fen_retain",
            format!(
                "define internal void @fen_retain(ptr %block) #0 {{\nentry:\n{start}  br i1 %mine, label %own, label %other\nown:\n  \
                 %count = load i64, ptr %block{HEADER_ACCESS}\n  %more = add i64 %count, 1\n  store i64 %more, ptr %block{HEADER_ACCESS}\n  ret void\n\
                 other:\n{immortal}  br i1 %immortal, label %kept, label %counted\nkept:\n  ret void\n\
                 counted:\n  %at = ptrtoint ptr %block to i64\n  call void @fennec_retain_shared(i64 %at, i64 %owner)\n  ret void\n}}\n\n"
            ),
        );
    }

    /// Defines `@fen_owner_gone` for a program that runs `parallel`: one owner fewer of a block,
    /// by whichever way this thread counts it, and whether that was the last one here to free.
    pub(crate) fn counted_owner_gone_helper(&mut self) {
        self.declare(THREAD_GLOBAL);
        self.declare("declare void @fennec_release_shared(i64, i64) nounwind");
        let (start, immortal) = (whose_block(self.module.cells), immortal());
        self.module.add_helper(
            "fen_owner_gone",
            format!(
                "define internal i1 @fen_owner_gone(ptr %block) #0 {{\nentry:\n{start}  br i1 %mine, label %own, label %other\nown:\n  \
                 %count = load i64, ptr %block{HEADER_ACCESS}\n  %fewer = add i64 %count, -1\n  store i64 %fewer, ptr %block{HEADER_ACCESS}\n  \
                 %gone = icmp eq i64 %fewer, 0\n  ret i1 %gone\nother:\n{immortal}  br i1 %immortal, label %kept, label %counted\nkept:\n  ret i1 false\n\
                 counted:\n  %at = ptrtoint ptr %block to i64\n  call void @fennec_release_shared(i64 %at, i64 %owner)\n  ret i1 false\n}}\n\n"
            ),
        );
    }

    /// Defines `@fen_alone`, for a program that runs `parallel`: whether a block is this thread's,
    /// with one owner.
    pub(crate) fn alone_helper(&mut self) {
        self.declare(THREAD_GLOBAL);
        let start = whose_block(self.module.cells);
        self.module.add_helper(
            "fen_alone",
            format!(
                "define internal i1 @fen_alone(ptr %block) #0 {{\nentry:\n{start}  br i1 %mine, label %own, label %other\nown:\n  \
                 %count = load i64, ptr %block{HEADER_ACCESS}\n  %one = icmp eq i64 %count, 1\n  ret i1 %one\nother:\n  ret i1 false\n}}\n\n"
            ),
        );
    }

    /// Defines what counts a closure in a program that runs `parallel`: atomically, as any thread
    /// may hold one.
    pub(crate) fn closure_helpers(&mut self) {
        if !self.module.threads {
            return;
        }
        self.module.add_helper(
            "fen_closure_retain",
            "define internal void @fen_closure_retain(ptr %closure) #0 {\nentry:\n  %old = atomicrmw add ptr %closure, i64 1 monotonic\n  ret void\n}\n\n".to_owned(),
        );
        self.module.add_helper(
            "fen_closure_gone",
            "define internal i1 @fen_closure_gone(ptr %closure) #0 {\nentry:\n  %old = atomicrmw sub ptr %closure, i64 1 acq_rel\n  %gone = icmp eq i64 %old, 1\n  ret i1 %gone\n}\n\n".to_owned(),
        );
    }

    /// One more owner for a closure.
    pub(crate) fn retain_closure(&mut self, closure: &str) {
        if self.module.whole.runtime_closures {
            self.retain(closure);
        } else if self.module.threads {
            self.closure_helpers();
            self.line(&format!("call void @fen_closure_retain(ptr {closure})"));
        } else {
            self.retain(closure);
        }
    }

    /// One owner fewer for a closure, saying whether that was the last.
    pub(crate) fn closure_gone(&mut self, closure: &str) -> String {
        if !self.module.threads {
            return self.owner_gone(closure);
        }
        self.closure_helpers();
        let gone = self.temp();
        self.line(&format!("{gone} = call i1 @fen_closure_gone(ptr {closure})"));
        gone
    }

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
            ("parallel.serve", [server, task]) => self.parallel_serve(dest, server, task),
            // A comprehension whose items can fail made no list when one failed with a value.
            ("parallel.map_failed", [list]) => {
                let (list, _) = self.operand(list)?;
                let none = self.temp();
                self.line(&format!("{none} = icmp eq ptr {list}, null"));
                self.assign(dest.ok_or("a test no one keeps")?, &none)
            }
            ("parallel.failure", []) => {
                self.declare("declare i64 @fennec_parallel_failure() nounwind");
                let (number, block) = (self.temp(), self.temp());
                self.line(&format!("{number} = call i64 @fennec_parallel_failure()"));
                self.line(&format!("{block} = inttoptr i64 {number} to ptr"));
                self.take_handed_failure(dest.ok_or("a failure no one keeps")?, &block)
            }
            ("parallel.serve_failure", []) => {
                self.declare("declare i64 @fennec_net_serve_failure() nounwind");
                let (number, block) = (self.temp(), self.temp());
                self.line(&format!("{number} = call i64 @fennec_net_serve_failure()"));
                self.line(&format!("{block} = inttoptr i64 {number} to ptr"));
                self.take_handed_failure(dest.ok_or("a failure no one keeps")?, &block)
            }
            ("parallel.peer", [Operand::Local(conn)]) => {
                let (handle, _) = self.operand(&Operand::Local(*conn))?;
                self.socket_method(dest, &handle, "Connection.peer", &[], span)
            }
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
        // Shims are only in programs that run `parallel`, whose modules all declare these for
        // each thread; asking the module keeps every declaration in it alike.
        let threads = self.module.threads;
        self.declare(stack_limit_global(threads));
        self.declare(failed_global(threads));
        self.declare(THREAD_GLOBAL);
        self.declare("declare i64 @fennec_aot_failed_flag()");
        self.declare("declare i64 @fennec_aot_stack_limit()");
        self.module.add_helper(name, shape.shim());
        name
    }

    /// The list's address and the closure's, as the words the runtime takes.
    /// The ending of the runtime's entry point for a `parallel` call whose ranges run `range`:
    /// `_waiting` when that body may wait, so the scheduler can park a range rather than its
    /// thread, and nothing for one that only computes. A body made here is known; one passed
    /// in may be any closure, and waits if any the program makes may.
    fn waiting(&self, range: &Operand) -> &'static str {
        let shared = &self.module.shared;
        let any_closure = !shared.waiting_closures.is_empty();
        let Operand::Local(local) = range else {
            return if any_closure { "_waiting" } else { "" };
        };
        // Every closure written to it, when only closures made here are.
        let writes: Vec<Option<&String>> = self
            .function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|inst| inst.kind.dest() == Some(*local))
            .map(|inst| match &inst.kind {
                InstKind::MakeClosure { function, .. } => Some(function),
                _ => None,
            })
            .collect();
        let known = !writes.is_empty() && writes.iter().all(Option::is_some);
        let waits = if known {
            writes.iter().flatten().any(|function| shared.waiting.contains(*function))
        } else {
            any_closure
        };
        if waits { "_waiting" } else { "" }
    }

    fn list_and_env(&mut self, list: &Operand, range: &Operand) -> Result<(String, String), String> {
        let ((list, _), (closure, _)) = (self.operand(list)?, self.operand(range)?);
        let env = self.temp();
        self.line(&format!("{env} = ptrtoint ptr {closure} to i64"));
        Ok((list, env))
    }

    /// `parallel.serve(server, task)`: each connection a task of the runtime's, until it's stopped,
    /// giving back 2 when a round failed and passed its failure up.
    fn parallel_serve(
        &mut self,
        dest: Option<LocalId>,
        server: &Operand,
        task: &Operand,
    ) -> Result<(), String> {
        let handle = self.socket_handle(server)?;
        let (closure, _) = self.operand(task)?;
        let env = self.temp();
        self.line(&format!("{env} = ptrtoint ptr {closure} to i64"));
        let shim = self.shim(Shape::Task);
        self.declare("declare i64 @fennec_net_serve(i64, ptr, i64) nounwind");
        let done = self.temp();
        self.line(&format!("{done} = call i64 @fennec_net_serve(i64 {handle}, ptr {shim}, i64 {env})"));
        self.leave_if_failed();
        match dest {
            Some(dest) => self.assign(dest, &done),
            None => Ok(()),
        }
    }

    /// `task.failed(error)`: hands a task's failure over to whoever's waiting for it, as a block of
    /// the runtime's, before the task gives back that it failed. A failure held in place goes in a
    /// block of its own type.
    pub(crate) fn hand_over_failure(&mut self, error: &Operand) -> Result<(), String> {
        let (value, kind) = self.operand(error)?;
        let block = match kind {
            Kind::Struct(id) | Kind::Enum(id) => {
                let block = self.new_block(id)?;
                let fields = self.offset(&block, crate::lists::FIELDS_OFFSET);
                self.store_field(&kind, &value, &fields, "");
                self.retain_inline(&kind, &fields);
                block
            }
            Kind::Boxed(_) => {
                self.retain(&value);
                value
            }
            other => {
                return Err(format!("a task fails with a {other:?}, which this backend doesn't take yet"));
            }
        };
        self.declare("declare void @fennec_task_failed(i64) nounwind");
        let number = self.temp();
        self.line(&format!("{number} = ptrtoint ptr {block} to i64"));
        self.line(&format!("call void @fennec_task_failed(i64 {number})"));
        Ok(())
    }

    /// A failure a task handed over, now this thread's own, as the local `dest`: read out of its
    /// block when it's held in place, the block then let go of.
    fn take_handed_failure(&mut self, dest: LocalId, block: &str) -> Result<(), String> {
        let kind = self.kind_of(dest)?.clone();
        match kind {
            Kind::Struct(_) | Kind::Enum(_) => {
                let fields = self.offset(block, crate::lists::FIELDS_OFFSET);
                self.read_into(dest, &kind, &fields, "")?;
                self.release(block);
                Ok(())
            }
            Kind::Boxed(_) => self.assign(dest, block),
            other => Err(format!("a task fails with a {other:?}, which this backend doesn't take yet")),
        }
    }

    /// `parallel.for(list, range)`: each range of the list's positions, as the runtime splits
    /// them.
    fn parallel_for(&mut self, list: &Operand, range: &Operand) -> Result<(), String> {
        let call = format!("fennec_parallel_for{}", self.waiting(range));
        let (list, env) = self.list_and_env(list, range)?;
        let shim = self.shim(Shape::Each);
        self.declare(&format!("declare i64 @{call}(ptr, ptr, i64) nounwind"));
        let done = self.temp();
        self.line(&format!("{done} = call i64 @{call}(ptr {list}, ptr {shim}, i64 {env})"));
        self.leave_if_failed();
        Ok(())
    }

    /// `dest = parallel.map(list, range)`: a new list, each range putting its items' results
    /// in it.
    fn parallel_map(&mut self, dest: LocalId, list: &Operand, range: &Operand) -> Result<(), String> {
        let item = list_item(&self.kind_of(dest)?.clone())?;
        let call = format!("fennec_parallel_map{}", self.waiting(range));
        let (list, env) = self.list_and_env(list, range)?;
        let symbol = element_symbol(self.module.layouts, item);
        self.module.shared.elements.insert(symbol.clone(), item);
        self.declare(&format!("@\"{symbol}\" = external dso_local global i64"));
        // A range whose items can fail gives back how it ended.
        let (_, kind) = self.operand(range)?;
        let fails = matches!(kind, Kind::Closure(_, returns) if *returns == Kind::Int);
        let shim = self.shim(if fails { Shape::IntoFailing } else { Shape::Into });
        self.declare(&format!("declare ptr @{call}(ptr, ptr, i64, i64) nounwind"));
        let (element, made) = (self.temp(), self.temp());
        self.line(&format!("{element} = load i64, ptr @\"{symbol}\", !invariant.load !1"));
        self.line(&format!("{made} = call ptr @{call}(ptr {list}, ptr {shim}, i64 {env}, i64 {element})"));
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
        let call = format!("fennec_parallel_reduce{}", self.waiting(range));
        let (held, env) = self.list_and_env(list, range)?;
        let shim = self.shim(Shape::Partial { float });
        let combine = self.combine(float, span);
        self.declare(&format!("declare i64 @{call}(ptr, ptr, ptr, i64, ptr) nounwind"));
        let (slot, done, length, empty) = (self.temp(), self.temp(), self.list_length(&held), self.temp());
        self.line(&format!("{slot} = alloca i64"));
        self.line(&format!("store i64 0, ptr {slot}"));
        self.line(&format!(
            "{done} = call i64 @{call}(ptr {held}, ptr {shim}, ptr {combine}, i64 {env}, ptr {slot})"
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
        // Where the sum is, worked out in the combine itself from the function's start, a
        // constant, since a combine runs on any thread.
        let label = self.label("sum");
        let (where_, args) = self.span_values("span", span);
        let name = format!("@\"fen_parallel_add_int.{}.{label}\"", self.function.name);
        self.declare("declare { i64, i1 } @llvm.sadd.with.overflow.i64(i64, i64)");
        self.declare("declare void @fennec_fail(i64, i64, i64) #1");
        let mut text =
            format!("define internal i64 {name}(i64 %env, i64 %a, i64 %b, ptr %out) #0 {{\nentry:\n");
        let _ = write!(
            text,
            "  %pair = call {{ i64, i1 }} @llvm.sadd.with.overflow.i64(i64 %a, i64 %b)\n  \
             %sum = extractvalue {{ i64, i1 }} %pair, 0\n  %bad = extractvalue {{ i64, i1 }} %pair, 1\n  \
             br i1 %bad, label %overflow, label %fine\nfine:\n  store i64 %sum, ptr %out\n  ret i64 0\n\
             overflow:\n{where_}  call void @fennec_fail(i64 1, {args})\n  ret i64 1\n}}\n\n"
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
        (list, at, new): (&Operand, &Operand, &Operand),
    ) -> Result<(), String> {
        let (held, kind) = self.operand(list)?;
        let item = self.item_kind(list_item(&kind)?)?;
        let (place, _) = self.operand(at)?;
        let memory = item.memory(self.module.layouts);
        let slot = self.item_address(&held, &memory, &place);
        // What was there is let go only once the new item is in, which may be the same item,
        // held by nothing else while it's put back.
        let mut old = None;
        let mut old_inline = None;
        if back {
            if self.holds_inline(&item) {
                let (place, held) = (self.place_for(&item), self.temp());
                self.line(&format!("{held} = load {memory}, ptr {slot}"));
                self.line(&format!("store {memory} {held}, ptr {place}"));
                old_inline = Some(place);
            } else if item.counted() {
                old = Some(self.load_field(&item, &slot, ITEM_ACCESS));
            }
        }
        if item.in_memory() {
            // An inline struct's blocks get an owner in the list, or move there from a local
            // read for the last time, as any write into a list's place does.
            self.fill(&item, new, &slot, ITEM_ACCESS)?;
        } else {
            let (value, value_kind) = self.operand(new)?;
            self.put_item(&item, &value_kind, &value, &slot);
        }
        if let Some(block) = old {
            self.release_kind(&block, &item);
        }
        if let Some(place) = old_inline {
            self.release_inline(&item, &place);
        }
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
        let options = fennec_ir::Options { outline_parallel: true, ..fennec_ir::Options::default() };
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

    /// One function's text, lowered with `parallel` lifted.
    fn text_of(source: &str, name: &str) -> Result<String, String> {
        let mut module = fennec_syntax::parse(source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let options = fennec_ir::Options { outline_parallel: true, ..fennec_ir::Options::default() };
        let (units, _, _) = crate::emit::emit(&fennec_ir::lower_with(&module, &checked, options))?;
        units
            .into_iter()
            .find(|unit| unit.function == name)
            .map(|unit| unit.text)
            .ok_or("no such function".to_owned())
    }

    #[test]
    fn only_what_a_range_can_reach_pays_for_threads() -> Result<(), String> {
        let source = "\
def plain(n: int, ref held: list[str]) -> int:
    held.append(str(n))
    if n > 0:
        return plain(n - 1, ref held)
    n ** 2

def in_a_range(n: int) -> int:
    if n > 0:
        return in_a_range(n - 1)
    n ** 2

def main(stdout: Stdout):
    mut held = [\"a\"]
    stdout.print(plain(3, ref held), sum(in_a_range(n) for n in parallel([1, 2])))
";
        let plain = text_of(source, "plain")?;
        assert!(plain.contains("@fennec_stack_limit_main"), "{plain}");
        assert!(!plain.contains("@fennec_stack_limit,"), "{plain}");
        assert!(!plain.contains("call void @fen_retain"), "{plain}");
        assert!(plain.contains("load ptr, ptr @fennec_failed_main"), "{plain}");
        assert!(!plain.contains("load ptr, ptr @fennec_failed,"), "{plain}");
        let reached = text_of(source, "in_a_range")?;
        assert!(reached.contains("load i64, ptr @fennec_stack_limit,"), "{reached}");
        assert!(!reached.contains("@fennec_stack_limit_main"), "{reached}");
        assert!(reached.contains("load ptr, ptr @fennec_failed,"), "{reached}");
        assert!(!reached.contains("@fennec_failed_main"), "{reached}");
        Ok(())
    }

    /// A `parallel` call whose body may wait, here by sleeping, directly or in what it calls,
    /// goes to the runtime's `_waiting` entry points, so the scheduler can park a waiting range
    /// rather than its thread. Bodies that only compute go to the plain ones.
    #[test]
    fn parallel_bodies_that_may_wait_say_so() -> Result<(), String> {
        let computes = "\
def main(stdout: Stdout):
    mut xs = [1, 2, 3]
    for ref x in parallel(xs):
        x += 1
    doubled = [x * 2 for x in parallel(xs)]
    stdout.print(doubled, sum(x for x in parallel(xs)))
";
        let text = texts(computes)?;
        for call in ["fennec_parallel_for", "fennec_parallel_map", "fennec_parallel_reduce"] {
            assert!(text.contains(&format!("@{call}(ptr")), "{call}: {text}");
            assert!(!text.contains(&format!("@{call}_waiting")), "{call}: {text}");
        }
        let waits = "\
from std.time import Clock

def rest(clock: Clock, x: int) -> int:
    clock.sleep(1ms)
    x

def main(stdout: Stdout, clock: Clock):
    mut xs = [1, 2, 3]
    for ref x in parallel(xs):
        clock.sleep(1ms)
        x += 1
    rested = [rest(clock, x) for x in parallel(xs)]
    stdout.print(rested, sum(rest(clock, x) for x in parallel(xs)))
";
        let text = texts(waits)?;
        for call in ["fennec_parallel_for", "fennec_parallel_map", "fennec_parallel_reduce"] {
            assert!(text.contains(&format!("@{call}_waiting(ptr")), "{call}: {text}");
            assert!(!text.contains(&format!("@{call}(ptr")), "{call}: {text}");
        }
        // A body reading a connection waits, and one doing arithmetic beside it doesn't.
        let reads = "\
from std.net import Connection, NetError

def heard(conn: Connection, x: int) -> int:
    got = conn.read(1) else:
        return x
    got.len() + x

def main(stdout: Stdout, net: Net) fails NetError:
    xs = [1, 2, 3]
    with net.connect(\"localhost\", 9000)? as conn:
        stdout.print([heard(conn, x) for x in parallel(xs)])
    stdout.print([x * 2 for x in parallel(xs)])
";
        let text = texts(reads)?;
        assert!(text.contains("@fennec_parallel_map_waiting(ptr"), "{text}");
        assert!(text.contains("@fennec_parallel_map(ptr"), "{text}");
        Ok(())
    }

    #[test]
    fn a_for_mut_over_parallel_makes_the_list_its_own_and_puts_items_back_in_place() -> Result<(), String> {
        let source = "\
def grow(ref xs: list[int], by: int):
    for ref x in parallel(xs):
        x += by

def main(stdout: Stdout):
    mut xs = [1, 2]
    grow(ref xs, 3)
    stdout.print(xs)
";
        let text = texts(source)?;
        assert!(text.contains("@fennec_parallel_for(ptr"), "{text}");
        // The range writes in place: no copy of the list it holds, which would lose the change.
        assert!(!text.contains("fennec_list_copy"), "{text}");
        Ok(())
    }
}
