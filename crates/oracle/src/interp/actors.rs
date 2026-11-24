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
//! Two things about a run hang on time and are tried every way, each way on a run of its own
//! (`lib.rs`), and every run has to print the same and end the same:
//! - where several tasks send on one channel, which of their messages comes next. Before a
//!   receive, whatever else can go on goes first, so every message that could be next is there,
//!   and where the first from each sender differ, each is taken in turn;
//! - where a listener of a channel to every listener starts among messages sent beside it,
//!   neither before nor after: each place it could start. A timed wait that times out is taken to
//!   come after every other task got as far as it could, as the note on timeouts below says, its
//!   listeners started included: one that exists then started before what's sent after, so a send
//!   repeated until it's heard has an end.
//!
//! Anything else whose outcome hangs on time is refused:
//! - two tasks taking messages from one channel to one receiver at once, which gets which hanging
//!   on time: one after another, where each had heard of the other's last take, is in order;
//! - several senders' messages to a listener of a channel to every listener, or waiting for room,
//!   when the next ones differ;
//! - a channel to every listener letting go of its oldest message, to keep only as many as it
//!   keeps, where a listener read it or fell behind it in an order that hangs on time: a read of it
//!   that didn't happen before the send letting it go, or a receive that finds it gone and didn't
//!   happen after that send. Otherwise a listener that fell behind does on every run: its next
//!   receive fails as a crash does, as the runtime's does, or with `when_behind=.DropOldest` goes
//!   on from the oldest kept;
//! - a send waiting for room when its channel closes, which got in or not;
//! - waiting on a channel inside a `lock`;
//! - every task waiting on another, which hangs.
//!
//! A wait given a timeout times out only when nothing else can go on: whatever could still answer
//! it is taken to answer in time, however short its time. Only a wait of no time, `0ms`, is read
//! as looking once, as the runtime's does. It's refused unless what it finds is the same on every
//! run: nothing else holding an end of its channel could still send, take or close, a task waiting
//! for room behind a send beside its own counting as one that could, and a task that doesn't know
//! the channel was made holding none; what it finds was there before it; and it isn't in a
//! `parallel` round beside others, which could use any channel, one the round made included. A
//! listener of a channel to every listener read by two tasks at once is refused as one channel's
//! receivers are.
//!
//! The rounds of a `parallel` call run one after another here, on the thread that made the call,
//! but each is one of its own to the channels it uses, as it runs beside the others in the
//! compiled program: what one round sends doesn't come before what another sends, and two taking
//! from one channel is refused. What they all did comes before what the call's caller does next.
//! A round that receives, or waits for room, from a channel made outside it hands the rounds
//! after it to a thread of their own, given a copy of the names they read: they go first, as
//! everything else that can go on does, so their messages are there before it takes the next,
//! and it can wait for them.
//! What a task's block and the task itself reach, printing, the file system and `shared` values,
//! is held to what `tasks.rs` says, each seeing the launch from its own side.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use fennec_syntax::{Span, Stmt, WithName};

use super::calls::ArgVal;
use super::parallel::{ChannelUse, Handed, RoundsRan, Scope, Work};
use super::tasks::Launched;
use super::{Flow, Interp, Run, unsupported};
use crate::value::{Channel, Text, Value, same};

/// What a run where every task waits on another says, when the runtime wouldn't see it.
const HANGS: &str = "every task waits on another, so the program hangs";

/// How a run where every task waits on another ends when the runtime would see it: stopped, with
/// exit code 1, after what it printed, as `docs/language.md` says. The runtime's report says this
/// first, then who waits for what.
pub(crate) const DEADLOCK: &str = "a deadlock: every task is waiting for another, and nothing can wake them";

/// Each task's thread's stack.
const TASK_STACK: usize = 64 << 20;

/// The most tasks one run may launch.
const MOST_TASKS: usize = 1_000;

// The runtime reports a deadlock through at most 1,024 stuck ranges on one thread (`MOST_RANGES`
// in `crates/rt/src/embed/serve.rs`), and past that the program hangs. A run with more tasks
// than that could be said to stop where compiled code hangs, so the oracle refuses first.
const _: () = assert!(MOST_TASKS <= 1024);

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

    /// A send to a task's inbox, which the task could end before: one way, the send now, until a
    /// run finds the task ended with the message left (`may_race`). The script then takes the
    /// other, the task going on until it waits or ends before the send. Gives back where the
    /// choice is, and whether it's the other way.
    fn race_point(&mut self) -> (usize, bool) {
        let at = self.made.len();
        let task_first = self.script.get(at).copied() == Some(1);
        self.made.push((usize::from(task_first), if task_first { 2 } else { 1 }));
        (at, task_first)
    }

    /// Gives the send at `at` its other way, which a later run tries.
    fn may_race(&mut self, at: usize) {
        if let Some(choice) = self.made.get_mut(at) {
            choice.1 = choice.1.max(2);
        }
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
    /// The thread each `parallel` round's number belongs to: what a round learns is its thread's.
    thread_of: BTreeMap<usize, usize>,
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
            thread_of: BTreeMap::new(),
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
    /// Which one launched it, for a task.
    launcher: Option<usize>,
    /// The tasks it launched that failed or crashed while it's still in their blocks, which cuts
    /// short every wait it's in there, as `docs/language.md` says, until their blocks end.
    cut_short: Vec<usize>,
    /// For a thread running a `parallel` call's rounds handed on to it, the one that handed them
    /// on: they're in every block it's in.
    beside: Option<usize>,
}

impl Actor {
    fn ready(scopes: Vec<Scope>, clock: Clock) -> Actor {
        Actor {
            state: Doing::Ready,
            scopes,
            ended: None,
            timed_out: false,
            clock,
            launcher: None,
            cut_short: Vec::new(),
            beside: None,
        }
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

/// A message sent: who sent it, what, and the sender's clock as it sent it, and for one sent to a
/// task's inbox, where its send's choice is (`Orders::race_point`).
struct Sent {
    sender: usize,
    value: Value,
    clock: Clock,
    race: Option<usize>,
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
    /// For this task to wait or end: a send to its inbox taking the way where the task goes on
    /// first.
    Settled(usize),
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
    /// For a channel to every listener, the place in `sent` of the oldest message it still keeps:
    /// it keeps the last `size`, letting the oldest go as another is sent.
    first: usize,
    /// For each message let go, by its place in `sent`, the clock of the send that let it go.
    dropped: Vec<Clock>,
    /// The clock of the close, once it's closed.
    closed_at: Option<Clock>,
    /// Its maker's clock as it made it: a task that doesn't know of that holds no end of it.
    made: Clock,
    /// Who's tried to take a message from it, each with what it knew when it last tried, or once
    /// it had taken one, and whether it ever has.
    receivers: BTreeMap<usize, (Clock, bool)>,
    /// Each send waiting for room.
    waiting_sends: Vec<Sent>,
    /// Whether it's a task's inbox the task has left by ending.
    left: bool,
    /// The task whose inbox it is.
    task: Option<usize>,
}

/// A listener to a channel to every listener: the channel, how many of its messages it's read,
/// where it started, whether, fallen behind, it goes on from the oldest kept rather than failing,
/// and the clock of its read of each message the channel still keeps.
struct Listener {
    channel: i64,
    at: usize,
    start: usize,
    drop_oldest: bool,
    reads: BTreeMap<usize, Clock>,
    /// Who's tried to take a message from it, as a channel's receivers are kept.
    receivers: BTreeMap<usize, (Clock, bool)>,
    /// Its maker's clock as it started listening: a message sent beside that, neither before nor
    /// after it, it hears or not by time.
    started: Clock,
    /// Whether a timed wait has timed out since it started: one does only once nothing else can go
    /// on, so it's taken to have started before whatever's sent after.
    settled: bool,
}

impl State {
    /// What a run says when nothing can go on: `DEADLOCK` where the runtime finds one, as every
    /// wait is a receive from a channel to one receiver, a send waiting for room or a block's end
    /// waiting for its task, a `parallel` call's rounds' included, and `HANGS` otherwise. A
    /// listener's wait on a channel to every listener isn't caught by the runtime yet, so a run
    /// stuck in one is refused rather than said to stop.
    fn stuck_said(&self) -> &'static str {
        let caught = self.actors.iter().all(|actor| match actor.state {
            Doing::Ended | Doing::Waiting(Wait::Room(_) | Wait::Task(_), false) => true,
            Doing::Waiting(Wait::Message(number), false) => {
                !self.listeners.contains_key(&number)
                    && self.channels.get(&number).is_some_and(|chan| !chan.every)
            }
            _ => false,
        });
        if caught { DEADLOCK } else { HANGS }
    }

    /// After a send on the channel to every listener numbered `number`: a listener that hasn't
    /// read yet, whose start this send is beside, neither before nor after it, could have started
    /// after it on another run, and not heard it, nor anything it hasn't read before it. Each way
    /// is a run of its own, the first where it heard it. One that existed when a timed wait timed
    /// out is taken to have started first: a timeout comes only once nothing else can go on, so a
    /// send repeated until it's heard, as a handshake does, has an end.
    fn heard_or_not(&mut self, number: i64, orders: &mut Orders) {
        let Some(chan) = self.channels.get(&number) else { return };
        let Some(this) = chan.sent.len().checked_sub(1) else { return };
        let beside = |listener: &Listener| {
            let unread = chan.sent.get(listener.start..=this).unwrap_or_default();
            listener.channel == number
                && !listener.settled
                && listener.at == listener.start
                && listener.start <= this
                && unread.iter().all(|sent| !happened_before(&listener.started, &sent.clock))
        };
        let racing: Vec<i64> =
            self.listeners.iter().filter(|(_, listener)| beside(listener)).map(|(at, _)| *at).collect();
        for at in racing {
            if orders.choose(2) == 1
                && let Some(listener) = self.listeners.get_mut(&at)
            {
                listener.start = this + 1;
                listener.at = this + 1;
            }
        }
    }

    /// After a send, stamped `sent`, on the channel to every listener numbered `number`: it keeps
    /// the last `size`, so the oldest goes. A listener that read it did so before it went on every
    /// run only if its read happened before this send; otherwise it could have fallen behind
    /// instead, by time. One that hasn't read it is looked at as it next receives.
    fn kept_for_every(&mut self, number: i64, sent: &Clock) -> Run<Value> {
        let Some(chan) = self.channels.get_mut(&number) else { return Ok(Value::Int(1)) };
        if chan.sent.len() - chan.first <= chan.size {
            return Ok(Value::Int(0));
        }
        let gone = chan.first;
        chan.first += 1;
        chan.dropped.push(sent.clone());
        for listener in self.listeners.values_mut() {
            if listener.channel != number || listener.start > gone || listener.at <= gone {
                continue;
            }
            let read = listener.reads.remove(&gone);
            if !read.is_some_and(|read| happened_before(&read, sent)) {
                return unsupported(
                    "a channel to every listener let go of a message a listener read, the read not before the send, so it read it or fell behind by time",
                );
            }
        }
        Ok(Value::Int(0))
    }

    /// Whether any but those in `apart` can go on now, or is waiting with a timeout, which could end
    /// whenever it does.
    ///
    /// For the channel numbered `of`, that's only while another task, going or not, holds an end
    /// of it or could come to, since one going could wake one blocked that does. A task that
    /// doesn't know the channel was made holds no end of it, nor can come to but through what it
    /// could come to know, as `could_come_to_know` says. An end reaches a task only by its launch
    /// or a message, which both carry what their sender knew, or a `shared` value, which two tasks
    /// can't both reach while one changes it. Taking from a channel takes an end of it too.
    fn others_could_go(&self, apart: &[usize], of: Option<i64>) -> bool {
        let others: Vec<usize> = (0..self.actors.len()).filter(|other| !apart.contains(other)).collect();
        let could_go = |other: usize| {
            self.can_go(other)
                || self.actors.get(other).is_some_and(|found| matches!(found.state, Doing::Waiting(_, true)))
                || self.behind_a_racing_send(other)
        };
        // Nothing going is the common case, and settles it without working out who knows what.
        if !others.iter().any(|&other| could_go(other)) {
            return false;
        }
        let Some(made) = of.and_then(|number| self.channels.get(&number)).map(|chan| &chan.made) else {
            return true;
        };
        // What each channel's waiting messages tell, together, worked out once for every task.
        let told: Vec<(&Clock, Clock)> = self
            .channels
            .values()
            .map(|chan| {
                let every = chan.sent.get(chan.first..).unwrap_or_default();
                let mut told = Clock::new();
                for sent in chan.queue.iter().chain(&chan.waiting_sends).chain(every) {
                    merge(&mut told, &sent.clock);
                }
                (&chan.made, told)
            })
            .collect();
        others.iter().any(|&other| {
            self.actors.get(other).is_some_and(|found| !matches!(found.state, Doing::Ended))
                && happened_before(made, &self.could_come_to_know(other, &told))
        })
    }

    /// What the thread numbered `actor` could come to know: what it knows, what the `parallel`
    /// rounds it runs know, and those of each thread its rounds were handed on to, which it takes
    /// in at their call's end; and then what each message waiting on a channel it knows of tells,
    /// until that gives nothing more, as an end handed on through one channel can reach another.
    fn could_come_to_know(&self, actor: usize, told: &[(&Clock, Clock)]) -> Clock {
        let mut threads = vec![actor];
        let mut more = true;
        while more {
            more = false;
            for (at, found) in self.actors.iter().enumerate() {
                if found.beside.is_some_and(|from| threads.contains(&from)) && !threads.contains(&at) {
                    threads.push(at);
                    more = true;
                }
            }
        }
        let mut known = Clock::new();
        for thread in &threads {
            if let Some(found) = self.actors.get(*thread) {
                merge(&mut known, &found.clock);
            }
        }
        for (round, thread) in &self.thread_of {
            if threads.contains(thread)
                && let Some(found) = self.actors.get(*round)
            {
                merge(&mut known, &found.clock);
            }
        }
        loop {
            let mut grew = known.clone();
            for (made, by_messages) in told {
                if happened_before(made, &known) {
                    merge(&mut grew, by_messages);
                }
            }
            if happened_before(&grew, &known) {
                return known;
            }
            known = grew;
        }
    }

    /// Whether `actor` waits for room on a channel where a send waits behind a message another
    /// sent beside it: on another run that send got in first, and its sender went on. A send in a
    /// `parallel` round is stamped as the round's, not the thread's that waits, so any waiting
    /// send there counts.
    fn behind_a_racing_send(&self, actor: usize) -> bool {
        let Some(Actor { state: Doing::Waiting(Wait::Room(number), _), .. }) = self.actors.get(actor) else {
            return false;
        };
        let Some(chan) = self.channels.get(number) else { return false };
        chan.waiting_sends.iter().any(|waiting| {
            chan.queue.iter().any(|queued| {
                queued.sender != waiting.sender && !happened_before(&queued.clock, &waiting.clock)
            })
        })
    }

    /// Whether a send of no time on `number`, stamped `sent`, finds room or not the same on every
    /// run: nothing but those in `apart` could send or take now, and each take it could find room
    /// by, and each message filling it, happened before it.
    fn room_now_is_fixed(&self, number: i64, sent: &Clock, apart: &[usize]) -> bool {
        let Some(chan) = self.channels.get(&number) else { return true };
        let took_beside = chan.receivers.values().any(|(tried, took)| *took && !happened_before(tried, sent));
        let full = chan.queue.len() >= chan.size;
        let filled_beside = full && chan.queue.iter().any(|queued| !happened_before(&queued.clock, sent));
        !took_beside && !filled_beside && !self.others_could_go(apart, Some(number))
    }

    /// Whether what a receive from `number`, a channel or a listener, could find now was there
    /// before `now` on every run: each message that could be the next one, or else the close.
    fn there_before(&self, number: i64, now: &Clock) -> bool {
        let (unread, chan): (Vec<&Sent>, &Chan) = if let Some(listener) = self.listeners.get(&number) {
            let Some(chan) = self.channels.get(&listener.channel) else { return true };
            (chan.sent.get(listener.at.max(chan.first)..).unwrap_or_default().iter().collect(), chan)
        } else {
            let Some(chan) = self.channels.get(&number) else { return true };
            (chan.queue.iter().collect(), chan)
        };
        if unread.is_empty() {
            return !chan.closed
                || chan.closed_at.as_ref().is_some_and(|closed| happened_before(closed, now));
        }
        could_come_next(&unread)
            .into_iter()
            .all(|at| unread.get(at).is_some_and(|sent| happened_before(&sent.clock, now)))
    }

    /// Whether the one numbered `actor` can go on.
    fn can_go(&self, actor: usize) -> bool {
        let Some(found) = self.actors.get(actor) else { return false };
        match found.state {
            Doing::Ready => true,
            Doing::Ended => false,
            Doing::Waiting(Wait::Task(task), _) => {
                self.actors.get(task).is_none_or(|task| matches!(task.state, Doing::Ended))
            }
            Doing::Waiting(Wait::Settled(task), _) => {
                self.actors.get(task).is_none_or(|task| !matches!(task.state, Doing::Ready))
            }
            // A wait in the block of a task that failed or crashed is cut short.
            Doing::Waiting(Wait::Room(_) | Wait::Message(_), _) if !found.cut_short.is_empty() => true,
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
            // Every other task has got as far as it could, its listeners started included.
            for listener in self.listeners.values_mut() {
                listener.settled = true;
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

/// After a send, stamped `sent`, on the channel to every listener numbered `number`: which of its
/// listeners started beside it heard it, each way tried, and what it lets go.
fn sent_to_every(state: &mut State, orders: &Mutex<Orders>, number: i64, sent: &Clock) -> Run<Value> {
    let Ok(mut orders) = orders.lock() else {
        return unsupported("the orders of messages were lost");
    };
    state.heard_or_not(number, &mut orders);
    drop(orders);
    state.kept_for_every(number, sent)
}

/// Who runs, and the channels they share, held.
fn lock_state(shared: &super::Shared) -> Run<MutexGuard<'_, State>> {
    shared.sched.state.lock().map_or_else(|_| unsupported("the tasks were lost"), Ok)
}

/// The number the channels know whoever runs now by: the innermost `parallel` round the thread
/// `actor` runs inside, `rounds_as`, or else the thread itself. A round is one of its own, which
/// starts knowing what whoever made its call knew, and is given its number the first time it
/// uses a channel, as each round it's inside is. Its rounds' numbers are never scheduled: what
/// waits is the thread.
fn who(rounds_as: &mut [Option<usize>], actor: usize, state: &mut State) -> Run<usize> {
    let mut parent = actor;
    for round in rounds_as.iter_mut() {
        if let Some(number) = *round {
            parent = number;
            continue;
        }
        if state.actors.len() >= MOST_TASKS {
            return unsupported("ran too long: too many tasks and `parallel` rounds using channels");
        }
        let number = state.actors.len();
        let mut clock = state.actors.get(parent).map(|actor| actor.clock.clone()).unwrap_or_default();
        tick(&mut clock, number);
        state.actors.push(Actor {
            state: Doing::Ended,
            ended: Some(Ok(())),
            ..Actor::ready(Vec::new(), clock)
        });
        state.thread_of.insert(number, actor);
        *round = Some(number);
        parent = number;
    }
    Ok(parent)
}

impl Interp {
    fn sched_state(&self) -> Run<MutexGuard<'_, State>> {
        lock_state(&self.shared)
    }

    /// Hands the rounds after the one running now, of each `parallel` call this thread runs
    /// whose rounds can be handed on and that `channel` is from outside of, to a thread of their
    /// own, as they run beside it in the compiled program: so whatever they'd send on a channel
    /// is there before a receive takes the next message, and a round can wait on one after it.
    /// The thread is given a copy of the names the call's rounds read and of what they run
    /// inside, and runs them from the call's `Work`.
    pub(crate) fn hand_on_rounds(&mut self, channel: i64) -> Run<()> {
        for at in 0..self.handing.len() {
            let Some(handing) = self.handing.get(at) else { break };
            if handing.handed.is_some() || handing.maybe || handing.round + 1 >= handing.count {
                continue;
            }
            let Some(work) = handing.work.clone() else { continue };
            let (from, count, mark, scope, level) =
                (handing.round + 1, handing.count, handing.mark, handing.scope, handing.level);
            let mut frames: Vec<super::Frame> =
                self.frames.get(..handing.frames).unwrap_or_default().to_vec();
            if let Some(frame) = frames.get_mut(mark.0) {
                frame.vars.truncate(mark.1);
            }
            let scopes = {
                let Ok(mut held) = self.shared.scopes.lock() else { return unsupported("a launch was lost") };
                let Some(Scope::Rounds(call)) = held.get_mut(scope) else {
                    return unsupported("a `parallel` call was lost");
                };
                // A channel made in the round running now is its own: no other round reaches it.
                if channel >= call.first_channel {
                    continue;
                }
                call.handed = true;
                let mut scopes: Vec<Scope> =
                    held.get(..=scope).unwrap_or_default().iter().map(Scope::copy).collect();
                if let Some(Scope::Rounds(call)) = scopes.last_mut() {
                    call.handed = false;
                }
                scopes
            };
            let done = Arc::new(Mutex::new(None));
            let slot = {
                let mut state = lock_state(&self.shared)?;
                // The rounds handed on start knowing what the call's caller knew, as each round does.
                let caller =
                    who(self.rounds_as.get_mut(..level).unwrap_or_default(), self.actor, &mut state)?;
                if state.actors.len() >= MOST_TASKS {
                    return unsupported("ran too long: too many tasks");
                }
                let clock = state.actors.get(caller).map(|actor| actor.clock.clone()).unwrap_or_default();
                let cut_short =
                    state.actors.get(self.actor).map(|actor| actor.cut_short.clone()).unwrap_or_default();
                let slot = state.actors.len();
                state.actors.push(Actor {
                    beside: Some(self.actor),
                    cut_short,
                    ..Actor::ready(scopes, clock)
                });
                slot
            };
            let rest = Rest {
                work,
                from,
                count,
                frames,
                cur: mark.0,
                next_id: self.next_id,
                rounds_as: self.rounds_as.get(..level).unwrap_or_default().to_vec(),
                scope,
                done: Arc::clone(&done),
            };
            let shared = Arc::clone(&self.shared);
            let thread = std::thread::Builder::new().stack_size(TASK_STACK).spawn(move || {
                super::walks_within(TASK_STACK);
                rest_thread(&shared, slot, rest);
            });
            if thread.is_err() {
                self.give_up();
                return unsupported("a thread for a `parallel` call's rounds couldn't start");
            }
            if let Some(handing) = self.handing.get_mut(at) {
                handing.handed = Some(Handed { slot, done });
            }
        }
        Ok(())
    }

    /// Waits for the thread rounds were handed on to to end, and gives back what they made.
    pub(crate) fn rest_ended(&mut self, handed: &Handed) -> Run<RoundsRan> {
        loop {
            {
                let state = lock_state(&self.shared)?;
                if state.actors.get(handed.slot).is_none_or(|actor| matches!(actor.state, Doing::Ended)) {
                    break;
                }
            }
            self.wait_for(Wait::Task(handed.slot), false)?;
        }
        let done = handed.done.lock().ok().and_then(|mut done| done.take());
        match done {
            Some(done) => Ok(done),
            None => self.given_up_why(),
        }
    }

    /// Notes the `shared` values a message holds, and those they hold, as sent: no `parallel`
    /// round has them to itself after that.
    fn note_sent_cells(&self, message: &Value) -> Run<()> {
        let mut found = Vec::new();
        let mut waiting: Vec<Value> = vec![message.clone()];
        while let Some(held) = waiting.pop() {
            match held {
                Value::List(items) | Value::Tuple(items) | Value::Set(items) => waiting.extend(items),
                Value::Dict(pairs) => {
                    for (key, value) in pairs {
                        waiting.push(key);
                        waiting.push(value);
                    }
                }
                Value::Instance(instance) => {
                    waiting.extend(instance.into_inner().fields.into_iter().map(|(_, field)| field));
                }
                Value::Table(table) => waiting.extend(table.live().map(|(_, item)| item.clone())),
                Value::Shared(cell) => {
                    if found.contains(&cell.id) {
                        continue;
                    }
                    found.push(cell.id);
                    // What it holds, unless a `lock` holds it now, which this can't look inside.
                    let Ok(inside) = cell.held.try_lock() else {
                        return unsupported("a message holding a `shared` value a `lock` holds now");
                    };
                    waiting.push(inside.clone());
                }
                _ => {}
            }
        }
        if !found.is_empty()
            && let Ok(mut sent) = self.shared.sent_cells.lock()
        {
            sent.extend(found);
        }
        Ok(())
    }

    /// The number the next channel made gets, so one made since is told apart.
    pub(crate) fn channels_made(&self) -> Run<i64> {
        Ok(self.sched_state()?.next_number)
    }

    /// Takes in what the rounds of a `parallel` call that used channels did, by their numbers,
    /// once they've all ended: all of it happened before what its caller does next.
    pub(crate) fn rounds_ended(&mut self, rounds: &[usize]) -> Run<()> {
        if rounds.is_empty() {
            return Ok(());
        }
        let mut state = lock_state(&self.shared)?;
        let caller = who(&mut self.rounds_as, self.actor, &mut state)?;
        let clocks: Vec<Clock> = rounds
            .iter()
            .filter_map(|&round| state.actors.get(round).map(|round| round.clock.clone()))
            .collect();
        if let Some(actor) = state.actors.get_mut(caller) {
            for clock in &clocks {
                merge(&mut actor.clock, clock);
            }
            tick(&mut actor.clock, caller);
        }
        Ok(())
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
            let said = state.stuck_said();
            if let Ok(mut refused) = self.shared.refused.lock() {
                refused.get_or_insert_with(|| said.to_owned());
            }
            state.given_up = true;
            drop(state);
            self.shared.sched.turn.notify_all();
            return self.given_up_why();
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
    /// Whether a task runs beside this one, which may do something while this waits.
    pub(crate) fn tasks_beside(&self) -> Run<bool> {
        Ok(self.sched_state()?.actors.len() > 1)
    }

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
        if let Some(channel) = channel
            && let Some(chan) = self.sched_state()?.channels.get_mut(&channel)
        {
            chan.task = Some(number);
        }
        self.push_scope(Scope::Launch(launched))?;
        let mark = self.mark();
        let lent = channel.map_or(Value::Int(0), |number| end(true, number));
        let ran = self.lend_names(names, lent, span).and_then(|()| self.stmts(body));
        self.restore(mark);
        self.pop_scope();
        let pending = match ran {
            Ok(()) => None,
            Err(flow @ (Flow::Return(_) | Flow::Break | Flow::Continue)) => Some(flow),
            // A send to the inbox of a task that's failed or crashed fails with `Closed`, and the
            // block ends with the task's failure in its place (#1724).
            Err(flow @ Flow::Fail(..)) if is_closed(&flow) && self.task_stopped(number)? => Some(flow),
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
        {
            let mut state = self.sched_state()?;
            if let Some(actor) = state.actors.get_mut(self.actor) {
                actor.cut_short.retain(|&task| task != number);
            }
        }
        if let Some(channel) = channel {
            // A task that ends with messages left closes its inbox, and whether the block's sends
            // of them came after that and failed with `Closed` hangs on time. One the task knew of
            // as it ended was sent before in every order. For each other, a later run tries the
            // task going on first, and the runs have to end the same.
            let mut state = self.sched_state()?;
            let unread = state.channels.remove(&channel).map(|chan| chan.queue).unwrap_or_default();
            let ended_at = state.actors.get(number).map(|task| task.clock.clone()).unwrap_or_default();
            drop(state);
            let mut races = Vec::new();
            for sent in unread.iter().filter(|sent| !happened_before(&sent.clock, &ended_at)) {
                match sent.race {
                    Some(at) => races.push(at),
                    None => return unsupported("a launched task that ends with messages left in its inbox"),
                }
            }
            if !races.is_empty() {
                let Ok(mut orders) = self.shared.sched.orders.lock() else {
                    return unsupported("the orders of messages were lost");
                };
                for at in races {
                    orders.may_race(at);
                }
            }
        }
        match ended {
            Ok(()) => pending.map_or(Ok(()), Err),
            Err(Flow::Fail(error, _)) => Err(Flow::Fail(error, span)),
            Err(Flow::Crash(..)) => {
                let why = Value::Str(Text::from("the task crashed".to_owned()));
                Err(Flow::Fail(Value::instance("Crashed", None, vec![("why".to_owned(), why)]), span))
            }
            Err(Flow::Return(_) | Flow::Break | Flow::Continue) => {
                unsupported("a `return`, `break` or `continue` leaving a launched task")
            }
            Err(other) => Err(other),
        }
    }

    /// Whether the task numbered `number` has ended by failing or crashing.
    fn task_stopped(&self, number: usize) -> Run<bool> {
        let state = self.sched_state()?;
        Ok(state.actors.get(number).is_some_and(|task| {
            matches!(task.state, Doing::Ended)
                && matches!(task.ended, Some(Err(Flow::Fail(..) | Flow::Crash(..))))
        }))
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
            state.actors.push(Actor { launcher: Some(self.actor), ..Actor::ready(task_scopes, clock) });
            number
        };
        let shared = Arc::clone(&self.shared);
        let thread = std::thread::Builder::new().stack_size(TASK_STACK).spawn(move || {
            super::walks_within(TASK_STACK);
            task_thread(&shared, (number, inbox), &task, (task_expr, given));
        });
        if thread.is_err() {
            self.give_up();
            return unsupported("a task's thread couldn't start");
        }
        Ok((number, launched))
    }

    /// A new channel of `size`, to every listener when `every`, by its number.
    fn new_channel(&mut self, size: usize, every: bool) -> Run<i64> {
        if size == 0 {
            return unsupported("a channel of no size");
        }
        let mut state = lock_state(&self.shared)?;
        let maker = who(&mut self.rounds_as, self.actor, &mut state)?;
        let made = match state.actors.get_mut(maker) {
            Some(actor) => {
                tick(&mut actor.clock, maker);
                actor.clock.clone()
            }
            None => return unsupported("a task was lost"),
        };
        let number = state.next_number;
        state.next_number += 1;
        let chan = Chan {
            size,
            every,
            closed: false,
            queue: VecDeque::new(),
            sent: Vec::new(),
            first: 0,
            dropped: Vec::new(),
            closed_at: None,
            made,
            receivers: BTreeMap::new(),
            waiting_sends: Vec::new(),
            left: false,
            task: None,
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
            ("listen", [Value::Int(number), Value::Bool(drop_oldest)]) => self.listen(*number, *drop_oldest),
            ("send", [Value::Int(number), message, Value::Int(timeout)]) => {
                self.charge(message)?;
                self.note_sent_cells(message)?;
                self.channel_send(*number, message, *timeout)
            }
            ("receive", [Value::Int(number), Value::Int(timeout)]) => {
                let (got, why) = self.channel_receive(*number, *timeout)?;
                let Some(place) = why_place else {
                    return unsupported("`channel_receive` with no place for why");
                };
                self.write_place(&place, Value::Int(why), span)?;
                // A `T?`, so a `None` sent on a `channel[int?]` is kept apart from the channel
                // closing.
                Ok(if why == 0 { Value::found(got) } else { got })
            }
            ("close", [Value::Int(number)]) => {
                self.channel_in_rounds(*number, ChannelUse::Close)?;
                let mut state = lock_state(&self.shared)?;
                let closer = who(&mut self.rounds_as, self.actor, &mut state)?;
                let closed_at = state.actors.get_mut(closer).map(|actor| {
                    tick(&mut actor.clock, closer);
                    actor.clock.clone()
                });
                let Some(chan) = state.channels.get_mut(number) else { return Ok(Value::Unit) };
                chan.closed_at = closed_at;
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

    /// `channel_listen`: a listener of the channel to every listener numbered `number`, or the
    /// channel's own number for one to one receiver. It starts after every message sent before
    /// it, and hears every one sent after it; one sent beside it, neither, it heard on some run and
    /// not on another, so each place it could start among those is a run of its own.
    fn listen(&mut self, number: i64, drop_oldest: bool) -> Run<Value> {
        let mut state = lock_state(&self.shared)?;
        let maker = who(&mut self.rounds_as, self.actor, &mut state)?;
        let started = match state.actors.get_mut(maker) {
            Some(actor) => {
                tick(&mut actor.clock, maker);
                actor.clock.clone()
            }
            None => return unsupported("a task was lost"),
        };
        let Some(chan) = state.channels.get(&number) else {
            return unsupported("a channel let go of");
        };
        if !chan.every {
            return Ok(Value::Int(number));
        }
        // It starts after every message sent before it, and hears every one sent after it.
        // One sent beside it, neither, it heard on some run and not on another: each place
        // it could start among those is a run of its own, the first where it is here.
        let sent = chan.sent.len();
        let surely_before =
            chan.sent.iter().rposition(|sent| happened_before(&sent.clock, &started)).map_or(0, |at| at + 1);
        let at = if surely_before < sent {
            let Ok(mut orders) = self.shared.sched.orders.lock() else {
                return unsupported("the orders of messages were lost");
            };
            sent - orders.choose(sent - surely_before + 1)
        } else {
            sent
        };
        let listener = state.next_number;
        state.next_number += 1;
        let (reads, receivers) = (BTreeMap::new(), BTreeMap::new());
        let made = Listener {
            channel: number,
            at,
            start: at,
            drop_oldest,
            reads,
            receivers,
            started,
            settled: false,
        };
        state.listeners.insert(listener, made);
        Ok(Value::Int(listener))
    }

    /// `channel_send`: 0 sent, 1 closed, 2 timed out.
    pub(crate) fn channel_send(&mut self, number: i64, message: &Value, timeout: i64) -> Run<Value> {
        self.channel_in_rounds(number, ChannelUse::Send)?;
        let beside_rounds = timeout == 0 && self.beside_other_rounds()?;
        let me = self.actor;
        // A send to a task's inbox, which the task could end before, without taking it.
        let inbox_of = self.sched_state()?.channels.get(&number).and_then(|chan| chan.task);
        let (race, task_first) = match inbox_of {
            Some(task) => {
                let Ok(mut orders) = self.shared.sched.orders.lock() else {
                    return unsupported("the orders of messages were lost");
                };
                let (at, task_first) = orders.race_point();
                drop(orders);
                if task_first {
                    let _ = self.wait_for(Wait::Settled(task), false)?;
                }
                (Some(at), task_first)
            }
            None => (None, false),
        };
        loop {
            let sender = {
                let mut state = lock_state(&self.shared)?;
                let sender = who(&mut self.rounds_as, me, &mut state)?;
                let clock = match state.actors.get_mut(sender) {
                    Some(actor) => {
                        tick(&mut actor.clock, sender);
                        actor.clock.clone()
                    }
                    None => return unsupported("a task was lost"),
                };
                if cut_short(&mut state, me, sender) {
                    if let Some(chan) = state.channels.get_mut(&number) {
                        chan.waiting_sends.retain(|sent| sent.sender != sender);
                    }
                    return Ok(Value::Int(1));
                }
                let Some(chan) = state.channels.get_mut(&number) else { return Ok(Value::Int(1)) };
                chan.waiting_sends.retain(|sent| sent.sender != sender);
                // The task ended before this send: on a run trying that way, the send fails, as a
                // send to a closed channel does. On another, whether it got in hangs on time.
                if chan.left {
                    if task_first {
                        return Ok(Value::Int(1));
                    }
                    return unsupported(
                        "a send to the inbox of a task that's ended, which got in or not by time",
                    );
                }
                if chan.closed {
                    return Ok(Value::Int(1));
                }
                let sent = Sent { sender, value: message.clone(), clock: clock.clone(), race };
                if chan.every {
                    chan.sent.push(sent);
                    return sent_to_every(&mut state, &self.shared.sched.orders, number, &clock);
                }
                // A send of no time in a `parallel` round beside others finds room or not by time.
                if timeout == 0 && (beside_rounds || !state.room_now_is_fixed(number, &clock, &[me, sender]))
                {
                    return unsupported("a send of no time, which finds room or not by time");
                }
                let Some(chan) = state.channels.get_mut(&number) else { return Ok(Value::Int(1)) };
                if chan.queue.len() < chan.size {
                    chan.queue.push_back(sent);
                    return Ok(Value::Int(0));
                }
                if timeout == 0 {
                    return Ok(Value::Int(2));
                }
                chan.waiting_sends.push(sent);
                sender
            };
            // The rounds after this one run beside it, so they can make room for it.
            self.hand_on_rounds(number)?;
            if self.wait_for(Wait::Room(number), timeout > 0)? {
                let mut state = self.sched_state()?;
                if let Some(chan) = state.channels.get_mut(&number) {
                    chan.waiting_sends.retain(|sent| sent.sender != sender);
                }
                return Ok(Value::Int(2));
            }
        }
    }

    /// A receive of no time: what's there now, as the runtime's looks once. It's the same on every
    /// run only when nothing else could still send, take or close, what it could find, a message or
    /// the close, was there before it, and it isn't in a `parallel` round beside others.
    fn receive_now(&mut self, number: i64, beside_rounds: bool) -> Run<(Value, i64)> {
        let me = self.actor;
        let mut state = lock_state(&self.shared)?;
        let receiver = who(&mut self.rounds_as, me, &mut state)?;
        let mut orders =
            self.shared.sched.orders.lock().map_or_else(|_| unsupported("the tasks were lost"), Ok)?;
        if cut_short(&mut state, me, receiver) {
            return Ok((Value::None, 1));
        }
        let now = state.actors.get(receiver).map(|actor| actor.clock.clone()).unwrap_or_default();
        // Another `parallel` round runs beside it, which could send first or not.
        let channel = state.listeners.get(&number).map_or(number, |listener| listener.channel);
        if beside_rounds
            || state.others_could_go(&[me, receiver], Some(channel))
            || !state.there_before(number, &now)
        {
            return unsupported("a receive of no time, which finds a message or not by time");
        }
        Ok(take_message(&mut state, receiver, number, &mut orders)?.unwrap_or((Value::None, 2)))
    }

    /// `channel_receive`: the message, or `None`, and why: 0 a message, 1 closed and empty, 2
    /// timed out.
    pub(crate) fn channel_receive(&mut self, number: i64, timeout: i64) -> Run<(Value, i64)> {
        let channel = self.sched_state()?.listeners.get(&number).map_or(number, |listener| listener.channel);
        // The rounds after this one run beside it, so their messages can be the next.
        if timeout == 0 {
            self.channel_in_rounds(channel, ChannelUse::Receive)?;
            let beside_rounds = self.beside_other_rounds()?;
            return self.receive_now(number, beside_rounds);
        }
        self.hand_on_rounds(channel)?;
        self.channel_in_rounds(channel, ChannelUse::Receive)?;
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
                let mut state = lock_state(&self.shared)?;
                let receiver = who(&mut self.rounds_as, me, &mut state)?;
                let mut orders = self
                    .shared
                    .sched
                    .orders
                    .lock()
                    .map_or_else(|_| unsupported("the tasks were lost"), Ok)?;
                if cut_short(&mut state, me, receiver) {
                    return Ok((Value::None, 1));
                }
                if let Some(taken) = take_message(&mut state, receiver, number, &mut orders)? {
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

/// Whether `me`'s waits are cut short, by a task it launched failing or crashing while it's in
/// that task's block, as `docs/language.md` says: a cut wait ends as a closed channel's does, after
/// the task's end, so `who`, `me` or the round it runs, takes in what the task had done.
fn cut_short(state: &mut State, me: usize, who: usize) -> bool {
    let stopped: Vec<usize> = state.actors.get(me).map(|actor| actor.cut_short.clone()).unwrap_or_default();
    if stopped.is_empty() {
        return false;
    }
    let clocks: Vec<Clock> =
        stopped.iter().filter_map(|&task| state.actors.get(task).map(|task| task.clock.clone())).collect();
    if let Some(actor) = state.actors.get_mut(who) {
        for clock in &clocks {
            merge(&mut actor.clock, clock);
        }
        tick(&mut actor.clock, who);
    }
    true
}

/// Two tasks taking from the listener numbered `number` at once, as for a channel to one
/// receiver, where neither had heard of the other's last take: which gets which hangs on time.
/// Notes `receiver`'s try.
fn two_at_once(state: &mut State, receiver: usize, number: i64) -> Run<()> {
    let Some(listener) = state.listeners.get(&number) else { return Ok(()) };
    let Some(chan) = state.channels.get(&listener.channel) else { return Ok(()) };
    let now = state.actors.get(receiver).map(|actor| actor.clock.clone()).unwrap_or_default();
    let takes = listener.at < chan.sent.len();
    let races = listener.receivers.iter().any(|(other, (tried, took))| {
        *other != receiver && (takes || *took) && !happened_before(tried, &now)
    });
    if races {
        return unsupported(
            "two tasks taking messages from one listener of a channel to every listener, which gets which hanging on time",
        );
    }
    let took = listener.receivers.get(&receiver).is_some_and(|(_, took)| *took);
    if let Some(listener) = state.listeners.get_mut(&number) {
        listener.receivers.insert(receiver, (now, took));
    }
    Ok(())
}

/// Where the listener numbered `number` reads next. One whose next message has gone fails, as a
/// crash does, or goes on from the oldest kept, as the runtime's does. It fell behind on every run
/// only if each send that let what it missed go happened before this receive, by `receiver`.
fn fallen_behind(state: &mut State, receiver: usize, number: i64) -> Run<usize> {
    let Some(listener) = state.listeners.get(&number) else { return Ok(0) };
    let (at, drop_oldest) = (listener.at, listener.drop_oldest);
    let Some(chan) = state.channels.get(&listener.channel) else { return Ok(at) };
    if at >= chan.first {
        return Ok(at);
    }
    let now = state.actors.get(receiver).map(|actor| actor.clock.clone()).unwrap_or_default();
    let missed_drops = chan.dropped.get(at..chan.first).unwrap_or_default();
    if !missed_drops.iter().all(|dropped| happened_before(dropped, &now)) {
        return unsupported(
            "a listener of a channel to every listener found a message gone that it could have read first, by time",
        );
    }
    if !drop_oldest {
        let missed = chan.first - at;
        return super::crash(
            format!(
                "this listener fell behind a channel to `.Every`, missing {missed} message{}: listen \
                 with `when_behind=.DropOldest` to skip what it misses",
                if missed == 1 { "" } else { "s" }
            ),
            Span::default(),
        );
    }
    let first = chan.first;
    if let Some(listener) = state.listeners.get_mut(&number) {
        listener.at = first;
    }
    Ok(first)
}

/// Takes the next message from `number`, a channel or a listener, for `receiver`: `None` while
/// there's none, `(None, 1)` once it's closed and empty. The taker's clock takes in the sender's.
fn take_message(
    state: &mut State,
    receiver: usize,
    number: i64,
    orders: &mut Orders,
) -> Run<Option<(Value, i64)>> {
    let mut read_at = None;
    let taken = if let Some(listener) = state.listeners.get(&number) {
        let channel = listener.channel;
        two_at_once(state, receiver, number)?;
        let at = fallen_behind(state, receiver, number)?;
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
        read_at = Some(at);
        taken
    } else {
        let now = state.actors.get(receiver).map(|actor| actor.clock.clone()).unwrap_or_default();
        let Some(chan) = state.channels.get_mut(&number) else { return Ok(Some((Value::None, 1))) };
        // Two at once, where one takes a message, which the other could have taken instead: which
        // gets which hangs on time. One after another, where each knew of the other's last try,
        // is in order, and so are two that only wait, which nothing is taken from.
        let takes = !chan.queue.is_empty();
        let races = chan.receivers.iter().any(|(other, (tried, took))| {
            *other != receiver && (takes || *took) && !happened_before(tried, &now)
        });
        if races {
            return unsupported(
                "two tasks taking messages from one channel, which gets which hanging on time",
            );
        }
        let took = chan.receivers.get(&receiver).is_some_and(|(_, took)| *took);
        chan.receivers.insert(receiver, (now, took));
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
    if let Some(actor) = state.actors.get_mut(receiver) {
        merge(&mut actor.clock, &clock);
        tick(&mut actor.clock, receiver);
        let took = actor.clock.clone();
        // A listener's read is stamped, to be held against the send that lets the message go.
        if let (Some(at), Some(listener)) = (read_at, state.listeners.get_mut(&number)) {
            listener.reads.insert(at, took.clone());
            listener.receivers.insert(receiver, (took.clone(), true));
        }
        if let Some(chan) = state.channels.get_mut(&number)
            && let Some(tried) = chan.receivers.get_mut(&receiver)
        {
            *tried = (took, true);
        }
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

/// The rounds of a `parallel` call handed on to a thread of their own: what they run, which of
/// them, a copy of the names they read and of the rounds they're inside, where the call is in
/// what runs side by side, and where to leave what they made.
struct Rest {
    work: Arc<Work>,
    from: usize,
    count: usize,
    frames: Vec<super::Frame>,
    cur: usize,
    next_id: u64,
    rounds_as: Vec<Option<usize>>,
    scope: usize,
    done: Arc<Mutex<Option<RoundsRan>>>,
}

/// A thread running rounds handed on to it: it waits for the baton, runs them, leaves what they
/// made, and ends.
fn rest_thread(shared: &Arc<super::Shared>, slot: usize, rest: Rest) {
    let sched = &shared.sched;
    let Ok(mut state) = sched.state.lock() else { return };
    while state.running != slot && !state.given_up {
        let Ok(next) = sched.turn.wait(state) else { return };
        state = next;
    }
    if state.given_up {
        return;
    }
    let scopes =
        state.actors.get_mut(slot).map(|actor| std::mem::take(&mut actor.scopes)).unwrap_or_default();
    drop(state);
    if let Ok(mut held) = shared.scopes.lock() {
        *held = scopes;
    }
    let mut interp = Interp::new(Arc::clone(shared), None);
    interp.actor = slot;
    interp.frames = rest.frames;
    interp.cur = rest.cur;
    interp.next_id = rest.next_id;
    interp.rounds_as = rest.rounds_as;
    let (frames, mark, level) = (interp.frames.len(), interp.mark(), interp.rounds_as.len());
    interp.handing.push(super::parallel::Handing {
        work: Some(Arc::clone(&rest.work)),
        count: rest.count,
        round: rest.from,
        maybe: false,
        frames,
        mark,
        scope: rest.scope,
        level,
        handed: None,
    });
    let work = rest.work;
    let ran = interp.each_round(rest.from, rest.count, &mut |interp, at, _| interp.run_round(&work, at));
    match ran {
        Ok(done) => {
            if let Ok(mut left) = rest.done.lock() {
                *left = Some(done);
            }
        }
        Err(Flow::Abandoned) => return,
        Err(other) => {
            // What the rounds couldn't be run through gives up the whole run, saying what.
            let why = match other {
                Flow::Unsupported(why) => why,
                _ => "a `parallel` round handed on ended the way no round can".to_owned(),
            };
            if let Ok(mut refused) = shared.refused.lock() {
                refused.get_or_insert(why);
            }
            if let Ok(mut state) = sched.state.lock() {
                state.given_up = true;
            }
            sched.turn.notify_all();
            return;
        }
    }
    let Ok(mut state) = sched.state.lock() else { return };
    if let Some(actor) = state.actors.get_mut(slot) {
        actor.state = Doing::Ended;
    }
    if let Ok(mut held) = shared.scopes.lock() {
        held.clear();
    }
    if !state.hand_on(slot) {
        state.given_up = true;
        let said = state.stuck_said();
        if let Ok(mut refused) = shared.refused.lock() {
            refused.get_or_insert_with(|| said.to_owned());
        }
    }
    drop(state);
    sched.turn.notify_all();
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
    // The runtime cuts the block's waits short when its task fails as when it crashes, so its
    // launcher waiting on a reply that will never come reaches the block's end, and the failure
    // comes out there.
    let stopped = matches!(ended, Err(Flow::Crash(..) | Flow::Fail(..)));
    let mut launcher = None;
    if let Some(actor) = state.actors.get_mut(number) {
        actor.state = Doing::Ended;
        actor.ended = Some(ended);
        launcher = actor.launcher;
    }
    // A task that fails or crashes cuts short every wait its block is in: its launcher's, and those of
    // the threads running rounds it handed on, which are in the block too.
    if stopped && let Some(launcher) = launcher {
        let mut block = vec![launcher];
        while let Some(more) = state.actors.iter().enumerate().position(|(at, actor)| {
            !block.contains(&at)
                && !matches!(actor.state, Doing::Ended)
                && actor.beside.is_some_and(|beside| block.contains(&beside))
        }) {
            block.push(more);
        }
        for at in block {
            if let Some(actor) = state.actors.get_mut(at) {
                actor.cut_short.push(number);
            }
        }
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
        let said = state.stuck_said();
        if let Ok(mut refused) = shared.refused.lock() {
            refused.get_or_insert_with(|| said.to_owned());
        }
    }
    drop(state);
    sched.turn.notify_all();
}

/// Whether a failure is a `Closed`, as a send to a closed channel fails with.
fn is_closed(flow: &Flow) -> bool {
    matches!(flow, Flow::Fail(Value::Instance(instance), _) if instance.ty == "Closed")
}
