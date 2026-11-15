//! `process.run` in a task that can park, on Unix: the task parks while its program runs, rather
//! than holding a blocking thread for as long as the program takes. Only starting the program,
//! which can wait on the disk, goes to a blocking thread.
//!
//! The program's pipes, and what says it has ended, are registered with a poll of their own: a
//! pidfd on Linux, and on macOS a kqueue of its own told of the program's exit, kqueue's
//! `EVFILT_PROC`, through nix's safe `Kqueue`. The task parks on that poll's descriptor, which its
//! worker's poll says is ready once any of them is. So one wait covers writing the input, reading
//! both outputs and the program's end, and none of them can hold up another: a program blocked
//! writing to one output while the task waits on the other would never finish.
//!
//! Where nothing can say when the program ends, on a Linux older than pidfds or if making one
//! fails, the task looks for its end once both outputs are closed: after 1 ms, then at twice the
//! wait each time, up to 50 ms. A program almost always ends as it closes its outputs, so only one
//! that closes them and runs on is looked at for long.

use std::io::{ErrorKind, Read as _, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use mio::unix::pipe::{Receiver, Sender};
use mio::{Events, Interest, Poll, Token};

use super::{Ending, stop};
use crate::embed::serve::{Parked, next_token, park_on_fd};

const STDOUT: Token = Token(0);
const STDERR: Token = Token(1);
const STDIN: Token = Token(2);
const EXITED: Token = Token(3);

/// What's ready to read once a program has ended: a pidfd on Linux, and on macOS a kqueue of its
/// own, told of the program's exit.
#[cfg(not(target_os = "macos"))]
type End = std::os::fd::OwnedFd;
#[cfg(target_os = "macos")]
type End = nix::sys::event::Kqueue;

/// What says `child` has ended, or `None` where this system can't say, or saying failed. A
/// program that has ended already has nothing to say it, and is found ended when it's looked for.
#[cfg(target_os = "linux")]
fn end_of(child: &Child) -> Option<End> {
    let pid = rustix::process::Pid::from_child(child);
    rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::NONBLOCK).ok()
}

/// As on Linux, through a kqueue holding `EVFILT_PROC` for the program, which is ready to read once
/// the program has exited. It's added and nothing taken, so `kevent` doesn't wait.
#[cfg(target_os = "macos")]
fn end_of(child: &Child) -> Option<End> {
    use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};
    let kqueue = Kqueue::new().ok()?;
    let pid = usize::try_from(child.id()).ok()?;
    let flags = EvFlags::EV_ADD | EvFlags::EV_ONESHOT;
    let exited = KEvent::new(pid, EventFilter::EVFILT_PROC, flags, FilterFlag::NOTE_EXIT, 0, 0);
    kqueue.kevent(&[exited], &mut [], None).ok()?;
    Some(kqueue)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn end_of(_: &Child) -> Option<End> {
    None
}

/// The first wait before looking again whether a program whose outputs have closed has ended, and
/// the longest, where its end can't be watched for.
const FIRST_LOOK: Duration = Duration::from_millis(1);
const LONGEST_LOOK: Duration = Duration::from_millis(50);

/// Runs `command` as `run_limited` does, parked while it waits: its input written, both outputs
/// read, at most `most` bytes of each kept, and stopped, with what it started, past `timeout`. A
/// task stopped while its program runs stops the program too.
pub(super) fn run(
    mut command: Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    most: usize,
) -> std::io::Result<Ending> {
    let stdin = if input.is_some() { Stdio::piped() } else { Stdio::null() };
    command.stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::piped());
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let deadline = Instant::now() + timeout;
    let mut child = crate::stdlib::fs::blocking(move || command.spawn())?;
    let mut running = match Running::watching(&mut child, input) {
        Ok(running) => running,
        Err(error) => {
            stop(&mut child);
            return Err(error);
        }
    };
    let mut look = FIRST_LOOK;
    loop {
        if let Err(error) = running.move_bytes(most) {
            stop(&mut child);
            return Err(error);
        }
        if running.past(most) {
            stop(&mut child);
            return Ok(Ending::TooMuchOutput);
        }
        if running.outputs_closed()
            && let Some(status) = child.try_wait()?
        {
            let (stdout, stderr) = running.outputs();
            return Ok(Ending::Done(std::process::Output { status, stdout, stderr }));
        }
        let now = Instant::now();
        if now >= deadline {
            stop(&mut child);
            return Ok(Ending::TimedOut);
        }
        // Where its end can't be watched for, a program whose outputs have closed is looked at
        // again soon, and less often the longer it runs on.
        let until = if running.outputs_closed() && !running.end_watched() {
            let soon = now + look;
            look = (look * 2).min(LONGEST_LOOK);
            soon.min(deadline)
        } else {
            deadline
        };
        if park_on_fd(next_token(), running.poll.as_raw_fd(), Interest::READABLE, Some(until))
            == Parked::Cancelled
        {
            stop(&mut child);
            return Err(std::io::Error::new(ErrorKind::Interrupted, "its task was stopped"));
        }
        // What woke the task is taken, so its poll isn't ready again until something new happens.
        running.poll.poll(&mut running.events, Some(Duration::ZERO))?;
    }
}

/// A running program's pipes, registered with a poll of their own, and what's come out of them.
struct Running {
    poll: Poll,
    events: Events,
    stdout: Option<Receiver>,
    stderr: Option<Receiver>,
    /// The input, how much of it's written, and the pipe it goes to, until it's all written or the
    /// program stops reading.
    stdin: Option<(Sender, Vec<u8>, usize)>,
    /// What says the program has ended, where something can.
    end: Option<End>,
    out: Vec<u8>,
    err: Vec<u8>,
    /// Where a read lands before it's kept: on the heap, as a task's stack is small.
    chunk: Vec<u8>,
}

impl Running {
    fn watching(child: &mut Child, input: Option<Vec<u8>>) -> std::io::Result<Self> {
        let poll = Poll::new()?;
        let registry = poll.registry();
        let mut stdout = child.stdout.take().map(Receiver::from);
        let mut stderr = child.stderr.take().map(Receiver::from);
        for (pipe, token) in [(&mut stdout, STDOUT), (&mut stderr, STDERR)] {
            if let Some(pipe) = pipe {
                pipe.set_nonblocking(true)?;
                registry.register(pipe, token, Interest::READABLE)?;
            }
        }
        let mut stdin = None;
        if let Some((pipe, input)) = child.stdin.take().map(Sender::from).zip(input) {
            pipe.set_nonblocking(true)?;
            let mut pipe = pipe;
            registry.register(&mut pipe, STDIN, Interest::WRITABLE)?;
            stdin = Some((pipe, input, 0));
        }
        let end = end_of(child);
        if let Some(end) = &end {
            registry.register(
                &mut mio::unix::SourceFd(&end.as_fd().as_raw_fd()),
                EXITED,
                Interest::READABLE,
            )?;
        }
        Ok(Self {
            poll,
            events: Events::with_capacity(8),
            stdout,
            stderr,
            stdin,
            end,
            out: Vec::new(),
            err: Vec::new(),
            chunk: vec![0; 64 * 1024],
        })
    }

    /// Writes what input it can and reads what output there is, until each would wait. An output
    /// that's closed, and an input the program stopped reading, are let go of.
    fn move_bytes(&mut self, most: usize) -> std::io::Result<()> {
        if let Some((pipe, input, written)) = &mut self.stdin {
            let finished = loop {
                let Some(left) = input.get(*written..).filter(|left| !left.is_empty()) else { break true };
                match pipe.write(left) {
                    Ok(0) => break true,
                    Ok(wrote) => *written += wrote,
                    Err(error) if error.kind() == ErrorKind::Interrupted => {}
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break false,
                    // A program that stops reading leaves the rest unwritten, which isn't an error.
                    Err(_) => break true,
                }
            };
            if finished && let Some((mut pipe, _, _)) = self.stdin.take() {
                let _ = self.poll.registry().deregister(&mut pipe);
            }
        }
        for (pipe, kept) in [(&mut self.stdout, &mut self.out), (&mut self.stderr, &mut self.err)] {
            let Some(open) = pipe else { continue };
            if read_while_ready(open, kept, most, &mut self.chunk)?
                && let Some(mut closed) = pipe.take()
            {
                let _ = self.poll.registry().deregister(&mut closed);
            }
        }
        Ok(())
    }

    /// Whether either output has gone past the most kept.
    fn past(&self, most: usize) -> bool {
        self.out.len() > most || self.err.len() > most
    }

    fn outputs_closed(&self) -> bool {
        self.stdout.is_none() && self.stderr.is_none()
    }

    /// Whether the program's end wakes the task, so it needn't look for it.
    fn end_watched(&self) -> bool {
        self.end.is_some()
    }

    fn outputs(&mut self) -> (Vec<u8>, Vec<u8>) {
        (std::mem::take(&mut self.out), std::mem::take(&mut self.err))
    }
}

/// Reads `pipe` into `kept`, through `chunk`, until it would wait, keeping at most one byte past
/// `most`. Gives whether it's closed.
fn read_while_ready(
    pipe: &mut Receiver,
    kept: &mut Vec<u8>,
    most: usize,
    chunk: &mut [u8],
) -> std::io::Result<bool> {
    loop {
        match pipe.read(chunk) {
            Ok(0) => return Ok(true),
            Ok(read) => {
                // One byte past the most, to tell it went past.
                let room = (most + 1).saturating_sub(kept.len());
                kept.extend_from_slice(chunk.get(..read.min(room)).unwrap_or_default());
                if kept.len() > most {
                    return Ok(false);
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::Duration;

    use mio::{Events, Interest, Poll, Token};

    use super::end_of;

    /// What says a program has ended is ready to read once it has, and not before: on Linux and
    /// macOS a parked run is woken by its program's end, never looking for it.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn a_programs_end_wakes_its_poll() {
        use std::os::fd::{AsFd as _, AsRawFd as _};
        let mut child = Command::new("sleep").arg("0.3").spawn().expect("sleep starts");
        let end = end_of(&child).expect("this system says when a program ends");
        let mut poll = Poll::new().expect("a poll");
        let fd = end.as_fd().as_raw_fd();
        poll.registry()
            .register(&mut mio::unix::SourceFd(&fd), Token(7), Interest::READABLE)
            .expect("registered");
        let mut events = Events::with_capacity(4);
        poll.poll(&mut events, Some(Duration::ZERO)).expect("polled");
        assert!(events.is_empty(), "ready while the program still runs");
        poll.poll(&mut events, Some(Duration::from_secs(20))).expect("polled");
        assert!(events.iter().any(|event| event.token() == Token(7)), "never ready");
        assert!(child.try_wait().expect("looked at").is_some(), "ready before the program ended");
    }
}
