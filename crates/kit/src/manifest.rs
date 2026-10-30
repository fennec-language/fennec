//! `kit.toml`, which marks a kit's root: its name, its version and the Fennec it needs.
//!
//! This reads only the few keys a kit has so far, with a small reader of its own.
//! TODO: switch to `std.toml` (`crates/interp/src/formats/toml.rs`) and keep these checks on
//! top of it. It needs a way to use it outside the interpreter, and the span of each key so
//! errors can still point at their line.

use fennec_syntax::{Diagnostic, Span};

/// What `kit.toml` says about a kit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    /// The Fennec toolchain the kit is built with.
    pub fennec: String,
}

impl Manifest {
    /// Reads `kit.toml`'s text, giving every problem found.
    pub fn parse(text: &str) -> Result<Manifest, Vec<Diagnostic>> {
        parse(text, 0)
    }
}

const KEYS: &str = "`name`, `version` and `fennec`";

/// Reads `kit.toml`'s text, whose spans start at `base`.
pub(crate) fn parse(text: &str, base: u32) -> Result<Manifest, Vec<Diagnostic>> {
    let mut reader =
        Reader { base, errors: Vec::new(), table: None, name: None, version: None, fennec: None };
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
            self.errors.push(Diagnostic::new("dependencies aren't supported yet", span).with_help(
                "for now a kit is the files under its `src` folder; leave `[dependencies]` empty",
            ));
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
