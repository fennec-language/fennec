//! Capabilities narrowed with `only`: a `Process` that can run only the programs named, and an
//! `Fs` that reaches only what's inside the folders named. An `Fs` narrowed with `read_only`
//! changes nothing, and a `Net` narrowed with `client`, `server` or `hosts` only connects out,
//! only listens, or reaches only the hosts named. Narrowing again only takes more away. Something outside what's allowed fails the way the capability fails, with a
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
    /// The programs a `Process` may run, the folders an `Fs` may reach, as absolute paths, or
    /// the hosts a `Net` may reach, with the marks below among them. A mark starts with a NUL,
    /// which no path, program or host has, so a list of text is all native code keeps.
    pub(crate) allowed: Vec<Rc<str>>,
}

/// An `Fs` or `Net` narrowed in what it may do, but not to folders or hosts.
pub(crate) const EVERYWHERE: &str = "\0everywhere";
/// An `Fs` that only reads.
pub(crate) const READ_ONLY: &str = "\0read-only";
/// A `Net` that only connects out.
pub(crate) const CLIENT: &str = "\0client";
/// A `Net` that only listens.
pub(crate) const SERVER: &str = "\0server";

impl Limited {
    pub(crate) fn to(capability: Capability, allowed: Vec<Rc<str>>) -> Value {
        Value::Limited(Rc::new(Limited { capability, allowed }))
    }

    /// Whether it carries `mark`.
    pub(crate) fn has(&self, mark: &str) -> bool {
        self.allowed.iter().any(|item| &**item == mark)
    }

    /// The programs, folders or hosts it names, without its marks.
    pub(crate) fn named(&self) -> impl Iterator<Item = &Rc<str>> {
        self.allowed.iter().filter(|item| !item.starts_with('\0'))
    }

    /// Its marks.
    fn marks(&self) -> impl Iterator<Item = Rc<str>> + '_ {
        self.allowed.iter().filter(|item| item.starts_with('\0')).cloned()
    }

    /// What's allowed, for a message: "`git` or `ls`", or "nothing".
    fn listed(&self) -> String {
        if self.has(EVERYWHERE) {
            return "everything".to_owned();
        }
        let names: Vec<String> = self.named().map(|name| format!("`{name}`")).collect();
        if names.is_empty() {
            return "nothing".to_owned();
        }
        names.join(" or ")
    }
}

/// Whether `host`, on `port`, is one `patterns` names: a name as it's written, in lower case; `*.`
/// in front for any names under a domain, though not the domain itself; an IP address as itself,
/// an IPv6 one in brackets; and `:port` after any of them for that port alone.
pub(crate) fn host_allowed<'p>(patterns: impl IntoIterator<Item = &'p str>, host: &str, port: u16) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    patterns.into_iter().any(|pattern| {
        let (name, wanted_port) = match pattern.rsplit_once(':') {
            Some((name, digits))
                if !digits.is_empty()
                    && digits.bytes().all(|b| b.is_ascii_digit())
                    && (name.ends_with(']') || !name.contains(':')) =>
            {
                (name, digits.parse::<u16>().ok())
            }
            _ => (pattern, None),
        };
        let name = name.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
        if wanted_port.is_some_and(|wanted| wanted != port) {
            return false;
        }
        match name.strip_prefix("*.") {
            Some(domain) => {
                host.strip_suffix(domain).is_some_and(|front| front.len() > 1 && front.ends_with('.'))
            }
            None => host == name,
        }
    })
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

/// A method of a narrowed `Net`: refused if it's one this `Net` can't make, saying why, and
/// otherwise run, with the hosts it may reach handed on to a request so its redirects are held
/// to them too.
fn narrowed_net(
    limited: &Rc<Limited>,
    method: &str,
    args: &[Value],
    call: &mut CallWhole<'_>,
) -> Result<Value, Diagnostic> {
    let denied =
        |reason: String| Ok(crate::stdlib::net::failed(&crate::stdlib::net::Failure::Denied(reason)));
    let base = method.split('.').next().unwrap_or(method);
    match base {
        "client" | "server" | "hosts" => {
            let mut allowed: Vec<Rc<str>> = limited.allowed.clone();
            let mark = if base == "server" { SERVER } else { CLIENT };
            if !limited.has(mark) {
                allowed.push(Rc::from(mark));
            }
            if base == "hosts" {
                let Some(Value::List(asked)) = args.first() else {
                    return call(Capability::Net, method, args);
                };
                // Only hosts this `Net` already reaches stay.
                let everywhere = limited.has(EVERYWHERE);
                let held: Vec<&str> = limited.named().map(|name| &**name).collect();
                allowed.retain(|item| item.starts_with('\0') && &**item != EVERYWHERE);
                for host in asked.iter() {
                    if let Value::Str(host) = host {
                        let host = host.to_ascii_lowercase();
                        if everywhere || held.iter().any(|&kept| kept == host) {
                            allowed.push(Rc::from(host));
                        }
                    }
                }
            }
            return Ok(Limited::to(Capability::Net, allowed));
        }
        "_listen" if limited.has(CLIENT) => {
            return denied("this Net only connects out, so it can't listen".to_owned());
        }
        "get" | "post" | "request" | "_connect" | "_udp" if limited.has(SERVER) => {
            return denied("this Net only listens, so it can't connect out".to_owned());
        }
        _ => {}
    }
    if limited.has(EVERYWHERE) || !matches!(base, "get" | "post" | "request" | "_connect" | "_udp") {
        return call(Capability::Net, method, args);
    }
    let hosts: Vec<Value> = limited.named().map(|host| Value::Str(Rc::clone(host))).collect();
    match base {
        "_udp" => denied(format!(
            "this Net reaches only {}, so it can't open a UDP socket, which could send anywhere",
            limited.listed()
        )),
        "_connect" => {
            let (Some(Value::Str(host)), Some(Value::Int(port))) = (args.first(), args.get(1)) else {
                return call(Capability::Net, method, args);
            };
            let port = u16::try_from(*port).unwrap_or(0);
            if host_allowed(limited.named().map(|name| &**name), host, port) {
                return call(Capability::Net, method, args);
            }
            denied(format!("`{}:{port}` is outside what this Net can reach: {}", cut(host), limited.listed()))
        }
        // A request is checked by its client, at its URL and at every redirect.
        _ => {
            let mut given = args.to_vec();
            given.push(Value::list(hosts));
            call(Capability::Net, &format!("{method}._hosts"), &given)
        }
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
            let allowed = limited.named().filter(|name| ***name == **program).cloned().collect();
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
            if limited.named().any(|name| **name == *program) {
                return call(Capability::Process, method, args);
            }
            let message =
                format!("this Process can run only {}, so it can't run `{program}`", limited.listed());
            Ok(crate::stdlib::process::process_error(&program, &message))
        }
        (Capability::Process, _) => call(Capability::Process, method, args),
        (Capability::Fs, _) => narrowed_fs(limited, method, args, (real, at), call),
        (Capability::Net, _) => narrowed_net(limited, method, args, call),
        (capability, _) => call(capability, method, args),
    }
}

/// The methods of an `Fs` that change what's on the disk, which one narrowed with `read_only`
/// refuses. `_sqlite_open` is let through, to open its database for reading only.
const FS_WRITES: [&str; 6] = ["write_text", "write_bytes", "make_dir", "remove", "_create", "_finish"];

fn narrowed_fs(
    limited: &Rc<Limited>,
    method: &str,
    args: &[Value],
    (real, at): (bool, Span),
    call: &mut CallWhole<'_>,
) -> Result<Value, Diagnostic> {
    if method == "read_only" {
        let mut allowed = limited.allowed.clone();
        if !limited.has(READ_ONLY) {
            allowed.push(Rc::from(READ_ONLY));
        }
        return Ok(Limited::to(Capability::Fs, allowed));
    }
    let read_only = limited.has(READ_ONLY);
    let Some(Value::Str(path)) = args.first().map(path_text) else {
        if read_only && FS_WRITES.contains(&method) {
            let reason = format!("this Fs only reads, so it can't `{method}`");
            return Ok(crate::stdlib::fs::io_failed(&crate::stdlib::fs::Fail::permission_denied(reason)));
        }
        return call(Capability::Fs, method, args);
    };
    if read_only && FS_WRITES.contains(&method) {
        let shown = cut(&path);
        let reason = format!("this Fs only reads, so it can't change `{shown}`");
        return Ok(crate::stdlib::fs::io_failed(&crate::stdlib::fs::Fail::permission_denied(reason)));
    }
    // An `Fs` that reaches everywhere is the whole one, but for what it may do.
    let method = if read_only && method == "_sqlite_open" { "_sqlite_read" } else { method };
    if limited.has(EVERYWHERE) {
        if method == "only" {
            let mut allowed: Vec<Rc<str>> = limited.marks().filter(|mark| &**mark != EVERYWHERE).collect();
            allowed.push(Rc::from(absolute(&path)));
            return Ok(Limited::to(Capability::Fs, allowed));
        }
        return call(Capability::Fs, method, args);
    }
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
        .named()
        .find(|folder| inside(&full, folder) && inside(&followed, &with_links_followed(folder)));
    if method == "only" {
        let mut allowed: Vec<Rc<str>> = limited.marks().collect();
        if reaching.is_some() {
            allowed.push(Rc::from(full));
        }
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

    /// Through an `Fs` narrowed to a folder and to reading, a database on the disk opens for
    /// reading only, and one that isn't there isn't made.
    #[test]
    fn a_read_only_fs_opens_a_database_to_read_and_makes_none() {
        use super::READ_ONLY;
        let folder = scratch("read-only-db");
        let made = crate::stdlib::sqlite::open_file(&format!("{folder}/kept.db"), false);
        assert!(matches!(made, Value::Ok(_)), "{made:?}");
        let fs = Rc::new(Limited {
            capability: Capability::Fs,
            allowed: vec![Rc::from(absolute(&folder)), Rc::from(READ_ONLY)],
        });
        let opened = call(&fs, "_sqlite_open", &[Value::str(&format!("{folder}/kept.db"))]);
        assert!(matches!(opened, Value::Ok(_)), "{opened:?}");
        let missing = call(&fs, "_sqlite_open", &[Value::str(&format!("{folder}/missing.db"))]);
        assert!(matches!(missing, Value::Err(_)), "{missing:?}");
        assert!(!std::path::Path::new(&format!("{folder}/missing.db")).exists(), "a database was made");
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

    /// A link inside a narrowed `Fs`'s folder to something outside it, or to nothing yet outside
    /// it, is refused for every method, saying so, and nothing is made or changed outside. A path
    /// holding a NUL is refused for what it is, not as the system's bad input.
    #[cfg(unix)]
    #[test]
    fn a_link_out_is_refused_for_every_method_and_a_nul_is_named() {
        let folder = scratch("links");
        std::fs::create_dir_all(format!("{folder}/inside/sub")).unwrap();
        std::fs::write(format!("{folder}/secret.txt"), "outside").unwrap();
        std::os::unix::fs::symlink("../secret.txt", format!("{folder}/inside/file")).unwrap();
        std::os::unix::fs::symlink("..", format!("{folder}/inside/up")).unwrap();
        std::os::unix::fs::symlink("../made.txt", format!("{folder}/inside/dangling")).unwrap();
        let fs = fs_in(&format!("{folder}/inside"));
        let at = |name: &str| Value::str(&format!("{folder}/inside/{name}"));
        for (method, args) in [
            ("read_text", vec![at("file")]),
            ("write_text", vec![at("file"), Value::str("changed")]),
            ("write_text", vec![at("dangling"), Value::str("made")]),
            ("write_text", vec![at("up/made.txt"), Value::str("made")]),
            ("list_dir", vec![at("up")]),
            ("make_dir", vec![at("up/new")]),
            ("remove", vec![at("up/secret.txt")]),
        ] {
            let got = call(&fs, method, &args);
            assert!(
                matches!(&got, Value::Err(_))
                    && format!("{got:?}").contains("outside what this Fs can reach"),
                "{method} {args:?} gave {got:?}"
            );
        }
        assert_eq!(std::fs::read_to_string(format!("{folder}/secret.txt")).unwrap(), "outside");
        for made in ["made.txt", "new"] {
            assert!(!std::path::Path::new(&format!("{folder}/{made}")).exists(), "{made} was made outside");
        }
        let Value::Err(nul) = call(&fs, "read_text", &[at("a\0b")]) else { panic!("a NUL was read") };
        assert!(format!("{nul:?}").contains("can't be a path: it holds a NUL"), "{nul:?}");
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

    /// A host is matched by name as it's written, `*.` covering what's under a domain but not the
    /// domain itself, an address as itself, and `:port` holding it to that port.
    #[test]
    fn hosts_match_as_written() {
        use super::host_allowed;
        let listed = ["api.example.com", "*.cdn.test", "10.0.0.5:5432", "[::1]:8080", "Mixed.Case"];
        for (host, port) in [
            ("api.example.com", 443),
            ("API.Example.com", 80),
            ("a.cdn.test", 443),
            ("a.b.cdn.test", 443),
            ("10.0.0.5", 5432),
            ("[::1]", 8080),
            ("::1", 8080),
            ("mixed.case", 1),
        ] {
            assert!(host_allowed(listed, host, port), "{host}:{port}");
        }
        for (host, port) in [
            ("example.com", 443),
            ("evil-api.example.com", 443),
            ("api.example.com.evil.test", 443),
            ("cdn.test", 443),
            ("xcdn.test", 443),
            ("10.0.0.5", 5433),
            ("10.0.0.6", 5432),
            ("::1", 8081),
        ] {
            assert!(!host_allowed(listed, host, port), "{host}:{port}");
        }
    }

    /// Narrowing a `Net` or an `Fs` again only takes more away, whichever way round.
    #[test]
    fn narrowing_again_only_takes_more_away() {
        use super::{CLIENT, EVERYWHERE, READ_ONLY, SERVER};
        let limited = |value: Value| match value {
            Value::Limited(limited) => limited,
            other => panic!("not narrowed: {other:?}"),
        };
        let mut never =
            |_: Capability, method: &str, _: &[Value]| -> Result<Value, fennec_syntax::Diagnostic> {
                panic!("`{method}` reached the whole capability")
            };
        let client = Rc::new(Limited {
            capability: Capability::Net,
            allowed: vec![Rc::from(EVERYWHERE), Rc::from(CLIENT)],
        });
        let both = limited(narrowed(&client, "server", &[], (true, Span::default()), &mut never).unwrap());
        assert!(both.has(CLIENT) && both.has(SERVER));
        let hosts = Value::list(vec![Value::str("A.test"), Value::str("b.test")]);
        let some =
            limited(narrowed(&client, "hosts", &[hosts], (true, Span::default()), &mut never).unwrap());
        assert!(!some.has(EVERYWHERE) && some.has(CLIENT));
        assert_eq!(some.named().map(ToString::to_string).collect::<Vec<_>>(), ["a.test", "b.test"]);
        let fewer = Value::list(vec![Value::str("b.test"), Value::str("c.test")]);
        let fewer = limited(narrowed(&some, "hosts", &[fewer], (true, Span::default()), &mut never).unwrap());
        assert_eq!(fewer.named().map(ToString::to_string).collect::<Vec<_>>(), ["b.test"]);
        let fs = Rc::new(Limited { capability: Capability::Fs, allowed: vec![Rc::from(absolute("saves"))] });
        let reading = limited(narrowed(&fs, "read_only", &[], (true, Span::default()), &mut never).unwrap());
        assert!(reading.has(READ_ONLY) && reading.named().count() == 1);
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
