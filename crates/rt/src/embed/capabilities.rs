//! Capabilities for native code, as blocks. A plain capability has one block for each kind,
//! made the first time it's asked for and never freed. A capability narrowed with `only` has a
//! block of its own, owned like any other, holding the list of what it allows. Native code
//! carries a capability as its block's address, and the runtime reads from the block what the
//! capability may do, so passing one through native code can't widen it.
//!
//! A kind on its own, 0 to 10, still means that plain capability where one is taken: no
//! block's address is that small.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use super::heap::{CAPABILITY_SHAPE, IMMORTAL, read_block, text_address};
use super::{State, attempt, span, with};
use crate::limits::{Limited, narrowed};
use crate::packed::Element;
use crate::stdlib::fs::path_text;
use crate::value::{Capability, Value};

/// A capability's block: its owner count, its shape, its kind, and its list of what it
/// allows, which is 0 for a plain one.
const WORDS: usize = 4;

/// The kinds a capability word can be on its own, below any block's address.
const KINDS: i64 = 11;

/// The kind native code numbers a capability by, the inverse of `super::capability`.
fn kind_of(capability: Capability) -> i64 {
    match capability {
        Capability::Stdout => 0,
        Capability::Fs => 1,
        Capability::Clock => 2,
        Capability::Args => 3,
        Capability::Stdin => 4,
        Capability::Process => 5,
        Capability::Entropy => 6,
        Capability::Net => 7,
        Capability::Env => 8,
        Capability::Signals => 9,
        Capability::Stderr => 10,
    }
}

/// The block of a plain capability of `kind`, made the first time it's asked for.
fn plain(state: &mut State, kind: i64) -> i64 {
    let at = usize::try_from(kind.clamp(0, KINDS - 1)).unwrap_or(0);
    if let Some(address) = state.capabilities.get(at).copied().filter(|address| *address != 0) {
        return address;
    }
    let mut block = state.heap.fresh(WORDS);
    block.copy_from_slice(&[IMMORTAL, CAPABILITY_SHAPE, u64::try_from(at).unwrap_or(0), 0]);
    let address = i64::try_from(block.as_ptr().addr()).unwrap_or(0);
    // It lives for the rest of the run, like a literal, so it isn't counted as allocated.
    state.heap.adopt(&mut vec![block]);
    if state.capabilities.len() <= at {
        state.capabilities.resize(at + 1, 0);
    }
    if let Some(slot) = state.capabilities.get_mut(at) {
        *slot = address;
    }
    address
}

/// `fennec_capability_block(kind)`: the block of the plain capability of a kind, which
/// native code needn't count.
pub extern "C" fn block(kind: i64) -> i64 {
    with(|state| plain(state, kind))
}

/// `fennec_signals_listen()`: `main` was given `Signals`, so the signals that ask a program to stop
/// are caught from the start. Native code calls it only then, so a program that isn't given
/// `Signals` links none of what catching them takes, a thread and a socket pair among it. A
/// replay's stops come from its recording, so it catches nothing.
pub extern "C" fn signals_listen() {
    if !crate::replay::replaying() {
        crate::signals::listen();
    }
}

/// What a `Value` holds for a capability word: a kind, or a capability's block.
pub(super) fn value_of(state: &State, word: i64) -> Value {
    if (0..KINDS).contains(&word) {
        return Value::Capability(super::capability(word));
    }
    u64::try_from(word).map_or(Value::Unit, |address| read_block(state, address))
}

/// The value of a capability's block, which `read_block` gives for one.
pub(super) fn read(state: &State, block: &[u64]) -> Value {
    let kind = block.get(2).and_then(|kind| i64::try_from(*kind).ok()).unwrap_or(0);
    let capability = super::capability(kind);
    let allowed = block.get(3).copied().unwrap_or(0);
    if allowed == 0 {
        return Value::Capability(capability);
    }
    let allowed = match read_block(state, allowed).unpacked() {
        Value::List(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::Str(text) => Some(Rc::clone(text)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    Limited::to(capability, allowed)
}

/// A capability's block, owned once, for a temporary holding a capability: the plain block
/// for a plain one, and a new block for one narrowed with `only`.
pub extern "C" fn from_value(value: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| match state.temp(value) {
        Value::Capability(capability) => Ok(plain(state, kind_of(capability))),
        Value::Limited(limited) => {
            let allowed = limited.allowed.iter().map(|item| Value::Str(Rc::clone(item))).collect();
            let str = Rc::new(Element::Str);
            let list = super::lists::write_list(state, &str, &Value::list(allowed)).unwrap_or(0);
            let mut block = state.heap.fresh(WORDS);
            let kind = u64::try_from(kind_of(limited.capability)).unwrap_or(0);
            block.copy_from_slice(&[1, CAPABILITY_SHAPE, kind, u64::try_from(list).unwrap_or(0)]);
            Ok(state.heap.insert(block))
        }
        other => Err(Diagnostic::new(
            format!("expected a capability here, but this is a {}", other.type_name()),
            span(start, end),
        )),
    })
}

/// `fennec_native_capability(word, method, count, start, end)`: a method of a capability for
/// compiled code.
pub extern "C" fn native_call(word: i64, method: i64, count: i64, start: i64, end: i64) -> i64 {
    let at = span(start, end);
    let called = attempt(|state: &mut State| {
        let receiver = value_of(state, word);
        let name = u64::try_from(method)
            .ok()
            .and_then(|address| state.heap.block(address))
            .filter(|block| super::heap::is_text(state, block))
            .map(super::heap::block_text)
            .ok_or_else(|| Diagnostic::new("the native code lost a method's name", at))?;
        let count = usize::try_from(count).unwrap_or(0).min(state.inputs.len());
        let given = state.inputs.split_off(state.inputs.len() - count);
        let given: Vec<Value> = given.into_iter().map(Value::unpacked).collect();
        Ok(Some((receiver, name, given)))
    });
    let Some((receiver, name, given)) = called else { return 0 };
    // A test's capabilities are fakes the runtime holds, which never wait, so they run with the
    // state borrowed. What they leave to the real ones, like `only`, runs below.
    let faked = attempt(|state: &mut State| {
        let Some(fakes) = state.world_mut() else { return Ok(None) };
        let done = match &receiver {
            // Narrowing a whole capability touches nothing, so a test's is narrowed as the real one is.
            Value::Capability(_) if narrows(&name) => None,
            Value::Capability(capability) => fakes.method(*capability, &name, &given, at),
            Value::Limited(limited) => {
                let real = fakes.real();
                Some(narrowed(limited, &name, &given, (real, at), &mut |capability, method, args| {
                    fakes
                        .method(capability, method, args, at)
                        .unwrap_or_else(|| native_method(capability, method, args, at))
                }))
            }
            _ => None,
        };
        match done {
            Some(done) => done.map(|value| Some(state.push(value))),
            None => Ok(None),
        }
    });
    if let Some(pushed) = faked {
        return pushed;
    }
    // The method runs with nothing borrowed, so a task waiting in it, on the network, can park.
    let live = || match &receiver {
        Value::Capability(capability) => native_method(*capability, &name, &given, at),
        Value::Limited(limited) => {
            narrowed(limited, &name, &given, (true, at), &mut |capability, method, args| {
                native_method(capability, method, args, at)
            })
        }
        other => Err(crate::methods::no_method(other, &name, at)),
    };
    // What a capability gives is an outside input, recorded and replayed (see `crate::replay`):
    // all but `only`, which narrows what it's given. What's been printed goes out first, in a
    // recording and a replay alike, so the bursts standard output goes out in break at the same
    // places, whatever the call does when it runs for real: a replay doesn't wait for input.
    if crate::replay::active() {
        super::write_kept_output();
    }
    let result = match &receiver {
        Value::Capability(capability) if !narrows(&name) => {
            crate::replay::value(format_args!("{capability:?}.{name}"), at, live)
        }
        Value::Limited(limited) if !narrows(&name) => {
            crate::replay::value(format_args!("{:?}.{name}", limited.capability), at, live)
        }
        _ => live(),
    };
    attempt(|state: &mut State| result.map(|value| state.push(value)))
}

/// `fennec_crypto_call(name, count, start, end)`: one of `std.crypto`'s functions, with the last
/// `count` temporaries handed in as its arguments. `name` is a string block with its full name.
pub extern "C" fn crypto_call(name: i64, count: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let at = span(start, end);
        let name = u64::try_from(name)
            .ok()
            .and_then(|address| state.heap.block(address))
            .filter(|block| super::heap::is_text(state, block))
            .map(super::heap::block_text)
            .ok_or_else(|| Diagnostic::new("the native code lost a function's name", at))?;
        let count = usize::try_from(count).unwrap_or(0).min(state.inputs.len());
        let given = state.inputs.split_off(state.inputs.len() - count);
        let given: Vec<Value> = given.into_iter().map(Value::unpacked).collect();
        let result = if name.starts_with("compress.") {
            crate::stdlib::compress::run(&name, &given, at)?
        } else if name.starts_with("unicode.") {
            crate::stdlib::unicode::run(&name, &given, at)?
        } else {
            crate::stdlib::crypto::run(&name, &given, at)?
        };
        Ok(state.push(result))
    })
}

/// `fennec_net_call(handle, method, count, start, end)`: a method of a socket by its
/// handle, with the last `count` inputs native code handed over as its arguments. `method` is a
/// string block like `Connection.read_line`. Nothing's borrowed while it waits.
pub extern "C" fn net_call(handle: i64, method: i64, count: i64, start: i64, end: i64) -> i64 {
    let at = span(start, end);
    let called = attempt(|state: &mut State| {
        let name = u64::try_from(method)
            .ok()
            .and_then(|address| state.heap.block(address))
            .filter(|block| super::heap::is_text(state, block))
            .map(super::heap::block_text)
            .ok_or_else(|| Diagnostic::new("the native code lost a method's name", at))?;
        let count = usize::try_from(count).unwrap_or(0).min(state.inputs.len());
        let given = state.inputs.split_off(state.inputs.len() - count);
        Ok(Some((name, given.into_iter().map(Value::unpacked).collect::<Vec<Value>>())))
    });
    let Some((name, given)) = called else { return 0 };
    // A `File` is a handle in the same table in a test as anywhere, its fake files with the rest,
    // so it never goes to the fake network.
    // A database's handle, like a file's, is in a table of its own, a test's with the rest.
    if let Some(method) = name.strip_prefix("Database.").or_else(|| name.strip_prefix("Transaction.")) {
        let done = crate::stdlib::sqlite::call(handle, method, &given, at);
        return attempt(|state: &mut State| done.map(|value| state.push(value)));
    }
    if let Some(method) = name.strip_prefix("File.") {
        if crate::replay::active() {
            super::write_kept_output();
        }
        let done = crate::replay::value(format_args!("{name}"), at, || {
            crate::stdlib::fs::real_file(handle, method, &given, at)
        });
        return attempt(|state: &mut State| done.map(|value| state.push(value)));
    }
    // A test's network is a fake the runtime holds, which never waits, so it runs here with the
    // state borrowed. The real one runs below with nothing borrowed, so a socket's wait can park
    // its task.
    if with(|state| state.world().is_some_and(|world| !world.real())) {
        return attempt(|state: &mut State| {
            let done = match state.world_mut() {
                Some(world) => world.net.socket_method(handle, &name, &given, at)?,
                None => crate::stdlib::net::failed(&crate::stdlib::net::Failure::Closed),
            };
            Ok(state.push(done))
        });
    }
    super::write_kept_output();
    // Whether a wait timed out is in what it gave, so a replay doesn't time it again.
    let done = crate::replay::value(format_args!("{name}"), at, || {
        crate::stdlib::net::timed(&name, &given, at).and_then(|(method, args, timeout)| {
            let args = args.as_slice();
            match method.split_once('.') {
                Some(("Server", method)) => {
                    crate::stdlib::net::real_server(handle, method, args, timeout, at)
                }
                Some(("UdpSocket", method)) => {
                    crate::stdlib::net::real_udp(handle, method, args, timeout, at)
                }
                Some((_, method)) => super::serve::served_call(handle, method, args, timeout, at)
                    .unwrap_or_else(|| {
                        crate::stdlib::net::real_connection(handle, method, args, timeout, at)
                    }),
                None => Err(Diagnostic::new("the native code lost a method's name", at)),
            }
        })
    });
    attempt(|state: &mut State| done.map(|value| state.push(value)))
}

/// Whether `method` narrows a capability, which reads nothing from outside, so it's neither
/// recorded nor faked.
fn narrows(method: &str) -> bool {
    matches!(method, "only" | "read_only" | "client" | "server" | "hosts")
}

/// A whole `Fs` narrowed with `read_only`, or a whole `Net` with `client`, `server` or `hosts`,
/// or `None` for arguments that aren't what they should be.
fn narrowed_whole(capability: Capability, method: &str, args: &[Value]) -> Option<Value> {
    use crate::limits::{CLIENT, EVERYWHERE, READ_ONLY, SERVER};
    let marked = |marks: &[&str]| Limited::to(capability, marks.iter().map(|mark| Rc::from(*mark)).collect());
    match (method, args) {
        ("read_only", []) => Some(marked(&[EVERYWHERE, READ_ONLY])),
        ("client", []) => Some(marked(&[EVERYWHERE, CLIENT])),
        ("server", []) => Some(marked(&[EVERYWHERE, SERVER])),
        ("hosts", [Value::List(hosts)]) => {
            let mut allowed = vec![Rc::from(CLIENT)];
            for host in hosts.iter() {
                let Value::Str(host) = host else { return None };
                allowed.push(Rc::from(host.to_ascii_lowercase()));
            }
            Some(Limited::to(capability, allowed))
        }
        _ => None,
    }
}

/// A method of a whole capability.
fn native_method(
    capability: Capability,
    method: &str,
    args: &[Value],
    at: Span,
) -> Result<Value, Diagnostic> {
    let wrong = || Diagnostic::new(format!("`{method}` was called with the wrong arguments"), at);
    match (capability, method, args) {
        (Capability::Fs, "only", [path, ..]) => {
            let Value::Str(folder) = path_text(path) else { return Err(wrong()) };
            Ok(Limited::to(Capability::Fs, vec![Rc::from(crate::limits::absolute(&folder))]))
        }
        (Capability::Fs, "read_only", []) | (Capability::Net, "client" | "server" | "hosts", _) => {
            narrowed_whole(capability, method, args).ok_or_else(wrong)
        }
        (Capability::Fs, _, _) => {
            let paths: Vec<Value> = args.iter().map(path_text).collect();
            crate::stdlib::fs::real_fs(method, &paths, at)
        }
        (Capability::Process, run, _) if run == "run" || run.starts_with("run.") => {
            let (program, arguments, options) = match crate::stdlib::process::run_of(run, args, at)? {
                Ok(ran) => ran,
                Err(refused) => return Ok(refused),
            };
            // A prompt this program printed shows before the other program starts.
            super::write_kept_output();
            Ok(crate::stdlib::process::run_program(&program, &arguments, &options))
        }
        (Capability::Process, "only", [Value::Str(program)]) => {
            Ok(Limited::to(Capability::Process, vec![Rc::clone(program)]))
        }
        (Capability::Stdin, "lines", []) => {
            // A prompt shows before the program waits for an answer.
            super::write_kept_output();
            crate::inputs::lines_of(&mut super::aot::StdinLines::default(), at)
        }
        (Capability::Stdin, "read_line", []) => {
            super::write_kept_output();
            Ok(crate::inputs::read_line())
        }
        (Capability::Stdin, "read_chunk", [Value::Int(size)]) => {
            let Ok(size) = usize::try_from(*size) else {
                return Err(Diagnostic::new(format!("`read_chunk` can't give {size} bytes"), at)
                    .with_help("give it how many bytes you want, 0 or more"));
            };
            super::write_kept_output();
            Ok(crate::inputs::read_chunk(size))
        }
        (Capability::Net, _, _) => {
            // What's been printed comes out first, as it does before anything else that waits.
            super::write_kept_output();
            crate::stdlib::net::native_net(method, args, at)
        }
        (Capability::Entropy, "bytes", [Value::Int(count)]) => {
            let Ok(count) = usize::try_from(*count) else {
                return Err(Diagnostic::new(format!("`bytes` can't give {count} bytes"), at)
                    .with_help("give it how many bytes you want, 0 or more"));
            };
            Ok(Value::Bytes(Rc::from(crate::entropy::os_random(count, at)?)))
        }
        (Capability::Entropy, "seed", []) => {
            let drawn = crate::entropy::os_random(8, at)?;
            let word: [u8; 8] = drawn
                .try_into()
                .map_err(|_| Diagnostic::new("the operating system gave too few bytes", at))?;
            Ok(Value::Int(i64::from_le_bytes(word)))
        }
        (Capability::Env, "get", [Value::Str(name)]) => Ok(crate::env::variable(name)),
        (Capability::Env, "all", []) => Ok(crate::env::variables()),
        (Capability::Stderr, _, _) => {
            // A task or range a test started writes into the test's capture.
            if let Some(capture) = super::capture_given() {
                return crate::stderr::faked(&capture, method, args, at);
            }
            // What's been printed to standard output comes out first, so the two read in order.
            super::write_kept_output();
            crate::stderr::written(method, args, at)
        }
        (Capability::Signals, "stopping", []) => Ok(Value::Bool(crate::signals::stopping())),
        (Capability::Signals, "wait", []) => {
            // What's been printed comes out first, as it does before anything else that waits.
            super::write_kept_output();
            crate::signals::wait().map_err(|why| Diagnostic::new(why, at))?;
            Ok(Value::Unit)
        }
        _ => Err(crate::methods::no_method(&Value::Capability(capability), method, at)),
    }
}

/// `fennec_use_tls()`: compiled code that makes a request calls this, which is all that brings TLS
/// into a program.
pub extern "C" fn use_tls() {
    crate::stdlib::http::use_tls();
}

/// `clock.now()`, as the nanoseconds of an `Instant`.
pub extern "C" fn clock_now() -> i64 {
    if let Some(nanos) = fake_clock() {
        return nanos;
    }
    recorded_word("Clock.now", crate::collections::monotonic_nanos)
}

/// A test's fake clock's reading, which stands still but for sleeping, if a test's running.
fn fake_clock() -> Option<i64> {
    with(|state| state.world().filter(|world| !world.real()).map(crate::fakes::TestWorld::now))
}

/// A word an outside call gives, recorded and replayed (see `crate::replay`). A replay that's
/// diverged from its recording fails, at once.
fn recorded_word(call: &str, live: impl FnOnce() -> i64) -> i64 {
    crate::replay::word(call, live).unwrap_or_else(|why| {
        super::fail_with(Diagnostic::new(why, Span::default()));
        0
    })
}

/// `clock.time()`, as the nanoseconds of a `Time`.
pub extern "C" fn clock_time() -> i64 {
    if let Some(elapsed) = fake_clock() {
        return crate::collections::FAKE_TIME.saturating_add(elapsed);
    }
    recorded_word("Clock.time", crate::collections::wall_nanos)
}

/// `clock.sleep(duration)`, by the nanoseconds of a `Duration`. What's been printed comes out
/// first, as it does before anything else that waits. It's recorded like the clock's readings,
/// so a replay doesn't wait it out, and a replay that sleeps where its recording didn't stops.
pub extern "C" fn clock_sleep(nanos: i64) {
    // A test's sleep passes no time at all.
    let faked = with(|state| match state.world_mut().filter(|world| !world.real()) {
        Some(world) => {
            world.sleep(u64::try_from(nanos).unwrap_or(0));
            true
        }
        None => false,
    });
    if faked {
        return;
    }
    super::write_kept_output();
    recorded_word("Clock.sleep", || {
        wait(nanos);
        0
    });
}

/// Waits a sleep out: by parking, for a task, so its thread runs others meanwhile.
fn wait(nanos: i64) {
    let length = std::time::Duration::from_nanos(u64::try_from(nanos).unwrap_or(0));
    #[cfg(any(test, feature = "threads"))]
    if super::serve::can_park() {
        // A task parks for it, so its thread runs others meanwhile, and a stop cuts it short.
        let deadline = std::time::Instant::now() + length;
        let token = super::serve::next_token();
        while std::time::Instant::now() < deadline {
            if super::serve::park_on(token, Some(deadline)) == super::serve::Parked::Cancelled {
                return;
            }
        }
        return;
    }
    std::thread::sleep(length);
}

/// `len(args)`.
pub extern "C" fn args_len() -> i64 {
    with(|state| i64::try_from(state.arguments.len()).unwrap_or(i64::MAX))
}

/// `args.program`, as a new string.
pub extern "C" fn args_program() -> i64 {
    with(|state| {
        let program = state.program.clone();
        text_address(state, &program)
    })
}

/// `args[index]`, as a new string.
pub extern "C" fn args_item(index: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let at = crate::ops::position(index, state.arguments.len(), span(start, end))?;
        let argument = state.arguments.get(at).cloned().unwrap_or_default();
        Ok(text_address(state, &argument))
    })
}

/// A new list of the arguments, for `for arg in args`.
pub extern "C" fn args_list(element: i64) -> i64 {
    super::made_with("fennec_args_list", |state| {
        let arguments = state.arguments.clone();
        let strings: Vec<&str> = arguments.iter().map(String::as_str).collect();
        super::lists::list_of_strings(state, element, &strings)
    })
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::super::{Tables, finish, heap, start, with};
    use crate::limits::Limited;
    use crate::packed::Element;
    use crate::value::{Capability, Value};

    fn value(word: i64) -> Value {
        with(|state| Some(super::value_of(state, word))).unwrap()
    }

    #[test]
    fn a_capability_crosses_native_code_as_a_block_that_keeps_what_it_allows() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(tables, crate::Inputs::default());
        let fs = super::block(1);
        assert_eq!(super::block(1), fs);
        assert!(fs >= super::KINDS);
        assert_eq!(value(fs), Value::Capability(Capability::Fs));
        assert_eq!(value(1), Value::Capability(Capability::Fs));
        // A plain capability is never freed: letting go of it never brings its count to zero.
        with(|state| heap::release(state, u64::try_from(fs).unwrap()));
        assert_eq!(value(fs), Value::Capability(Capability::Fs));
        let live = with(|state| state.heap.live());
        let narrowed = Limited::to(Capability::Process, vec![Rc::from("git"), Rc::from("ls")]);
        let temp = with(|state| Some(state.push(narrowed.clone()))).unwrap();
        let block = super::from_value(temp, 0, 0);
        assert_ne!(block, 0);
        let (Value::Limited(read), Value::Limited(made)) = (value(block), &narrowed) else {
            panic!("narrowed")
        };
        assert_eq!(read, *made);
        let plain = with(|state| Some(state.push(Value::Capability(Capability::Stdout)))).unwrap();
        assert_eq!(super::from_value(plain, 0, 0), super::block(0));
        heap::free(block);
        assert_eq!(with(|state| state.heap.live()), live);
        let not = with(|state| Some(state.push(Value::Int(3)))).unwrap();
        assert_eq!(super::from_value(not, 0, 0), 0);
        let (_, failure, _) = finish();
        assert!(failure.is_some_and(|failure| failure.message.contains("expected a capability")));
    }

    #[test]
    fn a_narrowed_capability_s_methods_run_from_native_code() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(tables, crate::Inputs::default());
        let string = |text: &str| with(|state| heap::text_address(state, text));
        let input = |value: Value| {
            let temp = with(|state| Some(state.push(value))).unwrap();
            super::super::eval::input(temp);
        };
        // `process.only("echo")` gives a narrowed capability, which crosses as a block.
        input(Value::str("echo"));
        let narrowed = super::native_call(super::block(5), string("only"), 1, 0, 0);
        let block = super::from_value(narrowed, 0, 0);
        let Value::Limited(limited) = value(block) else { panic!("narrowed") };
        assert_eq!(limited.allowed, vec![Rc::<str>::from("echo")]);
        // Running something else is refused as a `ProcessError`, and nothing is started.
        input(Value::str("rm"));
        input(Value::list(vec![Value::str("-rf"), Value::str("/")]));
        let refused = super::native_call(block, string("run"), 2, 0, 0);
        let Value::Err(error) = with(|state| Some(state.temp(refused))).unwrap() else { panic!("refused") };
        assert!(format!("{error:?}").contains("can run only `echo`"), "{error:?}");
        heap::free(block);
        finish();
    }

    /// How much each part of the runtime's state holds: its live blocks, temporaries, inputs and
    /// strings' texts.
    fn sizes() -> (usize, usize, usize, usize) {
        with(|state| {
            Some((state.heap.live(), state.temps.len(), state.inputs.len(), state.heap.texts.borrow().len()))
        })
        .unwrap()
    }

    /// A capability's method that can fail, called from native code over and over the way it calls
    /// one, `fs.read_text(path)?`, leaves nothing behind in the runtime's state.
    #[test]
    fn a_capability_call_from_native_code_leaves_nothing_behind() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(tables, crate::Inputs::default());
        let dir = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-capability-calls-{}", std::process::id())),
        );
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("w.txt");
        std::fs::write(&path, "z".repeat(500)).unwrap();
        let path = path.to_string_lossy().into_owned();
        let round = || {
            let mark = super::super::temps::mark();
            let text = with(|state| heap::text_address(state, &path));
            let value = heap::to_value(text);
            super::super::eval::input(value);
            let name = with(|state| heap::text_address(state, "read_text"));
            let result = super::native_call(1, name, 1, 0, 0);
            assert_eq!(super::super::temps::is_err(result), 0);
            let read = super::super::temps::unwrap(result);
            let block = super::super::strings::from_value(read, 0, 0);
            heap::free(text);
            heap::free(name);
            heap::free(block);
            super::super::temps::truncate(mark);
        };
        for _ in 0..100 {
            round();
        }
        let before = sizes();
        for _ in 0..1000 {
            round();
        }
        assert_eq!(sizes(), before);
        finish();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
