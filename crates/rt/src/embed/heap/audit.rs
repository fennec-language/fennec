//! The heap's audit: every live block's owner count checked against the holders actually found.
//!
//! A block's count says how many hold it. The audit counts them again from what's there. Each
//! place in a live block that holds an address is one holder of the block at it, as
//! `children_into` gives them: the same places freeing the block releases. Each address the
//! runtime says it holds itself is one more. A count that differs is a bug in Fennec, in native
//! code or in the runtime, and so is a block left live that nothing reaches from a root: a block
//! that's never freed, like a literal's, or one the runtime holds.
//!
//! It reads the heap where it lies and keeps a few words for each live block. The way to a block
//! is worked out only for one found wrong, and only for the first few of those.
//!
//! During a run it can check less. Native code holds owners in its frames, which the heap doesn't
//! show, so a block may rightly have fewer holders here than its count, and a block nothing here
//! holds may be anything's. One thing is still certain: a block with more holders in the heap than
//! its count is wrong whatever the frames hold, and it's freed while one of them still has it.
//! That's what `during` checks, as a task ends and after every so many blocks made, when asked.
//!
//! What it can't see:
//! - Owners native code holds in its frames. So only the check at the end of a run is exact.
//! - Native blocks, which aren't in the table. An audited program is built without them, so the
//!   code that makes and frees them is never the code audited.
//! - Blocks in the table behind a `shared` value's lock.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::hash::BuildHasherDefault;
use std::rc::Rc;

use super::{AddressHasher, State, children_into, is_never_freed, shape_of, shape_word};
use crate::packed::Element;

/// The exit code of a run whose audit found something wrong.
#[cfg(feature = "audit")]
pub(crate) const EXIT_CODE: i32 = 87;

/// How many of the blocks found wrong are said with the way to them.
const WAYS: usize = 16;

/// How many of the blocks found wrong are said at all. The rest are counted.
const SAID: usize = 64;

/// What's wrong with a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Wrong {
    /// It's live and nothing holds it: its owners were dropped without being released.
    Leaked,
    /// Blocks hold it, and no root leads to any of them: somewhere they hold each other in a ring.
    Ring,
    /// Its count is more than its holders: an owner was dropped, though something still holds it.
    CountedHigh,
    /// Its count is less than its holders: it's freed while one of them still holds it.
    CountedLow,
    /// A live block holds this address, and no block is there.
    Gone,
}

/// A block found wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Finding {
    pub(crate) wrong: Wrong,
    pub(crate) address: u64,
    /// What kind of block it is, as a program would name it.
    pub(crate) kind: String,
    /// The count it has, and how many holders were found.
    pub(crate) stored: u64,
    pub(crate) found: u64,
    /// How many more blocks are held only through it, for one that's leaked or in a ring.
    pub(crate) under: usize,
    /// The blocks that lead to it, each with its kind, the one that holds it last. They start at
    /// a root, or at a block that's leaked when no root leads to it. Empty for a block nothing
    /// holds, and for one found wrong after the first few.
    pub(crate) way: Vec<(u64, String)>,
    /// For a block found wrong during a run, each block that holds it, with its kind, once for
    /// each place it holds it at.
    pub(crate) holders: Vec<(u64, String)>,
}

/// What an audit found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Audit {
    /// How many blocks that can be freed were live.
    pub(crate) live: usize,
    pub(crate) wrong: Vec<Finding>,
}

/// What's known of the block at an address: how many holders were found, and whether it's been
/// reached, from a root or from a block already found wrong.
#[derive(Clone, Copy, Default)]
struct Tally {
    found: u32,
    reached: bool,
}

type Tallies = HashMap<u64, Tally, BuildHasherDefault<AddressHasher>>;

/// Checks every block in this thread's table. `held` is each address the runtime holds itself,
/// outside any block, once for each owner it has there.
pub(in crate::embed) fn audit(state: &State, held: &[u64]) -> Audit {
    let mut tallies = Tallies::default();
    let mut children = Vec::new();
    let mut roots = held.to_vec();
    let mut freeable = Vec::new();
    for block in state.heap.blocks.all() {
        let address = address_of(block);
        if is_never_freed(block) {
            roots.push(address);
        } else {
            freeable.push(address);
        }
        children.clear();
        children_into(state, block, &mut children);
        for child in &children {
            hold(&mut tallies, *child);
        }
    }
    for address in held {
        hold(&mut tallies, *address);
    }
    // In the order of their addresses, so one heap is always said the same way.
    freeable.sort_unstable();
    for root in &roots {
        reach(state, &mut tallies, *root);
    }
    let mut wrong = Vec::new();
    let mut tops = Vec::new();
    // First the blocks nothing holds, each with all it holds. What's left unreached after them is
    // held only by blocks that are unreached themselves: a ring, said once by its first block.
    for nothing_holds in [true, false] {
        for address in &freeable {
            let tally = tallies.get(address).copied().unwrap_or_default();
            if tally.reached || (tally.found == 0) != nothing_holds {
                continue;
            }
            let under = reach(state, &mut tallies, *address).saturating_sub(1);
            let which = if nothing_holds { Wrong::Leaked } else { Wrong::Ring };
            wrong.push(finding(state, which, *address, tally.found, under));
            tops.push(*address);
        }
    }
    for address in &freeable {
        let found = tallies.get(address).map_or(0, |tally| tally.found);
        let stored = state.heap.block(*address).map_or(0, counted);
        // A block nothing holds is said already, with its count.
        let which = match stored.cmp(&u64::from(found)) {
            std::cmp::Ordering::Greater if found > 0 => Wrong::CountedHigh,
            std::cmp::Ordering::Less => Wrong::CountedLow,
            _ => continue,
        };
        wrong.push(finding(state, which, *address, found, 0));
    }
    let mut gone: Vec<(u64, u32)> = tallies
        .iter()
        .filter(|(address, _)| **address != 0 && state.heap.block(**address).is_none())
        .map(|(address, tally)| (*address, tally.found))
        .collect();
    gone.sort_unstable();
    wrong.extend(gone.into_iter().map(|(address, found)| finding(state, Wrong::Gone, address, found, 0)));
    for finding in wrong.iter_mut().filter(|finding| finding.found > 0).take(WAYS) {
        let way = way_to(state, &roots, finding.address).or_else(|| way_to(state, &tops, finding.address));
        finding.way =
            way.unwrap_or_default().into_iter().map(|address| (address, kind_at(state, address))).collect();
    }
    Audit { live: freeable.len(), wrong }
}

/// Checks what can be checked while native code is still running: that no block in this thread's
/// table has more holders, among the table's blocks, than its count. Each one that has is said
/// with the blocks that hold it.
pub(in crate::embed) fn during(state: &State) -> Audit {
    let mut tallies = Tallies::default();
    let mut children = Vec::new();
    let mut live = 0;
    for block in state.heap.blocks.all() {
        live += usize::from(!is_never_freed(block));
        children.clear();
        children_into(state, block, &mut children);
        for child in &children {
            hold(&mut tallies, *child);
        }
    }
    let mut low: Vec<(u64, u32)> = tallies
        .iter()
        .filter(|(address, tally)| {
            // Only a block of this table: another's count is another thread's to say.
            let block = usize::try_from(**address).ok().and_then(|at| state.heap.blocks.get(at));
            block.is_some_and(|block| !is_never_freed(block) && counted(block) < u64::from(tally.found))
        })
        .map(|(address, tally)| (*address, tally.found))
        .collect();
    low.sort_unstable();
    let mut wrong: Vec<Finding> = low
        .into_iter()
        .map(|(address, found)| finding(state, Wrong::CountedLow, address, found, 0))
        .collect();
    for finding in wrong.iter_mut().take(WAYS) {
        for block in state.heap.blocks.all() {
            children.clear();
            children_into(state, block, &mut children);
            let holder = address_of(block);
            let times = children.iter().filter(|child| **child == finding.address).count();
            finding.holders.extend(std::iter::repeat_n((holder, kind_at(state, holder)), times));
        }
        finding.holders.sort_unstable();
    }
    Audit { live, wrong }
}

/// The owners a block is counted as having. A lock's log marks a block it has in its count, which
/// is no owner.
fn counted(block: &[u64]) -> u64 {
    block.first().map_or(0, |count| count & !super::super::lists::MARKED)
}

fn address_of(block: &[u64]) -> u64 {
    u64::try_from(block.as_ptr().addr()).unwrap_or(0)
}

/// One more holder of the block at `address`.
fn hold(tallies: &mut Tallies, address: u64) {
    let tally = tallies.entry(address).or_default();
    tally.found = tally.found.saturating_add(1);
}

/// Marks the block at `from` and all it holds, through any number of blocks, as reached, stopping
/// at those reached already. Gives back how many it marked.
fn reach(state: &State, tallies: &mut Tallies, from: u64) -> usize {
    let mut todo = vec![from];
    let mut children = Vec::new();
    let mut marked = 0;
    while let Some(address) = todo.pop() {
        let Some(block) = state.heap.block(address) else { continue };
        let tally = tallies.entry(address).or_default();
        if tally.reached {
            continue;
        }
        tally.reached = true;
        marked += 1;
        children.clear();
        children_into(state, block, &mut children);
        todo.extend(children.iter().filter(|child| !tallies.get(child).is_some_and(|tally| tally.reached)));
    }
    marked
}

/// The blocks that lead from one of `starts` to the block at `target`, the one that holds it last:
/// the shortest such way. Nothing if none of them leads there.
fn way_to(state: &State, starts: &[u64], target: u64) -> Option<Vec<u64>> {
    let mut came_from: HashMap<u64, u64, BuildHasherDefault<AddressHasher>> = HashMap::default();
    let mut todo: VecDeque<u64> = VecDeque::new();
    for start in starts {
        if came_from.insert(*start, *start).is_none() {
            todo.push_back(*start);
        }
    }
    let mut children = Vec::new();
    while let Some(address) = todo.pop_front() {
        let Some(block) = state.heap.block(address) else { continue };
        children.clear();
        children_into(state, block, &mut children);
        if children.contains(&target) {
            let mut way = vec![address];
            let mut at = address;
            while let Some(before) = came_from.get(&at).copied().filter(|before| *before != at) {
                way.push(before);
                at = before;
            }
            way.reverse();
            return Some(way);
        }
        for child in &children {
            if !came_from.contains_key(child) {
                came_from.insert(*child, address);
                todo.push_back(*child);
            }
        }
    }
    None
}

fn finding(state: &State, wrong: Wrong, address: u64, found: u32, under: usize) -> Finding {
    let stored = state.heap.block(address).map_or(0, counted);
    Finding {
        wrong,
        address,
        kind: kind_at(state, address),
        stored,
        found: u64::from(found),
        under,
        way: Vec::new(),
        holders: Vec::new(),
    }
}

/// What kind of block is at an address, as a program would name it.
fn kind_at(state: &State, address: u64) -> String {
    let Some(block) = state.heap.block(address) else { return "no block".to_owned() };
    let shape = shape_word(block).unwrap_or(0);
    if shape == super::CAPABILITY_SHAPE {
        return "capability".to_owned();
    }
    if shape == super::BYTES_SHAPE {
        return "bytes".to_owned();
    }
    if super::is_text(state, block) {
        return "str".to_owned();
    }
    if shape >= state.tables.str_shape {
        if super::super::dicts::is_set(state, block) {
            return "set".to_owned();
        }
        if super::super::dicts::is_dict(state, block) {
            return "dict".to_owned();
        }
        return match super::super::lists::element_in(state, block) {
            Some(element) => format!("list[{}]", named(state, element)),
            None => "list".to_owned(),
        };
    }
    shape_of(state, block).map_or_else(|| "block".to_owned(), |element| named(state, &element))
}

/// A kind of value's name.
fn named(state: &State, element: &Element) -> String {
    match element {
        Element::Int => "int".to_owned(),
        Element::Float => "float".to_owned(),
        Element::Bool => "bool".to_owned(),
        Element::U64 => "u64".to_owned(),
        Element::F32 => "f32".to_owned(),
        Element::Str | Element::Text => "str".to_owned(),
        Element::Object => "value".to_owned(),
        Element::Struct { name, fields } => match fields.first() {
            Some((field, _)) if &**field == "\0code" => "closure".to_owned(),
            _ => name.to_string(),
        },
        Element::Enum { name, .. } => name.to_string(),
        // Only its name: a type may hold itself.
        Element::Boxed(shape) => match state.tables.shapes.get(*shape).map(Rc::as_ref) {
            Some(Element::Struct { name, .. } | Element::Enum { name, .. }) => name.to_string(),
            _ => "block".to_owned(),
        },
        Element::List(item) => format!("list[{}]", named(state, item)),
        Element::ListOf(_) => "list".to_owned(),
        Element::Maybe(inner) => format!("{}?", named(state, inner)),
    }
}

impl Audit {
    /// What it found, a line for each block, or nothing when every count is right.
    pub(crate) fn said(&self) -> String {
        if self.wrong.is_empty() {
            return String::new();
        }
        let mut said = format!(
            "heap audit: {} wrong among {} live {}\n",
            self.wrong.len(),
            self.live,
            if self.live == 1 { "block" } else { "blocks" }
        );
        for finding in self.wrong.iter().take(SAID) {
            let _ = writeln!(said, "heap audit: {}", finding.said());
        }
        if self.wrong.len() > SAID {
            let _ = writeln!(said, "heap audit: and {} more", self.wrong.len() - SAID);
        }
        said.push_str("heap audit: native blocks and blocks behind a lock aren't checked\n");
        said
    }
}

impl Finding {
    fn said(&self) -> String {
        let Finding { address, kind, stored, found, under, .. } = self;
        let more = match under {
            0 => String::new(),
            1 => ", and 1 more block is held only through it".to_owned(),
            under => format!(", and {under} more blocks are held only through it"),
        };
        let way = if !self.holders.is_empty() {
            let holders: Vec<String> =
                self.holders.iter().map(|(address, kind)| format!("{kind} at {address:#x}")).collect();
            format!(", by {}", holders.join(" and "))
        } else if self.way.is_empty() {
            String::new()
        } else {
            let steps: Vec<String> =
                self.way.iter().map(|(address, kind)| format!("{kind} at {address:#x}")).collect();
            format!(", by way of {}", steps.join(" -> "))
        };
        match self.wrong {
            Wrong::Leaked => {
                format!("leaked: {kind} at {address:#x} is counted {stored} and nothing holds it{more}")
            }
            Wrong::Ring => format!(
                "ring: {kind} at {address:#x} is counted {stored} and held {found} times, by blocks no root \
                 leads to{more}"
            ),
            Wrong::CountedHigh => {
                format!(
                    "counted too high: {kind} at {address:#x} is counted {stored} and held {found} times{way}"
                )
            }
            Wrong::CountedLow => {
                format!(
                    "counted too low: {kind} at {address:#x} is counted {stored} and held {found} times{way}"
                )
            }
            Wrong::Gone => format!("gone: no block is at {address:#x}, which is held {found} times{way}"),
        }
    }
}

/// When this thread's heap is next checked during a run: how many more blocks it makes first, and
/// whether that many have been made.
#[cfg(feature = "audit")]
mod due {
    use std::cell::Cell;
    use std::sync::OnceLock;

    thread_local! {
        static LEFT: Cell<u64> = const { Cell::new(0) };
        static DUE: Cell<bool> = const { Cell::new(false) };
    }

    /// How many blocks a thread makes between checks, from `FENNEC_AUDIT_EVERY`, in a run under
    /// `FENNEC_AUDIT`. Nothing when it isn't asked for.
    fn every() -> Option<u64> {
        static EVERY: OnceLock<Option<u64>> = OnceLock::new();
        *EVERY.get_or_init(|| {
            std::env::var_os("FENNEC_AUDIT")?;
            std::env::var("FENNEC_AUDIT_EVERY").ok()?.parse().ok().filter(|every| *every > 0)
        })
    }

    /// Whether a run checks each thread's heap as a task ends on it: under `FENNEC_AUDIT=tasks`.
    pub(super) fn at_task_ends() -> bool {
        static TASKS: OnceLock<bool> = OnceLock::new();
        *TASKS.get_or_init(|| std::env::var("FENNEC_AUDIT").is_ok_and(|audit| audit == "tasks"))
    }

    /// This thread's heap made a block.
    pub(in crate::embed) fn made_one() {
        let Some(every) = every() else { return };
        // 0 is a thread that hasn't made a block yet.
        let left = match LEFT.get() {
            0 => every,
            left => left,
        };
        if left == 1 {
            DUE.set(true);
            LEFT.set(every);
        } else {
            LEFT.set(left - 1);
        }
    }

    /// Whether a check is due on this thread, which it no longer is once asked.
    pub(super) fn take() -> bool {
        DUE.replace(false)
    }
}

#[cfg(feature = "audit")]
pub(in crate::embed) use due::made_one;

/// Checks this thread's heap if it's made as many blocks as `FENNEC_AUDIT_EVERY` says since the
/// last check. The runtime asks as each of its calls ends, where its own work on the heap is done.
#[cfg(feature = "audit")]
pub(in crate::embed) fn check_if_due(state: &State) {
    if due::take() {
        stop_if_wrong(state, "after as many blocks made as FENNEC_AUDIT_EVERY says");
    }
}

/// Checks this thread's heap as a task ends on it, under `FENNEC_AUDIT=tasks`.
#[cfg(feature = "audit")]
pub(in crate::embed) fn at_a_task_end(state: &State) {
    if due::at_task_ends() {
        stop_if_wrong(state, "as a task ended");
    }
}

/// How many checks every thread has made during the run, for `FENNEC_COUNTS` to say: a test can
/// tell from it that a run asked to check did.
#[cfg(feature = "audit")]
static CHECKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many checks were made during the run so far.
#[cfg(feature = "audit")]
pub(in crate::embed) fn checks() -> u64 {
    CHECKS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Says what a check during the run found, and ends the program there with the audit's exit code:
/// a heap that's wrong is no heap to go on running in.
#[cfg(feature = "audit")]
fn stop_if_wrong(state: &State, when: &str) {
    use std::io::Write as _;
    CHECKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let said = during(state).said();
    if said.is_empty() {
        return;
    }
    let said = format!("heap audit: during the run, {when}\n{said}");
    let _ = std::io::stderr().write_all(said.as_bytes());
    super::super::aot::stop_now(EXIT_CODE);
}

#[cfg(test)]
mod tests;
