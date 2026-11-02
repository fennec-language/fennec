use std::cell::{Cell, RefCell};
use std::rc::Rc;

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

/// What a task asks for on its `step`th park, and what it's woken with.
fn request(task: TaskId, step: u64) -> Request {
    task * 1_000 + step
}

fn answer(request: Request) -> Wake {
    Wake::Ready(request ^ 0x5eed)
}

/// A task that parks `parks` times, checking each time that it's woken with its own answer and
/// that what it keeps on its stack is as it left it. It gives back its number.
fn checked_task(task: TaskId, parks: u64) -> impl FnOnce() -> i64 {
    move || {
        let kept: [u64; 16] = std::array::from_fn(|at| task.wrapping_mul(31).wrapping_add(at as u64));
        for step in 0..parks {
            let asked = request(task, step);
            assert_eq!(park(asked), Some(answer(asked)), "task {task} woken with another's answer");
            let again: [u64; 16] = std::array::from_fn(|at| task.wrapping_mul(31).wrapping_add(at as u64));
            assert_eq!(kept, again, "task {task}'s stack changed while it was parked");
        }
        i64::try_from(task).unwrap()
    }
}

#[test]
fn thousands_of_tasks_take_turns_on_one_thread_in_seeded_orders() {
    for seed in [1, 2, 3, 0xfeed, 0xdead_beef] {
        let mut order = Seeded(seed);
        let mut scheduler = Scheduler::new(STACK);
        let mut parked: Vec<(TaskId, Request)> = Vec::new();
        let mut ended = 0;
        for _ in 0..2_000 {
            let task = scheduler.next;
            let parks = u64::try_from(order.below(6)).unwrap();
            let made = scheduler.spawn(checked_task(task, parks)).unwrap();
            assert_eq!(made, task);
            match scheduler.resume(task, Wake::Ready(0)) {
                Step::Parked(asked) => parked.push((task, asked)),
                Step::Done(result) => {
                    assert_eq!(result, i64::try_from(task).unwrap());
                    ended += 1;
                }
                Step::Unknown => unreachable!(),
            }
            // Now and then, resume some parked tasks in a random order.
            for _ in 0..order.below(3) {
                if parked.is_empty() {
                    break;
                }
                let (task, asked) = parked.swap_remove(order.below(parked.len()));
                match scheduler.resume(task, answer(asked)) {
                    Step::Parked(next) => {
                        assert_eq!(next, asked + 1);
                        parked.push((task, next));
                    }
                    Step::Done(result) => {
                        assert_eq!(result, i64::try_from(task).unwrap());
                        ended += 1;
                    }
                    Step::Unknown => unreachable!(),
                }
            }
        }
        while !parked.is_empty() {
            let (task, asked) = parked.swap_remove(order.below(parked.len()));
            match scheduler.resume(task, answer(asked)) {
                Step::Parked(next) => parked.push((task, next)),
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
    assert_eq!(scheduler.resume(12_345, Wake::Ready(0)), Step::Unknown);
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
    assert_eq!(scheduler.resume(third, answer(request(3, 0))), Step::Parked(request(3, 1)));
    assert_eq!(scheduler.resume(third, answer(request(3, 1))), Step::Done(3));
    assert_eq!(scheduler.resume(first, answer(request(1, 0))), Step::Parked(request(1, 1)));
    assert_eq!(scheduler.resume(first, answer(request(1, 1))), Step::Parked(request(1, 2)));
    assert_eq!(scheduler.resume(first, answer(request(1, 2))), Step::Done(1));
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
    assert_eq!(scheduler.resume(other, answer(request(2, 0))), Step::Parked(request(2, 1)));
    assert_eq!(scheduler.resume(other, answer(request(2, 1))), Step::Done(2));
}

#[test]
fn a_panic_in_a_parked_task_leaves_the_thread_and_the_other_tasks_as_they_were() {
    let mut scheduler = Scheduler::new(STACK);
    let panicking = scheduler
        .spawn(|| {
            park(1);
            panic!("a task's own bug");
        })
        .unwrap();
    let other = scheduler.spawn(checked_task(2, 1)).unwrap();
    assert_eq!(scheduler.resume(panicking, Wake::Ready(0)), Step::Parked(1));
    assert_eq!(scheduler.resume(other, Wake::Ready(0)), Step::Parked(request(2, 0)));
    let resumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        scheduler.resume(panicking, Wake::Ready(0));
    }));
    assert!(resumed.is_err());
    assert_eq!(park(7), None);
    assert_eq!(scheduler.resume(panicking, Wake::Ready(0)), Step::Unknown);
    assert_eq!(scheduler.resume(other, answer(request(2, 0))), Step::Done(2));
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
fn task_numbers_are_never_given_twice() {
    let mut scheduler = Scheduler::new(STACK);
    let first = scheduler.spawn(|| 0).unwrap();
    assert_eq!(scheduler.resume(first, Wake::Ready(0)), Step::Done(0));
    let second = scheduler.spawn(|| 0).unwrap();
    assert_ne!(first, second);
    assert_eq!(scheduler.resume(first, Wake::Ready(0)), Step::Unknown);
    assert_eq!(scheduler.resume(second, Wake::Ready(0)), Step::Done(0));
}

/// Set in a copy of this test program run to break an invariant on purpose.
const BREAK: &str = "FENNEC_PARKED_BREAK";

#[test]
fn a_broken_invariant_stops_the_process_with_a_message_instead_of_switching_stacks() {
    if let Some(how) = std::env::var_os(BREAK) {
        let breaking = how.clone();
        let mut scheduler = Scheduler::new(STACK);
        let task = scheduler
            .spawn(move || {
                if breaking == "suspend" {
                    // Another task marked as running while this one parks.
                    super::current::pretend_running(999);
                    park(1);
                } else {
                    park(1);
                }
                0
            })
            .unwrap();
        if how == "resume" {
            scheduler.resume(task, Wake::Ready(0));
            // Another task marked as running while this one is resumed.
            let _running = super::current::Running::start(999, false);
            scheduler.tasks.get_mut(&task).unwrap().coroutine.resume(Wake::Ready(0));
        } else if how == "enter" {
            let mut running = scheduler.tasks.remove(&task).unwrap();
            let _running = super::current::Running::start(999, false);
            running.coroutine.resume(Wake::Ready(0));
        } else {
            scheduler.resume(task, Wake::Ready(0));
        }
        return;
    }
    for how in ["suspend", "resume", "enter"] {
        let ran = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::a_broken_invariant_stops_the_process_with_a_message_instead_of_switching_stacks",
            ])
            .args(["--nocapture", "--test-threads=1"])
            .env(BREAK, how)
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&ran.stderr);
        assert!(!ran.status.success(), "{how}: the process went on");
        assert!(said.contains("stopping rather than switching onto the wrong stack"), "{how}: {said}");
    }
}
