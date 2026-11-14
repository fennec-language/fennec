//! A kit's dependencies: kits from git, pinned to one commit, or from a folder on this machine.
//!
//! `kit.lock` pins each kit from git by the hash of its files, and nothing changes it but
//! `fennec update`. Fetching a kit runs nothing of it (see `git.rs`): a kit has no install or build
//! scripts. A fetched kit is kept whole in the store, `~/.fennec/kits/<hash>/`, written once and
//! renamed into place, so the network is only needed when a kit pinned isn't there yet.
//!
//! A kit from a path isn't pinned: it's work in progress, like an engine beside a game, and a pin
//! would break at every save. A kit with path dependencies can't be published to the Den, so they
//! never reach anyone else's tree.
//!
//! Dependencies of dependencies are followed, and `kit.lock` pins all of them, one kit to a name.

mod git;
mod lock;
pub mod manifest;
mod plain;

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use fennec_blake3::Hash;
pub use lock::{Pin, pins, with_pins};
pub use manifest::{Dependency, Manifest, Source, valid_name};
pub use plain::{plain, plain_text};

/// A kit's files, by their path from its folder, like `kit.toml` and `src/shapes.fen`.
pub type Files = BTreeMap<String, Vec<u8>>;

/// The hash a kit is pinned by: of each of its files' path and bytes, in order of path.
pub fn hash_of(files: &Files) -> Hash {
    let mut hasher = fennec_blake3::Hasher::new();
    hasher.update(b"fennec kit files 1\n");
    for (path, bytes) in files {
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    hasher.finalize()
}

/// Two of these paths that a Mac or Windows would take as one, as `src/A.fen` and `src/a.fen`, or
/// `src/Shapes/box.fen` and `src/shapes/ball.fen`, whose folders would merge: any two that differ
/// only by case, in the file's name or a folder's.
pub(crate) fn same_but_for_case<'p>(paths: impl Iterator<Item = &'p str>) -> Option<(String, String)> {
    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    for path in paths {
        // Each folder on the way, then the file.
        let ends = path.match_indices('/').map(|(at, _)| at).chain([path.len()]);
        for end in ends {
            let Some(part) = path.get(..end) else { continue };
            match seen.get(&part.to_lowercase()) {
                Some(had) if *had != part => return Some(((*had).to_owned(), part.to_owned())),
                Some(_) => {}
                None => {
                    seen.insert(part.to_lowercase(), part);
                }
            }
        }
    }
    None
}

/// A dependency, and where its files are on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The name it's imported by.
    pub name: String,
    /// Its folder: the one that holds its `kit.toml`.
    pub folder: PathBuf,
    /// What `kit.lock` pins it to, when it's from git.
    pub pin: Option<Pin>,
    /// The kits it names in its own `kit.toml`, which are the only ones it can import.
    pub uses: Vec<String>,
    /// What its own `kit.toml` says each kit it names gets: nothing for a line with no `gets`.
    pub grants: Vec<(String, Vec<String>)>,
}

/// The kits in the store, and whether fetching any that are missing is allowed.
#[derive(Clone, Debug)]
pub struct Kits {
    home: PathBuf,
    offline: bool,
    /// Whether a kit fetched from git may name another by `file://`, which only fennec-deps' own
    /// tests ask for: see `with_local_git_chains`.
    local_git_chains: bool,
    /// How many bytes a kit fetched from git may come to: `MOST_BYTES`, but for tests.
    most_bytes: u64,
    /// Whether this is for loading a program, which never fetches: only `fennec fetch` and
    /// `fennec update` reach the network. See `for_loading`.
    loading: bool,
}

/// What the store holds for a pin.
enum Kept {
    /// Nothing.
    Absent,
    /// The kit, whole: its files hash to the pin.
    Whole,
    /// A folder whose files don't hash to the pin: changed since it was fetched.
    Changed,
}

/// How many bytes a kit's files fetched from git may come to, all told: far more than any kit's
/// source, and little enough that reading them can't take the machine's memory.
pub const MOST_BYTES: u64 = 256 << 20;

/// Finds a kit from git, by its name, address, commit and folder: its pin and its folder in the
/// store.
type Resolve<'r> = &'r mut dyn FnMut(&str, &str, &str, Option<&str>) -> Result<(Pin, PathBuf), String>;

/// How many fetches this process has made, for naming each one's scratch folder.
static FETCHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Who named a dependency, for following theirs: the kit's folder, its name for messages, and
/// whether it came from git, where a path means nothing.
struct Named {
    folder: PathBuf,
    by: String,
    from_git: bool,
}

impl Kits {
    /// The store in the Fennec folder `home`. Offline, a kit that isn't there is an error rather
    /// than fetched.
    pub fn new(home: &Path, offline: bool) -> Self {
        Kits {
            home: home.to_path_buf(),
            offline,
            local_git_chains: false,
            most_bytes: MOST_BYTES,
            loading: false,
        }
    }

    /// The store in this machine's Fennec folder, which fetches nothing with `FENNEC_OFFLINE` set
    /// (as the launcher sets it for `--offline`). None with no home folder to keep it in.
    pub fn of_this_machine() -> Option<Self> {
        let home = fennec_build::fennec_home()?;
        Some(Kits::new(&home, std::env::var_os("FENNEC_OFFLINE").is_some()))
    }

    /// This store, letting a kit fetched from git name another by `file://`. Only fennec-deps' own
    /// tests use it: a test can't serve a repository over https or ssh, and without it nothing
    /// would test pinning a git kit's own git dependency. It's a call, not a setting or a Cargo
    /// feature, so nothing a user or a build sets can turn it on: `fennec` never makes this call,
    /// and a feature turned on for tests would reach the `fennec` its CLI tests run too.
    #[must_use]
    pub fn with_local_git_chains(mut self) -> Self {
        self.local_git_chains = true;
        self
    }

    /// This store, for loading a program: it never fetches. Opening a project, checking it or
    /// running it reaches no host the repository names; `fennec fetch` gets what `kit.lock` pins,
    /// and `fennec update` moves the pins.
    #[must_use]
    pub fn for_loading(mut self) -> Self {
        self.loading = true;
        self
    }

    /// What the store holds for the kit with this hash, its files hashed as they are now: a kit
    /// changed in the store, by a tool or by hand, isn't the one pinned.
    fn kept(&self, hash: &Hash) -> Kept {
        let at = self.stored(hash);
        if !at.is_dir() {
            return Kept::Absent;
        }
        match read_files(&at) {
            Some(files) if hash_of(&files) == *hash => Kept::Whole,
            _ => Kept::Changed,
        }
    }

    /// Takes a kit that changed in the store out of it, so it's fetched again whole.
    fn take_out(&self, hash: &Hash) -> Result<(), String> {
        let at = self.stored(hash);
        std::fs::remove_dir_all(&at)
            .map_err(|err| format!("couldn't take {} out of the store: {err}", at.display()))
    }

    /// This store, taking kits from git of at most `most` bytes, for tests that can't commit
    /// `MOST_BYTES` of files. Like `with_local_git_chains`, it's a call, which `fennec` never makes.
    #[must_use]
    pub fn with_most_bytes(mut self, most: u64) -> Self {
        self.most_bytes = most;
        self
    }

    /// Where a kit with these files' hash is kept.
    pub fn stored(&self, hash: &Hash) -> PathBuf {
        self.home.join("kits").join(hash.to_hex())
    }

    /// Every dependency of the kit at `root`, as `kit.lock` pins them, fetching any pinned kit
    /// that isn't in the store yet.
    ///
    /// # Errors
    ///
    /// When `kit.toml` names a kit `kit.lock` doesn't pin, or pins elsewhere, which `fennec update`
    /// fixes, and when a kit fetched isn't the one pinned.
    pub fn ensure(&self, root: &Path) -> Result<Vec<Found>, String> {
        let pinned = pins(&std::fs::read_to_string(root.join("kit.lock")).unwrap_or_default())?;
        walk(root, self.local_git_chains, &mut |name, url, commit, folder| {
            let wanted = lock::shown(url, commit, folder);
            let pin = pinned.iter().find(|pin| pin.name == name).ok_or_else(|| {
                format!("kit.toml names `{name}` from {wanted}, which kit.lock doesn't pin yet: `fennec update` fetches it and pins it")
            })?;
            if !pin.is(url, commit, folder) {
                return Err(format!(
                    "kit.toml asks for `{name}` from {wanted}, and kit.lock pins {}: `fennec update` moves the pin",
                    pin.shown()
                ));
            }
            let at = self.stored(&pin.hash);
            let kept = self.kept(&pin.hash);
            if matches!(kept, Kept::Changed) {
                if self.loading || self.offline {
                    return Err(format!(
                        "the kit `{name}` kept at {} isn't what kit.lock pins: its files changed after it was fetched, so it isn't built; `fennec fetch` fetches it again",
                        at.display()
                    ));
                }
                self.take_out(&pin.hash)?;
            }
            if !matches!(kept, Kept::Whole) {
                let files = self.fetch(name, url, commit, folder)?;
                let hash = hash_of(&files);
                if hash != pin.hash {
                    return Err(format!(
                        "what {wanted} has for `{name}` isn't what kit.lock pinned: its files' hash is {}, and kit.lock's is {}; the repository's history may have been changed",
                        hash.to_hex(),
                        pin.hash.to_hex()
                    ));
                }
                self.store(&hash, &files)?;
            }
            Ok((pin.clone(), at))
        })
    }

    /// `fennec update`: pins every dependency from git to the commit `kit.toml` names, fetching
    /// the ones not fetched yet, and writes `kit.lock`. Gives back what changed, with each file a
    /// moved pin changes.
    ///
    /// # Errors
    ///
    /// When a kit can't be fetched or read, or `kit.lock` can't be written.
    pub fn update(&self, root: &Path) -> Result<String, String> {
        let lock_path = root.join("kit.lock");
        let lock_text = std::fs::read_to_string(&lock_path).unwrap_or_default();
        let before = pins(&lock_text)?;
        let found = walk(root, self.local_git_chains, &mut |name, url, commit, folder| {
            if let Some(pin) = before.iter().find(|pin| pin.name == name && pin.is(url, commit, folder)) {
                match self.kept(&pin.hash) {
                    Kept::Whole => return Ok((pin.clone(), self.stored(&pin.hash))),
                    Kept::Changed => self.take_out(&pin.hash)?,
                    Kept::Absent => {}
                }
            }
            let files = self.fetch(name, url, commit, folder)?;
            let hash = hash_of(&files);
            let at = self.store(&hash, &files)?;
            let pin = Pin {
                name: name.to_owned(),
                hash,
                url: url.to_owned(),
                commit: commit.to_owned(),
                folder: folder.map(str::to_owned),
            };
            Ok((pin, at))
        })?;
        let after: Vec<Pin> = found.into_iter().filter_map(|found| found.pin).collect();
        let mut said = String::new();
        for pin in &after {
            match before.iter().find(|old| old.name == pin.name) {
                None => {
                    let count =
                        read_files(&self.stored(&pin.hash)).map(|files| files.len()).unwrap_or_default();
                    {
                        let _ = writeln!(
                            said,
                            "kit.lock: `{}` pinned to {}, {count} files",
                            pin.name,
                            pin.shown()
                        );
                    };
                }
                Some(old) if old == pin => {}
                Some(old) => said.push_str(&self.moved(old, pin)),
            }
        }
        for old in before.iter().filter(|old| !after.iter().any(|pin| pin.name == old.name)) {
            {
                let _ = writeln!(
                    said,
                    "kit.lock: `{}` isn't a dependency any more, so it's no longer pinned",
                    old.name
                );
            };
        }
        if said.is_empty() {
            return Ok(if after.is_empty() {
                "there are no dependencies from git to pin\n".to_owned()
            } else {
                "kit.lock pins every dependency already\n".to_owned()
            });
        }
        fennec_dist::write_kit_file(&lock_path, &with_pins(&lock_text, &after))?;
        Ok(said)
    }

    /// What moving a pin changes: a line for the pin, then one for each file.
    fn moved(&self, old: &Pin, new: &Pin) -> String {
        let mut said = format!("kit.lock: `{}` {} → {}\n", new.name, old.shown(), new.shown());
        let (Some(before), Some(after)) =
            (read_files(&self.stored(&old.hash)), read_files(&self.stored(&new.hash)))
        else {
            return said;
        };
        for (path, bytes) in &after {
            match before.get(path) {
                None => {
                    let _ = writeln!(said, "  added {path}");
                }
                Some(was) if was != bytes => {
                    let _ = writeln!(said, "  changed {path}");
                }
                Some(_) => {}
            }
        }
        for path in before.keys().filter(|path| !after.contains_key(*path)) {
            {
                let _ = writeln!(said, "  removed {path}");
            };
        }
        said
    }

    /// Fetches a kit from git, unless fetching is off.
    fn fetch(&self, name: &str, url: &str, commit: &str, folder: Option<&str>) -> Result<Files, String> {
        let place = lock::shown(url, commit, folder);
        if self.loading {
            return Err(format!(
                "`{name}` from {place} isn't on this machine yet: `fennec fetch` fetches every kit kit.lock pins"
            ));
        }
        if self.offline {
            return Err(format!(
                "`{name}` from {place} isn't on this machine yet, and fetching is off (`--offline` or FENNEC_OFFLINE)"
            ));
        }
        let count = FETCHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let work = self.home.join("work").join(format!("fetch-{}-{count}", std::process::id()));
        git::fetch(url, commit, folder, &work, self.most_bytes)
            .map_err(|err| format!("couldn't fetch `{name}`: {err}"))
    }

    /// Keeps a kit's files in the store, written whole and renamed into place, giving back its
    /// folder there.
    fn store(&self, hash: &Hash, files: &Files) -> Result<PathBuf, String> {
        let at = self.stored(hash);
        match self.kept(hash) {
            Kept::Whole => return Ok(at),
            Kept::Changed => self.take_out(hash)?,
            Kept::Absent => {}
        }
        if let Some((one, other)) = same_but_for_case(files.keys().map(String::as_str)) {
            return Err(format!("a kit can't have both {one} and {other}, which differ only by case"));
        }
        let partial = at.with_extension(format!("partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&partial);
        for (path, bytes) in files {
            let relative = Path::new(path);
            if !relative.components().all(|part| matches!(part, Component::Normal(_))) {
                return Err(format!("a kit's file can't be at `{path}`"));
            }
            let file = partial.join(relative);
            if let Some(folder) = file.parent() {
                std::fs::create_dir_all(folder)
                    .map_err(|err| format!("couldn't make {}: {err}", folder.display()))?;
            }
            std::fs::write(&file, bytes)
                .map_err(|err| format!("couldn't write {}: {err}", file.display()))?;
        }
        match std::fs::rename(&partial, &at) {
            Ok(()) => Ok(at),
            // Another command stored the same kit first, which has the same files.
            Err(_) if at.is_dir() => {
                let _ = std::fs::remove_dir_all(&partial);
                Ok(at)
            }
            Err(err) => Err(format!("couldn't keep the kit in {}: {err}", at.display())),
        }
    }
}

/// Follows the dependencies of the kit at `root`, and theirs, giving each once. `resolve` finds
/// a kit from git: its pin and its folder in the store.
///
/// What the user's own disk holds may name what else is on it: the kit at `root`, and a kit it
/// names by a path, and theirs. A kit fetched from git, and everything it names, may name neither
/// a path nor a `file://` repository: one could carry a bare repository and name it, or point at
/// any repository of the user's, and have it fetched into the build. `local_git_chains` lets a
/// `file://` through for fennec-deps' own tests.
fn walk(root: &Path, local_git_chains: bool, resolve: Resolve<'_>) -> Result<Vec<Found>, String> {
    let mut found: Vec<(Found, String)> = Vec::new();
    let mut queue: VecDeque<(Dependency, Named)> = VecDeque::new();
    let first = Named { folder: root.to_path_buf(), by: "this kit".to_owned(), from_git: false };
    follow(&first, &manifest_of(root)?, &mut queue);
    while let Some((dependency, named)) = queue.pop_front() {
        let name = dependency.name.as_str();
        let (place, folder, pin) = match &dependency.source {
            Source::Git { url, commit, folder } => {
                if named.from_git && url.starts_with("file://") && !local_git_chains {
                    return Err(format!(
                        "{} is from git, and names `{name}` at `{url}`, a repository on this machine: a kit fetched from git names others only by `https://`, `ssh://` or `git@host:path`",
                        named.by
                    ));
                }
                let place = lock::shown(url, commit, folder.as_deref());
                if seen(&found, name, &place)? {
                    continue;
                }
                let (pin, at) = resolve(name, url, commit, folder.as_deref())?;
                (place, at, Some(pin))
            }
            Source::Path(path) => {
                if named.from_git {
                    return Err(format!(
                        "`{name}` comes from the path `{path}`, and {} that names it is from git, where a path means nothing",
                        named.by
                    ));
                }
                let folder = named.folder.join(path);
                let folder = folder.canonicalize().map(|found| plain(&found)).map_err(|err| {
                    format!("{} names `{name}` at {}, which isn't there: {err}", named.by, folder.display())
                })?;
                let place = folder.display().to_string();
                if seen(&found, name, &place)? {
                    continue;
                }
                (place, folder, None)
            }
        };
        let manifest =
            manifest_of(&folder).map_err(|err| format!("`{name}`, which {} names: {err}", named.by))?;
        let by = format!("`{name}`");
        follow(&Named { folder: folder.clone(), by, from_git: pin.is_some() }, &manifest, &mut queue);
        let uses = manifest.dependencies.iter().map(|used| used.name.clone()).collect();
        let grants = grants_of(&manifest);
        found.push((Found { name: name.to_owned(), folder, pin, uses, grants }, place));
    }
    Ok(found.into_iter().map(|(found, _)| found).collect())
}

/// What a kit's `kit.toml` says each kit it names gets, by each line: nothing for a line with no
/// `gets`.
#[must_use]
pub fn grants_of(manifest: &Manifest) -> Vec<(String, Vec<String>)> {
    manifest
        .dependencies
        .iter()
        .map(|dependency| (dependency.name.clone(), dependency.gets.clone()))
        .collect()
}

/// Puts the dependencies a kit names on the queue.
fn follow(named: &Named, manifest: &Manifest, queue: &mut VecDeque<(Dependency, Named)>) {
    for dependency in &manifest.dependencies {
        let copy = Named { folder: named.folder.clone(), by: named.by.clone(), from_git: named.from_git };
        queue.push_back((dependency.clone(), copy));
    }
}

/// Whether a kit named `name` was found already, from `place`. One from elsewhere is an error.
fn seen(found: &[(Found, String)], name: &str, place: &str) -> Result<bool, String> {
    match found.iter().find(|(found, _)| found.name == name) {
        Some((_, had)) if had == place => Ok(true),
        Some((_, had)) => Err(format!(
            "two kits are named `{name}`: one from {had}, and one from {place}; a kit's dependencies can have only one kit of a name"
        )),
        None => Ok(false),
    }
}

/// The kit.toml in `folder`.
fn manifest_of(folder: &Path) -> Result<Manifest, String> {
    let path = folder.join("kit.toml");
    let text =
        std::fs::read_to_string(&path).map_err(|err| format!("couldn't read {}: {err}", path.display()))?;
    Manifest::parse(&text).map_err(|errors| {
        let said: Vec<String> = errors.into_iter().map(|error| error.message).collect();
        format!("{}: {}", path.display(), said.join("; "))
    })
}

/// A kit's files in a folder of the store, by their path from it.
fn read_files(folder: &Path) -> Option<Files> {
    fn walk(folder: &Path, prefix: &str, files: &mut Files) -> Option<()> {
        for item in std::fs::read_dir(folder).ok()?.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            if item.file_type().ok()?.is_dir() {
                walk(&item.path(), &path, files)?;
            } else {
                files.insert(path, std::fs::read(item.path()).ok()?);
            }
        }
        Some(())
    }
    let mut files = Files::new();
    walk(folder, "", &mut files)?;
    Some(files)
}
