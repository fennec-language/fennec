//! Messages sent and received as words record and replay as today's do: a run recorded on one
//! worker replays on four, and one on four replays on one, with each task seeing what it saw. A
//! recording made with today's sends and receives replays with words in their place, and the other
//! way round: so the words calls take the same turns, and the log is the same whichever way a
//! message goes.
//!
//! A recording or a replay is a whole process's, so each run is this test binary run again with
//! the one test that runs it, [`a_run_recorded_or_replayed`].

use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use super::super::{TIMED_OUT, channel_close, channel_free, channel_new, channel_receive, channel_send};
use super::*;
use crate::embed::parallel::Pool;
use crate::embed::serve::{launch_end, launch_on};
use crate::embed::{Tables, finish, start};

const FOREVER: i64 = -1;
const MILLISECOND: i64 = 1_000_000;
const PATIENCE: i64 = 10_000 * MILLISECOND;

/// A message: who sent it and what they said.
const MESSAGE: i64 = 1;
const WIDTH: i64 = 2;

/// What tells a run what to do: record or replay, into or from which log, on how many workers, with
/// messages as words or as today's, and where to say what it saw.
const ROLE: &str = "FENNEC_TEST_WORDS_ROLE";
const LOG: &str = "FENNEC_TEST_WORDS_LOG";
const WORKERS: &str = "FENNEC_TEST_WORDS_WORKERS";
const WAY: &str = "FENNEC_TEST_WORDS_WAY";
const SAW: &str = "FENNEC_TEST_WORDS_SAW";

fn tables() -> Tables {
    let message = Element::Struct {
        name: Rc::from("Message"),
        fields: vec![(Rc::from("user"), Element::Int), (Rc::from("text"), Element::Str)],
    };
    let elements = vec![Rc::new(Element::Str), Rc::new(message)];
    Tables { shapes: vec![Rc::new(Element::Str)], elements, str_shape: 0, ..Tables::default() }
}

/// Whether this run sends and receives as words.
static AS_WORDS: AtomicBool = AtomicBool::new(false);
static INBOX: AtomicI64 = AtomicI64::new(0);
static READY: AtomicI64 = AtomicI64::new(0);
/// What `main` sends on once its sends that give up at once are done, so the task reads none of
/// them before then, and they find the channel full however the threads are scheduled.
static GO: AtomicI64 = AtomicI64::new(0);
/// What the launched task saw, in its order.
static TASK_SAW: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn pool(workers: usize) -> &'static Pool {
    static POOL: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let setup: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(|| {
            start(tables(), crate::Inputs::default());
        });
        crate::embed::parallel::with_setup(workers, &setup)
    })
}

/// Sends a message from `user` saying `text`, a string's block, waiting up to `timeout_ns`.
fn send(inbox: i64, user: i64, text: u64, timeout_ns: i64) -> i64 {
    if !AS_WORDS.load(Ordering::SeqCst) {
        return channel_send(inbox, user, 0, timeout_ns);
    }
    with(|state| {
        super::super::super::dicts::value_buffer_of(state, |buffer| {
            for (cell, word) in buffer.iter().zip([user.cast_unsigned(), text]) {
                cell.set(word);
            }
        });
    });
    channel_send_words(inbox, MESSAGE, WIDTH, 0, timeout_ns)
}

/// Receives the next message, waiting up to `timeout_ns`: what the receive gave back, and who sent
/// what it took, 0 when it took nothing.
fn receive(inbox: i64, timeout_ns: i64) -> (i64, i64) {
    if !AS_WORDS.load(Ordering::SeqCst) {
        let mut user = 0;
        let code = channel_receive(inbox, timeout_ns, &mut user);
        return (code, user);
    }
    let code = channel_receive_words(inbox, MESSAGE, WIDTH, timeout_ns);
    if code != DONE {
        return (code, 0);
    }
    let words = with(|state| {
        super::super::super::dicts::value_buffer_of(state, |buffer| {
            buffer.iter().take(2).map(std::cell::Cell::get).collect::<Vec<_>>()
        })
    });
    // The text it took is its own, and it's done with it.
    heap::free_plain(words.get(1).copied().unwrap_or(0).cast_signed());
    (code, words.first().copied().unwrap_or(0).cast_signed())
}

/// The launched task: a receive with nothing sent yet, which runs out of time, then, once `main`
/// says go, every message until the inbox closes, as `main` closes it, mostly while this waits
/// for the next.
extern "C" fn reads(_env: i64, _thread: i64) -> i64 {
    let inbox = INBOX.load(Ordering::SeqCst);
    let mut saw = Vec::new();
    let (code, _) = receive(inbox, MILLISECOND);
    saw.push(format!("waited {code}"));
    channel_send(READY.load(Ordering::SeqCst), 1, 0, FOREVER);
    let mut go = 0;
    saw.push(format!("told to go {}", channel_receive(GO.load(Ordering::SeqCst), FOREVER, &mut go)));
    loop {
        let (code, user) = receive(inbox, PATIENCE);
        saw.push(format!("got {code} from {user}"));
        if code != DONE {
            break;
        }
    }
    *TASK_SAW.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = saw;
    0
}

/// One run: `main` waits for the task to time out, sends six messages into a channel with room
/// for one, each giving up at once if it's full, then tells the task to read, then sends three
/// that wait for room, then closes it. The task reads nothing until it's told, so the first send
/// fills the channel and the other five find it full.
/// What `main` saw, then what the task did.
fn run(workers: usize) -> (Vec<String>, Vec<String>) {
    start(tables(), crate::Inputs::default());
    let (inbox, ready, go) = (channel_new(1), channel_new(1), channel_new(1));
    INBOX.store(inbox, Ordering::SeqCst);
    READY.store(ready, Ordering::SeqCst);
    GO.store(go, Ordering::SeqCst);
    let task = launch_on(pool(workers), reads, 0);
    let mut saw = Vec::new();
    let mut got = 0;
    saw.push(format!("ready {}", channel_receive(ready, FOREVER, &mut got)));
    let text = with(|state| heap::text_address(state, "a text long enough to need a block of its own"))
        .cast_unsigned();
    for user in 1..=6 {
        saw.push(format!("sent {user}: {}", send(inbox, user, text, 0)));
    }
    saw.push(format!("said go {}", channel_send(go, 1, 0, FOREVER)));
    for user in 7..=9 {
        saw.push(format!("sent {user}: {}", send(inbox, user, text, FOREVER)));
    }
    channel_close(inbox);
    let mut failure = 0;
    saw.push(format!("ended {}", launch_end(task, FOREVER, 0, &mut failure)));
    channel_free(inbox);
    channel_free(ready);
    channel_free(go);
    finish();
    let task_saw = std::mem::take(&mut *TASK_SAW.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    (saw, task_saw)
}

/// A recording or a replay of [`run`], as the environment says, saying what it saw into a file.
/// Run by [`messages_sent_as_words_replay_as_recorded_and_as_todays_do`], each in a process of its
/// own; on its own, it does nothing.
#[test]
#[ignore = "a recording or a replay needs a process of its own"]
fn a_run_recorded_or_replayed() {
    let (Some(role), Some(log), Some(saw_into)) =
        (std::env::var_os(ROLE), std::env::var_os(LOG).map(PathBuf::from), std::env::var_os(SAW))
    else {
        return;
    };
    let workers = std::env::var(WORKERS).ok().and_then(|workers| workers.parse().ok()).unwrap_or(1);
    AS_WORDS.store(std::env::var(WAY).is_ok_and(|way| way == "words"), Ordering::SeqCst);
    let recording = role == "record";
    if recording {
        crate::replay::start_log(&log, b"words").expect("a log to record into");
        crate::replay::record_to(&log).expect("a recording");
    } else {
        crate::replay::replay_from(&log).expect("a replay");
    }
    let (main_saw, task_saw) = run(workers);
    if recording {
        crate::replay::ended();
    }
    let mut said = main_saw;
    said.push("--".to_owned());
    said.extend(task_saw);
    if let Some(why) = crate::replay::divergence() {
        said.push(format!("diverged: {why}"));
    }
    if let Some(why) = crate::replay::unfinished().filter(|_| !recording) {
        said.push(format!("unfinished: {why}"));
    }
    std::fs::write(saw_into, said.join("\n")).expect("somewhere to say what it saw");
}

/// Runs [`a_run_recorded_or_replayed`] in a process of its own, as `role`, on `workers` workers,
/// with messages sent `way`: what it saw. It's given 60 seconds.
fn ran(role: &str, log: &Path, workers: usize, way: &str) -> String {
    let saw = log.with_extension(format!("{role}-saw"));
    let _ = std::fs::remove_file(&saw);
    let mut child = Command::new(std::env::current_exe().expect("this test's binary"))
        .args([
            "--exact",
            "embed::channels::words::replayed::a_run_recorded_or_replayed",
            "--ignored",
            "--quiet",
        ])
        .env(ROLE, role)
        .env(LOG, log)
        .env(WORKERS, workers.to_string())
        .env(WAY, way)
        .env(SAW, &saw)
        .spawn()
        .expect("this test's binary runs");
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().expect("the run can be waited for") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the {role} on {workers} workers with messages as {way} was still running after 60 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "the {role} on {workers} workers with messages as {way} failed: {status}");
    let said = std::fs::read_to_string(&saw).unwrap_or_default();
    let _ = std::fs::remove_file(&saw);
    said
}

/// Each run recorded is replayed, and each replay sees what its recording saw, though recordings
/// differ: on one worker and
/// four, with messages as words in both, and as today's in one and words in the other. The
/// recordings come to a timed receive with nothing sent, sends into a full channel that give up,
/// sends that wait for room, and a receive the channel's closing ends.
#[test]
fn messages_sent_as_words_replay_as_recorded_and_as_todays_do() {
    let folder = std::env::temp_dir().join(format!("fennec-words-replay-{}", std::process::id()));
    std::fs::create_dir_all(&folder).expect("a folder for the logs");
    let runs = [
        ("words", 1, "words", 4),
        ("words", 4, "words", 1),
        ("today", 1, "words", 4),
        ("words", 4, "today", 1),
    ];
    // Runs on more than one worker come out differently from one recording to the next, so each is
    // recorded three times.
    for (at, (recorded_way, recorded_on, replayed_way, replayed_on)) in
        runs.into_iter().cycle().take(3 * runs.len()).enumerate()
    {
        let log = folder.join(format!("run{at}.log"));
        let recorded = ran("record", &log, recorded_on, recorded_way);
        assert!(recorded.contains("waited 2"), "the first receive runs out of time:\n{recorded}");
        for user in 2..=6 {
            assert!(
                recorded.contains(&format!("sent {user}: {TIMED_OUT}")),
                "send {user} finds the channel full:\n{recorded}"
            );
        }
        assert!(recorded.contains("got 1 from 0"), "the last receive finds the channel closed:\n{recorded}");
        let replayed = ran("replay", &log, replayed_on, replayed_way);
        assert_eq!(
            replayed, recorded,
            "recorded on {recorded_on} with messages as {recorded_way}, replayed on {replayed_on} as {replayed_way}"
        );
    }
    let _ = std::fs::remove_dir_all(&folder);
}
