//! What the build daemon keeps of each kit's check between builds: what the first check found, kept
//! apart by function, and the files' texts it was found in, as that check parsed them. The next
//! build of the kit carries over what an edit left as it was (`fennec_check::check_to_build_keeping`).
//! A build in the command's own process keeps nothing, and checks as it always has.
//!
//! An edit to one file moves no other file's spans, since each file starts in a slot of its own
//! (`Sources::add`). In an edited file, the text from its first changed byte on is the edit's: to
//! its last changed byte when its length is the same, or else to the end of the file's slots.
//! Nothing found there is used again.
//!
//! After a build that carried anything over, the daemon checks all of the kit again on a thread of
//! its own, off the build's way, and compares (`verify_pending`). A difference is the compiler's
//! mistake: the kit's kept check goes, a report is written, and the kit's next build says so and
//! checks all of it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use fennec_check::{Edit, KeptCheck, Signatures};
use fennec_syntax::{Diagnostic, Module, Sources};

/// How much the daemon may hold between builds, counted as it's allocated: the kept checks of every
/// kit, and the analyses and objects kept for builds in processes of their own. Past it, the kit
/// built longest ago goes first, and its next build checks all of it. `FENNEC_KEPT_BUDGET` gives
/// another, in bytes, for tests.
#[cfg(unix)]
const BUDGET: usize = 4 << 30;

/// The checks kept, once the daemon keeps them.
static KEPT: Mutex<Option<Kept>> = Mutex::new(None);

/// What the build in progress is noted as in the store once it's built (`built::build_here`), for
/// the check it keeps: a disagreement found later takes that back.
static STORED_AS: Mutex<Option<String>> = Mutex::new(None);

#[derive(Default)]
struct Kept {
    kits: HashMap<PathBuf, Kit>,
    /// How many functions and tests the last build could carry over, for `daemon status`.
    last_carried: Option<usize>,
    /// What each kit's next build says, once checking all of it again after its last disagreed.
    notices: HashMap<PathBuf, String>,
    /// How many checks have been kept, which tells a kit's kept check from the next build's.
    generations: u64,
}

/// One kit's kept check.
struct Kit {
    check: KeptCheck,
    /// The files the check was found in, as it parsed them.
    sources: Sources,
    built: Instant,
    /// Which check of the kit this is: the kept checks counted when it was kept.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only the build daemon, which is unix's alone, reads it")
    )]
    generation: u64,
    /// The module as the check saw it, with the files the edit changed, until all of it is checked
    /// again and compared.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only the build daemon, which is unix's alone, reads it")
    )]
    pending: Option<(Module, Vec<String>)>,
    /// What the build that kept it is noted as in the store.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only the build daemon, which is unix's alone, reads it")
    )]
    stored_as: Option<String>,
}

/// From now on this process keeps each kit's check for its build after an edit: the build daemon.
#[cfg(unix)]
pub(crate) fn keep_checks() {
    let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    kept.get_or_insert_with(Kept::default);
}

/// What `daemon status` says of the checks kept, once this process keeps one.
#[cfg(unix)]
pub(crate) fn status() -> Option<String> {
    let kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    let kept = kept.as_ref().filter(|kept| !kept.kits.is_empty() || kept.last_carried.is_some())?;
    let kits = if kept.kits.len() == 1 { "1 kit".to_owned() } else { format!("{} kits", kept.kits.len()) };
    let last = kept
        .last_carried
        .map_or_else(String::new, |carried| format!("; the last build carried over {carried} functions"));
    let held = crate::held_bytes() >> 20;
    Some(format!("checks kept: {kits}, with {held} MB held in all{last}\n"))
}

/// The build in progress is noted in the store as `name` once it's built, so the check it keeps
/// knows what to take back if checking it all again disagrees later.
pub(crate) fn building_as(name: Option<String>) {
    *STORED_AS.lock().unwrap_or_else(PoisonError::into_inner) = name;
}

/// What the daemon made while it kept analyses that's taken back if one turns out wrong, besides
/// the programs `fennec build` and `fennec run` stored, which go by [`analyses_found_wrong`]. Only the daemon keeps
/// analyses, so only Unix has any.
#[cfg(unix)]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TakenBack {
    /// What a kit's tests passed with, by the kit's absolute path (`test::forget_passed`).
    Tests(String),
}

/// What the daemon made while it kept analyses, since checking them last disagreed: any may have
/// used an analysis kept for another, so all are taken back if one is found wrong. One entry a kit
/// and kind, however often it's built.
#[cfg(unix)]
static BUILT_WHILE_KEEPING: Mutex<std::collections::BTreeSet<TakenBack>> =
    Mutex::new(std::collections::BTreeSet::new());

/// Notes what the daemon made, to take back if an analysis kept for builds turns out wrong.
#[cfg(unix)]
pub(crate) fn built_while_keeping(built: TakenBack) {
    // A daemon whose builds go to processes of their own keeps their analyses there.
    if !fennec_llvm::keeps_between_builds() && !crate::child::wanted() {
        return;
    }
    BUILT_WHILE_KEEPING.lock().unwrap_or_else(PoisonError::into_inner).insert(built);
}

/// Where the daemon keeps the note the next build says once checking the analyses builds used
/// again has disagreed: in the Fennec folder's `daemon`, so another daemon, or a build with none,
/// finds it too.
fn note_at(home: &std::path::Path) -> PathBuf {
    home.join("daemon").join("kept-analyses-note.txt")
}

/// How many times checking the analyses builds used again has disagreed, for this cache
/// (`Cache::analyses_found_wrong`). It's part of what names a stored program (`built::key`), so a
/// program stored before is never put out after: whatever its settings, it may have used the
/// analysis found wrong. While it can't be read, `u64::MAX`, nothing is put out from the store.
pub(crate) fn analyses_found_wrong() -> u64 {
    crate::run::cache().map_or(0, |cache| cache.analyses_found_wrong())
}

/// Checks what builds used again of the analyses the builds before kept, analyzing those functions
/// again and comparing. A difference is the compiler's mistake, and goes as one the checker's kept
/// check finds does ([`verify_pending`]):
/// - every kept analysis goes;
/// - every program `fennec build` and `fennec run` stored, by the count that names them
///   ([`analyses_found_wrong`]), and every kit's test verdicts made while analyses were kept, so
///   nothing made with a wrong fact is put out or trusted again: the same files build and test
///   again;
/// - a report is written in `home`'s daemon folder, naming the functions and what was taken back;
/// - whatever builds, runs or tests next says so, in this daemon or another, and the next build
///   analyzes every function.
///
/// Gives back the report written, if one was.
#[cfg(unix)]
pub(crate) fn verify_analyses(home: &std::path::Path, check: fennec_llvm::AnalysesCheck) -> Option<PathBuf> {
    // The check counts as running till what it found is written down: a daemon that's ending, or
    // a build that's starting, waits for all of it.
    let differ = check.run();
    let written = report_wrong_analyses(home, &differ);
    drop(check);
    written
}

/// What `verify_analyses` does once the functions whose analyses differ are known, as a build in a
/// process of its own finds them and tells the daemon (`crate::child`). Nothing when none differ.
#[cfg(unix)]
pub(crate) fn report_wrong_analyses(home: &std::path::Path, differ: &[String]) -> Option<PathBuf> {
    static REPORTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if differ.is_empty() {
        return None;
    }
    let note_at = note_at(home);
    let _ = std::fs::create_dir_all(home.join("daemon"));
    // The stored programs are taken back only if the count that names them is written.
    let programs = crate::run::cache().is_some_and(|cache| cache.count_analysis_found_wrong().is_ok());
    let taken = std::mem::take(&mut *BUILT_WHILE_KEEPING.lock().unwrap_or_else(PoisonError::into_inner));
    let mut named = Vec::new();
    for built in &taken {
        match built {
            TakenBack::Tests(path) => {
                crate::test::forget_passed(path);
                named.push(format!("which tests of {path} passed"));
            }
        }
    }
    let number = REPORTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let report =
        home.join("daemon").join(format!("kept-analysis-differs-{number}-{}.txt", std::process::id()));
    let shown: Vec<String> = differ.iter().take(20).map(|name| format!("`{name}`")).collect();
    let more = if differ.len() > shown.len() {
        format!(" and {} more", differ.len() - shown.len())
    } else {
        String::new()
    };
    let text = format!(
        "The build daemon analyzed again the functions builds used the analysis of from the builds before, and for {} of them it disagrees: {}{more}. This is a mistake in the compiler, not in the program: please report it with the program and this file.\n\nTaken back: {}{}.\n\nFennec {}\n",
        differ.len(),
        shown.join(", "),
        if programs {
            "every program `fennec build` and `fennec run` had stored"
        } else {
            "no stored program, as the count that names them couldn't be written"
        },
        if named.is_empty() { String::new() } else { format!(", {}", named.join(", ")) },
        env!("CARGO_PKG_VERSION"),
    );
    let written = std::fs::write(&report, text);
    let said = if written.is_ok() { format!(", reported in {}", report.display()) } else { String::new() };
    let taken = if programs {
        "every stored program, run and test result made since is taken back"
    } else {
        "the test results made since are taken back, but the stored programs couldn't be"
    };
    let note = format!(
        "note: the build daemon analyzed again what builds used the analyses of, and it disagreed{said}: the programs they made may be wrong, so {taken}, and the next build analyzes every function\n"
    );
    let _ = std::fs::write(&note_at, note);
    Some(report)
}

/// Checks what's waiting to be checked of the analyses builds used again, and waits for a check
/// that's running, as the daemon ends: what's checked after the answer is checked before it goes.
#[cfg(unix)]
pub(crate) fn finish_analyses_check(home: &std::path::Path) {
    // Builds made in processes of their own are checked there: what they found is taken back now.
    crate::child::after_builds();
    if let Some(check) = fennec_llvm::AnalysesCheck::take() {
        let _ = verify_analyses(home, check);
    }
    fennec_llvm::wait_for_analyses_check();
}

/// Says, in whatever builds, runs or tests now, that checking the analyses builds used again
/// disagreed, if it did since this was last said, in any daemon.
pub(crate) fn say_analyses_notice() {
    let Some(home) = fennec_build::fennec_home() else { return };
    let note_at = note_at(&home);
    if let Ok(note) = std::fs::read_to_string(&note_at)
        && std::fs::remove_file(&note_at).is_ok()
    {
        eprint!("{note}");
    }
}

/// `fennec_check::check_to_build_saying`, as a build checks. In the build daemon, it carries over
/// what the check of the kit's last build found for what an edit left as it was, and keeps what this
/// one finds for the next. One build in 8 of a dev compiler, picked by what's built, or every one
/// with `FENNEC_COMPARE_KEPT` set, checks all of it again before going on, to compare; none with
/// it set to `0`.
///
/// # Errors
///
/// What the checker finds wrong with it.
pub(crate) fn check_to_build(
    module: &mut Module,
    sources: &Sources,
) -> Result<(Signatures, bool), Vec<Diagnostic>> {
    let compare = match std::env::var_os("FENNEC_COMPARE_KEPT") {
        Some(said) => said != "0",
        None => fennec_check::compares_kept_check(inputs_hash(sources)),
    };
    checked_keeping(module, sources, compare)
}

/// `check_to_build`, comparing as `compare` says.
fn checked_keeping(
    module: &mut Module,
    sources: &Sources,
    compare: bool,
) -> Result<(Signatures, bool), Vec<Diagnostic>> {
    let keeping = KEPT.lock().unwrap_or_else(PoisonError::into_inner).is_some();
    let Some(kit) = keeping.then(|| kit_of(sources)).flatten() else {
        return fennec_check::check_to_build_saying(module);
    };
    let (before, notice) = KEPT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
        .map(|kept| (kept.kits.remove(&kit), kept.notices.remove(&kit)))
        .unwrap_or_default();
    if let Some(notice) = notice {
        eprintln!("{notice}");
    }
    // The files the kit's last check was found in, against these: what the edit changed, when
    // they're the same files, each where it was.
    let carried = before.and_then(|Kit { check, sources: was, .. }| {
        let edit = Edit::between(&was, sources)?;
        let edited: Vec<String> = was
            .files()
            .zip(sources.files())
            .filter(|((_, was), (_, is))| was != is)
            .map(|(_, (path, _))| path.to_owned())
            .collect();
        Some((check, edit, edited))
    });
    let (found, edited) = match carried {
        Some((check, edit, edited)) => {
            (fennec_check::check_to_build_keeping(module, (Some(check), Some(&edit)), compare), edited)
        }
        None => (fennec_check::check_to_build_keeping(module, (None, None), compare), Vec::new()),
    };
    let (signatures, as_left, check) = found?;
    keep(kit, check, sources.clone(), edited);
    Ok((signatures, as_left))
}

/// Keeps `check` for `kit`, found in `sources` after an edit to `edited`. One that carried anything
/// over is checked all again later (`verify_pending`). What's kept is held to the budget once the
/// build is done (`fit_budget`).
fn keep(kit: PathBuf, mut check: KeptCheck, sources: Sources, edited: Vec<String>) {
    let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(kept) = kept.as_mut() else { return };
    kept.last_carried = Some(check.carried_over());
    kept.generations += 1;
    let pending = check.take_module().map(|module| (module, edited));
    let generation = kept.generations;
    let stored_as = STORED_AS.lock().unwrap_or_else(PoisonError::into_inner).take();
    let built = Instant::now();
    kept.kits.insert(kit, Kit { check, sources, built, generation, pending, stored_as });
}

/// Lets go of the kits built longest ago while the daemon holds more than its budget, counted as
/// it's allocated: the last one too, if it's past it by itself. It's done once no build is under
/// way, as one holds what it's working on besides what's kept.
#[cfg(unix)]
pub(crate) fn fit_budget() {
    let budget =
        std::env::var("FENNEC_KEPT_BUDGET").ok().and_then(|bytes| bytes.parse().ok()).unwrap_or(BUDGET);
    let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(kept) = kept.as_mut() {
        within_budget(&mut kept.kits, budget, &mut |_| crate::held_bytes());
    }
}

/// Whether a kit's kept check carried anything over and hasn't been checked all again yet.
#[cfg(unix)]
pub(crate) fn pending() -> bool {
    KEPT.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_some_and(|kept| kept.kits.values().any(|kit| kit.pending.is_some()))
}

/// Checks all of each kit again whose kept check carried anything over, with nothing held while
/// it does, and compares with what that check found, if the kit's kept check is still that one. A
/// difference is the compiler's mistake:
/// - the kit's kept check goes, and so does what its build noted in the store, so the next build of
///   the same files builds again rather than putting out the program this one made;
/// - a report is written in `home`'s daemon folder, naming the edit and where they disagree;
/// - the kit's next build says so and checks all of it.
///
/// Gives back the reports written.
#[cfg(unix)]
pub(crate) fn verify_pending(home: &std::path::Path) -> Vec<PathBuf> {
    let mut reports = Vec::new();
    loop {
        let taken = {
            let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
            kept.as_mut().and_then(|kept| {
                kept.kits.iter_mut().find_map(|(kit, found)| {
                    found
                        .pending
                        .take()
                        .map(|(module, edited)| (kit.clone(), found.generation, module, edited))
                })
            })
        };
        let Some((kit, generation, module, edited)) = taken else { return reports };
        let whole = fennec_check::check_whole(&module);
        drop(module);
        // Compared as they are, with the lock held, which is almost always all it takes. Otherwise
        // the kit's check comes out of the store, so a build of it meanwhile checks all of it, and
        // the slow way is taken with nothing held.
        let unsure = {
            let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(kept) = kept.as_mut() else { return reports };
            match kept.kits.get(&kit).filter(|found| found.generation == generation) {
                Some(found) if !found.check.agrees_quickly(&whole) => kept.kits.remove(&kit),
                _ => None,
            }
        };
        let Some(found) = unsure else { continue };
        let Some(part) = found.check.differs_from(&whole) else {
            let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(kept) = kept.as_mut() {
                kept.kits.entry(kit).or_insert(found);
            }
            continue;
        };
        if let Some(name) = &found.stored_as {
            crate::built::forget(name);
        }
        drop(found);
        let report =
            home.join("daemon").join(format!("kept-check-differs-{generation}-{}.txt", std::process::id()));
        let text = format!(
            "The build daemon checked all of a kit again after a build that carried over what its last build's check found, and they disagree, in {part}. This is a mistake in the compiler, not in the program: please report it with the kit and this file.\n\nkit: {}\nfiles the edit changed: {}\nFennec {}\n",
            kit.display(),
            if edited.is_empty() { "none".to_owned() } else { edited.join(", ") },
            env!("CARGO_PKG_VERSION"),
        );
        let written = std::fs::create_dir_all(report.parent().unwrap_or(home))
            .and_then(|()| std::fs::write(&report, text));
        let said =
            if written.is_ok() { format!(", reported in {}", report.display()) } else { String::new() };
        let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(kept) = kept.as_mut() {
            kept.notices.insert(
                kit,
                format!("note: the build daemon checked the last build of this kit all again, and it disagreed{said}: the program that build made may be wrong, so this build checks all of it"),
            );
        }
        reports.push(report);
    }
}

/// Lets go of the kits built longest ago while what's `held` comes to more than `budget`. Each kit
/// let go of is dropped before `held` is asked again.
#[cfg_attr(
    all(not(unix), not(test)),
    expect(dead_code, reason = "only the build daemon, which is unix's alone, keeps kits")
)]
fn within_budget<K: Clone + Eq + std::hash::Hash, T: Built>(
    kits: &mut HashMap<K, T>,
    budget: usize,
    held: &mut dyn FnMut(&HashMap<K, T>) -> usize,
) {
    while held(kits) > budget {
        let Some(oldest) = kits.iter().min_by_key(|(_, kit)| kit.built()).map(|(key, _)| key.clone()) else {
            return;
        };
        kits.remove(&oldest);
    }
}

/// When a kept kit was built: the budget lets go of the one built longest ago first.
trait Built {
    fn built(&self) -> Instant;
}

impl Built for Kit {
    fn built(&self) -> Instant {
        self.built
    }
}

/// Which kit a program is: its first file, where the build is. Nothing for a program with no file
/// that can be found.
fn kit_of(sources: &Sources) -> Option<PathBuf> {
    let (first, _) = sources.files().next()?;
    let here = std::env::current_dir().ok()?;
    std::fs::canonicalize(here.join(first)).ok()
}

/// A hash of every file's text, which picks the dev builds that compare.
fn inputs_hash(sources: &Sources) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::hash::DefaultHasher::new();
    for (path, hash) in sources.hashed_files() {
        path.hash(&mut hasher);
        hash.as_bytes().hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Kits built as the build daemon builds them, which only unix has.
    #[cfg(unix)]
    mod daemon {
        use std::path::Path;

        use super::super::*;

        /// A kit of `files` in a folder of its own, named `name`.
        fn kit(name: &str, files: &[(&str, &str)]) -> fennec_temp::Temp {
            let root = fennec_temp::Temp::folder(&format!("kept-{name}")).unwrap();
            let manifest = format!(
                "name = \"probe\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n",
                env!("CARGO_PKG_VERSION")
            );
            std::fs::write(root.join("kit.toml"), manifest).unwrap();
            write(&root, files);
            root
        }

        fn write(root: &Path, files: &[(&str, &str)]) {
            for (path, text) in files {
                let at = root.join(path);
                std::fs::create_dir_all(at.parent().unwrap()).unwrap();
                std::fs::write(at, text).unwrap();
            }
        }

        /// What a build of the kit at `root` finds: that it passes, or the messages of what's wrong.
        fn built(found: Result<(Signatures, bool), Vec<Diagnostic>>) -> Result<(), Vec<String>> {
            found
                .map(|_| ())
                .map_err(|problems| problems.iter().map(|problem| problem.message.clone()).collect())
        }

        fn load(root: &Path) -> fennec_kit::Program {
            fennec_kit::load(root, &root.display().to_string(), fennec_kit::Entry::Main).unwrap()
        }

        /// A build of the kit at `root` as the daemon makes it, keeping its check and comparing.
        fn kept_build(root: &Path) -> Result<(), Vec<String>> {
            keep_checks();
            let mut program = load(root);
            built(checked_keeping(&mut program.module, &program.sources, true))
        }

        /// A build that keeps nothing.
        fn cold_build(root: &Path) -> Result<(), Vec<String>> {
            let mut program = load(root);
            built(fennec_check::check_to_build_saying(&mut program.module))
        }

        /// How many functions the kit at `root`'s kept check carried over.
        fn carried(root: &Path) -> Option<usize> {
            let kit = std::fs::canonicalize(root.join("src/main.fen")).ok()?;
            KEPT.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()?
                .kits
                .get(&kit)
                .map(|kit| kit.check.carried_over())
        }

        /// Builds the kit as `before`, keeping its check, then as `after`, carrying over: what it finds
        /// is what a build that keeps nothing finds.
        fn edited(
            name: &str,
            before: &[(&str, &str)],
            after: &[(&str, &str)],
        ) -> (fennec_temp::Temp, Result<(), Vec<String>>) {
            let root = kit(name, before);
            assert_eq!(kept_build(&root), Ok(()));
            write(&root, after);
            let found = kept_build(&root);
            assert_eq!(found, cold_build(&root));
            (root, found)
        }

        /// An import's `as` name standing for another type, a leading-dot case whose one enum is
        /// another, and an import moved to the next file each change what a declaration written as it
        /// was means: each is refused as a whole check refuses it.
        #[test]
        fn what_a_files_imports_mean_is_never_carried_over_stale() {
            let main = "from squad import make\nfrom stats import lead_hp\n\ndef main(stdout: Stdout):\n    stdout.print(lead_hp(make()))\n";
            let squad = "from units import Grunt as Unit\n\nstruct Squad:\n    lead: Unit\n\ndef make() -> Squad:\n    return Squad(Unit())\n";
            let units = "struct Grunt:\n    hp = 10\n\nstruct Brute:\n    hp = 2.5\n";
            let stats =
                "from squad import Squad\n\ndef lead_hp(squad: Squad) -> int:\n    return squad.lead.hp\n";
            let files = [
                ("src/main.fen", main),
                ("src/squad.fen", squad),
                ("src/units.fen", units),
                ("src/stats.fen", stats),
            ];
            let brute = squad.replace("import Grunt as Unit", "import Brute as Unit");
            let (_, found) = edited("as-name", &files, &[("src/squad.fen", &brute)]);
            assert!(found.is_err(), "{found:?}");

            let main = "import colors\nimport moods\nfrom theme import START\nfrom uses import is_start\n\ndef main(stdout: Stdout):\n    stdout.print(is_start(START))\n";
            let theme = "from colors import Color\n\nSTART = .red\n";
            let files = [
                ("src/main.fen", main),
                ("src/colors.fen", "enum Color:\n    red\n    green\n"),
                ("src/moods.fen", "enum Mood:\n    red\n    calm\n"),
                ("src/theme.fen", theme),
                (
                    "src/uses.fen",
                    "from colors import Color\n\ndef is_start(color: Color) -> bool:\n    return color == .red\n",
                ),
            ];
            let moods = theme.replace("from colors import Color", "from moods import Mood");
            let (_, found) = edited("dot-case", &files, &[("src/theme.fen", &moods)]);
            assert!(found.is_err(), "{found:?}");

            let main = "from first import one\nfrom second import two\n\ndef main(stdout: Stdout):\n    stdout.print(len(one()), two())\n";
            let files = [
                ("src/main.fen", main),
                (
                    "src/first.fen",
                    "def one() -> Deque[int]:\n    return Deque[int]()\n\nfrom std.collections import Deque\n",
                ),
                ("src/second.fen", "def two() -> int:\n    return 2\n"),
            ];
            let moved = [
                ("src/first.fen", "def one() -> Deque[int]:\n    return Deque[int]()\n"),
                ("src/second.fen", "from std.collections import Deque\n\ndef two() -> int:\n    return 2\n"),
            ];
            let (_, found) = edited("import-moves", &files, &moved);
            assert!(found.is_err(), "{found:?}");
        }

        /// A function in one file grows, which moves a type and a function after it: what's before it,
        /// and every other file, is carried over, and the moved type is still the same type.
        #[test]
        fn an_edit_that_moves_the_rest_of_its_file_carries_over_the_rest() {
            let main = "from shapes import area\n\ndef main(stdout: Stdout):\n    stdout.print(area(2), first(1))\n\n\
                        def first(n: int) -> int:\n    return n + 1\n";
            let shapes = "def grown(n: int) -> int:\n    return n * 2\n\nstruct Square:\n    side = 1\n\n\
                          def area(n: int) -> int:\n    return Square().side * n + grown(n)\n";
            let longer = shapes.replace("    return n * 2\n", "    total = n * 2\n    return total\n");
            let (root, found) = edited(
                "moves",
                &[("src/main.fen", main), ("src/shapes.fen", shapes)],
                &[("src/shapes.fen", &longer)],
            );
            assert_eq!(found, Ok(()));
            assert!(carried(&root).is_some_and(|carried| carried > 0), "{:?}", carried(&root));
        }

        /// After a build that carried anything over, all of the kit is checked again: when it agrees,
        /// the kept check stays; when it doesn't, the kept check goes, a report is written, and the
        /// kit's next build says so.
        #[test]
        fn a_kept_check_is_checked_all_again_later() {
            let main = "def main(stdout: Stdout):\n    stdout.print(first(1) + second(2))\n\n\
                        def first(n: int) -> int:\n    return n + 1\n\n\
                        def second(n: int) -> int:\n    return n * 2\n";
            let root = kit("verify", &[("src/main.fen", main)]);
            let home = fennec_temp::Temp::folder("kept-verify-home").unwrap();
            keep_checks();
            let build = |root: &Path| {
                let mut program = load(root);
                built(checked_keeping(&mut program.module, &program.sources, false))
            };
            assert_eq!(build(&root), Ok(()));
            write(&root, &[("src/main.fen", &main.replace("n * 2", "n * 3"))]);
            assert_eq!(build(&root), Ok(()));
            let kit_key = std::fs::canonicalize(root.join("src/main.fen")).unwrap();
            let pending = |kit: &Path| {
                KEPT.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                    .and_then(|kept| kept.kits.get(kit))
                    .map(|kit| kit.pending.is_some())
            };
            assert_eq!(pending(&kit_key), Some(true));
            // Other kits' checks may be pending too, from tests running beside this one.
            verify_pending(&home);
            assert_eq!(pending(&kit_key), Some(false));
            // A module that isn't what the kept check saw stands in for a check that went wrong.
            write(&root, &[("src/main.fen", &main.replace("n * 2", "n * 4"))]);
            assert_eq!(build(&root), Ok(()));
            let other =
                fennec_syntax::parse("def main(stdout: Stdout):\n    stdout.print(\"other\")\n").unwrap();
            if let Some(kit) = KEPT
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_mut()
                .and_then(|kept| kept.kits.get_mut(&kit_key))
            {
                let edited = kit.pending.take().map(|(_, edited)| edited).unwrap_or_default();
                kit.pending = Some((other, edited));
            }
            let reports = verify_pending(&home);
            assert!(
                reports
                    .iter()
                    .any(|report| std::fs::read_to_string(report).is_ok_and(|text| text.contains("main.fen"))),
                "{reports:?}"
            );
            let kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
            let kept = kept.as_ref().unwrap();
            assert!(!kept.kits.contains_key(&kit_key));
            assert!(kept.notices.get(&kit_key).is_some_and(|notice| notice.contains("disagreed")));
        }
    }

    struct Fake(usize, Instant);

    impl Built for Fake {
        fn built(&self) -> Instant {
            self.1
        }
    }

    /// Past the budget, the kit built longest ago goes first, and what's held is asked again
    /// after each: here, 1 besides what the kits hold.
    #[test]
    fn the_kit_built_longest_ago_goes_first() {
        let start = Instant::now();
        let at = |seconds| start + std::time::Duration::from_secs(seconds);
        let mut kits: HashMap<&str, Fake> =
            HashMap::from([("old", Fake(3, at(0))), ("middle", Fake(3, at(1))), ("new", Fake(3, at(2)))]);
        let mut held = |kits: &HashMap<&str, Fake>| 1 + kits.values().map(|kit| kit.0).sum::<usize>();
        within_budget(&mut kits, 7, &mut held);
        let mut left: Vec<&str> = kits.keys().copied().collect();
        left.sort_unstable();
        assert_eq!(left, ["middle", "new"]);
        within_budget(&mut kits, 3, &mut held);
        assert!(kits.is_empty());
        within_budget(&mut kits, 0, &mut held);
        assert!(kits.is_empty());
    }
}
