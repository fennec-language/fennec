//! `fennec init`: sets up the current folder. A new folder gets a kit, and any folder gets
//! Fennec's guidance for coding agents in its `AGENTS.md`.
//!
//! The guidance ships inside `fennec`, so it matches the toolchain and works offline. It's one
//! marked block, the only part of the file this ever changes.

use std::path::Path;

use crate::manifest::valid_name;
use crate::starter;

/// Fennec's block for `AGENTS.md`, markers included.
pub const GUIDANCE: &str = include_str!("guidance.md");

const START: &str = "<!-- fennec:start";
const END: &str = "<!-- fennec:end -->";

/// What `fennec init` did to `AGENTS.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guidance {
    Added,
    Refreshed,
    AlreadyCurrent,
}

/// What `fennec init` did: the name of the kit it made, if it made one, and to the guidance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Initialized {
    pub kit: Option<String>,
    pub guidance: Guidance,
}

/// Sets up the folder `dir`. Everything it needs to refuse is found before anything's written.
pub fn init(dir: &Path) -> Result<Initialized, String> {
    let agents = dir.join("AGENTS.md");
    let current = match std::fs::read(&agents) {
        Ok(bytes) => Some(
            String::from_utf8(bytes)
                .map_err(|_| format!("{} isn't text, so it was left alone", agents.display()))?,
        ),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(format!("couldn't read {}: {err}", agents.display())),
    };
    let updated = with_guidance(current.as_deref()).map_err(|why| format!("{}: {why}", agents.display()))?;
    let kit = if is_new(dir)? { Some(new_kit(dir)?) } else { None };
    let guidance = match &current {
        Some(text) if *text == updated => Guidance::AlreadyCurrent,
        Some(text) if text.contains(START) => Guidance::Refreshed,
        _ => Guidance::Added,
    };
    if guidance != Guidance::AlreadyCurrent {
        std::fs::write(&agents, updated)
            .map_err(|err| format!("couldn't write {}: {err}", agents.display()))?;
    }
    Ok(Initialized { kit, guidance })
}

/// Whether a folder has nothing in it to be careful of. Hidden files, like `.git`, and
/// `AGENTS.md` don't count, so `git init` first is fine.
fn is_new(dir: &Path) -> Result<bool, String> {
    let entries = std::fs::read_dir(dir).map_err(|err| format!("couldn't read {}: {err}", dir.display()))?;
    Ok(entries.flatten().all(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        name.starts_with('.') || name == "AGENTS.md"
    }))
}

/// Makes a kit in `dir`, named after the folder.
fn new_kit(dir: &Path) -> Result<String, String> {
    let absolute =
        std::fs::canonicalize(dir).map_err(|err| format!("couldn't read {}: {err}", dir.display()))?;
    let folder = absolute.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let name = kit_name(&folder).ok_or_else(|| {
        format!(
            "the folder `{folder}` can't name a kit: use lowercase letters, digits and `_`, starting with a letter"
        )
    })?;
    starter::write(dir, &name)?;
    Ok(name)
}

/// The kit name a folder's name gives: `My-Game` is the kit `my_game`.
fn kit_name(folder: &str) -> Option<String> {
    let name = folder.to_lowercase().replace('-', "_");
    valid_name(&name).then_some(name)
}

/// `AGENTS.md`'s text with the current guidance in it, or why it can't be put there safely.
fn with_guidance(current: Option<&str>) -> Result<String, &'static str> {
    let Some(text) = current else { return Ok(GUIDANCE.to_owned()) };
    let markers = (text.matches(START).count(), text.matches(END).count());
    match (markers, text.find(START), text.find(END)) {
        ((0, 0), ..) if text.is_empty() => Ok(GUIDANCE.to_owned()),
        ((0, 0), ..) => {
            let gap = if text.ends_with("\n\n") {
                ""
            } else if text.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            Ok(format!("{text}{gap}{GUIDANCE}"))
        }
        ((1, 1), Some(start), Some(end)) if start < end => {
            let after = end + END.len();
            let (before, rest) = (text.get(..start).unwrap_or(""), text.get(after..).unwrap_or(""));
            let rest = rest.strip_prefix("\r\n").or_else(|| rest.strip_prefix('\n')).unwrap_or(rest);
            Ok(format!("{before}{GUIDANCE}{rest}"))
        }
        _ => Err("Fennec's block has a marker missing, out of order or repeated, so it was left alone"),
    }
}

#[cfg(test)]
mod tests {
    use super::{END, GUIDANCE, START, kit_name, with_guidance};

    #[test]
    fn a_new_file_is_just_the_block() {
        assert_eq!(with_guidance(None).unwrap(), GUIDANCE);
        assert_eq!(with_guidance(Some("")).unwrap(), GUIDANCE);
    }

    #[test]
    fn the_block_goes_after_what_a_file_has_with_a_line_between() {
        for text in ["# Mine", "# Mine\n", "# Mine\n\n"] {
            assert_eq!(with_guidance(Some(text)).unwrap(), format!("# Mine\n\n{GUIDANCE}"), "{text:?}");
        }
    }

    #[test]
    fn only_the_block_is_replaced() {
        let old = format!("# Before\n\n{START} old -->\nstale\n{END}\n\n## After\n");
        let refreshed = with_guidance(Some(&old)).unwrap();
        assert_eq!(refreshed, format!("# Before\n\n{GUIDANCE}\n## After\n"));
        assert_eq!(with_guidance(Some(&refreshed)).unwrap(), refreshed, "running again changes nothing");
    }

    #[test]
    fn broken_markers_leave_the_file_alone() {
        let twice = format!("{GUIDANCE}{GUIDANCE}");
        let backwards = format!("{END}\n{START} -->\n");
        for text in [format!("{START} -->\n"), END.to_owned(), twice, backwards] {
            assert!(with_guidance(Some(&text)).is_err(), "{text:?}");
        }
    }

    #[test]
    fn a_folder_names_a_kit_when_it_can() {
        assert_eq!(kit_name("My-Game").as_deref(), Some("my_game"));
        assert_eq!(kit_name("arena").as_deref(), Some("arena"));
        assert_eq!(kit_name("1st"), None);
        assert_eq!(kit_name("a b"), None);
    }

    #[test]
    fn the_guidance_is_one_block_ending_in_a_line() {
        assert!(GUIDANCE.starts_with(START) && GUIDANCE.ends_with(&format!("{END}\n")));
        assert_eq!(GUIDANCE.matches(START).count(), 1);
    }

    /// Every code block is a program the checker accepts, so the guidance can't teach something
    /// that stopped working.
    #[test]
    fn the_guidance_checks() {
        let blocks: Vec<&str> =
            GUIDANCE.split("```fennec\n").skip(1).filter_map(|rest| rest.split("```").next()).collect();
        assert_eq!(blocks.len(), 1);
        for block in blocks {
            let module = fennec_syntax::parse(block).unwrap();
            assert!(fennec_check::check(&module).is_ok(), "{block}");
        }
    }
}
