//! The evaluator: walks the syntax tree, one statement and one expression at a time.
//!
//! Each call has a frame of its own names. A name holds its own value, or, for a `ref` parameter,
//! a `for ref` loop's name or a local `ref`, a place: a name in an older frame and the fields and
//! positions under it. Reading a place copies what's there, and writing one writes there.

mod actors;
pub(crate) use actors::DEADLOCK;
mod builtins;
mod calls;
pub(crate) mod disk;
mod expr;
mod files;
mod json;
mod library;
mod locks;
mod methods;
mod own_index;
mod packing;
mod parallel;
mod paths;
pub(crate) mod saving;
mod show;
mod sources;
mod sqlite;
pub(crate) mod stream;
mod tables;
mod tasks;
mod typed;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fennec_syntax::{BinOp, Expr, ExprKind, Span, Stmt, StmtKind};

use crate::load::Program;
use crate::value::{Cap, Held, Reach, Text, Value};

/// How many expressions and statements a run may take, across every thread, before it's stopped
/// as one the oracle can't finish.
pub(crate) const MOST_STEPS: u64 = 40_000_000;

/// How deep calls may nest.
const MOST_DEPTH: usize = 1_500;

/// How deep a value's parts may nest where they're gone through one level inside another, as
/// printing one, comparing two or saving one does: deeper ones are refused there, before such a
/// walk could run out of its thread's stack. `fennec-fuzz` writes trees 200,000 deep, which is
/// 400,000 levels, each node's list of children one and the node the next. Copying a value and
/// letting go of one take a little stack however deep it is (`value/held.rs`), so those are never
/// refused.
const MOST_NESTING: usize = 500_000;

/// The stack each level of such a walk is allowed: printing a value takes about 1,000 bytes a
/// level and comparing two about 450, measured, so this is four times the most. A thread may go as
/// deep as its stack allows at this, up to `MOST_NESTING`: the run's own thread, with
/// `STACK_BYTES`, 2 GB, the whole of it, and a task's or a generator's, with 64 MB, 16,384
/// levels.
const STACK_PER_LEVEL: usize = 4_096;

thread_local! {
    /// How many levels into a value this thread's walk through one is.
    static WALKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How deep this thread's walks may go, for the stack it was given: a thread whose stack the
    /// oracle didn't choose goes as deep as the least stack a thread has, 2 MB.
    static MOST_WALKED: std::cell::Cell<usize> = const { std::cell::Cell::new((2 << 20) / STACK_PER_LEVEL) };
}

/// Says how big this thread's stack is, which sets how deep its walks through a value may go.
pub(crate) fn walks_within(stack_bytes: usize) {
    MOST_WALKED.with(|most| most.set((stack_bytes / STACK_PER_LEVEL).min(MOST_NESTING)));
}

/// A level deeper into a value a walk is going through, for as long as it's held.
pub(crate) struct Deeper;

impl Drop for Deeper {
    fn drop(&mut self) {
        WALKED.with(|walked| walked.set(walked.get().saturating_sub(1)));
    }
}

/// Goes a level deeper into a value a walk is going through, refusing one nested past what the
/// thread's stack allows.
pub(crate) fn deeper() -> Run<Deeper> {
    let most = MOST_WALKED.with(std::cell::Cell::get);
    WALKED.with(|walked| {
        let depth = walked.get() + 1;
        if depth > most {
            return unsupported("ran too long: a value nested too deeply");
        }
        walked.set(depth);
        // Each part a walk goes into is counted against the run's steps, as copying part by part
        // was: comparing two lists counts their items, and a dict's lookup comparing its keys one
        // at a time, as main's oracle did uncounted, counts none.
        if depth > 1 {
            crate::value::spend(1);
        }
        Ok(Deeper)
    })
}

/// The most bytes a program may print.
const MOST_OUTPUT: usize = 64 << 20;

/// The most items a list, string or other collection may hold.
pub(crate) const MOST_ITEMS: usize = 20_000_000;

/// How a statement or expression ended, other than with a value.
#[derive(Debug)]
pub(crate) enum Flow {
    Return(Value),
    Break,
    Continue,
    /// A failure passing up, and where it was made.
    Fail(Value, Span),
    /// A bug that stops the program, with what the program says about it.
    Crash(String, Span),
    /// Something the oracle doesn't model.
    Unsupported(String),
    /// The generator this runs in was let go of before it finished.
    Abandoned,
}

pub(crate) type Run<T> = Result<T, Flow>;

pub(crate) fn unsupported<T>(what: impl Into<String>) -> Run<T> {
    Err(Flow::Unsupported(what.into()))
}

pub(crate) fn crash<T>(message: impl Into<String>, span: Span) -> Run<T> {
    Err(Flow::Crash(message.into(), span))
}

/// What every thread of one run shares.
pub(crate) struct Shared {
    pub(crate) program: &'static Program,
    /// How many steps the run may take.
    most_steps: u64,
    pub(crate) out: Mutex<Vec<u8>>,
    steps: AtomicU64,
    /// Generator threads still running.
    pub(crate) threads: AtomicUsize,
    /// Something a generator's thread found it couldn't model, after nobody was asking it for
    /// anything.
    pub(crate) refused: Mutex<Option<String>>,
    /// The tag the next `Table` made gets.
    next_tag: AtomicU64,
    /// The file system an `Fs` reaches.
    pub(crate) disk: Arc<Mutex<disk::Disk>>,
    /// The SQLite databases the program has open, through `std.sqlite`.
    pub(crate) databases: Mutex<sqlite::Databases>,
    /// The number the next `shared` value made gets.
    next_cell: AtomicU64,
    /// The `shared` values sent on a channel, which no `parallel` round has to itself once they
    /// have been: another round may have taken them.
    pub(crate) sent_cells: Mutex<std::collections::BTreeSet<u64>>,
    /// What runs side by side now, `parallel` calls' rounds and `with launch` blocks and their
    /// tasks, the innermost last. Only one thread runs at a time, so they're what whatever runs
    /// now is inside.
    pub(crate) scopes: Mutex<Vec<parallel::Scope>>,
    /// How many rounds and tasks have started, so a generator made before the one running now is
    /// told apart from one made in it.
    pub(crate) rounds_started: AtomicU64,
    /// When the run started, which a `Clock`'s `now` counts from, and how far its `sleep`s have
    /// moved the clock on, in nanoseconds, without waiting.
    pub(crate) started: std::time::Instant,
    pub(crate) slept: AtomicU64,
    /// Who of `main` and the tasks it launched runs, and the channels they share.
    pub(crate) sched: actors::Sched,
    /// Whether standard error is left unwritten, as it is on every run but the first of a
    /// program whose tasks' messages are tried in each order.
    pub(crate) quiet: std::sync::atomic::AtomicBool,
}

impl Shared {
    pub(crate) fn new(program: &'static Program, most_steps: u64, workdir: &disk::Workdir) -> Shared {
        Shared {
            program,
            most_steps,
            out: Mutex::new(Vec::new()),
            steps: AtomicU64::new(0),
            threads: AtomicUsize::new(0),
            refused: Mutex::new(None),
            next_tag: AtomicU64::new(1),
            disk: Arc::new(Mutex::new(disk::Disk::new(workdir))),
            databases: Mutex::new(sqlite::Databases::default()),
            next_cell: AtomicU64::new(1),
            sent_cells: Mutex::new(std::collections::BTreeSet::new()),
            scopes: Mutex::new(Vec::new()),
            rounds_started: AtomicU64::new(0),
            started: std::time::Instant::now(),
            slept: AtomicU64::new(0),
            sched: actors::Sched::default(),
            quiet: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// The number the next `shared` value made gets: every one made so far has a lower one.
    pub(crate) fn cells_made(&self) -> u64 {
        self.next_cell.load(Ordering::Relaxed)
    }

    /// A number no `shared` value of this run has had.
    pub(crate) fn new_cell(&self) -> u64 {
        self.next_cell.fetch_add(1, Ordering::Relaxed)
    }

    /// The earliest a generator read now may have been made, inside what runs side by side now,
    /// or `None` outside all of it.
    pub(crate) fn reads_since(&self) -> Option<u64> {
        self.scopes.lock().ok().and_then(|scopes| parallel::reads_since(&scopes))
    }

    /// A tag no `Table` of this run has had.
    pub(crate) fn new_tag(&self) -> u64 {
        self.next_tag.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn refuse(&self, why: String) {
        if let Ok(mut refused) = self.refused.lock() {
            refused.get_or_insert(why);
        }
    }
}

/// A place a value is in: a name in a frame, then fields and positions under it.
#[derive(Clone, Debug)]
pub(crate) struct Place {
    pub(crate) frame: usize,
    pub(crate) name: String,
    pub(crate) path: Vec<Step>,
    /// How many of the path's steps a `ref` lent it through, a field or an item handed to a
    /// call or a `for ref` loop's turn on its own. The structs those steps pass through don't see
    /// a write here: their `where` rules run when what was lent comes back.
    pub(crate) lent: usize,
}

#[derive(Clone, Debug)]
pub(crate) enum Step {
    Field(String),
    /// A position in a list or tuple, or a key in a dict.
    At(Value),
}

impl Place {
    pub(crate) fn then(&self, step: Step) -> Place {
        let mut path = self.path.clone();
        path.push(step);
        Place { frame: self.frame, name: self.name.clone(), path, lent: self.lent }
    }

    /// The place a name whose `ref` lends it stands for: every struct along the way is the
    /// lender's, so a write through the name checks none of their rules.
    pub(crate) fn lent_alone(mut self) -> Place {
        self.lent = self.path.len();
        self
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Binding {
    Value(Value),
    Ref(Place),
}

/// One call's names.
#[derive(Clone)]
pub(crate) struct Frame {
    pub(crate) id: u64,
    pub(crate) vars: Vec<(String, Binding)>,
    /// The frame of the call a nested function was written in, whose names it reads.
    pub(crate) parent: Option<u64>,
    /// The type whose method this call runs, which `Self` names.
    pub(crate) owner: Option<String>,
    /// What the type parameters of the function it runs stand for in this call.
    pub(crate) types: typed::Bound,
    /// How many levels of optional the function it runs gives back, in this call: 2 for an
    /// `int??`, or for a `T?` with `T` an `int?`. A `None` returned from fewer is held in that many
    /// more `Some`s, as returning an `int?` holding `None` as an `int??` gives `Some(None)`.
    pub(crate) returns: usize,
}

/// A `lend ... except:` cleanup running for a block that failed: the frame it runs in, and once a
/// statement of its own has failed, whether that failure takes the block's place rather than being
/// noted on it.
pub(crate) struct Cleanup {
    pub(crate) frame: usize,
    pub(crate) replaces: Option<bool>,
}

pub(crate) struct Interp {
    pub(crate) shared: Arc<Shared>,
    pub(crate) program: &'static Program,
    pub(crate) frames: Vec<Frame>,
    /// The frame names are read and written in: the newest, except while a `with` block runs
    /// inside its source.
    pub(crate) cur: usize,
    pub(crate) next_id: u64,
    pub(crate) lends: Vec<sources::Lending>,
    pub(crate) yields: Option<stream::YieldSide>,
    depth: usize,
    constants: HashMap<String, Value>,
    generators: HashMap<usize, bool>,
    /// How many `lock` blocks this thread is inside.
    pub(crate) holding: usize,
    /// The parts read through a type's own `def []` to be changed, waiting to be written back
    /// through its `def []=` once the change is done, the newest last.
    pub(crate) write_backs: Vec<own_index::WriteBack>,
    /// How many such parts this run has read, which names each one's hidden local.
    pub(crate) parts_read: u64,
    /// The `lend ... except:` cleanup running for a block that failed, if one is.
    pub(crate) cleanup: Option<Cleanup>,
    /// What each type's own `def serialize` running for `json.dumps` has saved, the newest last.
    pub(crate) recordings: Vec<Option<json::write::Saved>>,
    /// What each type's own `def deserialize` running for `json.loads` reads, the newest last.
    pub(crate) loadings: Vec<json::read::Loading>,
    /// Which of `main`'s thread and the tasks it launched this runs as: 0 for `main`.
    pub(crate) actor: usize,
    /// The `parallel` rounds this thread runs inside, the innermost last. Each round is one of
    /// its own to the channels it uses, as it runs on a thread of its own in the compiled
    /// program, and gets the number they know it by the first time it uses one.
    pub(crate) rounds_as: Vec<Option<usize>>,
    /// The `parallel` calls whose rounds this thread runs, the innermost last, each with whether
    /// the rounds after the one running now were handed to a thread of their own.
    pub(crate) handing: Vec<parallel::Handing>,
    /// The failures `except name:` blocks running name, innermost last, with the frame each runs
    /// in: a new failure raised in one carries that one's words as a note.
    pub(crate) excepts: Vec<(usize, &'static str, Value)>,
    /// The local `Buffer` a `reserve(n)` was just called on, by its frame and name, and the room
    /// it promised: `n` more than it held. Any write anywhere forgets it, so it holds only until
    /// the buffer, or anything else, changes.
    pub(crate) reserved: Option<(usize, String, i64)>,
}

/// How `main` ended.
pub(crate) enum MainEnded {
    Returned(Value),
    Failed(Value, Span),
    Crashed(String, Span),
    Unsupported(String),
}

impl Interp {
    pub(crate) fn new(shared: Arc<Shared>, yields: Option<stream::YieldSide>) -> Interp {
        let program = shared.program;
        Interp {
            shared,
            program,
            frames: Vec::new(),
            cur: 0,
            next_id: 1,
            lends: Vec::new(),
            yields,
            depth: 0,
            constants: HashMap::new(),
            generators: HashMap::new(),
            holding: 0,
            write_backs: Vec::new(),
            parts_read: 0,
            cleanup: None,
            recordings: Vec::new(),
            loadings: Vec::new(),
            actor: 0,
            rounds_as: Vec::new(),
            handing: Vec::new(),
            excepts: Vec::new(),
            reserved: None,
        }
    }

    /// Runs `main`, with standard output, empty arguments and input, and the file system.
    pub(crate) fn run_main(&mut self) -> MainEnded {
        let main = self.program.main;
        let mut given = Vec::new();
        for param in &main.params {
            let cap = match param.ty.as_ref().map(|ty| ty.name.as_str()) {
                Some("Stdout") => Cap::Stdout,
                Some("Args") => Cap::Args,
                Some("Stdin") => Cap::Stdin,
                Some("Fs") => Cap::Fs(Reach { folders: None, read_only: false }),
                Some("Stderr") => Cap::Stderr,
                Some("Signals") => Cap::Signals,
                Some("Clock") => Cap::Clock,
                Some("Entropy") => Cap::Entropy,
                Some("Net") => Cap::Net,
                // `std.env`'s struct, run from its own source, whose `get` is modeled here.
                Some("Env") => {
                    let env = crate::value::Instance {
                        ty: "Env".to_owned(),
                        case: None,
                        fields: vec![("_given".to_owned(), Value::Int(0))],
                        notes: Vec::new(),
                    };
                    given.push(calls::ArgVal::Value(None, Value::Instance(Held::new(env))));
                    continue;
                }
                _ => return MainEnded::Unsupported("a capability main takes".to_owned()),
            };
            given.push(calls::ArgVal::Value(None, Value::Cap(cap)));
        }
        let ended = self.call_def(main, None, None, given, main.span);
        // A task still waiting for its turn when `main` is done runs nothing more.
        self.give_up();
        match ended {
            Ok(value) => MainEnded::Returned(value),
            Err(Flow::Fail(error, span)) => MainEnded::Failed(error, span),
            Err(Flow::Crash(message, span)) => MainEnded::Crashed(message, span),
            Err(Flow::Unsupported(why)) => MainEnded::Unsupported(why),
            Err(Flow::Abandoned) => MainEnded::Unsupported("a generator was let go of".to_owned()),
            Err(Flow::Return(_) | Flow::Break | Flow::Continue) => {
                MainEnded::Unsupported("a `break`, `continue` or `return` left its place".to_owned())
            }
        }
    }

    /// Counts a step, and the work done since the last, stopping a run that's gone on too long.
    pub(crate) fn tick(&self) -> Run<()> {
        let steps = 1 + crate::value::spent();
        if self.shared.steps.fetch_add(steps, Ordering::Relaxed) > self.shared.most_steps {
            return unsupported("ran too long");
        }
        Ok(())
    }

    /// Counts a copy of a value against the run's steps. A list, dict or struct shares its parts
    /// with its copies until one is changed (`value/held.rs`), so its copy costs nothing more than
    /// the step that made it, and what's cloned when it's changed is counted then. A `Table` copies
    /// its slots.
    pub(crate) fn charge(&self, value: &Value) -> Run<()> {
        let Value::Table(table) = value else { return Ok(()) };
        let count = u64::try_from(table.slots.len()).unwrap_or(u64::MAX);
        if self.shared.steps.fetch_add(count, Ordering::Relaxed) > self.shared.most_steps {
            return unsupported("ran too long");
        }
        Ok(())
    }

    /// Writes text to standard output.
    pub(crate) fn print(&self, text: &str) -> Run<()> {
        self.reach_out(parallel::Outside::Printing)?;
        self.outside_in_order(parallel::Outside::Printing)?;
        let Ok(mut out) = self.shared.out.lock() else { return unsupported("standard output was lost") };
        if out.len() + text.len() > MOST_OUTPUT {
            return unsupported("ran too long: it printed too much");
        }
        out.extend_from_slice(text.as_bytes());
        Ok(())
    }

    // Frames.

    pub(crate) fn push_frame(&mut self, vars: Vec<(String, Binding)>, parent: Option<u64>) -> Run<usize> {
        self.depth += 1;
        if self.depth > MOST_DEPTH {
            self.depth -= 1;
            return unsupported("ran too long: calls nested too deeply");
        }
        let id = self.next_id;
        self.next_id += 1;
        let owner = parent
            .and_then(|parent| self.frames.iter().rfind(|frame| frame.id == parent))
            .and_then(|frame| frame.owner.clone());
        self.frames.push(Frame { id, vars, parent, owner, types: typed::Bound::new(), returns: 0 });
        let previous = self.cur;
        self.cur = self.frames.len() - 1;
        Ok(previous)
    }

    /// What letting go of a generator partway came to: a stop with compiled code's words where its
    /// cleanup failed.
    pub(crate) fn came_of_letting_go(&mut self, came: Option<stream::LetGo>, span: Span) -> Run<()> {
        match came {
            None | Some(stream::LetGo::Ended) => Ok(()),
            Some(stream::LetGo::Failed(error)) => {
                let shown = self.display(&error)?;
                crash(
                    format!("a generator's cleanup failed as it was let go by its last owner: {shown}"),
                    span,
                )
            }
            Some(stream::LetGo::Refused(why)) => unsupported(why),
        }
    }

    pub(crate) fn pop_frame(&mut self, previous: usize) {
        self.frames.pop();
        self.depth = self.depth.saturating_sub(1);
        self.cur = previous;
    }

    fn frame(&self) -> Run<&Frame> {
        self.frames.get(self.cur).map_or_else(|| unsupported("no frame"), Ok)
    }

    fn frame_mut(&mut self) -> Run<&mut Frame> {
        let at = self.cur;
        self.frames.get_mut(at).map_or_else(|| unsupported("no frame"), Ok)
    }

    pub(crate) fn frame_id(&self) -> u64 {
        self.frames.get(self.cur).map_or(0, |frame| frame.id)
    }

    /// The type `Self` names here.
    pub(crate) fn self_type(&self) -> Option<String> {
        self.frames.get(self.cur).and_then(|frame| frame.owner.clone())
    }

    /// The type a name stands for: itself, or the type `Self` names.
    pub(crate) fn type_named<'n>(&self, name: &'n str) -> std::borrow::Cow<'n, str> {
        match (name, self.self_type()) {
            ("Self", Some(owner)) => std::borrow::Cow::Owned(owner),
            _ => std::borrow::Cow::Borrowed(name),
        }
    }

    /// Where a name is bound: in this frame, or in the frames a nested function reads.
    fn binding_of(&self, name: &str) -> Option<(usize, &Binding)> {
        let mut at = self.cur;
        loop {
            let frame = self.frames.get(at)?;
            if let Some((_, binding)) = frame.vars.iter().rev().find(|(bound, _)| bound == name) {
                return Some((at, binding));
            }
            let parent = frame.parent?;
            at = self.frames.iter().rposition(|frame| frame.id == parent)?;
        }
    }

    /// Whether a name is bound to a value or a place here.
    pub(crate) fn is_bound(&self, name: &str) -> bool {
        self.binding_of(name).is_some()
    }

    /// The value a name holds, if it's bound.
    pub(crate) fn lookup(&self, name: &str, span: Span) -> Run<Option<Value>> {
        match self.binding_of(name) {
            Some((_, Binding::Value(value))) => {
                self.charge(value)?;
                Ok(Some(value.clone()))
            }
            Some((_, Binding::Ref(place))) => {
                let place = place.clone();
                self.read_place(&place, span).map(Some)
            }
            None => Ok(None),
        }
    }

    /// Gives a new name a value, hiding any it had.
    pub(crate) fn bind(&mut self, name: &str, value: Value) -> Run<()> {
        self.reserved = None;
        if name != "_" {
            self.frame_mut()?.vars.push((name.to_owned(), Binding::Value(value)));
        }
        Ok(())
    }

    pub(crate) fn bind_ref(&mut self, name: &str, place: Place) -> Run<()> {
        self.reserved = None;
        if name != "_" {
            self.frame_mut()?.vars.push((name.to_owned(), Binding::Ref(place)));
        }
        Ok(())
    }

    /// Sets a name: a value it holds is replaced, a place it stands for is written, and a name
    /// it doesn't have yet is made.
    pub(crate) fn set_name(&mut self, name: &str, value: Value, span: Span) -> Run<()> {
        self.reserved = None;
        if name == "_" {
            return Ok(());
        }
        let frame = self.frame_mut()?;
        match frame.vars.iter_mut().rev().find(|(bound, _)| bound == name) {
            Some((_, Binding::Value(held))) => {
                let before = std::mem::replace(held, value);
                let came = stream::letting_go(|| drop(before));
                self.came_of_letting_go(came, span)
            }
            Some((_, Binding::Ref(place))) => {
                let place = place.clone();
                self.write_place(&place, value, span)
            }
            None => self.bind(name, value),
        }
    }

    // Places.

    /// The place an expression names, working out its positions now, or `None` for one that's
    /// not a place.
    pub(crate) fn place_of(&mut self, expr: &'static Expr) -> Run<Option<Place>> {
        match &expr.kind {
            ExprKind::Name(name) => Ok(match self.binding_of(name) {
                Some((_, Binding::Ref(place))) => Some(place.clone()),
                Some((frame, Binding::Value(_))) => {
                    Some(Place { frame, name: name.clone(), path: Vec::new(), lent: 0 })
                }
                None => None,
            }),
            ExprKind::Attr { object, name, optional: false } => {
                Ok(self.place_of(object)?.map(|place| place.then(Step::Field(name.clone()))))
            }
            ExprKind::Index { object, index } => {
                let Some(place) = self.place_of(object)? else { return Ok(None) };
                let at = self.eval(index)?;
                self.indexed_place(place, at, false, expr.span)
            }
            _ => Ok(None),
        }
    }

    /// The value at the root of a place.
    fn root(&self, place: &Place) -> Run<&Value> {
        let frame =
            self.frames.get(place.frame).map_or_else(|| unsupported("a place outlived its frame"), Ok)?;
        match frame.vars.iter().rev().find(|(bound, _)| *bound == place.name) {
            Some((_, Binding::Value(value))) => Ok(value),
            _ => unsupported("a place whose name is gone"),
        }
    }

    fn root_mut(&mut self, place: &Place) -> Run<&mut Value> {
        let frame =
            self.frames.get_mut(place.frame).map_or_else(|| unsupported("a place outlived its frame"), Ok)?;
        match frame.vars.iter_mut().rev().find(|(bound, _)| *bound == place.name) {
            Some((_, Binding::Value(value))) => Ok(value),
            _ => unsupported("a place whose name is gone"),
        }
    }

    pub(crate) fn read_place(&self, place: &Place, span: Span) -> Run<Value> {
        let mut value = self.root(place)?;
        let mut char_at: Option<Value> = None;
        for step in &place.path {
            if char_at.is_some() {
                return unsupported("a place inside a character");
            }
            match step_into(value, step, span)? {
                Reached::Inside(inner) => value = inner,
                Reached::Made(made) => char_at = Some(made),
            }
        }
        if char_at.is_none() {
            self.charge(value)?;
        }
        Ok(char_at.unwrap_or_else(|| value.clone()))
    }

    /// Runs `change` on the value at a place. With `adding`, a key the last step names that a
    /// dict doesn't have yet is added, holding nothing until `change` gives it a value.
    pub(crate) fn change_place<R>(
        &mut self,
        place: &Place,
        span: Span,
        adding: bool,
        change: impl FnOnce(&mut Value) -> Run<R>,
    ) -> Run<R> {
        self.reserved = None;
        let last = place.path.len().saturating_sub(1);
        let mut value = self.root_mut(place)?;
        for (at, step) in place.path.iter().enumerate() {
            value = step_into_mut(value, step, span, adding && at == last)?;
        }
        let changed = change(value)?;
        self.rules_along(place, span)?;
        Ok(changed)
    }

    pub(crate) fn write_place(&mut self, place: &Place, new: Value, span: Span) -> Run<()> {
        self.change_place(place, span, true, |value| {
            *value = new;
            Ok(())
        })
    }

    /// Checks the `where` rules of every struct along a place just written, but those of the
    /// structs it was lent out of: a field or an item lent alone by `ref` is a plain value to
    /// whoever borrowed it, and its struct's rules run when it comes back.
    pub(crate) fn rules_along(&mut self, place: &Place, span: Span) -> Run<()> {
        if !self.program.has_rules {
            return Ok(());
        }
        let mut ruled = Vec::new();
        let mut value = self.root(place)?;
        for (depth, step) in std::iter::once(None).chain(place.path.iter().map(Some)).enumerate() {
            if let Some(step) = step {
                match step_into(value, step, span)? {
                    Reached::Inside(inner) => value = inner,
                    Reached::Made(_) => break,
                }
            }
            if depth < place.lent {
                continue;
            }
            if let Value::Instance(instance) = value
                && self.program.types.get(instance.ty.as_str()).is_some_and(|declared| declared.ruled)
            {
                ruled.push((**instance).clone());
            }
        }
        for instance in ruled {
            if let Some(broken) = self.broken_rule(&instance)? {
                return crash(format!("this breaks the `where` rule on `{}.{broken}`", instance.ty), span);
            }
        }
        Ok(())
    }

    /// Looks at the value at a place without copying it.
    pub(crate) fn peek_place<R>(&self, place: &Place, span: Span, look: impl FnOnce(&Value) -> R) -> Run<R> {
        let mut value = self.root(place)?;
        for step in &place.path {
            match step_into(value, step, span)? {
                Reached::Inside(inner) => value = inner,
                Reached::Made(made) => return Ok(look(&made)),
            }
        }
        Ok(look(value))
    }

    // Statements.

    /// Where the current frame's names end, so a block's own names can go when it ends.
    pub(crate) fn mark(&self) -> (usize, usize) {
        (self.cur, self.frames.get(self.cur).map_or(0, |frame| frame.vars.len()))
    }

    /// Lets go of the names made since `mark`: a block's names last until it ends.
    pub(crate) fn restore(&mut self, (frame, len): (usize, usize)) {
        if let Some(frame) = self.frames.get_mut(frame) {
            frame.vars.truncate(len);
        }
    }

    /// Runs a block, whose new names last until it ends.
    pub(crate) fn block(&mut self, body: &'static [Stmt]) -> Run<()> {
        let mark = self.mark();
        let ran = self.stmts(body);
        self.restore(mark);
        ran
    }

    pub(crate) fn stmts(&mut self, body: &'static [Stmt]) -> Run<()> {
        for stmt in body {
            self.stmt(stmt)?;
        }
        Ok(())
    }

    /// Runs a block whose last line is its value, as a `match` case's or an `except` block's are.
    pub(crate) fn block_value(&mut self, body: &'static [Stmt]) -> Run<Value> {
        let mark = self.mark();
        let value = self.stmts_value(body);
        self.restore(mark);
        value
    }

    pub(crate) fn stmts_value(&mut self, body: &'static [Stmt]) -> Run<Value> {
        let Some((last, rest)) = body.split_last() else { return Ok(Value::Unit) };
        self.stmts(rest)?;
        match &last.kind {
            StmtKind::Expr(expr) => self.eval(expr),
            StmtKind::With { value, names, body, lock: true } => self.lock(value, names, body, true),
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    if self.truth(cond)? {
                        return self.block_value(body);
                    }
                }
                match otherwise {
                    Some(body) => self.block_value(body),
                    None => Ok(Value::Unit),
                }
            }
            _ => {
                self.stmt(last)?;
                Ok(Value::Unit)
            }
        }
    }

    pub(crate) fn truth(&mut self, cond: &'static Expr) -> Run<bool> {
        match self.eval(cond)? {
            Value::Bool(truth) => Ok(truth),
            other => unsupported(format!("a {} used as a condition", other.kind())),
        }
    }

    /// Runs a statement, and in a cleanup running for a failed block, notes how the cleanup failed
    /// when it's this statement that failed first (`cleanup_failed_at`).
    fn stmt(&mut self, stmt: &'static Stmt) -> Run<()> {
        let ran = self.stmt_ran(stmt);
        if matches!(ran, Err(Flow::Fail(..))) {
            self.cleanup_failed_at(stmt);
        }
        ran
    }

    /// The first statement of a cleanup's own that fails decides how: a `fail`, or a line whose
    /// whole value is a `?`, as `kept = capped(code)?`, has its failure noted on the block's; any
    /// other, a `?` deeper in a line or on a `with`, has it take the block's place, as
    /// `docs/language.md` says. A statement inside one of the cleanup's blocks decides before the
    /// block does, and one in a function it calls, in a frame of its own, never does.
    fn cleanup_failed_at(&mut self, stmt: &Stmt) {
        let cur = self.cur;
        let Some(cleanup) =
            self.cleanup.as_mut().filter(|cleanup| cleanup.frame == cur && cleanup.replaces.is_none())
        else {
            return;
        };
        let noted = match &stmt.kind {
            StmtKind::Fail(_) => true,
            StmtKind::Expr(value) | StmtKind::Assign { value, .. } => {
                matches!(value.kind, ExprKind::Try { .. })
            }
            _ => false,
        };
        cleanup.replaces = Some(!noted);
    }

    /// `fail raised`, each side of a condition raised on its own, as the compiled program does.
    fn fail(&mut self, mut raised: &'static Expr, span: Span) -> Run<()> {
        while let ExprKind::Cond { then, cond, otherwise } = &raised.kind {
            raised = if self.truth(cond)? { then } else { otherwise };
        }
        let error = self.eval(raised)?;
        let error = self.with_cause_noted(raised, error)?;
        Err(Flow::Fail(error, span))
    }

    fn stmt_ran(&mut self, stmt: &'static Stmt) -> Run<()> {
        self.tick()?;
        match &stmt.kind {
            StmtKind::Expr(expr) => self.eval(expr).map(drop),
            StmtKind::Assign { .. } => self.assign_ran(stmt),
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    if self.truth(cond)? {
                        return self.block(body);
                    }
                }
                match otherwise {
                    Some(body) => self.block(body),
                    None => Ok(()),
                }
            }
            StmtKind::While { cond, body } => {
                while self.truth(cond)? {
                    match self.block(body) {
                        Ok(()) | Err(Flow::Continue) => {}
                        Err(Flow::Break) => break,
                        Err(other) => return Err(other),
                    }
                }
                Ok(())
            }
            StmtKind::For { names, is_mut, iter, body } => {
                if *is_mut {
                    self.for_ref(names, iter, body, stmt.span)
                } else {
                    self.for_each(names, iter, body, stmt.span)
                }
            }
            StmtKind::Return(value) => {
                let value = match value {
                    Some(value) => {
                        let given = self.eval(value)?;
                        self.returned(given, value.span)
                    }
                    None => Value::Unit,
                };
                Err(Flow::Return(value))
            }
            StmtKind::Fail(raised) => self.fail(raised, stmt.span),
            StmtKind::Yield(value) => {
                let value = self.eval(value)?;
                self.yield_value(value)
            }
            StmtKind::Assert(cond) => {
                if cond.asserted_failure().is_some() {
                    return unsupported("`assert ... fails`");
                }
                if self.truth(cond)? { Ok(()) } else { crash("this assertion failed", stmt.span) }
            }
            StmtKind::With { value, names, body, lock: true } => {
                self.lock(value, names, body, false).map(drop)
            }
            StmtKind::With { value, names, body, lock: false } => self.with(value, names, body),
            StmtKind::Def(def) => {
                let frame = self.frame_id();
                let func = crate::value::Func::Nested { def, frame };
                self.bind(&def.name, Value::Func(Box::new(func)))
            }
            StmtKind::Lend { value, otherwise, .. } => self.lend(value, otherwise, stmt.span),
            StmtKind::OnFailure { .. } => unsupported("a written-in failure block"),
            StmtKind::Break => Err(Flow::Break),
            StmtKind::Continue => Err(Flow::Continue),
            StmtKind::Pass => Ok(()),
        }
    }

    /// An assignment, a declaration, a `shared` one, or a local `ref`.
    fn assign_ran(&mut self, stmt: &'static Stmt) -> Run<()> {
        let StmtKind::Assign { target, op, value, ty, .. } = &stmt.kind else { return Ok(()) };
        if let Some((name, place)) = stmt.kind.local_ref() {
            let Some(place) = self.place_of(place)? else {
                return unsupported("a local `ref` to something that isn't a place");
            };
            return self.bind_ref(name, place);
        }
        if let Some(name) = shared_declared(&stmt.kind) {
            return self.declare_shared(name, value, stmt.span);
        }
        // A `T?` put in a `T??` is held in a `Some`: how many levels the place it goes in has,
        // as written or as the checker typed it.
        let wanted = match ty {
            Some(written) => self.levels_written(written),
            None => self.static_type(target.span).map_or(0, |ty| typed::levels(&ty)),
        };
        self.assign(target, *op, (value, wanted), stmt.span)
    }

    fn assign(
        &mut self,
        target: &'static Expr,
        op: Option<BinOp>,
        (value, wanted): (&'static Expr, usize),
        span: Span,
    ) -> Run<()> {
        match (&target.kind, op) {
            (ExprKind::Name(name), None) => {
                let given = self.eval(value)?;
                let given = self.put_in(given, value.span, wanted);
                self.set_name(name, given, span)
            }
            (ExprKind::Tuple(targets), None) => {
                let value = self.eval(value)?;
                self.destructure(targets, value, span)
            }
            (ExprKind::Index { object, index }, _) => self.assign_indexed(object, index, op, value, span),
            _ => {
                let mark = self.write_backs.len();
                let changed = (|| {
                    let Some(place) = self.place_for_change(target)? else {
                        return unsupported("assigning to something that isn't a place");
                    };
                    let new = if let Some(op) = op {
                        let old = self.read_place(&place, target.span)?;
                        let right = self.eval(value)?;
                        self.binary_values(op, old, right, target.span)?
                    } else {
                        let given = self.eval(value)?;
                        self.put_in(given, value.span, wanted)
                    };
                    self.write_place(&place, new, span)
                })();
                self.settled(mark, changed, span)
            }
        }
    }

    /// `object[index] = value` or `object[index] op= value`: written where it is, or through a
    /// type's own `def []=`, reading through its `def []` first for `op=`.
    fn assign_indexed(
        &mut self,
        object: &'static Expr,
        index: &'static Expr,
        op: Option<BinOp>,
        value: &'static Expr,
        span: Span,
    ) -> Run<()> {
        let mark = self.write_backs.len();
        let assigned = self.assign_indexed_at(object, index, op, value, span);
        self.settled(mark, assigned, span)
    }

    fn assign_indexed_at(
        &mut self,
        object: &'static Expr,
        index: &'static Expr,
        op: Option<BinOp>,
        value: &'static Expr,
        span: Span,
    ) -> Run<()> {
        let Some(place) = self.place_for_change(object)? else {
            return unsupported("assigning into something that isn't a place");
        };
        let own = self.peek_place(&place, span, |held| match held {
            Value::Instance(instance) => Some(instance.ty.clone()),
            _ => None,
        })?;
        let key = self.eval(index)?;
        let Some(ty) = own else {
            let place = place.then(Step::At(key));
            let new = match op {
                Some(op) => {
                    let old = self.read_place(&place, span)?;
                    let right = self.eval(value)?;
                    self.binary_values(op, old, right, span)?
                }
                None => self.eval(value)?,
            };
            return self.write_place(&place, new, span);
        };
        // Through the `ref` form of `def []`, the position it gives is written where it is.
        if matches!(key, Value::Int(_)) && self.program.method(&ty, "[]mut").is_some() {
            let Some(item) = self.indexed_place(place, key, true, span)? else {
                return unsupported(format!("a position of a {ty} its `def []` gives no place for"));
            };
            let new = match op {
                Some(op) => {
                    let old = self.read_place(&item, span)?;
                    let right = self.eval(value)?;
                    self.binary_values(op, old, right, span)?
                }
                None => self.eval(value)?,
            };
            return self.write_place(&item, new, span);
        }
        let Some(setter) = self.program.method(&ty, "[]=") else {
            return unsupported(format!("assigning into a {ty}"));
        };
        let new = match op {
            Some(op) => {
                let held = self.read_place(&place, span)?;
                let old = self.index_value(held, key.clone(), span)?;
                let right = self.eval(value)?;
                self.binary_values(op, old, right, span)?
            }
            None => self.eval(value)?,
        };
        let given = vec![calls::ArgVal::Value(None, key), calls::ArgVal::Value(None, new)];
        self.call_def(setter, None, Some(calls::Receiver::Place(place)), given, span).map(drop)
    }

    /// `a, b = pair`, a name or `_` for each part, and parts that are tuples again.
    fn destructure(&mut self, targets: &'static [Expr], value: Value, span: Span) -> Run<()> {
        let parts = match value {
            Value::Tuple(parts) | Value::List(parts) => parts,
            other => return unsupported(format!("taking a {} apart", other.kind())),
        };
        if parts.len() != targets.len() {
            return unsupported("taking apart a tuple of another size");
        }
        for (target, part) in targets.iter().zip(parts) {
            match &target.kind {
                ExprKind::Name(name) => self.set_name(name, part, span)?,
                ExprKind::Tuple(inner) => self.destructure(inner, part, span)?,
                _ => return unsupported("taking a tuple apart into something that isn't a name"),
            }
        }
        Ok(())
    }

    fn for_each(
        &mut self,
        names: &'static [String],
        iter: &'static Expr,
        body: &'static [Stmt],
        span: Span,
    ) -> Run<()> {
        if let Some(call) = self.parallel_call(iter)? {
            if parallel::returns_in(body) {
                return self.for_each(names, call.source(), body, span);
            }
            return self.for_parallel(names, call, body);
        }
        let over = self.eval(iter)?;
        // A generator made for the loop is the loop's, as one `iter` makes is, and one made right
        // inside `enumerate` or `zip` is too, and is let go when the loop is.
        let made = !Interp::is_stream(&over) || made_for_loop(iter);
        let wraps = arg_wraps(iter);
        let mut items = self.items_of(over, iter.span)?;
        let _ = span;
        let mut left = Ok(());
        loop {
            let _ = stream::failed_side();
            let item = match self.next_item(&mut items) {
                Ok(Some(item)) => item,
                Ok(None) => break,
                // One the loop owns failing partway through fails the loop, wrapped as its own
                // `? from Wrap(...)` says, where it has one, and the others it owns are let go on
                // its way out. In `zip`, that's the `? from` of the side that failed.
                Err(Flow::Fail(error, failed_at)) if made && Interp::is_stream_items(&items) => {
                    let side = stream::failed_side().unwrap_or(0);
                    let wrap = wraps.get(side).copied().flatten();
                    left = match wrap {
                        Some(_) => self.wrapped(wrap, iter.span),
                        None => Err(Flow::Fail(error, failed_at)),
                    };
                    break;
                }
                Err(other) => return Err(other),
            };
            let mark = self.mark();
            let ran = self.bind_fresh(names, item).and_then(|()| self.stmts(body));
            self.restore(mark);
            match ran {
                Ok(()) | Err(Flow::Continue) => {}
                Err(flow) => {
                    left = Err(flow);
                    break;
                }
            }
        }
        match left {
            Err(Flow::Break) if !made => Ok(()),
            left if made => self.let_go_of(&items, left, (&arg_wraps(iter), iter.span)),
            left => left,
        }
    }

    /// A failure passed up as the wrapper `wrap` makes, when there is one: `value? from Wrap(...)`
    /// passes the failure up as `Wrap(...)`, made only then.
    fn wrapped(&mut self, wrap: Option<&'static Expr>, at: Span) -> Run<()> {
        match wrap {
            Some(wrap) => {
                let wrapper = self.eval(wrap)?;
                Err(Flow::Fail(wrapper, at))
            }
            None => unsupported("a failure lost as it was wrapped"),
        }
    }

    /// Leaving a loop, as `left` says, over generators made for it: each is let go there, the
    /// last first, as compiled code does, running the cleanup of each `with` it's stopped in. A
    /// cleanup's failure is noted on a failure leaving the loop, and otherwise is the loop's own,
    /// in place of the `break` or `return`, wrapped as the loop's `? from Wrap(...)` says. A
    /// generator held anywhere else too is let go when its last owner is, which isn't modelled here.
    fn let_go_of(
        &mut self,
        items: &stream::Items,
        left: Run<()>,
        (wraps, at): (&[Option<&'static Expr>], Span),
    ) -> Run<()> {
        // A loop that ends as `zip`'s list runs out leaves its generator stopped at a `yield`, as
        // `break` does.
        let mut left = match left {
            Ok(()) | Err(Flow::Break) => Ok(()),
            Err(flow @ (Flow::Return(_) | Flow::Fail(..))) => Err(flow),
            other => return other,
        };
        let stream::Items::Stream(stream) = items else { return left };
        let mut failures = Vec::new();
        stream::let_go_owned(&stream.source, &mut failures, 0);
        for (index, failed) in failures {
            let wrap = wraps.get(index).copied().flatten();
            left = match (failed, left) {
                (Flow::Fail(cleanup, _), Err(Flow::Fail(mut error, failed_at))) => {
                    let Value::Instance(instance) = &mut error else {
                        return unsupported("a failure that takes no notes, leaving a generator's loop");
                    };
                    let note = self.display(&cleanup)?;
                    instance.notes.push(note);
                    Err(Flow::Fail(error, failed_at))
                }
                (Flow::Fail(cleanup, failed_at), _) => match wrap {
                    Some(_) => self.wrapped(wrap, at),
                    None => Err(Flow::Fail(cleanup, failed_at)),
                },
                (other, _) => return Err(other),
            };
        }
        left
    }

    /// `for ref item in items:`, `for at, ref item in enumerate(items):` and
    /// `for key, ref value in items.items():`, each name standing for the item where it is.
    fn for_ref(
        &mut self,
        names: &'static [String],
        iter: &'static Expr,
        body: &'static [Stmt],
        span: Span,
    ) -> Run<()> {
        if let ([name], Some(call)) = (names, self.parallel_call(iter)?) {
            if self.ref_loop_is_plain(name, call, body) {
                return self.for_ref(names, call.source(), body, span);
            }
            return self.for_ref_parallel(name, call, body, span);
        }
        let (place, numbered, keyed) = match (&iter.kind, names.len()) {
            (_, 1) => (self.place_of(iter)?, false, false),
            (ExprKind::Call { callee, args }, 2) => match (&callee.kind, args.as_slice()) {
                (ExprKind::Name(name), [arg]) if name == "enumerate" && !self.is_bound(name) => {
                    (self.place_of(&arg.value)?, true, false)
                }
                (ExprKind::Attr { object, name, .. }, []) if name == "items" => {
                    (self.place_of(object)?, false, true)
                }
                _ => (None, false, false),
            },
            _ => (None, false, false),
        };
        let Some(place) = place else { return unsupported("`for ref` over something that isn't a place") };
        // A value of a type with positions of its own: each place its `ref` `def []` gives.
        let own = self.peek_place(&place, iter.span, |held| match held {
            Value::Instance(instance) => Some(instance.ty.clone()),
            _ => None,
        })?;
        let positions: Vec<Value> = match (self.read_place(&place, iter.span)?, &own) {
            (Value::Instance(_), Some(ty)) if !keyed => self.own_positions(&place, ty, iter.span)?,
            (Value::List(items), _) if !keyed => (0..items.len()).map(|at| Value::Int(as_int(at))).collect(),
            (Value::Dict(pairs), _) if keyed => pairs.into_iter().map(|(key, _)| key).collect(),
            // A `Table`'s items are reached by their `Id`s, in slot order.
            (Value::Table(table), _) if !keyed => table.live().map(|(id, _)| Value::Id(id)).collect(),
            (other, _) => return unsupported(format!("`for ref` over a {}", other.kind())),
        };
        let item_name = names.last().map_or("_", String::as_str);
        // An item lent out of something written here, not a name lent already, comes back at the
        // end of each turn, and the `where` rules of the structs it's in run on it then.
        let lent_here = !matches!(
            match (&iter.kind, numbered, keyed) {
                (ExprKind::Call { args, .. }, true, _) => args.first().map(|arg| &arg.value.kind),
                (ExprKind::Call { callee, .. }, _, true) => match &callee.kind {
                    ExprKind::Attr { object, .. } => Some(&object.kind),
                    _ => None,
                },
                (kind, ..) => Some(kind),
            },
            Some(ExprKind::Name(_))
        );
        for (count, position) in positions.into_iter().enumerate() {
            let mark = self.mark();
            if numbered {
                let first = names.first().map_or("_", String::as_str);
                self.bind(first, Value::Int(as_int(count)))?;
            } else if keyed {
                let first = names.first().map_or("_", String::as_str);
                self.bind(first, position.clone())?;
            }
            match &own {
                Some(ty) => {
                    // An empty position, as its type's `is_live` says, is skipped.
                    if !self.live_at(&place, ty, &position, span)? {
                        self.restore(mark);
                        continue;
                    }
                    let Some(item) = self.indexed_place(place.clone(), position.clone(), false, span)? else {
                        return unsupported("a position its type's `def []` gives no place for");
                    };
                    self.bind_ref(item_name, item)?;
                }
                None => self.bind_ref(item_name, place.then(Step::At(position.clone())).lent_alone())?,
            }
            let ran = self.stmts(body);
            self.restore(mark);
            // However the turn ends but by a crash: a `return`, a `break` or a failure passing up
            // brings the item back too.
            let came_back =
                matches!(ran, Ok(()) | Err(Flow::Continue | Flow::Break | Flow::Return(_) | Flow::Fail(..)));
            if own.is_none() && lent_here && came_back {
                self.rules_along(&place.then(Step::At(position)), span)?;
            }
            match ran {
                Ok(()) | Err(Flow::Continue) => {}
                Err(Flow::Break) => break,
                Err(other) => return Err(other),
            }
        }
        Ok(())
    }

    /// The value of a constant, worked out the first time it's read.
    pub(crate) fn constant(&mut self, name: &str) -> Run<Option<Value>> {
        if let Some(value) = self.constants.get(name) {
            return Ok(Some(value.clone()));
        }
        let Some(expr) = self.program.constants.get(name).copied() else { return Ok(None) };
        let previous = self.push_frame(Vec::new(), None)?;
        let value = self.eval(expr);
        self.pop_frame(previous);
        let value = value?;
        self.constants.insert(name.to_owned(), value.clone());
        Ok(Some(value))
    }

    /// Whether a function's body yields, which makes calling it make a generator.
    pub(crate) fn is_generator(&mut self, def: &'static fennec_syntax::FnDef) -> bool {
        let key = std::ptr::from_ref(def) as usize;
        if let Some(known) = self.generators.get(&key) {
            return *known;
        }
        let yields = yields_in(&def.body);
        self.generators.insert(key, yields);
        yields
    }
}

/// The name `shared name = value` declares, when that's what a statement is: it's written in as
/// `name: shared = value`, a type of `shared` with nothing in its brackets.
fn shared_declared(kind: &StmtKind) -> Option<&str> {
    let StmtKind::Assign { target, op: None, ty: Some(written), .. } = kind else { return None };
    let ExprKind::Name(name) = &target.kind else { return None };
    (written.name == "shared" && written.args.is_empty()).then_some(name.as_str())
}

/// Whether statements yield, outside any function written inside them.
fn yields_in(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match &stmt.kind {
        StmtKind::Yield(_) => true,
        StmtKind::If { branches, otherwise } => {
            branches.iter().any(|(_, body)| yields_in(body)) || otherwise.as_deref().is_some_and(yields_in)
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => {
            yields_in(body)
        }
        StmtKind::Expr(expr) => match &expr.kind {
            ExprKind::Match { arms, .. } => arms.iter().any(|arm| yields_in(&arm.body)),
            _ => false,
        },
        StmtKind::Lend { otherwise, .. } => yields_in(otherwise),
        _ => false,
    })
}

/// A count as an int.
pub(crate) fn as_int(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

/// Where `at` is in something `len` long, counting from the end when it's negative.
pub(crate) fn position(at: i64, len: usize, span: Span) -> Run<usize> {
    let signed = as_int(len);
    let resolved = if at < 0 { at + signed } else { at };
    match usize::try_from(resolved) {
        Ok(found) if found < len => Ok(found),
        _ => crash(format!("index {at} is out of range for length {len}"), span),
    }
}

enum Reached<'v> {
    Inside(&'v Value),
    Made(Value),
}

/// One step into a value, for reading.
fn step_into<'v>(value: &'v Value, step: &Step, span: Span) -> Run<Reached<'v>> {
    match (value, step) {
        (Value::Instance(instance), Step::Field(name)) => match instance.field(name) {
            Some(field) => Ok(Reached::Inside(field)),
            None => unsupported(format!("the field `{name}`")),
        },
        (Value::List(items) | Value::Tuple(items), Step::At(Value::Int(at))) => {
            let at = position(*at, items.len(), span)?;
            items.get(at).map(Reached::Inside).map_or_else(|| crash("index out of range", span), Ok)
        }
        (Value::Str(text), Step::At(Value::Int(at))) => {
            let at = position(*at, text.char_count(), span)?;
            Ok(Reached::Made(Value::Str(text.char_at(at).map(Text::from).unwrap_or_default())))
        }
        (Value::Bytes(data), Step::At(Value::Int(at))) => {
            let at = position(*at, data.len(), span)?;
            Ok(Reached::Made(Value::Int(data.get(at).map_or(0, |byte| i64::from(*byte)))))
        }
        (Value::Dict(pairs), Step::At(key)) => match find_key(pairs, key)? {
            Some(at) => pairs
                .get(at)
                .map(|(_, value)| Reached::Inside(value))
                .map_or_else(|| unsupported("a key"), Ok),
            None => crash(format!("{} isn't a key here", plain_key(key)), span),
        },
        (Value::Cap(Cap::Args), Step::At(Value::Int(at))) => {
            position(*at, 0, span)?;
            unsupported("an argument")
        }
        (Value::Table(table), Step::At(Value::Id(id))) => {
            match table.find(*id).and_then(|at| table.slots.get(at)).and_then(|slot| slot.item.as_ref()) {
                Some(item) => Ok(Reached::Inside(item)),
                None => crash(crate::value::MISSING, span),
            }
        }
        (other, _) => unsupported(format!("reaching into a {}", other.kind())),
    }
}

/// One step into a value, for writing.
fn step_into_mut<'v>(value: &'v mut Value, step: &Step, span: Span, adding: bool) -> Run<&'v mut Value> {
    match (value, step) {
        (Value::Instance(instance), Step::Field(name)) => match instance.field_mut(name) {
            Some(field) => Ok(field),
            None => unsupported(format!("the field `{name}`")),
        },
        (Value::List(items), Step::At(Value::Int(at))) => {
            let at = position(*at, items.len(), span)?;
            items.get_mut(at).map_or_else(|| crash("index out of range", span), Ok)
        }
        (Value::Dict(pairs), Step::At(key)) => {
            let found = find_key(pairs, key)?;
            let at = match found {
                Some(at) => at,
                None if adding => {
                    pairs.push((key.clone(), Value::Unit));
                    pairs.len() - 1
                }
                None => return crash(format!("{} isn't a key here", plain_key(key)), span),
            };
            pairs.get_mut(at).map(|(_, value)| value).map_or_else(|| unsupported("a key"), Ok)
        }
        (Value::Table(table), Step::At(Value::Id(id))) => {
            let missing = crate::value::MISSING;
            match table.find(*id).and_then(|at| table.slots.get_mut(at)).and_then(|slot| slot.item.as_mut()) {
                Some(item) => Ok(item),
                None => crash(missing, span),
            }
        }
        (other, _) => unsupported(format!("changing part of a {}", other.kind())),
    }
}

/// Where a key is among a dict's pairs, by plain equality, which is what a dict's keys use.
pub(crate) fn find_key(pairs: &[(Value, Value)], key: &Value) -> Run<Option<usize>> {
    for (at, (held, _)) in pairs.iter().enumerate() {
        if show::plain_equal(held, key)? {
            return Ok(Some(at));
        }
    }
    Ok(None)
}

/// A key as a message names it.
fn plain_key(key: &Value) -> String {
    match key {
        Value::Str(text) => crate::text::quoted(text),
        Value::Int(n) => n.to_string(),
        other => other.kind().to_owned(),
    }
}

/// Whether a loop's generator is made for the loop: a call's, as `for x in counts()?:` makes one,
/// and not one held in a name or a field, which something else may hold too.
fn made_for_loop(iter: &Expr) -> bool {
    let made = match &iter.kind {
        ExprKind::Try { value, .. } => value,
        _ => iter,
    };
    matches!(made.kind, ExprKind::Call { .. })
}

/// The wrapper each generator a loop owns fails through, partway or when it's let go: its own
/// `? from Wrap(...)`'s, or for `enumerate` and `zip`, each argument's, in order.
fn arg_wraps(iter: &'static Expr) -> Vec<Option<&'static Expr>> {
    let context = |value: &'static Expr| match &value.kind {
        ExprKind::Try { context, .. } => context.as_deref(),
        _ => None,
    };
    if matches!(iter.kind, ExprKind::Try { .. }) {
        return vec![context(iter)];
    }
    match &iter.kind {
        ExprKind::Call { callee, args } if matches!(&callee.kind, ExprKind::Name(name) if name == "enumerate" || name == "zip") => {
            args.iter().map(|arg| context(&arg.value)).collect()
        }
        _ => Vec::new(),
    }
}
