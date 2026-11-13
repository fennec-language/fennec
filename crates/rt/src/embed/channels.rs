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
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use fennec_syntax::{Diagnostic, Span};

use super::table::Table;
use super::{State, attempt, guarded, heap, with};

/// What `send` and `receive` give back.
const DONE: i64 = 0;
const CLOSED: i64 = 1;
const TIMED_OUT: i64 = 2;

/// One message waiting in a channel.
enum Message {
    /// A value of only numbers.
    Word(u64),
    /// A block's copy, at this address in this table.
    Block(u64, Box<Table>),
    /// A block's copy, at this address in this table, kept for every listener of a channel to
    /// `.Every`: each takes a copy of its own.
    Shared(u64, Arc<Table>),
}

struct Channel {
    size: usize,
    /// Its name, in a recording or a replay, where what's done to it is logged and replayed in turn.
    replay: Option<Arc<str>>,
    queue: Mutex<Queue>,
    /// For waits that block their thread.
    changed: Condvar,
}

#[derive(Default)]
struct Queue {
    messages: VecDeque<Message>,
    closed: bool,
    /// Parked tasks waiting for room, and for a message, to wake when there may be one.
    senders: Vec<Waiter>,
    receivers: Vec<Waiter>,
    /// For a channel to `.Every`: where each listener's up to.
    every: Option<every::Listeners>,
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

static CHANNELS: Mutex<Option<HashMap<i64, Arc<Channel>>>> = Mutex::new(None);
static NEXT: AtomicI64 = AtomicI64::new(1);

fn channels<T>(use_them: impl FnOnce(&mut HashMap<i64, Arc<Channel>>) -> T) -> T {
    use_them(CHANNELS.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert_with(HashMap::new))
}

fn find(channel: i64) -> Option<Arc<Channel>> {
    channels(|channels| channels.get(&channel).cloned())
}

impl Channel {
    fn queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
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
    let channel = Channel {
        size,
        replay: crate::replay::place("c"),
        queue: Mutex::new(Queue::default()),
        changed: Condvar::new(),
    };
    let number = NEXT.fetch_add(1, Ordering::Relaxed);
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
    let timeout_ns = match replayed(&channel, "send") {
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
    let deadline = deadline(timeout_ns);
    let mut message = Some(message);
    loop {
        let mut queue = channel.queue();
        if queue.closed {
            met(&channel, "send", CLOSED);
            drop(queue);
            let_go(message);
            return CLOSED;
        }
        if queue.every.is_some() {
            // It never waits: the oldest message goes, if there's no room.
            met(&channel, "send", DONE);
            let dropped = every::push(&mut queue, message.take(), channel.size);
            let waiting = std::mem::take(&mut queue.receivers);
            drop(queue);
            let_go(dropped);
            wake(waiting);
            channel.changed.notify_all();
            return DONE;
        }
        if queue.messages.len() < channel.size {
            met(&channel, "send", DONE);
            queue.messages.extend(message.take());
            let waiting = std::mem::take(&mut queue.receivers);
            drop(queue);
            wake(waiting);
            channel.changed.notify_all();
            return DONE;
        }
        match wait(&channel, queue, deadline, Side::Sending) {
            Waited::Again => {}
            Waited::TimedOut => {
                met(&channel, "send", TIMED_OUT);
                let_go(message);
                return TIMED_OUT;
            }
            Waited::Stopped => {
                let_go(message);
                return DONE;
            }
            Waited::CutShort => {
                met(&channel, "send", CLOSED);
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
    if let Some((channel, listener)) = every::listening(channel) {
        return every::receive(&channel, listener, timeout_ns, value);
    }
    let Some(channel) = find(channel) else { return CLOSED };
    let timeout_ns = match replayed(&channel, "receive") {
        Turn::Live => timeout_ns,
        Turn::Goes => -1,
        Turn::Gives(code) => return code,
    };
    let deadline = deadline(timeout_ns);
    loop {
        let mut queue = channel.queue();
        if let Some(message) = queue.messages.pop_front() {
            met(&channel, "receive", DONE);
            let waiting = std::mem::take(&mut queue.senders);
            drop(queue);
            wake(waiting);
            channel.changed.notify_all();
            *value = i64::from_ne_bytes(taken(message).to_ne_bytes());
            return DONE;
        }
        if queue.closed {
            met(&channel, "receive", CLOSED);
            return CLOSED;
        }
        match wait(&channel, queue, deadline, Side::Receiving) {
            Waited::Again => {}
            Waited::TimedOut => {
                met(&channel, "receive", TIMED_OUT);
                return TIMED_OUT;
            }
            Waited::Stopped => return DONE,
            Waited::CutShort => {
                met(&channel, "receive", CLOSED);
                return CLOSED;
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

/// Waits, in a replay, for the running task's turn to do `what` at `channel`. A replay that's
/// diverged fails, as having found the channel closed.
fn replayed(channel: &Channel, what: &str) -> Turn {
    match crate::replay::turn(channel.replay.as_deref(), what) {
        Ok(None) => Turn::Live,
        Ok(Some(DONE)) => Turn::Goes,
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

/// Says the running task did `what` at `channel`, coming to `outcome`: logged in a recording, and
/// in a replay, the end of its turn.
fn met(channel: &Channel, what: &str, outcome: i64) {
    crate::replay::met(channel.replay.as_deref(), what, outcome);
}

/// `fennec_channel_close(channel)`: no more can be sent. Receivers take what's left, then are told
/// it's closed. Everything waiting on it wakes.
pub extern "C" fn channel_close(channel: i64) {
    let Some(channel) = find(channel) else { return };
    if let Turn::Gives(_) = replayed(&channel, "close") {
        return;
    }
    let mut queue = channel.queue();
    met(&channel, "close", DONE);
    queue.closed = true;
    let mut waiting = std::mem::take(&mut queue.senders);
    waiting.append(&mut queue.receivers);
    drop(queue);
    wake(waiting);
    channel.changed.notify_all();
}

/// `fennec_channel_free(channel)`: its `with` ends, so nothing holds it any more. What's still in it
/// is let go of.
pub extern "C" fn channel_free(channel: i64) {
    every::forget(channel);
    let Some(channel) = channels(|channels| channels.remove(&channel)) else { return };
    let mut queue = channel.queue();
    queue.closed = true;
    let left: Vec<Message> = queue.messages.drain(..).collect();
    drop(queue);
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
    }
}

/// Lets go of a message that was never received.
fn let_go(message: Option<Message>) {
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
fn wait(
    channel: &Arc<Channel>,
    queue: MutexGuard<'_, Queue>,
    deadline: Option<Instant>,
    side: Side,
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
        && let Some((pool, at)) = super::parallel::here()
    {
        let mut queue = queue;
        let token = super::serve::next_token();
        // A crash of a task this one launched wakes it, said before its last look.
        super::threads::Launcher::waiting_here(Box::new(move || {
            pool.run_on(at, Box::new(move |io: &mut super::serve::Io| io.wake_token(token)));
        }));
        if super::threads::Launcher::cut_short_here() {
            super::threads::Launcher::waited_here();
            return Waited::CutShort;
        }
        let waiter = Waiter { pool, at, token };
        match side {
            Side::Sending => queue.senders.push(waiter),
            Side::Receiving => queue.receivers.push(waiter),
        }
        drop(queue);
        let parked = super::serve::park_on(token, deadline);
        super::threads::Launcher::waited_here();
        return match parked {
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
        };
    }
    let _ = side;
    // A crash of a task this code launched wakes it: told with the queue held, so the crash can't
    // come between this last look and the wait.
    #[cfg(any(test, feature = "threads"))]
    {
        let changed = Arc::downgrade(channel);
        super::threads::Launcher::waiting_here(Box::new(move || {
            if let Some(channel) = changed.upgrade() {
                let _queue = channel.queue();
                channel.changed.notify_all();
            }
        }));
        if super::threads::Launcher::cut_short_here() {
            super::threads::Launcher::waited_here();
            return Waited::CutShort;
        }
    }
    // What this thread printed comes out first, as before anything else that waits. The queue stays
    // held, so no change is missed before the wait.
    super::write_kept_output();
    let (_queue, timed_out) = match deadline {
        Some(deadline) => {
            let left = deadline.saturating_duration_since(Instant::now());
            let (queue, waited) =
                channel.changed.wait_timeout(queue, left).unwrap_or_else(PoisonError::into_inner);
            (queue, waited.timed_out())
        }
        None => (channel.changed.wait(queue).unwrap_or_else(PoisonError::into_inner), false),
    };
    #[cfg(any(test, feature = "threads"))]
    super::threads::Launcher::waited_here();
    if timed_out { Waited::TimedOut } else { Waited::Again }
}

/// Wakes parked tasks that were waiting on a channel: each looks again.
fn wake(waiting: Vec<Waiter>) {
    #[cfg(any(test, feature = "threads"))]
    for Waiter { pool, at, token } in waiting {
        pool.run_on(at, Box::new(move |io: &mut super::serve::Io| io.wake_token(token)));
    }
    #[cfg(not(any(test, feature = "threads")))]
    drop(waiting);
}

mod every;
pub use every::{channel_listen, channel_new_every};

#[cfg(test)]
mod tests;
