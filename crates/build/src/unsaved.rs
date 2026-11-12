//! What a command does when the cache can't keep something. The cache only saves work, so the
//! command goes on without it, but it says so: a write that fails without a word looks like a slow
//! build, or a run that can't find what it was given, and hides its cause. It says so once a
//! command, since one failure usually means the next write fails the same way.

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether this command has said the cache couldn't keep something.
static SAID: AtomicBool = AtomicBool::new(false);

/// The result of keeping something in the cache, which `what` names, as "keep the program it
/// linked". A failure is said, once a command, and is None.
pub fn kept<T>(what: &str, result: std::io::Result<T>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            said(what, &error);
            None
        }
    }
}

/// The result of changing something that may be gone already, as another command's cleanup may
/// have taken it: removing it, or marking it used. Its being gone is no failure; anything else is
/// said as [`kept`] says it.
pub fn touched(what: &str, result: std::io::Result<()>) {
    if let Err(error) = result
        && error.kind() != std::io::ErrorKind::NotFound
    {
        said(what, &error);
    }
}

fn said(what: &str, error: &std::io::Error) {
    if !SAID.swap(true, Ordering::Relaxed) {
        // Where even the warning can't be written, there's no one left to tell.
        let _ = writeln!(
            std::io::stderr(),
            "warning: Fennec's cache couldn't {what}: {error}\n  \
             this command goes on without it, and the next one does that work again"
        );
    }
}

/// Whether this command has said an entry didn't match its hash.
static DOUBTED: AtomicBool = AtomicBool::new(false);

/// Whether this command has said the cache folder isn't trusted.
static REFUSED: AtomicBool = AtomicBool::new(false);

/// An entry whose bytes didn't match its hash, read as a miss: said once a command, since it means
/// something tore or changed the cache.
pub(crate) fn doubted(path: &std::path::Path) {
    if !DOUBTED.swap(true, Ordering::Relaxed) {
        // Where even the warning can't be written, there's no one left to tell.
        let _ = writeln!(
            std::io::stderr(),
            "warning: an entry in Fennec's cache doesn't match what was written, so it's made again: {}",
            path.display()
        );
    }
}

/// A cache folder that isn't used, because it isn't its user's alone: said once a command.
pub(crate) fn refused(why: &str) {
    if !REFUSED.swap(true, Ordering::Relaxed) {
        // Where even the warning can't be written, there's no one left to tell.
        let _ = writeln!(
            std::io::stderr(),
            "warning: Fennec's cache isn't used: {why}\n  \
             anyone who can write it could put a program there that the next run starts"
        );
    }
}
