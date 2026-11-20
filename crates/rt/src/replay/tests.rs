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

/// A replay says an older Fennec made a log when it's any older log whose run had tasks, `FENREPL8`, `FENREPL7`,
/// `FENREPL6`, `FENREPL5` and `FENREPL4` as well as `FENREPL3` and `FENREPL2`, and a task that only
/// used a channel counts: each replayed wrong with nothing said, where a task had been stopped or a
/// `parallel` call's piece had failed.
#[test]
fn a_replay_of_any_older_log_with_tasks_says_so() {
    let marked = |magic: &[u8; 8], entries: &[(&str, &str, &[u8])]| {
        let mut log = log_of(entries);
        log.splice(..MAGIC.len(), magic.iter().copied());
        let player = player_of(&log).unwrap();
        made_by_an_older_fennec(&log, &player)
    };
    let task: &[(&str, &str, &[u8])] = &[("0.0", "Clock.now", &5_i64.to_le_bytes())];
    let at_a_channel: &[(&str, &str, &[u8])] = &[("0:c0", "0.0 receive", &0_i64.to_le_bytes())];
    let main_alone: &[(&str, &str, &[u8])] =
        &[("0", "Clock.now", &5_i64.to_le_bytes()), ("0:c0", "0 send", &0_i64.to_le_bytes())];
    for old in [
        MAGIC_BEFORE_DEADLOCKS,
        MAGIC_BEFORE_WORK_STOPS,
        MAGIC_BEFORE_UNBEGUN,
        MAGIC_BEFORE_SERVING_STOPS,
        MAGIC_BEFORE_SKIPS,
        MAGIC_BEFORE_STOPS,
        MAGIC_UNMARKED,
    ] {
        assert!(marked(old, task));
        assert!(marked(old, at_a_channel));
        assert!(!marked(old, main_alone));
    }
    assert!(!marked(MAGIC, task));
}

/// A log from before stops were recorded still reads, whole when it ends with its mark, and a
/// replay can tell whether it has tasks, to say a stopped one may not be stopped in it.
#[test]
fn a_log_from_before_stops_were_recorded_still_reads() {
    let mut log = log_of(&[("0.0", "Clock.now", &5_i64.to_le_bytes())]);
    log.splice(..MAGIC.len(), MAGIC_BEFORE_STOPS.iter().copied());
    let player = player_of(&log).unwrap();
    assert!(player.whole);
    assert!(player.has_tasks());
    let alone = player_of(&log_of(&[("0", "Clock.now", &5_i64.to_le_bytes())])).unwrap();
    assert!(!alone.has_tasks());
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

/// Whether a task in `tasks_take_their_turns_at_a_place_in_the_recorded_order` has had to wait for
/// its turn.
static WAITED: AtomicBool = AtomicBool::new(false);

/// Makes way for another task, as a task waiting for its turn does, noting that one has.
fn wait_noted() {
    WAITED.store(true, Ordering::SeqCst);
    std::thread::yield_now();
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
                let got = turn_in(&player, ("0:c0", what, true), Duration::from_secs(5), wait_noted).unwrap();
                order.lock().unwrap().push((name, got));
                end_turn(&player, "0:c0", what);
            }
        })
    };
    // The receiver arrives first, and waits for the send that came first in the recording: the
    // sender starts only once it's waiting, however long that takes.
    let receiving = go("0.0", "receive", 2);
    while !WAITED.load(Ordering::SeqCst) {
        std::thread::yield_now();
    }
    let sending = go("0.1", "send", 1);
    receiving.join().unwrap();
    sending.join().unwrap();
    assert_eq!(*order.lock().unwrap(), [("0.1", 0), ("0.0", 0), ("0.0", 2)]);
}

/// What a replay's tasks write comes out as the bursts that were recorded, in their order and
/// each as long as it was, however the replay's tasks happened to cut their output: all at once
/// where the recording had two bursts, or a byte at a time. Where a burst ends depends on when its
/// task waited, which a replay doesn't repeat, so a replay that went by how many times a task
/// wrote ended with a burst the recording had and it hadn't asked for.
#[test]
fn a_replay_writes_the_recorded_bursts_however_its_tasks_cut_their_output() {
    let (three, two) = (3_u64.to_le_bytes(), 2_u64.to_le_bytes());
    let recorded: &[(&str, &str, &[u8])] =
        &[(OUTPUT, "0 write", &three), (OUTPUT, "0.1 write", &two), (OUTPUT, "0 write", &three)];
    let written = RefCell::new(Vec::new());
    let write = |bytes: &[u8]| {
        written.borrow_mut().push(bytes.to_vec());
        Ok(())
    };
    let whole = |player: &Player| player.held.values().all(VecDeque::is_empty) && left_over(player).is_none();

    // Both of a task's bursts written as one: the first goes out, and the second waits for the
    // other task's, which was recorded between them.
    let mut at_once = player_of(&log_of(recorded)).unwrap();
    at_once.takes_output("0", b"aaabbb", write).unwrap();
    assert_eq!(*written.borrow(), [b"aaa".to_vec()]);
    assert!(!whole(&at_once));
    at_once.takes_output("0.1", b"cc", write).unwrap();
    assert_eq!(*written.borrow(), [b"aaa".to_vec(), b"cc".to_vec(), b"bbb".to_vec()]);
    assert!(whole(&at_once));

    // A byte at a time, the tasks taking turns: the same three bursts.
    written.borrow_mut().clear();
    let mut in_bytes = player_of(&log_of(recorded)).unwrap();
    for (task, byte) in [("0", b'a'), ("0.1", b'c'), ("0", b'a'), ("0", b'a'), ("0", b'b'), ("0.1", b'c')] {
        in_bytes.takes_output(task, &[byte], write).unwrap();
    }
    assert_eq!(*written.borrow(), [b"aaa".to_vec(), b"cc".to_vec()]);
    in_bytes.takes_output("0", b"bb", write).unwrap();
    assert_eq!(*written.borrow(), [b"aaa".to_vec(), b"cc".to_vec(), b"bbb".to_vec()]);
    assert!(whole(&in_bytes));

    // More than was recorded is kept back, for the replay's end to say.
    in_bytes.takes_output("0", b"x", write).unwrap();
    assert_eq!(written.borrow().len(), 3);
    assert!(!whole(&in_bytes));
}

/// A task asking for something the recording didn't do there has diverged.
#[test]
fn a_turn_the_recording_doesnt_have_is_a_divergence() {
    let player = player(&[("0:c0", "0 send", &0_i64.to_le_bytes())]);
    let why = turn_in(&player, ("0:c0", "close", false), Duration::ZERO, std::thread::yield_now);
    assert!(why.is_err_and(|why| why.contains("`0 close`") && why.contains("`0 send`")));
    assert_eq!(turn_in(&player, ("0:c0", "send", true), Duration::ZERO, std::thread::yield_now), Ok(0));
    end_turn(&player, "0:c0", "send");
    let why = turn_in(&player, ("0:c0", "send", true), Duration::ZERO, std::thread::yield_now).unwrap_err();
    assert!(why.contains("nothing more"), "{why}");
}

/// A recorded deadlock's lines, from its `deadlock` entry under `main`, which no task asks for.
fn deadlocked_player(lines: &str, entries: &[(&str, &str, &[u8])]) -> Mutex<Player> {
    let mut all = entries.to_vec();
    all.push(("0", DEADLOCK, lines.as_bytes()));
    player(&all)
}

/// A recording of a run that deadlocked has its lines, kept apart from `main`'s entries. A wait its
/// recording has nothing more for at a place goes on into the real wait, past the end: only a
/// send or a receive (`waits`), only in a whole recording that deadlocked. A step that takes
/// effect there is a divergence, said when the replay comes to its deadlock or its end.
#[test]
fn a_replay_of_a_deadlock_waits_past_its_recordings_end() {
    let player = deadlocked_player(
        "0 receive 0:c0\n0.0 receive 0:c1\n\nerror: a deadlock: as the recording said it\n",
        &[("0:c0", "0.0 send", &0_i64.to_le_bytes()), ("0", "Clock.now", &5_i64.to_le_bytes())],
    );
    let lines = || vec!["0 receive 0:c0".to_owned(), "0.0 receive 0:c1".to_owned()];
    {
        let player = player.lock().unwrap();
        assert_eq!(player.deadlock, Some(lines()));
        assert_eq!(
            player.streams.get("0").map(VecDeque::len),
            Some(1),
            "the deadlock isn't main's to ask for"
        );
    }
    let past = turn_in(&player, ("0:c1", "receive", true), Duration::from_millis(50), wait_noted);
    assert_eq!(past, Ok(PAST_THE_END));
    let why = turn_in(&player, ("0:c1", "close", false), Duration::from_millis(50), wait_noted).unwrap_err();
    assert!(why.contains("had nothing more"), "{why}");
    // Not yet at the deadlock: the recording has more of main's, and a task's send.
    let why = deadlocked_in(&player.lock().unwrap(), &lines()).unwrap_err();
    assert!(why.contains("asked for `Clock.now` next,"), "{why}");
    replayed(&player, "Clock.now").unwrap();
    let why = deadlocked_in(&player.lock().unwrap(), &lines()).unwrap_err();
    assert!(why.contains("asked for `0.0 send` next at 0:c0"), "{why}");
    TASK.with_borrow_mut(|running| *running = Some(Task::named(Arc::from("0.0"))));
    assert_eq!(turn_in(&player, ("0:c0", "send", true), Duration::from_millis(50), wait_noted), Ok(0));
    end_turn(&player, "0:c0", "send");
    TASK.with_borrow_mut(|running| *running = None);
    // The same deadlock is said as the recording said it, whatever this replay's own words.
    assert_eq!(
        deadlocked_in(&player.lock().unwrap(), &lines()),
        Ok(Some("error: a deadlock: as the recording said it\n".to_owned()))
    );
    // Another names only the lines that differ.
    let other = vec!["0 receive 0:c0".to_owned(), "0.0 send 0:c1".to_owned()];
    let why = deadlocked_in(&player.lock().unwrap(), &other).unwrap_err();
    assert!(
        why.contains("waiting at `0.0 send 0:c1` where the recording wasn't, and not at `0.0 receive 0:c1` where it was"),
        "{why}"
    );
    // A receive that went through past the end is a divergence, whatever comes after.
    end_turn(&player, "0:c1", "receive");
    let why = deadlocked_in(&player.lock().unwrap(), &lines()).unwrap_err();
    assert!(
        why.contains("`0 receive` at 0:c1 went through past where its recording ended in a deadlock"),
        "{why}"
    );
}

/// A replay of a recording that didn't deadlock and comes to a deadlock has diverged: as one that
/// misses a stop the recording has, which used to hang. Nothing goes past such a recording's end.
#[test]
fn a_replay_that_deadlocks_where_its_recording_didnt_has_diverged() {
    let player = player(&[("0:c0", "0.0 send", &0_i64.to_le_bytes())]);
    let why = deadlocked_in(&player.lock().unwrap(), &["0 receive 0:c1".to_owned()]).unwrap_err();
    assert!(
        why.contains("came to a deadlock its recording didn't have, its tasks waiting at `0 receive 0:c1`"),
        "{why}"
    );
    let why = turn_in(&player, ("0:c1", "receive", true), Duration::from_millis(50), wait_noted).unwrap_err();
    assert!(why.contains("had nothing more"), "{why}");
}

/// The place `a_turn_waits_as_long_as_its_place_moves` takes turns at, which `taking_turns` moves
/// on a turn at a time, as other tasks would.
static MOVING: OnceLock<Mutex<Player>> = OnceLock::new();

/// Ends the front turn at `MOVING`'s place every 30 ms, as a line of other tasks taking their
/// turns there would.
fn taking_turns() {
    std::thread::sleep(Duration::from_millis(30));
    if let Some(player) = MOVING.get() {
        let mut player = player.lock().unwrap();
        if let Some(stream) = player.streams.get_mut("0:c0")
            && stream.len() > 1
        {
            stream.pop_front();
        }
    }
}

/// A turn waits as long as its place's next entry keeps moving on, however long the line ahead of
/// it takes: its patience starts again each time (Fable 5.2's review, finding 5). Here 6 turns of
/// 30 ms each go ahead of it, past a patience of 100 ms.
#[test]
fn a_turn_waits_as_long_as_its_place_moves() {
    let mut entries: Vec<(&str, &str, &[u8])> = vec![("0:c0", "0.1 send", &[0; 8]); 6];
    entries.push(("0:c0", "0 receive", &[0; 8]));
    let player = MOVING.get_or_init(|| player(&entries));
    assert_eq!(turn_in(player, ("0:c0", "receive", true), Duration::from_millis(100), taking_turns), Ok(0));
}

/// A replay that printed more than its recording did, past its last burst, and comes to its
/// recording's deadlock has diverged, as at a normal end: what it held back was never written.
#[test]
fn a_replay_that_printed_more_than_its_recording_has_diverged_at_its_deadlock() {
    let three = 3_u64.to_le_bytes();
    let player = deadlocked_player("0 receive 0:c0\n\nsaid\n", &[(OUTPUT, "0 write", &three)]);
    let mut player = player.into_inner().unwrap();
    player.takes_output("0", b"abc", |_| Ok(())).unwrap();
    assert_eq!(deadlocked_in(&player, &["0 receive 0:c0".to_owned()]), Ok(Some("said\n".to_owned())));
    player.takes_output("0", b"more", |_| Ok(())).unwrap();
    let why = deadlocked_in(&player, &["0 receive 0:c0".to_owned()]).unwrap_err();
    assert!(why.starts_with("this replay printed more than its recording did,"), "{why}");
}

/// A `deadlock` entry is a recorded deadlock only in a log new enough to have one: in an older log
/// it's an entry of `main`'s like any other.
#[test]
fn only_a_log_new_enough_has_a_deadlock() {
    let mut log = log_of(&[("0", DEADLOCK, b"0 receive 0:c0")]);
    assert!(player_of(&log).unwrap().deadlock.is_some());
    log.splice(..MAGIC.len(), MAGIC_BEFORE_DEADLOCKS.iter().copied());
    let older = player_of(&log).unwrap();
    assert!(older.deadlock.is_none());
    assert_eq!(older.streams.get("0").map(VecDeque::len), Some(1));
}
