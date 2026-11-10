//! What a test's capabilities reach: an in-memory `Fs`, a fixed `Clock`, captured `Stdout`, a
//! `Process` that runs nothing but gives back what the test scripted, an `Entropy` whose
//! bytes are fixed, an `Env` holding only what the test set, and a `Net` whose servers and clients talk in memory, fresh for each run of a test. These hold state every copy of a capability shares, which
//! Fennec code has no way to do yet, so they're written here. With `fennec test --real` only
//! the output is kept, and the rest is real.

use std::collections::BTreeMap;
use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

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

pub(crate) struct TestWorld {
    capabilities: TestCapabilities,
    /// By path, as `key` writes it. The top folder is always there, and isn't listed.
    nodes: BTreeMap<String, Node>,
    nanos: i64,
    printed: String,
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

impl TestWorld {
    pub(crate) fn new(capabilities: TestCapabilities) -> Self {
        TestWorld {
            capabilities,
            nodes: BTreeMap::new(),
            nanos: 0,
            printed: String::new(),
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

    pub(crate) fn printed(&self) -> &str {
        &self.printed
    }

    pub(crate) fn print(&mut self, text: &str, ending: &str) {
        self.printed.push_str(text);
        self.printed.push_str(ending);
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

    fn read(&self, path: &str) -> Result<&[u8], String> {
        match self.nodes.get(&key(path)) {
            Some(Node::File(data)) => Ok(data),
            Some(Node::Folder) => Err(format!("{path} is a folder")),
            None if key(path).is_empty() => Err(format!("{path} is a folder")),
            None => Err(format!("{path} doesn't exist")),
        }
    }

    /// `fs._sqlite_open(path)` in a test: the test's database in memory for `path`. Its file is
    /// there to see, empty, as a real one would be, and a folder that isn't there fails it.
    pub(crate) fn open_database(&mut self, path: &str) -> Value {
        let key = key(path);
        if !matches!(self.nodes.get(&key), Some(Node::File(_)))
            && let Err(reason) = self.write(path, &[])
        {
            return crate::stdlib::fs::io_error(&reason);
        }
        crate::stdlib::sqlite::open_fake(&mut self.databases, &key)
    }

    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        let key = key(path);
        if self.is_folder(&key) {
            return Err(format!("{path} is a folder"));
        }
        if !self.is_folder(parent(&key)) {
            return Err(format!("the folder of {path} doesn't exist"));
        }
        self.nodes.insert(key, Node::File(data.to_vec()));
        Ok(())
    }

    /// Makes a folder and the folders above it, which can already be there.
    fn make_dir(&mut self, path: &str) -> Result<(), String> {
        let key = key(path);
        let ends = key.match_indices('/').map(|(at, _)| at).chain([key.len()]);
        for end in ends {
            let folder = key.get(..end).unwrap_or_default();
            if matches!(self.nodes.get(folder), Some(Node::File(_))) {
                return Err(format!("{folder} is a file"));
            }
            if !folder.is_empty() {
                self.nodes.insert(folder.to_owned(), Node::Folder);
            }
        }
        Ok(())
    }

    fn remove(&mut self, path: &str) -> Result<(), String> {
        let key = key(path);
        if key.is_empty() {
            return Err(format!("{path} can't be removed"));
        }
        if self.children(&key).next().is_some() {
            return Err(format!("{path} isn't empty"));
        }
        match self.nodes.remove(&key) {
            Some(_) => Ok(()),
            None => Err(format!("{path} doesn't exist")),
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
    fn walk_into(&self, folder: &str, found: &mut Vec<String>) -> Result<(), String> {
        for name in self.list(folder)? {
            let path = crate::stdlib::fs::joined(folder, &name);
            found.push(path.clone());
            if self.is_folder(&key(&path)) {
                self.walk_into(&path, found)?;
            }
        }
        Ok(())
    }

    fn list(&self, path: &str) -> Result<Vec<String>, String> {
        let key = key(path);
        if self.is_folder(&key) {
            return Ok(self.children(&key).collect());
        }
        match self.nodes.get(&key) {
            Some(_) => Err(format!("{path} is a file")),
            None => Err(format!("{path} doesn't exist")),
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
            return Some(Ok(Value::str(self.printed())));
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
                return Some(Ok(Value::Bool(key.is_empty() || world.nodes.contains_key(&key))));
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
                    Err(reason) => Ok(crate::stdlib::fs::io_error(&reason)),
                });
            }
            ("walk", [Value::Str(path)]) => {
                let mut found = Vec::new();
                return Some(match world.walk_into(path, &mut found) {
                    Ok(()) => Ok(crate::stdlib::fs::paths_of(&found)),
                    Err(reason) => Ok(crate::stdlib::fs::io_error(&reason)),
                });
            }
            ("metadata", [Value::Str(path)]) => {
                return Some(Ok(match metadata(world, path) {
                    Ok(fields) => Value::Ok(Rc::new(Value::made("Metadata", fields))),
                    Err(reason) => crate::stdlib::fs::io_error(&reason),
                }));
            }
            _ => return None,
        };
        Some(match done {
            Ok(value) => Ok(Value::Ok(Rc::new(value))),
            Err(reason) => Ok(crate::stdlib::fs::io_error(&reason)),
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
                crate::stdlib::process::run_of(run, args, span)
                    .map(|(program, arguments, _)| self.run(&program, arguments))
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

fn metadata(world: &TestWorld, path: &str) -> Result<Vec<(&'static str, Value)>, String> {
    let key = key(path);
    let size = match world.nodes.get(&key) {
        Some(Node::File(data)) => Some(i64::try_from(data.len()).unwrap_or(i64::MAX)),
        Some(Node::Folder) => None,
        None if key.is_empty() => None,
        None => return Err(format!("{path} doesn't exist")),
    };
    Ok(vec![
        ("size", Value::Int(size.unwrap_or(0))),
        ("is_file", Value::Bool(size.is_some())),
        ("is_dir", Value::Bool(size.is_none())),
    ])
}
