//! `embed("path")`: a file's bytes, read while the program loads. The file is an input to the
//! build rather than something the program reads when it runs, so it takes no `Fs` capability,
//! and from here on the program just holds the bytes.

use std::path::{Component, Path};

use fennec_syntax::{Blob, Diagnostic, Expr, ExprKind, Item, Module, Span, visit};

use crate::{Embedded, Overlay};

/// The file an `embed` is written in.
pub(crate) struct Place<'a> {
    pub(crate) file: &'a Path,
    /// Its path as messages show it.
    pub(crate) shown: &'a str,
    /// The folder of its kit, which `embed` can't reach out of.
    pub(crate) root: Option<&'a Path>,
    pub(crate) overlay: &'a Overlay,
}

/// Replaces each `embed` call in a file with the bytes it names, unless the file has an `embed`
/// of its own.
pub(crate) fn read_files(
    module: &mut Module,
    place: &Place<'_>,
    embedded: &mut Vec<Embedded>,
    errors: &mut Vec<Diagnostic>,
) {
    if has_its_own(module) {
        return;
    }
    visit::exprs_mut(module, &mut |expr| {
        if let Some(path) = called(expr) {
            match path.and_then(|path| place.read(path, expr.span)) {
                Ok(file) => {
                    expr.kind = ExprKind::Bytes(file.data.clone());
                    if !embedded.iter().any(|known| known.path == file.path) {
                        embedded.push(file);
                    }
                }
                Err(diagnostic) => errors.push(diagnostic),
            }
        }
    });
}

/// Whether a file defines or imports a name `embed`.
fn has_its_own(module: &Module) -> bool {
    module.items.iter().any(|item| match item {
        Item::Function(def) => def.name == "embed",
        Item::Import(import) => import.bound().any(|(_, bound)| bound == "embed"),
        _ => false,
    })
}

/// The path an `embed` call gives, if this is one.
fn called(expr: &Expr) -> Option<Result<&str, Diagnostic>> {
    let ExprKind::Call { callee, args } = &expr.kind else { return None };
    if !matches!(&callee.kind, ExprKind::Name(name) if name == "embed") {
        return None;
    }
    Some(match args.as_slice() {
        [arg] if arg.name.is_none() => match &arg.value.kind {
            ExprKind::Str(path) => Ok(path),
            _ => Err(not_written_out(arg.value.span)),
        },
        _ => Err(not_written_out(expr.span)),
    })
}

fn not_written_out(span: Span) -> Diagnostic {
    Diagnostic::new("`embed` takes one path, written out as a string", span).with_help(
        "the file is read when the program is built, before anything has run; to choose a file while it runs, read it with `std.fs`",
    )
}

impl Place<'_> {
    fn read(&self, path: &str, span: Span) -> Result<Embedded, Diagnostic> {
        let written = Path::new(path);
        if written.components().any(|part| matches!(part, Component::RootDir | Component::Prefix(_))) {
            return Err(Diagnostic::new(format!("`embed` can't take the whole path `{path}`"), span)
                .with_help("write the path from the folder this file is in"));
        }
        let beside = self.file.parent().unwrap_or_else(|| Path::new(".")).join(written);
        let real = std::fs::canonicalize(&beside).ok();
        if let (Some(root), Some(real)) = (self.root, &real)
            && !real.starts_with(root)
        {
            return Err(Diagnostic::new(format!("`{path}` is outside this kit's folder"), span)
                .with_help("a kit embeds only files inside its own folder, so it still builds wherever it's shared; move the file into the kit"));
        }
        let data = match self.overlay.get(&beside) {
            Some(text) => text.clone().into_bytes(),
            None => self.read_disk(path, &beside, span)?,
        };
        Ok(Embedded { path: real.unwrap_or(beside), data: Blob::from(data) })
    }

    fn read_disk(&self, path: &str, beside: &Path, span: Span) -> Result<Vec<u8>, Diagnostic> {
        if beside.is_dir() {
            return Err(Diagnostic::new(format!("`{path}` is a folder"), span)
                .with_help("`embed` takes one file; embed each file you need"));
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
