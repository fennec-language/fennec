//! Fetching one commit of a kit from git, without running anything of it.
//!
//! The commit goes into a bare repository of its own, made with no templates and with hooks
//! pointed at an empty folder, and its files are read out of git's objects. Nothing is checked
//! out, so no filter, hook or attribute of the repository ever runs. Only `kit.toml` and what's
//! under `src` are taken, and a link or a submodule there is refused.
//!
//! What's taken is bounded before any of it is read: git says each file's size, and a kit whose
//! files come to more than the store takes is refused, so a dependency with a file of 20 GB can't
//! take the machine's memory. Two files whose names differ only by case are refused too: git holds
//! both, but a Mac or Windows keeps one, so the same pin would build different code there.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::Files;

/// The files of the kit at `folder` in the repository at `url`, as of `commit`. `work` is a
/// scratch folder of the caller's, which this removes when it's done.
/// `most` is how many bytes its files may come to.
pub(crate) fn fetch(
    url: &str,
    commit: &str,
    folder: Option<&str>,
    work: &Path,
    most: u64,
) -> Result<Files, String> {
    let fetched = fetch_into(url, commit, folder, work, most);
    let _ = std::fs::remove_dir_all(work);
    fetched
}

fn fetch_into(
    url: &str,
    commit: &str,
    folder: Option<&str>,
    work: &Path,
    most: u64,
) -> Result<Files, String> {
    // Paths as `git` takes them, which on Windows isn't the verbatim form `canonicalize` gives.
    let work = crate::plain(work);
    let plain_url = crate::plain_text(url);
    let url = plain_url.as_deref().unwrap_or(url);
    let repository = work.join("repository");
    std::fs::create_dir_all(work.join("no-hooks"))
        .map_err(|err| format!("couldn't make {}: {err}", work.display()))?;
    let git = Git { repository: repository.clone(), hooks: work.join("no-hooks") };
    git.run_bare(&["init", "--quiet", "--bare", "--template="], &repository)?;
    // Most hosts let a client ask for one commit by its hash. One that doesn't, like a plain
    // folder, gives its branches and tags, among which the commit should be.
    if git.run(&["fetch", "--quiet", "--no-tags", "--depth", "1", "--", url, commit]).is_err() {
        git.run(&[
            "fetch",
            "--quiet",
            "--no-tags",
            "--",
            url,
            "+refs/heads/*:refs/fetched/heads/*",
            "+refs/tags/*:refs/fetched/tags/*",
        ])
        .map_err(|err| format!("couldn't fetch {url}: {err}"))?;
    }
    if git.run(&["cat-file", "-e", &format!("{commit}^{{commit}}")]).is_err() {
        return Err(format!("{url} has no commit {commit}"));
    }
    let prefix = folder.map(|folder| format!("{folder}/")).unwrap_or_default();
    // With `-l`, each file's size, so none is read before what they come to is known.
    let listing = git.run(&["ls-tree", "-r", "-l", "-z", "--full-tree", commit])?;
    let mut wanted = Vec::new();
    let mut total: u64 = 0;
    for item in listing.split(|byte| *byte == 0).filter(|item| !item.is_empty()) {
        let item = String::from_utf8_lossy(item);
        let Some((about, path)) = item.split_once('\t') else { continue };
        let Some(inside) = path.strip_prefix(&prefix) else { continue };
        if inside != "kit.toml" && !inside.starts_with("src/") {
            continue;
        }
        let mut words = about.split_whitespace();
        let (Some(mode), Some(kind), Some(object), Some(size)) =
            (words.next(), words.next(), words.next(), words.next())
        else {
            return Err(format!("git listed `{item}`, which isn't a file"));
        };
        match (mode, kind) {
            ("100644" | "100755", "blob") => {
                let size: u64 = size.parse().map_err(|_| format!("git listed `{item}` with no size"))?;
                total = total.saturating_add(size);
                if total > most {
                    return Err(format!(
                        "{url} at {commit} has more under `src` than a kit may, {most} bytes, so none of it is read"
                    ));
                }
                wanted.push((inside.to_owned(), object.to_owned()));
            }
            ("120000", _) => {
                return Err(format!("{url} has a link at {path}, and a kit's files can't be links"));
            }
            _ => return Err(format!("{url} has a submodule at {path}, and a kit can't have one")),
        }
    }
    if !wanted.iter().any(|(path, _)| path == "kit.toml") {
        let at = folder.map_or_else(|| "at its top".to_owned(), |folder| format!("in `{folder}`"));
        return Err(format!("{url} at {commit} has no kit.toml {at}, so there's no kit there"));
    }
    if let Some((one, other)) = crate::same_but_for_case(wanted.iter().map(|(path, _)| path.as_str())) {
        return Err(format!(
            "{url} has both {one} and {other}, which differ only by case: a Mac or Windows keeps one of them, so the kit would build differently there"
        ));
    }
    let objects: Vec<&str> = wanted.iter().map(|(_, object)| object.as_str()).collect();
    let contents = git.read_objects(&objects)?;
    Ok(wanted.into_iter().map(|(path, _)| path).zip(contents).collect())
}

/// `git`, run on one bare repository with nothing of the repository's or the user's that could
/// run a program: no hooks, no file-system monitor, no `ext::` transport, and no prompt waiting
/// for a password.
struct Git {
    repository: PathBuf,
    hooks: PathBuf,
}

impl Git {
    fn command(&self) -> Command {
        let mut command = Command::new("git");
        command
            .arg("-c")
            .arg(format!("core.hooksPath={}", self.hooks.display()))
            .args(["-c", "core.fsmonitor=false", "-c", "protocol.ext.allow=never"])
            // Only the kinds of address a dependency may have, so no other transport's helper runs.
            .args(["-c", "protocol.allow=never", "-c", "protocol.https.allow=always"])
            .args(["-c", "protocol.ssh.allow=always", "-c", "protocol.file.allow=always"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null());
        command
    }

    fn run_bare(&self, args: &[&str], folder: &Path) -> Result<Vec<u8>, String> {
        let mut command = self.command();
        command.args(args).arg(folder);
        finished(&mut command, args)
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        let mut command = self.command();
        command.arg("--git-dir").arg(&self.repository).args(args);
        finished(&mut command, args)
    }

    /// Each object's bytes, in the order asked, through one `git cat-file --batch`.
    fn read_objects(&self, objects: &[&str]) -> Result<Vec<Vec<u8>>, String> {
        let mut command = self.command();
        command
            .arg("--git-dir")
            .arg(&self.repository)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|err| format!("couldn't run git: {err}"))?;
        let asked: String = objects.iter().flat_map(|object| [*object, "\n"]).collect();
        let mut input = child.stdin.take().ok_or("git took no input")?;
        // Written from a thread of its own, so git's answers never fill the pipe while it waits.
        let writer = std::thread::spawn(move || input.write_all(asked.as_bytes()));
        let mut output = Vec::new();
        child
            .stdout
            .take()
            .ok_or("git gave no output")?
            .read_to_end(&mut output)
            .map_err(|err| err.to_string())?;
        let status = child.wait().map_err(|err| err.to_string())?;
        let wrote = writer.join().map_err(|_| "writing to git failed")?;
        if !status.success() || wrote.is_err() {
            return Err("git couldn't read the kit's files".to_owned());
        }
        let mut rest = output.as_slice();
        let mut contents = Vec::with_capacity(objects.len());
        for object in objects {
            let end = rest.iter().position(|byte| *byte == b'\n').ok_or("git's answer ended early")?;
            let header = String::from_utf8_lossy(rest.get(..end).unwrap_or_default()).into_owned();
            let size: usize = match header.split(' ').collect::<Vec<_>>().as_slice() {
                [had, "blob", size] if had == object => {
                    size.parse().map_err(|_| format!("git said `{header}`"))?
                }
                _ => return Err(format!("git said `{header}` for {object}")),
            };
            let bytes = rest.get(end + 1..end + 1 + size).ok_or("git's answer ended early")?;
            contents.push(bytes.to_vec());
            rest = rest.get(end + 2 + size..).unwrap_or_default();
        }
        Ok(contents)
    }
}

fn finished(command: &mut Command, args: &[&str]) -> Result<Vec<u8>, String> {
    let output =
        command.output().map_err(|err| format!("couldn't run git, which fetching a kit needs: {err}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let said = String::from_utf8_lossy(&output.stderr);
        Err(format!("`git {}` failed: {}", args.first().unwrap_or(&""), said.trim()))
    }
}
