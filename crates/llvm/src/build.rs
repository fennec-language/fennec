//! Compiling modules with `opt` and `llc`, a function at a time, through the cache.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use fennec_build::Cache;

use crate::emit::Unit;

const KIND: &str = "llvm";
const MOST_THREADS: usize = 4;
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
/// for a build to say so. Only a module of more than [`SMALLEST_HUGE`] bytes is looked through.
#[must_use]
pub fn huge_functions(units: &[Unit]) -> Vec<String> {
    units
        .iter()
        .filter(|unit| unit.text.len() > SMALLEST_HUGE && huge(&unit.text))
        .map(|unit| unit.function.clone())
        .collect()
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
fn sanitizer() -> Option<&'static str> {
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
                let _ = std::fs::write(at, folder.to_string_lossy().as_bytes());
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

/// An object's bytes and whether it's fresh, or why it couldn't be made.
type Compiled = Result<(Vec<u8>, bool), String>;

/// What compiling gave: every object's bytes, the fresh ones counted.
#[derive(Debug, Default)]
pub struct Built {
    pub objects: Vec<Vec<u8>>,
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
    std::fs::read(&output).map_err(|err| format!("couldn't read {}: {err}", output.display()))
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

/// For a release: every function's module and `entry` linked into one, optimized as a whole,
/// with nothing but `main` seen from outside, so LLVM inlines and specializes across the whole
/// program. Given the runtime's own bitcode, what the program uses of it joins the unit too, so
/// its calls inline as well. Nothing's cached, as any change changes the whole. A huge function
/// (see [`HUGE`]) is compiled apart, as written, and what it reaches stays as it calls it.
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
        units.iter().enumerate().partition(|(_, unit)| huge(&unit.text));
    let kept = work.join("kept.txt");
    let mut keep = vec!["main".to_owned()];
    if !apart.is_empty() {
        let (module, names) = kept_for(&apart.iter().map(|(_, unit)| *unit).collect::<Vec<_>>())?;
        let input = work.join("kept.ll");
        std::fs::write(&input, module).map_err(|err| format!("couldn't write {}: {err}", input.display()))?;
        inputs.push(input);
        keep.extend(names);
    }
    std::fs::write(&kept, keep.join("\n") + "\n")
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
        let mut objects = vec![whole_object(tools, &inputs, &kept, work, runtime)?];
        for compiling in quick {
            objects.push(
                compiling
                    .join()
                    .unwrap_or_else(|_| Err("compiling a huge function stopped unexpectedly".to_owned()))?,
            );
        }
        Ok(Built { objects, compiled: units.len() })
    })
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
    // The functions' files go to `llvm-link` in a file, one to a line: on the command line, a big
    // program's thousands go past what Windows lets a command be. Each is quoted, and on Windows
    // written with `/`, so no `\` in one is read as an escape.
    let listed = work.join("inputs.rsp");
    let line = |input: &PathBuf| {
        let path = input.display().to_string();
        format!("\"{}\"\n", if cfg!(windows) { path.replace('\\', "/") } else { path })
    };
    let lines: String = inputs.iter().map(line).collect();
    std::fs::write(&listed, lines).map_err(|err| format!("couldn't write {}: {err}", listed.display()))?;
    run(Command::new(&tools.link).arg(format!("@{}", listed.display())).arg("-o").arg(&whole), "llvm-link")?;
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
    // The entry is made fresh each build and needs nothing of the functions, so it's compiled
    // beside them rather than after.
    let entry = std::thread::scope(|scope| {
        let entry = scope.spawn(|| one(tools, entry, work, "entry", Way::AsWritten));
        for _ in 0..threads.saturating_sub(1).max(1) {
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
                        None => {
                            // Which way is the text's own doing, so the key needn't say.
                            let way = if huge(&unit.text) { Way::Quickly } else { Way::Optimized };
                            one(tools, &unit.text, work, &format!("f{at}"), way)
                        }
                        .map(|bytes| {
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
        entry.join().unwrap_or_else(|_| Err("compiling the entry stopped unexpectedly".to_owned()))
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
    built.objects.push(entry?);
    Ok(built)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A program whose `long` makes a list and a string on each of `lines` lines, which only it
    /// formats.
    pub(crate) fn long_program(lines: usize) -> String {
        let mut body = String::new();
        for line in 0..lines {
            body.push_str(&format!(
                "    total += at_or_zero([{line}, {line} + 1], {line} % 2) + f\"{{total}}\".len()\n"
            ));
        }
        format!(
            "def at_or_zero(items: list[int], at: int) -> int:\n    if items.len() == 0: return 0\n    items[at % items.len()]\n\ndef long() -> int:\n    mut total = 0\n{body}    total\n\ndef main(stdout: Stdout):\n    stdout.print(long())\n"
        )
    }

    fn units(source: &str) -> Result<Vec<Unit>, String> {
        let module = fennec_syntax::parse(source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check(&module).map_err(|err| format!("{err:?}"))?;
        Ok(crate::emit(&fennec_ir::lower(&module, &checked))?.0)
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
}
