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
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use fennec_check::{Edit, KeptCheck, Signatures};
use fennec_syntax::{Diagnostic, Module, Sources, Span};

/// How much the kept checks of every kit may come to, by estimate. Past it, the kit built longest ago
/// goes first: its next build checks all of it.
const BUDGET: usize = 4 << 30;

/// About what a kit's kept check holds for each byte of its source. It was measured at 451 MB for
/// 7.4 MB of source at 200,000 lines and 72 MB for 0.8 MB at 20,000, with the parsed standard
/// library held in both, so this is on the high side.
const BYTES_PER_SOURCE_BYTE: usize = 100;

/// How far apart files start while there's room for slots (`Sources::add`).
const SLOT: u32 = 1 << 18;

/// The checks kept, once the daemon keeps them.
static KEPT: Mutex<Option<Kept>> = Mutex::new(None);

#[derive(Default)]
struct Kept {
    kits: HashMap<PathBuf, Kit>,
    /// How many functions and tests the last build could carry over, for `daemon status`.
    last_carried: Option<usize>,
    /// What each kit's next build says, once checking all of it again after its last disagreed.
    notices: HashMap<PathBuf, String>,
    /// How many checks have been kept, which tells a kit's kept check from the next build's.
    checks_kept: u64,
}

/// One kit's kept check.
struct Kit {
    check: KeptCheck,
    files: Vec<File>,
    /// What it's estimated to hold, in bytes.
    size: usize,
    built: Instant,
    /// Which check of the kit this is: the kept checks counted when it was kept.
    generation: u64,
    /// The module as the check saw it, with the files the edit changed, until all of it is checked
    /// again and compared.
    pending: Option<(Module, Vec<String>)>,
}

/// A file a kept check was found in: its path, the span its text covers, and its text.
#[derive(Clone, Debug, PartialEq)]
struct File {
    path: String,
    span: Span,
    text: String,
}

/// From now on this process keeps each kit's check for its build after an edit: the build daemon.
pub(crate) fn keep_checks() {
    let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    kept.get_or_insert_with(Kept::default);
}

/// What `daemon status` says of the checks kept, once this process keeps one.
pub(crate) fn status() -> Option<String> {
    let kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    let kept = kept.as_ref().filter(|kept| !kept.kits.is_empty() || kept.last_carried.is_some())?;
    let size: usize = kept.kits.values().map(|kit| kit.size).sum();
    let kits = if kept.kits.len() == 1 { "1 kit".to_owned() } else { format!("{} kits", kept.kits.len()) };
    let last = kept
        .last_carried
        .map_or_else(String::new, |carried| format!("; the last build carried over {carried} functions"));
    Some(format!("checks kept: {kits}, about {} MB{last}\n", size >> 20))
}

/// `fennec_check::check_to_build_saying`, as a build checks. In the build daemon, it carries over
/// what the check of the kit's last build found for what an edit left as it was, and keeps what this
/// one finds for the next. One build in 8 of a dev compiler, picked by what's built, or every one
/// with `FENNEC_COMPARE_KEPT` set, checks all of it again before going on, to compare.
///
/// # Errors
///
/// What the checker finds wrong with it.
pub(crate) fn check_to_build(
    module: &mut Module,
    sources: &Sources,
) -> Result<(Signatures, bool), Vec<Diagnostic>> {
    let compare = std::env::var_os("FENNEC_COMPARE_KEPT").is_some()
        || fennec_check::compares_kept_check(inputs_hash(sources));
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
    let files = files_of(sources);
    let (before, notice) = KEPT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
        .map(|kept| (kept.kits.remove(&kit), kept.notices.remove(&kit)))
        .unwrap_or_default();
    if let Some(notice) = notice {
        eprintln!("{notice}");
    }
    let mut edited = Vec::new();
    let found = match before {
        Some(Kit { check, files: was, .. }) if same_layout(&was, &files) => {
            edited = was
                .iter()
                .zip(&files)
                .filter(|(was, is)| was.text != is.text)
                .map(|(_, is)| is.path.clone())
                .collect();
            let edit = Edit { dirty: dirty(&was, &files), files: slots(&was, &files) };
            fennec_check::check_to_build_keeping(module, (Some(check), Some(&edit)), compare)
        }
        _ => fennec_check::check_to_build_keeping(module, (None, None), compare),
    };
    let (signatures, as_left, check) = found?;
    keep(kit, check, files, edited);
    Ok((signatures, as_left))
}

/// Keeps `check` for `kit`, found in `files` after an edit to `edited`, letting go of the kits built
/// longest ago while the kept checks come to more than the budget: this one too, if it's past it by
/// itself. One that carried anything over is checked all again later (`verify_pending`).
fn keep(kit: PathBuf, mut check: KeptCheck, files: Vec<File>, edited: Vec<String>) {
    let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(kept) = kept.as_mut() else { return };
    kept.last_carried = Some(check.carried_over());
    kept.checks_kept += 1;
    let size = files.iter().map(|file| file.text.len()).sum::<usize>().saturating_mul(BYTES_PER_SOURCE_BYTE);
    let pending = check.take_module().map(|module| (module, edited));
    let generation = kept.checks_kept;
    kept.kits.insert(kit, Kit { check, files, size, built: Instant::now(), generation, pending });
    within_budget(&mut kept.kits, BUDGET);
}

/// Whether a kit's kept check carried anything over and hasn't been checked all again yet.
pub(crate) fn pending() -> bool {
    KEPT.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_some_and(|kept| kept.kits.values().any(|kit| kit.pending.is_some()))
}

/// Checks all of each kit again whose kept check carried anything over, with nothing held while
/// it does, and compares with what that check found, if the kit's kept check is still that one. A
/// difference is the compiler's mistake: the kit's kept check goes, a report is written in `home`'s
/// daemon folder naming the edit and where they disagree, and the kit's next build says so and
/// checks all of it. Gives back the reports written.
pub(crate) fn verify_pending(home: &Path) -> Vec<PathBuf> {
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
        let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(kept) = kept.as_mut() else { return reports };
        let differs = kept
            .kits
            .get(&kit)
            .filter(|found| found.generation == generation)
            .and_then(|found| found.check.differs_from(&whole));
        let Some(part) = differs else { continue };
        kept.kits.remove(&kit);
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
        kept.notices.insert(
            kit,
            format!("note: the build daemon checked the last build of this kit all again, and it disagreed{said}; this build checks all of it"),
        );
        reports.push(report);
    }
}

/// Lets go of the kits built longest ago until the rest come to `budget` or less.
fn within_budget<K: Clone + Eq + std::hash::Hash, T: Sizes>(kits: &mut HashMap<K, T>, budget: usize) {
    while kits.values().map(Sizes::size).sum::<usize>() > budget {
        let Some(oldest) = kits.iter().min_by_key(|(_, kit)| kit.built()).map(|(key, _)| key.clone()) else {
            return;
        };
        kits.remove(&oldest);
    }
}

/// What a kept kit comes to, and when it was built: what the budget is kept by.
trait Sizes {
    fn size(&self) -> usize;
    fn built(&self) -> Instant;
}

impl Sizes for Kit {
    fn size(&self) -> usize {
        self.size
    }

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

/// Each file of a program, as a kept check keeps it: the texts the check parsed.
fn files_of(sources: &Sources) -> Vec<File> {
    sources
        .files()
        .zip(sources.spans())
        .map(|((path, text), span)| File { path: path.to_owned(), span, text: text.to_owned() })
        .collect()
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

/// Whether a program has the files a kept check was found in, by path, in the same order, each
/// starting where it did: then an edit to one moved no other's spans.
fn same_layout(before: &[File], now: &[File]) -> bool {
    before.len() == now.len()
        && before.iter().zip(now).all(|(was, is)| was.path == is.path && was.span.start == is.span.start)
}

/// The span each file's slots cover, the same before an edit and after: to where the next file
/// starts, or for the last, past the longer of its texts to the slot's end.
fn slots(before: &[File], now: &[File]) -> Vec<Span> {
    before
        .iter()
        .zip(now)
        .enumerate()
        .map(|(at, (was, is))| {
            let end = before.get(at + 1).map_or_else(
                || {
                    was.span
                        .end
                        .max(is.span.end)
                        .saturating_add(1)
                        .checked_next_multiple_of(SLOT)
                        .unwrap_or(u32::MAX)
                },
                |next| next.span.start,
            );
            Span { start: was.span.start, end }
        })
        .collect()
}

/// The text of the files a kept check was found in that an edit changed or moved, in their spans:
/// in each changed file, from the first byte that differs to the last, when its length is the same,
/// or else to the end of its slots.
fn dirty(before: &[File], now: &[File]) -> Vec<Span> {
    let slots = slots(before, now);
    before
        .iter()
        .zip(now)
        .zip(slots)
        .filter(|((was, is), _)| was.text != is.text)
        .map(|((was, is), slot)| {
            let (was_text, is_text) = (was.text.as_bytes(), is.text.as_bytes());
            let prefix = was_text.iter().zip(is_text).take_while(|(a, b)| a == b).count();
            let room = was_text.len().min(is_text.len()) - prefix;
            let suffix =
                was_text.iter().rev().zip(is_text.iter().rev()).take_while(|(a, b)| a == b).count().min(room);
            let offset = |at: usize| was.span.start.saturating_add(u32::try_from(at).unwrap_or(u32::MAX));
            let end =
                if was_text.len() == is_text.len() { offset(was_text.len() - suffix) } else { slot.end };
            Span { start: offset(prefix), end }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, start: u32, text: &str) -> File {
        let end = start + u32::try_from(text.len()).unwrap_or(u32::MAX);
        File { path: path.to_owned(), span: Span { start, end }, text: text.to_owned() }
    }

    /// An edit of the same length is its changed bytes alone; one that changes the length runs to
    /// the end of the file's slots: where the next file starts. An unchanged file has none.
    #[test]
    fn an_edits_text_is_its_changed_bytes_or_to_the_end_of_its_slots() {
        let before = [file("a.fen", 0, "def f():\n    pass\n"), file("b.fen", SLOT, "x = 1\n")];
        let same_length = [file("a.fen", 0, "def g():\n    pass\n"), file("b.fen", SLOT, "x = 1\n")];
        assert_eq!(dirty(&before, &same_length), [Span { start: 4, end: 5 }]);
        let longer = [file("a.fen", 0, "def f():\n    return\n"), file("b.fen", SLOT, "x = 1\n")];
        assert_eq!(dirty(&before, &longer), [Span { start: 13, end: SLOT }]);
        assert_eq!(dirty(&before, &before), Vec::<Span>::new());
        let last_longer = [file("a.fen", 0, "def f():\n    pass\n"), file("b.fen", SLOT, "x = 12\n")];
        assert_eq!(dirty(&before, &last_longer), [Span { start: SLOT + 5, end: 2 * SLOT }]);
    }

    /// Files moved, added or taken away mean nothing kept is used.
    #[test]
    fn a_program_with_other_files_or_moved_ones_has_another_layout() {
        let before = [file("a.fen", 0, "x = 1\n"), file("b.fen", SLOT, "y = 2\n")];
        assert!(same_layout(&before, &before));
        assert!(!same_layout(&before, &before[..1]));
        assert!(!same_layout(&before, &[file("a.fen", 0, "x = 1\n"), file("b.fen", 2 * SLOT, "y = 2\n")]));
        assert!(!same_layout(&before, &[file("a.fen", 0, "x = 1\n"), file("c.fen", SLOT, "y = 2\n")]));
    }

    /// A kit of `files` in a folder of its own, named `name`.
    fn kit(name: &str, files: &[(&str, &str)]) -> fennec_temp::Temp {
        let root = fennec_temp::Temp::folder(&format!("kept-{name}")).unwrap();
        let manifest =
            format!("name = \"probe\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
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
        found.map(|_| ()).map_err(|problems| problems.iter().map(|problem| problem.message.clone()).collect())
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
        let other = fennec_syntax::parse("def main(stdout: Stdout):\n    stdout.print(\"other\")\n").unwrap();
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

    struct Fake(usize, Instant);

    impl Sizes for Fake {
        fn size(&self) -> usize {
            self.0
        }

        fn built(&self) -> Instant {
            self.1
        }
    }

    /// Past the budget, the kit built longest ago goes first.
    #[test]
    fn the_kit_built_longest_ago_goes_first() {
        let start = Instant::now();
        let at = |seconds| start + std::time::Duration::from_secs(seconds);
        let mut kits: HashMap<&str, Fake> =
            HashMap::from([("old", Fake(3, at(0))), ("middle", Fake(3, at(1))), ("new", Fake(3, at(2)))]);
        within_budget(&mut kits, 6);
        let mut left: Vec<&str> = kits.keys().copied().collect();
        left.sort_unstable();
        assert_eq!(left, ["middle", "new"]);
        within_budget(&mut kits, 2);
        assert!(kits.is_empty());
    }
}
