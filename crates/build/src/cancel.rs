//! Whether the build in progress has been asked to stop. The build daemon asks when the command
//! that wanted the build has gone, as when it's interrupted with Ctrl-C: a build in its own process
//! stops with that process, and one the daemon runs for it stops here, between its steps and
//! between the functions it compiles.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set while a build is to stop. The daemon runs one build at a time, so one flag will do.
static CANCELLED: AtomicBool = AtomicBool::new(false);

/// Asks the build in progress to stop.
pub fn cancel() {
    CANCELLED.store(true, Ordering::Relaxed);
}

/// Whether the build in progress has been asked to stop.
#[must_use]
pub fn cancelled() -> bool {
    CANCELLED.load(Ordering::Relaxed)
}

/// Lets the next build run: the daemon clears the flag before each one it starts.
pub fn clear_cancel() {
    CANCELLED.store(false, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_cancel_holds_until_cleared() {
        super::clear_cancel();
        assert!(!super::cancelled());
        super::cancel();
        assert!(super::cancelled());
        super::clear_cancel();
        assert!(!super::cancelled());
    }
}
