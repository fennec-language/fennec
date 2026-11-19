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

/// A build using the units it kept last time again: the index it found, what it's found of
/// each read there so far, and the index it keeps for next time.
pub(crate) struct Using<'u, 'r, 'w, 'p> {
    reuse: &'u mut Reuse<'r>,
    reads: &'u Reads<'w, 'p>,
    layouts: &'u Layouts,
    /// Whether every unit used again is written anyway and checked.
    pub(crate) audit: bool,
    found: Index,
    by_name: HashMap<String, usize>,
    /// Whether each read in `found` gives what it gave, once it's been looked at.
    same: Vec<Option<bool>>,
    keeping: Index,
    numbered: HashMap<(What, String), u32>,
}

impl<'u, 'r, 'w, 'p> Using<'u, 'r, 'w, 'p> {
    /// What `reuse` kept last time, if it was kept with the settings and layouts this build has.
    pub(crate) fn new(
        reuse: &'u mut Reuse<'r>,
        build: Key,
        reads: &'u Reads<'w, 'p>,
        layouts: &'u Layouts,
    ) -> Self {
        let found = reuse
            .kept
            .as_deref()
            .and_then(Index::from_bytes)
            .filter(|index| index.build == build)
            .unwrap_or_default();
        let by_name = found.units.iter().enumerate().map(|(at, unit)| (unit.function.clone(), at)).collect();
        let same = vec![None; found.reads.len()];
        let audit = reuse.audit;
        Using {
            reuse,
            reads,
            layouts,
            audit,
            found,
            by_name,
            same,
            keeping: Index { build, ..Index::default() },
            numbered: HashMap::new(),
        }
    }

    /// The kept unit of `function`, if this build can use it: its function's IR is `ir`, as it
    /// was, every read it made gives what it gave, and its object is still there.
    pub(crate) fn usable(&mut self, function: &str, ir: &Key) -> Option<Record> {
        let record = self.found.units.get(*self.by_name.get(function)?)?;
        if record.ir != *ir {
            return None;
        }
        let reads = record.reads.clone();
        let text = record.text;
        if !reads.iter().all(|read| self.gives_what_it_gave(*read)) {
            return None;
        }
        if !(self.reuse.has_object)(&fennec_blake3::Hash(text)) {
            return None;
        }
        self.found.units.get(*self.by_name.get(function)?).cloned()
    }

    fn gives_what_it_gave(&mut self, read: u32) -> bool {
        let Ok(at) = usize::try_from(read) else { return false };
        if let Some(Some(same)) = self.same.get(at) {
            return *same;
        }
        let Some((what, name, value)) = self.found.reads.get(at) else { return false };
        let same = self.reads.value_of(*what, name, self.layouts) == *value;
        if let Some(known) = self.same.get_mut(at) {
            *known = Some(same);
        }
        same
    }

    /// The reads a kept unit made.
    pub(crate) fn reads_of(&self, record: &Record) -> Vec<(What, String)> {
        let found = |read: &u32| self.found.reads.get(usize::try_from(*read).ok()?);
        record.reads.iter().filter_map(found).map(|(what, name, _)| (*what, name.clone())).collect()
    }

    /// Keeps a unit used again for next time, as it was.
    pub(crate) fn keep(&mut self, mut record: Record) {
        let reads: Vec<(What, String, Option<Key>)> = record
            .reads
            .iter()
            .filter_map(|read| self.found.reads.get(usize::try_from(*read).ok()?).cloned())
            .collect();
        record.reads =
            reads.into_iter().map(|(what, name, value)| self.number_kept(what, name, value)).collect();
        self.keeping.units.push(record);
    }

    /// Keeps a unit just written for next time, with what each read it made gives now. One that
    /// shared, or asked for, a kind with no name isn't kept, and is written every time.
    pub(crate) fn keep_written(
        &mut self,
        unit: &Unit,
        ir: Key,
        reads: &[(What, String)],
        share: Share,
        asked: Option<Vec<Asked>>,
    ) {
        let (Some(share), Some(asked)) = (share.added, asked) else { return };
        let reads = reads.iter().map(|(what, name)| self.number_now(*what, name.clone())).collect();
        self.keeping.units.push(Record {
            function: unit.function.clone(),
            ir,
            text: *unit.hash.as_bytes(),
            huge: unit.huge,
            reads,
            asked,
            share,
        });
    }

    /// A read's number in the index being kept, known to give `value`.
    fn number_kept(&mut self, what: What, name: String, value: Option<Key>) -> u32 {
        match self.numbered.get(&(what, name.clone())) {
            Some(number) => *number,
            None => self.add_read(what, name, value),
        }
    }

    /// A read's number in the index being kept, with what it gives now.
    fn number_now(&mut self, what: What, name: String) -> u32 {
        if let Some(number) = self.numbered.get(&(what, name.clone())) {
            return *number;
        }
        let value = self.reads.value_of(what, &name, self.layouts);
        self.add_read(what, name, value)
    }

    fn add_read(&mut self, what: What, name: String, value: Option<Key>) -> u32 {
        let number = u32::try_from(self.keeping.reads.len()).unwrap_or(u32::MAX);
        self.keeping.reads.push((what, name.clone(), value));
        self.numbered.insert((what, name), number);
        number
    }

    /// Hands back the index to keep: none when a unit kept something an index can't hold.
    pub(crate) fn finish(self) {
        self.reuse.keep = self.keeping.to_bytes();
    }
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
