//! A program's index of kept units (edit builds, stage 1): for each function, everything a build
//! needs to use its unit again without writing it. Read once at the start of a dev build and
//! written once at its end, as one cache entry, which ends in a hash of its bytes, so a torn or
//! changed index is no index at all and every unit is written.
//!
//! Every read a unit made is kept once, in a table of what was read and what it gave, and each
//! unit names the reads it made there: thousands of units read a small function's IR or whether
//! it can fail, and each such read is checked once a build.

use std::collections::HashMap;

use fennec_layout::{Asked, Cap, Layouts, Named, NamedWord};

use crate::emit::{Reuse, Unit};
use crate::keys::Key;
use crate::reads::{Reads, What};
use crate::share::{Added, Place, Share};

/// What a build found in an index, or keeps to write one.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Index {
    /// The settings and layouts every unit was written with (see `emit::build_key`).
    pub(crate) build: Key,
    /// Each read some unit made, and what it gave then: `None` for a read of nothing.
    pub(crate) reads: Vec<(What, String, Option<Key>)>,
    pub(crate) units: Vec<Record>,
}

/// One unit as it was kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) function: String,
    /// Its function's IR (see `keys::ir_key`).
    pub(crate) ir: Key,
    /// Its text's hash, which names its object.
    pub(crate) text: Key,
    /// Whether it's too big to optimize, so it's compiled as written.
    pub(crate) huge: bool,
    /// The reads it made, by their place in [`Index::reads`].
    pub(crate) reads: Vec<u32>,
    pub(crate) asked: Vec<Asked>,
    pub(crate) share: Vec<Added>,
}

/// What an index's bytes start with: this layout's name and version.
const MAGIC: &[u8] = b"fennec units 1\n";

impl Index {
    /// The index's bytes, or `None` if a unit added something an index has no way to keep, which
    /// only a build that traces or notes checks does, and such a build keeps no index.
    pub(crate) fn to_bytes(&self) -> Option<Vec<u8>> {
        let mut out = Out(MAGIC.to_vec());
        out.key(&self.build);
        out.count(self.reads.len());
        for (what, name, value) in &self.reads {
            out.byte(what_number(*what));
            out.text(name);
            match value {
                Some(value) => {
                    out.byte(1);
                    out.key(value);
                }
                None => out.byte(0),
            }
        }
        out.count(self.units.len());
        for unit in &self.units {
            out.text(&unit.function);
            out.key(&unit.ir);
            out.key(&unit.text);
            out.byte(u8::from(unit.huge));
            out.count(unit.reads.len());
            for read in &unit.reads {
                out.number(*read);
            }
            out.count(unit.asked.len());
            for asked in &unit.asked {
                match asked {
                    Asked::Block(kind) => {
                        out.byte(0);
                        out.named(kind);
                    }
                    Asked::DictValue(name) => {
                        out.byte(1);
                        out.text(name);
                    }
                    Asked::DictText => out.byte(2),
                }
            }
            out.count(unit.share.len());
            for added in &unit.share {
                out.added(added)?;
            }
        }
        Some(out.0)
    }

    /// An index read back from its bytes, or `None` for bytes that aren't one.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Index> {
        let mut input = In { bytes: bytes.strip_prefix(MAGIC)? };
        let build = input.key()?;
        let mut reads = Vec::new();
        for _ in 0..input.count()? {
            let what = what_of(input.byte()?)?;
            let name = input.text()?;
            let value = match input.byte()? {
                0 => None,
                1 => Some(input.key()?),
                _ => return None,
            };
            reads.push((what, name, value));
        }
        let mut units = Vec::new();
        for _ in 0..input.count()? {
            let function = input.text()?;
            let (ir, text) = (input.key()?, input.key()?);
            let huge = input.byte()? == 1;
            let read_count = input.count()?;
            let unit_reads = (0..read_count).map(|_| input.number()).collect::<Option<Vec<u32>>>()?;
            let mut asked = Vec::new();
            for _ in 0..input.count()? {
                asked.push(match input.byte()? {
                    0 => Asked::Block(input.named()?),
                    1 => Asked::DictValue(input.text()?),
                    2 => Asked::DictText,
                    _ => return None,
                });
            }
            let mut share = Vec::new();
            for _ in 0..input.count()? {
                share.push(input.added()?);
            }
            units.push(Record { function, ir, text, huge, reads: unit_reads, asked, share });
        }
        input.bytes.is_empty().then_some(Index { build, reads, units })
    }
}

/// A build using the units it kept last time again: the index it found, which it brings up to
/// date where it stands, and what it's found of each read there so far.
///
/// A unit used again keeps its record as it is, and a unit written again has its record put in
/// its place, so a one-function edit touches one record rather than copying every one. Records
/// of functions the program no longer has are let go at the end, and reads no unit makes any
/// more once they're most of the table.
pub(crate) struct Using<'u, 'r, 'w, 'p> {
    reuse: &'u mut Reuse<'r>,
    reads: &'u Reads<'w, 'p>,
    layouts: &'u Layouts,
    /// Whether every unit used again is written anyway and checked.
    pub(crate) audit: bool,
    /// The index found, brought up to date as the build goes.
    index: Index,
    by_name: HashMap<String, usize>,
    /// Whether each read in the index gives what it gave, once it's been looked at.
    same: Vec<Option<bool>>,
    /// Which records this build used again or wrote: the rest are let go.
    seen: Vec<bool>,
    /// Each read's number in the index, by a hash of what it reads, made the first time a written
    /// unit's read is numbered: no read's name is copied to make it.
    numbered: Option<HashMap<u64, Vec<u32>>>,
    /// What each unit kept should come to, as a whole new index would hold it, when this build
    /// checks the index it brings up to date (see [`compares_index`]).
    expected: Option<Vec<Expected>>,
}

/// A unit's record with each read it made as what was read and what it gave: how the index's
/// check compares the index a build brought up to date with one made afresh.
type Expected = (String, Key, Key, bool, Vec<(u8, String, Option<Key>)>, Vec<Asked>, Vec<Added>);

/// Whether this build checks the index it brings up to date against one made afresh: always with
/// `FENNEC_CHECK_UNITS=1`, and in a dev compiler on one build in eight, picked by `picked`, the
/// units' IR keys taken together. An edit changes them, so each kit's builds are checked in turn,
/// and the same program always is or isn't. Picked by the build's settings, which an edit
/// doesn't change, one kit in eight was checked on every build and the rest never.
fn compares_index(picked: u64) -> bool {
    std::env::var("FENNEC_CHECK_UNITS").as_deref() == Ok("1")
        || (cfg!(debug_assertions) && picked.is_multiple_of(8))
}

impl<'u, 'r, 'w, 'p> Using<'u, 'r, 'w, 'p> {
    /// What `reuse` kept last time, if it was kept with the settings and layouts this build has.
    pub(crate) fn new(
        reuse: &'u mut Reuse<'r>,
        (build, picked): (Key, u64),
        reads: &'u Reads<'w, 'p>,
        layouts: &'u Layouts,
    ) -> Self {
        let index = reuse
            .kept
            .as_deref()
            .and_then(Index::from_bytes)
            .filter(|index| index.build == build)
            .unwrap_or(Index { build, ..Index::default() });
        let by_name = index.units.iter().enumerate().map(|(at, unit)| (unit.function.clone(), at)).collect();
        let same = vec![None; index.reads.len()];
        let seen = vec![false; index.units.len()];
        let audit = reuse.audit;
        let expected = compares_index(picked).then(Vec::new);
        Using { reuse, reads, layouts, audit, index, by_name, same, seen, numbered: None, expected }
    }

    /// Where the kept unit of `function` is in the index, if this build can use it: its function's
    /// IR is `ir`, as it was, every read it made gives what it gave, and its object is still there.
    pub(crate) fn usable(&mut self, function: &str, ir: &Key) -> Option<usize> {
        let at = *self.by_name.get(function)?;
        let record = self.index.units.get(at)?;
        if record.ir != *ir {
            return None;
        }
        let (text, read_count) = (record.text, record.reads.len());
        for read in 0..read_count {
            let number = *self.index.units.get(at)?.reads.get(read)?;
            if !self.gives_what_it_gave(number) {
                return None;
            }
        }
        (self.reuse.has_object)(&fennec_blake3::Hash(text)).then_some(at)
    }

    /// The kept unit at `at`.
    pub(crate) fn record(&self, at: usize) -> Option<&Record> {
        self.index.units.get(at)
    }

    fn gives_what_it_gave(&mut self, read: u32) -> bool {
        let Ok(at) = usize::try_from(read) else { return false };
        if let Some(Some(same)) = self.same.get(at) {
            return *same;
        }
        let Some((what, name, value)) = self.index.reads.get(at) else { return false };
        let same = self.reads.value_of(*what, name, self.layouts) == *value;
        if let Some(known) = self.same.get_mut(at) {
            *known = Some(same);
        }
        same
    }

    /// The reads the kept unit at `at` made.
    pub(crate) fn reads_of(&self, at: usize) -> Vec<(What, String)> {
        let found = |read: &u32| self.index.reads.get(usize::try_from(*read).ok()?);
        let Some(record) = self.index.units.get(at) else { return Vec::new() };
        record.reads.iter().filter_map(found).map(|(what, name, _)| (*what, name.clone())).collect()
    }

    /// Keeps the unit at `at`, used again, for next time, as it was.
    pub(crate) fn keep(&mut self, at: usize) {
        if let Some(seen) = self.seen.get_mut(at) {
            *seen = true;
        }
        if self.expected.is_some()
            && let Some(record) = self.index.units.get(at)
        {
            let reads = record
                .reads
                .iter()
                .filter_map(|read| self.index.reads.get(usize::try_from(*read).ok()?))
                .map(|(what, name, value)| (what_number(*what), name.clone(), *value))
                .collect();
            let expected = (
                record.function.clone(),
                record.ir,
                record.text,
                record.huge,
                reads,
                record.asked.clone(),
                record.share.clone(),
            );
            if let Some(all) = self.expected.as_mut() {
                all.push(expected);
            }
        }
    }

    /// Keeps a unit just written for next time, in its function's record's place, with what each
    /// read it made gives now. One that shared, or asked for, a kind with no name isn't kept, and
    /// is written every time.
    pub(crate) fn keep_written(
        &mut self,
        unit: &Unit,
        ir: Key,
        reads: &[(What, String)],
        share: Share,
        asked: Option<Vec<Asked>>,
    ) {
        let (Some(share), Some(asked)) = (share.added, asked) else { return };
        let now: Vec<(What, String, Option<Key>)> = reads
            .iter()
            .map(|(what, name)| (*what, name.clone(), self.reads.value_of(*what, name, self.layouts)))
            .collect();
        if let Some(all) = self.expected.as_mut() {
            let reads =
                now.iter().map(|(what, name, value)| (what_number(*what), name.clone(), *value)).collect();
            all.push((
                unit.function.clone(),
                ir,
                *unit.hash.as_bytes(),
                unit.huge,
                reads,
                asked.clone(),
                share.clone(),
            ));
        }
        let numbers = now.into_iter().map(|(what, name, value)| self.number(what, name, value)).collect();
        let record = Record {
            function: unit.function.clone(),
            ir,
            text: *unit.hash.as_bytes(),
            huge: unit.huge,
            reads: numbers,
            asked,
            share,
        };
        if let Some(&at) = self.by_name.get(&unit.function) {
            if let Some(place) = self.index.units.get_mut(at) {
                *place = record;
            }
            if let Some(seen) = self.seen.get_mut(at) {
                *seen = true;
            }
        } else {
            self.by_name.insert(unit.function.clone(), self.index.units.len());
            self.index.units.push(record);
            self.seen.push(true);
        }
    }

    /// A read's number in the index, giving `value`: its own, made to give `value` if it gave
    /// something else, as every unit that read it then was written again this build; or a new one.
    fn number(&mut self, what: What, name: String, value: Option<Key>) -> u32 {
        let reads = &self.index.reads;
        let numbered = self.numbered.get_or_insert_with(|| {
            let mut numbered: HashMap<u64, Vec<u32>> = HashMap::new();
            for (at, (what, name, _)) in reads.iter().enumerate() {
                numbered
                    .entry(read_hash(*what, name))
                    .or_default()
                    .push(u32::try_from(at).unwrap_or(u32::MAX));
            }
            numbered
        });
        let hash = read_hash(what, &name);
        let found = numbered.get(&hash).and_then(|numbers| {
            numbers.iter().copied().find(|number| {
                usize::try_from(*number)
                    .ok()
                    .and_then(|at| reads.get(at))
                    .is_some_and(|(was, read, _)| *was == what && *read == name)
            })
        });
        if let Some(number) = found {
            let at = usize::try_from(number).unwrap_or(usize::MAX);
            if let Some(read) = self.index.reads.get_mut(at) {
                read.2 = value;
            }
            if let Some(same) = self.same.get_mut(at) {
                *same = Some(true);
            }
            return number;
        }
        let number = u32::try_from(self.index.reads.len()).unwrap_or(u32::MAX);
        numbered.entry(hash).or_default().push(number);
        self.index.reads.push((what, name, value));
        self.same.push(Some(true));
        number
    }

    /// Lets go of the records of functions this build neither used again nor wrote, and of the
    /// reads no unit makes any more once they're most of the table, then hands back the index to
    /// keep: none when a unit kept something an index can't hold. A build that checks it holds it
    /// to the index made afresh from what this build kept, and fails if they differ.
    pub(crate) fn finish(mut self) -> Result<(), String> {
        if self.seen.iter().any(|seen| !*seen) {
            let mut seen = self.seen.iter();
            self.index.units.retain(|_| seen.next().copied().unwrap_or(false));
        }
        let mut made = vec![false; self.index.reads.len()];
        for read in self.index.units.iter().flat_map(|unit| &unit.reads) {
            if let Some(made) = usize::try_from(*read).ok().and_then(|at| made.get_mut(at)) {
                *made = true;
            }
        }
        let unmade = made.iter().filter(|made| !**made).count();
        if unmade * 2 > made.len() {
            let mut renumbered = vec![u32::MAX; made.len()];
            let mut reads = Vec::with_capacity(made.len() - unmade);
            for (at, read) in std::mem::take(&mut self.index.reads).into_iter().enumerate() {
                if made.get(at).copied().unwrap_or(false) {
                    if let Some(number) = renumbered.get_mut(at) {
                        *number = u32::try_from(reads.len()).unwrap_or(u32::MAX);
                    }
                    reads.push(read);
                }
            }
            self.index.reads = reads;
            for read in self.index.units.iter_mut().flat_map(|unit| &mut unit.reads) {
                *read = usize::try_from(*read)
                    .ok()
                    .and_then(|at| renumbered.get(at))
                    .copied()
                    .unwrap_or(u32::MAX);
            }
        }
        if let Some(expected) = self.expected.take() {
            let index_holds = canonical(&self.index);
            if index_holds != canonical_of(expected) {
                return Err(
                    "the index of kept units brought up to date isn't what one made afresh holds, which is a bug in the backend"
                        .to_owned(),
                );
            }
        }
        self.reuse.keep = self.index.to_bytes();
        Ok(())
    }
}

/// A hash of what a read reads, to find its number by.
fn read_hash(what: What, name: &str) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut state = std::hash::DefaultHasher::new();
    what_number(what).hash(&mut state);
    name.hash(&mut state);
    state.finish()
}

/// An index's units, by function, each with its reads as what was read and what it gave, in order:
/// what the index means, however its records and reads are placed.
fn canonical(index: &Index) -> std::collections::BTreeMap<String, Expected> {
    index
        .units
        .iter()
        .map(|unit| {
            let reads = unit
                .reads
                .iter()
                .filter_map(|read| index.reads.get(usize::try_from(*read).ok()?))
                .map(|(what, name, value)| (what_number(*what), name.clone(), *value))
                .collect();
            let expected = (
                unit.function.clone(),
                unit.ir,
                unit.text,
                unit.huge,
                reads,
                unit.asked.clone(),
                unit.share.clone(),
            );
            (unit.function.clone(), expected)
        })
        .collect()
}

/// [`canonical`], of records with their reads given as what was read and what it gave.
fn canonical_of(expected: Vec<Expected>) -> std::collections::BTreeMap<String, Expected> {
    expected.into_iter().map(|record| (record.0.clone(), record)).collect()
}

/// Each kind of read's number in an index. A kind added to `What` has to be given one here, and
/// the same in [`what_of`], which the debug check below holds to.
fn what_number(what: What) -> u8 {
    let number = match what {
        What::Function => 0,
        What::Analysis => 1,
        What::CanFail => 2,
        What::Generator => 3,
        What::Settles => 4,
        What::Lengths => 5,
        What::Builders => 6,
        What::Needs => 7,
        What::UnderLocks => 8,
        What::Constant => 9,
        What::OnWorkers => 10,
        What::Task => 11,
        What::NonNegative => 12,
        What::Waiting => 13,
        What::Constants => 14,
        What::Native => 15,
        What::WaitingClosures => 16,
        What::Abandons => 17,
        What::Layout => 18,
        What::Recurs => 19,
    };
    debug_assert_eq!(what_of(number), Some(what), "`what_of` reads {what:?}'s number back as another");
    number
}

/// The kind of read an index's number names, as [`what_number`] gave it.
fn what_of(number: u8) -> Option<What> {
    Some(match number {
        0 => What::Function,
        1 => What::Analysis,
        2 => What::CanFail,
        3 => What::Generator,
        4 => What::Settles,
        5 => What::Lengths,
        6 => What::Builders,
        7 => What::Needs,
        8 => What::UnderLocks,
        9 => What::Constant,
        10 => What::OnWorkers,
        11 => What::Task,
        12 => What::NonNegative,
        13 => What::Waiting,
        14 => What::Constants,
        15 => What::Native,
        16 => What::WaitingClosures,
        17 => What::Abandons,
        18 => What::Layout,
        19 => What::Recurs,
        _ => return None,
    })
}

/// Bytes being written.
struct Out(Vec<u8>);

impl Out {
    fn byte(&mut self, byte: u8) {
        self.0.push(byte);
    }

    fn number(&mut self, number: u32) {
        self.0.extend_from_slice(&number.to_le_bytes());
    }

    fn count(&mut self, count: usize) {
        self.number(u32::try_from(count).unwrap_or(u32::MAX));
    }

    fn text(&mut self, text: &str) {
        self.count(text.len());
        self.0.extend_from_slice(text.as_bytes());
    }

    fn key(&mut self, key: &Key) {
        self.0.extend_from_slice(key);
    }

    fn place(&mut self, place: Place) {
        match place {
            Place::From(offset) => {
                self.byte(0);
                self.0.extend_from_slice(&offset.to_le_bytes());
            }
            Place::At(at) => {
                self.byte(1);
                self.number(at);
            }
        }
    }

    fn named(&mut self, named: &Named) {
        let tag = match named {
            Named::Int => 0,
            Named::Float => 1,
            Named::Bool => 2,
            Named::U64 => 3,
            Named::F32 => 4,
            Named::Unit => 5,
            Named::Cap(_) => 6,
            Named::Obj => 7,
            Named::Struct(_) => 8,
            Named::Boxed(_) => 9,
            Named::Str => 10,
            Named::Bytes => 11,
            Named::Gen => 12,
            Named::Dict(..) => 13,
            Named::List(_) => 14,
            Named::Maybe(_) => 15,
            Named::Block(_) => 16,
        };
        self.byte(tag);
        match named {
            Named::Cap(cap) => self.text(cap.name()),
            Named::Struct(name) | Named::Boxed(name) => self.text(name),
            Named::Dict(key, value) => {
                self.word(key);
                self.word(value);
            }
            Named::List(inner) | Named::Maybe(inner) | Named::Block(inner) => self.named(inner),
            _ => {}
        }
    }

    fn word(&mut self, word: &NamedWord) {
        let tag = match word {
            NamedWord::Int => 0,
            NamedWord::Float => 1,
            NamedWord::Bool => 2,
            NamedWord::Str => 3,
            NamedWord::Nothing => 4,
            NamedWord::Plain(_) => 5,
            NamedWord::Text => 6,
            NamedWord::Block(_) => 7,
            NamedWord::U64 => 8,
            NamedWord::F32 => 9,
            NamedWord::InPlace(_) => 10,
            NamedWord::TextInPlace => 11,
        };
        self.byte(tag);
        match word {
            NamedWord::Plain(name) | NamedWord::InPlace(name) => self.text(name),
            NamedWord::Block(kind) => self.named(kind),
            _ => {}
        }
    }

    fn added(&mut self, added: &Added) -> Option<()> {
        match added {
            Added::String(symbol, text) => {
                self.byte(0);
                self.text(symbol);
                self.text(text);
            }
            Added::Literal(symbol, text) => {
                self.byte(1);
                self.text(symbol);
                self.text(text);
            }
            Added::Element(symbol, item) => {
                self.byte(2);
                self.text(symbol);
                self.named(item);
            }
            Added::Shape(symbol, name) => {
                self.byte(3);
                self.text(symbol);
                self.text(name);
            }
            Added::Dict(symbol, key, value) => {
                self.byte(4);
                self.text(symbol);
                self.word(key);
                self.word(value);
            }
            Added::Start(symbol, function, start) => {
                self.byte(5);
                self.text(symbol);
                self.text(function);
                self.place(*start);
            }
            Added::Places(symbol, function, places) => {
                self.byte(6);
                self.text(symbol);
                self.text(function);
                self.count(places.len());
                for place in places {
                    self.place(*place);
                }
            }
            // Only a build that traces or notes checks adds these, and it keeps no index.
            Added::Ran(_) | Added::Check(..) | Added::Site(..) => return None,
        }
        Some(())
    }
}

/// Bytes being read.
struct In<'b> {
    bytes: &'b [u8],
}

impl In<'_> {
    fn take(&mut self, length: usize) -> Option<&[u8]> {
        let (taken, rest) = self.bytes.split_at_checked(length)?;
        self.bytes = rest;
        Some(taken)
    }

    fn byte(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    fn number(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn count(&mut self) -> Option<usize> {
        usize::try_from(self.number()?).ok()
    }

    fn text(&mut self) -> Option<String> {
        let length = self.count()?;
        String::from_utf8(self.take(length)?.to_vec()).ok()
    }

    fn key(&mut self) -> Option<Key> {
        self.take(32)?.try_into().ok()
    }

    fn place(&mut self) -> Option<Place> {
        match self.byte()? {
            0 => Some(Place::From(i64::from_le_bytes(self.take(8)?.try_into().ok()?))),
            1 => Some(Place::At(self.number()?)),
            _ => None,
        }
    }

    fn named(&mut self) -> Option<Named> {
        Some(match self.byte()? {
            0 => Named::Int,
            1 => Named::Float,
            2 => Named::Bool,
            3 => Named::U64,
            4 => Named::F32,
            5 => Named::Unit,
            6 => Named::Cap(Cap::named(&self.text()?)?),
            7 => Named::Obj,
            8 => Named::Struct(self.text()?),
            9 => Named::Boxed(self.text()?),
            10 => Named::Str,
            11 => Named::Bytes,
            12 => Named::Gen,
            13 => Named::Dict(self.word()?, self.word()?),
            14 => Named::List(Box::new(self.named()?)),
            15 => Named::Maybe(Box::new(self.named()?)),
            16 => Named::Block(Box::new(self.named()?)),
            _ => return None,
        })
    }

    fn word(&mut self) -> Option<NamedWord> {
        Some(match self.byte()? {
            0 => NamedWord::Int,
            1 => NamedWord::Float,
            2 => NamedWord::Bool,
            3 => NamedWord::Str,
            4 => NamedWord::Nothing,
            5 => NamedWord::Plain(self.text()?),
            6 => NamedWord::Text,
            7 => NamedWord::Block(Box::new(self.named()?)),
            8 => NamedWord::U64,
            9 => NamedWord::F32,
            10 => NamedWord::InPlace(self.text()?),
            11 => NamedWord::TextInPlace,
            _ => return None,
        })
    }

    fn added(&mut self) -> Option<Added> {
        Some(match self.byte()? {
            0 => Added::String(self.text()?, self.text()?),
            1 => Added::Literal(self.text()?, self.text()?),
            2 => Added::Element(self.text()?, self.named()?),
            3 => Added::Shape(self.text()?, self.text()?),
            4 => Added::Dict(self.text()?, self.word()?, self.word()?),
            5 => Added::Start(self.text()?, self.text()?, self.place()?),
            6 => {
                let (symbol, function) = (self.text()?, self.text()?);
                let count = self.count()?;
                let places = (0..count).map(|_| self.place()).collect::<Option<Vec<Place>>>()?;
                Added::Places(symbol, function, places)
            }
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An index read back from its bytes is the index written, every kind of thing kept in it
    /// included; and bytes that are cut short, or have more after, are no index.
    #[test]
    fn an_index_reads_back_as_it_was_written() -> Result<(), String> {
        let list = Named::List(Box::new(Named::Block(Box::new(Named::Struct("Spot".to_owned())))));
        let word = NamedWord::Block(Box::new(Named::Maybe(Box::new(Named::Cap(Cap::Fs)))));
        let index = Index {
            build: [7; 32],
            reads: vec![
                (What::Function, "tiny".to_owned(), Some([1; 32])),
                (What::CanFail, "gone".to_owned(), None),
                (What::WaitingClosures, String::new(), Some([2; 32])),
            ],
            units: vec![Record {
                function: "main".to_owned(),
                ir: [3; 32],
                text: [4; 32],
                huge: true,
                reads: vec![0, 2],
                asked: vec![Asked::Block(list.clone()), Asked::DictValue("Spot".to_owned())],
                share: vec![
                    Added::String("s_1".to_owned(), "text".to_owned()),
                    Added::Literal("l_1".to_owned(), "Spot".to_owned()),
                    Added::Element("e_k_1".to_owned(), list),
                    Added::Shape("h_Twig".to_owned(), "Twig".to_owned()),
                    Added::Dict("d_1".to_owned(), NamedWord::Text, word),
                    Added::Start("p_main".to_owned(), "main".to_owned(), Place::From(-3)),
                    Added::Places(
                        "p_main.sites".to_owned(),
                        "main".to_owned(),
                        vec![Place::From(4), Place::At(0)],
                    ),
                ],
            }],
        };
        let bytes = index.to_bytes().ok_or("not kept")?;
        assert_eq!(Index::from_bytes(&bytes).as_ref(), Some(&index));
        assert_eq!(Index::from_bytes(bytes.get(..bytes.len() - 1).unwrap_or_default()), None);
        assert_eq!(Index::from_bytes(&[bytes.as_slice(), &[0]].concat()), None);
        Ok(())
    }
}
