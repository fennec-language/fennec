//! What a test's capabilities reach: an in-memory `Fs`, a fixed `Clock`, captured `Stdout` and a
//! `Process` that runs nothing but gives back what the test scripted, fresh for each run of a test. These hold state every copy of a capability shares, which
//! Fennec code has no way to do yet, so they're written here. With `fennec test --real` only
//! the output is kept, and the rest is real.

use std::collections::BTreeMap;
use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use crate::interp::Interp;
use crate::value::Value;

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

impl Interp<'_> {
    /// A method of the fake `Fs`, or nothing if `fs` isn't fake or the arguments are wrong.
    pub(crate) fn fake_fs(
        &mut self,
        method: &str,
        paths: &[Value],
        span: Span,
    ) -> Option<Result<Value, Diagnostic>> {
        let world = self.world.as_mut().filter(|world| !world.real())?;
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
                    Ok(names) => self.paths_in(path, names, span),
                    Err(reason) => self.io_error(&std::io::Error::other(reason), span),
                });
            }
            ("metadata", [Value::Str(path)]) => {
                return Some(match metadata(world, path) {
                    Ok(fields) => {
                        self.make_value("Metadata", fields, span).map(|found| Value::Ok(Rc::new(found)))
                    }
                    Err(reason) => self.io_error(&std::io::Error::other(reason), span),
                });
            }
            _ => return None,
        };
        Some(match done {
            Ok(value) => Ok(Value::Ok(Rc::new(value))),
            Err(reason) => self.io_error(&std::io::Error::other(reason), span),
        })
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
