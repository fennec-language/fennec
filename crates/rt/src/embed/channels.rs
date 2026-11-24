//! Channels: bounded queues of messages between tasks, `with Channel[T](size=n) as c:` and the
//! inbox `with launch` makes. Senders and receivers are plain values holding a channel's number,
//! so there can be many of each.
//!
//! - **Bounded:** `send` waits while a channel's full, and `receive` while it's empty. Each takes a
//!   timeout. A wait parks in a task, so its thread runs others meanwhile, and blocks a thread that
//!   isn't one. A stop cuts a parked wait short, as it does any other.
//! - **Closing:** after `close`, a send fails at once, and receivers take what's left before they're
//!   told it's closed. That's how a task drains its inbox.
//! - **Messages:** a message crosses threads as what's given to a launched task does. It's copied
//!   at `send` into a table of its own, which whoever receives it takes as its own. A message of only
//!   numbers is a word and isn't copied.
//! - **Lifetime:** a channel can't leave its `with`, so everything holding it has ended by the time
//!   the block ends and frees it. A number that's been freed acts as a closed channel's.
//! - **To every listener:** a channel made `to=.Every` gives each listener every message, instead of
//!   one receiver each (see `every`).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use fennec_syntax::{Diagnostic, Span};

use super::deadlock::{self, Kind, Note};
use super::table::Table;
use super::{State, attempt, guarded, heap, with};

/// What `send` and `receive` give back.
const DONE: i64 = 0;
const CLOSED: i64 = 1;
const TIMED_OUT: i64 = 2;
/// What a send or receive a stop cut short came to, in a recording, which native code never sees:
/// its task fails as stopped, and gets 0.
const STOPPED: i64 = 3;

/// One message waiting in a channel.
enum Message {
    /// A value of only numbers.
    Word(u64),
    /// A block's copy, at this address in this table.
    Block(u64, Box<Table>),
    /// A block's copy, at this address in this table, kept for every listener of a channel to
    /// `.Every`: each takes a copy of its own.
    Shared(u64, Arc<Table>),
    /// A flat message's words, with copies of the blocks they name (see `words`).
    Words(Box<words::Flat>),
    /// The same, kept for every listener of a channel to `.Every`: each takes copies of its own.
    SharedWords(Arc<words::Flat>),
}

struct Channel {
    /// Its number in `CHANNELS`, which a deadlock check's note of a wait on it finds it by.
    number: i64,
    size: usize,
    /// Its name, in a recording or a replay, where what's done to it is logged and replayed in turn.
    replay: Option<Arc<str>>,
    queue: Mutex<Queue>,
    /// For waits that block their thread: `Queue::asleep` says how many are in one.
    changed: Condvar,
    /// Whether it's been freed, and is gone from `CHANNELS`: a thread that found it before, and kept
    /// it to find again, takes it for gone as `CHANNELS` would.
    freed: std::sync::atomic::AtomicBool,
    /// For tests: how many times it's told the threads asleep on it to look again.
    #[cfg(test)]
    told: AtomicI64,
    /// Whether it gives every listener every message, as one made `to=.Every` does, rather than
    /// each message to one receiver. Set as it's made, so it's read without the queue.
    to_every: bool,
    /// How many times the notes parked tasks left have been taken, to wake them: those waiting
    /// for room, then those waiting for a message. Changed with the queue held. A wait that's
    /// just woken reads it without the queue, and knows from it whether its note has gone.
    #[cfg(any(test, feature = "threads"))]
    taken: [std::sync::atomic::AtomicU64; 2],
}

#[derive(Default)]
struct Queue {
    messages: VecDeque<Message>,
    closed: bool,
    /// Parked tasks waiting for room, and for a message, to wake when there may be one.
    senders: Vec<Waiter>,
    receivers: Vec<Waiter>,
    /// How many threads are asleep on `changed`, waiting for room and for a message: a thread that
    /// isn't a task can't park. Counted under this lock, by the wait itself as it starts and ends,
    /// so whoever changes the queue knows whether there's anyone to tell, and tells nobody when
    /// there isn't: on Linux telling is a system call, made or not.
    asleep: Asleep,
    /// For a channel to `.Every`: where each listener's up to.
    every: Option<every::Listeners>,
}

/// The threads asleep on a channel, by what each waits for.
#[derive(Clone, Copy, Default)]
struct Asleep {
    for_room: usize,
    for_a_message: usize,
}

impl Channel {
    /// Tells the threads asleep on it to look again, when `anyone` are: what a change to its queue
    /// does once the queue's let go of, with what `asleep` said while it was held.
    fn tell(&self, anyone: usize) {
        if anyone == 0 {
            return;
        }
        #[cfg(test)]
        self.told.fetch_add(1, Ordering::Relaxed);
        self.changed.notify_all();
    }
}

/// A parked task to wake: the pool's worker it's on, and the token it's parked on.
#[cfg(any(test, feature = "threads"))]
struct Waiter {
    pool: &'static super::parallel::Pool,
    at: usize,
    token: mio::Token,
}

/// Without threads, nothing parks.
#[cfg(not(any(test, feature = "threads")))]
struct Waiter;

/// Channels by number. A channel's number is never given out again, so a number names one channel
/// for the whole run.
type ByNumber<T> = HashMap<i64, T, std::hash::BuildHasherDefault<heap::AddressHasher>>;

/// How many times a thread tries a channel's queue that's held before it waits for it. On a
/// program whose tasks pass most of what they do through channels, `chat_server`, this took 6.5% off
/// its instructions at the default threads, nearly all the system's: a held queue's wait was a
/// call into the system to sleep, and its let-go another to wake the sleeper. More tries took no
/// more off.
const QUEUE_TRIES: u32 = 16;

static CHANNELS: Mutex<Option<ByNumber<Arc<Channel>>>> = Mutex::new(None);
static NEXT: AtomicI64 = AtomicI64::new(1);

/// How many channels a thread keeps to find again before it lets go of those that are gone.
const FOUND_KEPT: usize = 64;

thread_local! {
    /// The channels this thread found lately, kept to find again without taking `CHANNELS`' lock,
    /// which every thread's sends and receives took each time, and waited on each other for.
    static FOUND: std::cell::RefCell<ByNumber<std::sync::Weak<Channel>>> =
        std::cell::RefCell::new(ByNumber::default());
}

fn channels<T>(use_them: impl FnOnce(&mut ByNumber<Arc<Channel>>) -> T) -> T {
    use_them(CHANNELS.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert_with(ByNumber::default))
}

fn find(channel: i64) -> Option<Arc<Channel>> {
    let kept = FOUND.with_borrow(|found| found.get(&channel).and_then(std::sync::Weak::upgrade));
    if let Some(kept) = kept {
        return (!kept.freed.load(Ordering::Acquire)).then_some(kept);
    }
    let found = channels(|channels| channels.get(&channel).cloned())?;
    FOUND.with_borrow_mut(|kept| {
        if kept.len() >= FOUND_KEPT {
            kept.retain(|_, channel| channel.strong_count() > 0);
            if kept.len() >= FOUND_KEPT {
                kept.clear();
            }
        }
        kept.insert(channel, Arc::downgrade(&found));
    });
    Some(found)
}

impl Channel {
    /// Its queue, held. A sender and a receiver on two threads take it in turn for a push or a
    /// pop, so it's let go of almost at once: a thread that finds it held tries again a few times
    /// before it waits, as a wait goes through the system both to sleep and to be woken.
    fn queue(&self) -> MutexGuard<'_, Queue> {
        for _ in 0..QUEUE_TRIES {
            match self.queue.try_lock() {
                Ok(queue) => return queue,
                Err(TryLockError::Poisoned(queue)) => return queue.into_inner(),
                Err(TryLockError::WouldBlock) => std::hint::spin_loop(),
            }
        }
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether a wait on `side` would still find nothing to do, with `queue`, its own, held: it's
    /// open, and empty for a receiver or full for a sender. A listener's, on a channel to `.Every`,
    /// is caught up: what it has yet to see is its own, not the queue's.
    fn still_waits(&self, queue: &Queue, side: Side, listener: Option<i64>) -> bool {
        let open = !queue.closed && !self.freed.load(Ordering::Acquire);
        open && match (side, listener) {
            (Side::Receiving, Some(listener)) => every::caught_up(queue, listener),
            (Side::Receiving, None) => queue.messages.is_empty(),
            (Side::Sending, _) => queue.messages.len() >= self.size,
        }
    }

    /// The note a wait on `side` with no time limit leaves for the deadlock check: on the channel,
    /// or for a listener's on a channel to `.Every`, on the listener, whose number the check finds
    /// it by, as what it has yet to see is its own and not the queue's.
    fn stuck_note(&self, side: Side, listener: Option<i64>) -> Option<Note> {
        let kind = match side {
            Side::Receiving => Kind::Receive,
            Side::Sending => Kind::Send,
        };
        let on = match (self.to_every, listener) {
            (false, _) => self.number,
            (true, Some(listener)) => listener,
            // A send to `.Every` never waits.
            (true, None) => return None,
        };
        Some(Note::on_channel(on, kind, self.replay.as_deref()))
    }
}

/// For the deadlock check, whether a wait of `kind` on the channel numbered `channel` would still
/// find nothing to do: it's open, and empty for a receive or full for a send. For a listener's
/// number, it's the listener's receive: its channel to `.Every` is open and it's caught up. A
/// channel that's gone is as closed.
pub(in crate::embed) fn still_waits_on(channel: i64, kind: Kind) -> bool {
    let side = match kind {
        Kind::Receive => Side::Receiving,
        Kind::Send => Side::Sending,
        Kind::BlockEnd | Kind::Join | Kind::Turn => return false,
    };
    if let Some((channel, listener)) = every::listening(channel) {
        return matches!(side, Side::Receiving)
            && channel.still_waits(&channel.queue(), side, Some(listener));
    }
    find(channel).is_some_and(|channel| channel.still_waits(&channel.queue(), side, None))
}

/// Whether the number a stuck wait's note names is a listener's, on a channel to `.Every`, for the
/// deadlock report to say so.
pub(in crate::embed) fn is_listener(number: i64) -> bool {
    every::listening(number).is_some()
}

/// A channel's size, or the failure of one that couldn't hold a message.
fn size_of(size: i64) -> Option<usize> {
    let held = usize::try_from(size).ok().filter(|size| *size > 0);
    if held.is_none() {
        let message = format!("a channel has to hold at least 1 message, but its size is {size}");
        super::fail_with(Diagnostic::new(message, Span::default()));
    }
    held
}

/// `fennec_channel_new(size)`: an open channel holding at most `size` messages. A size below 1
/// fails.
pub extern "C" fn channel_new(size: i64) -> i64 {
    let Some(size) = size_of(size) else { return 0 };
    let number = NEXT.fetch_add(1, Ordering::Relaxed);
    let channel = Channel {
        number,
        size,
        replay: crate::replay::place("c"),
        queue: Mutex::new(Queue::default()),
        changed: Condvar::new(),
        freed: std::sync::atomic::AtomicBool::new(false),
        #[cfg(test)]
        told: AtomicI64::new(0),
        to_every: false,
        #[cfg(any(test, feature = "threads"))]
        taken: Default::default(),
    };
    channels(|channels| channels.insert(number, Arc::new(channel)));
    number
}

/// When a wait of `timeout_ns` gives up: never, for -1.
fn deadline(timeout_ns: i64) -> Option<Instant> {
    u64::try_from(timeout_ns).ok().map(|nanos| Instant::now() + Duration::from_nanos(nanos))
}

/// `fennec_channel_send(channel, value, is_block, timeout_ns)`: sends `value`, a block's address if
/// `is_block` is 1, which is copied, the sender keeping its own. Waits up to `timeout_ns` for room,
/// or as long as it takes for -1. Gives back 0 when it's sent, 1 when the channel's closed, and 2
/// when it timed out. A stop cuts the wait short, failing it.
pub extern "C" fn channel_send(channel: i64, value: i64, is_block: i64, timeout_ns: i64) -> i64 {
    // What this task printed comes before anything whoever takes this goes on to print.
    super::write_kept_output();
    let Some(channel) = find(channel) else { return CLOSED };
    // In a replay, it waits its turn, and a send that didn't go through doesn't wait again.
    let timeout_ns = match replayed(&channel, "send", timeout_ns < 0) {
        Turn::Live => timeout_ns,
        Turn::Goes => -1,
        Turn::Gives(code) => return code,
    };
    // A word's bits go across as they are: a negative number or float is as good as any.
    let address = u64::from_ne_bytes(value.to_ne_bytes());
    let message = if is_block != 0 && address != 0 {
        let copied = attempt(|state: &mut State| guarded::copied_for_task(state, address).map(Some));
        let Some((copy, table)) = copied else { return DONE };
        Message::Block(copy, table)
    } else {
        Message::Word(address)
    };
    send_on(&channel, message, timeout_ns)
}

/// Puts `message`, made for `channel` once the send's turn came, in its queue: waiting up to
/// `timeout_ns` for room, or as long as it takes for -1. What's given back is what
/// `fennec_channel_send` gives back, and a message that didn't go is let go of.
fn send_on(channel: &Arc<Channel>, message: Message, timeout_ns: i64) -> i64 {
    let deadline = deadline(timeout_ns);
    let mut message = Some(message);
    loop {
        let mut queue = channel.queue();
        if queue.closed {
            met(channel, "send", CLOSED);
            drop(queue);
            let_go(message);
            return CLOSED;
        }
        if queue.every.is_some() {
            // It never waits: the oldest message goes, if there's no room.
            met(channel, "send", DONE);
            let dropped = every::push(&mut queue, message.take(), channel.size);
            let waiting = channel.take_notes(&mut queue, Side::Receiving);
            let asleep = queue.asleep.for_a_message;
            drop(queue);
            let_go(dropped);
            wake(waiting);
            channel.tell(asleep);
            return DONE;
        }
        if queue.messages.len() < channel.size {
            met(channel, "send", DONE);
            if let Some(message) = message.take() {
                queue.messages.push_back(message);
            }
            let waiting = channel.take_notes(&mut queue, Side::Receiving);
            let asleep = queue.asleep.for_a_message;
            drop(queue);
            wake(waiting);
            channel.tell(asleep);
            return DONE;
        }
        match wait(channel, queue, deadline, Side::Sending, None) {
            Waited::Again => {}
            Waited::TimedOut => {
                met(channel, "send", TIMED_OUT);
                let_go(message);
                return TIMED_OUT;
            }
            Waited::Stopped => {
                let_go(message);
                return stopped(channel, "send");
            }
            Waited::CutShort => {
                met(channel, "send", CLOSED);
                let_go(message);
                return CLOSED;
            }
        }
    }
}

/// `fennec_channel_receive(channel, timeout_ns, value)`: takes the next message into `value`, now
/// the receiver's own, holding one count, and gives back 0. Waits up to `timeout_ns` for one, or as
/// long as it takes for -1. Gives back 1 once the channel's closed and empty, and 2 when it timed
/// out. A stop cuts the wait short, failing it.
pub extern "C" fn channel_receive(channel: i64, timeout_ns: i64, value: &mut i64) -> i64 {
    *value = 0;
    match next(channel, timeout_ns) {
        Ok(message) => {
            *value = i64::from_ne_bytes(taken(message).to_ne_bytes());
            DONE
        }
        Err(code) => code,
    }
}

/// The next message for `channel`, a channel's number or a listener's, taken off its queue once the
/// receive's turn came: waiting up to `timeout_ns` for one, or as long as it takes for -1. With
/// none, what a receive gives back instead: `DONE` when the running task is failing, as for a stop.
fn next(channel: i64, timeout_ns: i64) -> Result<Message, i64> {
    if let Some((channel, listener)) = every::listening(channel) {
        return every::next(&channel, listener, timeout_ns);
    }
    let Some(channel) = find(channel) else { return Err(CLOSED) };
    // A channel to every listener has no one message to hand out: each listener has a place of its
    // own in what's kept. Taking one here used to pull the oldest out from under them, and a
    // listener that was caught up then never saw the message sent next, with nothing said.
    // `std.concurrent` stops a program that asks, saying where; this is for whatever gets past it.
    if channel.to_every {
        super::fail_with(Diagnostic::new(
            "this channel is to every listener, so it has no one next message to take: loop over it \
             with `for message in channel`, or take an `inbox()` and receive from that",
            Span::default(),
        ));
        return Err(DONE);
    }
    let timeout_ns = match replayed(&channel, "receive", timeout_ns < 0) {
        Turn::Live => timeout_ns,
        Turn::Goes => -1,
        Turn::Gives(code) => return Err(code),
    };
    let deadline = deadline(timeout_ns);
    loop {
        let mut queue = channel.queue();
        if let Some(message) = queue.messages.pop_front() {
            met(&channel, "receive", DONE);
            let waiting = channel.take_notes(&mut queue, Side::Sending);
            let asleep = queue.asleep.for_room;
            drop(queue);
            wake(waiting);
            channel.tell(asleep);
            return Ok(message);
        }
        if queue.closed {
            met(&channel, "receive", CLOSED);
            return Err(CLOSED);
        }
        match wait(&channel, queue, deadline, Side::Receiving, None) {
            Waited::Again => {}
            Waited::TimedOut => {
                met(&channel, "receive", TIMED_OUT);
                return Err(TIMED_OUT);
            }
            Waited::Stopped => return Err(stopped(&channel, "receive")),
            Waited::CutShort => {
                met(&channel, "receive", CLOSED);
                return Err(CLOSED);
            }
        }
    }
}

/// What a replay says to do at a channel.
enum Turn {
    /// This isn't a replay: it runs as it would.
    Live,
    /// It's this task's turn, and what it does goes through, without waiting.
    Goes,
    /// It's this task's turn, and it came to this without going through: closed, or timed out.
    Gives(i64),
}

/// Waits, in a replay, for the running task's turn to do `what` at `channel`, a wait with no time
/// limit when `for_good`. A replay that's diverged fails, as having found the channel closed.
fn replayed(channel: &Channel, what: &str, for_good: bool) -> Turn {
    // A channel's named only in a recording or a replay: anywhere else nothing's waited for.
    let Some(place) = channel.replay.as_deref() else { return Turn::Live };
    // A send or a receive with no time limit is a stuck wait where nothing can end it, which a
    // replay of a run that deadlocked goes on into past its recording's end, as the run was when
    // the check fired: a listener's on a channel to `.Every` too, the one receive there is. One with
    // a limit is no stuck wait, so it never goes past: it fails at once, where it waited its limit
    // out first. A send to `.Every` never waits.
    let stuck_kind = what == "receive" || (what == "send" && !channel.to_every);
    let turn = if for_good && stuck_kind {
        crate::replay::turn_to_wait(Some(place), what)
    } else {
        crate::replay::turn(Some(place), what)
    };
    match turn {
        Ok(None | Some(crate::replay::PAST_THE_END)) => Turn::Live,
        Ok(Some(DONE)) => Turn::Goes,
        Ok(Some(STOPPED)) => {
            met(channel, what, STOPPED);
            crate::replay::stopped_as_recorded();
            Turn::Gives(DONE)
        }
        Ok(Some(code)) => {
            // It changes nothing, so this is where its turn ends.
            met(channel, what, code);
            Turn::Gives(code)
        }
        Err(why) => {
            super::fail_with(Diagnostic::new(why, Span::default()));
            Turn::Gives(CLOSED)
        }
    }
}

/// A stop cut short the running task's wait to do `what` at `channel`, and it's failing: recorded,
/// so a replay, which doesn't wait, stops it at the same wait.
fn stopped(channel: &Channel, what: &str) -> i64 {
    met(channel, what, STOPPED);
    DONE
}

/// Says the running task did `what` at `channel`, coming to `outcome`: logged in a recording, and
/// in a replay, the end of its turn.
fn met(channel: &Channel, what: &str, outcome: i64) {
    if let Some(place) = channel.replay.as_deref() {
        crate::replay::met(Some(place), what, outcome);
    }
}

/// `fennec_channel_close(channel)`: no more can be sent. Receivers take what's left, then are told
/// it's closed. Everything waiting on it wakes.
pub extern "C" fn channel_close(channel: i64) {
    let Some(channel) = find(channel) else { return };
    if let Turn::Gives(_) = replayed(&channel, "close", false) {
        return;
    }
    let mut queue = channel.queue();
    met(&channel, "close", DONE);
    queue.closed = true;
    let senders = channel.take_notes(&mut queue, Side::Sending);
    let receivers = channel.take_notes(&mut queue, Side::Receiving);
    drop(queue);
    wake(senders);
    wake(receivers);
    channel.changed.notify_all();
}

/// `fennec_channel_free(channel)`: its `with` ends, so nothing holds it any more. What's still in it
/// is let go of, and anything still waiting on it wakes and finds it closed, as at `close`.
///
/// Nothing the checker lets through waits on a channel past its `with`, but a task a cancelled
/// round left behind can be parked on one its round then frees. It used to wait on until its stop
/// woke it; and a waiter nobody wakes would be a stuck wait for good to the deadlock check.
pub extern "C" fn channel_free(channel: i64) {
    every::forget(channel);
    let Some(channel) = channels(|channels| channels.remove(&channel)) else { return };
    channel.freed.store(true, Ordering::Release);
    let mut queue = channel.queue();
    queue.closed = true;
    let left: Vec<Message> = queue.messages.drain(..).collect();
    let senders = channel.take_notes(&mut queue, Side::Sending);
    let receivers = channel.take_notes(&mut queue, Side::Receiving);
    drop(queue);
    wake(senders);
    wake(receivers);
    channel.changed.notify_all();
    for message in left {
        let_go(Some(message));
    }
}

/// A message, taken as this thread's own: its block's address, or its word.
fn taken(message: Message) -> u64 {
    match message {
        Message::Word(word) => word,
        Message::Block(address, table) => {
            with(|state| state.heap.adopt_made(table.into_blocks()));
            address
        }
        Message::Shared(address, table) => every::copied_out(address, &table),
        // Native code asks for words for a kind it sends as words, and for a word otherwise.
        Message::Words(_) | Message::SharedWords(_) => {
            heap::runtime_bug("a message sent as words was received as one word")
        }
    }
}

/// Lets go of a message that was never received.
fn let_go(message: Option<Message>) {
    // A message of words owns its copies outside any heap, so dropping it frees them.
    if let Some(Message::Words(_) | Message::SharedWords(_)) = &message {
        return;
    }
    let message = match message {
        // Kept for others too, it's only let go of by the last to hold it.
        Some(Message::Shared(address, table)) => match Arc::try_unwrap(table) {
            Ok(table) => Some(Message::Block(address, Box::new(table))),
            Err(_) => None,
        },
        message => message,
    };
    if let Some(Message::Block(..)) = &message {
        let address = message.map_or(0, taken);
        attempt(|state: &mut State| {
            heap::release(state, address);
            Ok(())
        });
    }
}

#[derive(Clone, Copy)]
enum Side {
    Sending,
    Receiving,
}

impl Channel {
    /// Takes every note on a `side` out of `queue`, which is this channel's, for the tasks that
    /// left them to be woken, and counts that it did when there were any.
    ///
    /// One note, the usual case, is taken out of its list and the list kept, room and all, for the
    /// next wait to leave its note in: taking the whole list, as for several, let it go and had the
    /// next wait make it again, which cost a send or a receive about as much as the channel's lock.
    fn take_notes(&self, queue: &mut Queue, side: Side) -> Notes {
        let notes = match side {
            Side::Sending => &mut queue.senders,
            Side::Receiving => &mut queue.receivers,
        };
        let taken = match notes.len() {
            0 => return Notes::None,
            1 => notes.pop().map_or(Notes::None, Notes::One),
            _ => Notes::Several(std::mem::take(notes)),
        };
        #[cfg(any(test, feature = "threads"))]
        {
            // Only whoever holds the queue changes it, so it's read and written, not added to.
            let count = self.taken_on(side);
            count.store(count.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
        }
        #[cfg(not(any(test, feature = "threads")))]
        let _: (&Channel, Side) = (self, side);
        taken
    }
}

#[cfg(any(test, feature = "threads"))]
impl Channel {
    fn taken_on(&self, side: Side) -> &std::sync::atomic::AtomicU64 {
        let [for_room, for_a_message] = &self.taken;
        match side {
            Side::Sending => for_room,
            Side::Receiving => for_a_message,
        }
    }

    /// How many times the notes on a `side` have been taken.
    fn taken(&self, side: Side) -> u64 {
        self.taken_on(side).load(Ordering::Relaxed)
    }

    /// Takes back the note a parked wait left on its `side` when the notes there had been taken
    /// `left_at` times, if it's still there. The wait is known by its pool, its worker and its
    /// token there: a token is one worker's.
    ///
    /// Whoever wakes a wait through its note takes every note on that side and counts it. So a
    /// count that's moved on says the note has gone, with no look for it and the queue not held,
    /// which is how a wait nearly always ends. A wait woken another way, by its time running
    /// out, a stop, or a crash of a task it launched, finds the count where it left it, and
    /// takes its note out from among the others'. Left there, it was woken through for nothing
    /// by the next change, and they piled up from a wait that times out in a loop.
    fn take_back(
        &self,
        side: Side,
        (pool, at, token): (&'static super::parallel::Pool, usize, mio::Token),
        left_at: u64,
    ) {
        if self.taken(side) != left_at {
            return;
        }
        let mut queue = self.queue();
        // Looked at again with the queue held: a take may have come since the look without it.
        if self.taken(side) != left_at {
            return;
        }
        let notes = match side {
            Side::Sending => &mut queue.senders,
            Side::Receiving => &mut queue.receivers,
        };
        notes.retain(|note| !(std::ptr::eq(note.pool, pool) && note.at == at && note.token == token));
    }
}

enum Waited {
    /// Look again: something may have changed.
    Again,
    TimedOut,
    /// A stop cut it short: the task's failing.
    #[cfg_attr(not(any(test, feature = "threads")), expect(dead_code, reason = "only tasks are stopped"))]
    Stopped,
    /// A task this code launched crashed or failed, so nothing may ever come: it's over as if the
    /// channel were closed, and the `with launch` block's end says how the task did.
    #[cfg_attr(not(any(test, feature = "threads")), expect(dead_code, reason = "only tasks crash"))]
    CutShort,
}

/// Waits for the channel to change, with `queue` held to see that it hasn't: parked, in a task that
/// can, and otherwise blocking.
fn wait<'a>(
    channel: &'a Arc<Channel>,
    queue: MutexGuard<'a, Queue>,
    deadline: Option<Instant>,
    side: Side,
    listener: Option<i64>,
) -> Waited {
    if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
        return Waited::TimedOut;
    }
    #[cfg(any(test, feature = "threads"))]
    if super::threads::Launcher::cut_short_here() {
        return Waited::CutShort;
    }
    #[cfg(any(test, feature = "threads"))]
    if super::serve::can_park()
        && let Some(here) = super::parallel::here()
    {
        return wait_parked(channel, queue, deadline, (side, listener), here);
    }
    let mut queue = queue;
    // A crash of a task this code launched wakes it: told with the queue held, so the crash can't
    // come between this last look and the wait.
    #[cfg(any(test, feature = "threads"))]
    let said = {
        let changed = Arc::downgrade(channel);
        let said = super::threads::Launcher::waiting_here(move || {
            if let Some(channel) = changed.upgrade() {
                let _queue = channel.queue();
                channel.changed.notify_all();
            }
        });
        if super::threads::Launcher::cut_short_here() {
            super::threads::Launcher::waited_here(said);
            return Waited::CutShort;
        }
        said
    };
    // What this thread printed comes out first, as before anything else that waits. The queue stays
    // held, so no change is missed before the wait.
    super::write_kept_output();
    // Counted as asleep from here to when it wakes, with the queue held at both ends. The wait
    // lets go of the queue only as it goes to sleep, so whoever changes the queue next takes it
    // after that, sees there's someone to tell, and tells them. Whoever changed it before had let
    // go of it before this thread's last look, which saw the change.
    match side {
        Side::Sending => queue.asleep.for_room += 1,
        Side::Receiving => queue.asleep.for_a_message += 1,
    }
    let (mut queue, timed_out, stuck) = if let Some(deadline) = deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        let (queue, waited) =
            channel.changed.wait_timeout(queue, left).unwrap_or_else(PoisonError::into_inner);
        (queue, waited.timed_out(), false)
    } else {
        let Some((queue, stuck)) = wait_stuck(channel, queue, side, listener) else {
            #[cfg(any(test, feature = "threads"))]
            super::threads::Launcher::waited_here(said);
            return Waited::Again;
        };
        (queue, false, stuck)
    };
    // Awake again, however it was woken, with the queue held once more. It was counted above, so
    // the count has it: one that didn't would mean a thread left asleep and uncounted, which a
    // count held at nothing would hide.
    let counted = match side {
        Side::Sending => &mut queue.asleep.for_room,
        Side::Receiving => &mut queue.asleep.for_a_message,
    };
    debug_assert!(*counted > 0, "a thread woke on a channel that didn't count it asleep");
    *counted -= 1;
    drop(queue);
    if stuck {
        deadlock::thread_woke();
    }
    #[cfg(any(test, feature = "threads"))]
    super::threads::Launcher::waited_here(said);
    if timed_out { Waited::TimedOut } else { Waited::Again }
}

/// A wait with no time limit that blocks its thread, with `queue`, this channel's, held and counted
/// asleep: waits until it's woken. A wait that could be a stuck one waits a moment first (see
/// `deadlock::PATIENCE`), and only if nothing's changed by then is it noted and counted, looking for
/// a deadlock if the counts say there may be one. Gives back the queue, held, once it's woken, with
/// whether its note was counted, or `None` when the wait was over while it looked.
fn wait_stuck<'a>(
    channel: &'a Arc<Channel>,
    queue: MutexGuard<'a, Queue>,
    side: Side,
    listener: Option<i64>,
) -> Option<(MutexGuard<'a, Queue>, bool)> {
    let Some(note) = channel.stuck_note(side, listener) else {
        return Some((channel.changed.wait(queue).unwrap_or_else(PoisonError::into_inner), false));
    };
    let (queue, waited) =
        channel.changed.wait_timeout(queue, deadlock::PATIENCE).unwrap_or_else(PoisonError::into_inner);
    #[cfg(any(test, feature = "threads"))]
    let cut_short = super::threads::Launcher::cut_short_here();
    #[cfg(not(any(test, feature = "threads")))]
    let cut_short = false;
    if !waited.timed_out() || cut_short || !channel.still_waits(&queue, side, listener) {
        return Some((queue, false));
    }
    // Noted and counted with the queue held, as it's counted asleep.
    let look = deadlock::thread_waits(note.blocking(), true);
    let queue = if look { looked(channel, queue, side, listener)? } else { queue };
    Some((channel.changed.wait(queue).unwrap_or_else(PoisonError::into_inner), true))
}

/// For a wait that blocks its thread, noted and counted, when the counts say every
/// context is stuck: lets go of `queue`, this channel's, while it looks for a deadlock, as the check
/// looks at this channel, then looks at it again. A change that came meanwhile told nobody, as this
/// thread wasn't asleep yet, so the wait's over if anything changed: it's taken back, and `None`
/// says so. Otherwise gives the queue back, held, to wait.
fn looked<'a>(
    channel: &'a Arc<Channel>,
    queue: MutexGuard<'a, Queue>,
    side: Side,
    listener: Option<i64>,
) -> Option<MutexGuard<'a, Queue>> {
    drop(queue);
    deadlock::look();
    let mut queue = channel.queue();
    #[cfg(any(test, feature = "threads"))]
    let cut_short = super::threads::Launcher::cut_short_here();
    #[cfg(not(any(test, feature = "threads")))]
    let cut_short = false;
    if !cut_short && channel.still_waits(&queue, side, listener) {
        return Some(queue);
    }
    match side {
        Side::Sending => queue.asleep.for_room -= 1,
        Side::Receiving => queue.asleep.for_a_message -= 1,
    }
    drop(queue);
    deadlock::thread_woke();
    None
}

/// `wait`, for a task that can park, on worker `at` of `pool`: it leaves a note on the channel to
/// be woken through, and parks until it is, or its time runs out, or it's stopped. However it was
/// woken, the note's taken back if it's still there.
#[cfg(any(test, feature = "threads"))]
fn wait_parked(
    channel: &Arc<Channel>,
    queue: MutexGuard<'_, Queue>,
    deadline: Option<Instant>,
    (side, listener): (Side, Option<i64>),
    (pool, at): (&'static super::parallel::Pool, usize),
) -> Waited {
    let mut queue = queue;
    let token = super::serve::next_token();
    // A crash of a task this one launched wakes it, said before its last look.
    let said = super::threads::Launcher::waiting_here(move || {
        pool.run_on(at, Box::new(move |io: &mut super::serve::Io| io.wake_token(token)));
    });
    if super::threads::Launcher::cut_short_here() {
        super::threads::Launcher::waited_here(said);
        return Waited::CutShort;
    }
    let waiter = Waiter { pool, at, token };
    match side {
        Side::Sending => queue.senders.push(waiter),
        Side::Receiving => queue.receivers.push(waiter),
    }
    let left_at = channel.taken(side);
    drop(queue);
    // With no time limit it's a stuck wait, whose note goes with it, for the deadlock check.
    let parked = match deadline.is_none().then(|| channel.stuck_note(side, listener)).flatten() {
        Some(note) => super::serve::park_stuck(token, note),
        None => super::serve::park_on(token, deadline),
    };
    channel.take_back(side, (pool, at, token), left_at);
    super::threads::Launcher::waited_here(said);
    match parked {
        // Stopped, it's failing. Cancelled, as by a server that's stopping, it can't wait any more
        // and ends here: it'd never be parked again, and nothing would ever change what it saw.
        super::serve::Parked::Cancelled => {
            if !super::failed() {
                super::serve::fail_as_stopped();
            }
            Waited::Stopped
        }
        super::serve::Parked::TimedOut => Waited::TimedOut,
        super::serve::Parked::Ready => Waited::Again,
    }
}

/// The notes taken off one side of a channel, of parked tasks to wake.
enum Notes {
    None,
    One(Waiter),
    Several(Vec<Waiter>),
}

#[cfg(test)]
impl Notes {
    fn len(&self) -> usize {
        match self {
            Notes::None => 0,
            Notes::One(_) => 1,
            Notes::Several(several) => several.len(),
        }
    }
}

/// Wakes parked tasks that were waiting on a channel: each looks again.
fn wake(waiting: Notes) {
    #[cfg(any(test, feature = "threads"))]
    let wake_one = |Waiter { pool, at, token }: Waiter| {
        pool.run_on(at, Box::new(move |io: &mut super::serve::Io| io.wake_token(token)));
    };
    #[cfg(not(any(test, feature = "threads")))]
    let wake_one = drop::<Waiter>;
    match waiting {
        Notes::None => {}
        Notes::One(one) => wake_one(one),
        Notes::Several(several) => several.into_iter().for_each(wake_one),
    }
}

mod every;
pub use every::{channel_listen, channel_new_every};
mod words;
pub use words::{channel_receive_words, channel_send_words};

#[cfg(test)]
mod tests;
