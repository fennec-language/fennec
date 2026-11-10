//! Stopping a program cleanly, for `main` given `Signals`: the signals that ask a program to stop,
//! SIGTERM, SIGINT (Ctrl-C) and SIGHUP on Unix, and Ctrl-C and Ctrl-Break on Windows, are caught
//! from the start of the run, and `signals.stopping()` says one came. A second one ends the program
//! at once, as it would have without Fennec, so a program that's stuck can still be stopped.
//!
//! The handlers are well-vetted crates', `signal-hook` and `ctrlc`, so nothing here is `unsafe`.
//! What runs in a handler is theirs: a flag set, and a byte written for a thread of theirs or ours
//! to wake on.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, Once, PoisonError};

/// Whether a stop has been asked for, set by the handler itself.
static ASKED: LazyLock<Arc<AtomicBool>> = LazyLock::new(|| Arc::new(AtomicBool::new(false)));

/// What a thread waiting for a stop waits on.
static WOKEN: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

/// Whether a stop has been asked for.
pub(crate) fn stopping() -> bool {
    ASKED.load(Ordering::SeqCst)
}

/// Catches the signals that ask a program to stop, the first time it's called.
pub(crate) fn listen() {
    static LISTENING: Once = Once::new();
    LISTENING.call_once(install);
}

/// What to do when a stop's asked for, by a number to forget it by: a server draining.
type OnStop = Box<dyn Fn() + Send + Sync>;
static ON_STOP: Mutex<Vec<(u64, OnStop)>> = Mutex::new(Vec::new());
#[cfg_attr(
    not(any(test, feature = "threads")),
    expect(dead_code, reason = "only a server on the threads runtime drains")
)]
static NEXT_ON_STOP: AtomicU64 = AtomicU64::new(0);

/// Runs `then` when a stop's asked for, on the thread that saw it, or now if one has been. Gives
/// the number to forget it by.
#[cfg_attr(
    not(any(test, feature = "threads")),
    expect(dead_code, reason = "only a server on the threads runtime drains")
)]
pub(crate) fn on_stop(then: OnStop) -> u64 {
    let number = NEXT_ON_STOP.fetch_add(1, Ordering::Relaxed);
    let mut waiting = ON_STOP.lock().unwrap_or_else(PoisonError::into_inner);
    // Looked at under the lock `asked` takes, so a stop is never missed between the two.
    if stopping() {
        drop(waiting);
        then();
    } else {
        waiting.push((number, then));
    }
    number
}

/// Forgets what `on_stop` gave `number`, once there's nothing left for it to do.
#[cfg_attr(
    not(any(test, feature = "threads")),
    expect(dead_code, reason = "only a server on the threads runtime drains")
)]
pub(crate) fn forget(number: u64) {
    ON_STOP.lock().unwrap_or_else(PoisonError::into_inner).retain(|(waiting, _)| *waiting != number);
}

/// Wakes what waits for a stop, once the flag's set, and does what was asked for then.
fn asked() {
    let waiting = {
        let mut waiting = ON_STOP.lock().unwrap_or_else(PoisonError::into_inner);
        // Set under the lock, so `on_stop` either sees it or leaves what it's given here.
        ASKED.store(true, Ordering::SeqCst);
        std::mem::take(&mut *waiting)
    };
    for (_, then) in &waiting {
        then();
    }
    let _held = WOKEN.0.lock().unwrap_or_else(PoisonError::into_inner);
    WOKEN.1.notify_all();
}

#[cfg(unix)]
fn install() {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    let stops = [SIGTERM, SIGINT, SIGHUP];
    for signal in stops {
        // In the order they run: a signal that comes once a stop's been asked for does what it
        // would have without Fennec, ending the program, and the first sets the flag.
        let _ = signal_hook::flag::register_conditional_default(signal, Arc::clone(&ASKED));
        let _ = signal_hook::flag::register(signal, Arc::clone(&ASKED));
    }
    // The handler only sets the flag; this thread wakes what waits for it.
    let Ok(mut caught) = signal_hook::iterator::Signals::new(stops) else { return };
    let _ = std::thread::Builder::new().name("fennec-signals".to_owned()).spawn(move || {
        for _ in caught.forever() {
            asked();
        }
    });
}

#[cfg(windows)]
fn install() {
    // ctrlc runs this on a thread of its own, for Ctrl-C and Ctrl-Break. Closing the console isn't
    // waited for: Windows ends the program as soon as the handler returns.
    let _ = ctrlc::set_handler(|| {
        if ASKED.load(Ordering::SeqCst) {
            // A second one ends the program at once, with the code a Ctrl-C's usually gives.
            std::process::exit(130);
        }
        asked();
    });
}

/// WebAssembly has no signals: a stop is never asked for.
#[cfg(not(any(unix, windows)))]
fn install() {}

/// Waits until a stop is asked for: by parking, for a task, so its thread runs others meanwhile.
/// WebAssembly is never asked to stop, so a wait there fails rather than never ending.
pub(crate) fn wait() -> Result<(), &'static str> {
    if cfg!(target_arch = "wasm32") {
        return Err("a WebAssembly program is never asked to stop, so `wait` would wait forever");
    }
    #[cfg(any(test, feature = "threads"))]
    if crate::embed::serve::can_park() {
        let token = crate::embed::serve::next_token();
        while !stopping() {
            // Looked at again every 50 ms: a stop comes once a run, so waking for it at once
            // isn't worth a way for the handler to reach every worker.
            let soon = std::time::Instant::now() + std::time::Duration::from_millis(50);
            if crate::embed::serve::park_on(token, Some(soon)) == crate::embed::serve::Parked::Cancelled {
                return Ok(());
            }
        }
        return Ok(());
    }
    let mut held = WOKEN.0.lock().unwrap_or_else(PoisonError::into_inner);
    while !stopping() {
        held = WOKEN.1.wait(held).unwrap_or_else(PoisonError::into_inner);
    }
    Ok(())
}
