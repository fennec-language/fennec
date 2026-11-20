//! Failures put into `std.fs`'s work on the disk, for tests only, so a test can see what a program
//! does with a disk that fills partway through a write. A runtime built with the `faults` feature
//! reads its rules from `FENNEC_FAULTS`; this crate's own tests give each thread its own, with
//! [`put_in`]. A runtime that ships is built with neither: it never reads the variable, and each
//! check here is `Ok(())`.
//!
//! Rules are split by commas, each `work:n:error`, or `write:n:error:bytes`:
//! - `work` is `create` (making the file a write goes into, beside its target), `write` (each
//!   write of the program's bytes), `flush` (a `File`'s, as `flush`, `sync` and its block's end
//!   do), `sync` (a file's, or its folder's after the rename) or `rename` (putting a written file
//!   in its target's place);
//! - `n` is which of that work fails, counted from 1 over the whole run, in the order it's begun;
//! - `error` is `nospace`, `quota`, `readonly`, `denied`, `toolarge` or `io`, or `kill`, which
//!   ends the process there at once, as SIGKILL does (`abort` on Windows), with nothing after it
//!   done: no block's end, no file thrown away;
//! - `bytes`, for a write, is how many of its bytes reach the file before it fails, 0 when left
//!   out, as a write the system cuts short leaves them. A write killed partway puts what's held
//!   back for the file into it first, then those bytes, so they're all there to find after.
//!
//! Each rule fails its work once. A failure put in is made from its kind alone, as one with no
//! code of the system's, so it's said in the same words on every system, and the work is counted
//! the same on each: a folder's sync is counted on Windows too, where there's none to do.

#[cfg(any(test, feature = "faults"))]
use std::io::ErrorKind;
use std::io::Write;

/// A kind of work on the disk that a rule can fail.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Work {
    Create,
    #[cfg_attr(
        not(any(test, feature = "faults")),
        expect(
            dead_code,
            reason = "a runtime that ships fails no write, so its `write_all` never names one"
        )
    )]
    Write,
    Flush,
    Sync,
    Rename,
}

/// Fails `work` here when a rule says to, as the system would have.
#[cfg(any(test, feature = "faults"))]
pub(crate) fn check(work: Work) -> std::io::Result<()> {
    match fails(work) {
        Some(Failed { how: How::Fails(kind), .. }) => Err(std::io::Error::from(kind)),
        Some(Failed { how: How::Killed, .. }) => killed(),
        None => Ok(()),
    }
}

/// `out.write_all(data)`, unless a rule fails this write, after the bytes it lets through.
#[cfg(any(test, feature = "faults"))]
pub(crate) fn write_all(out: &mut impl Write, data: &[u8]) -> std::io::Result<()> {
    match fails(Work::Write) {
        Some(Failed { how, bytes }) => {
            out.write_all(data.get(..bytes.min(data.len())).unwrap_or_default())?;
            match how {
                How::Fails(kind) => Err(std::io::Error::from(kind)),
                How::Killed => {
                    out.flush()?;
                    killed()
                }
            }
        }
        None => out.write_all(data),
    }
}

/// Ends the process at once, as a kill from outside does, so nothing after it runs.
#[cfg(any(test, feature = "faults"))]
fn killed() -> ! {
    // A kill sent to the process may land on another of its threads after `kill` returns, so this
    // one waits for it, doing nothing more.
    #[cfg(unix)]
    if rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::KILL).is_ok() {
        loop {
            std::thread::park();
        }
    }
    // Windows ends a process that aborts at once, running nothing of it, as TerminateProcess does.
    std::process::abort()
}

/// A runtime that ships has no rules, so nothing fails here.
#[cfg(not(any(test, feature = "faults")))]
#[expect(clippy::unnecessary_wraps, reason = "a test build's `check` fails work here, in the same calls")]
#[inline]
pub(crate) fn check(_work: Work) -> std::io::Result<()> {
    Ok(())
}

/// A runtime that ships writes all of it.
#[cfg(not(any(test, feature = "faults")))]
#[inline]
pub(crate) fn write_all(out: &mut impl Write, data: &[u8]) -> std::io::Result<()> {
    out.write_all(data)
}

/// Whether the rules can be read, checked before `main` runs, so a test never runs believing it
/// put a failure in.
#[cfg(all(feature = "faults", not(test)))]
pub(crate) fn checked() -> Result<(), String> {
    from_variable::RULES.as_ref().map(|_| ()).map_err(Clone::clone)
}

/// Nothing to read: a runtime that ships has no rules, and this crate's tests put theirs in.
#[cfg(not(all(feature = "faults", not(test))))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "a runtime built with `faults` fails here, for rules it can't read"
)]
#[inline]
pub(crate) fn checked() -> Result<(), String> {
    Ok(())
}

/// What fails a piece of work: how, and for a write, the bytes it lets through.
#[cfg(any(test, feature = "faults"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Failed {
    how: How,
    bytes: usize,
}

/// How a piece of work fails: as the system fails it, or with the process killed there.
#[cfg(any(test, feature = "faults"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum How {
    Fails(ErrorKind),
    Killed,
}

#[cfg(all(feature = "faults", not(test)))]
fn fails(work: Work) -> Option<Failed> {
    from_variable::fails(work)
}

#[cfg(test)]
fn fails(work: Work) -> Option<Failed> {
    on_this_thread::fails(work)
}

#[cfg(test)]
pub(crate) use on_this_thread::put_in;

/// One rule: the nth of `work` fails as `failed` says.
#[cfg(any(test, feature = "faults"))]
#[derive(Debug, PartialEq, Eq)]
struct Rule {
    work: Work,
    nth: u64,
    failed: Failed,
}

/// The rules `spec` says, or what's wrong with it, naming the rule.
#[cfg(any(test, feature = "faults"))]
fn read(spec: &str) -> Result<Vec<Rule>, String> {
    spec.split(',').map(str::trim).filter(|rule| !rule.is_empty()).map(read_rule).collect()
}

#[cfg(any(test, feature = "faults"))]
fn read_rule(rule: &str) -> Result<Rule, String> {
    let wrong = |why: &str| format!("FENNEC_FAULTS: `{rule}` {why}");
    let parts: Vec<&str> = rule.split(':').collect();
    let (work, nth, error, bytes) = match parts.as_slice() {
        [work, nth, error] => (*work, *nth, *error, None),
        [work, nth, error, bytes] => (*work, *nth, *error, Some(*bytes)),
        _ => return Err(wrong("isn't `work:n:error` or `write:n:error:bytes`")),
    };
    let work = match work {
        "create" => Work::Create,
        "write" => Work::Write,
        "flush" => Work::Flush,
        "sync" => Work::Sync,
        "rename" => Work::Rename,
        _ => return Err(wrong("names no work: it's create, write, flush, sync or rename")),
    };
    let nth = match nth.parse::<u64>() {
        Ok(nth) if nth > 0 => nth,
        _ => return Err(wrong("has no count from 1")),
    };
    let how = match error {
        "nospace" => How::Fails(ErrorKind::StorageFull),
        "quota" => How::Fails(ErrorKind::QuotaExceeded),
        "readonly" => How::Fails(ErrorKind::ReadOnlyFilesystem),
        "denied" => How::Fails(ErrorKind::PermissionDenied),
        "toolarge" => How::Fails(ErrorKind::FileTooLarge),
        "io" => How::Fails(ErrorKind::Other),
        "kill" => How::Killed,
        _ => {
            return Err(wrong("names no error: it's nospace, quota, readonly, denied, toolarge, io or kill"));
        }
    };
    let bytes = match (work, bytes) {
        (_, None) => 0,
        (Work::Write, Some(bytes)) => bytes.parse::<usize>().map_err(|_| wrong("has no count of bytes"))?,
        (_, Some(_)) => return Err(wrong("gives bytes, which only a write takes")),
    };
    Ok(Rule { work, nth, failed: Failed { how, bytes } })
}

/// The rules of a runtime built with `faults`, from `FENNEC_FAULTS`, counted over the whole run.
#[cfg(all(feature = "faults", not(test)))]
mod from_variable {
    use std::sync::LazyLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{Failed, Rule, Work, read};

    pub(super) static RULES: LazyLock<Result<Vec<Rule>, String>> =
        LazyLock::new(|| match std::env::var("FENNEC_FAULTS") {
            Ok(spec) => read(&spec),
            Err(std::env::VarError::NotPresent) => Ok(Vec::new()),
            Err(std::env::VarError::NotUnicode(_)) => Err("FENNEC_FAULTS isn't UTF-8".to_owned()),
        });

    /// How much of each kind of work has begun, by `Work`'s order.
    static BEGUN: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

    /// Counts `work` as begun, and says how it fails, if a rule fails it. Rules that can't be read
    /// stopped the run before `main`, so there are none here.
    pub(super) fn fails(work: Work) -> Option<Failed> {
        let rules = RULES.as_ref().ok()?;
        let nth = BEGUN.get(work as usize)?.fetch_add(1, Ordering::SeqCst) + 1;
        rules.iter().find(|rule| rule.work == work && rule.nth == nth).map(|rule| rule.failed)
    }
}

/// The rules of this crate's tests: each thread its own, counted from when they're put in, so
/// tests running at once never count each other's work.
#[cfg(test)]
mod on_this_thread {
    use std::cell::RefCell;

    use super::{Failed, Rule, Work, read};

    thread_local! {
        static RULES: RefCell<(Vec<Rule>, [u64; 5])> = const { RefCell::new((Vec::new(), [0; 5])) };
    }

    /// Puts `spec`'s rules in for this thread, in place of any before, with nothing counted.
    pub(crate) fn put_in(spec: &str) {
        let rules = read(spec).unwrap_or_else(|why| panic!("{why}"));
        RULES.with_borrow_mut(|kept| *kept = (rules, [0; 5]));
    }

    pub(super) fn fails(work: Work) -> Option<Failed> {
        RULES.with_borrow_mut(|(rules, begun)| {
            let nth = begun.get_mut(work as usize)?;
            *nth += 1;
            rules.iter().find(|rule| rule.work == work && rule.nth == *nth).map(|rule| rule.failed)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use super::{Failed, How, Rule, Work, read};

    #[test]
    fn rules_read_as_they_say() {
        assert_eq!(
            read("write:3:nospace:10, rename:1:io,write:2:kill:4"),
            Ok(vec![
                Rule {
                    work: Work::Write,
                    nth: 3,
                    failed: Failed { how: How::Fails(ErrorKind::StorageFull), bytes: 10 }
                },
                Rule {
                    work: Work::Rename,
                    nth: 1,
                    failed: Failed { how: How::Fails(ErrorKind::Other), bytes: 0 }
                },
                Rule { work: Work::Write, nth: 2, failed: Failed { how: How::Killed, bytes: 4 } },
            ])
        );
        assert_eq!(read(""), Ok(Vec::new()));
    }

    #[test]
    fn a_rule_that_cant_be_read_is_named() {
        for (spec, why) in [
            ("write:3", "isn't `work:n:error`"),
            ("seek:1:io", "names no work"),
            ("write:0:io", "has no count from 1"),
            ("write:x:io", "has no count from 1"),
            ("write:1:full", "names no error"),
            ("write:1:io:many", "has no count of bytes"),
            ("sync:1:io:4", "only a write takes"),
        ] {
            let said = read(spec).err().unwrap_or_default();
            assert!(said.contains(&format!("`{spec}`")) && said.contains(why), "{spec}: {said}");
        }
    }
}
