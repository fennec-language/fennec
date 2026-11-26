//! What a unit's text is made from beyond its own body. Every read writing a unit makes of what
//! the whole program's analysis worked out goes through [`Reads`], which notes it, so a build can
//! use the unit again when everything it read is as it was (edit builds, stage 1). The fields
//! behind it are private, so a read that isn't noted doesn't compile.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};

use fennec_ir::{Function, GeneratorDef};

use crate::analysis::{Analysis, Whole};
use crate::keys::{Key, Stable, hash_set, ir_key, key_of};
use crate::uniqueness::{Callees, FieldPath, Summaries, Unsettled};

/// What a read was of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum What {
    /// A function's IR.
    Function,
    Analysis,
    CanFail,
    Generator,
    Settles,
    Lengths,
    Builders,
    Needs,
    UnderLocks,
    Constant,
    OnWorkers,
    Task,
    NonNegative,
    /// Whether a function may wait (see `waits`).
    Waiting,
    /// Every function that gives a constant's value, as `main` works the kept ones out.
    Constants,
    /// The boxed types whose blocks generated code makes and frees itself.
    Native,
    /// Whether any closure the program makes may wait.
    WaitingClosures,
    /// Whether any generator the program has closes something when it's let go partway.
    Abandons,
    /// A type's layout, by its name: its size and its fields' kinds and places, read through
    /// `Layouts::get` (see `Layouts::note_reads`).
    Layout,
    /// Whether a task can call itself again (see `ticks::recurring`).
    Recurs,
}

/// One read: of what, and by which name. A name that isn't there is a read too, of nothing.
pub(crate) type Read<'p> = (What, Cow<'p, str>);

/// What the program's analysis worked out, read through a unit's notes, and what of `Shared` was
/// worked out before any unit was written: which types are native, and what may wait.
pub(crate) struct Reads<'w, 'p> {
    whole: &'w Whole<'p>,
    native: &'w BTreeSet<u32>,
    waiting: &'w BTreeSet<String>,
    waiting_closures: bool,
    /// Whether reads are noted: only when the build keeps what each unit read.
    noting: bool,
    noted: RefCell<BTreeSet<Read<'p>>>,
}

impl<'w, 'p> Reads<'w, 'p> {
    pub(crate) fn new(
        whole: &'w Whole<'p>,
        native: &'w BTreeSet<u32>,
        (waiting, waiting_closures): (&'w BTreeSet<String>, bool),
        noting: bool,
    ) -> Self {
        Reads { whole, native, waiting, waiting_closures, noting, noted: RefCell::new(BTreeSet::new()) }
    }

    /// Whether reads are noted, and units keep what they share.
    pub(crate) fn noting(&self) -> bool {
        self.noting
    }

    /// Everything read so far, and starts again with nothing.
    pub(crate) fn take(&self) -> BTreeSet<Read<'p>> {
        std::mem::take(&mut *self.noted.borrow_mut())
    }

    fn note(&self, what: What, name: Cow<'p, str>) {
        if self.noting {
            self.noted.borrow_mut().insert((what, name));
        }
    }

    /// Notes a read of `name` in a table the program's names key: by the key's own text when
    /// it's there, and by a copy of `name` when it isn't.
    fn note_found(&self, what: What, key: Option<&'p str>, name: &str) {
        if self.noting {
            self.note(what, key.map_or_else(|| Cow::Owned(name.to_owned()), Cow::Borrowed));
        }
    }

    /// Notes whether `name` is in `set`.
    fn note_member(&self, what: What, set: &HashSet<&'p str>, name: &str) -> bool {
        let key = set.get(name).copied();
        self.note_found(what, key, name);
        key.is_some()
    }

    /// A function's IR, by its name.
    pub(crate) fn function(&self, name: &str) -> Option<&'p Function> {
        let found = self.whole.functions.get_key_value(name);
        self.note_found(What::Function, found.map(|(key, _)| *key), name);
        found.map(|(_, function)| *function)
    }

    pub(crate) fn analysis(&self, name: &str) -> Option<&'w Analysis> {
        entry(self, What::Analysis, &self.whole.analyses, name).map(std::sync::Arc::as_ref)
    }

    pub(crate) fn can_fail(&self, name: &str) -> bool {
        self.note_member(What::CanFail, &self.whole.can_fail, name)
    }

    /// A generator, by its name or the name of one of its functions.
    pub(crate) fn generator(&self, name: &str) -> Option<&'p GeneratorDef> {
        entry(self, What::Generator, &self.whole.generators, name).copied()
    }

    pub(crate) fn builders(&self, name: &str) -> Option<&'w HashSet<usize>> {
        entry(self, What::Builders, &self.whole.builders, name)
    }

    pub(crate) fn needs(&self, name: &str) -> Option<&'w Vec<FieldPath>> {
        entry(self, What::Needs, &self.whole.needs, name)
    }

    pub(crate) fn under_locks(&self, name: &str) -> bool {
        self.note_member(What::UnderLocks, &self.whole.under_locks, name)
    }

    pub(crate) fn constant(&self, name: &str) -> bool {
        self.note_member(What::Constant, &self.whole.constants, name)
    }

    /// Every function that gives a constant's value, in their names' order.
    pub(crate) fn constants(&self) -> Vec<&'p str> {
        self.note(What::Constants, Cow::Borrowed(""));
        let mut constants: Vec<&'p str> = self.whole.constants.iter().copied().collect();
        constants.sort_unstable();
        constants
    }

    pub(crate) fn on_workers(&self, name: &str) -> bool {
        self.note_member(What::OnWorkers, &self.whole.on_workers, name)
    }

    pub(crate) fn task(&self, name: &str) -> bool {
        self.note_member(What::Task, &self.whole.tasks, name)
    }

    /// Whether a task can call itself again, and so counts its calls (see `ticks::recurring`).
    pub(crate) fn recurs(&self, name: &str) -> bool {
        self.note_member(What::Recurs, &self.whole.recurring, name)
    }

    /// The places in a function proven not negative (see `signs`).
    pub(crate) fn non_negative(&self, name: &str) -> Option<&'w HashSet<(usize, usize)>> {
        if self.noting {
            self.note(What::NonNegative, Cow::Owned(name.to_owned()));
        }
        self.whole.non_negative.get(name)
    }

    /// Whether a function may wait.
    pub(crate) fn waiting(&self, name: &str) -> bool {
        if self.noting {
            self.note(What::Waiting, Cow::Owned(name.to_owned()));
        }
        self.waiting.contains(name)
    }

    pub(crate) fn waiting_closures(&self) -> bool {
        self.note(What::WaitingClosures, Cow::Borrowed(""));
        self.waiting_closures
    }

    /// Whether any generator the program has closes something when it's let go partway: without
    /// one, letting go of a value never runs program code, so every free is plain.
    pub(crate) fn abandons(&self) -> bool {
        self.note(What::Abandons, Cow::Borrowed(""));
        self.whole.abandons
    }

    /// The layouts of the types whose blocks generated code makes and frees itself, in their
    /// numbers' order, which is how a unit walks them.
    pub(crate) fn native(&self) -> &'w BTreeSet<u32> {
        self.note(What::Native, Cow::Borrowed(""));
        self.native
    }

    /// What each function may give another owner, or replace, under each parameter, read
    /// through these notes.
    pub(crate) fn settles(&'w self) -> Noted<'w, 'p> {
        Noted { reads: self, what: What::Settles, summaries: &self.whole.settles }
    }

    /// What each function may shorten, or replace, under each parameter, read through these
    /// notes.
    pub(crate) fn lengths(&'w self) -> Noted<'w, 'p> {
        Noted { reads: self, what: What::Lengths, summaries: &self.whole.lengths }
    }

    /// Whether the program has any `shared` type. It's part of every unit's settings.
    pub(crate) fn cells(&self) -> bool {
        self.whole.cells
    }

    /// Whether the program's closures are blocks in the runtime's table. It's part of every
    /// unit's settings.
    pub(crate) fn runtime_closures(&self) -> bool {
        self.whole.runtime_closures
    }

    /// Whether the program has tests built into it. It's part of every unit's settings.
    pub(crate) fn tested(&self) -> bool {
        self.whole.tested
    }
}

impl Reads<'_, '_> {
    /// What a read gives now, hashed, without noting it: `None` for a read of nothing, as a
    /// name that isn't there, or that isn't in a set, is. A build uses a kept unit again only
    /// if every read it made gives what it gave then.
    pub(crate) fn value_of(&self, what: What, name: &str, layouts: &fennec_layout::Layouts) -> Option<Key> {
        let whole = self.whole;
        let member = |set: &HashSet<&str>| set.contains(name).then(|| key_of("member", &true));
        let keyed = |value: &dyn Fn(&mut Stable)| {
            let mut state = Stable::new("fennec unit read 2026-11-17");
            value(&mut state);
            state.key()
        };
        match what {
            What::Function => whole
                .ir_keys
                .get(name)
                .copied()
                .or_else(|| whole.functions.get(name).map(|function| ir_key(function))),
            What::Analysis => {
                whole.analysis_keys.get(name).copied().or_else(|| {
                    whole.analyses.get(name).map(|analysis| keyed(&|state| analysis.stable(state)))
                })
            }
            What::CanFail => member(&whole.can_fail),
            What::Generator => whole.generators.get(name).map(|def| key_of("generator", def)),
            What::Settles => whole.settles.get(name).map(|summary| key_of("settles", summary)),
            What::Lengths => whole.lengths.get(name).map(|summary| key_of("lengths", summary)),
            What::Builders => whole.builders.get(name).map(|locals| keyed(&|state| hash_set(locals, state))),
            What::Needs => whole.needs.get(name).map(|paths| key_of("needs", paths)),
            What::UnderLocks => member(&whole.under_locks),
            What::Constant => member(&whole.constants),
            What::OnWorkers => member(&whole.on_workers),
            What::Task => member(&whole.tasks),
            What::Recurs => member(&whole.recurring),
            What::NonNegative => {
                whole.non_negative.get(name).map(|places| keyed(&|state| hash_set(places, state)))
            }
            What::Waiting => self.waiting.contains(name).then(|| key_of("member", &true)),
            What::Constants => {
                let mut constants: Vec<&str> = whole.constants.iter().copied().collect();
                constants.sort_unstable();
                Some(key_of("constants", &constants))
            }
            What::Native => {
                // In the numbers' order, as a unit walks them, by the types' names.
                let names: Vec<Option<&str>> = self
                    .native
                    .iter()
                    .map(|id| layouts.get(*id).map(|layout| layout.name.as_str()))
                    .collect();
                Some(key_of("native", &names))
            }
            What::WaitingClosures => Some(key_of("waiting closures", &self.waiting_closures)),
            What::Abandons => Some(key_of("abandons", &whole.abandons)),
            What::Layout => layouts.layout_fingerprint(name).map(|text| key_of("layout", &text)),
        }
    }
}

/// Looks `name` up in `table`, noting the read: by the key's own text when it's there, and by a
/// copy of `name` when it isn't.
fn entry<'w, 'p, V>(
    reads: &Reads<'w, 'p>,
    what: What,
    table: &'w HashMap<&'p str, V>,
    name: &str,
) -> Option<&'w V> {
    let found = table.get_key_value(name);
    reads.note_found(what, found.map(|(key, _)| *key), name);
    found.map(|(_, value)| value)
}

/// A table of summaries, each read of it noted.
pub(crate) struct Noted<'w, 'p> {
    reads: &'w Reads<'w, 'p>,
    what: What,
    summaries: &'w Summaries<'p>,
}

impl Callees for Noted<'_, '_> {
    fn summary(&self, name: &str) -> Option<&[Vec<Unsettled>]> {
        entry(self.reads, self.what, self.summaries, name).map(Vec::as_slice)
    }
}
