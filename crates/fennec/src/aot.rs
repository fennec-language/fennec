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
        eprintln!("error: `fennec build` can't link executables on Windows yet; use `fennec run --jit`");
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
