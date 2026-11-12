//! Capabilities narrowed with `only`: a `Process` that can run only the programs named, and an
//! `Fs` that reaches only what's inside the folders named. Narrowing again only takes more
//! away. Something outside what's allowed fails the way the capability fails, with a
//! `ProcessError` or an `IoError` value that says what was allowed, rather than stopping the
//! program. Folders are compared by their paths with `.` and `..` worked out, so a path can't
//! climb out of its folder by naming its parent, and again with every link followed, so a link
//! inside a folder can't lead out of it. On the real file system, what an `Fs` narrowed to a
//! folder then does goes through a handle held on that folder, by names beneath it, so nothing
//! swapped in between the check and the work can lead out either.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

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

/// What makes `path` mean more on Windows than its text says, if anything: a `\\?\` or `\\.\`
/// start, which turns off Windows' own reading of `..` and names; a part ending in `.` or a space,
/// which Windows drops, so `.. ` is `..`; or a part that names a device, like `CON` or `NUL.txt`.
/// It reads `/` and `\` both as separators, as Windows does, so it can be tested anywhere.
fn windows_trick(path: &str) -> Option<String> {
    const DEVICES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    let slashed = path.replace('\\', "/");
    if slashed.starts_with("//?/") || slashed.starts_with("//./") {
        return Some(
            "it starts with `\\\\?\\` or `\\\\.\\`, which turns off how Windows reads a path".to_owned(),
        );
    }
    for part in slashed.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            continue;
        }
        if part.ends_with('.') || part.ends_with(' ') {
            let shown = cut(part);
            return Some(format!("its part `{shown}` ends in `.` or a space, which Windows drops"));
        }
        // A device is its name alone or before any extension, in any case: `nul.txt` is `NUL`.
        let stem = part.split('.').next().unwrap_or(part).trim_end().to_ascii_uppercase();
        let numbered = |kind: &str| {
            stem.strip_prefix(kind).is_some_and(|n| {
                matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
            })
        };
        if DEVICES.contains(&stem.as_str()) || numbered("COM") || numbered("LPT") {
            let shown = cut(part);
            return Some(format!("its part `{shown}` names a device on Windows"));
        }
    }
    None
}

/// A part of a path from outside as a message shows it: whole, or past 40 characters, its first 20
/// and `…`, with control characters escaped.
fn cut(part: &str) -> String {
    let shown: String = if part.chars().count() <= 40 {
        part.to_owned()
    } else {
        part.chars().take(20).chain(['…']).collect()
    };
    shown.escape_debug().to_string()
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

/// `path`, inside `folder`, as a path relative to it: `.` for the folder itself.
fn beneath(path: &str, folder: &str) -> std::path::PathBuf {
    let folder = folder.strip_suffix('/').unwrap_or(folder);
    let rest = path.strip_prefix(folder).unwrap_or_default().trim_start_matches('/');
    std::path::PathBuf::from(if rest.is_empty() { "." } else { rest })
}

/// Runs a method of a whole capability.
type CallWhole<'c> = dyn FnMut(Capability, &str, &[Value]) -> Result<Value, Diagnostic> + 'c;

/// A method of a narrowed capability: what it may do goes to `call`, and what it may not fails.
/// `real` says the whole capability is the real one rather than a test's fake, so a narrowed `Fs`
/// works beneath its folder's handle itself.
pub(crate) fn narrowed(
    limited: &Rc<Limited>,
    method: &str,
    args: &[Value],
    (real, at): (bool, Span),
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
        (Capability::Fs, _) => narrowed_fs(limited, method, args, (real, at), call),
        (capability, _) => call(capability, method, args),
    }
}

fn narrowed_fs(
    limited: &Rc<Limited>,
    method: &str,
    args: &[Value],
    (real, at): (bool, Span),
    call: &mut CallWhole<'_>,
) -> Result<Value, Diagnostic> {
    let Some(Value::Str(path)) = args.first().map(path_text) else {
        return call(Capability::Fs, method, args);
    };
    // Windows reads some paths as more than their text says, so a narrowed `Fs` refuses them there.
    if cfg!(windows)
        && let Some(why) = windows_trick(&path)
    {
        let reason = format!("`{path}` can't be reached through this Fs on Windows: {why}");
        return Ok(crate::stdlib::fs::io_failed(&crate::stdlib::fs::Fail::permission_denied(reason)));
    }
    let full = absolute(&path);
    let followed = with_links_followed(&full);
    let reaching = limited
        .allowed
        .iter()
        .find(|folder| inside(&full, folder) && inside(&followed, &with_links_followed(folder)));
    if method == "only" {
        let allowed = if reaching.is_some() { vec![Rc::from(full)] } else { Vec::new() };
        return Ok(Limited::to(Capability::Fs, allowed));
    }
    if let Some(folder) = reaching {
        if real {
            let paths: Vec<Value> = args.iter().map(path_text).collect();
            return crate::stdlib::fs::beneath(folder, (&full, &beneath(&full, folder)), method, &paths, at);
        }
        return call(Capability::Fs, method, args);
    }
    let reason = format!("`{path}` is outside what this Fs can reach: {}", limited.listed());
    Ok(crate::stdlib::fs::io_failed(&crate::stdlib::fs::Fail::permission_denied(reason)))
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use fennec_syntax::Span;

    use super::{Limited, absolute, inside, narrowed, plain};
    use crate::stdlib::fs::path_text;
    use crate::value::{Capability, Value};

    /// A new, empty folder of this test's own.
    fn scratch(name: &str) -> String {
        let folder = std::env::temp_dir().join(format!("fennec-beneath-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder.to_string_lossy().into_owned()
    }

    /// The real `Fs` narrowed to `folder`.
    fn fs_in(folder: &str) -> Rc<Limited> {
        Rc::new(Limited { capability: Capability::Fs, allowed: vec![Rc::from(absolute(folder))] })
    }

    /// A method of the narrowed real `Fs`, as compiled code calls it.
    fn call(fs: &Rc<Limited>, method: &str, args: &[Value]) -> Value {
        narrowed(fs, method, args, (true, Span::default()), &mut |capability, method, args| {
            assert_eq!(capability, Capability::Fs);
            let paths: Vec<Value> = args.iter().map(path_text).collect();
            crate::stdlib::fs::real_fs(method, &paths, Span::default())
        })
        .unwrap()
    }

    fn ok(given: Value) -> Value {
        match given {
            Value::Ok(value) => (*value).clone(),
            other => panic!("not Ok: {other:?}"),
        }
    }

    /// The texts of a list of `Path`s.
    fn texts(paths: &Value) -> Vec<String> {
        let Value::List(paths) = paths else { panic!("not a list: {paths:?}") };
        paths
            .iter()
            .map(|path| match path {
                Value::Instance(path) => match path.field("text") {
                    Some(Value::Str(text)) => text.to_string(),
                    other => panic!("a Path's text isn't text: {other:?}"),
                },
                other => panic!("not a Path: {other:?}"),
            })
            .collect()
    }

    fn handle(file: &Value) -> i64 {
        match file {
            Value::Instance(file) => match file.field("_handle") {
                Some(Value::Int(handle)) => *handle,
                _ => panic!("no handle"),
            },
            other => panic!("not a File: {other:?}"),
        }
    }

    /// Every method of an `Fs` narrowed to a folder works on what's beneath it, through the
    /// folder's handle: a file written takes its place whole, by name beneath the folder, and one
    /// thrown away leaves nothing behind.
    #[test]
    fn a_narrowed_fs_does_all_it_does_beneath_its_folder() {
        let folder = scratch("works");
        let fs = fs_in(&folder);
        let at = |name: &str| Value::str(&format!("{folder}/{name}"));
        ok(call(&fs, "make_dir", &[at("sub/deeper")]));
        ok(call(&fs, "write_text", &[at("sub/note.txt"), Value::str("hello")]));
        assert_eq!(ok(call(&fs, "exists", &[at("sub/note.txt")])), Value::Bool(true));
        assert_eq!(ok(call(&fs, "exists", &[at("sub/none.txt")])), Value::Bool(false));
        assert_eq!(ok(call(&fs, "read_text", &[at("sub/note.txt")])), Value::str("hello"));
        assert_eq!(
            ok(call(&fs, "read_bytes", &[at("sub/note.txt")])),
            Value::Bytes(Rc::from(b"hello".to_vec()))
        );
        let Value::Instance(found) = ok(call(&fs, "metadata", &[at("sub/note.txt")])) else {
            panic!("no metadata")
        };
        assert_eq!(
            (found.field("size"), found.field("is_file")),
            (Some(&Value::Int(5)), Some(&Value::Bool(true)))
        );
        let made = ok(call(&fs, "_create", &[at("sub/made.txt"), Value::Bool(true)]));
        ok(crate::stdlib::fs::real_file(handle(&made), "write", &[Value::str("made")], Span::default())
            .unwrap());
        ok(call(&fs, "_finish", &[made]));
        assert_eq!(ok(call(&fs, "read_text", &[at("sub/made.txt")])), Value::str("made"));
        let dropped = ok(call(&fs, "_create", &[at("sub/never.txt"), Value::Bool(false)]));
        call(&fs, "_discard", &[dropped]);
        let opened = ok(call(&fs, "_open", &[at("sub/note.txt")]));
        let read = crate::stdlib::fs::real_file(handle(&opened), "read", &[], Span::default()).unwrap();
        assert_eq!(ok(read), Value::Bytes(Rc::from(b"hello".to_vec())));
        call(&fs, "_finish", &[opened]);
        let listed = ok(call(&fs, "list_dir", &[at("sub")]));
        let names = ["deeper", "made.txt", "note.txt"].map(|name| format!("{folder}/sub/{name}"));
        assert_eq!(texts(&listed), names);
        let walked = ok(call(&fs, "walk", &[Value::str(&folder)]));
        let mut wanted = vec![format!("{folder}/sub")];
        wanted.extend(names);
        assert_eq!(texts(&walked), wanted);
        ok(call(&fs, "remove", &[at("sub/made.txt")]));
        assert_eq!(ok(call(&fs, "exists", &[at("sub/made.txt")])), Value::Bool(false));
        #[cfg(unix)]
        {
            // A link inside the folder to a file inside it is written through, and stays a link.
            std::os::unix::fs::symlink("note.txt", format!("{folder}/sub/link.txt")).unwrap();
            ok(call(&fs, "write_text", &[at("sub/link.txt"), Value::str("again")]));
            assert_eq!(ok(call(&fs, "read_text", &[at("sub/note.txt")])), Value::str("again"));
            assert!(std::fs::symlink_metadata(format!("{folder}/sub/link.txt")).unwrap().is_symlink());
            assert!(matches!(call(&fs, "_sqlite_open", &[at("sub/data.db")]), Value::Ok(_)));
        }
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// A path out of a narrowed `Fs`'s folder, by `..` or by a link inside it, is refused.
    #[test]
    fn a_narrowed_fs_refuses_what_lies_outside_its_folder() {
        let folder = scratch("refuses");
        std::fs::create_dir_all(format!("{folder}/inside")).unwrap();
        std::fs::write(format!("{folder}/secret.txt"), "outside").unwrap();
        let fs = fs_in(&format!("{folder}/inside"));
        let climbing = Value::str(&format!("{folder}/inside/../secret.txt"));
        assert!(matches!(call(&fs, "read_text", &[climbing]), Value::Err(_)));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(format!("{folder}/secret.txt"), format!("{folder}/inside/out.txt"))
                .unwrap();
            let linked = Value::str(&format!("{folder}/inside/out.txt"));
            assert!(matches!(call(&fs, "read_text", &[linked]), Value::Err(_)));
        }
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// Another process swapping a folder inside a narrowed `Fs`'s for a link to one outside, and
    /// back, over and over while the `Fs` reads a file under it, never lets it read the file
    /// outside: each read gives the file inside, or fails.
    #[cfg(unix)]
    #[test]
    fn a_folder_swapped_for_a_link_out_never_lets_a_narrowed_fs_read_outside() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let base = scratch("race");
        let (inside, outside) = (format!("{base}/inside"), format!("{base}/outside"));
        let (staged, link, sub) = (format!("{base}/staged"), format!("{base}/link"), format!("{inside}/sub"));
        for made in [&inside, &outside, &staged] {
            std::fs::create_dir_all(made).unwrap();
        }
        std::fs::write(format!("{outside}/secret.txt"), "OUTSIDE").unwrap();
        std::fs::write(format!("{staged}/secret.txt"), "INSIDE").unwrap();
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        std::fs::rename(&staged, &sub).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let swapping = {
            let stop = Arc::clone(&stop);
            let (staged, link, sub) = (staged.clone(), link.clone(), sub.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = std::fs::rename(&sub, &staged);
                    let _ = std::fs::rename(&link, &sub);
                    let _ = std::fs::rename(&sub, &link);
                    let _ = std::fs::rename(&staged, &sub);
                }
            })
        };
        let fs = fs_in(&inside);
        let secret = Value::str(&format!("{sub}/secret.txt"));
        let (mut read, mut refused) = (0, 0);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < until {
            match call(&fs, "read_text", std::slice::from_ref(&secret)) {
                Value::Ok(text) => {
                    assert_eq!(*text, Value::str("INSIDE"), "a narrowed Fs read outside its folder");
                    read += 1;
                }
                _ => refused += 1,
            }
        }
        stop.store(true, Ordering::Relaxed);
        swapping.join().unwrap();
        assert!(read > 0 && refused > 0, "{read} read, {refused} refused: the swap never raced the reads");
        let _ = std::fs::remove_dir_all(&base);
    }

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

    /// What Windows reads as more than a path's text says is found anywhere, so it's tested here on
    /// every system, though only a narrowed `Fs` on Windows refuses it.
    #[test]
    fn windows_tricks_in_a_path_are_found() {
        use super::windows_trick;
        for fine in [
            "saves/game.json",
            "saves\\game.json",
            "a/./b/../c",
            "C:/data/x",
            "\\\\server\\share\\x",
            "notes.txt",
            ".hidden",
            "console.txt",
            "COM10",
            "nullable",
        ] {
            assert_eq!(windows_trick(fine), None, "{fine}");
        }
        for (asked, said) in [
            ("\\\\?\\C:\\data\\..\\x", "starts with"),
            ("//?/C:/x", "starts with"),
            ("\\\\.\\PhysicalDrive0", "starts with"),
            ("saves/.. /secret", "ends in"),
            ("saves\\x.\\y", "ends in"),
            ("saves/secret.", "ends in"),
            ("saves/CON", "device"),
            ("saves/nul.txt", "device"),
            ("Aux", "device"),
            ("saves\\com1", "device"),
            ("LPT9.log", "device"),
            ("CON .txt", "device"),
        ] {
            let found = windows_trick(asked).unwrap_or_default();
            assert!(found.contains(said), "{asked}: {found}");
        }
    }

    /// On Windows, a narrowed `Fs` refuses what Windows reads as more than its text says, before it
    /// looks at the disk: `.. ` climbing out, a device, and a path that turns Windows' reading off.
    #[cfg(windows)]
    #[test]
    fn a_narrowed_fs_on_windows_refuses_its_tricks() {
        let folder = scratch("windows-tricks");
        std::fs::create_dir_all(format!("{folder}/inside")).unwrap();
        std::fs::write(format!("{folder}/secret.txt"), "outside").unwrap();
        std::fs::write(format!("{folder}/inside/fine.txt"), "inside").unwrap();
        let fs = fs_in(&format!("{folder}/inside"));
        for asked in [
            format!("{folder}/inside/.. /secret.txt"),
            format!("{folder}\\inside\\..\\secret.txt"),
            format!("{folder}/inside/secret.txt."),
            format!("{folder}/inside/CON"),
            format!("{folder}/inside/nul.txt"),
            format!("\\\\?\\{}\\inside\\..\\secret.txt", folder.replace('/', "\\")),
        ] {
            let read = call(&fs, "read_text", &[Value::str(&asked)]);
            assert!(matches!(read, Value::Err(_)), "{asked}: {read:?}");
        }
        // Either separator, mixed, reaches what's inside.
        let mixed = Value::str(&format!("{folder}\\inside/fine.txt"));
        assert!(matches!(call(&fs, "read_text", &[mixed]), Value::Ok(_)));
        let _ = std::fs::remove_dir_all(&folder);
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
