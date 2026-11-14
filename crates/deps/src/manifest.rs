//! `kit.toml`, which marks a kit's root: its name, its version, the Fennec it needs, and the kits
//! it depends on.
//!
//! This reads only the few keys a kit has so far, with a small reader of its own.
//! TODO: share one TOML reader with `std.toml` and keep these checks on top of it. It needs the
//! span of each key so errors can still point at their line.

use fennec_syntax::{Diagnostic, Span};

/// What `kit.toml` says about a kit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    /// The Fennec toolchain the kit is built with.
    pub fennec: String,
    /// The kits in its `[dependencies]` table, in the order it lists them.
    pub dependencies: Vec<Dependency>,
}

/// A kit another kit depends on, by the name it imports it as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    pub source: Source,
}

/// Where a dependency comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A git repository at one commit, named by its whole hash, never a branch or a tag, which can
    /// move. `folder` is the kit's folder inside the repository, when it isn't at the top.
    Git { url: String, commit: String, folder: Option<String> },
    /// A folder on this machine, relative to the kit that names it: work in progress.
    Path(String),
}

impl Manifest {
    /// Reads `kit.toml`'s text, giving every problem found.
    pub fn parse(text: &str) -> Result<Manifest, Vec<Diagnostic>> {
        parse(text, 0)
    }
}

const KEYS: &str = "`name`, `version` and `fennec`";

/// Reads `kit.toml`'s text, whose spans start at `base`.
pub fn parse(text: &str, base: u32) -> Result<Manifest, Vec<Diagnostic>> {
    let mut reader = Reader {
        base,
        errors: Vec::new(),
        table: None,
        name: None,
        version: None,
        fennec: None,
        dependencies: Vec::new(),
    };
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        reader.line(line.trim_end_matches(['\n', '\r']), offset);
        offset += line.len();
    }
    let top = reader.span(0, 0);
    let mut take = |value: Option<String>, key: &str, example: &str| {
        if value.is_none() {
            reader.errors.push(
                Diagnostic::new(format!("kit.toml needs `{key}`"), top)
                    .with_help(format!("add `{key} = {example}`")),
            );
        }
        value.unwrap_or_default()
    };
    let manifest = Manifest {
        name: take(reader.name.take(), "name", "\"my_game\""),
        version: take(reader.version.take(), "version", "\"0.1.0\""),
        fennec: take(reader.fennec.take(), "fennec", &format!("\"{}\"", env!("CARGO_PKG_VERSION"))),
        dependencies: std::mem::take(&mut reader.dependencies),
    };
    if reader.errors.is_empty() { Ok(manifest) } else { Err(reader.errors) }
}

/// Whether `name` can name a kit: lowercase letters, digits and `_`, starting with a letter,
/// so it can be imported like any module.
pub fn valid_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A version's three numbers, if it's written `major.minor.patch`.
pub(crate) fn version_numbers(version: &str) -> Option<[u64; 3]> {
    let mut numbers = version.split('.').map(|part| {
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) { None } else { part.parse().ok() }
    });
    let found = [numbers.next()??, numbers.next()??, numbers.next()??];
    if numbers.next().is_some() { None } else { Some(found) }
}

struct Reader {
    base: u32,
    errors: Vec<Diagnostic>,
    /// The `[table]` the lines are in, or None before the first one.
    table: Option<String>,
    name: Option<String>,
    version: Option<String>,
    fennec: Option<String>,
    dependencies: Vec<Dependency>,
}

impl Reader {
    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(start, end).shifted(self.base)
    }

    fn line(&mut self, line: &str, offset: usize) {
        let content = strip_comment(line).trim_end();
        let indent = content.len() - content.trim_start().len();
        let content = content.trim_start();
        let span = self.span(offset + indent, offset + indent + content.len());
        if content.is_empty() {
            return;
        }
        if let Some(header) = content.strip_prefix('[') {
            self.header(header, span);
        } else if let Some((key, value)) = content.split_once('=') {
            self.entry(key.trim(), value.trim(), span);
        } else {
            self.errors.push(
                Diagnostic::new("expected `key = \"value\"` or a `[table]`", span)
                    .with_help(format!("kit.toml has {KEYS}, and a `[dependencies]` table")),
            );
        }
    }

    fn header(&mut self, header: &str, span: Span) {
        let name = header.strip_suffix(']').map(str::trim);
        if name == Some("dependencies") {
            self.table = Some("dependencies".to_owned());
        } else {
            self.errors.push(
                Diagnostic::new(format!("kit.toml has no table `[{header}`"), span)
                    .with_help("the only table so far is `[dependencies]`"),
            );
        }
    }

    fn entry(&mut self, key: &str, value: &str, span: Span) {
        if self.table.is_some() {
            match dependency(key, value) {
                Ok(dependency) if self.dependencies.iter().any(|other| other.name == dependency.name) => {
                    self.errors.push(Diagnostic::new(format!("`{key}` is listed twice"), span));
                }
                Ok(dependency) => self.dependencies.push(dependency),
                Err((message, help)) => self.errors.push(Diagnostic::new(message, span).with_help(help)),
            }
            return;
        }
        let text = string(value);
        let problem = match (key, &text) {
            ("name" | "version" | "fennec", None) => Some((
                format!("`{key}` should be a string in double quotes"),
                format!("write `{key} = \"...\"`"),
            )),
            ("name", Some(text)) if !valid_name(text) => Some((
                format!("`{text}` can't name a kit"),
                "a kit's name is lowercase letters, digits and `_`, starting with a letter".to_owned(),
            )),
            // Whoever works on Fennec itself runs the build they linked: see `fennec toolchain link`.
            ("fennec", Some(text)) if text == DEV => None,
            ("version" | "fennec", Some(text)) if version_numbers(text).is_none() => Some((
                format!("`{text}` isn't a version"),
                "write versions as three numbers, like \"0.1.0\"".to_owned(),
            )),
            ("fennec", Some(text)) if newer_than_this(text) => Some((
                format!("this kit needs Fennec {text}, and this is Fennec {}", env!("CARGO_PKG_VERSION")),
                "update Fennec to run it".to_owned(),
            )),
            ("name" | "version" | "fennec", Some(_)) => None,
            _ => Some((format!("kit.toml has no key called `{key}`"), format!("kit.toml has {KEYS}"))),
        };
        let slot = match key {
            "name" => Some(&mut self.name),
            "version" => Some(&mut self.version),
            "fennec" => Some(&mut self.fennec),
            _ => None,
        };
        // A key with a bad value still counts as written, so it isn't reported missing too.
        let twice = slot.is_some_and(|slot| slot.replace(text.unwrap_or_default()).is_some());
        if let Some((message, help)) = problem {
            self.errors.push(Diagnostic::new(message, span).with_help(help));
        } else if twice {
            self.errors.push(Diagnostic::new(format!("`{key}` is set twice"), span));
        }
    }
}

/// The names a dependency can't have: the standard library's, a kit's own `main` and `lib`, and
/// `test`. `fennec` has an error of its own.
const RESERVED: [&str; 4] = ["std", "main", "lib", "test"];

/// What `fennec = ...` says for a kit built with a local build of Fennec.
const DEV: &str = "dev";

const DEPENDENCY_HELP: &str = "write `name = { git = \"https://...\", commit = \"<its whole hash>\" }`, or `name = { path = \"../name\" }`";

/// Why a dependency can't have the name `name`, with what to do instead.
fn name_refused(name: &str) -> Result<(), (String, String)> {
    if !valid_name(name) {
        return Err((
            format!("`{name}` can't name a kit"),
            "a kit's name is lowercase letters, digits and `_`, starting with a letter".to_owned(),
        ));
    }
    if name == "fennec" {
        return Err((
            "the Fennec a kit builds with isn't a dependency".to_owned(),
            "say which with `fennec = \"0.4.2\"` at the top of kit.toml, before any table".to_owned(),
        ));
    }
    // Names a program already means something by: a dependency named one would be shadowed, or
    // would shadow.
    if RESERVED.contains(&name) {
        return Err((
            format!("`{name}` is a name Fennec keeps for itself, so a dependency can't have it"),
            "give the dependency another name here: a kit is imported by the name kit.toml gives it"
                .to_owned(),
        ));
    }
    Ok(())
}

/// One line of `[dependencies]`: `physics = { git = "...", commit = "..." }`, with a `folder` when
/// the kit isn't at the top of its repository, or `tools = { path = "../tools" }`.
fn dependency(name: &str, value: &str) -> Result<Dependency, (String, String)> {
    name_refused(name)?;
    if string(value).is_some() {
        return Err((
            format!("`{name}` asks for a version from the Den, which isn't built yet"),
            format!("for now a kit comes from git or a path: {DEPENDENCY_HELP}"),
        ));
    }
    if has_gets(value) {
        return Err((
            format!("`{name}`'s capabilities aren't written in kit.toml"),
            "the compiler works out what each kit is handed, and `fennec update` shows it and records it in kit.lock: take `gets` out".to_owned(),
        ));
    }
    let Some(pairs) = inline_table(value) else {
        return Err((format!("`{name}` should be a table in braces"), DEPENDENCY_HELP.to_owned()));
    };
    let get = |key: &str| pairs.iter().find(|(had, _)| had == key).map(|(_, value)| value.clone());
    if let Some((key, _)) =
        pairs.iter().find(|(key, _)| !matches!(key.as_str(), "git" | "commit" | "folder" | "path"))
    {
        return Err((format!("a dependency has no key called `{key}`"), DEPENDENCY_HELP.to_owned()));
    }
    let source = match (get("git"), get("commit"), get("folder"), get("path")) {
        (Some(url), Some(commit), folder, None) => {
            if !known_address(&url) {
                return Err((
                    format!("`{url}` isn't a git repository's address"),
                    "give it as `https://...`, `ssh://...` or `git@host:path`".to_owned(),
                ));
            }
            if !whole_commit(&commit) {
                return Err((
                    format!("`{commit}` isn't a commit's whole hash"),
                    "a dependency is pinned to the commit's whole hash, 40 hex digits as `git rev-parse` gives it, never a branch, a tag or a short hash, which can change or name more than one".to_owned(),
                ));
            }
            if let Some(folder) = &folder
                && !plain_folder(folder)
            {
                return Err((
                    format!("`{folder}` isn't a folder inside the repository"),
                    "give it from the repository's top, like `kits/postgres`".to_owned(),
                ));
            }
            Source::Git { url, commit, folder }
        }
        (None, None, None, Some(path)) if !path.is_empty() => Source::Path(path),
        (None, Some(_), _, None) | (None, _, Some(_), None) => {
            return Err((
                format!("`{name}` has a `commit` or a `folder` but no `git`"),
                DEPENDENCY_HELP.to_owned(),
            ));
        }
        (Some(_), None, _, None) => {
            return Err((
                format!("`{name}` needs the `commit` it's pinned to"),
                "add `commit = \"<its whole hash>\"`: a dependency from git is pinned to one commit"
                    .to_owned(),
            ));
        }
        (Some(_), _, _, Some(_)) => {
            return Err((format!("`{name}` has both `git` and `path`"), "give one or the other".to_owned()));
        }
        _ => {
            return Err((
                format!("`{name}` says where it comes from with `git` or `path`"),
                DEPENDENCY_HELP.to_owned(),
            ));
        }
    };
    Ok(Dependency { name: name.to_owned(), source })
}

/// Whether a dependency's line has a `gets` key, outside any string: what a kit is handed isn't
/// written in kit.toml, as the compiler works it out.
fn has_gets(value: &str) -> bool {
    let mut in_string = false;
    // The last character outside a string: a key starts after `{` or `,`.
    let mut before = '{';
    for (at, c) in value.char_indices() {
        if c == '"' {
            in_string = !in_string;
        }
        let rest = value.get(at..).unwrap_or_default();
        if !in_string
            && matches!(before, '{' | ',')
            && rest.strip_prefix("gets").is_some_and(|after| after.trim_start().starts_with('='))
        {
            return true;
        }
        if !in_string && !c.is_whitespace() {
            before = c;
        }
    }
    false
}

/// Whether a commit is named by its whole hash: 40 lowercase hex digits, or 64 in a repository
/// that uses SHA-256.
fn whole_commit(commit: &str) -> bool {
    matches!(commit.len(), 40 | 64) && commit.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether a folder inside a repository is given plainly, from its top: no `..`, `.`, empty
/// parts, leading `/` or spaces.
fn plain_folder(folder: &str) -> bool {
    !folder.is_empty()
        && !folder.contains(char::is_whitespace)
        && folder
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains('\\'))
}

/// The pairs of an inline table of strings, `{ key = "value", other = "value" }`, or None when
/// it isn't one.
fn inline_table(value: &str) -> Option<Vec<(String, String)>> {
    let mut rest = value.trim().strip_prefix('{')?.strip_suffix('}')?.trim();
    let mut pairs: Vec<(String, String)> = Vec::new();
    while !rest.is_empty() {
        let (key, after) = rest.split_once('=')?;
        let key = key.trim();
        let after = after.trim_start().strip_prefix('"')?;
        let end = after.find('"')?;
        let text = after.get(..end)?;
        if text.contains('\\') || pairs.iter().any(|(had, _)| had == key) || key.is_empty() {
            return None;
        }
        pairs.push((key.to_owned(), text.to_owned()));
        rest = after.get(end + 1..)?.trim_start();
        if let Some(next) = rest.strip_prefix(',') {
            rest = next.trim_start();
            if rest.is_empty() {
                return None;
            }
        } else if !rest.is_empty() {
            return None;
        }
    }
    Some(pairs)
}

/// Whether a kit asking for Fennec `version` needs a newer one than this.
fn newer_than_this(version: &str) -> bool {
    version_numbers(version) > version_numbers(env!("CARGO_PKG_VERSION"))
}

/// The line without a `#` comment, unless the `#` is inside a string.
fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    for (at, c) in line.char_indices() {
        match c {
            '"' => in_string = !in_string,
            '#' if !in_string => return line.get(..at).unwrap_or(line),
            _ => {}
        }
    }
    line
}

/// A basic string's contents. Escapes aren't needed by any key yet, so they're refused.
fn string(value: &str) -> Option<String> {
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    if inner.contains(['"', '\\']) { None } else { Some(inner.to_owned()) }
}

/// Whether a git address is one of the kinds a dependency may have: `https://`, `ssh://`,
/// `user@host:path`, or `file://` for a repository on this machine. Any other `scheme://` would
/// have git run a program named for it, `git-remote-<scheme>`, from wherever the `PATH` finds one.
fn known_address(url: &str) -> bool {
    if url.is_empty() || url.starts_with('-') || url.contains("::") || url.contains(char::is_whitespace) {
        return false;
    }
    if let Some((scheme, _)) = url.split_once("://") {
        return matches!(scheme, "https" | "ssh" | "file");
    }
    // `user@host:path`, as `git@github.com:owner/kit.git`: a user, a host and a path, with no `/`
    // before the `:`, which would make it a local path instead.
    url.split_once(':').is_some_and(|(before, _)| before.contains('@') && !before.contains('/'))
}

#[cfg(test)]
mod address_tests {
    use super::{dependency, known_address};

    /// A dependency can't take a name Fennec keeps for itself: the standard library's, a kit's own
    /// `main` and `lib`, `test`, or `fennec`.
    #[test]
    fn names_fennec_keeps_are_refused() {
        let from = "{ path = \"../tools\" }";
        for kept in ["std", "main", "lib", "test"] {
            let (said, _) = dependency(kept, from).unwrap_err();
            assert!(said.contains("is a name Fennec keeps for itself"), "{kept}: {said}");
        }
        assert!(dependency("fennec", from).is_err());
        assert!(dependency("tools", from).is_ok());
    }

    /// A dependency's git address is `https://`, `ssh://`, `user@host:path`, or `file://` for a
    /// repository on this machine. Any other scheme would have git run a helper named for it from
    /// the `PATH`, and a plain local path, a leading `-` or a `::` transport is refused.
    #[test]
    fn a_dependency_by_a_local_path_or_an_unknown_scheme_is_refused() {
        for taken in [
            "https://github.com/owner/kit.git",
            "ssh://git@github.com/owner/kit.git",
            "git@github.com:owner/kit.git",
            "file:///home/me/kits/physics",
        ] {
            assert!(known_address(taken), "{taken}");
        }
        for refused in [
            "evil://host/kit",
            "http://example.com/kit.git",
            "ext::sh -c touch% /tmp/pwned",
            "-upload-pack=touch",
            "../physics",
            "/home/me/kits/physics",
            "C:/kits/physics",
            "./a@b:c",
            "",
        ] {
            assert!(!known_address(refused), "{refused}");
        }
    }
}
