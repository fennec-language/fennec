//! `process.run` in a task that can park, on Unix: the task parks while its program runs, rather
//! than holding a blocking thread for as long as the program takes. Only starting the program,
//! which can wait on the disk, goes to a blocking thread.
//!
//! The program's pipes, and on Linux its exit, are registered with a poll of their own. The task
//! parks on that poll's descriptor, which its worker's poll says is ready once any of them is. So
//! one wait covers writing the input, reading both outputs and the program's end, and none of them
//! can hold up another: a program blocked writing to one output while the task waits on the other
//! would never finish.
//!
//! macOS has no way to be told of a program's end without `unsafe`, as kqueue's `EVFILT_PROC` is
//! reached through an `unsafe` call. A program almost always ends as it closes its outputs, so
//! once both are closed the task checks for its end after 1 ms, then at twice the wait each time,
//! up to 50 ms. Only a program that closes its outputs and runs on is checked for long.

use std::io::{ErrorKind, Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use mio::unix::pipe::{Receiver, Sender};
use mio::{Events, Interest, Poll, Token};

use super::{Ending, stop};
use crate::embed::serve::{Parked, park_on_fd};

const STDOUT: Token = Token(0);
const STDERR: Token = Token(1);
const STDIN: Token = Token(2);
#[cfg(target_os = "linux")]
const EXITED: Token = Token(3);

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
        if park_on_fd(running.poll.as_raw_fd(), Interest::READABLE, Some(until)) == Parked::Cancelled {
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
    /// What says the program has ended: a pidfd on Linux, and nothing elsewhere.
    end: Option<std::os::fd::OwnedFd>,
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
        #[cfg(not(target_os = "linux"))]
        let end = None;
        #[cfg(target_os = "linux")]
        let end = {
            let pid = rustix::process::Pid::from_child(child);
            // A pidfd says the program has ended by being ready to read. An older kernel has none,
            // and its end is looked for as on macOS.
            let end = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::NONBLOCK).ok();
            if let Some(end) = &end {
                registry.register(&mut mio::unix::SourceFd(&end.as_raw_fd()), EXITED, Interest::READABLE)?;
            }
            end
        };
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
