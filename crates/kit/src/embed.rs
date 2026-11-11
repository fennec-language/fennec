//! `embed_file("path")`: a file's bytes, read while the program loads. The file is an input to the
//! build rather than something the program reads when it runs, so it takes no `Fs` capability,
//! and from here on the program just holds the bytes.

use std::path::{Component, Path, PathBuf};

use fennec_syntax::{Blob, Diagnostic, Expr, ExprKind, Item, Module, Span, visit};

use crate::{Embedded, Overlay};

/// What a program calls to embed a file.
pub(crate) const NAME: &str = "embed_file";

/// What a program calls to embed a folder's files.
pub(crate) const FOLDER: &str = "embed_folder";

/// The file an `embed_file` is written in.
pub(crate) struct Place<'a> {
    pub(crate) file: &'a Path,
    /// Its path as messages show it.
    pub(crate) shown: &'a str,
    /// The folder of its kit, which `embed_file` can't reach out of.
    pub(crate) root: Option<&'a Path>,
    pub(crate) overlay: &'a Overlay,
}

/// Replaces each `embed_file` call in a file with the bytes it names, unless the file has an `embed_file`
/// of its own.
pub(crate) fn read_files(
    module: &mut Module,
    place: &Place<'_>,
    embedded: &mut Vec<Embedded>,
    errors: &mut Vec<Diagnostic>,
) {
    let (files, folders) = (!has_its_own(module, NAME), !has_its_own(module, FOLDER));
    let note = |read: Embedded, embedded: &mut Vec<Embedded>| {
        if !embedded.iter().any(|known| known.path == read.path) {
            embedded.push(read);
        }
    };
    visit::exprs_mut(module, &mut |expr| {
        if let Some(path) = called(expr, NAME).filter(|_| files) {
            match path.and_then(|path| place.read(path, expr.span)) {
                Ok(file) => {
                    expr.kind = ExprKind::Bytes(file.data.clone());
                    note(file, embedded);
                }
                Err(diagnostic) => errors.push(diagnostic),
            }
        } else if let Some(path) = called(expr, FOLDER).filter(|_| folders) {
            match path.and_then(|path| place.read_folder(path, expr.span)) {
                Ok(Found(held, read)) => {
                    expr.kind = ExprKind::Folder(held);
                    for each in read {
                        note(each, embedded);
                    }
                }
                Err(diagnostic) => errors.push(diagnostic),
            }
        }
    });
}

/// Whether a file defines or imports a function called `name`.
fn has_its_own(module: &Module, name: &str) -> bool {
    module.items.iter().any(|item| match item {
        Item::Function(def) => def.name == name,
        Item::Import(import) => import.bound().any(|(_, bound)| bound == name),
        _ => false,
    })
}

/// The path a call to `called` gives, `embed_file` or `embed_folder`, if this is one.
fn called<'e>(expr: &'e Expr, called: &str) -> Option<Result<&'e str, Diagnostic>> {
    let ExprKind::Call { callee, args } = &expr.kind else { return None };
    if !matches!(&callee.kind, ExprKind::Name(name) if name == called) {
        return None;
    }
    Some(match args.as_slice() {
        [arg] if arg.name.is_none() => match &arg.value.kind {
            ExprKind::Str(path) => Ok(path),
            _ => Err(not_written_out(called, arg.value.span)),
        },
        _ => Err(not_written_out(called, expr.span)),
    })
}

fn not_written_out(called: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{called}` takes one path, written out as a string"), span).with_help(
        "what it names is read when the program is built, before anything has run; to choose a file while it runs, read it with `std.fs`",
    )
}

/// That `path` reaches outside the folder embedding stops at.
fn outside(path: &str, kit: bool, span: Span) -> Diagnostic {
    let (said, help) = if kit {
        (
            "this kit's folder",
            "a kit embeds only files inside its own folder, so it still builds wherever it's shared; move the file into the kit",
        )
    } else {
        (
            "this file's folder",
            "a file with no kit embeds only files in its own folder or under it, so it still builds wherever it's copied; move the file there",
        )
    };
    Diagnostic::new(format!("`{path}` is outside {said}"), span).with_help(help)
}

impl Place<'_> {
    fn read(&self, path: &str, span: Span) -> Result<Embedded, Diagnostic> {
        let written = Path::new(path);
        if written.components().any(|part| matches!(part, Component::RootDir | Component::Prefix(_))) {
            return Err(Diagnostic::new(format!("`embed_file` can't take the whole path `{path}`"), span)
                .with_help("write the path from the folder this file is in"));
        }
        let folder = self.file.parent().unwrap_or_else(|| Path::new("."));
        let beside = folder.join(written);
        let real = std::fs::canonicalize(&beside).ok();
        let (edge, kit) = self.edge(folder);
        if let Some(real) = &real
            && !real.starts_with(&edge)
        {
            return Err(outside(path, kit, span));
        }
        let data = match self.overlay.get(&beside) {
            Some(text) => text.clone().into_bytes(),
            None => self.read_disk(path, &beside, span)?,
        };
        Ok(Embedded { path: real.unwrap_or(beside), data: Blob::from(data) })
    }

    /// The folder nothing embedded can be outside of, as it really is on disk: the kit's, which
    /// holds the nearest `kit.toml`, or for a file with no kit, its own folder. Whether it's a kit's
    /// comes with it.
    fn edge(&self, folder: &Path) -> (PathBuf, bool) {
        let real = |at: &Path| std::fs::canonicalize(at).unwrap_or_else(|_| at.to_path_buf());
        if let Some(root) = self.root {
            return (real(root), true);
        }
        let own = real(folder);
        match own.ancestors().find(|at| at.join("kit.toml").is_file()) {
            Some(kit) => (kit.to_path_buf(), true),
            None => (own, false),
        }
    }

    /// The files under a folder, by their paths from it, sorted, with what was read to make
    /// them: each file, and each folder's list of names, so a file added or taken away is seen.
    /// Hidden files and folders are left out.
    fn read_folder(&self, path: &str, span: Span) -> Result<Found, Diagnostic> {
        let written = Path::new(path);
        if written.components().any(|part| matches!(part, Component::RootDir | Component::Prefix(_))) {
            return Err(Diagnostic::new(format!("`{FOLDER}` can't take the whole path `{path}`"), span)
                .with_help("write the path from the folder this file is in"));
        }
        let folder = self.file.parent().unwrap_or_else(|| Path::new("."));
        let beside = folder.join(written);
        let Ok(real) = std::fs::canonicalize(&beside) else {
            return Err(Diagnostic::new(format!("there's no folder `{path}` to embed"), span)
                .with_help("the path starts from the folder this file is in"));
        };
        let (edge, kit) = self.edge(folder);
        if !real.starts_with(&edge) {
            return Err(outside(path, kit, span));
        }
        if !real.is_dir() {
            return Err(Diagnostic::new(format!("`{path}` is a file"), span)
                .with_help("`embed_folder` takes a folder; `embed_file` takes one file"));
        }
        let mut found = Found::default();
        let walk = Walk { edge: &edge, kit, span, overlay: self.overlay, seen: Default::default() };
        walk.folder(&real, &beside, "", path, &mut found)?;
        found.0.sort_by(|first, second| first.0.cmp(&second.0));
        Ok(found)
    }

    fn read_disk(&self, path: &str, beside: &Path, span: Span) -> Result<Vec<u8>, Diagnostic> {
        if beside.is_dir() {
            return Err(Diagnostic::new(format!("`{path}` is a folder"), span)
                .with_help("`embed_file` takes one file; embed each file you need"));
        }
        std::fs::read(beside).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                let folder = self.shown.rsplit_once('/').map_or("", |(folder, _)| folder);
                let shown = if folder.is_empty() { path.to_owned() } else { format!("{folder}/{path}") };
                Diagnostic::new(format!("there's no file `{path}` to embed"), span).with_help(format!(
                    "the path starts from the folder this file is in, so it would be {shown}"
                ))
            } else {
                Diagnostic::new(format!("couldn't read `{path}` to embed: {err}"), span)
            }
        })
    }
}

/// A folder's files, by their paths from it, and what was read to make them.
#[derive(Default)]
pub(crate) struct Found(Vec<(String, Blob)>, Vec<Embedded>);

/// Going through a folder `embed_folder` names.
struct Walk<'a> {
    edge: &'a Path,
    kit: bool,
    span: Span,
    overlay: &'a Overlay,
    /// The real folders read so far. A link can lead back to one, as `assets/loop -> .` does, and
    /// reading it again would read it again inside that, without end.
    seen: std::cell::RefCell<std::collections::HashSet<PathBuf>>,
}

impl Walk<'_> {
    /// Reads `real`, the folder `beside` really is, whose files' paths start with `prefix`.
    /// `written` is how the call names it, for messages.
    fn folder(
        &self,
        real: &Path,
        beside: &Path,
        prefix: &str,
        written: &str,
        found: &mut Found,
    ) -> Result<(), Diagnostic> {
        let unreadable = |err: std::io::Error| {
            Diagnostic::new(format!("couldn't read `{written}` to embed: {err}"), self.span)
        };
        if !self.seen.borrow_mut().insert(real.to_path_buf()) {
            return Err(Diagnostic::new(
                format!("`{written}` is a folder this embeds already, reached again through a link"),
                self.span,
            )
            .with_help("a link back to a folder would have it read again inside itself without end; take the link out"));
        }
        let mut names: Vec<String> = std::fs::read_dir(real)
            .map_err(unreadable)?
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| !name.starts_with('.'))
            .collect();
        names.sort();
        found.1.push(Embedded { path: real.to_path_buf(), data: Blob::from(names.join("\n").into_bytes()) });
        for name in names {
            let (inner, shown) = (beside.join(&name), format!("{prefix}{name}"));
            let reached = format!("{written}/{name}");
            let real = std::fs::canonicalize(&inner).map_err(unreadable)?;
            if !real.starts_with(self.edge) {
                return Err(outside(&reached, self.kit, self.span));
            }
            if real.is_dir() {
                self.folder(&real, &inner, &format!("{shown}/"), &reached, found)?;
                continue;
            }
            let data = match self.overlay.get(&inner) {
                Some(text) => text.clone().into_bytes(),
                None => std::fs::read(&real).map_err(unreadable)?,
            };
            let data = Blob::from(data);
            found.0.push((shown, data.clone()));
            found.1.push(Embedded { path: real, data });
        }
        Ok(())
    }
}
