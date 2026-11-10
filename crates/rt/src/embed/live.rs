//! A count of what each thread's runtime holds, for finding what grows in a long-running program:
//! its live blocks by size, its temporaries and inputs, the texts it's kept, its names, and a
//! worker's parked tasks and served connections. With `FENNEC_LIVE_REPORT=file` set, each
//! worker publishes its own every so often and whenever it goes idle, and a thread of its own
//! writes the sum to `file` a few times a second, replacing what was there. A test drives a program
//! the same way over and over and checks the counts hold. Unlike the process's memory, they say
//! what's live, not what's cached for reuse. What is cached, freed blocks kept for reuse and the
//! pages of the table of blocks, comes on lines of its own starting `cached`, which grow as a
//! program warms up and then have to stay within their bounds. So do blocks that are never freed:
//! each is made once, the first time its literal, constant, capability or case is asked for, so
//! how many there are says when the program first went that way, not what it holds on to.
//!
//! Nothing's counted unless the variable's set, and then only every 100 ms on each thread.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// What one thread's runtime holds.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub(crate) struct Counts {
    /// Live blocks, by how many words each takes.
    pub(crate) blocks: BTreeMap<usize, usize>,
    pub(crate) temps: usize,
    pub(crate) inputs: usize,
    pub(crate) texts: usize,
    pub(crate) names: usize,
    /// A worker's parked tasks, and the accounts, temporaries and waits it keeps for them.
    pub(crate) tasks: usize,
    /// The connections it's serving.
    pub(crate) connections: usize,
    /// Words of freed blocks kept for reuse, at most 8 MB's worth on each thread.
    pub(crate) spare: usize,
    /// Pages in its table of blocks, at most about twice those with blocks on them.
    pub(crate) pages: usize,
    /// Blocks never freed, one for each literal, constant, capability or case made so far.
    pub(crate) constants: usize,
    /// How many threads' counts are in the sum.
    pub(crate) threads: usize,
}

impl Counts {
    fn add(&mut self, other: &Counts) {
        for (words, count) in &other.blocks {
            *self.blocks.entry(*words).or_default() += count;
        }
        self.temps += other.temps;
        self.inputs += other.inputs;
        self.texts += other.texts;
        self.names += other.names;
        self.tasks += other.tasks;
        self.connections += other.connections;
        self.spare += other.spare;
        self.threads += other.threads;
        self.pages += other.pages;
        self.constants += other.constants;
    }

    /// The report's lines: one for each count, and one for each size of block.
    fn lines(&self) -> String {
        let blocks: usize = self.blocks.values().sum();
        let mut text = format!(
            "blocks {blocks}\ntemps {}\ninputs {}\ntexts {}\nnames {}\ntasks {}\nconnections {}\nthreads {}\ncached spare {}\ncached pages {}\ncached constants {}\n",
            self.temps,
            self.inputs,
            self.texts,
            self.names,
            self.tasks,
            self.connections,
            self.threads,
            self.spare,
            self.pages,
            self.constants
        );
        for (words, count) in &self.blocks {
            let _ = writeln!(text, "words {words} {count}");
        }
        text
    }
}

/// Where the report goes, if it's asked for.
fn report() -> Option<&'static PathBuf> {
    static REPORT: OnceLock<Option<PathBuf>> = OnceLock::new();
    REPORT.get_or_init(|| std::env::var_os("FENNEC_LIVE_REPORT").map(PathBuf::from)).as_ref()
}

/// Each thread's latest counts, by the thread's number.
static PUBLISHED: Mutex<Option<HashMap<u16, Counts>>> = Mutex::new(None);

thread_local! {
    static LAST: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// How often a thread publishes its counts while it's busy.
const EVERY: Duration = Duration::from_millis(100);

/// Publishes this thread's counts, from `count`, if a report's asked for and it's been a while,
/// or always when `now` is set, as a worker going idle does.
pub(crate) fn publish(now: bool, count: impl FnOnce() -> Counts) {
    let Some(path) = report() else { return };
    if !now && LAST.get().is_some_and(|last| last.elapsed() < EVERY) {
        return;
    }
    LAST.set(Some(Instant::now()));
    let counts = count();
    let thread = super::threads::index();
    let mut published = PUBLISHED.lock().unwrap_or_else(PoisonError::into_inner);
    let first = published.is_none();
    published.get_or_insert_with(HashMap::new).insert(thread, counts);
    drop(published);
    if first {
        let path = path.clone();
        let _ = std::thread::Builder::new()
            .name("fennec-live-report".to_owned())
            .spawn(move || write_every(&path));
    }
}

/// Writes the sum of what every thread published to `path`, a few times a second, for as long as
/// the program runs.
fn write_every(path: &PathBuf) {
    let writing = path.with_extension("writing");
    loop {
        let mut total = Counts::default();
        if let Some(published) = PUBLISHED.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            for counts in published.values() {
                total.add(counts);
            }
        }
        let written =
            std::fs::File::create(&writing).and_then(|mut file| file.write_all(total.lines().as_bytes()));
        if written.is_ok() {
            let _ = std::fs::rename(&writing, path);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// This thread's runtime's own counts, without a worker's tasks.
pub(crate) fn of_this_thread() -> Counts {
    super::with(|state| {
        let (blocks, constants) = state.heap.live_by_size();
        Some(Counts {
            blocks,
            constants,
            temps: state.temps.len(),
            inputs: state.inputs.len(),
            texts: state.heap.texts.borrow().len(),
            names: state.name_ids.len(),
            spare: state.heap.spare_words(),
            pages: state.heap.table_pages(),
            threads: 1,
            ..Counts::default()
        })
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::Counts;

    #[test]
    fn counts_add_up_by_size_and_read_as_lines() {
        let mut total = Counts::default();
        let one = Counts { blocks: [(4, 2), (6, 1)].into(), temps: 1, ..Counts::default() };
        let other = Counts { blocks: [(4, 1)].into(), connections: 2, ..Counts::default() };
        total.add(&one);
        total.add(&other);
        assert_eq!(total.blocks, [(4, 3), (6, 1)].into());
        assert_eq!(
            total.lines(),
            "blocks 4\ntemps 1\ninputs 0\ntexts 0\nnames 0\ntasks 0\nconnections 2\nthreads 0\ncached spare 0\ncached pages 0\ncached constants 0\nwords 4 3\nwords 6 1\n"
        );
    }
}
