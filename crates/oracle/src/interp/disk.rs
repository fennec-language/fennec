//! The file system an `Fs` reaches, as the oracle models it. It starts as the folder the program
//! runs in was when the program started, and what the program writes, makes and removes after
//! that is held here, in memory: the disk itself is never written.
//!
//! The folder is a fresh empty one, as `compare` gives the compiled program and the tests give
//! the oracle, or the one the oracle runs in, as the gate runs the examples from the checkout's
//! root. In that one, the oracle looks at the disk to learn what was there before, one name at a
//! time, never through a link: only whether a file or a folder is at a path. A file's text and
//! size, a folder's listing and anything's time are never read, so anything that needs them is
//! refused. A path outside the folder is refused too, but for the folders on the way to it, which
//! are there.
//!
//! What a program would see differently from one system to another is refused: a name beside
//! another that differs from it only in case, which some systems take as the same and some don't,
//! a folder's listing while a file is being made in it, which shows under a hidden name holding
//! the process's number, and on Windows the paths and names it reads its own way and the failures
//! it gives in its own words.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf, Prefix};

/// The longest name the oracle models, in bytes: the most every file system it runs on allows.
const LONGEST_NAME: usize = 255;

/// The longest path the oracle models, in bytes, well inside what every system allows.
const LONGEST_PATH: usize = 1000;

/// The longest whole path the oracle models on Windows, in characters, inside its old limit.
const LONGEST_ON_WINDOWS: usize = 240;

/// The most bytes the files a program writes may hold between them.
const MOST_BYTES: usize = 64 << 20;

/// The folder a program runs in, which its relative paths start from, and whether it's empty
/// when the program starts.
#[derive(Clone, Debug)]
pub struct Workdir {
    home: Option<Home>,
}

impl Workdir {
    /// The folder the oracle runs in, as it is: what's in it is looked at, never changed.
    pub fn current() -> Workdir {
        let home = std::env::current_dir().ok().and_then(|folder| home_of(&folder, false));
        Workdir { home }
    }

    /// A folder at `path`, which has to be absolute, that's empty when the program starts, as a
    /// fresh temporary one is. The disk is never looked at for what's in it.
    pub fn empty(path: &Path) -> Workdir {
        Workdir { home: home_of(path, true) }
    }
}

/// The folder a program runs in: its path's parts below the root, with the drive in front on
/// Windows.
#[derive(Clone, Debug)]
struct Home {
    front: String,
    parts: Vec<String>,
    empty: bool,
}

/// An absolute path as a home: `None` for one that's relative, or isn't text, or is on Windows
/// but not on a drive.
fn home_of(path: &Path, empty: bool) -> Option<Home> {
    let mut front = String::new();
    let mut parts = Vec::new();
    let mut rooted = false;
    for part in path.components() {
        match part {
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                    front = format!("{}:", char::from(drive));
                }
                _ => return None,
            },
            Component::RootDir => rooted = true,
            Component::CurDir => {}
            Component::ParentDir => return None,
            Component::Normal(name) => parts.push(name.to_str()?.to_owned()),
        }
    }
    rooted.then_some(Home { front, parts, empty })
}

/// What's at a path.
#[derive(Clone, Debug)]
enum Entry {
    /// A file the program wrote, with its text.
    File(String),
    /// A file that was there before the program started, whose text the oracle never reads.
    Found,
    /// A folder, and whether everything in it is known here: for one the program made, or a
    /// fresh empty one it runs in.
    Folder { listed: bool },
    /// Nothing: it was never there, or the program removed it.
    Gone,
}

/// A file `fs.create` or `fs.open` has lent, by its handle.
#[derive(Debug)]
enum Open {
    /// Being written, to take `target`'s place when its block ends. Until then it's a hidden file
    /// beside `target`.
    Writing { target: Vec<String>, text: String },
    /// Being read, from what the file held when it was opened: `at` is how far in, in bytes.
    Reading { path: Vec<String>, text: String, at: usize },
}

/// Why a call on the file system didn't do what it was asked.
#[derive(Debug)]
pub(crate) enum Fault {
    /// It fails, with this case of `IoError`, saying this.
    Fails(&'static str, String),
    /// It reaches something the oracle doesn't model, named.
    Refused(String),
}

pub(crate) type Done<T> = Result<T, Fault>;

fn fails<T>(case: &'static str, reason: &str) -> Done<T> {
    Err(Fault::Fails(case, reason.to_owned()))
}

fn refused<T>(why: impl Into<String>) -> Done<T> {
    Err(Fault::Refused(why.into()))
}

fn nothing_there<T>() -> Done<T> {
    fails("NotFound", "nothing is there")
}

fn a_folder<T>() -> Done<T> {
    fails("Other", "it's a folder, not a file")
}

/// A path through a file, as `note.txt/x`: Unix says it's not a folder, and Windows that the
/// path isn't there, so the oracle refuses it there.
fn through_a_file<T>() -> Done<T> {
    if cfg!(windows) {
        return refused("a path through a file, which Windows says isn't there");
    }
    fails("Other", "part of the path is a file, not a folder")
}

/// Where a path leads, once each folder on the way to it is found.
#[derive(Debug)]
pub(crate) enum Reached {
    /// The folder at the end of the way, and the name in it the path ends with, or `None` for a
    /// path that ends with the folder itself, as `.`, `..` and a last `/` do.
    At(Vec<String>, Option<String>),
    /// Something on the way isn't there.
    Missing,
    /// Something on the way is a file.
    ThroughFile,
}

/// What `fs.metadata` finds: a file's size, when the oracle knows it, and what it is.
pub(crate) struct Metadata {
    pub(crate) size: Option<usize>,
    pub(crate) is_dir: bool,
}

/// The file system, as it is while a program runs.
#[derive(Debug)]
pub(crate) struct Disk {
    home: Option<Home>,
    /// What's at each path the program has reached, by the parts of its absolute path.
    known: BTreeMap<Vec<String>, Entry>,
    open: BTreeMap<i64, Open>,
    /// The handle the next file lent gets: the first is 1, as the runtime counts them.
    next_handle: i64,
    /// How many bytes the files the program wrote hold between them, the ones being made too.
    bytes: usize,
}

impl Disk {
    pub(crate) fn new(workdir: &Workdir) -> Disk {
        let mut known = BTreeMap::new();
        if let Some(home) = &workdir.home {
            // The folders on the way to it are there, and nothing in them is known.
            for at in 0..home.parts.len() {
                let above = home.parts.get(..at).unwrap_or_default().to_vec();
                known.insert(above, Entry::Folder { listed: false });
            }
            known.insert(home.parts.clone(), Entry::Folder { listed: home.empty });
        }
        Disk { home: workdir.home.clone(), known, open: BTreeMap::new(), next_handle: 1, bytes: 0 }
    }

    fn home(&self) -> Done<&Home> {
        self.home.as_ref().map_or_else(|| refused("a folder to run in that couldn't be found"), Ok)
    }

    /// An absolute path as a message shows it: `/home/a`, or `C:/Users/a` on Windows.
    pub(crate) fn shown(&self, parts: &[String]) -> String {
        let front = self.home.as_ref().map_or("", |home| home.front.as_str());
        format!("{front}/{}", parts.join("/"))
    }

    /// A path made absolute against the folder the program runs in, `.` and `..` worked out from
    /// its text alone, as a narrowed `Fs` reads a path. On Windows, a narrowed `Fs` refuses a name
    /// it reads its own way before anything else, in words of its own, so the oracle refuses one.
    pub(crate) fn absolute(&self, text: &str) -> Done<Vec<String>> {
        odd_on_windows(text)?;
        for part in text.split('/').filter(|part| !matches!(*part, "" | "." | "..")) {
            checked_name(part)?;
        }
        let mut parts = if text.starts_with('/') { Vec::new() } else { self.home()?.parts.clone() };
        for part in text.split('/') {
            match part {
                "" | "." => {}
                ".." => drop(parts.pop()),
                name => parts.push(name.to_owned()),
            }
        }
        Ok(parts)
    }

    /// Where a path leads through the whole file system, as the system goes: from the folder
    /// the program runs in, or from the root for a path starting with `/`, a `..` going back up,
    /// and every name on the way a folder.
    pub(crate) fn whole(&mut self, text: &str) -> Done<Reached> {
        if text.is_empty() {
            return refused("an empty path");
        }
        if text.len() > LONGEST_PATH {
            return refused("a path longer than the oracle models");
        }
        odd_on_windows(text)?;
        let (start, rest) = match text.strip_prefix('/') {
            Some(rest) => (Vec::new(), rest),
            None => (self.home()?.parts.clone(), text),
        };
        let mut steps: Vec<&str> = rest.split('/').collect();
        let last = steps.pop().unwrap_or_default();
        let name = match last {
            "" | "." | ".." => {
                steps.push(last);
                None
            }
            name => Some(name.to_owned()),
        };
        Ok(match self.walk(start, &steps)? {
            Walked::To(folder) => Reached::At(folder, name),
            Walked::Missing => Reached::Missing,
            Walked::ThroughFile => Reached::ThroughFile,
        })
    }

    /// Where a path leads beneath the folder a narrowed `Fs` reaches, by its parts there. The
    /// runtime opens the folder before anything else, so every call fails where it isn't there,
    /// even asking whether something exists.
    pub(crate) fn beneath(&mut self, folder: &[String], inside: &[String]) -> Done<Reached> {
        self.open_folder(folder)?;
        let Some((name, way)) = inside.split_last() else {
            return Ok(Reached::At(folder.to_vec(), None));
        };
        let way: Vec<&str> = way.iter().map(String::as_str).collect();
        Ok(match self.walk(folder.to_vec(), &way)? {
            Walked::To(at) => Reached::At(at, Some(name.clone())),
            Walked::Missing => Reached::Missing,
            Walked::ThroughFile => Reached::ThroughFile,
        })
    }

    /// Finds the folder a narrowed `Fs` reaches, failing where it isn't there or isn't a folder.
    fn open_folder(&mut self, folder: &[String]) -> Done<()> {
        let names: Vec<&str> = folder.iter().map(String::as_str).collect();
        match self.walk(Vec::new(), &names)? {
            Walked::To(_) => Ok(()),
            Walked::Missing => nothing_there(),
            Walked::ThroughFile => through_a_file(),
        }
    }

    /// Goes from `start` a step at a time: an empty step or `.` stays, `..` goes back up, and a
    /// name goes into the folder it names.
    fn walk(&mut self, start: Vec<String>, steps: &[&str]) -> Done<Walked> {
        let mut at = start;
        for step in steps {
            match *step {
                "" | "." => {}
                ".." => drop(at.pop()),
                name => {
                    let inside = joined(&at, name);
                    match self.kind(&inside)? {
                        Entry::Folder { .. } => at = inside,
                        Entry::Gone => return Ok(Walked::Missing),
                        Entry::File(_) | Entry::Found => return Ok(Walked::ThroughFile),
                    }
                }
            }
        }
        Ok(Walked::To(at))
    }

    /// What's at an absolute path: what the program left there, or what was there before.
    fn kind(&mut self, parts: &[String]) -> Done<Entry> {
        if let Some(entry) = self.known.get(parts) {
            return Ok(entry.clone());
        }
        let Some((name, folder)) = parts.split_last() else { return refused("the root") };
        checked_name(name)?;
        if cfg!(windows) && self.shown(parts).chars().count() > LONGEST_ON_WINDOWS {
            return refused("a path longer than Windows allows unless it's told to");
        }
        let home = self.home()?;
        if parts.len() <= home.parts.len() || !parts.starts_with(&home.parts) {
            return refused("a path outside the folder the program runs in");
        }
        let (empty, front) = (home.empty, home.front.clone());
        self.no_other_case(folder, name)?;
        let listed = matches!(self.known.get(folder), Some(Entry::Folder { listed: true }));
        if listed || empty {
            return Ok(Entry::Gone);
        }
        let found = looked_at(&front, parts)?;
        self.known.insert(parts.to_vec(), found.clone());
        Ok(found)
    }

    /// Refuses a name beside another that differs from it only in case, or in characters past
    /// ASCII, which macOS and Windows can take as the same name and Linux doesn't.
    fn no_other_case(&self, folder: &[String], name: &str) -> Done<()> {
        for (path, _) in self.children(folder) {
            if let Some(other) = path.last()
                && other != name
                && (other.eq_ignore_ascii_case(name) || (!other.is_ascii() && !name.is_ascii()))
            {
                return refused(format!(
                    "the name `{name}` beside `{other}`, which some systems take as the same name"
                ));
            }
        }
        Ok(())
    }

    /// What's known in a folder, by path.
    fn children<'d>(&'d self, folder: &'d [String]) -> impl Iterator<Item = (&'d Vec<String>, &'d Entry)> {
        self.known
            .range(folder.to_vec()..)
            .take_while(move |(path, _)| path.starts_with(folder))
            .filter(move |(path, _)| path.len() == folder.len() + 1)
    }

    /// Refuses changing anything but what's inside the folder the program runs in.
    fn changing(&self, parts: &[String]) -> Done<()> {
        let home = self.home()?;
        if parts.len() <= home.parts.len() || !parts.starts_with(&home.parts) {
            return refused("changing something outside the folder the program runs in");
        }
        Ok(())
    }

    /// Refuses changing a file that's open to read on Windows, which won't replace or remove one.
    fn not_busy(&self, parts: &[String]) -> Done<()> {
        let busy = self.open.values().any(|open| matches!(open, Open::Reading { path, .. } if path == parts));
        if cfg!(windows) && busy {
            return refused("changing a file that's open to read, which Windows won't do");
        }
        Ok(())
    }

    /// Puts a file's new text at a path, counting what the files hold.
    fn put(&mut self, parts: Vec<String>, text: String) -> Done<()> {
        let old = match self.known.get(&parts) {
            Some(Entry::File(old)) => old.len(),
            _ => 0,
        };
        let bytes = self.bytes.saturating_sub(old).saturating_add(text.len());
        if bytes > MOST_BYTES {
            return refused("ran too long: it wrote too much");
        }
        self.bytes = bytes;
        self.known.insert(parts, Entry::File(text));
        Ok(())
    }

    /// Takes away what's at a path: forgotten where everything in its folder is known, and
    /// noted as gone where the disk would still show it.
    fn take_away(&mut self, parts: Vec<String>) {
        if let Some(Entry::File(old)) = self.known.get(&parts) {
            self.bytes = self.bytes.saturating_sub(old.len());
        }
        let listed = parts.split_last().is_some_and(|(_, folder)| {
            matches!(self.known.get(folder), Some(Entry::Folder { listed: true }))
        });
        if listed {
            self.known.retain(|path, _| !path.starts_with(&parts));
        } else {
            self.known.insert(parts, Entry::Gone);
        }
    }

    /// What's at the end of a path, with its absolute path, failing as the system does where
    /// something on the way isn't there or is a file.
    fn what_is(&mut self, reached: Reached) -> Done<(Vec<String>, Entry)> {
        match reached {
            Reached::Missing => nothing_there(),
            Reached::ThroughFile => through_a_file(),
            Reached::At(folder, None) => {
                let entry = self.kind(&folder)?;
                Ok((folder, entry))
            }
            Reached::At(folder, Some(name)) => {
                let path = joined(&folder, &name);
                let entry = self.kind(&path)?;
                Ok((path, entry))
            }
        }
    }

    /// The path a call that changes something reaches, which has to end with a name.
    fn named(reached: Reached, doing: &str) -> Done<Vec<String>> {
        match reached {
            Reached::At(folder, Some(name)) => Ok(joined(&folder, &name)),
            Reached::At(_, None) => refused(format!("{doing} a path that ends with a folder")),
            Reached::Missing => nothing_there(),
            Reached::ThroughFile => through_a_file(),
        }
    }

    pub(crate) fn exists(&mut self, reached: Reached) -> Done<bool> {
        match reached {
            Reached::Missing => Ok(false),
            other => match self.what_is(other)? {
                (_, Entry::Gone) => Ok(false),
                _ => Ok(true),
            },
        }
    }

    pub(crate) fn read_text(&mut self, reached: Reached) -> Done<String> {
        match self.what_is(reached)? {
            (_, Entry::File(text)) => Ok(text),
            (_, Entry::Found) => refused("reading a file that was there before the program started"),
            (_, Entry::Folder { .. }) => a_folder(),
            (_, Entry::Gone) => nothing_there(),
        }
    }

    /// Writes a whole file, which takes the old one's place at once.
    pub(crate) fn write_text(&mut self, reached: Reached, text: &str) -> Done<()> {
        let target = Self::named(reached, "writing to")?;
        if let Entry::Folder { .. } = self.kind(&target)? {
            if cfg!(windows) {
                return refused("writing to a folder, which Windows refuses in its own words");
            }
            return a_folder();
        }
        self.changing(&target)?;
        self.not_busy(&target)?;
        self.put(target, text.to_owned())
    }

    /// Makes a folder and every folder on the way to it, through the whole file system.
    pub(crate) fn make_whole(&mut self, text: &str) -> Done<()> {
        if text.is_empty() || text.len() > LONGEST_PATH {
            return refused("making a folder by an empty path, or one longer than the oracle models");
        }
        odd_on_windows(text)?;
        let (start, rest) = match text.strip_prefix('/') {
            Some(rest) => (Vec::new(), rest),
            None => (self.home()?.parts.clone(), text),
        };
        let steps: Vec<&str> = rest.split('/').collect();
        self.make_dir(start, &steps)
    }

    /// Makes a folder and every folder on the way to it, beneath the folder a narrowed `Fs`
    /// reaches, which has to be there first.
    pub(crate) fn make_beneath(&mut self, folder: &[String], inside: &[String]) -> Done<()> {
        self.open_folder(folder)?;
        let steps: Vec<&str> = inside.iter().map(String::as_str).collect();
        self.make_dir(folder.to_vec(), &steps)
    }

    /// Makes a folder and every folder on the way to it, from `start` by `steps`, as
    /// `create_dir_all` does: a file on the way fails before anything's made, a file at the end
    /// is there already, and a folder at the end is fine.
    fn make_dir(&mut self, start: Vec<String>, steps: &[&str]) -> Done<()> {
        let names: Vec<&str> = steps.iter().copied().filter(|step| !matches!(*step, "" | ".")).collect();
        if names.is_empty()
            || names.contains(&"..")
            || steps.last().is_some_and(|last| matches!(*last, "" | "."))
        {
            return refused("making a folder by a path with `..`, or that ends with `.` or `/`");
        }
        let mut at = start;
        let mut first_missing = None;
        for (count, name) in names.iter().enumerate() {
            let inside = joined(&at, name);
            match self.kind(&inside)? {
                Entry::Folder { .. } => at = inside,
                Entry::Gone => {
                    first_missing = Some(count);
                    break;
                }
                Entry::File(_) | Entry::Found if count + 1 == names.len() => {
                    return fails("AlreadyExists", "something is there already");
                }
                Entry::File(_) | Entry::Found => return through_a_file(),
            }
        }
        let Some(first_missing) = first_missing else { return Ok(()) };
        for name in names.iter().skip(first_missing) {
            let inside = joined(&at, name);
            // Looking first checks the name against those beside it.
            self.kind(&inside)?;
            self.changing(&inside)?;
            self.known.insert(inside.clone(), Entry::Folder { listed: true });
            at = inside;
        }
        Ok(())
    }

    /// Removes a file, or a folder that's empty.
    pub(crate) fn remove(&mut self, reached: Reached) -> Done<()> {
        let target = Self::named(reached, "removing")?;
        match self.kind(&target)? {
            Entry::Gone => nothing_there(),
            Entry::File(_) | Entry::Found => {
                self.changing(&target)?;
                self.not_busy(&target)?;
                self.take_away(target);
                Ok(())
            }
            Entry::Folder { listed: false } => {
                refused("removing a folder that was there before the program started")
            }
            Entry::Folder { listed: true } => {
                if self.holds_anything(&target) {
                    return fails("Other", "the folder isn't empty");
                }
                self.changing(&target)?;
                self.take_away(target);
                Ok(())
            }
        }
    }

    /// Whether a folder the program made holds anything, a file being made in it included.
    fn holds_anything(&self, folder: &[String]) -> bool {
        self.children(folder).any(|(_, entry)| !matches!(entry, Entry::Gone)) || self.making_in(folder)
    }

    /// Whether a file is being made in a folder, beside where it's going.
    fn making_in(&self, folder: &[String]) -> bool {
        self.open.values().any(|open| {
            matches!(open, Open::Writing { target, .. }
                if target.split_last().is_some_and(|(_, beside)| beside == folder))
        })
    }

    /// The folder a path names, failing as listing it does where it isn't one.
    fn folder_at(&mut self, reached: Reached) -> Done<Vec<String>> {
        match self.what_is(reached)? {
            (folder, Entry::Folder { .. }) => Ok(folder),
            (_, Entry::Gone) => nothing_there(),
            (_, Entry::File(_) | Entry::Found) => through_a_file(),
        }
    }

    /// The names in a folder the program made, sorted as the runtime sorts them, by their bytes.
    fn names_in(&self, folder: &[String]) -> Done<Vec<String>> {
        if !matches!(self.known.get(folder), Some(Entry::Folder { listed: true })) {
            return refused("what's in a folder that was there before the program started");
        }
        if self.making_in(folder) {
            return refused(
                "what's in a folder while a file is made in it, under a name holding the process's number",
            );
        }
        let mut names: Vec<String> = self
            .children(folder)
            .filter(|(_, entry)| !matches!(entry, Entry::Gone))
            .filter_map(|(path, _)| path.last().cloned())
            .collect();
        names.sort();
        Ok(names)
    }

    /// The paths of what's in a folder, each as `shown`, the folder's path as the program wrote
    /// it, with its name after.
    pub(crate) fn list_dir(&mut self, reached: Reached, shown: &str) -> Done<Vec<String>> {
        let folder = self.folder_at(reached)?;
        Ok(self.names_in(&folder)?.iter().map(|name| path_in(shown, name)).collect())
    }

    /// The paths of everything under a folder, a folder's before what's in it.
    pub(crate) fn walk_from(&mut self, reached: Reached, shown: &str) -> Done<Vec<String>> {
        let folder = self.folder_at(reached)?;
        let mut found = Vec::new();
        self.walk_into(&folder, shown, &mut found)?;
        Ok(found)
    }

    fn walk_into(&self, folder: &[String], shown: &str, found: &mut Vec<String>) -> Done<()> {
        for name in self.names_in(folder)? {
            let path = path_in(shown, &name);
            found.push(path.clone());
            let inside = joined(folder, &name);
            if matches!(self.known.get(&inside), Some(Entry::Folder { .. })) {
                self.walk_into(&inside, &path, found)?;
            }
        }
        Ok(())
    }

    pub(crate) fn metadata(&mut self, reached: Reached) -> Done<Metadata> {
        match self.what_is(reached)? {
            (_, Entry::File(text)) => Ok(Metadata { size: Some(text.len()), is_dir: false }),
            (_, Entry::Found) => Ok(Metadata { size: None, is_dir: false }),
            (_, Entry::Folder { .. }) => Ok(Metadata { size: None, is_dir: true }),
            (_, Entry::Gone) => nothing_there(),
        }
    }

    /// A new file to write, lent by `fs.create`, which takes its path's place when its block
    /// ends. `shown` is the path as the runtime names it, should a folder be there.
    pub(crate) fn create(&mut self, reached: Reached, shown: &str) -> Done<i64> {
        let target = match reached {
            Reached::At(_, None) => {
                return fails("Other", &format!("`{shown}` isn't a file that can be replaced"));
            }
            other => Self::named(other, "making a file at")?,
        };
        if let Entry::Folder { .. } = self.kind(&target)? {
            return fails("Other", &format!("`{shown}` isn't a file that can be replaced"));
        }
        self.changing(&target)?;
        Ok(self.lend(Open::Writing { target, text: String::new() }))
    }

    /// The file at a path to read, lent by `fs.open`, as it is now.
    pub(crate) fn open_file(&mut self, reached: Reached) -> Done<i64> {
        match self.what_is(reached)? {
            (path, Entry::File(text)) => Ok(self.lend(Open::Reading { path, text, at: 0 })),
            (_, Entry::Found) => refused("opening a file that was there before the program started"),
            (_, Entry::Folder { .. }) => a_folder(),
            (_, Entry::Gone) => nothing_there(),
        }
    }

    fn lend(&mut self, open: Open) -> i64 {
        let handle = self.next_handle;
        self.next_handle += 1;
        self.open.insert(handle, open);
        handle
    }

    /// Ends a file's block: one written takes its path's place, and one read is closed.
    pub(crate) fn finish(&mut self, handle: i64) -> Done<()> {
        let Some(Open::Writing { target, text }) = self.open.remove(&handle) else { return Ok(()) };
        self.bytes = self.bytes.saturating_sub(text.len());
        if let Entry::Folder { .. } = self.kind(&target)? {
            return refused("a folder made where a file being made was to go");
        }
        self.not_busy(&target)?;
        self.put(target, text)
    }

    /// Ends a file's block that failed: one being written is thrown away, leaving its path as it
    /// was.
    pub(crate) fn discard(&mut self, handle: i64) {
        if let Some(Open::Writing { text, .. }) = self.open.remove(&handle) {
            self.bytes = self.bytes.saturating_sub(text.len());
        }
    }

    /// Writes text to a file being made.
    pub(crate) fn write_to(&mut self, handle: i64, more: &str) -> Done<()> {
        let bytes = self.bytes.saturating_add(more.len());
        match self.open.get_mut(&handle) {
            None => closed(),
            Some(Open::Reading { .. }) => fails("Other", "this file was opened to read, not to write"),
            Some(Open::Writing { .. }) if bytes > MOST_BYTES => refused("ran too long: it wrote too much"),
            Some(Open::Writing { text, .. }) => {
                text.push_str(more);
                self.bytes = bytes;
                Ok(())
            }
        }
    }

    /// Whether a file lent is still open, for `flush` and `sync`, which have nothing else to do.
    pub(crate) fn still_open(&self, handle: i64) -> Done<()> {
        if self.open.contains_key(&handle) { Ok(()) } else { closed() }
    }

    /// The next line of a file being read, without its line break, or `None` at its end.
    pub(crate) fn read_line(&mut self, handle: i64) -> Done<Option<String>> {
        let (text, at) = self.reading(handle)?;
        let rest = text.get(*at..).unwrap_or_default();
        if rest.is_empty() {
            return Ok(None);
        }
        let line = if let Some((line, _)) = rest.split_once('\n') {
            *at += line.len() + 1;
            line
        } else {
            *at = text.len();
            rest
        };
        Ok(Some(line.strip_suffix('\r').unwrap_or(line).to_owned()))
    }

    /// What's left of a file being read.
    pub(crate) fn read_rest(&mut self, handle: i64) -> Done<String> {
        let (text, at) = self.reading(handle)?;
        let rest = text.get(*at..).unwrap_or_default().to_owned();
        *at = text.len();
        Ok(rest)
    }

    /// A file being read, by its handle: its text and how far in it is.
    fn reading(&mut self, handle: i64) -> Done<(&String, &mut usize)> {
        match self.open.get_mut(&handle) {
            None => closed(),
            Some(Open::Writing { .. }) => fails("Other", "this file was made to write, not to read"),
            Some(Open::Reading { text, at, .. }) => Ok((text, at)),
        }
    }
}

/// Where a walk ended.
enum Walked {
    To(Vec<String>),
    Missing,
    ThroughFile,
}

fn closed<T>() -> Done<T> {
    fails("Other", "this file was closed when its `with` block ended")
}

fn joined(folder: &[String], name: &str) -> Vec<String> {
    let mut path = folder.to_vec();
    path.push(name.to_owned());
    path
}

/// The path of the thing called `name` in a folder, as the runtime joins them: a `/` between,
/// unless the folder's path ends with one.
pub(crate) fn path_in(folder: &str, name: &str) -> String {
    if folder.is_empty() || folder.ends_with('/') {
        format!("{folder}{name}")
    } else {
        format!("{folder}/{name}")
    }
}

/// What's on the disk at a path inside the folder the program runs in, each folder on the way to
/// it already found to be a folder: a link isn't followed, but refused.
fn looked_at(front: &str, parts: &[String]) -> Done<Entry> {
    let mut path = PathBuf::from(format!("{front}/"));
    path.extend(parts);
    match std::fs::symlink_metadata(&path) {
        Ok(found) if found.file_type().is_symlink() => refused("a link, which the oracle doesn't follow"),
        Ok(found) if found.is_dir() => Ok(Entry::Folder { listed: false }),
        Ok(found) if found.is_file() => Ok(Entry::Found),
        Ok(_) => refused("something on the disk that's neither a file nor a folder"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Entry::Gone),
        Err(_) => refused("a path the system won't let the oracle look at"),
    }
}

/// Refuses a name longer than every system allows, and on Windows one it reads its own way: one
/// ending in `.` or a space, which it drops, one holding a character it won't take, or a
/// device's name, like `NUL` or `com1.txt`.
fn checked_name(name: &str) -> Done<()> {
    if name.len() > LONGEST_NAME {
        return refused("a name longer than every system allows");
    }
    if !cfg!(windows) {
        return Ok(());
    }
    let stem = name.split('.').next().unwrap_or_default().trim_end().to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.chars().count() == 4);
    if device
        || name.ends_with(['.', ' '])
        || name.chars().any(|c| c.is_control() || matches!(c, '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return refused("a name Windows reads its own way");
    }
    Ok(())
}

/// Refuses a path on Windows with a `\` or a `:`, which it reads as a separator or a drive.
fn odd_on_windows(text: &str) -> Done<()> {
    if cfg!(windows) && text.contains(['\\', ':']) {
        return refused("a path with `\\` or `:`, which Windows reads its own way");
    }
    Ok(())
}
