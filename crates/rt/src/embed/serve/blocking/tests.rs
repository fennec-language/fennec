use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use super::Blocking;

/// Long enough for any thread to start and say so on a loaded machine, so only a pool that never
/// runs the work misses it.
const PATIENCE: Duration = Duration::from_secs(20);

fn pool(kept: usize, most: usize, idle: Duration) -> &'static Blocking {
    Box::leak(Box::new(Blocking::new(kept, most, idle)))
}

/// A job that says it started, then blocks until it's let go.
fn held(started: &Sender<usize>, number: usize) -> (super::Job, Sender<()>) {
    let (go, wait): (Sender<()>, Receiver<()>) = channel();
    let started = started.clone();
    let job: super::Job = Box::new(move || {
        let _ = started.send(number);
        let _ = wait.recv_timeout(PATIENCE);
    });
    (job, go)
}

/// Waits until `done` says so, for as long as `PATIENCE`.
fn eventually(done: impl Fn() -> bool) -> bool {
    let until = Instant::now() + PATIENCE;
    while Instant::now() < until {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    done()
}

#[test]
fn a_thread_starts_only_when_work_comes() {
    let pool = pool(4, 8, Duration::from_secs(60));
    assert_eq!(pool.threads(), 0);
    let (started, heard) = channel();
    let (job, go) = held(&started, 0);
    assert!(pool.run(job).is_ok());
    assert_eq!(heard.recv_timeout(PATIENCE), Ok(0));
    assert_eq!(pool.threads(), 1);
    let _ = go.send(());
}

#[test]
fn work_blocking_at_once_gets_a_thread_each_up_to_the_most_then_waits_its_turn() {
    let pool = pool(2, 6, Duration::from_secs(60));
    let (started, heard) = channel();
    let mut held_ones = Vec::new();
    for number in 0..6 {
        let (job, go) = held(&started, number);
        assert!(pool.run(job).is_ok());
        held_ones.push(go);
    }
    // All six run at once: none of them is let go until each has started.
    let mut heard_from: Vec<usize> = (0..6).filter_map(|_| heard.recv_timeout(PATIENCE).ok()).collect();
    heard_from.sort_unstable();
    assert_eq!(heard_from, [0, 1, 2, 3, 4, 5]);
    assert_eq!(pool.threads(), 6);
    // A seventh waits for one of them to finish, with no thread past the most.
    let (job, go) = held(&started, 6);
    assert!(pool.run(job).is_ok());
    assert_eq!(pool.queued(), 1);
    assert_eq!(pool.threads(), 6);
    let _ = held_ones.remove(0).send(());
    assert_eq!(heard.recv_timeout(PATIENCE), Ok(6));
    assert_eq!(pool.threads(), 6);
    for go in held_ones {
        let _ = go.send(());
    }
    let _ = go.send(());
}

#[test]
fn threads_past_the_kept_ones_leave_once_idle() {
    let pool = pool(2, 6, Duration::from_millis(50));
    let (started, heard) = channel();
    let mut held_ones = Vec::new();
    for number in 0..5 {
        let (job, go) = held(&started, number);
        assert!(pool.run(job).is_ok());
        held_ones.push(go);
    }
    for _ in 0..5 {
        assert!(heard.recv_timeout(PATIENCE).is_ok());
    }
    assert_eq!(pool.threads(), 5);
    for go in held_ones {
        let _ = go.send(());
    }
    assert!(eventually(|| pool.threads() == 2), "{} threads stayed", pool.threads());
    // The kept ones stay well past the idle time, and take the next work.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(pool.threads(), 2);
    let (job, go) = held(&started, 9);
    assert!(pool.run(job).is_ok());
    assert_eq!(heard.recv_timeout(PATIENCE), Ok(9));
    assert_eq!(pool.threads(), 2);
    let _ = go.send(());
}

/// The shape that starved the fixed pool: work that waits for other work queued after it. Eight
/// waiters, past the two kept threads, each wait for a sender queued behind them, and all finish
/// because the pool grows past them.
#[test]
fn work_waiting_on_work_queued_behind_it_finishes_below_the_most() {
    let pool = pool(2, 20, Duration::from_secs(60));
    let (finished, done) = channel();
    let mut senders = Vec::new();
    for number in 0..8 {
        let (give, take): (Sender<usize>, Receiver<usize>) = channel();
        let finished = finished.clone();
        assert!(
            pool.run(Box::new(move || {
                let got = take.recv_timeout(PATIENCE).ok();
                let _ = finished.send((number, got));
            }))
            .is_ok()
        );
        senders.push(give);
    }
    for (number, give) in senders.into_iter().enumerate() {
        assert!(
            pool.run(Box::new(move || {
                let _ = give.send(number);
            }))
            .is_ok()
        );
    }
    let mut ended: Vec<(usize, Option<usize>)> =
        (0..8).filter_map(|_| done.recv_timeout(PATIENCE).ok()).collect();
    ended.sort_unstable();
    let wanted: Vec<(usize, Option<usize>)> = (0..8).map(|number| (number, Some(number))).collect();
    assert_eq!(ended, wanted);
}

/// Both callers wait here until each has tried to start a thread, so their starts fail together.
static BOTH_TRYING: std::sync::Barrier = std::sync::Barrier::new(2);

/// A thread start that fails, once both callers have tried.
fn failing_together(_: String, _: super::Job) -> std::io::Result<()> {
    BOTH_TRYING.wait();
    Err(std::io::Error::other("no thread can be started"))
}

/// When thread starts fail at once with no thread running, every queued job comes back to run
/// where it was asked for: the last start to fail takes back them all. Counting a start that never
/// began as a thread, the first to fail left its job for the second's thread, which never came.
#[test]
fn jobs_come_back_when_thread_starts_fail_at_once() {
    let pool: &'static Blocking =
        Box::leak(Box::new(Blocking::starting_with(2, 6, Duration::from_secs(60), failing_together)));
    let (ran, heard) = channel();
    let callers: Vec<_> = (0..2)
        .map(|number| {
            let ran = ran.clone();
            std::thread::spawn(move || {
                let job: super::Job = Box::new(move || {
                    let _ = ran.send(number);
                });
                if let Err(given_back) = pool.run(job) {
                    for job in given_back {
                        job();
                    }
                }
            })
        })
        .collect();
    for caller in callers {
        assert!(caller.join().is_ok());
    }
    let mut ran_ones: Vec<usize> = (0..2).filter_map(|_| heard.recv_timeout(PATIENCE).ok()).collect();
    ran_ones.sort_unstable();
    assert_eq!(ran_ones, [0, 1]);
    assert_eq!(pool.threads(), 0);
    assert_eq!(pool.queued(), 0);
}
