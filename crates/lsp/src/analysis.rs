//! Checking the program a document belongs to, with the editor's unsaved text in place of
//! what's on disk, and writing what it finds the way the protocol does.
//!
//! A document in a kit's `src` folder belongs to the whole kit, every file of it, since a
//! change in one file can break another. Any other `.fen` file is a program of its own.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fennec_blake3::Hash;
use fennec_build::{Row, item_span, rows};
use fennec_check::Checked;
use fennec_kit::{Entry, Overlay};
use fennec_syntax::{Diagnostic, Item, Module, Sources, Span};

use crate::findings::{self, Located};
use crate::json::Json;
use crate::text;

/// What checking a program found.
pub(crate) struct Analysis {
    pub(crate) sources: Sources,
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// Problems with no place in a file, like a missing `kit.toml`.
    pub(crate) unplaced: Vec<String>,
    /// The program's files merged into one module, when they all parsed.
    pub(crate) module: Option<Module>,
    /// The types of everything, when the program has no mistakes.
    pub(crate) checked: Option<Checked>,
    /// What the linter finds and what may copy or allocate, when the program has no mistakes.
    pub(crate) findings: Vec<Located>,
    /// Whether this looked at only part of the program. See `edited_bodies`.
    pub(crate) partial: bool,
}

/// The program a file belongs to: its kit's folder when it's in a kit's `src`, or else the
/// file itself.
pub(crate) fn program_of(file: &Path) -> PathBuf {
    let kit = file
        .ancestors()
        .skip(1)
        .find(|folder| folder.join("kit.toml").is_file())
        .filter(|root| file.starts_with(root.join("src")));
    kit.map_or_else(|| file.to_path_buf(), Path::to_path_buf)
}

/// A program that had no mistakes, by each file's text and definitions, to tell what an edit
/// can have changed.
pub(crate) struct Clean {
    program: PathBuf,
    files: HashMap<String, (Hash, Vec<Row>)>,
    /// What there was to show in each file, which still holds for a file that hasn't changed.
    findings: HashMap<String, Vec<Located>>,
}

impl Clean {
    /// What an analysis says about a program, if it had no mistakes.
    pub(crate) fn of(program: &Path, analysis: &Analysis) -> Option<Clean> {
        if analysis.partial || analysis.checked.is_none() || !analysis.diagnostics.is_empty() {
            return None;
        }
        let module = analysis.module.as_ref()?;
        let mut items: HashMap<&str, Vec<&Item>> = HashMap::new();
        for item in &module.items {
            if let Some((path, ..)) = analysis.sources.file_at(item_span(item)) {
                items.entry(path).or_default().push(item);
            }
        }
        let files = analysis.sources.files().map(|(path, text)| {
            let of_file = items.get(path).map_or(&[][..], Vec::as_slice);
            (path.to_owned(), (fennec_blake3::hash(text.as_bytes()), rows(of_file.iter().copied())))
        });
        let mut findings: HashMap<String, Vec<Located>> = HashMap::new();
        for found in &analysis.findings {
            findings.entry(found.path.clone()).or_default().push(found.clone());
        }
        Some(Clean { program: program.to_path_buf(), files: files.collect(), findings })
    }
}

/// The module a document is, if it's in a kit's `src` folder.
fn module_of(program: &Path, file: &Path) -> Option<String> {
    let relative = file.strip_prefix(program.join("src")).ok()?.with_extension("");
    let parts: Vec<String> = relative.iter().map(|part| part.to_string_lossy().into_owned()).collect();
    Some(parts.join("."))
}

/// An analysis of what an edit can have broken, when it only changed the bodies of functions:
/// the open files and what they import. A kit's files only reach each other through imports, and
/// what uses a function only depends on its signature, so the rest still has no mistakes. None
/// if the edit reaches further, or there's something this can't say, which takes all of the
/// program. It knows mistakes only, so anything that asks more of it needs the whole program.
pub(crate) fn edited_bodies(clean: &Clean, program: &Path, overlay: &Overlay) -> Option<Analysis> {
    if clean.program != program {
        return None;
    }
    let modules: Vec<String> = overlay.keys().filter_map(|file| module_of(program, file)).collect();
    let mut loaded = fennec_kit::load_modules_with(program, &text::shown(program), &modules, overlay).ok()?;
    let mut items: HashMap<&str, Vec<&Item>> = HashMap::new();
    for item in &loaded.module.items {
        if let Some((path, ..)) = loaded.sources.file_at(item_span(item)) {
            items.entry(path).or_default().push(item);
        }
    }
    let mut edited = Vec::new();
    for (path, text) in loaded.sources.files() {
        let (old_text, old_rows) = clean.files.get(path)?;
        if fennec_blake3::hash(text.as_bytes()) != *old_text {
            let of_file = items.get(path).map_or(&[][..], Vec::as_slice);
            fennec_build::changed_bodies(old_rows, &rows(of_file.iter().copied()))?;
            edited.push(path.to_owned());
        }
    }
    drop(items);
    let linted = findings::located(&loaded.sources, &fennec_lint::lint(&loaded.module, &loaded.sources));
    let diagnostics = fennec_check::check_to_run(&mut loaded.module).err().unwrap_or_default();
    // A file that hasn't changed keeps what it had. One that has gets the linter's findings
    // afresh, and what may copy waits for the whole program to be checked again.
    let findings = if diagnostics.is_empty() {
        let kept =
            clean.findings.iter().filter(|(path, _)| !edited.contains(path)).flat_map(|(_, found)| found);
        let fresh = linted.into_iter().filter(|found| edited.contains(&found.path));
        kept.cloned().chain(fresh).collect()
    } else {
        Vec::new()
    };
    Some(Analysis {
        sources: loaded.sources,
        diagnostics,
        unplaced: Vec::new(),
        module: None,
        checked: None,
        findings,
        partial: true,
    })
}

/// Loads and checks a program, reading the files in `overlay` from it.
pub(crate) fn analyze(program: &Path, overlay: &Overlay) -> Analysis {
    let shown = text::shown(program);
    match fennec_kit::load_with(program, &shown, Entry::Everything, overlay) {
        Err(problems) => Analysis {
            sources: problems.sources,
            diagnostics: problems.diagnostics,
            unplaced: problems.unplaced,
            module: None,
            checked: None,
            findings: Vec::new(),
            partial: false,
        },
        Ok(loaded) => {
            let (diagnostics, checked) = match fennec_check::check_typed(&loaded.module) {
                Ok(checked) => (Vec::new(), Some(checked)),
                Err(diagnostics) => (diagnostics, None),
            };
            let findings = checked
                .as_ref()
                .map(|checked| findings::of_clean(&loaded.module, &loaded.sources, checked))
                .unwrap_or_default();
            Analysis {
                sources: loaded.sources,
                diagnostics,
                unplaced: Vec::new(),
                module: Some(loaded.module),
                checked,
                findings,
                partial: false,
            }
        }
    }
}

impl Analysis {
    /// The file a span is in, and the span's place in that file's text.
    pub(crate) fn place(&self, span: Span) -> Option<(&str, &str, usize, usize)> {
        if span.in_prelude() {
            return None;
        }
        let (path, text, base) = self.sources.file_at(span)?;
        let local = |at: u32| usize::try_from(at.saturating_sub(base)).unwrap_or(0);
        Some((path, text, local(span.start), local(span.end)))
    }

    /// Where a file's offset is in the program's spans.
    pub(crate) fn span_at(&self, path: &str, offset: usize) -> Option<u32> {
        let (_, base) = self.sources.by_path(path)?;
        base.checked_add(u32::try_from(offset).ok()?)
    }

    /// Every file of the program, by path, with its diagnostics in the protocol's form. Those
    /// with no place in a file, like the standard library's or a missing `kit.toml`, go on
    /// `fallback`, at its start.
    pub(crate) fn diagnostics_by_file(
        &self,
        fallback: &str,
        uri_of: &dyn Fn(&str) -> String,
    ) -> Vec<(String, Vec<Json>)> {
        let mut files: Vec<(String, Vec<Json>)> =
            self.sources.files().map(|(path, _)| (path.to_owned(), Vec::new())).collect();
        // A quick check keeps what files it didn't load had to show.
        for path in self.findings.iter().map(|found| found.path.as_str()).chain([fallback]) {
            if !files.iter().any(|(file, _)| file == path) {
                files.push((path.to_owned(), Vec::new()));
            }
        }
        let mut add = |path: &str, diagnostic: Json| {
            if let Some((_, list)) = files.iter_mut().find(|(file, _)| file == path) {
                list.push(diagnostic);
            }
        };
        let start = text::range("", 0, 0);
        for message in &self.unplaced {
            let message = message.trim_end().trim_start_matches("error: ");
            add(fallback, protocol_diagnostic(start.clone(), message, Vec::new()));
        }
        for diagnostic in &self.diagnostics {
            let (path, range) = match self.place(diagnostic.span) {
                Some((path, text, from, to)) => (path, text::range(text, from, to)),
                None => (fallback, start.clone()),
            };
            let related = self.related(diagnostic, uri_of);
            add(path, protocol_diagnostic(range, &message_of(diagnostic), related));
        }
        for found in &self.findings {
            add(&found.path, findings::protocol(found));
        }
        files
    }

    /// The other place a diagnostic involves, as the protocol's related information.
    fn related(&self, diagnostic: &Diagnostic, uri_of: &dyn Fn(&str) -> String) -> Vec<Json> {
        let Some((path, text, from, to)) = diagnostic.also.and_then(|also| self.place(also)) else {
            return Vec::new();
        };
        let location =
            Json::object([("uri", Json::string(uri_of(path))), ("range", text::range(text, from, to))]);
        vec![Json::object([("location", location), ("message", Json::string("the other one is here"))])]
    }

    /// The fixes for the diagnostics in `path` that touch bytes `from..to`, as code actions.
    pub(crate) fn fixes(
        &self,
        path: &str,
        from: usize,
        to: usize,
        uri_of: &dyn Fn(&str) -> String,
    ) -> Vec<Json> {
        let mut actions = Vec::new();
        for diagnostic in self.diagnostics.iter().filter(|diagnostic| !diagnostic.fixes.is_empty()) {
            let Some((file, text, start, end)) = self.place(diagnostic.span) else { continue };
            if file != path || end < from || start > to {
                continue;
            }
            let mut changes: Vec<(String, Json)> = Vec::new();
            for edit in &diagnostic.fixes {
                let Some((file, text, start, end)) = self.place(edit.span) else { continue };
                let text_edit = Json::object([
                    ("range", text::range(text, start, end)),
                    ("newText", Json::string(&edit.text)),
                ]);
                let uri = uri_of(file);
                match changes.iter_mut().find(|(changed, _)| *changed == uri) {
                    Some((_, Json::Array(edits))) => edits.push(text_edit),
                    _ => changes.push((uri, Json::Array(vec![text_edit]))),
                }
            }
            let title = diagnostic.help.as_deref().unwrap_or(&diagnostic.message);
            let shown =
                protocol_diagnostic(text::range(text, start, end), &message_of(diagnostic), Vec::new());
            actions.push(Json::object([
                ("title", Json::string(format!("Fix: {title}"))),
                ("kind", Json::string("quickfix")),
                ("diagnostics", Json::Array(vec![shown])),
                ("isPreferred", Json::Bool(true)),
                ("edit", Json::object([("changes", Json::Object(changes))])),
            ]));
        }
        let touched =
            self.findings.iter().filter(|found| found.path == path && found.to >= from && found.from <= to);
        actions.extend(touched.filter_map(|found| findings::action(found, uri_of)));
        actions
    }
}

/// A diagnostic's message with its help, which the protocol has no place of its own for.
fn message_of(diagnostic: &Diagnostic) -> String {
    match &diagnostic.help {
        Some(help) => format!("{}\nhelp: {help}", diagnostic.message),
        None => diagnostic.message.clone(),
    }
}

fn protocol_diagnostic(range: Json, message: &str, related: Vec<Json>) -> Json {
    let mut fields = vec![
        ("range".to_owned(), range),
        ("severity".to_owned(), Json::number(1)),
        ("source".to_owned(), Json::string("fennec")),
        ("message".to_owned(), Json::string(message)),
    ];
    if !related.is_empty() {
        fields.push(("relatedInformation".to_owned(), Json::Array(related)));
    }
    Json::Object(fields)
}
