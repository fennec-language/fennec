//! The log's parts, apart from the process-wide mode a run sets once.

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

fn player(entries: &[(&str, &[u8])]) -> Mutex<Player> {
    let mut log = MAGIC.to_vec();
    log.extend(3_u32.to_le_bytes());
    log.extend(b"run");
    let at = log.len();
    for (call, payload) in entries {
        log.extend(entry(call, payload));
    }
    assert_eq!(entries_start(&log), Some(at));
    Mutex::new(Player { log, at })
}

#[test]
fn a_replay_gives_back_each_call_in_turn_and_names_both_where_it_diverges() {
    let player = player(&[("Clock.now", &5_i64.to_le_bytes()), ("Fs.read_text", b"x")]);
    assert_eq!(replayed(&player, "Clock.now").unwrap(), 5_i64.to_le_bytes());
    let why = replayed(&player, "Entropy.seed").unwrap_err();
    assert!(why.contains("`Entropy.seed`") && why.contains("`Fs.read_text`"), "{why}");
    assert_eq!(replayed(&player, "Fs.read_text").unwrap(), b"x");
    let why = replayed(&player, "Clock.now").unwrap_err();
    assert!(why.contains("`Clock.now`") && why.contains("nothing more"), "{why}");
}

#[test]
fn a_replay_that_ends_early_has_its_next_call_left() {
    let player = player(&[("Stdin.lines", b"")]);
    assert_eq!(next_name(&player.lock().unwrap()).as_deref(), Some("Stdin.lines"));
    replayed(&player, "Stdin.lines").unwrap();
    assert_eq!(next_name(&player.lock().unwrap()), None);
}

#[test]
fn a_log_without_its_mark_isnt_a_recording() {
    assert_eq!(entries_start(b"not a log at all"), None);
    let mut log = MAGIC.to_vec();
    log.extend(100_u32.to_le_bytes());
    assert_eq!(entries_start(&log), None, "its header runs past its end");
}
