//! `fennec build`: compiles a program ahead of time to a standalone executable. The JIT's
//! compiler writes an object file, and the system's C compiler links it with Fennec's
//! runtime library, which sits next to the `fennec` command.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

/// The libraries Rust's standard library needs from the system, which the runtime
/// library uses, and the flag that drops the parts of the runtime a program never calls.
#[cfg(target_os = "macos")]
const SYSTEM_LIBRARIES: &[&str] = &["-lSystem", "-lc", "-lm", "-Wl,-dead_strip"];
#[cfg(not(target_os = "macos"))]
const SYSTEM_LIBRARIES: &[&str] =
    &["-lgcc_s", "-lutil", "-lrt", "-lpthread", "-lm", "-ldl", "-lc", "-Wl,--gc-sections"];

/// The runtime library's file name.
const RUNTIME: &str = "libfennec_runtime.a";

pub(crate) fn build(path: &str, output: Option<&str>) -> ExitCode {
    if cfg!(windows) {
        eprintln!("error: `fennec build` can't link executables on Windows yet; use `fennec run`");
        return ExitCode::FAILURE;
    }
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    if !crate::run::check(&mut program) {
        return ExitCode::FAILURE;
    }
    let started = Instant::now();
    let built = match fennec_jit::build(&program.module, &program.sources) {
        Ok(built) => built,
        Err(diagnostic) => {
            eprint!("{}", program.sources.render(&diagnostic));
            return ExitCode::FAILURE;
        }
    };
    let compiled = started.elapsed();
    let output = output.map_or_else(|| default_output(path, &program), PathBuf::from);
    match link(&built.object, &output) {
        Ok(linked) => {
            let size = std::fs::metadata(&output).map_or(0, |metadata| metadata.len());
            println!(
                "🦊 built {} ({} KB) in {} ms: compiled in {} ms, linked in {} ms",
                output.display(),
                size / 1024,
                (compiled + linked).as_millis(),
                compiled.as_millis(),
                linked.as_millis(),
            );
            if !built.interpreted.is_empty() {
                println!("   run by the interpreter: {}", built.interpreted.join(", "));
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// `fennec build --llvm`: the same through LLVM, which says what it can't take yet rather than
/// falling back to anything.
pub(crate) fn build_llvm(path: &str, output: Option<&str>, release: bool) -> ExitCode {
    if cfg!(windows) {
        eprintln!("error: `fennec build` can't link executables on Windows yet; use `fennec run`");
        return ExitCode::FAILURE;
    }
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Main) else { return ExitCode::FAILURE };
    if !crate::run::check(&mut program) {
        return ExitCode::FAILURE;
    }
    let output = output.map_or_else(|| default_output(path, &program), PathBuf::from);
    let started = Instant::now();
    match llvm_objects(&program, release) {
        Ok((objects, total, fresh)) => {
            let compiled = started.elapsed();
            match link_objects(&objects, &output) {
                Ok(linked) => {
                    println!(
                        "🦊 built {} with LLVM: compiled {fresh} of {total} functions in {} ms, linked in {} ms",
                        output.display(),
                        compiled.as_millis(),
                        linked.as_millis(),
                    );
                    ExitCode::SUCCESS
                }
                Err(message) => {
                    eprintln!("error: {message}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Lowers what the program reaches and compiles it: the objects, how many functions, and how
/// many weren't already in the cache.
fn llvm_objects(
    program: &fennec_kit::Program,
    release: bool,
) -> Result<(Vec<Vec<u8>>, usize, usize), String> {
    let mut reached = program.module.clone();
    fennec_build::keep_what_main_reaches(&mut reached);
    let checked =
        fennec_check::check(&reached).map_err(|_| "the program didn't check once trimmed".to_owned())?;
    let lowered = fennec_ir::lower(&reached, &checked);
    if let Some(skipped) = lowered.not_lowered.first() {
        return Err(format!("`{}` isn't lowered yet: {}", skipped.function, skipped.why));
    }
    let (units, shared) = fennec_llvm::emit(&lowered)?;
    let tools = fennec_llvm::Tools::find()?;
    let layouts = fennec_llvm::layouts_of(&lowered);
    let mut image = fennec_llvm::Image::of(&layouts, &shared)?;
    let bytes = fennec_interp::embed::codec::Image {
        module: program.module.clone(),
        sources: program.sources.clone(),
        tables: std::mem::take(&mut image.tables),
        literals: image.literals.clone(),
    }
    .to_bytes();
    let entry = fennec_llvm::entry(&lowered, &shared, &image, &bytes)?;
    let home = fennec_build::fennec_home();
    let cache = home
        .as_deref()
        .filter(|_| std::env::var_os("FENNEC_NO_CACHE").is_none())
        .map(fennec_build::Cache::new);
    let work = std::env::temp_dir().join(format!("fennec-llvm-{}", std::process::id()));
    let built = if release {
        fennec_llvm::compile_whole(&tools, &units, &entry, &work)
    } else {
        fennec_llvm::compile(&tools, &units, &entry, cache.as_ref(), &work)
    };
    // `FENNEC_LLVM_KEEP` leaves the modules' text to look at.
    if std::env::var_os("FENNEC_LLVM_KEEP").is_none() {
        let _ = std::fs::remove_dir_all(&work);
    }
    let built = built?;
    Ok((built.objects, units.len(), built.compiled))
}

/// Links object files with the runtime library, giving back how long linking took.
fn link_objects(objects: &[Vec<u8>], output: &Path) -> Result<std::time::Duration, String> {
    let runtime = runtime_library()?;
    let work = std::env::temp_dir().join(format!("fennec-link-{}", std::process::id()));
    std::fs::create_dir_all(&work).map_err(|err| format!("couldn't make {}: {err}", work.display()))?;
    let mut paths = Vec::with_capacity(objects.len());
    for (at, object) in objects.iter().enumerate() {
        let path = work.join(format!("{at}.o"));
        std::fs::write(&path, object).map_err(|err| format!("couldn't write an object file: {err}"))?;
        paths.push(path);
    }
    let started = Instant::now();
    let linked = Command::new("cc")
        .args(&paths)
        .arg(&runtime)
        .args(SYSTEM_LIBRARIES)
        // `FENNEC_SANITIZE=address` links in AddressSanitizer's runtime.
        .args(fennec_llvm::sanitizing().then_some("-fsanitize=address"))
        .arg("-o")
        .arg(output)
        .output()
        .map_err(|err| format!("couldn't run the system's C compiler, `cc`, to link: {err}"))?;
    let took = started.elapsed();
    let _ = std::fs::remove_dir_all(&work);
    if linked.status.success() {
        Ok(took)
    } else {
        Err(format!("linking failed:\n{}", String::from_utf8_lossy(&linked.stderr)))
    }
}

/// Where an executable goes when no `-o` says: named after the kit, or the file, in the
/// current folder.
fn default_output(path: &str, program: &fennec_kit::Program) -> PathBuf {
    let name = program
        .manifest
        .as_ref()
        .map(|manifest| manifest.name.clone())
        .or_else(|| Path::new(path).file_stem().map(|stem| stem.to_string_lossy().into_owned()));
    PathBuf::from(name.filter(|name| !name.is_empty() && name != ".").unwrap_or_else(|| "main".to_owned()))
}

/// Links an object file with the runtime library into an executable at `output`, giving
/// back how long linking took.
fn link(object: &[u8], output: &Path) -> Result<std::time::Duration, String> {
    let runtime = runtime_library()?;
    let work = std::env::temp_dir().join(format!("fennec-build-{}", std::process::id()));
    std::fs::create_dir_all(&work).map_err(|err| format!("couldn't make {}: {err}", work.display()))?;
    let object_path = work.join("program.o");
    std::fs::write(&object_path, object).map_err(|err| format!("couldn't write the object file: {err}"))?;
    let started = Instant::now();
    let linked = Command::new("cc")
        .arg(&object_path)
        .arg(&runtime)
        .args(SYSTEM_LIBRARIES)
        .arg("-o")
        .arg(output)
        .output()
        .map_err(|err| format!("couldn't run the system's C compiler, `cc`, to link: {err}"))?;
    let took = started.elapsed();
    let _ = std::fs::remove_dir_all(&work);
    if linked.status.success() {
        Ok(took)
    } else {
        Err(format!("linking failed:\n{}", String::from_utf8_lossy(&linked.stderr)))
    }
}

/// The runtime library, next to the `fennec` command.
fn runtime_library() -> Result<PathBuf, String> {
    let fennec =
        std::env::current_exe().map_err(|err| format!("couldn't find the `fennec` command: {err}"))?;
    let runtime = fennec.with_file_name(RUNTIME);
    if runtime.is_file() {
        Ok(runtime)
    } else {
        Err(format!(
            "the runtime library `{RUNTIME}` should be next to `fennec`, at {}; build it with `cargo build -p fennec-runtime`",
            runtime.display()
        ))
    }
}
