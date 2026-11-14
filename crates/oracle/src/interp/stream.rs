//! Looping over things, and generators. A generator runs on a thread of its own, with names of
//! its own, and it and whoever reads it take turns: the reader asks for an item and waits, and
//! the generator runs to its next `yield` and waits. Only one of them ever runs, so a run is the
//! same every time.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use fennec_syntax::{Expr, ExprKind, FnDef, Span};

use super::{Binding, Flow, Interp, Run, Shared, unsupported};
use crate::value::{Cap, Stream, Value};

/// The most generators that may be running at once.
const MOST_GENERATORS: usize = 1_000;

/// Each generator's thread's stack.
const GENERATOR_STACK: usize = 64 << 20;

/// Where items come from, one at a time.
#[derive(Debug)]
pub(crate) enum Source {
    Items(std::vec::IntoIter<Value>),
    Gen(GenLink),
    /// `enumerate` over a generator.
    Numbered(Box<Source>, i64),
    /// `zip` with a generator.
    Paired(Box<Source>, Box<Source>),
    /// Another stream, read on from where it is.
    Of(Stream),
    /// The lines of a file a `with` lent, read one at a time as they're asked for.
    Lines(FileLines),
    /// The messages a launched task's inbox holds, read until there are none.
    Inbox(Arc<Mutex<std::collections::VecDeque<Value>>>),
}

/// Where a file's `lines()` reads from: the file system, the file's handle, and where `lines()`
/// was called, which a failure to read is said to be.
#[derive(Debug)]
pub(crate) struct FileLines {
    pub(crate) disk: Arc<Mutex<super::disk::Disk>>,
    pub(crate) handle: i64,
    pub(crate) span: Span,
    /// How many `parallel` rounds and launched tasks had started when it was made.
    pub(crate) made: u64,
}

/// The reader's end of a generator.
#[derive(Debug)]
pub(crate) struct GenLink {
    resume: Option<Sender<()>>,
    items: Receiver<Msg>,
    done: bool,
    thread: Option<std::thread::JoinHandle<()>>,
    /// How many `parallel` rounds and launched tasks had started when it was made.
    made: u64,
}

/// What a generator sends its reader.
#[derive(Debug)]
pub(crate) enum Msg {
    Item(Value),
    End,
    Stopped(Flow),
}

/// The generator's end: where it sends items, and where it's asked for the next.
pub(crate) struct YieldSide {
    items: Sender<Msg>,
    resume: Receiver<()>,
}

impl Drop for GenLink {
    /// Letting go of a generator lets its thread go: it wakes, finds nobody asking, and ends,
    /// running nothing more of the program.
    fn drop(&mut self) {
        self.resume.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Refuses reading, inside a `parallel` round or a launched task, something made before it started:
/// what runs beside it could read it too, each reading on from wherever the other left it.
fn read_in_round(shared: &Shared, made: u64) -> Run<()> {
    match shared.reads_since() {
        Some(started) if made < started => {
            unsupported("a generator made outside a `parallel` round or a launched task, read inside it")
        }
        _ => Ok(()),
    }
}

impl GenLink {
    fn next(&mut self, shared: &Shared) -> Run<Option<Value>> {
        read_in_round(shared, self.made)?;
        if self.done {
            return Ok(None);
        }
        let asked = self.resume.as_ref().is_some_and(|resume| resume.send(()).is_ok());
        if !asked {
            self.done = true;
            return unsupported("a generator stopped");
        }
        match self.items.recv() {
            Ok(Msg::Item(value)) => Ok(Some(value)),
            Ok(Msg::End) => {
                self.done = true;
                Ok(None)
            }
            Ok(Msg::Stopped(flow)) => {
                self.done = true;
                Err(flow)
            }
            Err(_) => {
                self.done = true;
                unsupported("a generator's thread stopped")
            }
        }
    }
}

impl Source {
    fn next(&mut self, shared: &Shared) -> Run<Option<Value>> {
        match self {
            Source::Items(items) => Ok(items.next()),
            Source::Gen(link) => link.next(shared),
            Source::Numbered(inner, at) => match inner.next(shared)? {
                Some(item) => {
                    let numbered = Value::Tuple(vec![Value::Int(*at), item]);
                    *at += 1;
                    Ok(Some(numbered))
                }
                None => Ok(None),
            },
            Source::Paired(left, right) => {
                let Some(first) = left.next(shared)? else { return Ok(None) };
                let Some(second) = right.next(shared)? else { return Ok(None) };
                Ok(Some(Value::Tuple(vec![first, second])))
            }
            Source::Of(stream) => {
                let Ok(mut source) = stream.source.lock() else { return unsupported("a generator was lost") };
                source.next(shared)
            }
            Source::Lines(lines) => {
                read_in_round(shared, lines.made)?;
                let Ok(mut disk) = lines.disk.lock() else { return unsupported("the file system was lost") };
                let read = disk.read_line(lines.handle);
                drop(disk);
                super::files::done(read, lines.span).map(|line| line.map(Value::Str))
            }
            Source::Inbox(queue) => match queue.lock() {
                Ok(mut queue) => Ok(queue.pop_front()),
                Err(_) => unsupported("a channel was lost"),
            },
        }
    }
}

/// What a loop goes over.
pub(crate) enum Items {
    List(std::vec::IntoIter<Value>),
    Stream(Stream),
}

impl Interp {
    /// The items of something a loop goes over: a list's, a set's, a dict's keys, a string's
    /// characters, or a generator's, as they're asked for.
    pub(crate) fn items_of(&mut self, over: Value, span: Span) -> Run<Items> {
        Ok(Items::List(match over {
            Value::List(items) | Value::Tuple(items) | Value::Set(items) => items.into_iter(),
            Value::Dict(pairs) => pairs.into_iter().map(|(key, _)| key).collect::<Vec<_>>().into_iter(),
            Value::Str(text) => {
                text.chars().map(|c| Value::Str(c.to_string())).collect::<Vec<_>>().into_iter()
            }
            Value::Table(table) => {
                table.slots.into_iter().filter_map(|slot| slot.item).collect::<Vec<_>>().into_iter()
            }
            Value::Stream(stream) => return Ok(Items::Stream(stream)),
            Value::Channel(end) if !end.sends => {
                let Value::Stream(stream) = Interp::stream_of(Source::Inbox(end.queue), None) else {
                    return unsupported("an inbox that isn't one");
                };
                return Ok(Items::Stream(stream));
            }
            Value::Cap(Cap::Args) => Vec::new().into_iter(),
            Value::Instance(instance) => match self.program.method(&instance.ty, "iter") {
                Some(def) => {
                    let receiver = super::calls::Receiver::Value(Value::Instance(instance));
                    let made = self.call_def(def, None, Some(receiver), Vec::new(), span)?;
                    return self.items_of(made, span);
                }
                None => return unsupported(format!("looping over a {}", instance.ty)),
            },
            other => return unsupported(format!("looping over a {}", other.kind())),
        }))
    }

    pub(crate) fn next_item(&mut self, items: &mut Items) -> Run<Option<Value>> {
        self.tick()?;
        match items {
            Items::List(items) => Ok(items.next()),
            Items::Stream(stream) => {
                let Ok(mut source) = stream.source.lock() else { return unsupported("a generator was lost") };
                source.next(&self.shared)
            }
        }
    }

    /// Every item of something a loop goes over, at once.
    pub(crate) fn all_items(&mut self, over: Value, span: Span) -> Run<Vec<Value>> {
        let mut items = self.items_of(over, span)?;
        let mut all = Vec::new();
        while let Some(item) = self.next_item(&mut items)? {
            all.push(item);
            if all.len() > super::MOST_ITEMS {
                return unsupported("ran too long: a generator too long");
            }
        }
        Ok(all)
    }

    /// A stream over a source made here.
    pub(crate) fn stream_of(source: Source, shown: Option<String>) -> Value {
        Value::Stream(Stream { source: Arc::new(Mutex::new(source)), shown })
    }

    /// Whether a value is a stream, whose items are made as they're asked for.
    pub(crate) fn is_stream(value: &Value) -> bool {
        matches!(value, Value::Stream(_))
    }

    /// The source a stream reads, for `enumerate` and `zip` to read on from.
    pub(crate) fn source_of(&mut self, value: Value, span: Span) -> Run<Source> {
        match value {
            Value::Stream(stream) => Ok(Source::Of(stream)),
            other => Ok(Source::Items(self.all_items(other, span)?.into_iter())),
        }
    }

    /// Calling a function that yields: a generator that runs its body when first asked.
    pub(crate) fn generator_from_def(
        &mut self,
        def: &'static FnDef,
        vars: Vec<(String, Binding)>,
        shown: Option<String>,
    ) -> Run<Value> {
        if vars.iter().any(|(_, binding)| matches!(binding, Binding::Ref(_))) {
            return unsupported("a generator with a `ref` parameter");
        }
        let link = self.spawn(move |interp| {
            let previous = interp.push_frame(vars, None)?;
            let ran = interp.block(&def.body);
            interp.pop_frame(previous);
            match ran {
                Ok(()) | Err(Flow::Return(_)) => Ok(()),
                Err(other) => Err(other),
            }
        })?;
        Ok(Interp::stream_of(Source::Gen(link), shown))
    }

    /// A generator expression kept to use later: what it reads is copied now, as it was made.
    pub(crate) fn generator_of(&mut self, expr: &'static Expr) -> Run<Value> {
        let ExprKind::Comprehension { elem, names, iter, cond, .. } = &expr.kind else {
            return unsupported("a generator that isn't one");
        };
        let mut reads: Vec<&'static Expr> = vec![elem, iter];
        if let Some(cond) = cond {
            reads.push(cond);
        }
        let captured = self.captured(&reads, &[])?;
        let shown = self.program.type_at(expr.span).map(super::calls::shown_type);
        let call = self.parallel_call(iter)?;
        let link = self.spawn(move |interp| {
            let vars = captured.into_iter().map(|(name, value)| (name, Binding::Value(value))).collect();
            let previous = interp.push_frame(vars, None)?;
            let ran = match call {
                Some(call) => interp.yield_parallel(elem, names, call, cond.as_deref()),
                None => interp.yield_each(elem, names, iter, cond.as_deref()),
            };
            interp.pop_frame(previous);
            ran
        })?;
        Ok(Interp::stream_of(Source::Gen(link), shown))
    }

    fn yield_each(
        &mut self,
        elem: &'static Expr,
        names: &'static [String],
        iter: &'static Expr,
        cond: Option<&'static Expr>,
    ) -> Run<()> {
        self.each_of(elem, names, iter, cond, &mut |interp, item| {
            interp.yield_value(item)?;
            Ok(true)
        })
    }

    /// Runs `each` on every item a generator expression makes, here and now, until it says to
    /// stop: for a builtin the expression is written straight into, which reads it at once.
    pub(crate) fn each_of(
        &mut self,
        elem: &'static Expr,
        names: &'static [String],
        iter: &'static Expr,
        cond: Option<&'static Expr>,
        each: &mut dyn FnMut(&mut Interp, Value) -> Run<bool>,
    ) -> Run<()> {
        let over = self.eval(iter)?;
        let mut items = self.items_of(over, iter.span)?;
        let mark = self.frame()?.vars.len();
        let ran = (|| {
            while let Some(item) = self.next_item(&mut items)? {
                self.bind_fresh(names, item)?;
                let keep = match cond {
                    Some(cond) => self.truth(cond)?,
                    None => true,
                };
                if keep {
                    let made = self.eval(elem)?;
                    if !each(self, made)? {
                        break;
                    }
                }
                if let Ok(frame) = self.frame_mut() {
                    frame.vars.truncate(mark);
                }
            }
            Ok(())
        })();
        if let Ok(frame) = self.frame_mut() {
            frame.vars.truncate(mark);
        }
        ran
    }

    /// Runs `body` on a thread of its own as a generator, which starts when it's first asked
    /// for an item.
    fn spawn(&mut self, body: impl FnOnce(&mut Interp) -> Run<()> + Send + 'static) -> Run<GenLink> {
        let shared = Arc::clone(&self.shared);
        if shared.threads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= MOST_GENERATORS {
            shared.threads.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            return unsupported("ran too long: too many generators at once");
        }
        let (items, read) = channel();
        let (resume, asked) = channel();
        let thread = std::thread::Builder::new()
            .stack_size(GENERATOR_STACK)
            .spawn(move || generator_thread(&shared, YieldSide { items, resume: asked }, body));
        let Ok(thread) = thread else {
            self.shared.threads.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            return unsupported("a generator's thread couldn't start");
        };
        let made = self.shared.rounds_started.load(std::sync::atomic::Ordering::SeqCst);
        Ok(GenLink { resume: Some(resume), items: read, done: false, thread: Some(thread), made })
    }

    /// In a generator: hands its reader an item, then waits to be asked for the next.
    pub(crate) fn yield_value(&mut self, value: Value) -> Run<()> {
        if self.holding > 0 {
            return unsupported("a `yield` inside a `lock`");
        }
        let Some(side) = &self.yields else { return unsupported("`yield` outside a generator") };
        if side.items.send(Msg::Item(value)).is_err() || side.resume.recv().is_err() {
            return Err(Flow::Abandoned);
        }
        Ok(())
    }
}

fn generator_thread(shared: &Arc<Shared>, side: YieldSide, body: impl FnOnce(&mut Interp) -> Run<()>) {
    if side.resume.recv().is_ok() {
        let items = side.items.clone();
        let mut interp = Interp::new(Arc::clone(shared), Some(side));
        match body(&mut interp) {
            Ok(()) => {
                let _ = items.send(Msg::End);
            }
            Err(Flow::Abandoned) => {}
            Err(other) => {
                let _ = items.send(Msg::Stopped(other));
            }
        }
    }
    shared.threads.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
}
