//! What a test's result depends on, from the program functions it actually ran. Tests are
//! deterministic, so if nothing a test ran has changed, it takes the same path again. Changing
//! a function on a branch it never takes can't affect it, and changing one so that it takes
//! that branch changes a function it ran.
//!
//! A test's key covers the test and the bodies of the functions it ran, and the types,
//! constants and aliases those use, with everything they use in turn. The functions they name
//! but didn't run count only by name, so one added or removed still changes the key, as it may
//! change what a name means or which method a call finds.
//!
//! A test nothing traced, like a property test, depends on all it can reach (see `reach`).

use std::collections::{BTreeSet, HashMap};

use fennec_blake3::{Hash, Hasher};
use fennec_syntax::Module;

use crate::Cache;
use crate::cache::VERSION;
use crate::reach::{Definition, Graph};

/// Adds what every test's result depends on outside the program: which build of Fennec ran it,
/// and the platform, since results can differ between platforms.
pub(crate) fn platform(hasher: &mut Hasher, build: &str) {
    for part in [VERSION, build, std::env::consts::OS, std::env::consts::ARCH] {
        hasher.update(part.as_bytes());
        hasher.update(&[0]);
    }
}

/// The definitions of a program, for working out what its tests' results depend on.
pub struct TestGraph {
    graph: Graph,
    by_key: HashMap<String, usize>,
}

impl TestGraph {
    /// `build` says which build of Fennec the keys are for.
    pub fn of(module: &Module, build: &str) -> Self {
        let graph = Graph::of(module, build);
        let by_key = graph
            .definitions
            .iter()
            .enumerate()
            .map(|(at, definition)| (definition.key.clone(), at))
            .collect();
        TestGraph { graph, by_key }
    }

    /// The program's tests, by name, in the order they're written.
    pub fn tests(&self) -> impl Iterator<Item = &str> {
        self.graph.definitions.iter().filter_map(|definition| definition.test.as_deref())
    }

    /// The key of a test's result, given the functions it ran, by the keys the compiler knows
    /// them by, or None for a test nothing traced. None if there's no such test.
    pub fn key(&self, test: &str, ran: Option<&[String]>) -> Option<Hash> {
        let at =
            self.graph.definitions.iter().position(|definition| definition.test.as_deref() == Some(test))?;
        Some(match ran {
            Some(ran) => self.traced_key(at, ran),
            None => self.graph.key(at),
        })
    }

    /// Every function a test can reach, by the keys the compiler knows them by.
    pub fn reach(&self, test: &str) -> Vec<String> {
        self.graph
            .definitions
            .iter()
            .position(|definition| definition.test.as_deref() == Some(test))
            .map_or_else(Vec::new, |at| self.graph.functions_reached(at))
    }

    fn traced_key(&self, test: usize, ran: &[String]) -> Hash {
        let mut reached = BTreeSet::from([test]);
        // A function it ran that's gone has no definition, and that changes the key too.
        let mut named: BTreeSet<&str> = BTreeSet::new();
        let mut gone = Vec::new();
        for function in ran {
            match self.by_key.get(&format!("def {function}")) {
                Some(at) => {
                    reached.insert(*at);
                }
                None => gone.push(function.as_str()),
            }
        }
        let mut pending: Vec<usize> = reached.iter().copied().collect();
        while let Some(at) = pending.pop() {
            let Some(definition) = self.graph.definitions.get(at) else { continue };
            let uses = definition.uses.names.iter().filter_map(|name| self.graph.names.get(name));
            let members = definition.uses.members.iter().filter_map(|name| self.graph.members.get(name));
            for next in uses.chain(members).flatten() {
                let Some(target) = self.graph.definitions.get(*next) else { continue };
                if runs(definition) && runs(target) {
                    named.insert(&target.key);
                } else if reached.insert(*next) {
                    pending.push(*next);
                }
            }
            let implicit = self.graph.implicit.get(&at).into_iter().flatten();
            named.extend(implicit.filter_map(|at| Some(self.graph.definitions.get(*at)?.key.as_str())));
        }
        let mut hasher = Hasher::new_derive_key("fennec build cache 2026-10-31 traced test key");
        platform(&mut hasher, &self.graph.build);
        for definition in reached.iter().filter_map(|at| self.graph.definitions.get(*at)) {
            hasher.update(definition.key.as_bytes());
            hasher.update(&[0]);
            hasher.update(definition.hash.as_bytes());
        }
        for part in std::iter::once("named").chain(named).chain(std::iter::once("gone")).chain(gone) {
            hasher.update(part.as_bytes());
            hasher.update(&[0]);
        }
        hasher.finalize()
    }
}

/// Whether a definition is code that a trace sees run, rather than something code relies on.
fn runs(definition: &Definition) -> bool {
    definition.key.starts_with("def ") || definition.test.is_some()
}

/// A test that passed: its name, the key of what its result depends on, and the functions it
/// ran, if that was traced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestRecord {
    pub test: String,
    /// The module of its kit it's in.
    pub module: String,
    pub key: Hash,
    pub ran: Option<Vec<String>>,
    /// All the functions it can reach, for a test nothing traced.
    pub reach: Vec<String>,
}

impl TestRecord {
    /// Whether changing the body of the function with this key can change its result.
    pub fn depends_on(&self, function: &str) -> bool {
        let among = |functions: &[String]| functions.iter().any(|name| name == function);
        self.ran.as_deref().map_or_else(|| among(&self.reach), among)
    }
}

/// What separates records, and the fields of one. Neither can be in a name in a source file
/// that's run, and a record that doesn't read back only means running its test again.
const RECORD: char = '\u{1}';
const FIELD: char = '\u{0}';

fn encode(records: &[TestRecord]) -> String {
    let mut text = String::new();
    for record in records {
        text.push_str(&record.key.to_hex());
        text.push(FIELD);
        text.push_str(&record.test);
        text.push(FIELD);
        text.push_str(&record.module);
        text.push(FIELD);
        text.push_str(if record.ran.is_some() { "traced" } else { "reached" });
        for function in record.ran.as_ref().unwrap_or(&record.reach) {
            text.push(FIELD);
            text.push_str(function);
        }
        text.push(RECORD);
    }
    text
}

fn decode(text: &str) -> HashMap<String, TestRecord> {
    let mut records = HashMap::new();
    for record in text.split(RECORD) {
        let mut fields = record.split(FIELD);
        let (Some(key), Some(name), Some(module), Some(kind)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Some(key) = from_hex(key) else { continue };
        let functions: Vec<String> = fields.map(str::to_owned).collect();
        let (ran, reach) = if kind == "traced" { (Some(functions), Vec::new()) } else { (None, functions) };
        records.insert(
            name.to_owned(),
            TestRecord { test: name.to_owned(), module: module.to_owned(), key, ran, reach },
        );
    }
    records
}

pub(crate) fn from_hex(text: &str) -> Option<Hash> {
    let mut bytes = [0_u8; 32];
    if text.len() != bytes.len() * 2 {
        return None;
    }
    for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(Hash(bytes))
}

impl Cache {
    /// The tests of `program` that passed last time, by name, marking them as used.
    pub fn test_records(&self, program: &str) -> HashMap<String, TestRecord> {
        let bytes = self.read("tests", &results_name(program)).unwrap_or_default();
        std::str::from_utf8(&bytes).map_or_else(|_| HashMap::new(), decode)
    }

    /// Stores the tests of `program` that pass, replacing the last ones.
    pub fn store_test_records(&self, program: &str, records: &[TestRecord]) -> std::io::Result<()> {
        self.write("tests", &results_name(program), encode(records).as_bytes())
    }
}

/// The name of a program's test results: a hash of what names the program, like its path.
fn results_name(program: &str) -> String {
    format!("{}.bin", fennec_blake3::hash(program.as_bytes()).to_hex())
}

#[cfg(test)]
mod tests {
    use super::{TestGraph, TestRecord, decode, encode, from_hex};

    fn key(source: &str, ran: &[&str]) -> fennec_blake3::Hash {
        let module = fennec_syntax::parse(source).unwrap();
        let ran: Vec<String> = ran.iter().map(|name| (*name).to_owned()).collect();
        TestGraph::of(&module, "a build").key("t", Some(&ran)).unwrap()
    }

    #[test]
    fn another_build_of_fennec_changes_every_key() {
        let module = fennec_syntax::parse(SOURCE).unwrap();
        let keys = |build| {
            let graph = TestGraph::of(&module, build);
            [graph.key("t", Some(&[])), graph.key("t", None)]
        };
        let (one, other) = (keys("one"), keys("other"));
        assert_ne!(one[0], other[0]);
        assert_ne!(one[1], other[1]);
    }

    const SOURCE: &str = "LIMIT: int = 1\n\nstruct Point:\n    x: int\n\n\
        def ran(p: Point) -> int:\n    if p.x > LIMIT:\n        return other()\n    p.x\n\n\
        def other() -> int:\n    1\n\ntest \"t\":\n    assert ran(Point(x=1)) == 1\n";

    #[test]
    fn a_function_it_didnt_run_changes_nothing_unless_its_name_comes_or_goes() {
        let before = key(SOURCE, &["ran"]);
        assert_eq!(key(&SOURCE.replace("    1\n\ntest", "    2\n\ntest"), &["ran"]), before);
        assert_ne!(key(&SOURCE.replace("def other", "def another"), &["ran"]), before);
    }

    #[test]
    fn what_it_ran_and_what_that_relies_on_change_it() {
        let before = key(SOURCE, &["ran"]);
        assert_ne!(key(&SOURCE.replace("    p.x\n", "    p.x + 0\n"), &["ran"]), before, "the function");
        assert_ne!(key(&SOURCE.replace("x: int", "x: int = 1"), &["ran"]), before, "a type it uses");
        assert_ne!(key(&SOURCE.replace("LIMIT: int = 1", "LIMIT: int = 2"), &["ran"]), before, "a constant");
        assert_ne!(key(SOURCE, &["ran", "other"]), before, "taking another path");
        assert_ne!(key(SOURCE, &["ran", "deleted"]), before, "a function that's gone");
    }

    #[test]
    fn records_read_back_and_bad_ones_are_dropped() {
        let records = [
            TestRecord {
                test: "a".to_owned(),
                module: "main".to_owned(),
                key: fennec_blake3::hash(b"a"),
                ran: Some(vec!["f".to_owned(), "T.m".to_owned()]),
                reach: Vec::new(),
            },
            TestRecord {
                test: "b".to_owned(),
                module: "m.n".to_owned(),
                key: fennec_blake3::hash(b"b"),
                ran: None,
                reach: vec!["g".to_owned()],
            },
            TestRecord {
                test: "c".to_owned(),
                module: String::new(),
                key: fennec_blake3::hash(b"c"),
                ran: Some(Vec::new()),
                reach: Vec::new(),
            },
        ];
        let read = decode(&encode(&records));
        for record in &records {
            assert_eq!(read.get(&record.test), Some(record));
        }
        assert!(decode("not a record\u{1}zz\u{0}a\u{0}traced\u{1}").is_empty());
        assert_eq!(from_hex("zz"), None);
    }
}
