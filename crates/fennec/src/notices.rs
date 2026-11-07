//! Third-party notices built into a program. A program `fennec build --release` links on its own
//! on Linux has musl's C library in it, whose license asks for its notices to go with every copy,
//! so they go in the program itself as text, between two markers, and `fennec notices` prints
//! them.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// musl's copyright and license notices.
const MUSL: &str = include_str!("../../../licenses/musl.txt");

/// The marker before the notices in a program, or the one after them. Each is put together as
/// it's used, so `fennec` itself, which holds the notices' text, never holds a marker.
fn marker(end: bool) -> Vec<u8> {
    let pieces = std::hint::black_box(["FENNEC-THIRD-PARTY-", if end { "END" } else { "BEGIN" }]);
    pieces.concat().into_bytes()
}

/// The text a program holds: the notices between their markers.
fn held() -> Vec<u8> {
    [marker(false), b"\n".to_vec(), MUSL.as_bytes().to_vec(), marker(true), b"\n".to_vec()].concat()
}

/// The global holding the notices, which the linker is told to keep.
pub(crate) const SYMBOL: &str = "fennec_third_party_notices";

/// An object file in `work` that holds the notices as the global `SYMBOL`, compiled by LLVM's
/// `llc` from a module of its own.
pub(crate) fn object(work: &Path) -> Result<PathBuf, String> {
    let text = held();
    let mut written = String::with_capacity(text.len() * 2);
    for byte in &text {
        match byte {
            b' '..=b'~' if *byte != b'"' && *byte != b'\\' => written.push(char::from(*byte)),
            _ => {
                let _ = write!(written, "\\{byte:02X}");
            }
        }
    }
    let module = format!("@{SYMBOL} = constant [{} x i8] c\"{written}\", align 1\n", text.len());
    let source = work.join("notices.ll");
    let output = work.join("notices.o");
    std::fs::write(&source, module).map_err(|err| format!("couldn't write {}: {err}", source.display()))?;
    let tools = fennec_llvm::Tools::find()?;
    let compiled = Command::new(tools.folder().join("llc"))
        .args(["-filetype=obj", "-o"])
        .arg(&output)
        .arg(&source)
        .output()
        .map_err(|err| format!("couldn't run `llc`: {err}"))?;
    if compiled.status.success() {
        Ok(output)
    } else {
        Err(format!("`llc` failed on the notices:\n{}", String::from_utf8_lossy(&compiled.stderr)))
    }
}

/// The notices `bytes` hold between their markers, if they hold any.
fn found(bytes: &[u8]) -> Option<&[u8]> {
    let (begin, end) = (marker(false), marker(true));
    let start = bytes.windows(begin.len()).position(|window| window == begin)? + begin.len() + 1;
    let length = bytes.get(start..)?.windows(end.len()).position(|window| window == end)?;
    bytes.get(start..start + length)
}

/// `fennec notices <program>`: prints the third-party notices a program holds.
pub(crate) fn command(program: &str) -> ExitCode {
    let bytes = match std::fs::read(program) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("error: couldn't read {program}: {err}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(notices) = found(&bytes) {
        print!("{}", String::from_utf8_lossy(notices));
    } else {
        println!("{program} holds no third-party notices: nothing in it asks for one");
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_notices_are_found_between_their_markers() {
        let bytes = [b"\x00\x01before".as_slice(), &super::held(), b"after\x00"].concat();
        assert_eq!(super::found(&bytes), Some(super::MUSL.as_bytes()));
        assert_eq!(super::found(super::MUSL.as_bytes()), None);
        assert!(super::MUSL.contains("Rich Felker"));
    }
}
