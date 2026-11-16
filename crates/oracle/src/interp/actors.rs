//! Tasks that run beside their block and talk through `std.concurrent`'s channels: a `with
//! launch(task)?:` with no `as`, of a task that takes no inbox. The channels are modeled here from
//! what `intrinsics.fen` says each channel intrinsic does, and `std.concurrent`'s own Fennec runs
//! over them.
//!
//! Each task runs on a thread of its own, but only one thread ever runs: whoever holds the baton
//! runs until it has to wait, for a message, for room to send one, or for a task to end, and then
//! hands it to the next one after it, in the order they were launched, that can go on. So a run is
//! the same every time, and where nothing waits on a channel it's the block first and then its
//! task, as a launch with `as sends` runs.
//!
//! One thing about a run hangs on time and is tried every way: where several tasks send on one
//! channel, which of their messages comes next. Before a receive, whatever else can go on goes
//! first, so every message that could be next is there, and where the first from each sender
//! differ, each is taken in turn, on a run of its own (`lib.rs`), and every run has to print the
//! same and end the same. Anything else whose outcome hangs on time is refused:
//! - two tasks taking messages from one channel to one receiver, which gets which hanging on time;
//! - several senders' messages to a listener of a channel to every listener, or waiting for room,
//!   when the next ones differ;
//! - a channel to every listener sent more messages than it keeps, which a listener could fall
//!   behind on or not;
//! - a send waiting for room when its channel closes, which got in or not;
//! - waiting on a channel inside a `lock`;
//! - every task waiting on another, which hangs.
//!
//! A wait given a timeout times out only when nothing else can go on: whatever could still answer
//! it is taken to answer in time.
//! What a task's block and the task itself reach, printing, the file system and `shared` values,
//! is held to what `tasks.rs` says, each seeing the launch from its own side.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use fennec_syntax::{Span, Stmt, WithName};

use super::calls::ArgVal;
use super::parallel::Scope;
use super::tasks::Launched;
use super::{Flow, Interp, Run, unsupported};
use crate::value::{Channel, Value, same};

/// What a run where every task waits on another says.
const HANGS: &str = "every task waits on another, so the program hangs";

/// Each task's thread's stack.
const TASK_STACK: usize = 64 << 20;

/// The most tasks one run may launch.
const MOST_TASKS: usize = 1_000;

/// Who runs, and the channels they share.
#[derive(Default)]
pub(crate) struct Sched {
    state: Mutex<State>,
    turn: Condvar,
    /// Which message each receive takes where that hangs on time, as `lib.rs` tries every way.
    pub(crate) orders: Mutex<Orders>,
}

/// The choices of a run that tries one order of messages: those to make, in turn, and those
/// made, each with how many there were to choose from.
#[derive(Default)]
pub(crate) struct Orders {
    pub(crate) script: Vec<usize>,
    pub(crate) made: Vec<(usize, usize)>,
}

impl Orders {
    /// Chooses one of `count`: as the script says, or the first, past its end.
    fn choose(&mut self, count: usize) -> usize {
        let chosen = self.script.get(self.made.len()).copied().filter(|chosen| *chosen < count).unwrap_or(0);
        self.made.push((chosen, count));
        chosen
    }
}

struct State {
    /// The one that runs now.
    running: usize,
    /// Each one that runs, by number: `main`'s thread first, then each task in the order it was
    /// launched.
    actors: Vec<Actor>,
    channels: BTreeMap<i64, Chan>,
    /// Each listener to a channel to every listener, by the number it's received from by.
    listeners: BTreeMap<i64, Listener>,
    next_number: i64,
    /// Whether the run was given up, so every thread that waits is to end.
    given_up: bool,
    /// The last that printed, and the last that reached the file system, each with its clock then.
    last_outside: [Option<(usize, Clock)>; 2],
}

impl Default for State {
    fn default() -> State {
        State {
            running: 0,
            actors: vec![Actor::ready(Vec::new(), Vec::new())],
            channels: BTreeMap::new(),
            listeners: BTreeMap::new(),
            next_number: 1,
            given_up: false,
            last_outside: [None, None],
        }
    }
}

struct Actor {
    state: Doing,
    /// The scopes it runs inside, kept while another runs.
    scopes: Vec<Scope>,
    /// How a task ended, once it has.
    ended: Option<Run<()>>,
    /// Whether its wait timed out.
    timed_out: bool,
    /// What it's heard of each one's sends and ends, by how many of them it's seen: what happened
    /// before what it does now, whatever order things ran in.
    clock: Clock,
}

impl Actor {
    fn ready(scopes: Vec<Scope>, clock: Clock) -> Actor {
        Actor { state: Doing::Ready, scopes, ended: None, timed_out: false, clock }
    }
}

/// A vector clock: for each one that runs, by number, how many of its steps are known.
type Clock = Vec<u64>;

/// Counts a step of `actor`'s on its clock.
fn tick(clock: &mut Clock, actor: usize) {
    if clock.len() <= actor {
        clock.resize(actor + 1, 0);
    }
    if let Some(steps) = clock.get_mut(actor) {
        *steps += 1;
    }
}

/// Takes in what `other` knew.
fn merge(clock: &mut Clock, other: &Clock) {
    if clock.len() < other.len() {
        clock.resize(other.len(), 0);
    }
    for (mine, theirs) in clock.iter_mut().zip(other) {
        *mine = (*mine).max(*theirs);
    }
}

/// Whether what's stamped `earlier` happened before what's stamped `later`, whatever order
/// things ran in.
fn happened_before(earlier: &Clock, later: &Clock) -> bool {
    earlier.iter().enumerate().all(|(at, steps)| *steps <= later.get(at).copied().unwrap_or(0))
}

/// A message sent: who sent it, what, and the sender's clock as it sent it.
struct Sent {
    sender: usize,
    value: Value,
    clock: Clock,
}

enum Doing {
    Ready,
    /// Waiting, with a timeout when `timed`.
    Waiting(Wait, bool),
    Ended,
}

#[derive(Clone, Copy)]
enum Wait {
    /// For a message from the channel or listener of this number, or for it to close.
    Message(i64),
    /// For room to send on this channel, or for it to close.
    Room(i64),
    /// For this task to end.
    Task(usize),
}

/// A channel.
struct Chan {
    size: usize,
    /// Whether it's to every listener, rather than to one receiver.
    every: bool,
    closed: bool,
    /// What's sent and not yet taken, for a channel to one receiver.
    queue: VecDeque<Sent>,
    /// Every message sent, for a channel to every listener.
    sent: Vec<Sent>,
    /// Who's taken a message from it.
    receivers: BTreeSet<usize>,
    /// Each send waiting for room.
    waiting_sends: Vec<Sent>,
    /// Whether it's a task's inbox the task has left by ending.
    left: bool,
}

/// A listener to a channel to every listener: the channel, and how many of its messages it's read.
struct Listener {
    channel: i64,
    at: usize,
}

impl State {
    /// Whether the one numbered `actor` can go on.
    fn can_go(&self, actor: usize) -> bool {
        let Some(found) = self.actors.get(actor) else { return false };
        match found.state {
            Doing::Ready => true,
            Doing::Ended => false,
            Doing::Waiting(Wait::Task(task), _) => {
                self.actors.get(task).is_none_or(|task| matches!(task.state, Doing::Ended))
            }
            Doing::Waiting(Wait::Room(number), _) => {
                self.channels.get(&number).is_none_or(|chan| chan.closed || chan.queue.len() < chan.size)
            }
            Doing::Waiting(Wait::Message(number), _) => self.has_message(number),
        }
    }

    /// Whether a receive from `number`, a channel or a listener, would find a message, or the
    /// channel closed.
    fn has_message(&self, number: i64) -> bool {
        if let Some(listener) = self.listeners.get(&number) {
            return self
                .channels
                .get(&listener.channel)
                .is_none_or(|chan| chan.closed || listener.at < chan.sent.len());
        }
        self.channels.get(&number).is_none_or(|chan| chan.closed || !chan.queue.is_empty())
    }

    /// The next one after `from`, in order, that can go on, `from` itself last.
    fn next_after(&self, from: usize) -> Option<usize> {
        let count = self.actors.len();
        (1..=count).map(|step| (from + step) % count).find(|&actor| self.can_go(actor))
    }

    /// Hands the baton on from `from`: to the next that can go on, or, when none can, to the
    /// first waiting with a timeout, whose wait then times out. `false` when nothing can.
    fn hand_on(&mut self, from: usize) -> bool {
        let next = self.next_after(from).or_else(|| {
            let timed =
                self.actors.iter().position(|actor| matches!(actor.state, Doing::Waiting(_, true)))?;
            if let Some(actor) = self.actors.get_mut(timed) {
                actor.timed_out = true;
            }
            Some(timed)
        });
        let Some(next) = next else { return false };
        if let Some(actor) = self.actors.get_mut(next) {
            actor.state = Doing::Ready;
        }
        self.running = next;
        true
    }
}

impl Interp {
    fn sched_state(&self) -> Run<MutexGuard<'_, State>> {
        self.shared.sched.state.lock().map_or_else(|_| unsupported("the tasks were lost"), Ok)
    }

    /// Waits for `wait`, handing the baton on until it comes back. Says whether the wait timed
    /// out, which only one given a timeout can.
    fn wait_for(&self, wait: Wait, timed: bool) -> Run<bool> {
        if self.holding > 0 {
            return unsupported("waiting on a channel or a task inside a `lock`");
        }
        let me = self.actor;
        let mut state = self.sched_state()?;
        if state.given_up {
            drop(state);
            return self.given_up_why();
        }
        // What runs inside what is each one's own, kept while another runs.
        let mine =
            self.shared.scopes.lock().map(|mut scopes| std::mem::take(&mut *scopes)).unwrap_or_default();
        if let Some(actor) = state.actors.get_mut(me) {
            actor.state = Doing::Waiting(wait, timed);
            actor.scopes = mine;
        }
        if !state.hand_on(me) {
            // Every thread that's waiting says this, whichever found it, so it's noted first.
            if let Ok(mut refused) = self.shared.refused.lock() {
                refused.get_or_insert_with(|| HANGS.to_owned());
            }
            state.given_up = true;
            drop(state);
            self.shared.sched.turn.notify_all();
            return unsupported(HANGS);
        }
        self.shared.sched.turn.notify_all();
        while state.running != me && !state.given_up {
            state =
                self.shared.sched.turn.wait(state).map_or_else(|_| unsupported("the tasks were lost"), Ok)?;
        }
        if state.given_up {
            drop(state);
            return self.given_up_why();
        }
        let Some(actor) = state.actors.get_mut(me) else { return unsupported("a task was lost") };
        let timed_out = std::mem::take(&mut actor.timed_out);
        let scopes = std::mem::take(&mut actor.scopes);
        drop(state);
        if let Ok(mut held) = self.shared.scopes.lock() {
            *held = scopes;
        }
        Ok(timed_out)
    }

    /// Why a wait ends with the run given up: what a task found it couldn't model, or that it was.
    fn given_up_why<T>(&self) -> Run<T> {
        let refused = self.shared.refused.lock().ok().and_then(|refused| refused.clone());
        unsupported(refused.unwrap_or_else(|| "a task ran on after the run was given up".to_owned()))
    }

    /// Notes printing, or reaching the file system, refusing it where it could come before or after
    /// what another task did last of the same, as which hangs on time: it has to have heard of
    /// that, through a message, a task's end or a launch.
    pub(crate) fn outside_in_order(&self, outside: super::parallel::Outside) -> Run<()> {
        let mut state = self.sched_state()?;
        if state.actors.len() < 2 {
            return Ok(());
        }
        let me = self.actor;
        let Some(actor) = state.actors.get_mut(me) else { return unsupported("a task was lost") };
        tick(&mut actor.clock, me);
        let mine = actor.clock.clone();
        let kind = usize::from(matches!(outside, super::parallel::Outside::Disk));
        let Some(last) = state.last_outside.get_mut(kind) else { return Ok(()) };
        if let Some((who, clock)) = last.as_ref()
            && *who != me
            && !happened_before(clock, &mine)
        {
            return unsupported(format!(
                "{} in two tasks at once, whose order hangs on time",
                outside.what()
            ));
        }
        *last = Some((me, mine));
        Ok(())
    }

    /// Gives up the run: every task waiting ends, running nothing more of the program.
    pub(crate) fn give_up(&self) {
        if let Ok(mut state) = self.shared.sched.state.lock() {
            state.given_up = true;
        }
        self.shared.sched.turn.notify_all();
    }

    /// `with launch(task)?:` with no `as`, whose task takes no inbox: the task launched on a
    /// thread of its own, the block run, and then the task waited for. What the `with` ends with
    /// is the block's leaving, once the task has ended well, or how the task ended.
    pub(crate) fn launch_beside(
        &mut self,
        task: Value,
        (inbox, names): (Option<usize>, &'static [WithName]),
        body: &'static [Stmt],
        span: Span,
    ) -> Run<()> {
        // A task with an inbox has a channel of its own, which its block sends on through what it
        // lends, `as sends`, and which closes when the block ends.
        let channel = match inbox {
            Some(size) => Some(self.new_channel(size, false)?),
            None => None,
        };
        let end = |sends: bool, number: i64| Value::Channel(Channel { sends, number });
        let given: Vec<Value> = channel.map(|number| end(false, number)).into_iter().collect();
        let (number, launched) = self.start_task(task, given, channel)?;
        self.push_scope(Scope::Launch(launched))?;
        let mark = self.mark();
        let lent = channel.map_or(Value::Int(0), |number| end(true, number));
        let ran = self.lend_names(names, lent, span).and_then(|()| self.stmts(body));
        self.restore(mark);
        self.pop_scope();
        let pending = match ran {
            Ok(()) => None,
            Err(flow @ (Flow::Return(_) | Flow::Break | Flow::Continue)) => Some(flow),
            Err(Flow::Fail(..) | Flow::Crash(..)) => {
                self.give_up();
                return unsupported("a `with launch` block that fails, which cuts its task short at once");
            }
            Err(other) => {
                self.give_up();
                return Err(other);
            }
        };
        if let Some(channel) = channel {
            let mut state = self.sched_state()?;
            if let Some(chan) = state.channels.get_mut(&channel) {
                chan.closed = true;
            }
        }
        let ended = self.task_ended(number)?;
        if let Some(channel) = channel {
            let mut state = self.sched_state()?;
            // A task that ends with messages left closes its inbox, and whether the block's sends
            // after that failed with `Closed`, and so how the `with` ends, hangs on time.
            if state.channels.remove(&channel).is_some_and(|chan| !chan.queue.is_empty()) {
                return unsupported("a launched task that ends with messages left in its inbox");
            }
        }
        match ended {
            Ok(()) => pending.map_or(Ok(()), Err),
            Err(Flow::Fail(error, _)) => Err(Flow::Fail(error, span)),
            Err(Flow::Crash(..)) => {
                let why = Value::Str("the task crashed".to_owned());
                Err(Flow::Fail(Value::instance("Crashed", None, vec![("why".to_owned(), why)]), span))
            }
            Err(Flow::Return(_) | Flow::Break | Flow::Continue) => {
                unsupported("a `return`, `break` or `continue` leaving a launched task")
            }
            Err(other) => Err(other),
        }
    }

    /// Starts `task` on a thread of its own, given `given`, after what's run so far, and gives
    /// back its number and its launch's block's view of the launch. Its inbox, if it has one,
    /// closes as it ends.
    fn start_task(&mut self, task: Value, given: Vec<Value>, inbox: Option<i64>) -> Run<(usize, Launched)> {
        let task_expr = super::tasks::task_expr(&task)?;
        let started = self.shared.rounds_started.fetch_add(1, Ordering::SeqCst) + 1;
        let launched = Launched::new();
        let mut task_scopes = Vec::new();
        {
            let Ok(scopes) = self.shared.scopes.lock() else { return unsupported("a launch was lost") };
            for scope in scopes.iter() {
                match scope {
                    Scope::Launch(outer) => task_scopes.push(Scope::Launch(outer.clone())),
                    Scope::Rounds(_) => return unsupported("a launch inside a `parallel` round"),
                }
            }
        }
        task_scopes.push(Scope::Launch(launched.task_view(started)));
        let number = {
            let mut state = self.sched_state()?;
            if state.actors.len() >= MOST_TASKS {
                return unsupported("ran too long: too many tasks");
            }
            let number = state.actors.len();
            let mut clock = state.actors.get(self.actor).map(|actor| actor.clock.clone()).unwrap_or_default();
            tick(&mut clock, self.actor);
            if let Some(parent) = state.actors.get_mut(self.actor) {
                parent.clock.clone_from(&clock);
            }
            tick(&mut clock, number);
            state.actors.push(Actor::ready(task_scopes, clock));
            number
        };
        let shared = Arc::clone(&self.shared);
        let thread = std::thread::Builder::new()
            .stack_size(TASK_STACK)
            .spawn(move || task_thread(&shared, (number, inbox), &task, (task_expr, given)));
        if thread.is_err() {
            self.give_up();
            return unsupported("a task's thread couldn't start");
        }
        Ok((number, launched))
    }

    /// A new channel of `size`, to every listener when `every`, by its number.
    fn new_channel(&self, size: usize, every: bool) -> Run<i64> {
        if size == 0 {
            return unsupported("a channel of no size");
        }
        let mut state = self.sched_state()?;
        let number = state.next_number;
        state.next_number += 1;
        let chan = Chan {
            size,
            every,
            closed: false,
            queue: VecDeque::new(),
            sent: Vec::new(),
            receivers: BTreeSet::new(),
            waiting_sends: Vec::new(),
            left: false,
        };
        state.channels.insert(number, chan);
        Ok(number)
    }

    /// Waits for task `number` to end, and gives back how it did. What it did happened before
    /// what its launcher does next.
    fn task_ended(&self, number: usize) -> Run<Run<()>> {
        loop {
            {
                let mut state = self.sched_state()?;
                if let Some(actor) = state.actors.get_mut(number)
                    && matches!(actor.state, Doing::Ended)
                {
                    let ended = actor.ended.take().unwrap_or(Ok(()));
                    let theirs = actor.clock.clone();
                    if let Some(parent) = state.actors.get_mut(self.actor) {
                        merge(&mut parent.clock, &theirs);
                        tick(&mut parent.clock, self.actor);
                    }
                    return Ok(ended);
                }
            }
            self.wait_for(Wait::Task(number), false)?;
        }
    }

    /// A channel intrinsic, as `intrinsics.fen` says it.
    pub(crate) fn channel_intrinsic(&mut self, name: &str, given: Vec<ArgVal>, span: Span) -> Run<Value> {
        let mut values = Vec::with_capacity(given.len());
        let mut why_place = None;
        for arg in given {
            match arg {
                ArgVal::Ref(_, place) => why_place = Some(place),
                other => values.push(self.arg_as_value(other)?),
            }
        }
        match (name, values.as_slice()) {
            ("new" | "new_every", [Value::Int(size)]) => {
                let Ok(size) = usize::try_from(*size) else { return unsupported("a channel of no size") };
                self.new_channel(size, name == "new_every").map(Value::Int)
            }
            ("listen", [Value::Int(number), Value::Bool(_)]) => {
                let mut state = self.sched_state()?;
                let Some(chan) = state.channels.get(number) else {
                    return unsupported("a channel let go of");
                };
                if !chan.every {
                    return Ok(Value::Int(*number));
                }
                let at = chan.sent.len();
                let listener = state.next_number;
                state.next_number += 1;
                state.listeners.insert(listener, Listener { channel: *number, at });
                Ok(Value::Int(listener))
            }
            ("send", [Value::Int(number), message, Value::Int(timeout)]) => {
                self.charge(message)?;
                self.channel_send(*number, message, *timeout)
            }
            ("receive", [Value::Int(number), Value::Int(timeout)]) => {
                let (got, why) = self.channel_receive(*number, *timeout)?;
                let Some(place) = why_place else {
                    return unsupported("`channel_receive` with no place for why");
                };
                self.write_place(&place, Value::Int(why), span)?;
                Ok(got)
            }
            ("close", [Value::Int(number)]) => {
                let mut state = self.sched_state()?;
                let Some(chan) = state.channels.get_mut(number) else { return Ok(Value::Unit) };
                if !chan.waiting_sends.is_empty() {
                    return unsupported(
                        "a send waiting for room when its channel closes, which got in or not by time",
                    );
                }
                chan.closed = true;
                Ok(Value::Unit)
            }
            ("free", [Value::Int(number)]) => {
                let mut state = self.sched_state()?;
                state.channels.remove(number);
                state.listeners.retain(|_, listener| listener.channel != *number);
                Ok(Value::Unit)
            }
            _ => unsupported(format!("std.intrinsics.channel_{name}")),
        }
    }

    /// `channel_send`: 0 sent, 1 closed, 2 timed out.
    pub(crate) fn channel_send(&mut self, number: i64, message: &Value, timeout: i64) -> Run<Value> {
        let me = self.actor;
        loop {
            {
                let mut state = self.sched_state()?;
                let clock = match state.actors.get_mut(me) {
                    Some(actor) => {
                        tick(&mut actor.clock, me);
                        actor.clock.clone()
                    }
                    None => return unsupported("a task was lost"),
                };
                let Some(chan) = state.channels.get_mut(&number) else { return Ok(Value::Int(1)) };
                chan.waiting_sends.retain(|sent| sent.sender != me);
                if chan.left {
                    return unsupported(
                        "a send to the inbox of a task that's ended, which got in or not by time",
                    );
                }
                if chan.closed {
                    return Ok(Value::Int(1));
                }
                let sent = Sent { sender: me, value: message.clone(), clock };
                if chan.every {
                    chan.sent.push(sent);
                    if chan.sent.len() > chan.size {
                        return unsupported(
                            "a channel to every listener sent more than it keeps, which a listener could fall behind on or not",
                        );
                    }
                    return Ok(Value::Int(0));
                }
                if chan.queue.len() < chan.size {
                    chan.queue.push_back(sent);
                    return Ok(Value::Int(0));
                }
                if timeout == 0 {
                    return Ok(Value::Int(2));
                }
                chan.waiting_sends.push(sent);
            }
            if self.wait_for(Wait::Room(number), timeout > 0)? {
                let mut state = self.sched_state()?;
                if let Some(chan) = state.channels.get_mut(&number) {
                    chan.waiting_sends.retain(|sent| sent.sender != me);
                }
                return Ok(Value::Int(2));
            }
        }
    }

    /// `channel_receive`: the message, or `None`, and why: 0 a message, 1 closed and empty, 2
    /// timed out.
    pub(crate) fn channel_receive(&mut self, number: i64, timeout: i64) -> Run<(Value, i64)> {
        let me = self.actor;
        // Whatever else can go on goes first, so every message that could be the next one has
        // been sent or is waiting to be.
        let others_can_go = {
            let state = self.sched_state()?;
            state.actors.len() > 1
                && (1..state.actors.len()).any(|step| {
                    let other = (me + step) % state.actors.len();
                    state.can_go(other)
                })
        };
        // A receive given a timeout times out here if, once everything else has gone as far as
        // it can, there's still nothing to take.
        if others_can_go && self.wait_for(Wait::Message(number), timeout >= 0)? {
            return Ok((Value::None, 2));
        }
        loop {
            {
                let mut state = self.sched_state()?;
                let mut orders = self
                    .shared
                    .sched
                    .orders
                    .lock()
                    .map_or_else(|_| unsupported("the tasks were lost"), Ok)?;
                if let Some(taken) = take_message(&mut state, me, number, &mut orders)? {
                    return Ok(taken);
                }
                if timeout == 0 {
                    return Ok((Value::None, 2));
                }
            }
            if self.wait_for(Wait::Message(number), timeout > 0)? {
                return Ok((Value::None, 2));
            }
        }
    }
}

/// Takes the next message from `number`, a channel or a listener, for `me`: `None` while there's
/// none, `(None, 1)` once it's closed and empty. The taker's clock takes in the sender's.
fn take_message(state: &mut State, me: usize, number: i64, orders: &mut Orders) -> Run<Option<(Value, i64)>> {
    let taken = if let Some(listener) = state.listeners.get(&number) {
        let channel = listener.channel;
        let at = listener.at;
        let Some(chan) = state.channels.get(&channel) else { return Ok(Some((Value::None, 1))) };
        if at >= chan.sent.len() {
            return Ok(chan.closed.then_some((Value::None, 1)));
        }
        let unread: Vec<&Sent> = chan.sent.get(at..).unwrap_or_default().iter().collect();
        if could_come_next(&unread).len() > 1 {
            return unsupported("messages from several senders to a listener, whose order hangs on time");
        }
        let Some(first) = unread.first() else { return Ok(None) };
        let taken = (first.value.clone(), first.clock.clone());
        if let Some(listener) = state.listeners.get_mut(&number) {
            listener.at += 1;
        }
        taken
    } else {
        let Some(chan) = state.channels.get_mut(&number) else { return Ok(Some((Value::None, 1))) };
        chan.receivers.insert(me);
        if chan.receivers.len() > 1 {
            return unsupported(
                "two tasks taking messages from one channel, which gets which hanging on time",
            );
        }
        if chan.queue.is_empty() {
            return Ok(chan.closed.then_some((Value::None, 1)));
        }
        let queued: Vec<&Sent> = chan.queue.iter().collect();
        let mut next = could_come_next(&queued);
        if !chan.waiting_sends.is_empty() {
            let mut all: Vec<&Sent> = queued.clone();
            all.extend(chan.waiting_sends.iter());
            if could_come_next(&all).len() > next.len() {
                return unsupported(
                    "messages from several senders, one waiting for room, whose order hangs on time",
                );
            }
        }
        // Which sender's message comes next hangs on time: each is taken, on a run of its own.
        let chosen = if next.len() > 1 { orders.choose(next.len()) } else { 0 };
        let at = next.swap_remove(chosen.min(next.len().saturating_sub(1)));
        let Some(sent) = chan.queue.remove(at) else { return Ok(None) };
        (sent.value, sent.clock)
    };
    let (message, clock) = taken;
    if let Some(actor) = state.actors.get_mut(me) {
        merge(&mut actor.clock, &clock);
        tick(&mut actor.clock, me);
    }
    Ok(Some((message, 0)))
}

/// Of messages in the order they were sent, the places of those that could have been sent first
/// in another order, one for each value: each sender's first, which nothing sent before it
/// happened before. Two of the same value make the same run, so one stands for both.
fn could_come_next(sent: &[&Sent]) -> Vec<usize> {
    let mut found: Vec<usize> = Vec::new();
    let mut senders = BTreeSet::new();
    for (at, message) in sent.iter().enumerate() {
        if !senders.insert(message.sender) {
            continue;
        }
        let blocked = sent
            .get(..at)
            .unwrap_or_default()
            .iter()
            .any(|before| happened_before(&before.clock, &message.clock));
        let repeated = found
            .iter()
            .any(|&other| sent.get(other).is_some_and(|other| same(&other.value, &message.value)));
        if !blocked && !repeated {
            found.push(at);
        }
    }
    found
}

/// A task's thread: it waits for the baton, runs the task, and notes how it ended.
fn task_thread(
    shared: &Arc<super::Shared>,
    (number, inbox): (usize, Option<i64>),
    task: &Value,
    (task_expr, given): (&'static fennec_syntax::Expr, Vec<Value>),
) {
    let sched = &shared.sched;
    let Ok(mut state) = sched.state.lock() else { return };
    while state.running != number && !state.given_up {
        let Ok(next) = sched.turn.wait(state) else { return };
        state = next;
    }
    if state.given_up {
        return;
    }
    let scopes =
        state.actors.get_mut(number).map(|actor| std::mem::take(&mut actor.scopes)).unwrap_or_default();
    drop(state);
    if let Ok(mut held) = shared.scopes.lock() {
        *held = scopes;
    }
    let mut interp = Interp::new(Arc::clone(shared), None);
    interp.actor = number;
    let ended = interp.call_value(task, given, task_expr).map(|_| ());
    if matches!(ended, Err(Flow::Abandoned)) {
        return;
    }
    // Something the oracle doesn't model gives up the whole run at once, saying what it was,
    // rather than leaving what waits on this task waiting.
    if let Err(Flow::Unsupported(why)) = &ended {
        if let Ok(mut refused) = shared.refused.lock() {
            refused.get_or_insert_with(|| why.clone());
        }
        if let Ok(mut state) = sched.state.lock() {
            state.given_up = true;
        }
        sched.turn.notify_all();
        return;
    }
    let Ok(mut state) = sched.state.lock() else { return };
    if let Some(actor) = state.actors.get_mut(number) {
        actor.state = Doing::Ended;
        actor.ended = Some(ended);
    }
    // Its inbox closes as it ends: a send after that fails.
    if let Some(chan) = inbox.and_then(|inbox| state.channels.get_mut(&inbox)) {
        chan.closed = true;
        chan.left = true;
    }
    if let Ok(mut held) = shared.scopes.lock() {
        held.clear();
    }
    if !state.hand_on(number) {
        state.given_up = true;
        if let Ok(mut refused) = shared.refused.lock() {
            refused.get_or_insert_with(|| HANGS.to_owned());
        }
    }
    drop(state);
    sched.turn.notify_all();
}
