//! The log's parts, apart from the process-wide mode a run sets once.

use std::time::Duration;

use super::*;

fn round_trip(value: &Value) -> Value {
    let mut payload = Vec::new();
    encode_result(&Ok(value.clone()), &mut payload);
    decode_result(&payload, Span::default()).unwrap().unwrap()
}

fn shown(value: &Value) -> String {
    format!("{value:?}")
}

#[test]
fn what_a_capability_gives_reads_back_as_it_was() {
    let error = Value::Instance(Rc::new(Instance {
        type_name: Rc::from("IoError"),
        case: Some(Rc::from("NotFound")),
        fields: vec![(Rc::from("path"), Value::str("gone.txt"))],
        is_error: true,
        cause: Some(Value::Int(2)),
        notes: None,
    }));
    let dict =
        Value::Dict(Rc::new(crate::pairs::Pairs::from(vec![(Value::str("PATH"), Value::str("/bin"))])));
    for value in [
        Value::Unit,
        Value::None,
        Value::Bool(true),
        Value::Int(-7),
        Value::Float(-0.5),
        Value::str("héllo"),
        Value::Bytes(Rc::from(&[0_u8, 255, 7][..])),
        Value::List(Rc::new(vec![Value::Int(1), Value::str("two")])),
        Value::Tuple(Rc::new(vec![Value::Bool(false)])),
        Value::Set(Rc::new(vec![Value::Int(3)])),
        dict,
        Value::Ok(Rc::new(Value::Int(1))),
        Value::Err(Rc::new(error)),
    ] {
        assert_eq!(shown(&round_trip(&value)), shown(&value));
    }
}

#[test]
fn an_error_from_the_operating_system_reads_back_as_the_same_failure() {
    let failed = Diagnostic::new("disk full", Span::default()).with_help("free some room");
    let mut payload = Vec::new();
    encode_result(&Err(failed), &mut payload);
    let back = decode_result(&payload, Span::default()).unwrap().unwrap_err();
    assert_eq!((back.message.as_str(), back.help.as_deref()), ("disk full", Some("free some room")));
}

/// A log with `entries`, each a task's call and what it gave, after the worker count, ending with
/// the mark its run wrote as it ended.
fn log_of(entries: &[(&str, &str, &[u8])]) -> Vec<u8> {
    let mut log = log_cut_short(entries);
    log.extend(entry("", END, &[]));
    log
}

/// `log_of` without its end mark: as a run killed partway, or whose log couldn't be written, leaves.
fn log_cut_short(entries: &[(&str, &str, &[u8])]) -> Vec<u8> {
    let mut log = MAGIC.to_vec();
    log.extend(3_u32.to_le_bytes());
    log.extend(b"run");
    let at = log.len();
    log.extend(entry("", WORKERS, &4_u64.to_le_bytes()));
    for (task, call, payload) in entries {
        log.extend(entry(task, call, payload));
    }
    assert_eq!(entries_start(&log), Some(at));
    log
}

fn player(entries: &[(&str, &str, &[u8])]) -> Mutex<Player> {
    Mutex::new(player_of(&log_of(entries)).unwrap())
}

/// A replay that runs past the end of a recording cut short says so, rather than calling the run
/// a different one: the log's fault, not the program's. One that runs past a whole recording's end
/// says the program has changed.
#[test]
fn a_recording_cut_short_is_said_to_be_cut_short() {
    let entries: &[(&str, &str, &[u8])] = &[("0", "Clock.now", &5_i64.to_le_bytes())];
    let cut = Mutex::new(player_of(&log_cut_short(entries)).unwrap());
    assert_eq!(replayed(&cut, "Clock.now").unwrap(), 5_i64.to_le_bytes());
    let why = replayed(&cut, "Clock.now").unwrap_err();
    assert!(why.contains("past the end of its recording, which was cut short"), "{why}");
    let whole = player(entries);
    assert_eq!(replayed(&whole, "Clock.now").unwrap(), 5_i64.to_le_bytes());
    let why = replayed(&whole, "Clock.now").unwrap_err();
    assert!(why.contains("its recording had nothing more"), "{why}");
}

/// A recorded value nested deeper than anything a capability gives back reads as damaged, rather
/// than taking `fennec replay` down with it.
#[test]
fn a_value_nested_past_the_limit_reads_as_damaged() {
    let nested = |depth: usize| {
        let mut payload = Vec::new();
        for _ in 0..depth {
            payload.push(tag::LIST);
            payload.extend(1_u32.to_le_bytes());
        }
        payload.push(tag::UNIT);
        payload
    };
    assert!(decode_result(&nested(10), Span::default()).is_some());
    assert!(decode_result(&nested(100_000), Span::default()).is_none());
}

#[test]
fn a_replay_gives_back_each_call_in_turn_and_names_both_where_it_diverges() {
    let player = player(&[("0", "Clock.now", &5_i64.to_le_bytes()), ("0", "Fs.read_text", b"x")]);
    assert_eq!(player.lock().unwrap().workers, Some(4));
    assert_eq!(replayed(&player, "Clock.now").unwrap(), 5_i64.to_le_bytes());
    let why = replayed(&player, "Entropy.seed").unwrap_err();
    assert!(why.contains("`Entropy.seed`") && why.contains("`Fs.read_text`"), "{why}");
    assert_eq!(replayed(&player, "Fs.read_text").unwrap(), b"x");
    let why = replayed(&player, "Clock.now").unwrap_err();
    assert!(why.contains("`Clock.now`") && why.contains("nothing more"), "{why}");
}

/// Each task gets its own entries in order, however the tasks take turns, and a divergence says
/// which task it was in.
#[test]
fn each_task_replays_its_own_calls_in_whatever_order_the_tasks_run() {
    let player = player(&[
        ("0.0.0", "Clock.now", &1_i64.to_le_bytes()),
        ("0.0.8", "Clock.now", &2_i64.to_le_bytes()),
        ("0.0.0", "Clock.now", &3_i64.to_le_bytes()),
    ]);
    let as_task = |name: &str, call: &str| {
        let mut task = Some(Task::named(Arc::from(name)));
        TASK.with_borrow_mut(|running| std::mem::swap(running, &mut task));
        let got = replayed(&player, call);
        TASK.with_borrow_mut(|running| std::mem::swap(running, &mut task));
        got
    };
    assert_eq!(as_task("0.0.8", "Clock.now").unwrap(), 2_i64.to_le_bytes());
    assert_eq!(as_task("0.0.0", "Clock.now").unwrap(), 1_i64.to_le_bytes());
    assert_eq!(left_over(&player.lock().unwrap()), Some(("0.0.0".to_owned(), "Clock.now".to_owned())));
    assert_eq!(as_task("0.0.0", "Clock.now").unwrap(), 3_i64.to_le_bytes());
    let why = as_task("0.0.8", "Clock.time").unwrap_err();
    assert!(why.contains("in task 0.0.8") && why.contains("nothing more"), "{why}");
    assert_eq!(left_over(&player.lock().unwrap()), None);
}

#[test]
fn a_replay_that_ends_early_has_its_next_call_left() {
    let player = player(&[("0", "Stdin.lines", b"")]);
    assert_eq!(left_over(&player.lock().unwrap()), Some(("0".to_owned(), "Stdin.lines".to_owned())));
    replayed(&player, "Stdin.lines").unwrap();
    assert_eq!(left_over(&player.lock().unwrap()), None);
}

/// A log from before tasks were named replays as `main`'s.
#[test]
fn a_log_from_before_tasks_were_named_is_mains() {
    let mut log = MAGIC_UNNAMED.to_vec();
    log.extend(0_u32.to_le_bytes());
    log.extend(4_u16.to_le_bytes());
    log.extend(b"Args");
    log.extend(1_u32.to_le_bytes());
    log.push(7);
    let player = Mutex::new(player_of(&log).unwrap());
    assert_eq!(replayed(&player, "Args").unwrap(), [7]);
}

/// Task names are paths: no counter is shared, so they're the same however threads interleave.
#[test]
fn a_task_is_named_by_its_path() {
    let call = Task::named(Arc::from("0.2"));
    let named = range(Some(&call), 128).unwrap();
    assert_eq!(&*named.name, "0.2.128");
    assert!(range(None, 5).is_none());
}

#[test]
fn a_log_without_its_mark_isnt_a_recording() {
    assert_eq!(entries_start(b"not a log at all"), None);
    let mut log = MAGIC.to_vec();
    log.extend(100_u32.to_le_bytes());
    assert_eq!(entries_start(&log), None, "its header runs past its end");
}

/// At a place where tasks meet, each task waits for its turn, so they go in the recorded order
/// whatever order they arrive in, and each gets what it came to.
#[test]
fn tasks_take_their_turns_at_a_place_in_the_recorded_order() {
    let player = Arc::new(player(&[
        ("0:c0", "0.1 send", &0_i64.to_le_bytes()),
        ("0:c0", "0.0 receive", &0_i64.to_le_bytes()),
        ("0:c0", "0.0 receive", &2_i64.to_le_bytes()),
    ]));
    let order = Arc::new(Mutex::new(Vec::new()));
    let go = |name: &'static str, what: &'static str, times: usize| {
        let (player, order) = (Arc::clone(&player), Arc::clone(&order));
        std::thread::spawn(move || {
            TASK.with_borrow_mut(|running| *running = Some(Task::named(Arc::from(name))));
            for _ in 0..times {
                let got =
                    turn_in(&player, ("0:c0", what), Duration::from_secs(5), std::thread::yield_now).unwrap();
                order.lock().unwrap().push((name, got));
                end_turn(&player, "0:c0");
            }
        })
    };
    // The receiver arrives first, and waits for the send that came first in the recording.
    let receiving = go("0.0", "receive", 2);
    std::thread::sleep(Duration::from_millis(20));
    let sending = go("0.1", "send", 1);
    receiving.join().unwrap();
    sending.join().unwrap();
    assert_eq!(*order.lock().unwrap(), [("0.1", 0), ("0.0", 0), ("0.0", 2)]);
}

/// A task asking for something the recording didn't do there has diverged.
#[test]
fn a_turn_the_recording_doesnt_have_is_a_divergence() {
    let player = player(&[("0:c0", "0 send", &0_i64.to_le_bytes())]);
    let why = turn_in(&player, ("0:c0", "close"), Duration::ZERO, std::thread::yield_now);
    assert!(why.is_err_and(|why| why.contains("`0 close`") && why.contains("`0 send`")));
    assert_eq!(turn_in(&player, ("0:c0", "send"), Duration::ZERO, std::thread::yield_now), Ok(0));
    end_turn(&player, "0:c0");
    let why = turn_in(&player, ("0:c0", "send"), Duration::ZERO, std::thread::yield_now).unwrap_err();
    assert!(why.contains("nothing more"), "{why}");
}
