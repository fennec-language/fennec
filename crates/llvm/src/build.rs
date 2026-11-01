//! Compiling modules with `opt` and `llc`, a function at a time, through the cache.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use fennec_build::Cache;

use crate::emit::Unit;

const KIND: &str = "llvm";
const MOST_THREADS: usize = 4;
const PIPELINE: &str = "default<O3>";

/// Whether `FENNEC_SANITIZE=address` asks for `AddressSanitizer`, to test that built programs
/// touch only memory they own. Every function with attributes is instrumented, after the rest
/// of the pipeline, as Clang does it, and the link takes `-fsanitize=address`.
#[must_use]
pub fn sanitizing() -> bool {
    std::env::var_os("FENNEC_SANITIZE").is_some_and(|asked| asked == "address")
}

/// The passes `opt` runs.
fn pipeline() -> String {
    if sanitizing() { format!("{PIPELINE},asan") } else { PIPELINE.to_owned() }
}

/// A module's text, marked for `AddressSanitizer` to instrument when it's asked for: it only
/// instruments functions that say `sanitize_address`, and they all take one of its attribute sets.
fn marked(text: &str) -> std::borrow::Cow<'_, str> {
    if !sanitizing() {
        return text.into();
    }
    let lines = text.lines().map(|line| {
        if line.starts_with("attributes #") {
            line.replacen(" = { ", " = { sanitize_address ", 1)
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
    /// From `FENNEC_LLVM_BIN`, or the Rust toolchain's `llvm-tools` component. Nothing's
    /// downloaded; what's missing is reported. Where the toolchain keeps them is remembered in
    /// the Fennec home, as asking `rustc` each build costs more than the rest of an unchanged one.
    pub fn find() -> Result<Self, String> {
        let remembered = fennec_build::fennec_home().map(|home| home.join("llvm-tools"));
        let folder = if let Some(folder) = std::env::var_os("FENNEC_LLVM_BIN") {
            PathBuf::from(folder)
        } else if let Some(folder) = remembered.as_deref().and_then(remembered_folder) {
            folder
        } else {
            let folder = toolchain_tools()?;
            if let Some(at) = &remembered {
                let _ = std::fs::write(at, folder.to_string_lossy().as_bytes());
            }
            folder
        };
        let tools =
            Tools { opt: folder.join("opt"), llc: folder.join("llc"), link: folder.join("llvm-link") };
        if tools.opt.is_file() && tools.llc.is_file() {
            Ok(tools)
        } else {
            if let Some(at) = &remembered {
                let _ = std::fs::remove_file(at);
            }
            Err(format!(
                "LLVM's `opt` and `llc` aren't in {}; run `rustup component add llvm-tools` or set FENNEC_LLVM_BIN",
                folder.display()
            ))
        }
    }

    /// What makes one version of the tools differ from another, for cache keys: remembered
    /// beside `llc`'s size and time, so it's only asked again when `llc` changes.
    fn version(&self) -> Result<String, String> {
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
            let _ = std::fs::write(at, format!("{stamp}\n{version}"));
        }
        Ok(version)
    }
}

/// The folder remembered for the toolchain's tools, if they're still there.
fn remembered_folder(at: &Path) -> Option<PathBuf> {
    let folder = PathBuf::from(std::fs::read_to_string(at).ok()?.trim());
    (folder.join("opt").is_file() && folder.join("llc").is_file()).then_some(folder)
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

/// An object's bytes and whether it's fresh, or why it couldn't be made.
type Compiled = Result<(Vec<u8>, bool), String>;

/// What compiling gave: every object's bytes, the fresh ones counted.
#[derive(Debug, Default)]
pub struct Built {
    pub objects: Vec<Vec<u8>>,
    pub compiled: usize,
}

fn one(tools: &Tools, text: &str, work: &Path, name: &str, optimize: bool) -> Result<Vec<u8>, String> {
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
    let compiled_from = if optimize {
        run(
            Command::new(&tools.opt)
                .arg(format!("-passes={}", pipeline()))
                .arg(&input)
                .arg("-o")
                .arg(&middle),
            "opt",
        )?;
        &middle
    } else {
        &input
    };
    run(
        Command::new(&tools.llc)
            .args(["-O3", "-filetype=obj", "-relocation-model=pic"])
            .arg(compiled_from)
            .arg("-o")
            .arg(&output),
        "llc",
    )?;
    std::fs::read(&output).map_err(|err| format!("couldn't read {}: {err}", output.display()))
}

/// For a release: every function's module and `entry` linked into one, optimized as a whole,
/// with nothing but `main` seen from outside, so LLVM inlines and specializes across the whole
/// program. Nothing's cached, as any change changes the whole.
pub fn compile_whole(tools: &Tools, units: &[Unit], entry: &str, work: &Path) -> Result<Built, String> {
    std::fs::create_dir_all(work).map_err(|err| format!("couldn't make {}: {err}", work.display()))?;
    let mut inputs = Vec::with_capacity(units.len() + 1);
    for (at, unit) in units.iter().enumerate() {
        let input = work.join(format!("f{at}.ll"));
        std::fs::write(&input, marked(&unit.text).as_bytes())
            .map_err(|err| format!("couldn't write {}: {err}", input.display()))?;
        inputs.push(input);
    }
    let entry_at = work.join("entry.ll");
    std::fs::write(&entry_at, entry)
        .map_err(|err| format!("couldn't write {}: {err}", entry_at.display()))?;
    inputs.push(entry_at);
    let (whole, optimized, output) = (work.join("whole.bc"), work.join("optimized.bc"), work.join("whole.o"));
    let run = |command: &mut Command, what: &str| {
        let done = command.output().map_err(|err| format!("couldn't run {what}: {err}"))?;
        if done.status.success() {
            Ok(())
        } else {
            Err(format!("{what} failed on the whole program:\n{}", String::from_utf8_lossy(&done.stderr)))
        }
    };
    if !tools.link.is_file() {
        return Err(format!("LLVM's `llvm-link` isn't beside `opt` in {}", tools.link.display()));
    }
    run(Command::new(&tools.link).args(&inputs).arg("-o").arg(&whole), "llvm-link")?;
    run(
        Command::new(&tools.opt)
            .arg(format!("-passes=internalize,{}", pipeline()))
            .arg("-internalize-public-api-list=main")
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
    let object =
        std::fs::read(&output).map_err(|err| format!("couldn't read {}: {err}", output.display()))?;
    Ok(Built { objects: vec![object], compiled: units.len() })
}

/// Compiles every function's module, using the cache's copy of any it already has, and `entry`.
pub fn compile(
    tools: &Tools,
    units: &[Unit],
    entry: &str,
    cache: Option<&Cache>,
    work: &Path,
) -> Result<Built, String> {
    std::fs::create_dir_all(work).map_err(|err| format!("couldn't make {}: {err}", work.display()))?;
    let version = tools.version()?;
    let threads = std::thread::available_parallelism().map_or(1, usize::from).min(MOST_THREADS);
    let next = Mutex::new(0_usize);
    let results: Mutex<Vec<(usize, Compiled)>> = Mutex::new(Vec::with_capacity(units.len()));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let at = {
                        let Ok(mut next) = next.lock() else { return };
                        let at = *next;
                        *next += 1;
                        at
                    };
                    let Some(unit) = units.get(at) else { return };
                    let key =
                        fennec_blake3::hash(format!("{version}\n{}\n{}", pipeline(), unit.text).as_bytes())
                            .to_hex();
                    let cached = cache.and_then(|cache| cache.read(KIND, &key));
                    let result = match cached {
                        Some(bytes) => Ok((bytes, false)),
                        None => one(tools, &unit.text, work, &format!("f{at}"), true).map(|bytes| {
                            if let Some(cache) = cache {
                                // A failed write only means compiling it again next time.
                                let _ = cache.write(KIND, &key, &bytes);
                            }
                            (bytes, true)
                        }),
                    };
                    if let Ok(mut results) = results.lock() {
                        results.push((at, result));
                    }
                }
            });
        }
    });
    let mut results =
        results.into_inner().map_err(|_| "a compiling thread stopped unexpectedly".to_owned())?;
    results.sort_by_key(|(at, _)| *at);
    let mut built = Built::default();
    for (_, result) in results {
        let (bytes, fresh) = result?;
        built.objects.push(bytes);
        built.compiled += usize::from(fresh);
    }
    built.objects.push(one(tools, entry, work, "entry", false)?);
    Ok(built)
}
