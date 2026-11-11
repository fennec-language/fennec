//! Laying out a program's objects and linking them: where each section goes, what each symbol is,
//! and every relocation applied, into bytes for two regions of memory, one of code and one of
//! data. The sizes come first, so whoever maps the regions knows how big to make them, then the
//! link at the addresses they got.

use std::collections::{BTreeMap, BTreeSet};

use crate::macho::{Kind, Object, Relocation, read};
use crate::{Checked, Refused, apply, refuse};

const UNSIGNED: u8 = 0;
const SUBTRACTOR: u8 = 1;
const BRANCH26: u8 = 2;
const PAGE21: u8 = 3;
const PAGEOFF12: u8 = 4;
const GOT_LOAD_PAGE21: u8 = 5;
const GOT_LOAD_PAGEOFF12: u8 = 6;
const ADDEND: u8 = 10;

/// The most code a program may have, stubs included, so every call inside it reaches.
const MOST_CODE: u64 = 1 << 26;

/// The most data a program may have: 1 GB.
const MOST_DATA: u64 = 1 << 30;

/// The size of one entry of `__compact_unwind`.
const COMPACT_UNWIND_ENTRY: usize = 32;

/// Where one section goes: which region and how far into it.
#[derive(Clone, Copy, Debug)]
enum Place {
    Code(u64),
    Data(u64),
    /// Read for its unwind entries, never mapped.
    Unwind,
}

/// A program's objects, read and laid out, waiting for the addresses of its two regions.
#[derive(Debug)]
pub struct Plan {
    objects: Vec<Object>,
    /// Where each section of each object goes.
    places: Vec<Vec<Place>>,
    /// How many bytes of each region the sections take, before the stubs.
    code_bytes: u64,
    data_bytes: u64,
    /// The undefined symbols a branch goes to, each of which gets a stub in case it's far.
    stubs: BTreeSet<String>,
    /// The symbols loaded through the global offset table, each of which gets a slot holding its
    /// address, after the data.
    got: BTreeSet<String>,
}

/// A program linked for its two regions, as bytes to put there.
#[derive(Debug, PartialEq, Eq)]
pub struct Image {
    pub code: Vec<u8>,
    pub data: Vec<u8>,
    /// The address of `main`.
    pub entry: u64,
    /// How each function unwinds, from `__compact_unwind`, at its address.
    pub unwind: Vec<Unwind>,
    /// The `__eh_frame` sections, linked, at the address each starts at, in the data region.
    pub eh_frames: Vec<u64>,
}

/// A section, as `Plan::sections` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionInfo {
    pub object: usize,
    pub section: usize,
    pub name: String,
    /// Its address in the object's own address space, which a linker's hints count from.
    pub start: u64,
    /// The alignment it asks for, as a power of two.
    pub align: u32,
    pub size: u64,
    /// The symbols defined in it, at how far into it each is, and whether each is a weak
    /// definition, which other objects may have too.
    pub symbols: Vec<(String, u64, bool)>,
}

/// Where another linker placed a program's pieces, for `Plan::link_placed`.
pub struct Placement<'p> {
    /// Each section, by object and section number from 0.
    pub section: &'p dyn Fn(usize, usize) -> Option<u64>,
    /// A symbol that went somewhere apart from its section, as one the other linker merged with
    /// another object's, by object, section and how far into it the symbol is.
    pub symbol: &'p dyn Fn(usize, usize, u64) -> Option<u64>,
    /// The offset table's slot for each symbol loaded through it.
    pub slot: &'p dyn Fn(&str) -> Option<u64>,
    /// What the objects don't define, and the copy of each weak symbol that was kept.
    pub runtime: &'p dyn Fn(&str) -> Option<u64>,
}

/// What `Plan::link_placed` gives: each section placed, linked or why not, and the unwind
/// entries of the functions placed.
#[derive(Debug)]
pub struct PlacedLink {
    pub sections: Vec<Result<Linked, Refused>>,
    pub unwind: Vec<Unwind>,
}

/// A section linked at the address it was given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Linked {
    pub object: usize,
    pub section: usize,
    pub address: u64,
    pub bytes: Vec<u8>,
}

/// One function's compact unwind entry, linked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unwind {
    pub function: u64,
    pub length: u32,
    pub encoding: u32,
}

/// The undefined symbols a branch goes to, which get a stub each in case they're far, and the
/// symbols loaded through the offset table, which get a slot each.
fn stubs_and_slots(
    objects: &[Object],
    defined: &BTreeMap<&str, bool>,
) -> Result<(BTreeSet<String>, BTreeSet<String>), Refused> {
    let (mut stubs, mut got) = (BTreeSet::new(), BTreeSet::new());
    for object in objects {
        for section in &object.sections {
            for relocation in section
                .relocations
                .iter()
                .filter(|it| matches!(it.kind, GOT_LOAD_PAGE21 | GOT_LOAD_PAGEOFF12))
            {
                let symbol = object.symbols.get(usize::try_from(relocation.symbol).unwrap_or(usize::MAX));
                match symbol.filter(|_| relocation.external) {
                    Some(symbol) => got.insert(symbol.name.clone()),
                    None => {
                        return refuse(format!(
                            "a load through the offset table in {} names no symbol",
                            section.name
                        ));
                    }
                };
            }
            for relocation in section.relocations.iter().filter(|it| it.kind == BRANCH26 && it.external) {
                let symbol = object.symbols.get(usize::try_from(relocation.symbol).unwrap_or(usize::MAX));
                if let Some(symbol) =
                    symbol.filter(|it| it.defined.is_none() && !defined.contains_key(it.name.as_str()))
                {
                    stubs.insert(symbol.name.clone());
                }
            }
        }
    }
    Ok((stubs, got))
}

/// How far into its section a symbol at `value` is, if it's in it.
fn into_section(section: &crate::macho::Section, value: u64, name: &str) -> Result<u64, Refused> {
    value
        .checked_sub(section.addr)
        .filter(|offset| *offset <= section.size)
        .ok_or_else(|| Refused(format!("{name} is outside its section")))
}

fn aligned(at: u64, align: u32) -> Result<u64, Refused> {
    let unit = 1_u64 << align;
    at.checked_add(unit - 1)
        .map(|end| end & !(unit - 1))
        .map_or_else(|| refuse("a section's place overflows"), Ok)
}

impl Plan {
    /// Reads and lays out `objects`.
    pub fn new(objects: &[Checked]) -> Result<Plan, Refused> {
        let objects: Vec<Object> =
            objects.iter().map(|object| read(object.bytes())).collect::<Result<_, _>>()?;
        let (mut code_bytes, mut data_bytes) = (0_u64, 0_u64);
        let mut places = Vec::with_capacity(objects.len());
        // Each name defined, and whether its every definition is weak.
        let mut defined: BTreeMap<&str, bool> = BTreeMap::new();
        for object in &objects {
            let mut here = Vec::with_capacity(object.sections.len());
            for section in &object.sections {
                here.push(match section.kind {
                    Kind::Code => {
                        let at = aligned(code_bytes, section.align)?;
                        code_bytes = at
                            .checked_add(section.size)
                            .ok_or_else(|| Refused("code overflows".to_owned()))?;
                        Place::Code(at)
                    }
                    Kind::ReadOnly | Kind::ReadWrite { .. } | Kind::EhFrame => {
                        let at = aligned(data_bytes, section.align)?;
                        data_bytes = at
                            .checked_add(section.size)
                            .ok_or_else(|| Refused("data overflows".to_owned()))?;
                        Place::Data(at)
                    }
                    Kind::CompactUnwind => Place::Unwind,
                });
            }
            for symbol in object.symbols.iter().filter(|symbol| symbol.external) {
                if let Some((section, value)) = symbol.defined {
                    let within = object
                        .sections
                        .get(section)
                        .is_some_and(|it| value >= it.addr && value <= it.addr + it.size);
                    if !within {
                        return refuse(format!("{} is outside its section", symbol.name));
                    }
                    let all_weak = defined.get(symbol.name.as_str()).is_none_or(|weak| *weak) && symbol.weak;
                    if defined.insert(&symbol.name, all_weak).is_some() && !all_weak {
                        return refuse(format!("{} is defined twice", symbol.name));
                    }
                }
            }
            places.push(here);
        }
        let (stubs, got) = stubs_and_slots(&objects, &defined)?;
        let plan = Plan { objects, places, code_bytes, data_bytes, stubs, got };
        if plan.code_size()? > MOST_CODE || plan.data_size() > MOST_DATA {
            return refuse(format!(
                "a program of {} bytes of code and {} of data is too big",
                plan.code_size()?,
                plan.data_size()
            ));
        }
        Ok(plan)
    }

    fn stubs_at(&self) -> Result<u64, Refused> {
        aligned(self.code_bytes, 4)
    }

    /// How many bytes the code region needs, stubs included.
    pub fn code_size(&self) -> Result<u64, Refused> {
        let stubs = u64::try_from(self.stubs.len()).map_err(|_| Refused("too many stubs".to_owned()))?;
        self.stubs_at()?.checked_add(stubs * 16).map_or_else(|| refuse("code overflows"), Ok)
    }

    fn got_at(&self) -> u64 {
        aligned(self.data_bytes, 3).unwrap_or(u64::MAX)
    }

    /// How many bytes the data region needs, the offset table's slots included.
    pub fn data_size(&self) -> u64 {
        let slots = u64::try_from(self.got.len()).unwrap_or(u64::MAX);
        self.got_at().saturating_add(slots.saturating_mul(8))
    }

    /// Links the program for code at `code_base` and data at `data_base`, with what the objects
    /// don't define found by `runtime`, by its name in an object, as `_fennec_fail`.
    pub fn link(
        &self,
        code_base: u64,
        data_base: u64,
        runtime: &dyn Fn(&str) -> Option<u64>,
    ) -> Result<Image, Refused> {
        let bases = Bases { code: code_base, data: data_base };
        let address = |object: usize, section: usize| {
            self.places.get(object).and_then(|places| places.get(section)).and_then(|place| bases.of(*place))
        };
        let mut code = vec![0_u8; size(self.code_size()?)?];
        let mut data = vec![0_u8; size(self.data_size())?];
        let globals = self.globals(&address)?;
        let resolve = |name: &str| globals.get(name).copied().or_else(|| runtime(name));
        let stubs = self.write_stubs(&mut code, code_base, &resolve)?;
        let mut slots = BTreeMap::new();
        for (n, name) in self.got.iter().enumerate() {
            let at = self
                .got_at()
                .checked_add(8 * u64::try_from(n).unwrap_or(u64::MAX))
                .ok_or_else(|| Refused("the offset table overflows".to_owned()))?;
            let target = resolve(name).ok_or_else(|| Refused(format!("nothing defines {name}")))?;
            put(&mut data, at, &target.to_le_bytes(), "the offset table")?;
            slots.insert(
                name.as_str(),
                data_base.checked_add(at).ok_or_else(|| Refused("the offset table overflows".to_owned()))?,
            );
        }
        let slot = |name: &str| slots.get(name).copied();
        let (mut unwind, mut eh_frames) = (Vec::new(), Vec::new());
        for (index, (object, places)) in self.objects.iter().zip(&self.places).enumerate() {
            let here = Here {
                object,
                index,
                address: &address,
                resolve: &resolve,
                stubs: &stubs,
                slot: &slot,
                symbol: &|_, _, _| None,
            };
            for (section, place) in object.sections.iter().zip(places) {
                let mut bytes = section.bytes.clone();
                let base = bases.of(*place);
                for at in pairs(&section.relocations, &section.name)? {
                    here.apply(section.kind, &section.name, &mut bytes, base, at)?;
                }
                match place {
                    Place::Code(at) | Place::Data(at) => {
                        let region = if matches!(place, Place::Code(_)) { &mut code } else { &mut data };
                        put(region, *at, &bytes, &section.name)?;
                        if section.kind == Kind::EhFrame
                            && let Some(start) = base
                        {
                            eh_frames.push(start);
                        }
                    }
                    Place::Unwind => unwind.extend(unwind_entries(&bytes)?),
                }
            }
        }
        let entry =
            globals.get("_main").copied().ok_or_else(|| Refused("no object defines main".to_owned()))?;
        if entry < code_base || entry - code_base >= self.code_bytes {
            return refuse("main isn't in the code");
        }
        unwind.sort_by_key(|entry| entry.function);
        Ok(Image { code, data, entry, unwind, eh_frames })
    }

    /// Every symbol the objects define for the others, at its address. A symbol in a section
    /// that isn't placed has none.
    fn globals(&self, address: &dyn Fn(usize, usize) -> Option<u64>) -> Result<BTreeMap<&str, u64>, Refused> {
        let mut globals = BTreeMap::new();
        for (index, object) in self.objects.iter().enumerate() {
            for symbol in object.symbols.iter().filter(|symbol| symbol.external) {
                // The first of a weak symbol's definitions is the one kept.
                if let Some((section, value)) = symbol.defined
                    && address(index, section).is_some()
                    && !(symbol.weak && globals.contains_key(symbol.name.as_str()))
                {
                    globals.insert(
                        symbol.name.as_str(),
                        at_symbol(object, index, address, (section, value), &symbol.name)?,
                    );
                }
            }
        }
        Ok(globals)
    }

    /// Each section the objects have, by object and section number from 0, with its name and the
    /// symbols defined in it, at how far into it each is.
    pub fn sections(&self) -> Vec<SectionInfo> {
        let mut found = Vec::new();
        for (index, object) in self.objects.iter().enumerate() {
            for (number, section) in object.sections.iter().enumerate() {
                let symbols = object
                    .symbols
                    .iter()
                    .filter_map(|symbol| match symbol.defined {
                        Some((within, value)) if within == number => {
                            Some((symbol.name.clone(), value.wrapping_sub(section.addr), symbol.weak))
                        }
                        _ => None,
                    })
                    .collect();
                found.push(SectionInfo {
                    object: index,
                    section: number,
                    name: section.name.clone(),
                    start: section.addr,
                    align: section.align,
                    size: section.size,
                    symbols,
                });
            }
        }
        found
    }

    /// Links each section where `placement` puts it, leaving out those it puts nowhere, with every
    /// check `link` makes: as another linker placed them, to check this one writes what it wrote.
    /// There are no stubs, so every branch has to reach. Each section's result is its own, so one
    /// that refers to a section left out says so alone. An unwind section is read only when every
    /// function it describes is placed.
    pub fn link_placed(&self, placement: &Placement<'_>) -> Result<PlacedLink, Refused> {
        let (at, slot, runtime) = (placement.section, placement.slot, placement.runtime);
        let globals = self.globals(at)?;
        // A weak symbol goes where `runtime` says, when it says, as the other linker may have kept
        // another object's copy.
        let weak: BTreeSet<&str> = self
            .objects
            .iter()
            .flat_map(|object| {
                object.symbols.iter().filter(|symbol| symbol.weak).map(|symbol| symbol.name.as_str())
            })
            .collect();
        let resolve = |name: &str| {
            let kept = weak.contains(name).then(|| runtime(name)).flatten();
            kept.or_else(|| globals.get(name).copied()).or_else(|| runtime(name))
        };
        let stubs = BTreeMap::new();
        let (mut linked, mut unwind) = (Vec::new(), Vec::new());
        for (index, object) in self.objects.iter().enumerate() {
            let symbol = placement.symbol;
            let here = Here { object, index, address: at, resolve: &resolve, stubs: &stubs, slot, symbol };
            for (number, section) in object.sections.iter().enumerate() {
                if section.kind == Kind::CompactUnwind {
                    // Only for the functions that are placed: one that isn't leaves its section out.
                    let mut bytes = section.bytes.clone();
                    let applied =
                        pairs(&section.relocations, &section.name)?.into_iter().try_for_each(|relocation| {
                            here.apply(section.kind, &section.name, &mut bytes, None, relocation)
                        });
                    if applied.is_ok() {
                        unwind.extend(unwind_entries(&bytes)?);
                    }
                    continue;
                }
                let Some(base) = at(index, number) else { continue };
                let mut bytes = section.bytes.clone();
                let applied =
                    pairs(&section.relocations, &section.name)?.into_iter().try_for_each(|relocation| {
                        here.apply(section.kind, &section.name, &mut bytes, Some(base), relocation)
                    });
                linked.push(applied.map(|()| Linked {
                    object: index,
                    section: number,
                    address: base,
                    bytes,
                }));
            }
        }
        unwind.sort_by_key(|entry| entry.function);
        Ok(PlacedLink { sections: linked, unwind })
    }

    /// Writes a stub for each undefined symbol a branch goes to, giving back where each is.
    fn write_stubs(
        &self,
        code: &mut [u8],
        code_base: u64,
        resolve: &dyn Fn(&str) -> Option<u64>,
    ) -> Result<BTreeMap<String, u64>, Refused> {
        let mut stubs = BTreeMap::new();
        let mut at = self.stubs_at()?;
        for name in &self.stubs {
            let target = resolve(name).ok_or_else(|| Refused(format!("nothing defines {name}")))?;
            put(code, at, &apply::stub(target), "a stub")?;
            stubs.insert(
                name.clone(),
                code_base.checked_add(at).ok_or_else(|| Refused("a stub overflows".to_owned()))?,
            );
            at = at.checked_add(16).ok_or_else(|| Refused("stubs overflow".to_owned()))?;
        }
        Ok(stubs)
    }
}

/// Where the two regions start.
#[derive(Clone, Copy)]
struct Bases {
    code: u64,
    data: u64,
}

impl Bases {
    /// The address a section goes to, or none for one that isn't mapped.
    fn of(self, place: Place) -> Option<u64> {
        match place {
            Place::Code(at) => self.code.checked_add(at),
            Place::Data(at) => self.data.checked_add(at),
            Place::Unwind => None,
        }
    }
}

fn size(bytes: u64) -> Result<usize, Refused> {
    usize::try_from(bytes).map_or_else(|_| refuse("a region is too big"), Ok)
}

/// Puts `bytes` into `region` at `at`, if they fit.
fn put(region: &mut [u8], at: u64, bytes: &[u8], what: &str) -> Result<(), Refused> {
    let start = size(at)?;
    let slot = region.get_mut(start..).and_then(|rest| rest.get_mut(..bytes.len()));
    slot.ok_or_else(|| Refused(format!("{what} is outside its region")))?.copy_from_slice(bytes);
    Ok(())
}

/// The address of a symbol defined at `value` in a section of the object at `index`.
fn at_symbol(
    object: &Object,
    index: usize,
    address: &dyn Fn(usize, usize) -> Option<u64>,
    (section, value): (usize, u64),
    name: &str,
) -> Result<u64, Refused> {
    let Some(it) = object.sections.get(section) else {
        return refuse(format!("{name} is in a section that isn't there"));
    };
    let offset = into_section(it, value, name)?;
    address(index, section)
        .and_then(|start| start.checked_add(offset))
        .ok_or_else(|| Refused(format!("{name} is in a section that isn't placed")))
}

/// A section's compact unwind entries, linked.
fn unwind_entries(bytes: &[u8]) -> Result<Vec<Unwind>, Refused> {
    let (entries, rest) = bytes.as_chunks::<COMPACT_UNWIND_ENTRY>();
    if !rest.is_empty() {
        return refuse("__compact_unwind isn't whole entries");
    }
    entries
        .iter()
        .map(|entry| {
            Ok(Unwind {
                function: u64::from_le_bytes(word(entry, 0)?),
                length: u32::from_le_bytes(word(entry, 8)?),
                encoding: u32::from_le_bytes(word(entry, 12)?),
            })
        })
        .collect()
}

/// `N` bytes of `bytes` at `at`.
fn word<const N: usize>(bytes: &[u8], at: usize) -> Result<[u8; N], Refused> {
    let end = at.checked_add(N).ok_or_else(|| Refused("an offset overflows".to_owned()))?;
    bytes
        .get(at..end)
        .and_then(|it| it.try_into().ok())
        .ok_or_else(|| Refused("a value runs past its section".to_owned()))
}

/// A relocation with what goes with it: the addend before it, or the subtraction it finishes.
#[derive(Clone, Copy)]
struct At {
    relocation: Relocation,
    offset: usize,
    width: usize,
    addend: i64,
    minus: Option<Relocation>,
}

/// A section's relocations, each with its addend or subtraction, checked to be whole and inside
/// the section's bytes as far as their length goes.
fn pairs(relocations: &[Relocation], name: &str) -> Result<Vec<At>, Refused> {
    let mut found = Vec::with_capacity(relocations.len());
    let (mut addend, mut minus): (Option<(u32, i64)>, Option<Relocation>) = (None, None);
    for relocation in relocations {
        let width = match relocation.length {
            2 => 4,
            3 => 8,
            _ => return refuse(format!("a relocation in {name} has length {}", relocation.length)),
        };
        let offset = usize::try_from(relocation.offset)
            .map_err(|_| Refused("a relocation is too far in".to_owned()))?;
        match relocation.kind {
            ADDEND => {
                if addend.is_some() || minus.is_some() || relocation.external || relocation.pc_relative {
                    return refuse(format!("an addend in {name} isn't as it should be"));
                }
                // Its "symbol" is a 24-bit signed addend.
                let raw = i64::from(relocation.symbol);
                addend =
                    Some((relocation.offset, if raw & 0x0080_0000 != 0 { raw - 0x0100_0000 } else { raw }));
            }
            SUBTRACTOR => {
                if addend.is_some() || minus.is_some() || relocation.pc_relative || !relocation.external {
                    return refuse(format!("a subtraction in {name} isn't as it should be"));
                }
                minus = Some(*relocation);
            }
            _ => {
                let taken = match addend.take() {
                    Some((at, value))
                        if at == relocation.offset
                            && matches!(relocation.kind, BRANCH26 | PAGE21 | PAGEOFF12) =>
                    {
                        value
                    }
                    Some(_) => {
                        return refuse(format!(
                            "an addend in {name} isn't followed by a relocation that takes it"
                        ));
                    }
                    None => 0,
                };
                let subtracted = minus.take();
                if subtracted.is_some_and(|it| {
                    it.offset != relocation.offset
                        || it.length != relocation.length
                        || relocation.kind != UNSIGNED
                }) {
                    return refuse(format!("a subtraction in {name} isn't followed by its address"));
                }
                found.push(At { relocation: *relocation, offset, width, addend: taken, minus: subtracted });
            }
        }
    }
    if addend.is_some() || minus.is_some() {
        return refuse(format!("{name} ends partway through a relocation"));
    }
    Ok(found)
}

/// What linking a section needs from around it.
struct Here<'h> {
    object: &'h Object,
    /// Which object it is, from 0.
    index: usize,
    /// Where each section goes, by object and section number.
    address: &'h dyn Fn(usize, usize) -> Option<u64>,
    resolve: &'h dyn Fn(&str) -> Option<u64>,
    stubs: &'h BTreeMap<String, u64>,
    /// Where each symbol loaded through the offset table has its slot.
    slot: &'h dyn Fn(&str) -> Option<u64>,
    /// Where a symbol goes apart from its section, by object, section and how far into it.
    symbol: &'h dyn Fn(usize, usize, u64) -> Option<u64>,
}

impl Here<'_> {
    /// The address a relocation refers to: a symbol's, or for one naming a section, how far that
    /// section moved from its address in the object.
    fn target(&self, relocation: &Relocation) -> Result<u64, Refused> {
        let number = usize::try_from(relocation.symbol).unwrap_or(usize::MAX);
        if relocation.external {
            let symbol = self
                .object
                .symbols
                .get(number)
                .ok_or_else(|| Refused("a relocation's symbol isn't there".to_owned()))?;
            return match symbol.defined {
                // A weak symbol is the copy kept, wherever that is.
                Some(_) if symbol.weak => (self.resolve)(&symbol.name)
                    .ok_or_else(|| Refused(format!("{} isn't placed", symbol.name))),
                Some((section, value)) => {
                    let moved = self.object.sections.get(section).and_then(|it| value.checked_sub(it.addr));
                    match moved.and_then(|offset| (self.symbol)(self.index, section, offset)) {
                        Some(address) => Ok(address),
                        None => {
                            at_symbol(self.object, self.index, self.address, (section, value), &symbol.name)
                        }
                    }
                }
                None => (self.resolve)(&symbol.name)
                    .ok_or_else(|| Refused(format!("nothing defines {}", symbol.name))),
            };
        }
        let within =
            number.checked_sub(1).ok_or_else(|| Refused("a relocation names section 0".to_owned()))?;
        let Some(it) = self.object.sections.get(within) else {
            return refuse("a relocation names a section that isn't there");
        };
        (self.address)(self.index, within)
            .map(|start| start.wrapping_sub(it.addr))
            .ok_or_else(|| Refused("a relocation names a section that isn't placed".to_owned()))
    }

    /// Applies one relocation to a section's bytes, the section being at `base`.
    fn apply(
        &self,
        kind: Kind,
        name: &str,
        bytes: &mut [u8],
        base: Option<u64>,
        at: At,
    ) -> Result<(), Refused> {
        let relocation = at.relocation;
        if at.offset.checked_add(at.width).is_none_or(|end| end > bytes.len()) {
            return refuse(format!("a relocation runs past the end of {name}"));
        }
        if matches!(relocation.kind, GOT_LOAD_PAGE21 | GOT_LOAD_PAGEOFF12) {
            return self.through_table(kind, name, bytes, base, at);
        }
        let target = self
            .target(&relocation)?
            .checked_add_signed(at.addend)
            .ok_or_else(|| Refused("a target overflows".to_owned()))?;
        match relocation.kind {
            BRANCH26 | PAGE21 | PAGEOFF12 => {
                if kind != Kind::Code || at.width != 4 {
                    return refuse(format!("an instruction relocation in {name}, which isn't code"));
                }
                if relocation.pc_relative != (relocation.kind != PAGEOFF12) {
                    return refuse(format!(
                        "a relocation in {name} says it's pc-relative where it isn't, or isn't where it is"
                    ));
                }
                let place = base.and_then(|start| start.checked_add(u64::from(relocation.offset)));
                let place = place.ok_or_else(|| Refused("code that isn't placed".to_owned()))?;
                let insn = u32::from_le_bytes(word(bytes, at.offset)?);
                let written = match relocation.kind {
                    BRANCH26 => apply::branch26(insn, place, self.reachable(&relocation, place, target))?,
                    PAGE21 => apply::page21(insn, place, target)?,
                    _ => apply::pageoff12(insn, target)?,
                };
                put(bytes, u64::from(relocation.offset), &written.to_le_bytes(), name)
            }
            UNSIGNED => {
                if kind == Kind::Code || relocation.pc_relative {
                    return refuse(format!("an address relocation in {name}, where it can't be"));
                }
                let minus = at.minus.map(|it| self.target(&it)).transpose()?.unwrap_or(0);
                if at.width == 8 {
                    let old = u64::from_le_bytes(word(bytes, at.offset)?);
                    put(
                        bytes,
                        u64::from(relocation.offset),
                        &old.wrapping_add(target).wrapping_sub(minus).to_le_bytes(),
                        name,
                    )
                } else {
                    let old = u32::from_le_bytes(word(bytes, at.offset)?);
                    let value = i128::from(old) + i128::from(target) - i128::from(minus);
                    let value =
                        i32::try_from(value).map(i32::cast_unsigned).or_else(|_| u32::try_from(value));
                    let value = value.map_err(|_| Refused(format!("a 32-bit value in {name} overflows")))?;
                    put(bytes, u64::from(relocation.offset), &value.to_le_bytes(), name)
                }
            }
            other => refuse(format!("a relocation it doesn't take in {name}: type {other}")),
        }
    }

    /// A load of a symbol's address through its slot in the offset table: `adrp` of the slot's page,
    /// then a 64-bit `ldr` at its offset in the page.
    fn through_table(
        &self,
        kind: Kind,
        name: &str,
        bytes: &mut [u8],
        base: Option<u64>,
        at: At,
    ) -> Result<(), Refused> {
        let relocation = at.relocation;
        let page = relocation.kind == GOT_LOAD_PAGE21;
        if kind != Kind::Code || at.width != 4 || at.addend != 0 || relocation.pc_relative != page {
            return refuse(format!("a load through the offset table in {name} isn't as it should be"));
        }
        let symbol = self.object.symbols.get(usize::try_from(relocation.symbol).unwrap_or(usize::MAX));
        let symbol = symbol.ok_or_else(|| Refused("a relocation's symbol isn't there".to_owned()))?;
        let slot = (self.slot)(&symbol.name)
            .ok_or_else(|| Refused(format!("{} has no slot in the offset table", symbol.name)))?;
        let place = base.and_then(|start| start.checked_add(u64::from(relocation.offset)));
        let place = place.ok_or_else(|| Refused("code that isn't placed".to_owned()))?;
        let insn = u32::from_le_bytes(word(bytes, at.offset)?);
        let written = if page {
            apply::page21(insn, place, slot)?
        } else {
            // Only a 64-bit load can take an address out of a slot.
            if insn & 0xffc0_0000 != 0xf940_0000 {
                return refuse(format!(
                    "a load through the offset table in {name} is on {insn:#010x}, not a 64-bit `ldr`"
                ));
            }
            apply::pageoff12(insn, slot)?
        };
        put(bytes, u64::from(relocation.offset), &written.to_le_bytes(), name)
    }

    /// Where a branch to `target` goes: there, or through the target's stub when it's too far.
    fn reachable(&self, relocation: &Relocation, place: u64, target: u64) -> u64 {
        let far = i128::from(target) - i128::from(place);
        let reach = i128::from(apply::BRANCH_REACH);
        if !relocation.external || (-reach..reach).contains(&far) {
            return target;
        }
        let name = self.object.symbols.get(usize::try_from(relocation.symbol).unwrap_or(usize::MAX));
        name.and_then(|symbol| self.stubs.get(&symbol.name)).copied().unwrap_or(target)
    }
}
