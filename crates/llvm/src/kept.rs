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

use crate::analysis::Analysis;
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
}

/// The last two builds' analyses, by function.
static KEPT: Mutex<Option<KeptAnalyses>> = Mutex::new(None);

/// Whether this process keeps analyses from one build to the next.
static KEEPS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
    /// Each analysis used again, by function, with its key: what's checked after the build.
    used: Mutex<Vec<(String, Key)>>,
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
        let found = KEPT.lock().ok().and_then(|mut kept| kept.take());
        // Checking a build again found an analysis it used wrong: nothing kept since is used, as a
        // build that held the store while it was checked put back what it used.
        if FOUND_WRONG.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Keeping::from(None);
        }
        Keeping::from(found)
    }

    /// This build's number, by which what's checked after it turns about (see [`to_check_after`]).
    pub(crate) fn build_number(&self) -> u64 {
        self.build
    }

    /// Each analysis this build used again, by function, with its key.
    pub(crate) fn take_used(&self) -> Vec<(String, Key)> {
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
                used.push((function.name.clone(), kept.key));
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
#[must_use]
pub fn analyses_kept_and_checked(
    program: &fennec_ir::Program,
    kept: &mut KeptForNext,
) -> (Vec<String>, usize) {
    let keeping = Keeping { checks_all: true, ..Keeping::from(kept.0.take()) };
    let (_, differ) = crate::analysis::whole_keeping(program, &keeping);
    let used_again = keeping.used_again();
    // As a build does: what was found wrong isn't handed on.
    kept.0 = differ.is_empty().then(|| keeping.into_kept());
    (differ, used_again)
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
/// to find.
fn rule_off(part: &str) -> bool {
    std::env::var("FENNEC_KEPT_ANALYSIS_RULE_OFF").is_ok_and(|off| off.split(',').any(|named| named == part))
}

/// Whether checking a build again found an analysis it used wrong, since the store was last taken.
static FOUND_WRONG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The analyses the last build in this process used again, with their keys, till a check is
/// waiting for them.
static LAST_USED: Mutex<Vec<(String, Key)>> = Mutex::new(Vec::new());

/// Notes the analyses a build used again that are to be checked after it, for [`check_later`].
pub(crate) fn note_used(used: Vec<(String, Key)>) {
    if let Ok(mut last) = LAST_USED.lock() {
        *last = used;
    }
}

/// How many functions there are in a turn of the analyses checked after builds: each is checked at
/// least once in this many builds.
const TURNS: u64 = 16;

/// Of what a build used again, what's checked after it, and how many of those are near the edit:
/// - every function a call away from one analyzed afresh, a caller or a callee, as a stale
///   analysis almost always comes of a dependency near the edit that the rule missed (a function
///   asks only of its callees, so its callers cover what it was asked);
/// - of the rest, those whose turn it is: one in [`TURNS`], by a hash of the name and the build's
///   number, so each is checked within that many builds.
pub(crate) fn to_check_after(
    program: &fennec_ir::Program,
    used: Vec<(String, Key)>,
    build: u64,
) -> (Used, usize) {
    let reused: std::collections::HashSet<String> = used.iter().map(|(name, _)| name.clone()).collect();
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
    let turn = build % TURNS;
    let in_turn = |name: &str| {
        let hash = crate::keys::key_of("fennec analysis checked in turn", &name);
        u64::from(hash.first().copied().unwrap_or(0)) % TURNS == turn
    };
    let near_count = used.iter().filter(|(name, _)| near.contains(name.as_str())).count();
    let checked =
        used.into_iter().filter(|(name, _)| near.contains(name.as_str()) || in_turn(name)).collect();
    (checked, near_count)
}

/// A check waiting to be made of what a build used again: the program it built, and each
/// analysis it used again, by function, with its key.
static PENDING: Mutex<Option<(fennec_ir::Program, Used)>> = Mutex::new(None);

/// Each analysis a build used again, by function, with its key.
type Used = Vec<(String, Key)>;

/// Has the analyses the build just made used again checked later, against `program`, which it
/// built and needs no more ([`verify_pending_analyses`]): in the daemon, after a build that used
/// any. A release compiler has no other check of them. Whether a check is waiting.
pub fn check_later(program: fennec_ir::Program) -> bool {
    if !keeps_between_builds() {
        return false;
    }
    let used = LAST_USED.lock().map(|mut used| std::mem::take(&mut *used)).unwrap_or_default();
    if used.is_empty() {
        return false;
    }
    PENDING.lock().map(|mut pending| *pending = Some((program, used))).is_ok()
}

/// Whether a check of what a build used again is waiting.
#[must_use]
pub fn analyses_pending() -> bool {
    PENDING.lock().is_ok_and(|pending| pending.is_some())
}

/// Analyzes again each function whose analysis the last build used again, if a check is waiting,
/// and gives back those whose analysis now differs, sorted. Any is the compiler's mistake: every
/// kept analysis goes, and the next build works each out.
#[must_use]
pub fn verify_pending_analyses() -> Vec<String> {
    let Some((program, used)) = PENDING.lock().ok().and_then(|mut pending| pending.take()) else {
        return Vec::new();
    };
    let differ = crate::analysis::analyses_again(&program, &used);
    if !differ.is_empty() {
        FOUND_WRONG.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut kept) = KEPT.lock() {
            *kept = None;
        }
    }
    differ
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
        let used = again.take_used();
        assert!(used.len() >= 2, "{used:?}");
        assert_eq!(crate::analysis::analyses_again(&program, &used), Vec::<String>::new());
        let mut wrong = used.clone();
        let (name, key) = wrong.first_mut().ok_or("nothing used again")?;
        *key = [0; 32];
        let name = name.clone();
        assert_eq!(crate::analysis::analyses_again(&program, &wrong), [name]);
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
        let (differ, used) = analyses_kept_and_checked(&program, &mut kept);
        assert_eq!((differ.len(), used), (0, 0));
        // `listed`'s analysis swapped for `doubled`'s, as a mistake in what's compared would leave it.
        let store = kept.0.as_mut().ok_or("nothing kept")?;
        let doubled =
            store.by_function.get("doubled").map(|kept| Arc::clone(&kept.analysis)).ok_or("no `doubled`")?;
        store.by_function.get_mut("listed").ok_or("no `listed`")?.analysis = doubled;
        let (differ, _) = analyses_kept_and_checked(&program, &mut kept);
        assert_eq!(differ, ["listed"]);
        let (differ, used) = analyses_kept_and_checked(&program, &mut kept);
        assert_eq!((differ.len(), used), (0, 0), "the build after one found wrong used what was kept");
        Ok(())
    }
}
