//! What the build daemon holds of each kit it runs, so that a run of a kit whose files haven't
//! changed since a run before needs one look at its files and nothing read from the cache: whether
//! its tests passed, what the lint found, the image it was kept as, and the root of what that
//! built.
//!
//! Each is held with the files as they were when it was found, and is used only while they're the
//! same by the test the cache's own notes are held to (`verdict::same_files`): every file's text,
//! where a file's size and changed time say what that is only once it's settled. A run looks at
//! the files once, before anything else does, and any difference lets go of all that's held for
//! the kit, which then goes the way a run with no daemon does. So does anything the daemon takes
//! back, or another process does, by the count of analyses found wrong.
//!
//! Only the daemon holds anything: a command's own process starts with nothing and ends soon.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use fennec_build::{KitFile, KitState};

/// The most kits held at once. Each is a few hundred bytes a file, so this is far under the
/// daemon's budget, which lets go of all of them if it's passed anyway (`kept::fit_budget`).
const MOST_KITS: usize = 64;

/// What's held for one kit, by its absolute path.
#[derive(Clone)]
struct Held {
    /// The path the run was given, which the program names every file by: a run given another
    /// path to the same kit is another program, as `image::kept_hash` makes it.
    given: String,
    /// The kit's files when what's held was found, before any was read.
    state: KitState,
    /// How many analyses the daemon had found wrong then, which takes back what was made before.
    found_wrong: u64,
    /// That its tests had passed with these files.
    tested: bool,
    /// What the lint found, as `lint::errors_remembered` gives it.
    linted: Option<String>,
    /// The kept image, by the hash it was written with, and the hash `image::kept` makes of it.
    image: Option<(fennec_blake3::Hash, String)>,
    /// The root a run of that image built, by its name in the run roots.
    root: Option<(String, String)>,
    /// When it was last used, to let go of the one used longest ago past `MOST_KITS`.
    used: u64,
}

/// Every kit held, once the daemon holds any.
static HELD: Mutex<Option<HashMap<String, Held>>> = Mutex::new(None);

/// The run in progress: its kit, and what's held for it if its files are the same.
static NOW: Mutex<Option<Now>> = Mutex::new(None);

/// How many runs have started from what was held, for `fennec daemon status`.
static ANSWERED: AtomicU64 = AtomicU64::new(0);

/// How many runs have begun, which orders when each kit was used.
static BEGUN: AtomicU64 = AtomicU64::new(0);

struct Now {
    /// The path the run was given, and the absolute one what's held goes by.
    given: String,
    absolute: String,
    /// The files as the run looked at them, first, with what's found for them.
    found: Held,
    /// Whether what was held for these same files is in `found`.
    same: bool,
}

/// Starts holding what runs find: the daemon calls it once, as it starts.
#[cfg(unix)]
pub(crate) fn hold() {
    let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
    held.get_or_insert_with(HashMap::new);
}

/// Looks at a kit's files once, as a run starts, and takes what's held for them if they're the same
/// as when it was found: otherwise lets go of it. What the run finds is noted with these files, and
/// held once it's done (`done`). Nothing is looked at outside the daemon.
pub(crate) fn begin(path: &str) {
    let mut now = NOW.lock().unwrap_or_else(PoisonError::into_inner);
    *now = None;
    let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(held) = held.as_mut() else { return };
    let absolute = crate::run::absolute(path);
    let found_wrong = crate::kept::analyses_found_wrong();
    let before = held.remove(&absolute);
    // The files are looked at by what was held, so a file that's settled isn't read, or by what the
    // last run noted, the first time.
    let hint = before.as_ref().map(|before| before.state.clone()).or_else(|| crate::image::noted(path));
    let Some(files) = files_now(path, hint.as_ref()) else { return };
    let state = state_of(&files);
    let used = BEGUN.fetch_add(1, Ordering::Relaxed);
    let same = before.as_ref().is_some_and(|before| {
        before.given == path
            && before.found_wrong == found_wrong
            && crate::verdict::same_files(&before.state, &files)
    });
    let given = path.to_owned();
    let found = match before {
        Some(before) if same => Held { state, used, ..before },
        _ => Held { given, state, found_wrong, tested: false, linted: None, image: None, root: None, used },
    };
    *now = Some(Now { given: path.to_owned(), absolute, found, same });
}

/// Holds what the run found, with the files it looked at first, and lets go of the kit used
/// longest ago past `MOST_KITS`. A run that noted no image holds nothing: its look at the files
/// may be missing what `embed` read, which only the cache's note of a load has, and the next run
/// looks by that note.
pub(crate) fn done() {
    let Some(now) = NOW.lock().unwrap_or_else(PoisonError::into_inner).take() else { return };
    if now.found.image.is_none() {
        return;
    }
    let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(held) = held.as_mut() else { return };
    held.insert(now.absolute, now.found);
    while held.len() > MOST_KITS {
        let oldest = held.iter().min_by_key(|(_, kit)| kit.used).map(|(path, _)| path.clone());
        let Some(oldest) = oldest else { break };
        held.remove(&oldest);
    }
}

/// Lets go of everything held: something was taken back, or it's past the daemon's budget.
pub(crate) fn forget_all() {
    if let Some(held) = HELD.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
        held.clear();
    }
    if let Some(now) = NOW.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
        now.same = false;
        now.found.tested = false;
        now.found.linted = None;
        now.found.image = None;
        now.found.root = None;
    }
}

/// Lets go of what's held for one kit, by its absolute path: its tests are being run again.
pub(crate) fn forget(absolute: &str) {
    if let Some(held) = HELD.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
        held.remove(absolute);
    }
    let mut now = NOW.lock().unwrap_or_else(PoisonError::into_inner);
    if now.as_ref().is_some_and(|now| now.absolute == absolute) {
        *now = None;
    }
}

/// What `fennec daemon status` says of it, once a kit is held.
#[cfg(unix)]
pub(crate) fn status() -> Option<String> {
    let held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
    let kits = held.as_ref().map_or(0, HashMap::len);
    let answered = ANSWERED.load(Ordering::Relaxed);
    if kits == 0 && answered == 0 {
        return None;
    }
    let kits = if kits == 1 { "1 kit".to_owned() } else { format!("{kits} kits") };
    let runs = if answered == 1 { "1 run".to_owned() } else { format!("{answered} runs") };
    Some(format!("runs held: {kits}, and {runs} started from what was held\n"))
}

/// Gives what's held of the run in progress, if it's for `path` and its files are the same.
fn held<T>(path: &str, take: impl FnOnce(&Held) -> Option<T>) -> Option<T> {
    let now = NOW.lock().unwrap_or_else(PoisonError::into_inner);
    now.as_ref().filter(|now| now.same && now.given == path).and_then(|now| take(&now.found))
}

/// Notes what the run found, if it found it with the files it looked at first: a file changed in
/// between means it's for files nothing looked at first, so it isn't held.
fn note(path: &str, files: Option<&[KitFile]>, give: impl FnOnce(&mut Held)) {
    let mut now = NOW.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(now) = now.as_mut().filter(|now| now.given == path) else { return };
    if files.is_none_or(|files| crate::verdict::same_files(&now.found.state, files)) {
        give(&mut now.found);
    }
}

/// Whether the kit's tests passed with these same files.
pub(crate) fn tested(path: &str) -> bool {
    held(path, |held| held.tested.then_some(())).is_some()
}

/// Notes that the kit's tests passed with `files`.
pub(crate) fn note_tested(path: &str, files: &[KitFile]) {
    note(path, Some(files), |held| held.tested = true);
}

/// What the lint found with these same files.
pub(crate) fn linted(path: &str) -> Option<String> {
    held(path, |held| held.linted.clone())
}

/// Notes what the lint found with a kit's `files`.
pub(crate) fn note_linted(path: &str, files: &[KitFile], shown: &str) {
    note(path, Some(files), |held| held.linted = Some(shown.to_owned()));
}

/// Notes what the lint found in a single file with this text, if it's the text the file had when
/// the run looked at it first: one edited in between was linted as it is now, not as it was.
pub(crate) fn note_linted_file(path: &str, text: fennec_blake3::Hash, shown: &str) {
    // The file is noted by its full path, as `verdict::loaded_files` notes it.
    let module = std::path::absolute(path).map_or_else(|_| path.to_owned(), |at| at.display().to_string());
    note(path, None, |held| {
        if held.state.files.get(&module).is_some_and(|file| file.text == text) {
            held.linted = Some(shown.to_owned());
        }
    });
}

/// The kept image of these same files, as `image::kept` gives it, which a run starting from it
/// counts as one started from what was held.
pub(crate) fn image(path: &str) -> Option<(fennec_blake3::Hash, String)> {
    let image = held(path, |held| held.image.clone())?;
    ANSWERED.fetch_add(1, Ordering::Relaxed);
    Some(image)
}

/// Notes the kept image `image::kept` found for `files`.
pub(crate) fn note_image(path: &str, files: &[KitFile], image: (fennec_blake3::Hash, String)) {
    note(path, Some(files), |held| held.image = Some(image));
}

/// The root a run of the kept image held built, by its name in the run roots.
pub(crate) fn root(path: &str, name: &str) -> Option<String> {
    held(path, |held| held.root.as_ref().filter(|(held, _)| held == name).map(|(_, root)| root.clone()))
}

/// Notes the root a run of the kept image built, or found ready, by its name. It goes with the
/// image, so it's held only with one.
pub(crate) fn note_root(path: &str, name: &str, root: &str) {
    note(path, None, |held| {
        if held.image.is_some() {
            held.root = Some((name.to_owned(), root.to_owned()));
        }
    });
}

/// A kit's files now, or a single file's, by what was noted of them, as `image::kept` looks at
/// them.
fn files_now(path: &str, hint: Option<&KitState>) -> Option<Vec<KitFile>> {
    if std::path::Path::new(path).is_dir() {
        crate::verdict::kit_files(path, hint)
    } else {
        crate::verdict::noted_files(hint?)
    }
}

/// Files as a state, noted now, so one changed too close to now to be told from a later edit is
/// read again next time.
fn state_of(files: &[KitFile]) -> KitState {
    let noted_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    KitState { files: files.iter().map(|file| (file.module.clone(), file.clone())).collect(), noted_at }
}

#[cfg(test)]
mod tests {
    use super::{Held, NOW, Now, PoisonError, linted, note_linted_file, state_of};

    /// A single file's lint is held only for the text the run looked at first: one found after the
    /// file was edited is for text the next run may not have, as when the edit is put back.
    #[test]
    fn a_single_files_lint_is_held_only_for_the_text_first_looked_at() {
        let path = "held-lint-test.fen";
        let module = std::path::absolute(path).unwrap().display().to_string();
        let first =
            fennec_build::KitFile { module, text: fennec_blake3::hash(b"first"), size: 5, changed_at: 1 };
        let found = Held {
            given: path.to_owned(),
            state: state_of(&[first]),
            found_wrong: 0,
            tested: false,
            linted: None,
            image: None,
            root: None,
            used: 0,
        };
        *NOW.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Now { given: path.to_owned(), absolute: path.to_owned(), found, same: true });
        note_linted_file(path, fennec_blake3::hash(b"edited"), "lint: of the edited text\n");
        assert_eq!(linted(path), None);
        note_linted_file(path, fennec_blake3::hash(b"first"), "lint: of the first text\n");
        assert_eq!(linted(path).as_deref(), Some("lint: of the first text\n"));
        *NOW.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}
