//! Compiling modules with `opt` and `llc`, a function at a time, through the cache.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use fennec_build::Cache;

use crate::emit::Unit;

const KIND: &str = "llvm";

/// How many of a dev build's functions are compiled at once: one for each core, unless
/// `FENNEC_BUILD_JOBS` gives a number. It was four on any machine, which left an 18-core Mac
/// starting LLVM's tools three at a time for a program of thousands of functions (#1902).
pub(crate) fn workers() -> usize {
    std::env::var("FENNEC_BUILD_JOBS")
        .ok()
        .and_then(|jobs| jobs.parse().ok())
        .filter(|&jobs: &usize| jobs > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from))
}
const PIPELINE: &str = "default<O3>";

/// Allocas times blocks past which a function is compiled as written, with `llc -O0`, rather than
/// optimized. Promoting a function's allocas to registers costs about that product in `opt`, and
/// `llc`'s register allocation grows faster than the function, so a generated `main` of thousands
/// of lines took minutes. The biggest function of the bench kernels, the examples and the standard
/// library, which `toml` uses, is at 1.3 million; the fuzzer's 650-line `main` is at 65 million.
/// Its code keeps every check and only runs slower.
const HUGE: usize = 32_000_000;

/// Units by where they are among a program's.
type Numbered<'u> = Vec<(usize, &'u Unit)>;

/// How a module is compiled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Way {
    /// Through `opt`, then `llc -O3`: a function.
    Optimized,
    /// Straight to `llc -O3`: the entry, which is data and a little code.
    AsWritten,
    /// Straight to `llc -O0`: a huge function.
    Quickly,
}

/// The sanitizer's own passes, which a huge function still takes, unoptimized.
fn sanitizer_passes() -> Option<&'static str> {
    match sanitizer() {
        Some("address") => Some("asan"),
        Some(_) => Some("tsan-module,function(tsan)"),
        None => None,
    }
}

/// The name a line of LLVM's text declares or defines, unquoted.
fn symbol(line: &str) -> Option<String> {
    let at = line.find('@')?;
    let rest = line.get(at + 1..)?;
    if let Some(quoted) = rest.strip_prefix('"') {
        let name = quoted.get(..quoted.find('"')?)?;
        // A quoted name writes a byte it can't hold as `\` and two hex digits.
        let mut bytes = Vec::with_capacity(name.len());
        let mut chars = name.bytes();
        while let Some(byte) = chars.next() {
            if byte == b'\\' {
                let digits: Vec<u8> = chars.by_ref().take(2).collect();
                let hex = std::str::from_utf8(&digits).ok()?;
                bytes.push(u8::from_str_radix(hex, 16).ok()?);
            } else {
                bytes.push(byte);
            }
        }
        String::from_utf8(bytes).ok()
    } else {
        let end =
            rest.find(|c: char| !(c.is_ascii_alphanumeric() || "_.$-".contains(c))).unwrap_or(rest.len());
        Some(rest.get(..end)?.to_owned()).filter(|name| !name.is_empty())
    }
}

/// What a release keeps of the whole program for the huge functions compiled apart: every name
/// they reach outside themselves stays seen from outside, as they call it, and what's declared
/// joins the whole as used, so `llvm-link` brings in what the runtime defines of it. Gives back
/// the module that declares them, and the names.
fn kept_for(huge: &[&Unit]) -> Result<(String, Vec<String>), String> {
    let defined: std::collections::BTreeSet<String> = huge
        .iter()
        .flat_map(|unit| unit.text.lines())
        .filter(|line| line.starts_with("define ") && !line.starts_with("define available_externally "))
        .filter_map(symbol)
        .collect();
    let mut declared = std::collections::BTreeMap::new();
    let mut names = std::collections::BTreeSet::new();
    for line in huge.iter().flat_map(|unit| unit.text.lines()) {
        let external = line.starts_with('@') && line.contains(" = external ");
        let copy = line.starts_with("define available_externally ");
        if !(external || copy || line.starts_with("declare ")) {
            continue;
        }
        let Some(name) = symbol(line).filter(|name| !name.starts_with("llvm.") && !defined.contains(name))
        else {
            continue;
        };
        if !copy {
            // Without the attribute sets it names, which this module doesn't have.
            let words: Vec<&str> =
                line.split(' ').filter(|word| !(word.starts_with('#') && word.len() > 1)).collect();
            declared.entry(name.clone()).or_insert_with(|| words.join(" "));
        }
        names.insert(name);
    }
    let mut module = String::from(crate::header()?);
    for line in declared.values() {
        module.push_str(line);
        module.push('\n');
    }
    let used: Vec<String> = declared
        .keys()
        .map(|name| format!("ptr @\"{}\"", name.replace('\\', "\\5C").replace('"', "\\22")))
        .collect();
    if !used.is_empty() {
        module.push_str(&format!(
            "@llvm.used = appending global [{} x ptr] [{}], section \"llvm.metadata\"\n",
            used.len(),
            used.join(", ")
        ));
    }
    Ok((module, names.into_iter().collect()))
}

/// The functions of `units` too big to optimize, which are compiled as written and run slower,
/// for a build to say so. Only a module of more than `SMALLEST_HUGE` bytes is looked through.
#[must_use]
pub fn huge_functions(units: &[Unit]) -> Vec<String> {
    units.iter().filter(|unit| unit.huge).map(|unit| unit.function.clone()).collect()
}

/// Whether a module defines a function too big to optimize. Only a module of more than
/// `SMALLEST_HUGE` bytes is looked through.
pub(crate) fn is_huge(text: &str) -> bool {
    text.len() > SMALLEST_HUGE && huge(text)
}

/// The fewest bytes a module of a huge function has: one has thousands of allocas and of blocks,
/// a line each.
const SMALLEST_HUGE: usize = 100_000;

/// Whether a module defines a function too big to optimize: see [`HUGE`]. Only what it defines
/// counts, not the copies of others it has for inlining.
fn huge(text: &str) -> bool {
    let (mut allocas, mut blocks, mut inside) = (0_usize, 0_usize, false);
    for line in text.lines() {
        if line.starts_with("define ") {
            inside = !line.starts_with("define available_externally ");
            (allocas, blocks) = (0, 1);
        } else if line.starts_with('}') {
            if inside && allocas.saturating_mul(blocks) >= HUGE {
                return true;
            }
            inside = false;
        } else if inside {
            if line.contains(" = alloca ") {
                allocas += 1;
            } else if !line.starts_with(' ') && line.ends_with(':') {
                blocks += 1;
            }
        }
    }
    false
}

/// The sanitizer `FENNEC_SANITIZE` asks for, to test built programs: `address`, that they touch
/// only memory they own, or `thread`, that their threads don't race. Every function with
/// attributes is instrumented, after the rest of the pipeline, as Clang does it, and the link
/// takes what [`sanitize_flag`] gives.
pub(crate) fn sanitizer() -> Option<&'static str> {
    match std::env::var("FENNEC_SANITIZE").as_deref() {
        Ok("address") => Some("address"),
        Ok("thread") => Some("thread"),
        _ => None,
    }
}

/// What the link takes for the sanitizer asked for, which brings in its runtime.
#[must_use]
pub fn sanitize_flag() -> Option<String> {
    sanitizer().map(|name| format!("-fsanitize={name}"))
}

/// Every switch outside the program that changes what a build makes, as a line for a cache key: the
/// sanitizer, whether proofs are checked and facts stripped, whether results and other types are
/// kept in place, whether a flat message crosses a channel as its words, and whether it's built for
/// a heap audit.
/// A program built one way is never handed back for another, as a plain one was for a build asked
/// for the address sanitizer. The objects' own key has the pipeline, which names the sanitizer's
/// passes; checked proofs, stripped facts and boxed results change the text too. Said here as
/// well, so no key leans on that.
#[must_use]
pub fn output_switches() -> String {
    format!(
        "sanitize={:?} proofs_checked={} facts_stripped={} results_in_place={} types_in_place={} dict_values_in_place={} messages_as_words={} named_conn_calls={} audited={}",
        sanitizer(),
        crate::emit::proofs_checked(),
        crate::emit::facts_stripped(),
        fennec_layout::results_in_place(),
        fennec_layout::types_in_place(),
        fennec_layout::dict_values_in_place(),
        crate::channels::messages_as_words(),
        crate::conn_calls::named_conn_calls(),
        audited()
    )
}

/// Whether the program is built for a heap audit, as `FENNEC_AUDIT=1` asks: one that checks every
/// block's owners as it ends. Generated code keeps no native blocks then, since the audit walks
/// only what the runtime holds and can't see what a native block does. It changes what a build
/// makes, so a build's cache key says it.
#[must_use]
pub fn audited() -> bool {
    std::env::var("FENNEC_AUDIT").as_deref() == Ok("1")
}

/// The passes `opt` runs.
fn pipeline() -> String {
    match sanitizer() {
        Some("address") => format!("{PIPELINE},asan"),
        Some(_) => format!("{PIPELINE},tsan-module,function(tsan)"),
        None => PIPELINE.to_owned(),
    }
}

/// A module's text, marked for the sanitizer asked for: it only instruments functions that say
/// so, and they all take one of its attribute sets.
fn marked(text: &str) -> std::borrow::Cow<'_, str> {
    let Some(name) = sanitizer() else {
        return text.into();
    };
    let lines = text.lines().map(|line| {
        if line.starts_with("attributes #") {
            line.replacen(" = { ", &format!(" = {{ sanitize_{name} "), 1)
        } else {
            line.to_owned()
        }
    });
    lines.collect::<Vec<_>>().join("\n").into()
}

/// LLVM's programs.
#[derive(Clone, Debug)]
pub struct Tools {
    opt: PathBuf,
    llc: PathBuf,
    link: PathBuf,
}

impl Tools {
    /// From `FENNEC_LLVM_BIN`, the `llvm/bin` folder a release has beside `fennec`, or the Rust
    /// toolchain's `llvm-tools` component. Nothing's downloaded; what's missing is reported. Where
    /// the toolchain keeps them is remembered in the Fennec home, as asking `rustc` each build
    /// costs more than the rest of an unchanged one.
    pub fn find() -> Result<Self, String> {
        let remembered = fennec_build::fennec_home().map(|home| home.join("llvm-tools"));
        let folder = if let Some(folder) = std::env::var_os("FENNEC_LLVM_BIN") {
            PathBuf::from(folder)
        } else if let Some(folder) = shipped_tools() {
            folder
        } else if let Some(folder) = remembered.as_deref().and_then(remembered_folder) {
            folder
        } else {
            let folder = toolchain_tools()
                .map_err(|err| format!("LLVM's tools aren't in `llvm/bin` beside `fennec`, and {err}"))?;
            if let Some(at) = &remembered {
                remember(at, &folder.to_string_lossy());
            }
            folder
        };
        let tools = Tools {
            opt: folder.join(tool("opt")),
            llc: folder.join(tool("llc")),
            link: folder.join(tool("llvm-link")),
        };
        if tools.opt.is_file() && tools.llc.is_file() {
            Ok(tools)
        } else {
            if let Some(at) = &remembered {
                let _ = std::fs::remove_file(at);
            }
            Err(format!(
                "LLVM's `opt` and `llc` aren't in {}; a release has them in `llvm/bin` beside `fennec`, or run `rustup component add llvm-tools`, or set FENNEC_LLVM_BIN",
                folder.display()
            ))
        }
    }

    /// The folder they're in.
    #[must_use]
    pub fn folder(&self) -> &Path {
        self.llc.parent().unwrap_or_else(|| Path::new(""))
    }

    /// What makes one version of the tools differ from another, for cache keys: remembered
    /// beside `llc`'s size and time, so it's only asked again when `llc` changes.
    ///
    /// # Errors
    /// When `llc` can't be run to ask.
    pub fn version(&self) -> Result<String, String> {
        let stamp = std::fs::metadata(&self.llc)
            .ok()
            .map(|meta| format!("{} {:?}", meta.len(), meta.modified().ok()));
        let remembered = fennec_build::fennec_home().map(|home| home.join("llvm-version"));
        if let (Some(stamp), Some(at)) = (&stamp, &remembered)
            && let Ok(text) = std::fs::read_to_string(at)
            && let Some((was, version)) = text.split_once('\n')
            && was == stamp
        {
            return Ok(version.to_owned());
        }
        let out = Command::new(&self.llc).arg("--version").output().map_err(|err| err.to_string())?;
        let version = String::from_utf8_lossy(&out.stdout).lines().take(2).collect::<Vec<_>>().join(" ");
        if let (Some(stamp), Some(at)) = (stamp, remembered) {
            remember(&at, &format!("{stamp}\n{version}"));
        }
        Ok(version)
    }
}

/// Keeps `text` in the file at `at` for later builds, written beside it and renamed into place, so
/// a build reading it at the same moment, as another command sharing the Fennec home may, finds
/// the old text or the new and never part of one: half a version would name every function's
/// object as new. One that can't be written is asked again next time.
fn remember(at: &Path, text: &str) {
    let writing = at.with_extension(format!("{}.writing", std::process::id()));
    if std::fs::write(&writing, text).is_err() || std::fs::rename(&writing, at).is_err() {
        let _ = std::fs::remove_file(&writing);
    }
}

/// The folder remembered for the toolchain's tools, if they're still there.
fn remembered_folder(at: &Path) -> Option<PathBuf> {
    let folder = PathBuf::from(std::fs::read_to_string(at).ok()?.trim());
    (folder.join(tool("opt")).is_file() && folder.join(tool("llc")).is_file()).then_some(folder)
}

/// A tool's file name on this machine.
fn tool(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// The tools a release ships in `llvm/bin` beside `fennec`, if this is one.
fn shipped_tools() -> Option<PathBuf> {
    let folder = std::env::current_exe().ok()?.with_file_name("llvm").join("bin");
    (folder.join(tool("opt")).is_file() && folder.join(tool("llc")).is_file()).then_some(folder)
}

fn toolchain_tools() -> Result<PathBuf, String> {
    let rustc = |arg: &str| {
        Command::new("rustc")
            .args(["--print", arg])
            .output()
            .map_err(|err| format!("couldn't run `rustc` to find LLVM's tools: {err}"))
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    let root = rustc("sysroot")?;
    let host = rustc("host-tuple")?;
    Ok(Path::new(&root).join("lib/rustlib").join(host).join("bin"))
}

/// An object and whether it's fresh, or why it couldn't be made.
type Compiled = Result<(Object, bool), String>;

/// One object to link: its bytes, or the cache's file holding them, which the linker reads where
/// it lies. A dev build's functions are kept files, so a build after an edit writes out only the
/// objects it compiled, where it wrote every one before and deleted them after: on a Mac that was
/// most of the build (`Cache::keep_object`).
#[derive(Debug)]
pub enum Object {
    Bytes(Vec<u8>),
    Kept(PathBuf),
}

/// What compiling gave: every object, the fresh ones counted.
#[derive(Debug, Default)]
pub struct Built {
    pub objects: Vec<Object>,
    pub compiled: usize,
}

fn one(tools: &Tools, text: &str, work: &Path, name: &str, way: Way) -> Result<Vec<u8>, String> {
    let input = work.join(format!("{name}.ll"));
    let middle = work.join(format!("{name}.bc"));
    let output = work.join(format!("{name}.o"));
    std::fs::write(&input, marked(text).as_bytes())
        .map_err(|err| format!("couldn't write {}: {err}", input.display()))?;
    let run = |command: &mut Command, what: &str| {
        let done = command.output().map_err(|err| format!("couldn't run {what}: {err}"))?;
        if done.status.success() {
            Ok(())
        } else {
            Err(format!("{what} failed on `{name}`:\n{}", String::from_utf8_lossy(&done.stderr)))
        }
    };
    let passes = match way {
        Way::Optimized => Some(pipeline()),
        Way::Quickly => sanitizer_passes().map(str::to_owned),
        Way::AsWritten => None,
    };
    let compiled_from = if let Some(passes) = passes {
        run(
            Command::new(&tools.opt).arg(format!("-passes={passes}")).arg(&input).arg("-o").arg(&middle),
            "opt",
        )?;
        &middle
    } else {
        &input
    };
    run(
        Command::new(&tools.llc)
            .args(llc_levels(way))
            .args(["-filetype=obj", "-relocation-model=pic"])
            .arg(compiled_from)
            .arg("-o")
            .arg(&output),
        "llc",
    )?;
    std::fs::read(&output).map_err(|err| unreadable(&output, &err))
}

/// How hard `llc` works on a module compiled `way`. A huge function is compiled at `-O0`, but
/// through the same instruction selection as `-O3` rather than the fast one. On x86-64 the fast
/// one folds the load of a call's error into a compare against memory, and that compare reads
/// the error pointer the function was given, not the error the call left in `r12`. So every
/// failure check read address zero, and the program crashed.
fn llc_levels(way: Way) -> &'static [&'static str] {
    match way {
        Way::Quickly => &["-O0", "-fast-isel=false"],
        Way::Optimized | Way::AsWritten => &["-O3"],
    }
}

/// A name as `-internalize-public-api-file` reads it, which is as a pattern: `[`, `*`, `?`, `{`
/// and the rest are escaped, so a generic's instance like `f_twice[int]` names itself rather than
/// any one of `f_twicei`, `f_twicen` and `f_twicet`, which left it internal and unlinked.
fn as_pattern(name: &str) -> String {
    let mut pattern = String::with_capacity(name.len());
    for character in name.chars() {
        if matches!(character, '\\' | '[' | ']' | '*' | '?' | '{' | '}') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern
}

/// For a release: every function's module and `entry` linked into one, optimized as a whole,
/// with nothing but `main` seen from outside, so LLVM inlines and specializes across the whole
/// program. Given the runtime's own bitcode, what the program uses of it joins the unit too, so
/// its calls inline as well. Nothing's cached, as any change changes the whole. A huge function
/// (see `HUGE`) is compiled apart, as written, and what it reaches stays as it calls it.
pub fn compile_whole(
    tools: &Tools,
    units: &[Unit],
    entry: &str,
    work: &Path,
    runtime: Option<&Path>,
) -> Result<Built, String> {
    std::fs::create_dir_all(work).map_err(|err| format!("couldn't make {}: {err}", work.display()))?;
    let mut inputs = Vec::with_capacity(units.len() + 2);
    let (apart, together): (Numbered<'_>, Numbered<'_>) =
        units.iter().enumerate().partition(|(_, unit)| unit.huge);
    let kept = work.join("kept.txt");
    let mut keep = vec!["main".to_owned()];
    if !apart.is_empty() {
        let (module, names) = kept_for(&apart.iter().map(|(_, unit)| *unit).collect::<Vec<_>>())?;
        let input = work.join("kept.ll");
        std::fs::write(&input, module).map_err(|err| format!("couldn't write {}: {err}", input.display()))?;
        inputs.push(input);
        keep.extend(names);
    }
    let patterns: Vec<String> = keep.iter().map(|name| as_pattern(name)).collect();
    std::fs::write(&kept, patterns.join("\n") + "\n")
        .map_err(|err| format!("couldn't write {}: {err}", kept.display()))?;
    for (at, unit) in together {
        let input = work.join(format!("f{at}.ll"));
        std::fs::write(&input, marked(&unit.text).as_bytes())
            .map_err(|err| format!("couldn't write {}: {err}", input.display()))?;
        inputs.push(input);
    }
    let entry_at = work.join("entry.ll");
    std::fs::write(&entry_at, entry)
        .map_err(|err| format!("couldn't write {}: {err}", entry_at.display()))?;
    inputs.push(entry_at);
    // A huge function compiles beside the rest.
    std::thread::scope(|scope| {
        let quick: Vec<_> = apart
            .iter()
            .map(|(at, unit)| {
                scope.spawn(move || one(tools, &unit.text, work, &format!("f{at}"), Way::Quickly))
            })
            .collect();
        let mut objects = vec![Object::Bytes(whole_object(tools, &inputs, &kept, work, runtime)?)];
        for compiling in quick {
            objects.push(Object::Bytes(
                compiling
                    .join()
                    .unwrap_or_else(|_| Err("compiling a huge function stopped unexpectedly".to_owned()))?,
            ));
        }
        Ok(Built { objects, compiled: units.len() })
    })
}

/// Runs one of LLVM's tools on the whole program, with what it says when it fails.
fn run(command: &mut Command, what: &str) -> Result<(), String> {
    let done = command.output().map_err(|err| format!("couldn't run {what}: {err}"))?;
    if done.status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed on the whole program:\n{}", String::from_utf8_lossy(&done.stderr)))
    }
}

/// How many functions' modules `llvm-link` merges into one before those are linked together
/// (see [`linked`]).
const LINK_GROUP: usize = 512;

/// How many groups are linked at once. Each holds its group's modules, about 200 MB for 512 of
/// a big program's; the last link, of the groups together, holds the whole program, as linking
/// it in one did.
const LINKING_AT_ONCE: usize = 4;

/// `inputs` linked into `whole`, in order. A big program's thousands of modules are linked in
/// groups of `group`, [`LINK_GROUP`] in a build, several at once, and then the groups together,
/// in order. Merging a
/// module into a large one costs more the larger that is, and stalls on memory, so linking them
/// all into one grew faster than the program: its 35,000 modules at 200k lines took 439 G cycles
/// that way, and 162 G in two levels, for the same instructions. The modules and their order are
/// the same either way, and so is the program. A group that fails says so, the first by place.
fn linked(
    tools: &Tools,
    (inputs, group): (&[PathBuf], usize),
    work: &Path,
    whole: &Path,
) -> Result<(), String> {
    if inputs.len() <= group {
        return link_listed(tools, inputs, &work.join("inputs.rsp"), whole);
    }
    let groups: Vec<(&[PathBuf], PathBuf)> = inputs
        .chunks(group.max(1))
        .enumerate()
        .map(|(at, group)| (group, work.join(format!("group{at}.bc"))))
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failed: Mutex<Vec<(usize, String)>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..workers().min(LINKING_AT_ONCE).min(groups.len()) {
            scope.spawn(|| {
                loop {
                    let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((group, output)) = groups.get(at) else { return };
                    let listed = work.join(format!("group{at}.rsp"));
                    if let Err(why) = link_listed(tools, group, &listed, output)
                        && let Ok(mut failed) = failed.lock()
                    {
                        failed.push((at, why));
                    }
                }
            });
        }
    });
    let mut failed = failed.into_inner().map_err(|_| "a linking thread stopped unexpectedly".to_owned())?;
    failed.sort_by_key(|(at, _)| *at);
    if let Some((_, why)) = failed.into_iter().next() {
        return Err(why);
    }
    let outputs: Vec<PathBuf> = groups.into_iter().map(|(_, output)| output).collect();
    link_listed(tools, &outputs, &work.join("groups.rsp"), whole)
}

/// `inputs` linked into `output` by one `llvm-link`. They go to it in a file, one to a line,
/// `listed`: on the command line, a big program's thousands go past what Windows lets a command
/// be. Each is quoted, and on Windows written with `/`, so no `\` in one is read as an escape.
fn link_listed(tools: &Tools, inputs: &[PathBuf], listed: &Path, output: &Path) -> Result<(), String> {
    let line = |input: &PathBuf| {
        let path = input.display().to_string();
        format!("\"{}\"\n", if cfg!(windows) { path.replace('\\', "/") } else { path })
    };
    let lines: String = inputs.iter().map(line).collect();
    std::fs::write(listed, lines).map_err(|err| format!("couldn't write {}: {err}", listed.display()))?;
    run(Command::new(&tools.link).arg(format!("@{}", listed.display())).arg("-o").arg(output), "llvm-link")
}

/// The whole program's object: `inputs` linked, with the runtime's bitcode if it's given,
/// optimized with nothing seen from outside but the names in `kept`, and compiled.
fn whole_object(
    tools: &Tools,
    inputs: &[PathBuf],
    kept: &Path,
    work: &Path,
    runtime: Option<&Path>,
) -> Result<Vec<u8>, String> {
    let (whole, optimized, output) = (work.join("whole.bc"), work.join("optimized.bc"), work.join("whole.o"));
    if !tools.link.is_file() {
        return Err(format!("LLVM's `llvm-link` isn't beside `opt` in {}", tools.link.display()));
    }
    linked(tools, (inputs, LINK_GROUP), work, &whole)?;
    if let Some(runtime) = runtime {
        let program = work.join("program.bc");
        std::fs::rename(&whole, &program)
            .map_err(|err| format!("couldn't move {}: {err}", whole.display()))?;
        run(
            Command::new(&tools.link).arg(&program).arg("--only-needed").arg(runtime).arg("-o").arg(&whole),
            "llvm-link",
        )?;
    }
    run(
        Command::new(&tools.opt)
            .arg(format!("-passes=internalize,{}", pipeline()))
            .arg(format!("-internalize-public-api-file={}", kept.display()))
            .arg(&whole)
            .arg("-o")
            .arg(&optimized),
        "opt",
    )?;
    run(
        Command::new(&tools.llc)
            .args(["-O3", "-filetype=obj", "-relocation-model=pic"])
            .arg(&optimized)
            .arg("-o")
            .arg(&output),
        "llc",
    )?;
    std::fs::read(&output).map_err(|err| unreadable(&output, &err))
}

/// What a tool's output that can't be read is said as: with whether the folder it was written in
/// is still there, and what that holds, so a folder that went is told from a file that did.
fn unreadable(output: &Path, err: &std::io::Error) -> String {
    let shown = output.display();
    let Some(listing) = output.parent().and_then(|folder| std::fs::read_dir(folder).ok()) else {
        return format!("couldn't read {shown}: {err}, and the folder it was written in is gone too");
    };
    let mut names: Vec<String> =
        listing.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    let held = match names.len() {
        0 => "nothing".to_owned(),
        count if count > 12 => {
            format!("{count} files, starting {}", names.get(..12).unwrap_or_default().join(", "))
        }
        _ => names.join(", "),
    };
    format!("couldn't read {shown}: {err}; the folder it was written in is there, holding {held}")
}

/// What compiling says when a unit used again without writing it has lost its object from the
/// cache: it has no text to compile again from, so the build is made again writing every unit.
pub const USED_AGAIN_OBJECT_GONE: &str =
    "a unit used again lost its compiled object, and has no text to compile";

/// One function's module compiled on its own, with no cache: for a dev build whose kept object
/// for it went from the cache before the link.
///
/// # Errors
/// When LLVM's tools fail on it, and [`USED_AGAIN_OBJECT_GONE`] for a unit used again, which has
/// no text.
pub fn compile_again(tools: &Tools, unit: &Unit, work: &Path) -> Result<Vec<u8>, String> {
    if unit.text.is_empty() {
        return Err(USED_AGAIN_OBJECT_GONE.to_owned());
    }
    std::fs::create_dir_all(work).map_err(|err| format!("couldn't make {}: {err}", work.display()))?;
    let way = if unit.huge { Way::Quickly } else { Way::Optimized };
    one(tools, &unit.text, work, "again", way)
}

/// Whether the cache holds the object a unit with this text's hash compiles to, as `tools` and
/// this build's passes make it: a build uses a unit again without writing it only then. It's
/// found without marking it, as the program's list holds it (see [`hold_objects`]).
///
/// # Errors
/// When `llc` can't be run to ask its version.
///
/// In the daemon, an object the last build found or made is known to be there still when no
/// cleanup has taken anything since (`Cache::removals`), and isn't looked for. Should it be gone
/// anyway, the link finds that, and the build writes every unit again (`USED_AGAIN_OBJECT_GONE`).
///
/// Gives back too what the objects it finds are known under, in the daemon, for this build to hand
/// to [`objects_known`] once it has linked them.
pub fn has_object(
    cache: &Cache,
    tools: &Tools,
) -> Result<(impl Fn(&fennec_blake3::Hash) -> bool, Option<KnownUnder>), String> {
    let (version, pipeline) = (tools.version()?, pipeline());
    let cache = cache.clone();
    let (known, under) = if crate::kept::keeps_between_builds() {
        let now =
            Objects { removals: cache.removals(), version: version.clone(), pipeline: pipeline.clone() };
        let known = KNOWN.lock().ok().and_then(|mut known| known.take()).filter(|(then, _)| *then == now);
        (known.map(|(_, known)| known).unwrap_or_default(), Some(KnownUnder(now)))
    } else {
        (HashSet::new(), None)
    };
    let found = move |text: &fennec_blake3::Hash| {
        known.contains(text.as_bytes())
            || cache.held_object(KIND, &object_key(&version, &pipeline, text)).is_some()
    };
    Ok((found, under))
}

/// What the objects a build found are known under, as it started looking for them (see
/// [`has_object`]): handed back to [`objects_known`] by that build alone.
pub struct KnownUnder(Objects);

/// What the objects a build knows are there were known under: how many cleanups had taken
/// anything from the cache as it started, and the LLVM and passes that name its objects.
#[derive(Clone, PartialEq, Eq)]
struct Objects {
    removals: u64,
    version: String,
    pipeline: String,
}

/// The objects this process's last build found or made, by their units' texts' hashes, and what
/// they were known under. Only the daemon keeps them (`kept::keeps_between_builds`).
static KNOWN: Mutex<Option<(Objects, HashSet<[u8; 32]>)>> = Mutex::new(None);

/// Notes that every object of `units` is in the cache, as a build that has just linked them knows,
/// for the next build in this process to find without looking (see [`has_object`]), known under
/// what `under` says, which that build was given as it started. A build that didn't look for its
/// objects, as a release build doesn't, has nothing to note.
pub fn objects_known(under: Option<KnownUnder>, units: &[Unit]) {
    let Some(KnownUnder(started)) = under else { return };
    let known = units.iter().map(|unit| *unit.hash.as_bytes()).collect();
    if let Ok(mut kept) = KNOWN.lock() {
        *kept = Some((started, known));
    }
}

/// Keeps the list of the objects `units` compile to as what the program `name` holds, so cleanup
/// counts each as used whenever the list is marked: a build marks it once as it starts, rather
/// than marking each object it uses again (`fennec_build::Cache::hold`).
///
/// # Errors
/// When `llc` can't be run to ask its version, or the list can't be written.
pub fn hold_objects(cache: &Cache, tools: &Tools, name: &str, units: &[Unit]) -> Result<(), String> {
    holding(tools, units)?
        .hold(cache, name)
        .map_err(|err| format!("couldn't keep the program's list of objects: {err}"))
}

/// The list of the objects `units` compile to, to keep later, as the daemon does once a build has
/// answered: what names each is worked out then too (see [`Holding::hold`]).
///
/// # Errors
/// When `llc` can't be run to ask its version.
pub fn holding(tools: &Tools, units: &[Unit]) -> Result<Holding, String> {
    Ok(Holding {
        version: tools.version()?,
        pipeline: pipeline(),
        texts: units.iter().map(|unit| unit.hash).collect(),
    })
}

/// The objects a program holds, as [`holding`] took them, to keep as its list.
pub struct Holding {
    version: String,
    pipeline: String,
    texts: Vec<fennec_blake3::Hash>,
}

impl Holding {
    /// Keeps the list as what the program `name` holds (see [`hold_objects`]).
    ///
    /// # Errors
    /// When the list can't be written.
    pub fn hold(self, cache: &Cache, name: &str) -> std::io::Result<()> {
        let objects: Vec<String> =
            self.texts.iter().map(|text| object_key(&self.version, &self.pipeline, text)).collect();
        cache.hold(name, KIND, &objects)
    }
}

/// What names a function's compiled object in the cache: the hash of its module's text, with the
/// LLVM it's compiled by and how.
fn object_key(version: &str, pipeline: &str, text: &fennec_blake3::Hash) -> String {
    fennec_blake3::hash(format!("{version}\n{pipeline}\n{}", text.to_hex()).as_bytes()).to_hex()
}

/// What a build asked to stop gives instead of its objects.
pub const STOPPED: &str = "the build was stopped";

/// Compiles every function's module, using the cache's copy of any it already has, and `entry`.
/// The cache comes with the hash of each unit's text, in order, which the build has already
/// worked out for its root hash.
pub fn compile(
    tools: &Tools,
    units: &[Unit],
    entry: &str,
    cache: Option<&Cache>,
    work: &Path,
) -> Result<Built, String> {
    std::fs::create_dir_all(work).map_err(|err| format!("couldn't make {}: {err}", work.display()))?;
    let version = tools.version()?;
    let pipeline = pipeline();
    let threads = workers();
    let next = Mutex::new(0_usize);
    let results: Mutex<Vec<(usize, Compiled)>> = Mutex::new(Vec::with_capacity(units.len()));
    // The entry is made fresh each build and needs nothing of the functions, so it's compiled
    // beside them rather than after.
    let entry = std::thread::scope(|scope| {
        let entry = scope.spawn(|| one(tools, entry, work, "entry", Way::AsWritten));
        for _ in 0..threads.saturating_sub(1).max(1) {
            scope.spawn(|| {
                loop {
                    // A build asked to stop compiles nothing more (`fennec_build::cancel`).
                    if fennec_build::cancelled() {
                        return;
                    }
                    let at = {
                        let Ok(mut next) = next.lock() else { return };
                        let at = *next;
                        *next += 1;
                        at
                    };
                    let Some(unit) = units.get(at) else { return };
                    let keyed = cache.map(|cache| (cache, object_key(&version, &pipeline, &unit.hash)));
                    // The cache's own file, which the linker reads where it lies. A unit used again
                    // without writing it is one the program's list holds, which the build marked as
                    // it started; any other found is marked, so cleanup leaves it while it links.
                    let cached = keyed.as_ref().and_then(|(cache, key)| {
                        let found = if unit.text.is_empty() {
                            cache.held_object(KIND, key)
                        } else {
                            cache.kept_object(KIND, key)
                        };
                        found.map(Object::Kept)
                    });
                    let result = match cached {
                        Some(object) => Ok((object, false)),
                        // A unit used again without writing it has no text to compile: it was
                        // used because its object was there, so cleanup took it since.
                        None if unit.text.is_empty() => Err(USED_AGAIN_OBJECT_GONE.to_owned()),
                        None => {
                            // Which way is the text's own doing, so the key needn't say.
                            let way = if unit.huge { Way::Quickly } else { Way::Optimized };
                            one(tools, &unit.text, work, &format!("f{at}"), way)
                        }
                        .map(|bytes| match &keyed {
                            // A failed write only means compiling it again next time, and linking
                            // these bytes this time.
                            Some((cache, key)) => {
                                let kept = cache.keep_object(KIND, key, &bytes);
                                match fennec_build::kept("keep a compiled function", kept).flatten() {
                                    Some(kept) => (Object::Kept(kept), true),
                                    None => (Object::Bytes(bytes), true),
                                }
                            }
                            None => (Object::Bytes(bytes), true),
                        }),
                    };
                    if let Ok(mut results) = results.lock() {
                        results.push((at, result));
                    }
                }
            });
        }
        entry.join().unwrap_or_else(|_| Err("compiling the entry stopped unexpectedly".to_owned()))
    });
    let mut results =
        results.into_inner().map_err(|_| "a compiling thread stopped unexpectedly".to_owned())?;
    if fennec_build::cancelled() {
        return Err(STOPPED.to_owned());
    }
    results.sort_by_key(|(at, _)| *at);
    let mut built = Built::default();
    for (_, result) in results {
        let (object, fresh) = result?;
        built.objects.push(object);
        built.compiled += usize::from(fresh);
    }
    built.objects.push(Object::Bytes(entry?));
    Ok(built)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A remembered file read while it's written again is the old text or the new, whole.
    #[test]
    fn a_remembered_file_is_never_read_half_written() {
        let dir = std::env::temp_dir().join(format!("fennec-remember-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let at = dir.join("llvm-version");
        let (short, long) = ("1 stamp\nLLVM 19".to_owned(), format!("2 stamp\n{}", "LLVM 19 ".repeat(4096)));
        remember(&at, &short);
        let writing = std::thread::spawn({
            let (at, short, long) = (at.clone(), short.clone(), long.clone());
            move || {
                for n in 0..400 {
                    remember(&at, if n % 2 == 0 { &long } else { &short });
                }
            }
        });
        let mut reads = 0;
        while !writing.is_finished() {
            let read = std::fs::read_to_string(&at).unwrap();
            assert!(read == short || read == long, "read {} bytes of neither", read.len());
            reads += 1;
        }
        writing.join().unwrap();
        assert!(reads > 0);
        let left: Vec<_> =
            std::fs::read_dir(&dir).unwrap().flatten().map(|entry| entry.file_name()).collect();
        assert_eq!(left.len(), 1, "nothing but the file is left: {left:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A program whose `long` makes a list and a string on each of `lines` lines, which only it
    /// formats.
    pub(crate) fn long_program(lines: usize) -> String {
        let mut body = String::new();
        for line in 0..lines {
            body.push_str(&format!(
                "    total += at_or_zero([{line}, {line} + 1], {line} % 2) + len(f\"{{total}}\")\n"
            ));
        }
        format!(
            "def at_or_zero(items: list[int], at: int) -> int:\n    if len(items) == 0: return 0\n    return items[at % len(items)]\n\ndef long() -> int:\n    mut total = 0\n{body}    return total\n\ndef main(stdout: Stdout):\n    stdout.print(long())\n"
        )
    }

    /// A compiled object is named by its text's hash, the LLVM that compiled it and how: each of
    /// them changes the name, and nothing else does.
    #[test]
    fn an_objects_key_changes_with_its_text_its_llvm_and_its_pipeline() {
        let text = fennec_blake3::hash(b"define i64 @f() {}");
        let key = object_key("19.1", "default<O2>", &text);
        assert_eq!(key, object_key("19.1", "default<O2>", &fennec_blake3::hash(b"define i64 @f() {}")));
        assert_ne!(key, object_key("19.1", "default<O2>", &fennec_blake3::hash(b"define i64 @g() {}")));
        assert_ne!(key, object_key("19.2", "default<O2>", &text));
        assert_ne!(key, object_key("19.1", "default<O2>,asan", &text));
    }

    fn units(source: &str) -> Result<Vec<Unit>, String> {
        let mut module = fennec_syntax::parse(source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        Ok(crate::emit(&fennec_ir::lower(&module, &checked))?.0)
    }

    /// A function, written as IR, that holds an optional of an optional of `inner`, `inner??`:
    /// `None`, or what `value` is when `empty` isn't set, which is `Some(None)` when `value` is
    /// `None`. It's copied, asked whether it's `None`, and what's inside is read out as the
    /// `inner?` it gives back, or `None`. Both `inner??` locals are let go of as it ends.
    fn nested_optional(name: &str, inner: &fennec_check::Type) -> fennec_ir::Function {
        use fennec_check::Type;
        use fennec_ir::{Block, BlockId, Inst, InstKind, Local, LocalId, Operand, Terminator};
        let optional = |ty: &Type| Type::Optional(Box::new(ty.clone()));
        let local = |name: &str, ty: Type| Local { name: Some(name.to_owned()), ty, copied: false };
        let (value, empty, nested, copied, none, peeled) =
            (LocalId(0), LocalId(1), LocalId(2), LocalId(3), LocalId(4), LocalId(5));
        let inst = |kind: InstKind| Inst { kind, span: fennec_syntax::Span::default() };
        let block = |insts: Vec<InstKind>, end: Terminator| Block {
            insts: insts.into_iter().map(inst).collect(),
            end,
        };
        fennec_ir::Function {
            name: name.to_owned(),
            params: 2,
            mut_params: Vec::new(),
            returns: optional(inner),
            locals: vec![
                local("value", optional(inner)),
                local("empty", Type::Bool),
                local("nested", optional(&optional(inner))),
                local("copied", optional(&optional(inner))),
                local("none", Type::Bool),
                local("peeled", optional(inner)),
            ],
            blocks: vec![
                block(
                    vec![InstKind::Copy { dest: nested, value: Operand::None }],
                    Terminator::Branch {
                        cond: Operand::Local(empty),
                        then: BlockId(2),
                        otherwise: BlockId(1),
                    },
                ),
                block(
                    vec![InstKind::MakeSome { dest: nested, value: Operand::Local(value) }],
                    Terminator::Jump(BlockId(2)),
                ),
                block(
                    vec![
                        InstKind::Copy { dest: copied, value: Operand::Local(nested) },
                        InstKind::IsNone { dest: none, value: Operand::Local(copied) },
                    ],
                    Terminator::Branch {
                        cond: Operand::Local(none),
                        then: BlockId(4),
                        otherwise: BlockId(3),
                    },
                ),
                block(
                    vec![InstKind::GetSome { dest: peeled, value: Operand::Local(copied) }],
                    Terminator::Jump(BlockId(5)),
                ),
                block(
                    vec![InstKind::Copy { dest: peeled, value: Operand::None }],
                    Terminator::Jump(BlockId(5)),
                ),
                block(Vec::new(), Terminator::Return(Some(Operand::Local(peeled)))),
            ],
            counts: Vec::new(),
            places: Vec::new(),
        }
    }

    /// An `f32`'s `math.sqrt`, `math.sin`, `math.cos` and `math.acos`, its `abs`, and `f32(text)`,
    /// written as IR, as the checker gives them once it types them: each an `f32`, by LLVM's `f32`
    /// square root and `fabs`, `libm`'s `f32` functions through the runtime, and the runtime's
    /// reading of text straight to an `f32`. Nothing else makes these yet, so the IR is written
    /// here, and LLVM checks what's made of it.
    #[test]
    fn an_f32s_math_and_reading_one_from_text_are_its_own() -> Result<(), String> {
        use fennec_check::{Num, Type};
        use fennec_ir::{Block, Inst, InstKind, Local, LocalId, Operand, Terminator};
        let Ok(tools) = Tools::find() else {
            return Ok(());
        };
        let source = "def main(stdout: Stdout):\n    got = float(\"1.5\") except why:\n        stdout.print(why)\n        return\n    stdout.print(got)\n";
        let mut module = fennec_syntax::parse(source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let mut program = fennec_ir::lower(&module, &checked);
        let f32_ty = Type::Sized(Num::F32);
        let read = Type::Result(Box::new(f32_ty.clone()), Box::new(Type::Named("InvalidValue".to_owned())));
        let local =
            |name: &str, ty: &Type| Local { name: Some(name.to_owned()), ty: ty.clone(), copied: false };
        let builtin = |dest: usize, name: &str, from: usize| Inst {
            kind: InstKind::Builtin {
                dest: Some(LocalId(dest)),
                name: name.to_owned(),
                args: vec![Operand::Local(LocalId(from))],
                changes: false,
            },
            span: fennec_syntax::Span::default(),
        };
        program.functions.push(fennec_ir::Function {
            name: "f32_math".to_owned(),
            params: 2,
            mut_params: Vec::new(),
            returns: f32_ty.clone(),
            locals: vec![
                local("x", &f32_ty),
                local("text", &Type::Str),
                local("root", &f32_ty),
                local("sine", &f32_ty),
                local("cosine", &f32_ty),
                local("angle", &f32_ty),
                local("size", &f32_ty),
                local("read", &read),
            ],
            blocks: vec![Block {
                insts: vec![
                    builtin(2, "math.sqrt", 0),
                    builtin(3, "math.sin", 2),
                    builtin(4, "math.cos", 3),
                    builtin(5, "math.acos", 4),
                    builtin(6, "f32.abs", 5),
                    builtin(7, "f32", 1),
                ],
                end: Terminator::Return(Some(Operand::Local(LocalId(6)))),
            }],
            counts: Vec::new(),
            places: Vec::new(),
        });
        let (units, _, _) = crate::emit(&program)?;
        let unit = units.iter().find(|unit| unit.function == "f32_math").ok_or("no unit for `f32_math`")?;
        for wanted in [
            "call float @llvm.sqrt.f32(",
            "call double @fennec_sinf(",
            "call double @fennec_cosf(",
            "call double @fennec_acosf(",
            "@llvm.fabs",
            "@fennec_parse_f32",
        ] {
            assert!(unit.text.contains(wanted), "no {wanted} in:\n{}", unit.text);
        }
        let work = std::env::temp_dir().join(format!("fennec-f32-math-{}", std::process::id()));
        std::fs::create_dir_all(&work).map_err(|err| err.to_string())?;
        let input = work.join("f32_math.ll");
        let verified = std::fs::write(&input, marked(&unit.text).as_bytes())
            .map_err(|err| err.to_string())
            .and_then(|()| {
                run(
                    Command::new(&tools.opt).args(["-passes=verify", "-disable-output"]).arg(&input),
                    "opt on `f32_math`",
                )
            });
        let _ = std::fs::remove_dir_all(&work);
        verified.map_err(|why| format!("{why}\n{}", unit.text))
    }

    /// An optional of an optional, `T??`, is held as an `Option[T?]` enum for every way a `T?` is
    /// held: an enum of its own for an int, a bool or a struct kept in place, a block's address or
    /// null for a boxed struct or a list, and a string's header. Making one, copying it, asking
    /// whether it's `None` and reading what's inside make sound LLVM for each. The checker folds
    /// `T??` into `T?` until #2413, so only IR written here holds one. Reading what's inside was
    /// taken as making one, since both sides are optional: `int?` from an `int??` stored the
    /// inner optional as an `int`, which LLVM refused.
    #[test]
    fn an_optional_of_an_optional_is_made_copied_and_read_out_for_every_inner_kind() -> Result<(), String> {
        use fennec_check::Type;
        let Ok(tools) = Tools::find() else {
            return Ok(());
        };
        let source = "struct Point:\n    x: int\n    y: int\n\nstruct Node:\n    x: int\n    next: Node?\n\ndef main(stdout: Stdout):\n    point = Point(x=1, y=2)\n    node = Node(x=3, next=None)\n    stdout.print(point.x, node.x)\n";
        let mut module = fennec_syntax::parse(source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let mut program = fennec_ir::lower(&module, &checked);
        let inners = [
            ("nested_int", Type::Int),
            ("nested_bool", Type::Bool),
            ("nested_point", Type::Named("Point".to_owned())),
            ("nested_node", Type::Named("Node".to_owned())),
            ("nested_text", Type::Str),
            ("nested_list", Type::List(Box::new(Type::Int))),
        ];
        for (name, inner) in &inners {
            program.functions.push(nested_optional(name, inner));
        }
        fennec_ir::validate(&program).map_err(|problems| problems.join("; "))?;
        let (units, _, layouts) = crate::emit(&program)?;
        // Each is an enum whose `Some` holds the inner optional, whichever way that's held.
        for (_, inner) in &inners {
            let named = format!("Option[{}]", Type::Optional(Box::new(inner.clone())));
            assert!(layouts.id(&named).is_some(), "no layout for `{named}`");
        }
        let work = std::env::temp_dir().join(format!("fennec-nested-optionals-{}", std::process::id()));
        std::fs::create_dir_all(&work).map_err(|err| err.to_string())?;
        let checked_all = (|| -> Result<(), String> {
            for (name, _) in &inners {
                let unit = units
                    .iter()
                    .find(|unit| unit.function == *name)
                    .ok_or_else(|| format!("no unit for `{name}`"))?;
                assert!(unit.text.contains(&format!("@\"f_{name}\"")), "{}", unit.text);
                // What an `int??` holds is read out whole, as the `int?` a word past its case.
                if *name == "nested_int" {
                    let lines: Vec<&str> = unit.text.lines().map(str::trim).collect();
                    let read_out = lines.windows(2).any(|pair| {
                        pair[0].ends_with("= getelementptr inbounds i8, ptr %l3, i64 8")
                            && pair[1].contains("= load [2 x i64], ptr %t")
                    });
                    assert!(read_out, "{}", unit.text);
                }
                let input = work.join(format!("{name}.ll"));
                std::fs::write(&input, marked(&unit.text).as_bytes()).map_err(|err| err.to_string())?;
                run(
                    Command::new(&tools.opt).args(["-passes=verify", "-disable-output"]).arg(&input),
                    &format!("opt on `{name}`"),
                )
                .map_err(|why| format!("{why}\n{}", unit.text))?;
            }
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&work);
        checked_all
    }

    /// A function past [`HUGE`] is huge; a shorter one, and the copies of others a unit has
    /// for inlining, aren't.
    #[test]
    fn a_long_function_is_huge_and_a_short_one_isnt() -> Result<(), String> {
        let huge_ones = |lines: usize| -> Result<Vec<String>, String> {
            Ok(units(&long_program(lines))?
                .into_iter()
                .filter(|unit| huge(&unit.text))
                .map(|unit| unit.function)
                .collect())
        };
        assert_eq!(huge_ones(400)?, ["long"]);
        assert_eq!(huge_ones(100)?, Vec::<String>::new());
        assert_eq!(huge_functions(&units(&long_program(400))?), ["long"]);
        assert_eq!(huge_functions(&units(&long_program(100))?), Vec::<String>::new());
        Ok(())
    }

    /// A name is read from a line plain or quoted, with a quoted one's escapes undone.
    #[test]
    fn names_are_read_plain_and_quoted() {
        assert_eq!(symbol("declare ptr @fennec_alloc(i64)").as_deref(), Some("fennec_alloc"));
        assert_eq!(
            symbol("define available_externally swiftcc i64 @\"f_at_or_zero\"(ptr %p0) #2 {").as_deref(),
            Some("f_at_or_zero")
        );
        assert_eq!(
            symbol("@\"h_Result[int, Refused]\" = external global i64").as_deref(),
            Some("h_Result[int, Refused]")
        );
        assert_eq!(symbol("declare void @\"a\\22b\"()").as_deref(), Some("a\"b"));
    }

    /// A kept name is written as a pattern that matches only itself.
    #[test]
    fn kept_names_match_only_themselves() {
        assert_eq!(as_pattern("f_twice[int]"), "f_twice\\[int\\]");
        assert_eq!(as_pattern("h_Result[int, Refused]"), "h_Result\\[int, Refused\\]");
        assert_eq!(as_pattern("a*b?{c}\\d"), "a\\*b\\?\\{c\\}\\\\d");
        assert_eq!(as_pattern("main"), "main");
    }

    /// What a huge function reaches is kept as it calls it: what it declares is declared again
    /// and marked used, and what it has a copy of is only named.
    #[test]
    fn what_a_huge_function_reaches_is_kept() -> Result<(), String> {
        let units = units(&long_program(400))?;
        let huge_ones: Vec<&Unit> = units.iter().filter(|unit| huge(&unit.text)).collect();
        let (module, names) = kept_for(&huge_ones)?;
        assert!(names.iter().any(|name| name == "f_at_or_zero"), "{names:?}");
        assert!(names.iter().any(|name| name == "fennec_list_new"), "{names:?}");
        assert!(!names.iter().any(|name| name == "f_long" || name.starts_with("llvm.")), "{names:?}");
        assert!(module.contains("declare ptr @fennec_list_new("), "{module}");
        assert!(module.contains("@llvm.used = appending global ["), "{module}");
        assert!(!module.contains("f_at_or_zero"), "a copy is only named: {module}");
        assert!(!module.lines().any(|line| line.starts_with("declare") && line.contains(" #")), "{module}");
        Ok(())
    }

    /// Linked in groups, the last one shorter, then the groups together, a program's modules make
    /// the same module as linked all at once: the same functions, globals and types, in the same
    /// order.
    #[test]
    fn modules_linked_in_groups_make_the_module_linked_at_once() -> Result<(), String> {
        let Ok(tools) = Tools::find() else {
            return Ok(());
        };
        let source = "struct Spot:\n    across: int\n    down: int\n\ndef moved(spot: Spot, by: int) -> Spot:\n    return Spot(across=spot.across + by, down=spot.down)\n\ndef named(spot: Spot) -> str:\n    return f\"{spot.across},{spot.down}\"\n\ndef walked(steps: list[int]) -> list[Spot]:\n    mut spots: list[Spot] = []\n    mut at = Spot(across=0, down=0)\n    for step in steps:\n        at = moved(at, by=step)\n        spots.append(at)\n    return spots\n\ndef main(stdout: Stdout):\n    for spot in walked([1, 2, 3]):\n        stdout.print(named(spot))\n";
        let units = units(source)?;
        let work = std::env::temp_dir().join(format!("fennec-link-groups-{}", std::process::id()));
        let linked_both = || -> Result<(String, String, usize), String> {
            std::fs::create_dir_all(&work).map_err(|err| err.to_string())?;
            let mut inputs = Vec::new();
            for (at, unit) in units.iter().enumerate() {
                let input = work.join(format!("f{at}.ll"));
                std::fs::write(&input, marked(&unit.text).as_bytes()).map_err(|err| err.to_string())?;
                inputs.push(input);
            }
            let text_of = |name: &str, group: usize| -> Result<String, String> {
                let (whole, shown) = (work.join(format!("{name}.bc")), work.join(format!("{name}.ll")));
                linked(&tools, (&inputs, group), &work, &whole)?;
                run(
                    Command::new(&tools.opt).args(["-S", "-passes=verify"]).arg(&whole).arg("-o").arg(&shown),
                    "opt",
                )?;
                let text = std::fs::read_to_string(&shown).map_err(|err| err.to_string())?;
                // Each is named by the file it was read from.
                Ok(text.lines().filter(|line| !line.starts_with("; ModuleID")).collect::<Vec<_>>().join("\n"))
            };
            // Groups of three, the last of them shorter.
            Ok((text_of("at-once", usize::MAX)?, text_of("in-groups", 3)?, inputs.len()))
        };
        let made = linked_both();
        let _ = std::fs::remove_dir_all(&work);
        let (at_once, in_groups, count) = made?;
        assert!(count >= 4 && count % 3 != 0, "{count} modules, too few for groups of 3 and one shorter");
        assert!(at_once.contains("define"), "{at_once}");
        assert_eq!(at_once, in_groups);
        Ok(())
    }

    /// A huge function compiled for x86-64 checks each call's error in `r12`, where the call left
    /// it. The fast instruction selection compared memory at the error pointer the function was
    /// given instead, which crashed. This builds the assembly, so it runs on any machine.
    #[test]
    fn a_huge_function_checks_errors_where_calls_leave_them_on_x86_64() -> Result<(), String> {
        let Ok(tools) = Tools::find() else {
            return Ok(());
        };
        let units = units(&long_program(400))?;
        let unit = units.iter().find(|unit| huge(&unit.text)).ok_or("no huge function")?;
        let header = "target datalayout = \"e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128\"\ntarget triple = \"x86_64-unknown-linux-gnu\"\n";
        let body: String = unit.text.lines().filter(|line| !line.starts_with("target ")).fold(
            String::new(),
            |mut text, line| {
                text.push_str(line);
                text.push('\n');
                text
            },
        );
        let work = std::env::temp_dir().join(format!("fennec-x86-errors-{}", std::process::id()));
        std::fs::create_dir_all(&work).map_err(|err| err.to_string())?;
        let (input, output) = (work.join("long.ll"), work.join("long.s"));
        std::fs::write(&input, format!("{header}{body}")).map_err(|err| err.to_string())?;
        let done = Command::new(&tools.llc)
            .args(llc_levels(Way::Quickly))
            .args(["-filetype=asm", "-relocation-model=pic"])
            .arg(&input)
            .arg("-o")
            .arg(&output)
            .output()
            .map_err(|err| err.to_string());
        let assembly = std::fs::read_to_string(&output).map_err(|err| err.to_string());
        let _ = std::fs::remove_dir_all(&work);
        let done = done?;
        assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
        let assembly = assembly?;
        let calls =
            assembly.lines().filter(|line| line.contains("call") && line.contains("f_at_or_zero")).count();
        let checks = assembly.lines().filter(|line| line.trim() == "testq\t%r12, %r12").count();
        assert_eq!(calls, 400, "a call a line");
        assert!(checks >= calls, "{checks} checks of r12 for {calls} calls");
        Ok(())
    }

    /// A function is the same object byte for byte whatever its unit's number and folder, so one
    /// taken from the cache is the one a fresh build makes. Its module is named `fennec`: unnamed,
    /// LLVM names it by the file it's read from, `f{n}.ll`, and an ELF object keeps that name.
    /// This builds ELF objects, so it runs on any machine.
    #[test]
    fn a_function_is_the_same_object_whatever_its_number_and_folder() -> Result<(), String> {
        let Ok(tools) = Tools::find() else {
            return Ok(());
        };
        let units = units(&long_program(3))?;
        let unit = units.iter().find(|unit| unit.text.contains("define")).ok_or("no function")?;
        assert!(unit.text.starts_with("source_filename = \"fennec\"\n"), "{}", unit.text);
        let header = "target datalayout = \"e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128\"\ntarget triple = \"x86_64-unknown-linux-gnu\"\n";
        let named = unit.text.lines().filter(|line| !line.starts_with("target ")).fold(
            header.to_owned(),
            |mut text, line| {
                text.push_str(line);
                text.push('\n');
                text
            },
        );
        let unnamed: String = named.lines().filter(|line| !line.starts_with("source_filename")).fold(
            String::new(),
            |mut text, line| {
                text.push_str(line);
                text.push('\n');
                text
            },
        );
        let builds = [("one", "f0"), ("and-another", "f17")];
        let mut objects = Vec::new();
        let mut failed = None;
        for (folder, number) in builds {
            let work =
                std::env::temp_dir().join(format!("fennec-same-object-{folder}-{}", std::process::id()));
            let made = std::fs::create_dir_all(&work).map_err(|err| err.to_string()).and_then(|()| {
                Ok((
                    one(&tools, &named, &work, number, Way::Optimized)?,
                    one(&tools, &unnamed, &work, number, Way::Optimized)?,
                ))
            });
            let _ = std::fs::remove_dir_all(&work);
            match made {
                Ok(pair) => objects.push(pair),
                Err(err) => failed = Some(err),
            }
        }
        if let Some(err) = failed {
            return Err(err);
        }
        let [(named_here, unnamed_here), (named_there, unnamed_there)] = &objects[..] else {
            return Err("two builds give two objects each".to_owned());
        };
        assert!(named_here == named_there, "the same module gave two different objects");
        assert!(unnamed_here != unnamed_there, "an unnamed module's object should hold its file's name");
        Ok(())
    }
}

fennec_syntax::codec_fields!(Objects { removals, version, pipeline });

/// The objects this process's last build found or made, with what they were known under, as bytes,
/// for a daemon whose builds are each made in a process of their own: it holds them between
/// builds and hands them to the next (see [`has_object`]). Empty when nothing is known.
#[must_use]
pub fn known_objects_to_bytes() -> Vec<u8> {
    use fennec_syntax::codec::Encode as _;
    let known = KNOWN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(known) = known.as_ref() else { return Vec::new() };
    let mut to = fennec_syntax::codec::Writer::default();
    known.encode(&mut to);
    to.into_bytes()
}

/// Knows what [`known_objects_to_bytes`] wrote, for this process's next build. Bytes that don't
/// read know nothing, so that build looks for every object, and give `false`.
pub fn know_objects_from_bytes(bytes: &[u8]) -> bool {
    use fennec_syntax::codec::Decode as _;
    if bytes.is_empty() {
        return true;
    }
    let mut from = fennec_syntax::codec::Reader::new(bytes);
    let read = <(Objects, HashSet<[u8; 32]>)>::decode(&mut from).filter(|_| from.is_done());
    let read_it = read.is_some();
    *KNOWN.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = read;
    read_it
}
