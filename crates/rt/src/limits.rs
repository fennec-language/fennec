//! Capabilities narrowed with `only`: a `Process` that can run only the programs named, and an
//! `Fs` that reaches only what's inside the folders named. Narrowing again only takes more
//! away. Something outside what's allowed fails the way the capability fails, with a
//! `ProcessError` or an `IoError` value that says what was allowed, rather than stopping the
//! program. Folders are compared by their paths with `.` and `..` worked out, so a path can't
//! climb out of its folder by naming its parent, and again with every link followed, so a link
//! inside a folder can't lead out of it.

use std::rc::Rc;

use fennec_syntax::Diagnostic;

use crate::stdlib::fs::path_text;
use crate::value::{Capability, Value};

#[derive(Debug, PartialEq)]
pub(crate) struct Limited {
    pub(crate) capability: Capability,
    /// The programs a `Process` may run, or the folders an `Fs` may reach, as absolute paths.
    pub(crate) allowed: Vec<Rc<str>>,
}

impl Limited {
    pub(crate) fn to(capability: Capability, allowed: Vec<Rc<str>>) -> Value {
        Value::Limited(Rc::new(Limited { capability, allowed }))
    }

    /// What's allowed, for a message: "`git` or `ls`", or "nothing".
    fn listed(&self) -> String {
        if self.allowed.is_empty() {
            return "nothing".to_owned();
        }
        let names: Vec<String> = self.allowed.iter().map(|name| format!("`{name}`")).collect();
        names.join(" or ")
    }
}

/// A path made absolute, against the folder the program runs in, with `.` and `..` worked out.
/// Its parts are what the system says they are, so on Windows `\\` separates them as `/` does,
/// and a drive stays in front. It's written with `/` between its parts on every system, so paths
/// compare as text.
pub(crate) fn absolute(path: &str) -> String {
    // A path that has a drive stays as it is, and one from the root without a drive, on Windows,
    // is on the drive the program runs on.
    let joined = std::env::current_dir().unwrap_or_default().join(path);
    plain(&joined)
}

/// A path's parts, `.` and `..` worked out, written with `/` between them: `/home/a` on Unix, and
/// `C:/Users/a` on Windows, where a path the system has made whole (`\\?\C:\...`) reads the same.
fn plain(path: &std::path::Path) -> String {
    use std::path::{Component, Prefix};
    let mut front = String::new();
    let mut parts: Vec<String> = Vec::new();
    for part in path.components() {
        match part {
            Component::Prefix(prefix) => {
                front = match prefix.kind() {
                    Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => format!("{}:", char::from(drive)),
                    _ => prefix.as_os_str().to_string_lossy().replace('\\', "/"),
                };
            }
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => drop(parts.pop()),
            Component::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
        }
    }
    format!("{front}/{}", parts.join("/"))
}

/// `path` with every link in it followed, as far as it's there: what isn't yet, like a new file,
/// goes back on the end.
fn with_links_followed(path: &str) -> String {
    let mut there = std::path::Path::new(path);
    let mut missing: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(there) {
            let joined = missing.iter().rev().fold(real, |so_far, part| so_far.join(part));
            return plain(&joined);
        }
        let (Some(parent), Some(name)) = (there.parent(), there.file_name()) else {
            return path.to_owned();
        };
        missing.push(name);
        there = parent;
    }
}

/// Whether `path` is `folder` or inside it, both absolute, as [`plain`] writes them.
fn inside(path: &str, folder: &str) -> bool {
    if path == folder {
        return true;
    }
    // A folder that's a root, `/` or `C:/`, already ends with the separator.
    let folder = folder.strip_suffix('/').unwrap_or(folder);
    path.strip_prefix(folder).is_some_and(|rest| rest.starts_with('/'))
}

/// Runs a method of a whole capability.
type CallWhole<'c> = dyn FnMut(Capability, &str, &[Value]) -> Result<Value, Diagnostic> + 'c;

/// A method of a narrowed capability: what it may do goes to `call`, and what it may not fails.
pub(crate) fn narrowed(
    limited: &Rc<Limited>,
    method: &str,
    args: &[Value],
    call: &mut CallWhole<'_>,
) -> Result<Value, Diagnostic> {
    match (limited.capability, method) {
        (Capability::Process, "only") => {
            let Some(Value::Str(program)) = args.first() else {
                return call(Capability::Process, method, args);
            };
            let allowed = limited.allowed.iter().filter(|name| ***name == **program).cloned().collect();
            Ok(Limited::to(Capability::Process, allowed))
        }
        // `run.cwd` and the like are `run` with options, and run only what it may.
        (Capability::Process, run) if run == "run" || run.starts_with("run.") => {
            // Given a template, the program is its first word. One that's neither is refused
            // by the call, since a narrowed `Process` is never let past on a guess.
            let program: Rc<str> = match args.first() {
                Some(Value::Str(program)) => Rc::clone(program),
                Some(template) => match crate::stdlib::process::words_of(template) {
                    Some(words) => Rc::from(words.first().map_or("", String::as_str)),
                    None => return call(Capability::Process, method, args),
                },
                None => return call(Capability::Process, method, args),
            };
            if limited.allowed.iter().any(|name| **name == *program) {
                return call(Capability::Process, method, args);
            }
            let message =
                format!("this Process can run only {}, so it can't run `{program}`", limited.listed());
            Ok(crate::stdlib::process::process_error(&program, &message))
        }
        (Capability::Process, _) => call(Capability::Process, method, args),
        (Capability::Fs, _) => narrowed_fs(limited, method, args, call),
        (capability, _) => call(capability, method, args),
    }
}

fn narrowed_fs(
    limited: &Rc<Limited>,
    method: &str,
    args: &[Value],
    call: &mut CallWhole<'_>,
) -> Result<Value, Diagnostic> {
    let Some(Value::Str(path)) = args.first().map(path_text) else {
        return call(Capability::Fs, method, args);
    };
    let full = absolute(&path);
    let real = with_links_followed(&full);
    let reachable = limited
        .allowed
        .iter()
        .any(|folder| inside(&full, folder) && inside(&real, &with_links_followed(folder)));
    if method == "only" {
        let allowed = if reachable { vec![Rc::from(full)] } else { Vec::new() };
        return Ok(Limited::to(Capability::Fs, allowed));
    }
    if reachable {
        return call(Capability::Fs, method, args);
    }
    let reason = format!("`{path}` is outside what this Fs can reach: {}", limited.listed());
    Ok(crate::stdlib::fs::io_error(&reason))
}

#[cfg(test)]
mod tests {
    use super::{absolute, inside, plain};

    /// `.` and `..` are worked out, so a path can't climb out of a folder by naming its parent.
    #[test]
    fn a_path_is_made_plain() {
        let here = plain(&std::env::current_dir().unwrap_or_default());
        assert_eq!(absolute("a/./b/../c"), format!("{}/a/c", here.trim_end_matches('/')));
        assert!(inside(&absolute("saves/game.json"), &absolute("saves")));
        assert!(!inside(&absolute("saves/../secret"), &absolute("saves")));
        assert!(!inside(&absolute("savesmore/x"), &absolute("saves")));
        assert!(inside("/anything", "/"));
    }

    /// On Windows, `\\` separates a path's parts too, so `..` written with it climbs as with `/`,
    /// and a path the system made whole compares with one written plainly.
    #[cfg(windows)]
    #[test]
    fn windows_paths_cant_climb_out_with_backslashes() {
        assert!(!inside(&absolute("saves/..\\..\\secret"), &absolute("saves")));
        assert!(!inside(&absolute("saves\\..\\secret"), &absolute("saves")));
        assert!(inside(&absolute("saves\\game.json"), &absolute("saves")));
        assert_eq!(plain(std::path::Path::new("\\\\?\\C:\\Users\\a")), "C:/Users/a");
        assert!(inside("C:/x", "C:/"));
    }
}
