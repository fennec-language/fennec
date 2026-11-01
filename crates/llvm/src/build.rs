//! Compiling modules with `opt` and `llc`, a function at a time, through the cache.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use fennec_build::Cache;

use crate::emit::Unit;

const KIND: &str = "llvm";
const MOST_THREADS: usize = 4;
const PIPELINE: &str = "default<O3>";

/// LLVM's programs.
#[derive(Clone, Debug)]
pub struct Tools {
    opt: PathBuf,
    llc: PathBuf,
}

impl Tools {
    /// From `FENNEC_LLVM_BIN`, or the Rust toolchain's `llvm-tools` component. Nothing's
    /// downloaded; what's missing is reported.
    pub fn find() -> Result<Self, String> {
        let folder = match std::env::var_os("FENNEC_LLVM_BIN") {
            Some(folder) => PathBuf::from(folder),
            None => toolchain_tools()?,
        };
        let tools = Tools { opt: folder.join("opt"), llc: folder.join("llc") };
        if tools.opt.is_file() && tools.llc.is_file() {
            Ok(tools)
        } else {
            Err(format!(
                "LLVM's `opt` and `llc` aren't in {}; run `rustup component add llvm-tools` or set FENNEC_LLVM_BIN",
                folder.display()
            ))
        }
    }

    /// What makes one version of the tools differ from another, for cache keys.
    fn version(&self) -> Result<String, String> {
        let out = Command::new(&self.llc).arg("--version").output().map_err(|err| err.to_string())?;
        Ok(String::from_utf8_lossy(&out.stdout).lines().take(2).collect::<Vec<_>>().join(" "))
    }
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
    std::fs::write(&input, text).map_err(|err| format!("couldn't write {}: {err}", input.display()))?;
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
            Command::new(&tools.opt).arg(format!("-passes={PIPELINE}")).arg(&input).arg("-o").arg(&middle),
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
                    let key = fennec_blake3::hash(format!("{version}\n{PIPELINE}\n{}", unit.text).as_bytes())
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
