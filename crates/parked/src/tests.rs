use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{Request, Scheduler, Step, TaskId, Wake, park};

/// Small stacks, as servers use: plenty for these tasks.
const STACK: usize = 64 * 1024;

/// A sequence of numbers from a seed, the same on every machine.
struct Seeded(u64);

impl Seeded {
    fn next(&mut self) -> u64 {
        // A 64-bit LCG, taking its top bits, which are the well-mixed ones.
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next()).unwrap() % n
    }
}

/// What the task the test calls `key` asks for on its `step`th park, and what it's woken with.
fn request(key: u64, step: u64) -> Request {
    key * 1_000 + step
}

fn answer(request: Request) -> Wake {
    Wake::Ready(request ^ 0x5eed)
}

/// A task that parks `parks` times, checking each time that it's woken with its own answer and
/// that what it keeps on its stack is as it left it. It gives back its key.
fn checked_task(key: u64, parks: u64) -> impl FnOnce() -> i64 {
    move || {
        let kept: [u64; 16] = std::array::from_fn(|at| key.wrapping_mul(31).wrapping_add(at as u64));
        for step in 0..parks {
            let asked = request(key, step);
            assert_eq!(park(asked), Some(answer(asked)), "task {key} woken with another's answer");
            let again: [u64; 16] = std::array::from_fn(|at| key.wrapping_mul(31).wrapping_add(at as u64));
            assert_eq!(kept, again, "task {key}'s stack changed while it was parked");
        }
        i64::try_from(key).unwrap()
    }
}

/// Resumes `task` until it ends, answering each park as `checked_task` expects.
fn finish(scheduler: &mut Scheduler, task: TaskId, mut wake: Wake) -> i64 {
    loop {
        match scheduler.resume(task, wake) {
            Step::Parked(asked) => wake = answer(asked),
            Step::Done(result) => return result,
            Step::Unknown => unreachable!(),
        }
    }
}

#[test]
fn thousands_of_tasks_take_turns_on_one_thread_in_seeded_orders() {
    for seed in [1, 2, 3, 0xfeed, 0xdead_beef] {
        let mut order = Seeded(seed);
        let mut scheduler = Scheduler::new(STACK);
        let mut parked: Vec<(TaskId, u64, Request)> = Vec::new();
        let mut ended = 0;
        for key in 1..=2_000 {
            let parks = u64::try_from(order.below(6)).unwrap();
            let task = scheduler.spawn(checked_task(key, parks)).unwrap();
            match scheduler.resume(task, Wake::Ready(0)) {
                Step::Parked(asked) => parked.push((task, key, asked)),
                Step::Done(result) => {
                    assert_eq!(result, i64::try_from(key).unwrap());
                    ended += 1;
                }
                Step::Unknown => unreachable!(),
            }
            // Now and then, resume some parked tasks in a random order.
            for _ in 0..order.below(3) {
                if parked.is_empty() {
                    break;
                }
                let (task, key, asked) = parked.swap_remove(order.below(parked.len()));
                match scheduler.resume(task, answer(asked)) {
                    Step::Parked(next) => {
                        assert_eq!(next, asked + 1);
                        parked.push((task, key, next));
                    }
                    Step::Done(result) => {
                        assert_eq!(result, i64::try_from(key).unwrap());
                        ended += 1;
                    }
                    Step::Unknown => unreachable!(),
                }
            }
        }
        while !parked.is_empty() {
            let (task, key, asked) = parked.swap_remove(order.below(parked.len()));
            match scheduler.resume(task, answer(asked)) {
                Step::Parked(next) => parked.push((task, key, next)),
                Step::Done(_) => ended += 1,
                Step::Unknown => unreachable!(),
            }
        }
        assert_eq!(ended, 2_000);
        assert!(scheduler.is_empty());
        assert_eq!(scheduler.len(), 0);
    }
}

#[test]
fn outside_a_task_park_asks_the_caller_to_wait_itself() {
    assert_eq!(park(7), None);
    let mut scheduler = Scheduler::new(STACK);
    assert!(scheduler.is_empty());
    let task = scheduler.spawn(|| 0).unwrap();
    assert!(!scheduler.is_empty());
    assert_eq!(scheduler.len(), 1);
    assert_eq!(scheduler.resume(task, Wake::Ready(0)), Step::Done(0));
    assert!(scheduler.is_empty());
    // After a task, the thread is a plain thread again.
    assert_eq!(park(7), None);
    assert_eq!(scheduler.resume(task, Wake::Ready(0)), Step::Unknown);
    assert_eq!(scheduler.resume(12_345_678, Wake::Ready(0)), Step::Unknown);
}

#[test]
fn a_task_ends_while_others_stay_parked() {
    let mut scheduler = Scheduler::new(STACK);
    let first = scheduler.spawn(checked_task(1, 3)).unwrap();
    let second = scheduler.spawn(|| 2).unwrap();
    let third = scheduler.spawn(checked_task(3, 2)).unwrap();
    assert_eq!(scheduler.resume(first, Wake::Ready(0)), Step::Parked(request(1, 0)));
    assert_eq!(scheduler.resume(third, Wake::Ready(0)), Step::Parked(request(3, 0)));
    assert_eq!(scheduler.resume(second, Wake::Ready(0)), Step::Done(2));
    assert_eq!(scheduler.len(), 2);
    assert_eq!(finish(&mut scheduler, third, answer(request(3, 0))), 3);
    assert_eq!(finish(&mut scheduler, first, answer(request(1, 0))), 1);
    assert!(scheduler.is_empty());
}

/// What a connection's wake means, as a server gives it.
const CLOSED: u64 = 2;

#[test]
fn a_task_whose_connection_closes_while_parked_ends_with_a_failure() {
    let mut scheduler = Scheduler::new(STACK);
    let task = scheduler.spawn(|| match park(1) {
        Some(Wake::Ready(CLOSED)) => 1,
        _ => 0,
    });
    let task = task.unwrap();
    assert_eq!(scheduler.resume(task, Wake::Ready(0)), Step::Parked(1));
    assert_eq!(scheduler.resume(task, Wake::Ready(CLOSED)), Step::Done(1));
}

#[test]
fn a_failure_in_a_parked_task_ends_only_that_task() {
    let mut scheduler = Scheduler::new(STACK);
    let failing = scheduler
        .spawn(|| {
            park(1);
            1
        })
        .unwrap();
    let other = scheduler.spawn(checked_task(2, 2)).unwrap();
    assert_eq!(scheduler.resume(failing, Wake::Ready(0)), Step::Parked(1));
    assert_eq!(scheduler.resume(other, Wake::Ready(0)), Step::Parked(request(2, 0)));
    assert_eq!(scheduler.resume(failing, Wake::Ready(0)), Step::Done(1));
    assert_eq!(finish(&mut scheduler, other, answer(request(2, 0))), 2);
}

#[test]
fn a_cancelled_task_is_told_so_and_runs_to_its_end_without_parking_again() {
    let mut scheduler = Scheduler::new(STACK);
    let woken = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&woken);
    let task = scheduler
        .spawn(move || {
            for asked in 1..=3 {
                seen.borrow_mut().push(park(asked));
            }
            7
        })
        .unwrap();
    assert_eq!(scheduler.resume(task, Wake::Ready(0)), Step::Parked(1));
    assert_eq!(scheduler.cancel(task), Step::Done(7));
    assert_eq!(*woken.borrow(), [Some(Wake::Cancelled); 3]);
    assert_eq!(scheduler.cancel(task), Step::Unknown);
}

/// Parks when dropped, as closing a socket that flushes what's left might.
struct ParksWhenDropped(Rc<RefCell<Vec<Option<Wake>>>>);

impl Drop for ParksWhenDropped {
    fn drop(&mut self) {
        self.0.borrow_mut().push(park(9));
    }
}

#[test]
fn a_destructor_that_parks_while_its_task_is_cancelled_is_told_so_at_once() {
    let mut scheduler = Scheduler::new(STACK);
    let woken = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&woken);
    let task = scheduler
        .spawn(move || {
            let _socket = ParksWhenDropped(seen);
            while park(1) != Some(Wake::Cancelled) {}
            0
        })
        .unwrap();
    assert_eq!(scheduler.resume(task, Wake::Ready(0)), Step::Parked(1));
    assert_eq!(scheduler.cancel(task), Step::Done(0));
    assert_eq!(*woken.borrow(), [Some(Wake::Cancelled)]);
}

#[test]
fn dropping_a_scheduler_cancels_its_parked_and_unstarted_tasks() {
    let ended = Rc::new(Cell::new(0));
    {
        let mut scheduler = Scheduler::new(STACK);
        for at in 0..100 {
            let ended = Rc::clone(&ended);
            let task = scheduler
                .spawn(move || {
                    while park(1) != Some(Wake::Cancelled) {}
                    ended.set(ended.get() + 1);
                    0
                })
                .unwrap();
            if at % 2 == 0 {
                assert_eq!(scheduler.resume(task, Wake::Ready(0)), Step::Parked(1));
            }
        }
    }
    assert_eq!(ended.get(), 100);
    assert_eq!(park(7), None);
}

thread_local! {
    static KEPT: RefCell<Option<Scheduler>> = const { RefCell::new(None) };
}

#[test]
fn a_scheduler_dropped_as_its_thread_exits_runs_its_tasks_to_their_end() {
    let ended = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&ended);
    std::thread::spawn(move || {
        let mut scheduler = Scheduler::new(STACK);
        for _ in 0..10 {
            let ended = Arc::clone(&counted);
            let task = scheduler
                .spawn(move || {
                    while park(1) != Some(Wake::Cancelled) {}
                    ended.fetch_add(1, Ordering::Relaxed);
                    0
                })
                .unwrap();
            assert_eq!(scheduler.resume(task, Wake::Ready(0)), Step::Parked(1));
        }
        KEPT.set(Some(scheduler));
    })
    .join()
    .unwrap();
    assert_eq!(ended.load(Ordering::Relaxed), 10);
}

/// A runtime function native code calls, which parks from inside it.
extern "C" fn read_from_the_network(asked: u64) -> u64 {
    match park(asked) {
        Some(Wake::Ready(got)) => got,
        _ => 0,
    }
}

/// Native code calling the runtime, a few calls deep.
extern "C" fn native_code(depth: u64, asked: u64) -> u64 {
    if depth == 0 { read_from_the_network(asked) } else { native_code(depth - 1, asked) + 1 }
}

#[test]
fn a_task_parks_from_deep_inside_calls_and_inside_another_scheduler_s_task() {
    let mut outer = Scheduler::new(STACK);
    let task = outer
        .spawn(|| {
            let mut got = native_code(5, 10);
            // A task running tasks of its own: they park and resume inside it, and it parks
            // between them.
            let mut inner = Scheduler::new(STACK);
            let nested = inner.spawn(|| i64::try_from(native_code(3, 20)).unwrap()).unwrap();
            assert_eq!(inner.resume(nested, Wake::Ready(0)), Step::Parked(20));
            got += native_code(0, 30);
            assert_eq!(inner.resume(nested, Wake::Ready(200)), Step::Done(203));
            got += native_code(2, 40);
            i64::try_from(got).unwrap()
        })
        .unwrap();
    assert_eq!(outer.resume(task, Wake::Ready(0)), Step::Parked(10));
    assert_eq!(outer.resume(task, Wake::Ready(100)), Step::Parked(30));
    assert_eq!(outer.resume(task, Wake::Ready(300)), Step::Parked(40));
    assert_eq!(outer.resume(task, Wake::Ready(400)), Step::Done(105 + 300 + 402));
}

#[test]
fn an_inner_task_is_resumed_later_by_a_different_parent() {
    let inner = Rc::new(RefCell::new(Scheduler::new(STACK)));
    let nested = inner.borrow_mut().spawn(|| i64::try_from(native_code(1, 20) + native_code(0, 21)).unwrap());
    let nested = nested.unwrap();
    let mut outer = Scheduler::new(STACK);
    let started_by = Rc::clone(&inner);
    let first = outer
        .spawn(move || {
            assert_eq!(started_by.borrow_mut().resume(nested, Wake::Ready(0)), Step::Parked(20));
            i64::try_from(native_code(0, 1)).unwrap()
        })
        .unwrap();
    let finished_by = Rc::clone(&inner);
    let second = outer
        .spawn(move || {
            assert_eq!(finished_by.borrow_mut().resume(nested, Wake::Ready(200)), Step::Parked(21));
            let got = native_code(0, 2);
            assert_eq!(finished_by.borrow_mut().resume(nested, Wake::Ready(300)), Step::Done(501));
            i64::try_from(got).unwrap()
        })
        .unwrap();
    assert_eq!(outer.resume(first, Wake::Ready(0)), Step::Parked(1));
    assert_eq!(outer.resume(second, Wake::Ready(0)), Step::Parked(2));
    assert_eq!(outer.resume(first, Wake::Ready(10)), Step::Done(10));
    assert_eq!(outer.resume(second, Wake::Ready(20)), Step::Done(20));
    assert!(inner.borrow().is_empty());
}

#[test]
fn task_numbers_are_never_given_twice_on_a_thread() {
    let mut scheduler = Scheduler::new(STACK);
    let first = scheduler.spawn(|| 0).unwrap();
    assert_eq!(scheduler.resume(first, Wake::Ready(0)), Step::Done(0));
    let second = scheduler.spawn(|| 0).unwrap();
    let mut other = Scheduler::new(STACK);
    let third = other.spawn(|| 0).unwrap();
    assert!(first != second && second != third && first != third);
    assert_eq!(scheduler.resume(first, Wake::Ready(0)), Step::Unknown);
    assert_eq!(other.resume(second, Wake::Ready(0)), Step::Unknown);
    assert_eq!(scheduler.resume(second, Wake::Ready(0)), Step::Done(0));
    assert_eq!(other.resume(third, Wake::Ready(0)), Step::Done(0));
}

/// Set in a copy of this test program run to stop the process on purpose.
const STOP: &str = "FENNEC_PARKED_STOP";

/// What a copy of this test program does to stop the process, by name.
fn stop_the_process(how: &str) {
    let mut scheduler = Scheduler::new(STACK);
    match how {
        // Another task marked as running while this one parks.
        "another task parks" => {
            let task = scheduler
                .spawn(|| {
                    super::current::pretend_running(999_999);
                    park(1);
                    0
                })
                .unwrap();
            scheduler.resume(task, Wake::Ready(0));
        }
        // Another task marked as running while this one is resumed, or starts.
        "another task is resumed" | "another task starts" => {
            let task = scheduler.spawn(|| i64::try_from(native_code(0, 1)).unwrap()).unwrap();
            if how == "another task is resumed" {
                scheduler.resume(task, Wake::Ready(0));
            }
            let mut running = scheduler.tasks.remove(&task).unwrap();
            let _running = super::current::Running::start(999_999, running.stack, false);
            running.coroutine.resume(Wake::Ready(0));
        }
        // A task finding an outer task's records, number and all, while it runs on its own
        // stack: only the stack check tells them apart.
        "an outer task's records" => {
            let task = scheduler
                .spawn(|| {
                    let (current, resumed) = super::current::records();
                    let mut inner = Scheduler::new(STACK);
                    let nested = inner
                        .spawn(move || {
                            super::current::overwrite(current, resumed);
                            park(1);
                            0
                        })
                        .unwrap();
                    inner.resume(nested, Wake::Ready(0));
                    0
                })
                .unwrap();
            scheduler.resume(task, Wake::Ready(0));
        }
        // The running task's own record, with its yielder somewhere off its stack.
        "a yielder off its task's stack" => {
            let task = scheduler
                .spawn(|| {
                    let elsewhere = Box::new(0_u64);
                    super::current::move_yielder((&raw const *elsewhere).cast());
                    park(1);
                    0
                })
                .unwrap();
            scheduler.resume(task, Wake::Ready(0));
        }
        "a parked task is dropped" => {
            let task = scheduler.spawn(|| i64::try_from(native_code(0, 1)).unwrap()).unwrap();
            scheduler.resume(task, Wake::Ready(0));
            drop(scheduler.tasks.remove(&task));
        }
        "a task panics" => {
            let task = scheduler
                .spawn(|| {
                    park(1);
                    panic!("a task's own bug");
                })
                .unwrap();
            scheduler.resume(task, Wake::Ready(0));
            scheduler.resume(task, Wake::Ready(0));
        }
        "a task panics as its scheduler drops" => {
            for _ in 0..3 {
                let task = scheduler
                    .spawn(|| {
                        assert_ne!(park(1), Some(Wake::Cancelled), "a task's own bug, when cancelled");
                        0
                    })
                    .unwrap();
                scheduler.resume(task, Wake::Ready(0));
            }
            drop(scheduler);
        }
        _ => unreachable!("no way to stop the process called {how}"),
    }
}

#[test]
fn a_broken_invariant_a_dropped_parked_task_or_a_panic_stops_the_process() {
    if let Some(how) = std::env::var_os(STOP) {
        stop_the_process(&how.to_string_lossy());
        return;
    }
    let ours = "stopping rather than switching onto the wrong stack";
    for (how, said) in [
        ("another task parks", ours),
        ("another task is resumed", ours),
        ("another task starts", ours),
        ("an outer task's records", "on another stack"),
        ("a yielder off its task's stack", "with a yielder off its stack"),
        ("a parked task is dropped", "dropped before it ended"),
        ("a task panics", "a parked task panicked"),
        ("a task panics as its scheduler drops", "a parked task panicked"),
    ] {
        let ran = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::a_broken_invariant_a_dropped_parked_task_or_a_panic_stops_the_process"])
            .args(["--nocapture", "--test-threads=1"])
            .env(STOP, how)
            .output()
            .unwrap();
        let printed = String::from_utf8_lossy(&ran.stderr);
        assert!(!ran.status.success(), "{how}: the process went on");
        assert!(printed.contains(said), "{how}: {printed}");
    }
}
