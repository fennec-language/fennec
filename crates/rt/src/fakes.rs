//! What a test's capabilities reach: an in-memory `Fs`, a fixed `Clock`, captured `Stdout`, a
//! `Process` that runs nothing but gives back what the test scripted, an `Entropy` whose
//! bytes are fixed, an `Env` holding only what the test set, and a `Net` whose servers and clients talk in memory, fresh for each run of a test. These hold state every copy of a capability shares, which
//! Fennec code has no way to do yet, so they're written here. With `fennec test --real` only
//! the output is kept, and the rest is real.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use fennec_syntax::{Diagnostic, Span};

use crate::stdlib::fs::Fail;
use crate::value::{Capability, Value};

enum Node {
    File(Vec<u8>),
    Folder,
}

/// What tests get when they name a capability.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TestCapabilities {
    Fakes,
    Real,
}

/// What a test printed and wrote to standard error, shared with every task and `parallel` range it
/// starts, so what they print is the test's too, whatever thread runs them. It closes when the
/// test ends: a task still writing then has outlived its test, and what it writes goes to standard
/// error with a warning, never into another test's.
pub(crate) struct Capture(Mutex<Captured>);

#[derive(Default)]
struct Captured {
    printed: String,
    errors: String,
    closed: bool,
}

impl Capture {
    pub(crate) fn new() -> Arc<Capture> {
        Arc::new(Capture(Mutex::new(Captured::default())))
    }

    fn captured(&self) -> std::sync::MutexGuard<'_, Captured> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps what a task of the test printed.
    pub(crate) fn print(&self, text: &str) {
        let mut captured = self.captured();
        if captured.closed {
            drop(captured);
            late("printed", text);
        } else {
            captured.printed.push_str(text);
        }
    }

    /// Keeps what a task of the test wrote to standard error.
    pub(crate) fn error(&self, text: &str) {
        let mut captured = self.captured();
        if captured.closed {
            drop(captured);
            late("wrote to standard error", text);
        } else {
            captured.errors.push_str(text);
        }
    }

    pub(crate) fn printed(&self) -> String {
        self.captured().printed.clone()
    }

    fn errors(&self) -> String {
        self.captured().errors.clone()
    }

    /// The test has ended: anything written from now on is a task's that outlived it.
    fn close(&self) {
        self.captured().closed = true;
    }
}

/// What a task wrote after its test ended, said on standard error rather than kept for a test.
fn late(what: &str, text: &str) {
    let _ = writeln!(
        std::io::stderr(),
        "warning: a task {what} after its test ended, so it's here rather than in what the test captured:\n{text}"
    );
}

pub(crate) struct TestWorld {
    capabilities: TestCapabilities,
    /// By path, as `key` writes it. The top folder is always there, and isn't listed.
    nodes: BTreeMap<String, Node>,
    nanos: i64,
    /// What the test and its tasks printed and wrote to standard error.
    capture: Arc<Capture>,
    /// What each program gives back, by name, as the test scripted it.
    pub(crate) scripts: BTreeMap<String, Value>,
    /// Each program the test ran, with its arguments, in order.
    pub(crate) ran: Vec<Vec<String>>,
    /// How many bytes the fake `Entropy` has given so far.
    pub(crate) entropy_drawn: usize,
    /// The fake `Env`'s variables, by name, as the test set them.
    pub(crate) variables: BTreeMap<String, String>,
    /// Whether the test asked its fake `Signals` to stop.
    pub(crate) stopped: bool,
    /// The test's network, in memory.
    pub(crate) net: crate::stdlib::net::Fake,
    /// The test's SQLite databases, by path, each in a file of the test's own.
    pub(crate) databases: crate::stdlib::sqlite::Fakes,
}

/// A test's world goes when the test ends, and its capture closes with it.
impl Drop for TestWorld {
    fn drop(&mut self) {
        self.capture.close();
    }
}

impl TestWorld {
    pub(crate) fn new(capabilities: TestCapabilities) -> Self {
        TestWorld {
            capabilities,
            nodes: BTreeMap::new(),
            nanos: 0,
            capture: Capture::new(),
            scripts: BTreeMap::new(),
            ran: Vec::new(),
            entropy_drawn: 0,
            variables: BTreeMap::new(),
            stopped: false,
            net: crate::stdlib::net::Fake::default(),
            databases: crate::stdlib::sqlite::Fakes::default(),
        }
    }

    pub(crate) fn real(&self) -> bool {
        self.capabilities == TestCapabilities::Real
    }

    /// What the test and its tasks have printed, with what the test holds of a line it hasn't
    /// ended yet.
    pub(crate) fn printed(&self) -> String {
        crate::embed::flush_held_line();
        self.capture.printed()
    }

    /// The test's capture, for the tasks and ranges it starts to print into.
    pub(crate) fn capture(&self) -> Arc<Capture> {
        Arc::clone(&self.capture)
    }

    pub(crate) fn now(&self) -> i64 {
        self.nanos
    }

    /// Time passes at once, so a test that sleeps takes no time.
    pub(crate) fn sleep(&mut self, nanos: u64) {
        self.nanos = self.nanos.saturating_add(i64::try_from(nanos).unwrap_or(i64::MAX));
    }

    fn is_folder(&self, key: &str) -> bool {
        key.is_empty() || matches!(self.nodes.get(key), Some(Node::Folder))
    }

    fn read(&self, path: &str) -> Result<&[u8], Fail> {
        match self.nodes.get(&key(path)) {
            Some(Node::File(data)) => Ok(data),
            Some(Node::Folder) => Err(format!("{path} is a folder").into()),
            None if key(path).is_empty() => Err(format!("{path} is a folder").into()),
            None => Err(Fail::not_found(format!("{path} doesn't exist"))),
        }
    }

    /// `fs._sqlite_open(path)` in a test: the test's database in memory for `path`. Its file is
    /// there to see, empty, as a real one would be, and a folder that isn't there fails it.
    /// Read only, through an `Fs` narrowed with `read_only`, one that isn't there isn't made.
    pub(crate) fn open_database(&mut self, path: &str, read_only: bool) -> Value {
        let key = key(path);
        let there = matches!(self.nodes.get(&key), Some(Node::File(_)));
        if read_only && !there {
            return crate::stdlib::fs::io_failed(&Fail::not_found(format!("{path} doesn't exist")));
        }
        if !there && let Err(reason) = self.write(path, &[]) {
            return crate::stdlib::fs::io_failed(&reason);
        }
        crate::stdlib::sqlite::open_fake(&mut self.databases, &key, read_only)
    }

    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), Fail> {
        let key = key(path);
        if self.is_folder(&key) {
            return Err(format!("{path} is a folder").into());
        }
        if !self.is_folder(parent(&key)) {
            return Err(Fail::not_found(format!("the folder of {path} doesn't exist")));
        }
        self.nodes.insert(key, Node::File(data.to_vec()));
        Ok(())
    }

    /// Makes a folder and the folders above it, which can already be there.
    fn make_dir(&mut self, path: &str) -> Result<(), Fail> {
        let key = key(path);
        let ends = key.match_indices('/').map(|(at, _)| at).chain([key.len()]);
        for end in ends {
            let folder = key.get(..end).unwrap_or_default();
            if matches!(self.nodes.get(folder), Some(Node::File(_))) {
                return Err(format!("{folder} is a file").into());
            }
            if !folder.is_empty() {
                self.nodes.insert(folder.to_owned(), Node::Folder);
            }
        }
        Ok(())
    }

    fn remove(&mut self, path: &str) -> Result<(), Fail> {
        let key = key(path);
        if key.is_empty() {
            return Err(format!("{path} can't be removed").into());
        }
        if self.children(&key).next().is_some() {
            return Err(format!("{path} isn't empty").into());
        }
        match self.nodes.remove(&key) {
            Some(_) => Ok(()),
            None => Err(Fail::not_found(format!("{path} doesn't exist"))),
        }
    }

    /// The names of what's directly in a folder, in order.
    fn children<'w>(&'w self, folder: &str) -> impl Iterator<Item = String> + 'w {
        let prefix = if folder.is_empty() { String::new() } else { format!("{folder}/") };
        self.nodes.keys().filter_map(move |key| {
            let name = key.strip_prefix(&prefix)?;
            (!name.contains('/')).then(|| name.to_owned())
        })
    }

    /// Every path under `folder`, as the real `walk` gives them.
    fn walk_into(&self, folder: &str, found: &mut Vec<String>) -> Result<(), Fail> {
        for name in self.list(folder)? {
            let path = crate::stdlib::fs::joined(folder, &name);
            found.push(path.clone());
            if self.is_folder(&key(&path)) {
                self.walk_into(&path, found)?;
            }
        }
        Ok(())
    }

    fn list(&self, path: &str) -> Result<Vec<String>, Fail> {
        let key = key(path);
        if self.is_folder(&key) {
            return Ok(self.children(&key).collect());
        }
        match self.nodes.get(&key) {
            Some(_) => Err(format!("{path} is a file").into()),
            None => Err(Fail::not_found(format!("{path} doesn't exist"))),
        }
    }
}

/// A path with `.` and `..` worked out. There's one top folder, so `/a` and `a` are the same.
fn key(path: &str) -> String {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => drop(parts.pop()),
            part => parts.push(part),
        }
    }
    parts.join("/")
}

fn parent(key: &str) -> &str {
    key.rsplit_once('/').map_or("", |(parent, _)| parent)
}

impl TestWorld {
    /// A method of a test's capability, or nothing where the world leaves that to the real
    /// capability: with `fennec test --real`, all but `stdout.captured()`, and `only`, which
    /// narrows what it's given.
    pub(crate) fn method(
        &mut self,
        capability: Capability,
        method: &str,
        args: &[Value],
        span: Span,
    ) -> Option<Result<Value, Diagnostic>> {
        if capability == Capability::Stdout && method == "captured" {
            return Some(Ok(Value::str(&self.printed())));
        }
        // What tasks print comes out in the order they write it, which differs between runs, so a
        // test compares their lines sorted.
        if capability == Capability::Stdout && method == "sorted_lines" {
            let printed = self.printed();
            let mut lines: Vec<&str> = printed.lines().collect();
            lines.sort_unstable();
            return Some(Ok(Value::list(lines.into_iter().map(Value::str).collect())));
        }
        if capability == Capability::Stderr && method == "captured" {
            return Some(Ok(Value::str(&self.capture.errors())));
        }
        if self.real() || method == "only" {
            return None;
        }
        match capability {
            Capability::Fs => {
                let paths: Vec<Value> = args.iter().map(crate::stdlib::fs::path_text).collect();
                crate::stdlib::fs::fake_method(self, method, &paths, span)
            }
            Capability::Clock => Some(self.clock(method, args, span)),
            Capability::Entropy => Some(self.entropy(method, args, span)),
            Capability::Env => Some(self.env(method, args, span)),
            Capability::Signals => Some(self.signals(method, args, span)),
            Capability::Stderr => Some(crate::stderr::faked(&self.capture, method, args, span)),
            Capability::Process => self.process(method, args, span),
            Capability::Net => Some(self.net.net_method(method, args, span)),
            Capability::Stdout | Capability::Stdin | Capability::Args => None,
        }
    }

    /// A method of the fake `Fs` on paths as `path_text` gives them, or nothing if the
    /// arguments are wrong.
    pub(crate) fn fs(
        &mut self,
        method: &str,
        paths: &[Value],
        span: Span,
    ) -> Option<Result<Value, Diagnostic>> {
        let _ = span;
        let world = self;
        let done = match (method, paths) {
            ("exists", [Value::Str(path)]) => {
                let key = key(path);
                Ok(Value::Bool(key.is_empty() || world.nodes.contains_key(&key)))
            }
            ("read_text", [Value::Str(path)]) => world.read(path).and_then(|data| {
                let text = std::str::from_utf8(data).map_err(|err| format!("{path} isn't text: {err}"))?;
                Ok(Value::str(text))
            }),
            ("read_bytes", [Value::Str(path)]) => {
                world.read(path).map(|data| Value::Bytes(Rc::from(data.to_vec())))
            }
            ("write_text", [Value::Str(path), Value::Str(text)]) => {
                world.write(path, text.as_bytes()).map(|()| Value::Unit)
            }
            ("write_bytes", [Value::Str(path), Value::Bytes(data)]) => {
                world.write(path, data).map(|()| Value::Unit)
            }
            ("make_dir", [Value::Str(path)]) => world.make_dir(path).map(|()| Value::Unit),
            ("remove", [Value::Str(path)]) => world.remove(path).map(|()| Value::Unit),
            ("list_dir", [Value::Str(path)]) => {
                return Some(match world.list(path) {
                    Ok(names) => Ok(crate::stdlib::fs::paths_in(path, &names)),
                    Err(reason) => Ok(crate::stdlib::fs::io_failed(&reason)),
                });
            }
            ("walk", [Value::Str(path)]) => {
                let mut found = Vec::new();
                return Some(match world.walk_into(path, &mut found) {
                    Ok(()) => Ok(crate::stdlib::fs::paths_of(&found)),
                    Err(reason) => Ok(crate::stdlib::fs::io_failed(&reason)),
                });
            }
            ("metadata", [Value::Str(path)]) => {
                return Some(Ok(match metadata(world, path) {
                    Ok(fields) => Value::Ok(Rc::new(Value::made("Metadata", fields))),
                    Err(reason) => crate::stdlib::fs::io_failed(&reason),
                }));
            }
            _ => return None,
        };
        Some(match done {
            Ok(value) => Ok(Value::Ok(Rc::new(value))),
            Err(reason) => Ok(crate::stdlib::fs::io_failed(&reason)),
        })
    }

    /// The fake `Clock`, which stands still but for sleeping, which passes no time at all.
    fn clock(&mut self, method: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
        match (method, args) {
            ("now", []) => Ok(Value::made("Instant", vec![("nanos", Value::Int(self.now()))])),
            ("time", []) => {
                let nanos = crate::collections::FAKE_TIME.saturating_add(self.now());
                Ok(Value::made("Time", vec![("nanos", Value::Int(nanos))]))
            }
            ("sleep", [Value::Instance(duration)]) if duration.type_name.as_ref() == "Duration" => {
                let nanos = match duration.field("nanos") {
                    Some(Value::Int(nanos)) => u64::try_from(*nanos).unwrap_or(0),
                    _ => 0,
                };
                self.sleep(nanos);
                Ok(Value::Unit)
            }
            ("now" | "sleep" | "time", _) => {
                Err(Diagnostic::new(format!("wrong arguments to `{method}`"), span))
            }
            _ => Err(crate::methods::no_method(&Value::Capability(Capability::Clock), method, span)),
        }
    }

    /// The fake `Entropy`'s next `count` bytes, the same in every run.
    pub(crate) fn draw(&mut self, count: usize) -> Vec<u8> {
        let start = self.entropy_drawn;
        self.entropy_drawn += count;
        (start..start + count).map(crate::entropy::fake_byte).collect()
    }

    fn entropy(&mut self, method: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
        match (method, args) {
            ("bytes", [Value::Int(count)]) => {
                let Ok(count) = usize::try_from(*count) else {
                    return Err(Diagnostic::new(format!("`bytes` can't give {count} bytes"), span)
                        .with_help("give it how many bytes you want, 0 or more"));
                };
                Ok(Value::Bytes(Rc::from(self.draw(count))))
            }
            ("seed", []) => {
                let word: [u8; 8] = self
                    .draw(8)
                    .try_into()
                    .map_err(|_| Diagnostic::new("the operating system gave too few bytes", span))?;
                Ok(Value::Int(i64::from_le_bytes(word)))
            }
            ("bytes" | "seed", _) => Err(Diagnostic::new(format!("wrong arguments to `{method}`"), span)),
            _ => Err(crate::methods::no_method(&Value::Capability(Capability::Entropy), method, span)),
        }
    }

    /// The fake `Env`, holding only what the test set.
    pub(crate) fn env(&mut self, method: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
        match (method, args) {
            ("get", [Value::Str(name)]) => {
                Ok(self.variables.get(name.as_ref()).map_or(Value::None, |value| Value::str(value)))
            }
            ("all", []) => Ok(crate::env::dict_of(
                self.variables.iter().map(|(name, value)| (name.as_str(), value.as_str())),
            )),
            ("set", [Value::Str(name), Value::Str(value)]) => {
                self.variables.insert(name.to_string(), value.to_string());
                Ok(Value::Unit)
            }
            ("get" | "all" | "set", _) => {
                Err(Diagnostic::new(format!("wrong arguments to `{method}`"), span))
            }
            _ => Err(crate::methods::no_method(&Value::Capability(Capability::Env), method, span)),
        }
    }

    /// The fake `Signals`, which stops only when the test asks it to.
    pub(crate) fn signals(&mut self, method: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
        match (method, args) {
            ("stopping", []) => Ok(Value::Bool(self.stopped)),
            ("stop", []) => {
                self.stopped = true;
                Ok(Value::Unit)
            }
            ("wait", []) if self.stopped => Ok(Value::Unit),
            // Nothing else in a test can ask it to stop, so the wait would never end.
            ("wait", []) => {
                Err(Diagnostic::new("`wait` would wait forever: the test never stopped its `Signals`", span)
                    .with_help("call `signals.stop()` in the test first"))
            }
            _ => Err(crate::methods::no_method(&Value::Capability(Capability::Signals), method, span)),
        }
    }

    /// The fake `Process`, which runs nothing and gives back what the test scripted, or nothing
    /// for a method it leaves to the real one.
    pub(crate) fn process(
        &mut self,
        method: &str,
        args: &[Value],
        span: Span,
    ) -> Option<Result<Value, Diagnostic>> {
        Some(match (method, args) {
            (run, _) if run == "run" || run.starts_with("run.") => {
                crate::stdlib::process::run_of(run, args, span).map(|ran| match ran {
                    Ok((program, arguments, _)) => self.run(&program, arguments),
                    Err(refused) => refused,
                })
            }
            ("script", [Value::Str(program), output @ Value::Instance(_)]) => {
                self.scripts.insert(program.to_string(), output.clone());
                Ok(Value::Unit)
            }
            ("ran", []) => {
                let lists =
                    self.ran.iter().map(|run| Value::list(run.iter().map(|part| Value::str(part)).collect()));
                Ok(Value::list(lists.collect()))
            }
            ("script", _) => {
                Err(Diagnostic::new(format!("`{method}` was called with the wrong arguments"), span))
            }
            _ => return None,
        })
    }

    /// What the fake `Process` gives back for `program`: what the test scripted for it, or a
    /// `ProcessError` saying nothing was.
    pub(crate) fn run(&mut self, program: &str, arguments: Vec<String>) -> Value {
        let mut run = vec![program.to_owned()];
        run.extend(arguments);
        self.ran.push(run);
        if let Some(output) = self.scripts.get(program).cloned() {
            return Value::Ok(Rc::new(output));
        }
        let message = format!(
            "the test's fake Process has nothing scripted for `{program}`: give it what to answer with `process.script(\"{program}\", Output(...))`"
        );
        crate::stdlib::process::process_error(program, &message)
    }
}

fn metadata(world: &TestWorld, path: &str) -> Result<Vec<(&'static str, Value)>, Fail> {
    let key = key(path);
    let size = match world.nodes.get(&key) {
        Some(Node::File(data)) => Some(i64::try_from(data.len()).unwrap_or(i64::MAX)),
        Some(Node::Folder) => None,
        None if key.is_empty() => None,
        None => return Err(Fail::not_found(format!("{path} doesn't exist"))),
    };
    Ok(vec![
        ("size", Value::Int(size.unwrap_or(0))),
        ("is_file", Value::Bool(size.is_some())),
        ("is_dir", Value::Bool(size.is_none())),
    ])
}

#[cfg(test)]
mod tests {
    use super::Capture;

    /// Once its test has ended, a capture takes nothing more: a task still writing has outlived the
    /// test, and what it writes goes to standard error, never into what the test, or the next one,
    /// captured.
    #[test]
    fn a_capture_takes_nothing_once_its_test_has_ended() {
        let capture = Capture::new();
        capture.print("kept\n");
        capture.error("said\n");
        capture.close();
        capture.print("too late\n");
        capture.error("too late\n");
        assert_eq!((capture.printed(), capture.errors()), ("kept\n".to_owned(), "said\n".to_owned()));
    }
}
