//! Channels to `.Every`: `with Channel[Hit](size=1024, to=.Every) as hits:`. Every listener gets
//! every message sent after it started listening, and receiving reads as it does from any channel,
//! `for hit in hits`.
//!
//! - **Sending never waits.** The channel keeps the last `size` messages. When it's full, a send lets
//!   the oldest go.
//! - **A listener that falls behind fails loudly,** at its next receive, as a crash does. That's a
//!   listener whose next message has gone. If it asked for `when_behind=.DropOldest`, it goes on
//!   from the oldest message still kept instead.
//! - **Each listener gets its own copy:** a message holding blocks is copied once at `send`, and each
//!   listener that receives it takes a copy of that copy.
//!
//! A listener is a number of its own, given by `fennec_channel_listen`, which `receive` takes in
//! place of the channel's. A listener of a work-queue channel is the channel itself.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use fennec_syntax::{Diagnostic, Span};

use super::super::table::Table;
use super::super::{State, attempt, guarded, with};
use super::{
    CLOSED, Channel, DONE, Message, NEXT, Queue, Side, TIMED_OUT, Waited, channels, deadline, find, wait,
};

/// Where each listener of a channel to `.Every` is up to.
#[derive(Default)]
pub(super) struct Listeners {
    /// The number of the oldest message kept: messages are numbered from 0 as they're sent.
    first: u64,
    by_number: HashMap<i64, Listener>,
}

struct Listener {
    /// The number of the next message it reads.
    next: u64,
    drop_oldest: bool,
}

/// Each listener's channel, by the listener's number.
static LISTENING: Mutex<Option<HashMap<i64, i64>>> = Mutex::new(None);

fn listening_to<T>(use_it: impl FnOnce(&mut HashMap<i64, i64>) -> T) -> T {
    use_it(LISTENING.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert_with(HashMap::new))
}

/// `fennec_channel_new_every(size)`: an open channel to `.Every`, keeping the last `size` messages.
/// A size below 1 fails.
pub extern "C" fn channel_new_every(size: i64) -> i64 {
    let Some(size) = super::size_of(size) else { return 0 };
    let queue = Queue { every: Some(Listeners::default()), ..Queue::default() };
    let channel = Channel {
        size,
        replay: crate::replay::place("c"),
        queue: Mutex::new(queue),
        changed: Condvar::new(),
    };
    let number = NEXT.fetch_add(1, Ordering::Relaxed);
    channels(|channels| channels.insert(number, Arc::new(channel)));
    number
}

/// `fennec_channel_listen(channel, drop_oldest)`: starts listening to a channel to `.Every`, from the
/// next message sent, giving back the number `receive` takes. With `drop_oldest` 1, falling behind
/// skips what's gone instead of failing. Of a work-queue channel, it gives back the channel's own.
pub extern "C" fn channel_listen(channel: i64, drop_oldest: i64) -> i64 {
    let Some(found) = find(channel) else { return channel };
    if let super::Turn::Gives(_) = super::replayed(&found, "listen") {
        return channel;
    }
    let mut queue = found.queue();
    super::met(&found, "listen", DONE);
    let kept = u64::try_from(queue.messages.len()).unwrap_or(0);
    let Some(every) = &mut queue.every else { return channel };
    let number = NEXT.fetch_add(1, Ordering::Relaxed);
    let listener = Listener { next: every.first + kept, drop_oldest: drop_oldest != 0 };
    every.by_number.insert(number, listener);
    drop(queue);
    listening_to(|listening| listening.insert(number, channel));
    number
}

/// The channel a listener listens to, and the listener's number, if `number` is a listener's.
pub(super) fn listening(number: i64) -> Option<(Arc<Channel>, i64)> {
    let channel = listening_to(|listening| listening.get(&number).copied())?;
    Some((find(channel)?, number))
}

/// Its channel is freed: so are its listeners.
pub(super) fn forget(channel: i64) {
    listening_to(|listening| listening.retain(|_, of| *of != channel));
}

/// Adds a message for every listener, letting the oldest go if there's no room: that one's given
/// back, to be let go of without the queue held.
pub(super) fn push(
    queue: &mut MutexGuard<'_, Queue>,
    message: Option<Message>,
    size: usize,
) -> Option<Message> {
    let message = message.map(|message| match message {
        Message::Block(address, table) => Message::Shared(address, Arc::new(*table)),
        message => message,
    });
    queue.messages.extend(message);
    if queue.messages.len() <= size {
        return None;
    }
    let oldest = queue.messages.pop_front();
    if let Some(every) = &mut queue.every {
        every.first += 1;
    }
    oldest
}

/// The next message for `listener`, as `channel_receive` gives back.
pub(super) fn receive(channel: &Arc<Channel>, listener: i64, timeout_ns: i64, value: &mut i64) -> i64 {
    let timeout_ns = match super::replayed(channel, "receive") {
        super::Turn::Live => timeout_ns,
        super::Turn::Goes => -1,
        super::Turn::Gives(code) => return code,
    };
    let deadline = deadline(timeout_ns);
    loop {
        let mut queue = channel.queue();
        let kept = u64::try_from(queue.messages.len()).unwrap_or(0);
        let Some(every) = &mut queue.every else { return CLOSED };
        let first = every.first;
        let Some(this) = every.by_number.get_mut(&listener) else { return CLOSED };
        if this.next < first {
            let missed = first - this.next;
            if !this.drop_oldest {
                drop(queue);
                super::super::fail_with(Diagnostic::new(
                    format!(
                        "this listener fell behind a channel to `.Every`, missing {missed} message{}: listen \
                         with `when_behind=.DropOldest` to skip what it misses",
                        if missed == 1 { "" } else { "s" }
                    ),
                    Span::default(),
                ));
                return DONE;
            }
            this.next = first;
        }
        if this.next < first + kept {
            let at = usize::try_from(this.next - first).unwrap_or(usize::MAX);
            this.next += 1;
            let message = match queue.messages.get(at) {
                Some(Message::Word(word)) => Message::Word(*word),
                Some(Message::Shared(address, table)) => Message::Shared(*address, Arc::clone(table)),
                Some(Message::Block(..)) | None => return CLOSED,
            };
            super::met(channel, "receive", DONE);
            drop(queue);
            *value = i64::from_ne_bytes(super::taken(message).to_ne_bytes());
            return DONE;
        }
        if queue.closed {
            super::met(channel, "receive", CLOSED);
            return CLOSED;
        }
        match wait(channel, queue, deadline, Side::Receiving) {
            Waited::Again => {}
            Waited::TimedOut => {
                super::met(channel, "receive", TIMED_OUT);
                return TIMED_OUT;
            }
            Waited::Stopped => return DONE,
            Waited::CutShort => {
                super::met(channel, "receive", CLOSED);
                return CLOSED;
            }
        }
    }
}

/// A listener's own copy of a message kept for every listener, as this thread's.
pub(super) fn copied_out(address: u64, table: &Arc<Table>) -> u64 {
    let copied = attempt(|state: &mut State| {
        state.heap.reading.push(Arc::clone(table));
        let copy = guarded::copied_for_task(state, address);
        state.heap.reading.pop();
        copy.map(Some)
    });
    let Some((copy, made)) = copied else { return 0 };
    with(|state| state.heap.adopt_made(made.into_blocks()));
    copy
}
