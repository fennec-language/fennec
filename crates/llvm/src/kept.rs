//! Each function's own analysis, kept from one build to the next in the process that builds: the
//! build daemon, which builds one program after another, edit after edit. An analysis is worked
//! out from its function's IR and a few things it asks of the rest of the program: which of a
//! callee's parameters it changes and which it keeps, its own parameters' ranges, and whether a
//! kind of dict holds its values in its entries. Kept with each of those asks and what it was
//! told, it's used again by a build whose function has the same IR and is told the same: a body
//! edit analyzes the function it changed, and the few whose callees changed what they say, rather
//! than every function of the program.
//!
//! A build keeps its own analyses, and the build before's of the functions it doesn't have, for
//! one build more: a `fennec run` builds the tests a change affects and then the program, one
//! after the other, and each finds the other's. Anything older goes, so at most the functions of
//! the last two builds are kept. Only the daemon keeps them (`keep_analyses_between_builds`): a
//! command that builds once does none of this work, so a build without the daemon, as under CI,
//! costs what it did. What looks at a program beside a build, as `checks_left` does, keeps
//! nothing and uses nothing kept (`analysis::whole`).
//!
//! What's used again is checked, as the checker's kept check is (`fennec_check::compares_kept_check`):
//! a dev compiler analyzes every function again on one build in eight, picked by the program, and
//! any compiler does with `FENNEC_CHECK_WHOLE=1`. A kept analysis that isn't what analyzing again
//! gives stops the build, as the backend's own mistake.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fennec_check::Type;
use fennec_ir::Function;
use fennec_ir::facts::Range;

use crate::analysis::{Analysis, CheckPack};
use crate::keys::Key;

/// What an analysis asked of the rest of the program, and what it was told.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Asked {
    /// Which of a callee's parameters it changes.
    Changes(String, Vec<usize>),
    /// Whether a callee keeps a parameter.
    Keeps(String, usize, bool),
    /// Whether a kind of dict holds its values in its entries.
    InEntries(Type, bool),
}

/// What an analysis can ask of the rest of the program: which of a callee's parameters it changes,
/// whether a callee keeps a parameter, and whether a kind of dict holds its values in its entries.
pub(crate) type Told<'t> =
    (&'t dyn Fn(&str) -> Vec<usize>, &'t dyn Fn(&str, usize) -> bool, &'t dyn Fn(&Type) -> bool);

/// One function's analysis as it was kept.
pub(crate) struct Kept {
    ir: Key,
    params: Vec<Range>,
    asked: Vec<Asked>,
    analysis: Arc<Analysis>,
    /// The analysis's key (`analysis::analysis_key`), as a unit's read of it compares it.
    key: Key,
    /// The build that last worked it out or used it, by its number.
    built: u64,
    /// How many builds have used it again since it was last checked after a build, or since it was
    /// worked out, from where its count starts then ([`first_count`]): it's checked by the
    /// [`TURNS`]th (see [`to_check_after`]).
    unchecked: u32,
}

/// The last two builds' analyses, by function.
static KEPT: Mutex<Option<KeptAnalyses>> = Mutex::new(None);

/// Whether this process keeps analyses from one build to the next.
static KEEPS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the builds from here on may use what the build before kept (see [`use_kept_analyses`]).
static USES_KEPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Has the builds from here on use the analyses the build before kept, or not. A `--release`
/// build doesn't: it's the build that ships, not one of the edit loop's, so it trusts nothing a
/// build before it worked out, and works every analysis out itself. It still keeps its own for
/// the build after.
pub fn use_kept_analyses(uses: bool) {
    USES_KEPT.store(uses, std::sync::atomic::Ordering::Relaxed);
}

/// Has this process keep each build's analyses for the next, as the daemon does: it builds one
/// program after another. A process that builds once doesn't, and pays nothing for it.
pub fn keep_analyses_between_builds() {
    KEEPS.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether this process keeps what a build works out for the next (see
/// [`keep_analyses_between_builds`]).
#[must_use]
pub fn keeps_between_builds() -> bool {
    KEEPS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The analyses one build keeps for the next, by function, with the build's number.
pub(crate) struct KeptAnalyses {
    build: u64,
    by_function: HashMap<String, Kept>,
}

/// What a build keeps for the next, as it works its analyses out.
pub(crate) struct Keeping {
    found: Option<KeptAnalyses>,
    /// This build's number: one more than the build's it found.
    build: u64,
    kept_now: Mutex<HashMap<String, Kept>>,
    used_again: std::sync::atomic::AtomicUsize,
    /// Whether anything is kept at all: not in a process that builds once.
    keeps: bool,
    /// Each analysis used again, by function, with its key and how many builds have used it again
    /// since it was last checked, this one too: what's checked after the build is picked from these.
    used: Mutex<Vec<(String, Key, u32)>>,
    /// Whether what this build used again is checked after it: a daemon's build's is.
    checks_after: bool,
    /// Whether every analysis used again is checked against analyzing it again, whatever
    /// `compares` picks: as the edit fuzzer asks (`analyses_kept_and_checked`).
    checks_all: bool,
}

/// What was asked of the program for one analysis, as it's worked out.
pub(crate) struct Asking {
    asked: RefCell<std::collections::HashSet<Asked>>,
}

impl Asking {
    pub(crate) fn new() -> Self {
        Asking { asked: RefCell::new(std::collections::HashSet::new()) }
    }

    // Each ask is noted once, however often it's made: an analysis asks of a callee at every call.
    fn note(&self, asked: Asked) {
        self.asked.borrow_mut().insert(asked);
    }

    pub(crate) fn changes(&self, callee: &str, told: &[usize]) {
        self.note(Asked::Changes(callee.to_owned(), told.to_vec()));
    }

    pub(crate) fn keeps(&self, callee: &str, at: usize, told: bool) {
        self.note(Asked::Keeps(callee.to_owned(), at, told));
    }

    pub(crate) fn in_entries(&self, dict: &Type, told: bool) {
        self.note(Asked::InEntries(dict.clone(), told));
    }
}

impl Keeping {
    /// What the last build in this process kept, taken for this one: a build beside it, as tests
    /// run them, finds none and works all its analyses out.
    pub(crate) fn start() -> Self {
        if !KEEPS.load(std::sync::atomic::Ordering::Relaxed) {
            return Keeping::off();
        }
        // A check of what the builds before used again may yet find one wrong: the store is taken
        // once it's done, so no build uses what it's about to let go of.
        wait_for_a_running_check();
        let found = KEPT.lock().ok().and_then(|mut kept| kept.take());
        // Checking a build again found an analysis it used wrong: nothing kept since is used, as a
        // build that held the store while it was checked put back what it used.
        if FOUND_WRONG.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Keeping { checks_after: true, ..Keeping::from(None) };
        }
        if !USES_KEPT.load(std::sync::atomic::Ordering::Relaxed) {
            // Numbered on from the build before, but with nothing of it to use.
            return Keeping { found: None, checks_after: true, ..Keeping::from(found) };
        }
        Keeping { checks_after: true, ..Keeping::from(found) }
    }

    /// Whether what this build used again is checked after it.
    pub(crate) fn checks_after(&self) -> bool {
        self.checks_after
    }

    /// Each analysis this build used again, by function, with its key and how many builds have
    /// used it again since it was last checked.
    pub(crate) fn take_used(&self) -> Vec<(String, Key, u32)> {
        self.used.lock().map(|mut used| std::mem::take(&mut *used)).unwrap_or_default()
    }

    /// Using nothing kept, and keeping nothing.
    pub(crate) fn off() -> Self {
        Keeping { keeps: false, ..Keeping::from(None) }
    }

    /// Whether this build keeps its analyses for the next.
    pub(crate) fn keeps(&self) -> bool {
        self.keeps
    }

    /// Starting from `found`, as a test does, or from nothing.
    pub(crate) fn from(found: Option<KeptAnalyses>) -> Self {
        Keeping {
            build: found.as_ref().map_or(0, |found| found.build).wrapping_add(1),
            found,
            kept_now: Mutex::new(HashMap::new()),
            used_again: std::sync::atomic::AtomicUsize::new(0),
            keeps: true,
            used: Mutex::new(Vec::new()),
            checks_after: false,
            checks_all: false,
        }
    }

    /// Whether every analysis used again is to be checked against analyzing it again.
    pub(crate) fn checks_all(&self) -> bool {
        self.checks_all
    }

    /// How many analyses were used again.
    pub(crate) fn used_again(&self) -> usize {
        self.used_again.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The analysis kept for `function`, with its key, if its IR is `ir`, its parameters' ranges
    /// are `params`, and everything it asked is told the same now.
    pub(crate) fn usable(
        &self,
        function: &Function,
        (ir, params): (&Key, &[Range]),
        told: Told<'_>,
    ) -> Option<(Arc<Analysis>, Key)> {
        let kept = self.found.as_ref()?.by_function.get(&function.name)?;
        let (changes, keeps, in_entries) = told;
        let same = kept.ir == *ir
            && (kept.params == params || rule_off("ranges"))
            && kept.asked.iter().all(|asked| match asked {
                Asked::Changes(callee, was) => changes(callee) == *was,
                Asked::Keeps(callee, at, was) => keeps(callee, *at) == *was,
                Asked::InEntries(dict, was) => in_entries(dict) == *was,
            });
        if same {
            self.used_again.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if let Ok(mut used) = self.used.lock() {
                used.push((function.name.clone(), kept.key, kept.unchecked.saturating_add(1)));
            }
        }
        same.then(|| (Arc::clone(&kept.analysis), kept.key))
    }

    /// Keeps `analysis` of `function`, with its key, for the next build, with what it asked.
    pub(crate) fn keep(
        &self,
        function: &Function,
        (ir, params): (Key, &[Range]),
        asked: Vec<Asked>,
        (analysis, key): (&Arc<Analysis>, Key),
    ) {
        let kept = Kept {
            ir,
            params: params.to_vec(),
            asked,
            analysis: Arc::clone(analysis),
            key,
            built: self.build,
            unchecked: first_count(&function.name),
        };
        if let Ok(mut kept_now) = self.kept_now.lock() {
            kept_now.insert(function.name.clone(), kept);
        }
    }

    /// Keeps a function's analysis used again for the next build, as it was kept.
    pub(crate) fn keep_again(&self, function: &Function) {
        let Some(kept) = self.found.as_ref().and_then(|found| found.by_function.get(&function.name)) else {
            return;
        };
        let again = Kept {
            ir: kept.ir,
            params: kept.params.clone(),
            asked: kept.asked.clone(),
            analysis: Arc::clone(&kept.analysis),
            key: kept.key,
            built: self.build,
            unchecked: kept.unchecked.saturating_add(1),
        };
        if let Ok(mut kept_now) = self.kept_now.lock() {
            kept_now.insert(function.name.clone(), again);
        }
    }

    /// Hands what this build kept to the next in this process.
    pub(crate) fn finish(self) {
        if !self.keeps {
            return;
        }
        let kept_now = self.into_kept();
        if let Ok(mut kept) = KEPT.lock() {
            *kept = Some(kept_now);
        }
    }

    /// What this build keeps: its own analyses, and those the build before worked out or used of
    /// functions this build doesn't have, for one build more.
    pub(crate) fn into_kept(self) -> KeptAnalyses {
        let mut by_function = self.kept_now.into_inner().unwrap_or_default();
        if let Some(found) = self.found {
            let carried: Vec<(String, Kept)> = found
                .by_function
                .into_iter()
                .filter(|(name, kept)| kept.built == found.build && !by_function.contains_key(name))
                .collect();
            by_function.extend(carried);
        }
        KeptAnalyses { build: self.build, by_function }
    }
}

impl Asking {
    pub(crate) fn into_asked(self) -> Vec<Asked> {
        self.asked.into_inner().into_iter().collect()
    }
}

/// What one call of [`analyses_kept_and_checked`] keeps for the next: each function's analysis,
/// as the daemon keeps them from one build to the next.
#[derive(Default)]
pub struct KeptForNext(Option<KeptAnalyses>);

/// Every function's analysis of `program`, worked out as an edit build through the daemon works
/// them out: using again what `kept` holds from the call before, and leaving in it what's worked
/// out now. Every analysis used again is checked against analyzing it again, as
/// `FENNEC_CHECK_WHOLE=1` has a build do. Gives back the functions whose analysis used again isn't
/// what analyzing it again gives, sorted, and how many were used again. It's for the edit fuzzer,
/// which runs it on each version of a program in turn.
///
/// What was kept goes through bytes and back first, as a daemon whose builds are each made in a
/// process of their own hands it on: `Err` says so when it doesn't read back as it was written.
pub fn analyses_kept_and_checked(
    program: &fennec_ir::Program,
    kept: &mut KeptForNext,
) -> Result<(Vec<String>, usize), String> {
    let found = kept.0.take().as_ref().map(through_bytes).transpose()?;
    let keeping = Keeping { checks_all: true, ..Keeping::from(found) };
    let (_, differ) = crate::analysis::whole_keeping(program, &keeping);
    let used_again = keeping.used_again();
    // As a build does: what was found wrong isn't handed on.
    kept.0 = differ.is_empty().then(|| keeping.into_kept());
    Ok((differ, used_again))
}

/// What's kept, written as bytes and read back, and the bytes it reads back as written again the
/// same.
fn through_bytes(found: &KeptAnalyses) -> Result<KeptAnalyses, String> {
    use fennec_syntax::codec::{Decode as _, Encode as _};
    let mut to = fennec_syntax::codec::Writer::default();
    found.encode(&mut to);
    let bytes = to.into_bytes();
    let mut from = fennec_syntax::codec::Reader::new(&bytes);
    let read = KeptAnalyses::decode(&mut from).filter(|_| from.is_done());
    let read = read.ok_or("the kept analyses didn't read back from their bytes")?;
    let mut again = fennec_syntax::codec::Writer::default();
    read.encode(&mut again);
    if again.into_bytes() != bytes {
        return Err("the kept analyses read back from their bytes as something else".to_owned());
    }
    Ok(read)
}

/// How many analyses the last build in this process used again, of how many, for
/// `FENNEC_CHECK_WHOLE=1` to say.
static LAST: Mutex<(usize, usize)> = Mutex::new((0, 0));

/// Notes how many analyses a build used again, of how many.
pub(crate) fn note_used_again(used_again: usize, of: usize) {
    if let Ok(mut last) = LAST.lock() {
        *last = (used_again, of);
    }
}

/// How many analyses the last build in this process used again from the one before, of how many.
#[must_use]
pub fn analyses_used_again() -> (usize, usize) {
    LAST.lock().map_or((0, 0), |last| *last)
}

/// Whether this build analyzes every function again to check what it used again: always with
/// `FENNEC_CHECK_WHOLE=1`, and in a dev compiler on one build in eight, picked by `inputs`, so the
/// same program always does or doesn't.
pub(crate) fn compares(inputs: u64) -> bool {
    match std::env::var("FENNEC_CHECK_WHOLE").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => cfg!(debug_assertions) && inputs.is_multiple_of(8),
    }
}

/// Whether a test has turned part of the rule for using an analysis again off
/// (`FENNEC_KEPT_ANALYSIS_RULE_OFF`), so that a wrong one is used, for the check after the build
/// to find. Only in a dev compiler: a release compiler leaves out none.
fn rule_off(part: &str) -> bool {
    cfg!(debug_assertions)
        && std::env::var("FENNEC_KEPT_ANALYSIS_RULE_OFF")
            .is_ok_and(|off| off.split(',').any(|named| named == part))
}

/// Whether checking a build again found an analysis it used wrong, since the store was last taken.
static FOUND_WRONG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What's waiting to be checked of what builds used again, and whether a check is running.
struct Checks {
    /// Each build's pick, with what analyzing it was told, in the order they were made: every
    /// build that used anything again, a program's, a test's or a run's, hands its own.
    waiting: Vec<CheckPack>,
    running: bool,
    /// How many builds are waiting for the running check to end, for a test to know one is.
    #[cfg(test)]
    waiters: usize,
}

static CHECKS: Mutex<Checks> = Mutex::new(Checks {
    waiting: Vec::new(),
    running: false,
    #[cfg(test)]
    waiters: 0,
});

/// Told when a running check ends.
static CHECK_ENDED: std::sync::Condvar = std::sync::Condvar::new();

/// Notes what a build used again that's to be checked after it (see [`AnalysesCheck`]).
pub(crate) fn note_pack(pack: CheckPack) {
    if pack.is_empty() {
        return;
    }
    if let Ok(mut checks) = CHECKS.lock() {
        checks.waiting.push(pack);
    }
}

/// Waits till no check of what builds used again is running, as the daemon does before it ends.
pub fn wait_for_analyses_check() {
    wait_for_a_running_check();
}

/// Waits till no check is running: one that finds an analysis wrong lets the store go, and the
/// build that waited takes nothing of it.
fn wait_for_a_running_check() {
    let Ok(mut checks) = CHECKS.lock() else { return };
    while checks.running {
        #[cfg(test)]
        {
            checks.waiters += 1;
        }
        let Ok(waited) = CHECK_ENDED.wait(checks) else { return };
        checks = waited;
        #[cfg(test)]
        {
            checks.waiters -= 1;
        }
    }
}

/// How many builds may use an analysis again before it's checked after one: each is checked by
/// the time this many have used it, whatever else the daemon builds.
const TURNS: u32 = 16;

/// Of what a build used again, what's checked after it, and how many of those are near the edit:
/// - every function a call away from one analyzed afresh, a caller or a callee, as a stale
///   analysis almost always comes of a dependency near the edit that the rule missed (a function
///   asks only of its callees, so its callers cover what it was asked);
/// - every other whose count of uses has come to [`TURNS`], so each analysis is checked by its
///   sixteenth use since it was worked out or last checked, counted by its own uses, not the builds
///   around them.
pub(crate) fn to_check_after(program: &fennec_ir::Program, used: Vec<(String, Key, u32)>) -> (Used, usize) {
    let reused: std::collections::HashSet<String> = used.iter().map(|(name, ..)| name.clone()).collect();
    let mut near: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for function in program.functions.iter().chain(&program.constants) {
        let afresh = !reused.contains(&function.name);
        let calls: Vec<&str> = crate::analysis::callees(function).collect();
        if afresh {
            near.extend(calls.into_iter().filter(|callee| reused.contains(*callee)));
        } else if calls.iter().any(|callee| !reused.contains(*callee)) {
            near.insert(function.name.as_str());
        }
    }
    let near_count = used.iter().filter(|(name, ..)| near.contains(name.as_str())).count();
    let checked = used
        .into_iter()
        .filter(|(name, _, uses)| near.contains(name.as_str()) || *uses >= TURNS)
        .map(|(name, key, _)| (name, key))
        .collect();
    (checked, near_count)
}

/// Each analysis a build used again, by function, with its key.
type Used = Vec<(String, Key)>;

/// Where an analysis's count of uses starts when it's worked out: its function's place in the
/// [`TURNS`], by a hash of its name. So what one build works out comes due over the next sixteen,
/// about a sixteenth each, rather than all in one. A check sets the count back to 0, so each is
/// checked every sixteen uses after that, and the places stay spread.
fn first_count(name: &str) -> u32 {
    let hash = crate::keys::key_of("fennec analysis checked in turn", &name);
    u32::from(hash.first().copied().unwrap_or(0)) % TURNS
}

/// What's waiting to be checked of what builds used again, taken to check: no build takes the
/// store till it's done, and dropped, the check is ended.
pub struct AnalysesCheck {
    packs: Vec<CheckPack>,
}

impl AnalysesCheck {
    /// Takes what's waiting, if anything is, with the check marked running from now: before its
    /// thread starts, so a build after this one waits for it.
    #[must_use]
    pub fn take() -> Option<AnalysesCheck> {
        let mut checks = CHECKS.lock().ok()?;
        if checks.waiting.is_empty() {
            return None;
        }
        checks.running = true;
        Some(AnalysesCheck { packs: std::mem::take(&mut checks.waiting) })
    }

    /// Analyzes again each function picked, and gives back those whose analysis now differs,
    /// sorted. Any is the compiler's mistake: every kept analysis goes, and the next build works
    /// each out. Where none differs, each analysis checked starts its count of uses again.
    #[must_use]
    pub fn run(&self) -> Vec<String> {
        let mut differ: Vec<String> = self.packs.iter().flat_map(CheckPack::differ).collect();
        differ.sort();
        differ.dedup();
        if differ.is_empty() {
            if let Ok(mut kept) = KEPT.lock()
                && let Some(kept) = kept.as_mut()
            {
                for (name, key) in self.packs.iter().flat_map(CheckPack::checked) {
                    if let Some(entry) = kept.by_function.get_mut(name)
                        && entry.key == key
                    {
                        entry.unchecked = 0;
                    }
                }
            }
        } else {
            FOUND_WRONG.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Ok(mut kept) = KEPT.lock() {
                *kept = None;
            }
        }
        differ
    }
}

impl Drop for AnalysesCheck {
    fn drop(&mut self) {
        if let Ok(mut checks) = CHECKS.lock() {
            checks.running = false;
        }
        CHECK_ENDED.notify_all();
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    /// Checking again what a build used finds nothing when the analyses are right, and finds the one
    /// whose key isn't what analyzing it again gives.
    #[test]
    fn checking_again_what_a_build_used_finds_only_what_is_wrong() -> Result<(), String> {
        let text = "def doubled(count: int) -> int:\n    return count * 2\n\ndef listed(count: int) -> int:\n    return [5, 6, 7][count]\n\ndef main(stdout: Stdout):\n    stdout.print(doubled(2), listed(1))\n    return\n";
        let mut module = fennec_syntax::parse(text).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let program = fennec_ir::lower(&module, &checked);
        let first = Keeping::from(None);
        let _ = crate::analysis::whole_keeping(&program, &first);
        let again = Keeping::from(Some(first.into_kept()));
        let _ = crate::analysis::whole_keeping(&program, &again);
        let used: Vec<(String, Key)> =
            again.take_used().into_iter().map(|(name, key, _)| (name, key)).collect();
        assert!(used.len() >= 2, "{used:?}");
        assert_eq!(crate::analysis::analyses_again(&program, &used), Vec::<String>::new());
        let mut wrong = used.clone();
        let (name, key) = wrong.first_mut().ok_or("nothing used again")?;
        *key = [0; 32];
        let name = name.clone();
        assert_eq!(crate::analysis::analyses_again(&program, &wrong), [name]);
        Ok(())
    }

    /// An analysis used again build after build, with nothing near it edited, is picked to be
    /// checked by its sixteenth use since it was worked out, at its own place in the sixteen; once
    /// checked, at its sixteenth use after that, and not before.
    #[test]
    fn an_analysis_used_again_is_checked_by_its_sixteenth_use() -> Result<(), String> {
        let _alone = RUNNING_CHECKS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let text = "def doubled(count: int) -> int:\n    return count * 2\n\ndef main(stdout: Stdout):\n    stdout.print(doubled(2))\n    return\n";
        let mut module = fennec_syntax::parse(text).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let program = fennec_ir::lower(&module, &checked);
        let mut keeping = Keeping::from(None);
        let _ = crate::analysis::whole_keeping(&program, &keeping);
        let due = TURNS - first_count("doubled");
        for uses in 1..due {
            keeping = Keeping::from(Some(keeping.into_kept()));
            let _ = crate::analysis::whole_keeping(&program, &keeping);
            let (picked, near) = to_check_after(&program, keeping.take_used());
            assert_eq!(near, 0);
            assert!(
                !picked.iter().any(|(name, _)| name == "doubled"),
                "use {uses}, due at {due}: {picked:?}"
            );
        }
        // Due now: picked, and checked, as the daemon checks it, with the store where it keeps it.
        keeping = Keeping { checks_after: true, ..Keeping::from(Some(keeping.into_kept())) };
        let _ = crate::analysis::whole_keeping(&program, &keeping);
        keeping.finish();
        let check = AnalysesCheck::take().ok_or("nothing was picked to check")?;
        assert_eq!(check.run(), Vec::<String>::new());
        drop(check);
        let mut kept = KEPT.lock().map_err(|_| "the store's lock")?.take().ok_or("nothing kept")?;
        // Checked, it's picked again only at its sixteenth use after.
        for uses in 1..=TURNS {
            keeping = Keeping::from(Some(kept));
            let _ = crate::analysis::whole_keeping(&program, &keeping);
            let (picked, _) = to_check_after(&program, keeping.take_used());
            let doubled = picked.iter().any(|(name, _)| name == "doubled");
            assert_eq!(doubled, uses == TURNS, "use {uses} after the check: {picked:?}");
            kept = keeping.into_kept();
        }
        Ok(())
    }

    /// The tests that mark a check running, one at a time: a check that ends wakes every build
    /// waiting, so one test's ending would end the wait another is testing.
    static RUNNING_CHECKS: Mutex<()> = Mutex::new(());

    /// A build waits for a check that's running before it takes the store, and goes on once it ends.
    #[test]
    fn a_build_waits_for_a_running_check() {
        let _alone = RUNNING_CHECKS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let checks = || CHECKS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        checks().running = true;
        // Ended however the test goes, so no later test that takes the store waits for it.
        let check = AnalysesCheck { packs: Vec::new() };
        let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let waiting = {
            let ended = Arc::clone(&ended);
            std::thread::spawn(move || {
                wait_for_a_running_check();
                ended.load(std::sync::atomic::Ordering::SeqCst)
            })
        };
        // Till the build is waiting, or has stopped without: it counts itself under the lock, which
        // waiting gives up, so once it's counted here, it's waiting.
        while checks().waiters == 0 && !waiting.is_finished() {
            std::thread::yield_now();
        }
        ended.store(true, std::sync::atomic::Ordering::SeqCst);
        drop(check);
        assert_eq!(waiting.join().ok(), Some(true), "it didn't wait for the check");
    }

    /// What's kept reads back from its bytes as it was written, and bytes cut short or damaged keep
    /// nothing, so the next build works every analysis out.
    #[test]
    fn kept_analyses_read_back_as_written_and_damaged_bytes_keep_nothing() -> Result<(), String> {
        let text = "def doubled(count: int) -> int:\n    return count * 2\n\ndef listed(count: int) -> int:\n    return [5, 6, 7][count]\n\ndef main(stdout: Stdout):\n    stdout.print(doubled(2), listed(1))\n    return\n";
        let mut module = fennec_syntax::parse(text).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let program = fennec_ir::lower(&module, &checked);
        let keeping = Keeping::from(None);
        let _ = crate::analysis::whole_keeping(&program, &keeping);
        let kept = keeping.into_kept();
        let mut to = fennec_syntax::codec::Writer::default();
        fennec_syntax::codec::Encode::encode(&kept, &mut to);
        let bytes = to.into_bytes();
        let read = |bytes: &[u8]| {
            let mut from = fennec_syntax::codec::Reader::new(bytes);
            <KeptAnalyses as fennec_syntax::codec::Decode>::decode(&mut from).filter(|_| from.is_done())
        };
        let back = read(&bytes).ok_or("it didn't read back")?;
        assert_eq!(back.by_function.len(), kept.by_function.len());
        let mut again = fennec_syntax::codec::Writer::default();
        fennec_syntax::codec::Encode::encode(&back, &mut again);
        assert!(again.into_bytes() == bytes, "it read back as something else");
        for cut in [1, bytes.len() / 2, bytes.len() - 1] {
            assert!(read(bytes.get(..cut).ok_or("cut")?).is_none(), "{cut} bytes read");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(read(&longer).is_none(), "a byte too many read");
        Ok(())
    }

    /// A kept analysis the check finds isn't what analyzing again gives is handed to no later build:
    /// the next works every analysis out afresh.
    #[test]
    fn an_analysis_found_wrong_is_kept_for_no_later_build() -> Result<(), String> {
        let text = "def doubled(count: int) -> int:\n    return count * 2\n\ndef listed(count: int) -> int:\n    return [5, 6, 7][count]\n\ndef main(stdout: Stdout):\n    stdout.print(doubled(2), listed(1))\n    return\n";
        let mut module = fennec_syntax::parse(text).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let program = fennec_ir::lower(&module, &checked);
        let mut kept = KeptForNext::default();
        let (differ, used) = analyses_kept_and_checked(&program, &mut kept)?;
        assert_eq!((differ.len(), used), (0, 0));
        // `listed`'s analysis swapped for `doubled`'s, as a mistake in what's compared would leave it.
        let store = kept.0.as_mut().ok_or("nothing kept")?;
        let doubled =
            store.by_function.get("doubled").map(|kept| Arc::clone(&kept.analysis)).ok_or("no `doubled`")?;
        store.by_function.get_mut("listed").ok_or("no `listed`")?.analysis = doubled;
        let (differ, _) = analyses_kept_and_checked(&program, &mut kept)?;
        assert_eq!(differ, ["listed"]);
        let (differ, used) = analyses_kept_and_checked(&program, &mut kept)?;
        assert_eq!((differ.len(), used), (0, 0), "the build after one found wrong used what was kept");
        Ok(())
    }
}

// The store as bytes, for the daemon to keep between builds made in processes of their own (see
// `kept_to_bytes`).
fennec_syntax::codec_fields!(Kept { ir, params, asked, analysis, key, built, unchecked });
fennec_syntax::codec_fields!(KeptAnalyses { build, by_function });

impl fennec_syntax::codec::Encode for Asked {
    fn encode(&self, to: &mut fennec_syntax::codec::Writer) {
        match self {
            Asked::Changes(callee, told) => {
                to.tag(0);
                callee.encode(to);
                told.encode(to);
            }
            Asked::Keeps(callee, at, told) => {
                to.tag(1);
                callee.encode(to);
                at.encode(to);
                told.encode(to);
            }
            Asked::InEntries(dict, told) => {
                to.tag(2);
                dict.encode(to);
                told.encode(to);
            }
        }
    }
}

impl fennec_syntax::codec::Decode for Asked {
    fn decode(from: &mut fennec_syntax::codec::Reader<'_>) -> Option<Self> {
        Some(match from.tag()? {
            0 => Asked::Changes(String::decode(from)?, Vec::decode(from)?),
            1 => Asked::Keeps(String::decode(from)?, usize::decode(from)?, bool::decode(from)?),
            2 => Asked::InEntries(Type::decode(from)?, bool::decode(from)?),
            _ => return None,
        })
    }
}

/// What this process keeps for its next build, as bytes, for a daemon whose builds are each made
/// in a process of their own: it holds them while no build runs and hands them to the next. A
/// check of what the build used that's running is waited for, so what's written is what it left.
/// Empty when nothing is kept.
#[must_use]
pub fn kept_to_bytes() -> Vec<u8> {
    use fennec_syntax::codec::Encode as _;
    wait_for_a_running_check();
    let kept = KEPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(kept) = kept.as_ref() else { return Vec::new() };
    let mut to = fennec_syntax::codec::Writer::default();
    kept.encode(&mut to);
    to.into_bytes()
}

/// Keeps what [`kept_to_bytes`] wrote, for this process's next build to use. Bytes that don't read
/// keep nothing, as if no build had come before, and say so: `false`.
pub fn keep_from_bytes(bytes: &[u8]) -> bool {
    use fennec_syntax::codec::Decode as _;
    if bytes.is_empty() {
        return true;
    }
    let mut from = fennec_syntax::codec::Reader::new(bytes);
    let read = KeptAnalyses::decode(&mut from).filter(|_| from.is_done());
    let read_it = read.is_some();
    *KEPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = read;
    read_it
}
