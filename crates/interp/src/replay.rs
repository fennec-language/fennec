//! Recording a run, and replaying it bit for bit. A recorded run logs every outside input a program
//! takes, as it comes through a capability: the clock, entropy, files, the network, standard input,
//! the environment and other programs' results. What each call gave is logged, errors from the
//! operating system included. A replay gives the program the logged results in place of the calls
//! themselves. So it reads no clock, file or socket, and a single-task program does exactly what it
//! did.
//!
//! - **Recording and replaying** is asked for with `FENNEC_RECORD=log` or `FENNEC_REPLAY=log`, or
//!   from the same process with `record_to` and `replay_from`. `fennec run --record` and
//!   `fennec replay` do it for a program, with what it was run with in the log's header.
//! - **Divergence:** each entry names its call. A replay that asks for a different call than the log
//!   has next, or for more calls than it has, or ends with entries left, stops with both named. That
//!   catches any outside input this doesn't record yet.
//! - **A normal run** pays one load to see that nothing's recorded.
//!
//! A run of several tasks, whose order between them isn't recorded yet, can diverge.
//!
//! The log is `FENREPL1`, the header's length as four bytes and the header, then the entries. Each
//! entry is its call's name, with its length as two bytes, then what it gave, with its length as
//! four. All numbers are little-endian.

use std::io::Write as _;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock, PoisonError};

use fennec_syntax::{Diagnostic, Span};

use crate::value::{Instance, Value};

const MAGIC: &[u8; 8] = b"FENREPL1";

enum Mode {
    Off,
    Record(Mutex<std::fs::File>),
    Replay(Mutex<Player>),
}

struct Player {
    log: Vec<u8>,
    at: usize,
}

static MODE: OnceLock<Mode> = OnceLock::new();

fn mode() -> &'static Mode {
    MODE.get_or_init(|| {
        if let Some(path) = std::env::var_os("FENNEC_REPLAY") {
            return playing(Path::new(&path)).unwrap_or(Mode::Off);
        }
        match std::env::var_os("FENNEC_RECORD") {
            Some(path) => recording(Path::new(&path)).unwrap_or(Mode::Off),
            None => Mode::Off,
        }
    })
}

fn recording(path: &Path) -> std::io::Result<Mode> {
    let file = std::fs::OpenOptions::new().append(true).open(path)?;
    Ok(Mode::Record(Mutex::new(file)))
}

fn playing(path: &Path) -> std::io::Result<Mode> {
    let log = std::fs::read(path)?;
    let at = entries_start(&log).ok_or_else(|| std::io::Error::other("this isn't a recording"))?;
    Ok(Mode::Replay(Mutex::new(Player { log, at })))
}

/// Starts a log at `path` with `header`, for a run recorded into it.
pub fn start_log(path: &Path, header: &[u8]) -> std::io::Result<()> {
    let mut log = MAGIC.to_vec();
    log.extend(u32::try_from(header.len()).unwrap_or(u32::MAX).to_le_bytes());
    log.extend(header);
    std::fs::write(path, log)
}

/// The header of the log at `path`.
pub fn header_of(path: &Path) -> std::io::Result<Vec<u8>> {
    let log = std::fs::read(path)?;
    let start = entries_start(&log).ok_or_else(|| std::io::Error::other("this isn't a recording"))?;
    Ok(log.get(MAGIC.len() + 4..start).unwrap_or_default().to_vec())
}

fn entries_start(log: &[u8]) -> Option<usize> {
    if log.get(..MAGIC.len())? != MAGIC {
        return None;
    }
    let length = u32::from_le_bytes(log.get(MAGIC.len()..MAGIC.len() + 4)?.try_into().ok()?);
    let start = MAGIC.len() + 4 + usize::try_from(length).ok()?;
    (start <= log.len()).then_some(start)
}

/// Records this process's run into the log at `path`, which `start_log` began, if nothing's been
/// recorded or replayed yet.
pub fn record_to(path: &Path) -> std::io::Result<()> {
    let _ = MODE.set(recording(path)?);
    Ok(())
}

/// Replays the run logged at `path` in this process, if nothing's been recorded or replayed yet.
pub fn replay_from(path: &Path) -> std::io::Result<()> {
    let _ = MODE.set(playing(path)?);
    Ok(())
}

/// What a replay that's run out of its log, or ended before it, says: `None` when it hasn't.
pub fn unfinished() -> Option<String> {
    let Mode::Replay(player) = mode() else { return None };
    let player = player.lock().unwrap_or_else(PoisonError::into_inner);
    let next = next_name(&player)?;
    Some(format!(
        "this replay ended where its recording asked for `{next}` next, so it isn't the run that was \
         recorded: something it depends on wasn't recorded, or the program has changed"
    ))
}

fn next_name(player: &Player) -> Option<String> {
    let length = usize::from(u16::from_le_bytes(player.log.get(player.at..player.at + 2)?.try_into().ok()?));
    let name = player.log.get(player.at + 2..player.at + 2 + length)?;
    Some(String::from_utf8_lossy(name).into_owned())
}

/// The result of a capability's call named `call`: in a normal run what `live` gives, in a
/// recording what it gives, logged, and in a replay the logged one, without `live` running.
///
/// The call's name is written out only in a recording or a replay, so a normal run pays nothing for
/// it.
pub(crate) fn value(
    call: std::fmt::Arguments<'_>,
    span: Span,
    live: impl FnOnce() -> Result<Value, Diagnostic>,
) -> Result<Value, Diagnostic> {
    match mode() {
        Mode::Off => live(),
        Mode::Record(file) => {
            let result = live();
            let mut payload = Vec::new();
            encode_result(&result, &mut payload);
            log(file, &call.to_string(), &payload);
            result
        }
        Mode::Replay(player) => {
            let call = call.to_string();
            let payload = replayed(player, &call).map_err(|why| Diagnostic::new(why, span))?;
            decode_result(&payload, span).ok_or_else(|| {
                Diagnostic::new(format!("this replay's recording of `{call}` couldn't be read back"), span)
            })?
        }
    }
}

/// A word an outside call gives, like the clock's reading, recorded and replayed as `value` is.
pub(crate) fn word(call: &str, live: impl FnOnce() -> i64) -> Result<i64, String> {
    match mode() {
        Mode::Off => Ok(live()),
        Mode::Record(file) => {
            let word = live();
            log(file, call, &word.to_le_bytes());
            Ok(word)
        }
        Mode::Replay(player) => {
            let payload = replayed(player, call)?;
            let bytes: [u8; 8] =
                payload.try_into().map_err(|_| format!("this replay's recording of `{call}` is damaged"))?;
            Ok(i64::from_le_bytes(bytes))
        }
    }
}

fn log(file: &Mutex<std::fs::File>, call: &str, payload: &[u8]) {
    // Written at once, so a run that crashes or is killed leaves what it took.
    let _ = file.lock().unwrap_or_else(PoisonError::into_inner).write_all(&entry(call, payload));
}

/// One entry of a log.
fn entry(call: &str, payload: &[u8]) -> Vec<u8> {
    let mut entry = Vec::with_capacity(call.len() + payload.len() + 6);
    entry.extend(u16::try_from(call.len()).unwrap_or(u16::MAX).to_le_bytes());
    entry.extend(call.as_bytes());
    entry.extend(u32::try_from(payload.len()).unwrap_or(u32::MAX).to_le_bytes());
    entry.extend(payload);
    entry
}

/// The logged result of the next call, which must be `call`.
fn replayed(player: &Mutex<Player>, call: &str) -> Result<Vec<u8>, String> {
    let mut player = player.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(next) = next_name(&player) else {
        return Err(format!(
            "this replay asked for `{call}`, where its recording had nothing more, so it isn't the run that \
             was recorded: something it depends on wasn't recorded, or the program has changed"
        ));
    };
    if next != call {
        return Err(format!(
            "this replay asked for `{call}`, where its recording asked for `{next}`, so it isn't the run that \
             was recorded: something it depends on wasn't recorded, or the program has changed"
        ));
    }
    let start = player.at + 2 + call.len();
    let length = player
        .log
        .get(start..start + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .and_then(|length| usize::try_from(length).ok())
        .ok_or_else(|| format!("this replay's recording of `{call}` is damaged"))?;
    let payload = player
        .log
        .get(start + 4..start + 4 + length)
        .ok_or_else(|| format!("this replay's recording of `{call}` is damaged"))?
        .to_vec();
    player.at = start + 4 + length;
    Ok(payload)
}

/// What a value is, in the log.
mod tag {
    pub(super) const UNIT: u8 = 0;
    pub(super) const NONE: u8 = 1;
    pub(super) const BOOL: u8 = 2;
    pub(super) const INT: u8 = 3;
    pub(super) const FLOAT: u8 = 4;
    pub(super) const STR: u8 = 5;
    pub(super) const BYTES: u8 = 6;
    pub(super) const LIST: u8 = 7;
    pub(super) const TUPLE: u8 = 8;
    pub(super) const DICT: u8 = 9;
    pub(super) const SET: u8 = 10;
    pub(super) const INSTANCE: u8 = 11;
    pub(super) const OK: u8 = 12;
    pub(super) const ERR: u8 = 13;
    /// A value that isn't plain data, which a replay can't give back.
    pub(super) const UNRECORDABLE: u8 = 14;
    pub(super) const FAILED: u8 = 15;
}

fn encode_result(result: &Result<Value, Diagnostic>, out: &mut Vec<u8>) {
    match result {
        Ok(value) => encode(value, out),
        Err(diagnostic) => {
            out.push(tag::FAILED);
            text(&diagnostic.message, out);
            match &diagnostic.help {
                Some(help) => {
                    out.push(1);
                    text(help, out);
                }
                None => out.push(0),
            }
        }
    }
}

fn text(text: &str, out: &mut Vec<u8>) {
    out.extend(u32::try_from(text.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend(text.as_bytes());
}

fn count(count: usize, out: &mut Vec<u8>) {
    out.extend(u32::try_from(count).unwrap_or(u32::MAX).to_le_bytes());
}

fn encode(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Unit => out.push(tag::UNIT),
        Value::None => out.push(tag::NONE),
        Value::Bool(bool) => out.extend([tag::BOOL, u8::from(*bool)]),
        Value::Int(int) => {
            out.push(tag::INT);
            out.extend(int.to_le_bytes());
        }
        Value::Float(float) => {
            out.push(tag::FLOAT);
            out.extend(float.to_bits().to_le_bytes());
        }
        Value::Str(string) => {
            out.push(tag::STR);
            text(string, out);
        }
        Value::Bytes(bytes) => {
            out.push(tag::BYTES);
            count(bytes.len(), out);
            out.extend(bytes.iter());
        }
        Value::List(items) | Value::Tuple(items) | Value::Set(items) => {
            out.push(match value {
                Value::List(_) => tag::LIST,
                Value::Tuple(_) => tag::TUPLE,
                _ => tag::SET,
            });
            count(items.len(), out);
            for item in items.iter() {
                encode(item, out);
            }
        }
        Value::Packed(packed) => encode(&packed.unpack(), out),
        Value::Dict(pairs) => {
            out.push(tag::DICT);
            count(pairs.len(), out);
            for (key, item) in pairs.iter() {
                encode(key, out);
                encode(item, out);
            }
        }
        Value::Instance(instance) => {
            out.push(tag::INSTANCE);
            text(&instance.type_name, out);
            match &instance.case {
                Some(case) => {
                    out.push(1);
                    text(case, out);
                }
                None => out.push(0),
            }
            out.push(u8::from(instance.is_error));
            match &instance.cause {
                Some(cause) => {
                    out.push(1);
                    encode(cause, out);
                }
                None => out.push(0),
            }
            count(instance.fields.len(), out);
            for (name, field) in &instance.fields {
                text(name, out);
                encode(field, out);
            }
        }
        Value::Ok(held) | Value::Err(held) => {
            out.push(if matches!(value, Value::Ok(_)) { tag::OK } else { tag::ERR });
            encode(held, out);
        }
        _ => out.push(tag::UNRECORDABLE),
    }
}

/// Reads values back from a log entry.
struct Reader<'log> {
    bytes: &'log [u8],
    at: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(byte)
    }

    fn take(&mut self, length: usize) -> Option<&[u8]> {
        let taken = self.bytes.get(self.at..self.at + length)?;
        self.at += length;
        Some(taken)
    }

    fn eight(&mut self) -> Option<[u8; 8]> {
        self.take(8)?.try_into().ok()
    }

    fn count(&mut self) -> Option<usize> {
        usize::try_from(u32::from_le_bytes(self.take(4)?.try_into().ok()?)).ok()
    }

    fn text(&mut self) -> Option<Rc<str>> {
        let length = self.count()?;
        Some(Rc::from(std::str::from_utf8(self.take(length)?).ok()?))
    }

    fn values(&mut self) -> Option<Vec<Value>> {
        let length = self.count()?;
        (0..length).map(|_| self.value()).collect()
    }

    fn value(&mut self) -> Option<Value> {
        Some(match self.byte()? {
            tag::UNIT => Value::Unit,
            tag::NONE => Value::None,
            tag::BOOL => Value::Bool(self.byte()? != 0),
            tag::INT => Value::Int(i64::from_le_bytes(self.eight()?)),
            tag::FLOAT => Value::Float(f64::from_bits(u64::from_le_bytes(self.eight()?))),
            tag::STR => Value::Str(self.text()?),
            tag::BYTES => {
                let length = self.count()?;
                Value::Bytes(Rc::from(self.take(length)?))
            }
            tag::LIST => Value::List(Rc::new(self.values()?)),
            tag::TUPLE => Value::Tuple(Rc::new(self.values()?)),
            tag::SET => Value::Set(Rc::new(self.values()?)),
            tag::DICT => {
                let length = self.count()?;
                let pairs: Option<Vec<(Value, Value)>> =
                    (0..length).map(|_| Some((self.value()?, self.value()?))).collect();
                Value::Dict(Rc::new(crate::pairs::Pairs::from(pairs?)))
            }
            tag::INSTANCE => {
                let type_name = self.text()?;
                let case = if self.byte()? == 1 { Some(self.text()?) } else { None };
                let is_error = self.byte()? != 0;
                let raised_from = if self.byte()? == 1 { Some(self.value()?) } else { None };
                let length = self.count()?;
                let fields: Option<Vec<(Rc<str>, Value)>> =
                    (0..length).map(|_| Some((self.text()?, self.value()?))).collect();
                Value::Instance(Rc::new(Instance {
                    type_name,
                    case,
                    fields: fields?,
                    is_error,
                    cause: raised_from,
                    // What a capability gives back never has notes: a program adds them after.
                    notes: None,
                }))
            }
            tag::OK => Value::Ok(Rc::new(self.value()?)),
            tag::ERR => Value::Err(Rc::new(self.value()?)),
            _ => return None,
        })
    }
}

fn decode_result(payload: &[u8], span: Span) -> Option<Result<Value, Diagnostic>> {
    let mut reader = Reader { bytes: payload, at: 0 };
    if payload.first() == Some(&tag::FAILED) {
        reader.byte()?;
        let message = reader.text()?;
        let help = if reader.byte()? == 1 { Some(reader.text()?) } else { None };
        let mut diagnostic = Diagnostic::new(message.to_string(), span);
        if let Some(help) = help {
            diagnostic = diagnostic.with_help(help.to_string());
        }
        return Some(Err(diagnostic));
    }
    Some(Ok(reader.value()?))
}

#[cfg(test)]
mod tests;
