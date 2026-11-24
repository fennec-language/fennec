//! A build handed to a short-lived process of its own: `fennec build-child`.
//!
//! The daemon keeps what lasts between builds, the kept load and the kept check. Lowering, LLVM
//! and the link make many short-lived allocations, which leave the daemon's heap holding
//! gigabytes it can't give back. A child does that work and exits, and its memory goes with it.
//!
//! The two talk over one socket, the child's standard input, both ways. Its standard output and
//! error are the daemon's, so what the build prints goes where an in-process build's would. The
//! request starts with [`MAGIC`] and the daemon's build of Fennec, which a child from any other
//! build refuses. Then come sections, each a tag, its length and its bytes, ended by [`END`]:
//! - request: [`JOB`], [`PROGRAM`], [`SIGNATURES`], and section 4, the LLVM lane's kept
//!   analyses, once they go too;
//! - answer: [`STARTED`] once the request is read, then [`RESULT`], then the analyses it changed.
//!
//! A child runs in a process group of its own, with the tools it runs. Whoever ends it ends the
//! group: the daemon when the command that asked has gone or the child didn't start, and the
//! child itself when its socket closes, which means the daemon has gone.

use std::path::Path;

#[cfg(unix)]
use fennec_syntax::codec::{Decode, Encode, Reader, Writer};

use crate::aot::Built;

/// The command a daemon runs its builds in.
pub(crate) const COMMAND: &str = "build-child";

/// What a request starts with, before the build of Fennec that sent it.
#[cfg(unix)]
const MAGIC: &[u8; 8] = b"FENCHILD";

/// The build of Fennec this is: a child only builds for the one it's of.
#[cfg(unix)]
const BUILD: &str = env!("FENNEC_BUILD");

/// The last section.
#[cfg(unix)]
const END: u8 = 0;
/// The request's job: where the program goes and how it's built.
#[cfg(unix)]
const JOB: u8 = 1;
/// In an answer, the child read the request and started building.
#[cfg(unix)]
const STARTED: u8 = 1;
/// The program: its module, files and where its kit is.
#[cfg(unix)]
const PROGRAM: u8 = 2;
/// In an answer, what the build gave: what it built, or why it couldn't.
#[cfg(unix)]
const RESULT: u8 = 2;
/// What the daemon's check found, when it's handed on.
#[cfg(unix)]
const SIGNATURES: u8 = 3;
/// Each function's analysis the builds before kept, as `fennec_llvm::kept_to_bytes` wrote them: in a
/// request, for the build to use, and in an answer, what it keeps for the next, sent after the check
/// of what it used (see `serve`).
#[cfg(unix)]
const ANALYSES: u8 = 4;
/// In an answer, the functions whose analysis the build used again and the check after it found
/// wrong, for the daemon to take back what was made with them.
#[cfg(unix)]
const WRONG: u8 = 5;
/// The objects the last build found in the cache or made, as `fennec_llvm::known_objects_to_bytes`
/// wrote them: in a request, so the build doesn't look for them again, and in an answer, what
/// it found or made, for the next.
#[cfg(unix)]
const OBJECTS: u8 = 6;

/// Why a build in a child didn't answer, which the daemon says before building in-process.
#[derive(Debug)]
pub(crate) struct NoAnswer(pub(crate) String);

/// A child's job.
#[cfg(unix)]
struct Job {
    output: String,
    release: bool,
    baseline: bool,
    /// For an executable of a module's tests, as `fennec test` runs them, the functions it runs
    /// (see `aot::tests_executable`); for a program, nothing.
    tests: Option<Vec<String>>,
}

#[cfg(unix)]
fennec_syntax::codec_fields!(Job { output, release, baseline, tests });

/// A program as a child needs it: what `aot::executable_checked` reads of a `fennec_kit::Program`.
#[cfg(unix)]
struct Sent {
    module: fennec_syntax::Module,
    files: Vec<(String, String)>,
    kit_src: Option<(String, String)>,
    /// The kit's name, version and Fennec, from its `kit.toml`: a build reads its name.
    kit: Option<(String, String, String)>,
}

#[cfg(unix)]
fennec_syntax::codec_fields!(Sent { module, files, kit_src, kit });

#[cfg(unix)]
impl Sent {
    fn of(program: &fennec_kit::Program) -> Sent {
        Sent {
            module: program.module.clone(),
            files: program.sources.files().map(|(path, text)| (path.to_owned(), text.to_owned())).collect(),
            kit_src: program
                .kit_src
                .as_ref()
                .map(|(src, shown)| (src.to_string_lossy().into_owned(), shown.clone())),
            kit: program
                .manifest
                .as_ref()
                .map(|manifest| (manifest.name.clone(), manifest.version.clone(), manifest.fennec.clone())),
        }
    }

    /// A module alone, as an executable of its tests is built from.
    fn of_module(module: &fennec_syntax::Module) -> Sent {
        Sent { module: module.clone(), files: Vec::new(), kit_src: None, kit: None }
    }

    fn into_program(self) -> fennec_kit::Program {
        let mut sources = fennec_syntax::Sources::new();
        for (path, text) in self.files {
            sources.add(path, text);
        }
        fennec_kit::Program {
            module: self.module,
            sources,
            manifest: self.kit.map(|(name, version, fennec)| fennec_kit::Manifest {
                name,
                version,
                fennec,
                dependencies: Vec::new(),
            }),
            embedded: Vec::new(),
            kit_src: self.kit_src.map(|(src, shown)| (src.into(), shown)),
            dependency_files: std::collections::HashSet::new(),
        }
    }
}

#[cfg(unix)]
impl Encode for Built {
    fn encode(&self, to: &mut Writer) {
        self.total.encode(to);
        self.fresh.encode(to);
        self.reused.encode(to);
        u64::try_from(self.compiled.as_nanos()).unwrap_or(u64::MAX).encode(to);
        u64::try_from(self.linked.as_nanos()).unwrap_or(u64::MAX).encode(to);
        self.root.encode(to);
    }
}

#[cfg(unix)]
impl Decode for Built {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        Some(Built {
            total: Decode::decode(from)?,
            fresh: Decode::decode(from)?,
            reused: Decode::decode(from)?,
            compiled: std::time::Duration::from_nanos(Decode::decode(from)?),
            linked: std::time::Duration::from_nanos(Decode::decode(from)?),
            root: Decode::decode(from)?,
        })
    }
}

/// What a build gives back, as bytes: what it built, or why it couldn't.
#[cfg(unix)]
fn result_bytes(result: &Result<Built, String>) -> Vec<u8> {
    let mut to = Writer::default();
    match result {
        Ok(built) => {
            to.tag(0);
            built.encode(&mut to);
        }
        Err(why) => {
            to.tag(1);
            why.encode(&mut to);
        }
    }
    to.into_bytes()
}

#[cfg(unix)]
fn read_result(bytes: &[u8]) -> Option<Result<Built, String>> {
    read_either(bytes)
}

/// What a build of a module's tests gives back, as bytes: the functions with a flag saying they
/// ran, or why it couldn't.
#[cfg(unix)]
fn ran_bytes(result: &Result<Vec<String>, String>) -> Vec<u8> {
    let mut to = Writer::default();
    match result {
        Ok(flags) => {
            to.tag(0);
            flags.encode(&mut to);
        }
        Err(why) => {
            to.tag(1);
            why.encode(&mut to);
        }
    }
    to.into_bytes()
}

#[cfg(unix)]
fn read_ran(bytes: &[u8]) -> Option<Result<Vec<String>, String>> {
    read_either(bytes)
}

/// What either kind of build gave back, from all of `bytes`.
#[cfg(unix)]
fn read_either<T: Decode>(bytes: &[u8]) -> Option<Result<T, String>> {
    let mut from = Reader::new(bytes);
    let result = match from.tag()? {
        0 => Ok(T::decode(&mut from)?),
        1 => Err(String::decode(&mut from)?),
        _ => return None,
    };
    from.is_done().then_some(result)
}

/// One value's bytes, which read back must be the same value, giving the same bytes: what's sent
/// is what's built from. Checked where a build checks its proofs, and in dev builds of Fennec.
#[cfg(unix)]
fn bytes_of<T: Encode>(value: &T) -> Vec<u8> {
    let mut to = Writer::default();
    value.encode(&mut to);
    to.into_bytes()
}

/// Whether what was read is checked by writing it again: in a build checking its proofs, and
/// always in a dev build of Fennec.
#[cfg(unix)]
fn checks_what_it_read() -> bool {
    cfg!(debug_assertions) || std::env::var("FENNEC_CHECK_PROOFS").as_deref() == Ok("1")
}

#[cfg(unix)]
mod unix {
    use std::io::{BufReader, BufWriter, Read, Write};
    use std::os::fd::AsFd as _;
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::process::{Child, Command, ExitCode, Stdio};
    use std::sync::{Arc, Condvar, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use fennec_syntax::codec::{Decode, Reader};

    use super::{
        ANALYSES, BUILD, Built, END, JOB, Job, MAGIC, NoAnswer, OBJECTS, PROGRAM, RESULT, SIGNATURES,
        STARTED, Sent, WRONG, bytes_of, checks_what_it_read, ran_bytes, read_ran, read_result, result_bytes,
    };

    /// The last child's end, read on a thread of its own once it answers: it checks what it used
    /// again and sends what it keeps, and the next build waits for that. Its socket stays open until
    /// the child has ended, since a child takes its closing as the daemon gone.
    static LAST: Mutex<Option<Arc<Ending>>> = Mutex::new(None);

    /// Whether a child's end has been read.
    #[derive(Default)]
    struct Ending {
        done: Mutex<bool>,
        ended: Condvar,
    }

    /// The functions children's checks found wrong analyses of, not yet taken back: that's done
    /// once the command's own work is done (see [`after_builds`]), so the tests it ran with them
    /// are counted first, and then taken back.
    static FOUND: Mutex<Vec<String>> = Mutex::new(Vec::new());

    /// What the last child kept for the next, as it sent them: its build's analyses, and the
    /// objects it found or made. Bytes only a child reads, so the daemon holds a block each, not
    /// the many small ones they'd be here.
    static STORE: Mutex<Kept> = Mutex::new(Kept { analyses: Vec::new(), objects: Vec::new() });

    struct Kept {
        analyses: Vec<u8>,
        objects: Vec<u8>,
    }

    /// How long a child has to read its request and say it started: 60 s, or 2 s for a test of
    /// one that never does.
    fn start_within() -> Duration {
        Duration::from_secs(if super::test_failure().as_deref() == Some("hang") { 2 } else { 60 })
    }

    /// How often the daemon looks whether the command that asked for the build has gone.
    const LOOK_EVERY: Duration = Duration::from_millis(100);

    /// Builds `program` at `output` in a child process, as `aot::executable_checked` would here.
    /// `Err` when the child gave no answer: it couldn't start, crashed, was another build of
    /// Fennec or didn't start in time. The build is then done here.
    pub(crate) fn built(
        program: &fennec_kit::Program,
        output: &Path,
        release: bool,
        checked: Option<&fennec_check::Signatures>,
    ) -> Result<Result<Built, String>, NoAnswer> {
        let job =
            Job { output: output.to_string_lossy().into_owned(), release, baseline: baseline(), tests: None };
        in_child(&job, &Sent::of(program), checked, read_result)
    }

    /// Builds an executable of `module`'s `tests` at `output` in a child process, as
    /// `aot::tests_executable` would here, giving back the functions with a flag saying they ran.
    /// `Err` when the child gave no answer, as for [`built`].
    pub(crate) fn tests_built(
        module: &fennec_syntax::Module,
        tests: &[String],
        output: &Path,
    ) -> Result<Result<Vec<String>, String>, NoAnswer> {
        let job = Job {
            output: output.to_string_lossy().into_owned(),
            release: false,
            baseline: baseline(),
            tests: Some(tests.to_vec()),
        };
        in_child(&job, &Sent::of_module(module), None, read_ran)
    }

    /// Whether builds here are for x86-64 machines older than AVX2.
    fn baseline() -> bool {
        crate::aot::BASELINE.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Does `job` in a child, reading its answer with `read`.
    fn in_child<T>(
        job: &Job,
        sent: &Sent,
        checked: Option<&fennec_check::Signatures>,
        read: fn(&[u8]) -> Option<Result<T, String>>,
    ) -> Result<Result<T, String>, NoAnswer> {
        let no_answer = |why: String| NoAnswer(why);
        after_builds();
        let (ours, theirs) =
            UnixStream::pair().map_err(|err| no_answer(format!("no socket for it: {err}")))?;
        let exe = std::env::current_exe().map_err(|err| no_answer(format!("no `fennec` to run: {err}")))?;
        // The command holds the child's end of the socket until it's dropped, which it is here, so
        // the child's going is seen at once.
        let child = {
            let mut command = Command::new(exe);
            std::os::unix::process::CommandExt::process_group(&mut command, 0);
            command
                .arg(super::COMMAND)
                .stdin(Stdio::from(std::os::fd::OwnedFd::from(theirs)))
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
        }
        .map_err(|err| no_answer(format!("it couldn't start: {err}")))?;
        let mut child = Running { child: Some(child), socket: ours };
        let answer = child.asked(job, sent, checked, read);
        match answer {
            Ok(result) => {
                // A copy of the socket, open until the child has ended.
                let rest =
                    child.socket.try_clone().ok().and_then(|socket| Some((child.child.take()?, socket)));
                if let Some((process, socket)) = rest {
                    let ending = Arc::new(Ending::default());
                    let read = Arc::clone(&ending);
                    let reading = std::thread::Builder::new().spawn(move || {
                        read_rest(process, &socket);
                        *read.done.lock().unwrap_or_else(PoisonError::into_inner) = true;
                        read.ended.notify_all();
                    });
                    if reading.is_ok() {
                        *LAST.lock().unwrap_or_else(PoisonError::into_inner) = Some(ending);
                    }
                }
                Ok(result)
            }
            // Stopped because the command went, as on Ctrl-C: the build stops, as one here does.
            Err(_) if fennec_build::cancelled() => Ok(Err(fennec_llvm::STOPPED.to_owned())),
            Err(why) => Err(no_answer(why)),
        }
    }

    /// Waits for the last child to end, if it's working on after its answer, and takes back what
    /// was made with an analysis a child's check found wrong, as the daemon's own check of a build
    /// made in it does once the command's done: before the next build, and as the daemon ends.
    ///
    /// What was found is held till it's reported, so a call that finds another one reporting waits
    /// for it: the daemon ending right after a build, while the thread its session left is
    /// reporting, goes only once the report is written.
    pub(crate) fn after_builds() {
        let last = LAST.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(ending) = last {
            let mut done = ending.done.lock().unwrap_or_else(PoisonError::into_inner);
            while !*done {
                done = ending.ended.wait(done).unwrap_or_else(PoisonError::into_inner);
            }
        }
        let mut held = FOUND.lock().unwrap_or_else(PoisonError::into_inner);
        let found = std::mem::take(&mut *held);
        if found.is_empty() {
            return;
        }
        match super::HOME.get() {
            Some(home) => {
                let _ = crate::kept::report_wrong_analyses(home, &found);
            }
            None => eprintln!("error: a build's check found these analyses wrong: {}", found.join(", ")),
        }
        drop(held);
    }

    /// Reads what a child sends after its answer, and waits for it to end: what the check of what
    /// it used again found wrong, which is taken back as a check in the daemon's own build would
    /// have it, and what it keeps for the next build. A child that ends without sending all of it
    /// leaves nothing kept, so the next build works every analysis out.
    fn read_rest(mut process: Child, socket: &UnixStream) {
        let _ = socket.set_read_timeout(None);
        let (mut analyses, mut objects, mut wrong) = (None, None, Vec::new());
        let ended = loop {
            match read_section(&mut &*socket, || None) {
                Ok((ANALYSES, bytes)) => analyses = Some(bytes),
                Ok((OBJECTS, bytes)) => objects = Some(bytes),
                Ok((WRONG, bytes)) => wrong = read_back::<Vec<String>>(&bytes).unwrap_or_default(),
                Ok((END, _)) => break true,
                Ok(_) | Err(_) => break false,
            }
        };
        let _ = process.wait();
        let (analyses, objects) = if ended {
            (analyses.unwrap_or_default(), objects.unwrap_or_default())
        } else {
            Default::default()
        };
        *STORE.lock().unwrap_or_else(PoisonError::into_inner) = Kept { analyses, objects };
        FOUND.lock().unwrap_or_else(PoisonError::into_inner).extend(wrong);
    }

    /// A child the daemon is building in, killed and waited for unless it answered.
    struct Running {
        child: Option<Child>,
        socket: UnixStream,
    }

    impl Drop for Running {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                stop(&mut child);
                let _ = child.wait();
            }
        }
    }

    /// Kills a child and everything in its process group: the tools it was running.
    fn stop(child: &mut Child) {
        let group = rustix::process::Pid::from_child(child);
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        let _ = child.kill();
    }

    impl Running {
        /// Sends the request and reads the answer.
        fn asked<T>(
            &mut self,
            job: &Job,
            sent: &Sent,
            checked: Option<&fennec_check::Signatures>,
            read: fn(&[u8]) -> Option<Result<T, String>>,
        ) -> Result<Result<T, String>, String> {
            let mut request = MAGIC.to_vec();
            let sections = (|| {
                request.write_all(&bytes_of(&BUILD))?;
                section(&mut request, JOB, &bytes_of(job))?;
                section(&mut request, PROGRAM, &bytes_of(sent))?;
                if let Some(checked) = checked {
                    section(&mut request, SIGNATURES, &checked.to_bytes())?;
                }
                let kept = STORE.lock().unwrap_or_else(PoisonError::into_inner);
                section(&mut request, ANALYSES, &kept.analyses)?;
                section(&mut request, OBJECTS, &kept.objects)?;
                drop(kept);
                section(&mut request, END, &[])
            })();
            sections.map_err(|err| self.ended(&format!("its request couldn't be written: {err}")))?;
            let started = Instant::now();
            let _ = self.socket.set_read_timeout(Some(LOOK_EVERY));
            let _ = self.socket.set_write_timeout(Some(LOOK_EVERY));
            let mut socket = &self.socket;
            let sent = write_all(&mut socket, &request, || Running::gone(started, false));
            sent.map_err(|why| self.ended(&format!("it didn't take the request: {why}")))?;
            // Whether it's said it started, after which it has as long as it takes.
            let mut building = false;
            loop {
                let mut socket = &self.socket;
                let section_read = read_section(&mut socket, || Running::gone(started, building));
                match section_read {
                    Ok((STARTED, _)) if !building => building = true,
                    // What comes after is read once the command has its answer (see `read_rest`).
                    Ok((RESULT, bytes)) if building => {
                        return read(&bytes).ok_or_else(|| self.ended("its answer didn't read"));
                    }
                    Ok((END, _)) => return Err(self.ended("it ended its answer early")),
                    Ok((tag, _)) => return Err(self.ended(&format!("it sent section {tag} out of turn"))),
                    Err(why) => return Err(self.ended(&why)),
                }
            }
        }

        /// Why the child should be given up on while the daemon waits: the command that asked has
        /// gone, or it hasn't started in time.
        fn gone(started: Instant, reading: bool) -> Option<String> {
            if fennec_build::cancelled() {
                return Some(fennec_llvm::STOPPED.to_owned());
            }
            let within = start_within();
            (!reading && started.elapsed() > within)
                .then(|| format!("it didn't start within {} s", within.as_secs()))
        }

        /// Ends the child, saying why, with how it ended if it had.
        fn ended(&mut self, why: &str) -> String {
            let Some(mut child) = self.child.take() else { return why.to_owned() };
            let ended = child.try_wait().ok().flatten();
            if ended.is_none() {
                stop(&mut child);
            }
            let status = ended.or_else(|| child.wait().ok());
            match status {
                Some(status) if !status.success() => format!("{why}: it {}", ended_as(status)),
                _ => why.to_owned(),
            }
        }
    }

    /// How a process ended, in words.
    fn ended_as(status: std::process::ExitStatus) -> String {
        use std::os::unix::process::ExitStatusExt as _;
        match (status.code(), status.signal()) {
            (Some(code), _) => format!("exited with {code}"),
            (None, Some(signal)) => format!("was ended by signal {signal}"),
            (None, None) => "ended".to_owned(),
        }
    }

    /// Writes one section: its tag, its length and its bytes.
    fn section(to: &mut impl Write, tag: u8, bytes: &[u8]) -> std::io::Result<()> {
        to.write_all(&[tag])?;
        to.write_all(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes())?;
        to.write_all(bytes)
    }

    /// Reads one section, retrying a read that timed out until `stop` gives a reason to stop.
    fn read_section(
        from: &mut impl Read,
        stop: impl Fn() -> Option<String>,
    ) -> Result<(u8, Vec<u8>), String> {
        let mut head = [0; 9];
        read_all(from, &mut head, &stop)?;
        let length = usize::try_from(u64::from_le_bytes(head[1..].try_into().unwrap_or_default()))
            .map_err(|_| "a section too long".to_owned())?;
        let mut bytes = vec![0; length];
        read_all(from, &mut bytes, &stop)?;
        Ok((head[0], bytes))
    }

    /// Writes all of `bytes`, retrying a write that timed out until `stop` gives a reason to stop.
    fn write_all(to: &mut impl Write, bytes: &[u8], stop: impl Fn() -> Option<String>) -> Result<(), String> {
        let mut at = 0;
        while let Some(rest) = bytes.get(at..).filter(|rest| !rest.is_empty()) {
            match to.write(rest) {
                Ok(0) => return Err("it closed its socket".to_owned()),
                Ok(written) => at += written,
                Err(err)
                    if matches!(
                        err.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if let Some(why) = stop() {
                        return Err(why);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err.to_string()),
            }
        }
        Ok(())
    }

    fn read_all(
        from: &mut impl Read,
        into: &mut [u8],
        stop: &impl Fn() -> Option<String>,
    ) -> Result<(), String> {
        let mut at = 0;
        while let Some(rest) = into.get_mut(at..).filter(|rest| !rest.is_empty()) {
            match from.read(rest) {
                Ok(0) => return Err("it ended without an answer".to_owned()),
                Ok(read) => at += read,
                Err(err)
                    if matches!(
                        err.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if let Some(why) = stop() {
                        return Err(why);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) => return Err(format!("its answer couldn't be read: {err}")),
            }
        }
        Ok(())
    }

    /// `fennec build-child`: reads a request on standard input, builds it, and answers there.
    pub(crate) fn serve() -> ExitCode {
        let failing = super::test_failure();
        if failing.as_deref() == Some("crash") {
            let _ = rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::KILL);
        }
        // It keeps what its build works out about each function, for the daemon to hand the next.
        fennec_llvm::keep_analyses_between_builds();
        let Ok(socket) = std::io::stdin().as_fd().try_clone_to_owned().map(UnixStream::from) else {
            eprintln!("error: `fennec {}` is run by the daemon, with a socket to it", super::COMMAND);
            return ExitCode::FAILURE;
        };
        let Some((job, program, checked)) = request(&mut BufReader::new(&socket)) else {
            return ExitCode::FAILURE;
        };
        // The socket closing is the daemon gone, killed or stopped: nothing more comes on it, so a
        // read ends only then, and the child ends with the tools it's running.
        if let Ok(mut watched) = socket.try_clone() {
            std::thread::spawn(move || {
                let mut more = [0; 1];
                while matches!(watched.read(&mut more), Ok(1..)) {}
                let group = rustix::process::getpgrp();
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
            });
        }
        if failing.as_deref() == Some("hang") {
            never_ends();
        }
        let mut to = BufWriter::new(&socket);
        if section(&mut to, STARTED, &[]).and_then(|()| to.flush()).is_err() {
            return ExitCode::FAILURE;
        }
        let noted = failing.as_deref().and_then(|failing| failing.strip_prefix("hang-building:"));
        if let Some(noted) = noted.filter(|noted| !Path::new(noted).exists()) {
            // What it builds is started, as a link stopped partway leaves it.
            let _ = std::fs::write(&job.output, b"started");
            let _ = std::fs::write(noted, std::process::id().to_string());
            never_ends();
        }
        crate::aot::BASELINE.store(job.baseline, std::sync::atomic::Ordering::Relaxed);
        let output = std::path::PathBuf::from(&job.output);
        let built = std::thread::Builder::new()
            .stack_size(crate::STACK_BYTES)
            .spawn(move || match &job.tests {
                Some(tests) => ran_bytes(&crate::aot::tests_executable(&program.module, tests, &output)),
                None => result_bytes(&crate::aot::executable_checked_here(
                    &program,
                    &output,
                    job.release,
                    checked,
                )),
            })
            .ok()
            .and_then(|building| building.join().ok());
        let Some(built) = built else { return ExitCode::FAILURE };
        if section(&mut to, RESULT, &built).and_then(|()| to.flush()).is_err() {
            return ExitCode::FAILURE;
        }
        // Answered, it writes what it put off till then, the index of the units it used, so the
        // next build finds it. It checks what it used again of what the builds before kept, as the
        // daemon's own build is checked once it's answered, and then hands on what it keeps.
        crate::aot::later::finish(Duration::from_secs(60));
        let wrong = fennec_llvm::AnalysesCheck::take().map(|check| check.run()).unwrap_or_default();
        let mut rest = || {
            if !wrong.is_empty() {
                section(&mut to, WRONG, &bytes_of(&wrong))?;
            }
            section(&mut to, ANALYSES, &fennec_llvm::kept_to_bytes())?;
            section(&mut to, OBJECTS, &fennec_llvm::known_objects_to_bytes())?;
            section(&mut to, END, &[])?;
            to.flush()
        };
        if rest().is_err() { ExitCode::FAILURE } else { ExitCode::SUCCESS }
    }

    /// What a test of a child that never ends has it do.
    fn never_ends() -> ! {
        loop {
            std::thread::park();
        }
    }

    /// The request on `from`: the job, the program, and what the daemon's check found. Nothing
    /// when it's from another build of Fennec or doesn't read, said on standard error.
    fn request(from: &mut impl Read) -> Option<(Job, fennec_kit::Program, Option<fennec_check::Signatures>)> {
        let refused = |why: &str| {
            eprintln!("error: `fennec {}` was sent a request it can't read: {why}", super::COMMAND);
        };
        let never = || None;
        let mut magic = [0; 8];
        if read_all(from, &mut magic, &never).is_err() || &magic != MAGIC {
            refused("it isn't from the daemon");
            return None;
        }
        let mut length = [0; 8];
        read_all(from, &mut length, &never).ok()?;
        let mut build = vec![0; usize::try_from(u64::from_le_bytes(length)).ok()?.min(256)];
        read_all(from, &mut build, &never).ok()?;
        if build != BUILD.as_bytes() {
            refused("it's from another build of Fennec");
            return None;
        }
        let (mut job, mut program, mut checked) = (None, None, None);
        loop {
            let Ok((tag, bytes)) = read_section(from, never) else {
                refused("it ended early");
                return None;
            };
            match tag {
                END => break,
                JOB => job = read_back::<Job>(&bytes),
                PROGRAM => program = read_back::<Sent>(&bytes).map(Sent::into_program),
                SIGNATURES => checked = Some(read_signatures(&bytes)?),
                // What doesn't read is known as nothing: the build looks for every object.
                OBJECTS => {
                    if fennec_llvm::know_objects_from_bytes(&bytes) && checks_what_it_read() {
                        assert!(
                            fennec_llvm::known_objects_to_bytes() == bytes,
                            "the objects the daemon knew read back as something else"
                        );
                    }
                }
                // What doesn't read is kept as nothing, and said: the build works every analysis
                // out, which is slower, but right.
                ANALYSES => {
                    if !fennec_llvm::keep_from_bytes(&bytes) {
                        eprintln!(
                            "note: the analyses the daemon kept for this build didn't read, so it works every one out"
                        );
                    } else if checks_what_it_read() {
                        assert!(
                            fennec_llvm::kept_to_bytes() == bytes,
                            "the analyses the daemon kept read back as something else"
                        );
                    }
                }
                _ => {
                    refused(&format!("section {tag} isn't known"));
                    return None;
                }
            }
        }
        let (Some(job), Some(program)) = (job, program) else {
            refused("its job or its program is missing");
            return None;
        };
        Some((job, program, checked))
    }

    /// One value from all of `bytes`, written again and compared where [`checks_what_it_read`].
    fn read_back<T: Decode + super::Encode>(bytes: &[u8]) -> Option<T> {
        let mut from = Reader::new(bytes);
        let value = T::decode(&mut from)?;
        if !from.is_done() {
            return None;
        }
        if checks_what_it_read() {
            assert!(bytes_of(&value) == bytes, "what the daemon sent reads back as something else");
        }
        Some(value)
    }

    fn read_signatures(bytes: &[u8]) -> Option<fennec_check::Signatures> {
        let signatures = fennec_check::Signatures::from_bytes(bytes)?;
        if checks_what_it_read() {
            assert!(
                signatures.to_bytes() == bytes,
                "what the daemon's check found reads back as something else"
            );
        }
        Some(signatures)
    }
}

#[cfg(unix)]
pub(crate) use unix::{after_builds, built, serve, tests_built};

/// A build in a child, where there are no daemons: never.
#[cfg(not(unix))]
pub(crate) fn built(
    _program: &fennec_kit::Program,
    _output: &Path,
    _release: bool,
    _checked: Option<&fennec_check::Signatures>,
) -> Result<Result<Built, String>, NoAnswer> {
    Err(NoAnswer("builds go to their own process only where there's a daemon".to_owned()))
}

/// A build of tests in a child, where there are no daemons: never.
#[cfg(not(unix))]
pub(crate) fn tests_built(
    _module: &fennec_syntax::Module,
    _tests: &[String],
    _output: &Path,
) -> Result<Result<Vec<String>, String>, NoAnswer> {
    Err(NoAnswer("builds go to their own process only where there's a daemon".to_owned()))
}

#[cfg(not(unix))]
pub(crate) fn serve() -> std::process::ExitCode {
    eprintln!("error: `fennec {COMMAND}` is run by the daemon, which this system doesn't have");
    std::process::ExitCode::FAILURE
}

/// How a test asks a child to fail, in a dev build of Fennec only: `crash` before it reads its
/// request, `hang` before it says it started, and `hang-building:<file>` after, starting what it
/// builds and writing its process's id to the file first, when the file isn't there.
#[cfg(unix)]
fn test_failure() -> Option<String> {
    cfg!(debug_assertions).then(|| std::env::var("FENNEC_TEST_BUILD_CHILD").ok()).flatten()
}

/// Set once this process is the daemon, every build of which goes to a child.
static SERVING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The daemon's home, where what a child's check found wrong is reported.
#[cfg(unix)]
static HOME: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Makes this process's builds go to children from here on: it's the daemon, serving `home`.
#[cfg(unix)]
pub(crate) fn serving(home: &Path) {
    SERVING.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = HOME.set(home.to_path_buf());
}

/// Whether builds go to a child: the daemon's do, and any process's with `FENNEC_BUILD_CHILD=1`.
/// `FENNEC_BUILD_CHILD=0` keeps the daemon's in the daemon.
pub(crate) fn wanted() -> bool {
    match std::env::var("FENNEC_BUILD_CHILD").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => SERVING.load(std::sync::atomic::Ordering::Relaxed),
    }
}

/// Says why a build in a child gave no answer, before it's built here.
pub(crate) fn said_no_answer(NoAnswer(why): &NoAnswer, output: &Path) {
    eprintln!(
        "note: building {} in its own process gave no answer, so it's built here: {why}",
        output.display()
    );
}
