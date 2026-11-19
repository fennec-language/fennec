//! What the build daemon keeps of each kit's check between builds: what the first check found, kept
//! apart by function, and the files' texts it was found in. The next build of the kit carries over
//! what an edit left as it was (`fennec_check::check_to_build_keeping`). A build in the command's
//! own process keeps nothing, and checks as it always has.
//!
//! An edit to one file moves no other file's spans, since each file starts in a slot of its own
//! (`Sources::add`). In an edited file, the text from its first changed byte on is the edit's: to
//! its last changed byte when its length is the same, or else to its end. Nothing found there is
//! used again.

use std::collections::HashMap;
use std::path::PathBuf;
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

/// The checks kept, once the daemon keeps them.
static KEPT: Mutex<Option<Kept>> = Mutex::new(None);

#[derive(Default)]
struct Kept {
    kits: HashMap<PathBuf, Kit>,
    /// How many functions and tests the last build could carry over, for `daemon status`.
    last_carried: Option<usize>,
}

/// One kit's kept check.
struct Kit {
    check: KeptCheck,
    files: Vec<File>,
    /// What it's estimated to hold, in bytes.
    size: usize,
    built: Instant,
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
/// with `FENNEC_COMPARE_KEPT` set, checks all of it again to compare, and fails as the compiler's
/// mistake on a difference.
///
/// # Errors
///
/// What the checker finds wrong with it.
pub(crate) fn check_to_build(
    module: &mut Module,
    sources: &Sources,
) -> Result<(Signatures, bool), Vec<Diagnostic>> {
    let keeping = KEPT.lock().unwrap_or_else(PoisonError::into_inner).is_some();
    let Some(kit) = keeping.then(|| kit_of(sources)).flatten() else {
        return fennec_check::check_to_build_saying(module);
    };
    let files = files_of(sources);
    let before =
        KEPT.lock().unwrap_or_else(PoisonError::into_inner).as_mut().and_then(|kept| kept.kits.remove(&kit));
    let compare = std::env::var_os("FENNEC_COMPARE_KEPT").is_some()
        || fennec_check::compares_kept_check(inputs_hash(sources));
    let found = match before {
        Some(Kit { check, files: was, .. }) if same_layout(&was, &files) => {
            let same_text = |before: Span, now: Span| {
                text_at(&was, before).is_some_and(|before| text_at(&files, now) == Some(before))
            };
            let edit = Edit { dirty: dirty(&was, &files), same_text: &same_text };
            fennec_check::check_to_build_keeping(module, (Some(check), Some(&edit)), compare)
        }
        _ => fennec_check::check_to_build_keeping(module, (None, None), compare),
    };
    let (signatures, as_left, check) = found?;
    keep(kit, check, files);
    Ok((signatures, as_left))
}

/// Keeps `check` for `kit`, found in `files`, letting go of the kits built longest ago while the
/// kept checks come to more than the budget: this one too, if it's past it by itself.
fn keep(kit: PathBuf, check: KeptCheck, files: Vec<File>) {
    let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(kept) = kept.as_mut() else { return };
    kept.last_carried = Some(check.carried_over());
    let size = files.iter().map(|file| file.text.len()).sum::<usize>().saturating_mul(BYTES_PER_SOURCE_BYTE);
    kept.kits.insert(kit, Kit { check, files, size, built: Instant::now() });
    within_budget(&mut kept.kits, BUDGET);
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

/// Each file of a program, as a kept check keeps it.
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

/// The text of the files a kept check was found in that an edit changed or moved, in their spans:
/// in each changed file, from the first byte that differs to the last, when its length is the same,
/// or else to its end and the byte of room past it.
fn dirty(before: &[File], now: &[File]) -> Vec<Span> {
    before
        .iter()
        .zip(now)
        .filter(|(was, is)| was.text != is.text)
        .map(|(was, is)| {
            let (was_text, is_text) = (was.text.as_bytes(), is.text.as_bytes());
            let prefix = was_text.iter().zip(is_text).take_while(|(a, b)| a == b).count();
            let room = was_text.len().min(is_text.len()) - prefix;
            let suffix =
                was_text.iter().rev().zip(is_text.iter().rev()).take_while(|(a, b)| a == b).count().min(room);
            let offset = |at: usize| was.span.start.saturating_add(u32::try_from(at).unwrap_or(u32::MAX));
            let end = if was_text.len() == is_text.len() {
                offset(was_text.len() - suffix)
            } else {
                was.span.end.saturating_add(1)
            };
            Span { start: offset(prefix), end }
        })
        .collect()
}

/// The text at `span` in the file it's in.
fn text_at(files: &[File], span: Span) -> Option<&str> {
    let file = files.iter().find(|file| file.span.start <= span.start && span.end <= file.span.end)?;
    let at = |offset: u32| usize::try_from(offset - file.span.start).ok();
    file.text.get(at(span.start)?..at(span.end)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, start: u32, text: &str) -> File {
        let end = start + u32::try_from(text.len()).unwrap_or(u32::MAX);
        File { path: path.to_owned(), span: Span { start, end }, text: text.to_owned() }
    }

    /// An edit of the same length is its changed bytes alone; one that changes the length runs to
    /// the file's end and the byte past it. An unchanged file has none.
    #[test]
    fn an_edits_text_is_its_changed_bytes_or_to_the_files_end() {
        let before = [file("a.fen", 0, "def f():\n    pass\n"), file("b.fen", 1 << 18, "x = 1\n")];
        let same_length = [file("a.fen", 0, "def g():\n    pass\n"), file("b.fen", 1 << 18, "x = 1\n")];
        assert_eq!(dirty(&before, &same_length), [Span { start: 4, end: 5 }]);
        let longer = [file("a.fen", 0, "def f():\n    return\n"), file("b.fen", 1 << 18, "x = 1\n")];
        assert_eq!(dirty(&before, &longer), [Span { start: 13, end: 19 }]);
        assert_eq!(dirty(&before, &before), Vec::<Span>::new());
    }

    /// Files moved, added or taken away mean nothing kept is used.
    #[test]
    fn a_program_with_other_files_or_moved_ones_has_another_layout() {
        let before = [file("a.fen", 0, "x = 1\n"), file("b.fen", 1 << 18, "y = 2\n")];
        assert!(same_layout(&before, &before));
        assert!(!same_layout(&before, &before[..1]));
        assert!(!same_layout(&before, &[file("a.fen", 0, "x = 1\n"), file("b.fen", 2 << 18, "y = 2\n")]));
        assert!(!same_layout(&before, &[file("a.fen", 0, "x = 1\n"), file("c.fen", 1 << 18, "y = 2\n")]));
    }

    /// The text at a span is found in its file, in that file's own offsets.
    #[test]
    fn the_text_at_a_span_is_its_files() {
        let files = [file("a.fen", 0, "x = 1\n"), file("b.fen", 1 << 18, "y = 22\n")];
        assert_eq!(text_at(&files, Span { start: (1 << 18) + 4, end: (1 << 18) + 6 }), Some("22"));
        assert_eq!(text_at(&files, Span { start: 4, end: 9 }), None);
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
