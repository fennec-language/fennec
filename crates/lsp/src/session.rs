//! One editor's session: the documents it has open, and the answer to each message.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use fennec_kit::Overlay;
use fennec_syntax::Span;

use crate::analysis::{Analysis, Clean, analyze, edited_bodies, program_of};
use crate::completion;
use crate::declaration::{declarations, hover_text};
use crate::definition::{Target, target};
use crate::inlay;
use crate::json::Json;
use crate::references::{import_at, own, references, valid_name};
use crate::signature;
use crate::symbols::outline;
use crate::text;

/// The protocol's error codes.
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_REQUEST: i32 = -32600;
/// A request that could be answered but wasn't, like a rename that isn't safe.
const REQUEST_FAILED: i32 = -32803;

/// What a session keeps between messages.
#[derive(Default)]
pub struct Session {
    /// Each open document's text, by URI.
    documents: BTreeMap<String, String>,
    /// How many times a document has changed, so a cached analysis knows it's out of date.
    changes: u64,
    /// The last program analyzed, with the change count it saw.
    cached: Option<(PathBuf, u64, Analysis)>,
    /// The last program with no mistakes, to tell what an edit since can have changed.
    clean: Option<Clean>,
    /// The URIs each program last published diagnostics for, to clear them when they're gone.
    published: BTreeMap<PathBuf, BTreeSet<String>>,
    /// Whether a program's lints and costs wait for `finish`, after its mistakes are published.
    staged: bool,
    /// The document whose program's lints and costs are still to be published, by its URI and
    /// path, with the program.
    unfinished: Option<(String, PathBuf, PathBuf)>,
    shut_down: bool,
    exited: bool,
}

impl Session {
    /// A session that answers each message in full: what it publishes for a program with no
    /// mistakes holds what the linter finds and what may copy too.
    pub fn new() -> Self {
        Session::default()
    }

    /// A session that publishes a program's mistakes as soon as the checker has found them, and
    /// leaves what the linter finds and what may copy, which take longer, to `finish`, as the
    /// server runs it. On a program of 200,000 lines, that's the difference between about 6
    /// seconds and about 20 before an editor shows anything.
    pub fn staged() -> Self {
        Session { staged: true, ..Session::default() }
    }

    /// Whether the program last checked still has its lints and costs to work out.
    pub fn pending(&self) -> bool {
        self.cached.as_ref().is_some_and(|(_, _, analysis)| analysis.pending)
    }

    /// Works out what the linter finds and what may copy in the program last checked, when that's
    /// still to do, and publishes them with its diagnostics again.
    pub fn finish(&mut self) -> Vec<Json> {
        let unfinished = self.unfinished.take();
        let Some((program, _, analysis)) = self.cached.as_mut() else { return Vec::new() };
        if !analysis.pending {
            return Vec::new();
        }
        analysis.finish(program.is_dir());
        if let Some(clean) = Clean::of(program, analysis) {
            self.clean = Some(clean);
        }
        let program = program.clone();
        match unfinished {
            Some((uri, file, of)) if of == program => self.publish(&uri, &file, program),
            _ => Vec::new(),
        }
    }

    /// Whether the editor has said it's done: the session ends after this.
    pub fn exited(&self) -> bool {
        self.exited
    }

    /// Whether the editor asked to shut down before it said it was done, as it should.
    pub fn shut_down(&self) -> bool {
        self.shut_down
    }

    /// The answer to one message: a response if it's a request, and any notifications, like
    /// new diagnostics after a change.
    pub fn handle(&mut self, message: &Json) -> Vec<Json> {
        let method = message.get("method").and_then(Json::as_str).unwrap_or("");
        // A code action may be a lint's fix, and an inlay hint may be a copy, so these wait for
        // lints and costs, as does `fennec/settled`, which is answered once everything is.
        let mut answers = match method {
            "textDocument/codeAction" | "textDocument/inlayHint" | "fennec/settled" => self.finish(),
            _ => Vec::new(),
        };
        answers.extend(self.answer(message, method));
        answers
    }

    fn answer(&mut self, message: &Json, method: &str) -> Vec<Json> {
        let params = message.get("params").unwrap_or(&Json::Null);
        let Some(id) = message.get("id").cloned() else {
            return self.notification(method, params);
        };
        if self.shut_down {
            return vec![error(id, INVALID_REQUEST, "the server has shut down")];
        }
        let result = match method {
            "initialize" => Some(capabilities()),
            // Answered once everything the last change asked for is published, so whatever
            // sends it knows the server has finished, as the budgets time it.
            "fennec/settled" => Some(Json::Null),
            "shutdown" => {
                self.shut_down = true;
                Some(Json::Null)
            }
            "textDocument/definition" => Some(self.definition(params)),
            "textDocument/hover" => Some(self.hover(params)),
            "textDocument/codeAction" => Some(self.code_actions(params)),
            "textDocument/formatting" => Some(self.formatting(params)),
            "textDocument/references" => Some(self.references(params)),
            "textDocument/documentSymbol" => Some(self.symbols(params)),
            "textDocument/completion" => Some(self.completion(params)),
            "textDocument/signatureHelp" => Some(self.signature_help(params)),
            "textDocument/inlayHint" => Some(self.inlay_hints(params)),
            "textDocument/rename" => match self.rename(params) {
                Ok(edit) => Some(edit),
                Err(message) => return vec![error(id, REQUEST_FAILED, &message)],
            },
            _ => None,
        };
        match result {
            Some(result) => {
                vec![Json::object([("jsonrpc", Json::string("2.0")), ("id", id), ("result", result)])]
            }
            None => vec![error(id, METHOD_NOT_FOUND, &format!("Fennec doesn't answer `{method}`"))],
        }
    }

    fn notification(&mut self, method: &str, params: &Json) -> Vec<Json> {
        let uri = params.at(&["textDocument", "uri"]).and_then(Json::as_str).unwrap_or("").to_owned();
        match method {
            "exit" => {
                self.exited = true;
                Vec::new()
            }
            "textDocument/didOpen" => {
                let text = params.at(&["textDocument", "text"]).and_then(Json::as_str).unwrap_or("");
                self.documents.insert(uri.clone(), text.to_owned());
                self.changed(&uri)
            }
            "textDocument/didChange" => {
                // Documents are synced whole, so the last change holds all of the text.
                let changes = params.get("contentChanges").and_then(Json::as_array).unwrap_or(&[]);
                let Some(text) = changes.last().and_then(|change| change.get("text")).and_then(Json::as_str)
                else {
                    return Vec::new();
                };
                self.documents.insert(uri.clone(), text.to_owned());
                self.edited(&uri)
            }
            "textDocument/didSave" => self.changed(&uri),
            "textDocument/didClose" => {
                self.documents.remove(&uri);
                self.changed(&uri)
            }
            _ => Vec::new(),
        }
    }

    /// What an edit to a document means: only as much checking as it can need.
    fn edited(&mut self, uri: &str) -> Vec<Json> {
        self.changes += 1;
        let Some(file) = text::path_of(uri).map(|path| canonical(&path)) else { return Vec::new() };
        let program = program_of(&file);
        // The lints and costs of the program last checked, when they're still to work out, come
        // first: an edit's quick check starts from them. Left to the server's idle moments, an
        // editor typing right after an open or a save never left one, so each edit checked all of
        // the program again. What finishing publishes, this edit publishes again just after.
        if self.pending() && self.cached.as_ref().is_some_and(|(cached, ..)| *cached == program) {
            let _ = self.finish();
        }
        let quick = self.clean.as_ref().and_then(|clean| edited_bodies(clean, &program, &self.overlay()));
        match quick {
            Some(analysis) => self.cached = Some((program.clone(), self.changes, analysis)),
            None => self.refresh(&program),
        }
        self.publish_checked(uri, &file, program)
    }

    /// Checks again the program a changed document belongs to, and publishes what it finds.
    fn changed(&mut self, uri: &str) -> Vec<Json> {
        self.changes += 1;
        let Some(file) = text::path_of(uri).map(|path| canonical(&path)) else { return Vec::new() };
        let program = program_of(&file);
        self.refresh(&program);
        self.publish_checked(uri, &file, program)
    }

    /// Publishes what the checker found, and keeps what's needed to publish again once the
    /// lints and costs are worked out, when they wait for `finish`.
    fn publish_checked(&mut self, uri: &str, file: &Path, program: PathBuf) -> Vec<Json> {
        self.unfinished = self.pending().then(|| (uri.to_owned(), file.to_path_buf(), program.clone()));
        self.publish(uri, file, program)
    }

    /// Publishes what's in each file of the analysis. A file that no longer has anything gets an
    /// empty list, which clears it.
    fn publish(&mut self, uri: &str, file: &Path, program: PathBuf) -> Vec<Json> {
        let Some((_, _, analysis)) = &self.cached else { return Vec::new() };
        let uri_of = |path: &str| self.uri_of(path);
        let mut lists = analysis.diagnostics_by_file(&text::shown(file), &uri_of);
        // A single file that's closed is no one's concern anymore.
        if program == *file && !self.documents.contains_key(uri) {
            lists = vec![(text::shown(file), Vec::new())];
        }
        let mut published = BTreeSet::new();
        let mut notifications = Vec::new();
        for (path, diagnostics) in lists {
            let uri = self.uri_of(&path);
            if !diagnostics.is_empty() {
                published.insert(uri.clone());
            }
            notifications.push(publish(&uri, diagnostics));
        }
        let before = self.published.insert(program, published.clone()).unwrap_or_default();
        let gone =
            before.difference(&published).filter(|uri| !notifications.iter().any(|sent| sent_to(sent, uri)));
        notifications.extend(gone.map(|uri| publish(uri, Vec::new())).collect::<Vec<_>>());
        notifications
    }

    /// Analyzes a program, unless it's analyzed already and no document has changed since.
    fn refresh(&mut self, program: &Path) {
        let fresh = matches!(
            &self.cached,
            Some((cached, changes, analysis))
                if cached == program && *changes == self.changes && !analysis.partial
        );
        if !fresh {
            let mut analysis = analyze(program, &self.overlay());
            if !self.staged {
                analysis.finish(program.is_dir());
            }
            if let Some(clean) = Clean::of(program, &analysis) {
                self.clean = Some(clean);
            }
            self.cached = Some((program.to_path_buf(), self.changes, analysis));
        }
    }

    /// What to answer a request about a place in `file` from: the quick analysis an edit left,
    /// when it has the file, typed the first time it's needed, or else the whole program's. After
    /// an edit, that's the files it touched and what they import, so a definition or a hover
    /// there doesn't wait for all of a big kit to be checked again.
    fn analysis_of(&mut self, file: &Path) -> Option<&Analysis> {
        let program = program_of(file);
        let path = text::shown(file);
        let quick = matches!(
            &self.cached,
            Some((cached, changes, analysis))
                if *cached == program && *changes == self.changes && analysis.partial
                    && analysis.module.is_some() && analysis.sources.by_path(&path).is_some()
        );
        if !quick {
            return self.analysis(&program);
        }
        let (_, _, analysis) = self.cached.as_mut()?;
        analysis.typed();
        Some(&*analysis)
    }

    /// The analysis of a program, made fresh if it isn't.
    fn analysis(&mut self, program: &Path) -> Option<&Analysis> {
        self.refresh(program);
        self.cached.as_ref().map(|(_, _, analysis)| analysis)
    }

    /// The open documents' text, by the paths loading reads them from.
    fn overlay(&self) -> Overlay {
        self.documents
            .iter()
            .filter_map(|(uri, text)| Some((canonical(&text::path_of(uri)?), text.clone())))
            .collect()
    }

    /// The URI of a path: the open document's own when one is open there, since an editor
    /// may name a file by a path with links in it.
    fn uri_of(&self, path: &str) -> String {
        uri_among(&self.documents, path)
    }

    /// What a request's position refers to, with the analysis of its document's program: all of
    /// it when the request is about the `whole` program, as finding references is.
    fn at_position(&mut self, params: &Json, whole: bool) -> Option<(Target, &Analysis)> {
        let uri = params.at(&["textDocument", "uri"])?.as_str()?;
        let file = canonical(&text::path_of(uri)?);
        let path = text::shown(&file);
        let analysis = if whole { self.analysis(&program_of(&file))? } else { self.analysis_of(&file)? };
        let (text, _) = analysis.sources.by_path(&path)?;
        let offset = text::offset(text, params.get("position")?)?;
        let at = analysis.span_at(&path, offset)?;
        let found =
            target(analysis.module.as_ref()?, &analysis.sources, at).or_else(|| import_at(analysis, at))?;
        Some((found, analysis))
    }

    fn definition(&mut self, params: &Json) -> Json {
        let documents = self.documents.clone();
        let Some((found, analysis)) = self.at_position(params, false) else { return Json::Null };
        let Some(module) = &analysis.module else { return Json::Null };
        let locations: Vec<(String, Json)> =
            declarations(&found, module, &analysis.sources, analysis.checked.as_ref())
                .iter()
                .filter_map(|declaration| {
                    let (file, text, start, end) = if declaration.library {
                        library_place(declaration.name_at)?
                    } else {
                        analysis.place(declaration.name_at)?
                    };
                    Some((file.to_owned(), text::range(text, start, end)))
                })
                .collect();
        let locations: Vec<Json> = locations
            .into_iter()
            .map(|(file, range)| {
                Json::object([("uri", Json::string(uri_among(&documents, &file))), ("range", range)])
            })
            .collect();
        if locations.is_empty() { Json::Null } else { Json::Array(locations) }
    }

    /// Every place that refers to what the name at a position does, as locations. The
    /// declaration is among them unless the request leaves it out.
    fn references(&mut self, params: &Json) -> Json {
        let documents = self.documents.clone();
        let with_declaration =
            !matches!(params.at(&["context", "includeDeclaration"]), Some(Json::Bool(false)));
        let Some((found, analysis)) = self.at_position(params, true) else { return Json::Null };
        let declared: Vec<Span> = match &analysis.module {
            Some(module) => declarations(&found, module, &analysis.sources, analysis.checked.as_ref())
                .iter()
                .map(|declaration| declaration.name_at)
                .collect(),
            None => Vec::new(),
        };
        let locations = references(analysis, &found)
            .into_iter()
            .filter(|span| with_declaration || !declared.contains(span))
            .filter_map(|span| {
                let (file, text, start, end) = analysis.place(span)?;
                let uri = uri_among(&documents, file);
                Some(Json::object([("uri", Json::string(uri)), ("range", text::range(text, start, end))]))
            });
        Json::Array(locations.collect())
    }

    /// Every place that refers to what the name at a position does, renamed, as one edit. Only
    /// a program with no mistakes, whose every member is known, and only its own names.
    fn rename(&mut self, params: &Json) -> Result<Json, String> {
        let documents = self.documents.clone();
        let new_name = params.get("newName").and_then(Json::as_str).unwrap_or("").to_owned();
        if !valid_name(&new_name) {
            return Err(format!("`{new_name}` can't be a name"));
        }
        let Some((found, analysis)) = self.at_position(params, true) else {
            return Err("there's no name here to rename".to_owned());
        };
        if analysis.checked.is_none() {
            return Err("fix the program's mistakes first, so every use of the name is known".to_owned());
        }
        if !own(analysis, &found) {
            return Err(format!("`{}` belongs to the standard library", found.name()));
        }
        let mut changes: Vec<(String, Json)> = Vec::new();
        for span in references(analysis, &found) {
            let Some((file, text, start, end)) = analysis.place(span) else { continue };
            let edit = Json::object([
                ("range", text::range(text, start, end)),
                ("newText", Json::string(new_name.as_str())),
            ]);
            let uri = uri_among(&documents, file);
            match changes.iter_mut().find(|(changed, _)| *changed == uri) {
                Some((_, Json::Array(edits))) => edits.push(edit),
                _ => changes.push((uri, Json::Array(vec![edit]))),
            }
        }
        Ok(Json::object([("changes", Json::Object(changes))]))
    }

    /// A document's outline.
    fn symbols(&mut self, params: &Json) -> Json {
        let Some(file) = params.at(&["textDocument", "uri"]).and_then(Json::as_str).and_then(text::path_of)
        else {
            return Json::Null;
        };
        let file = canonical(&file);
        let path = text::shown(&file);
        let Some(analysis) = self.analysis_of(&file) else { return Json::Null };
        let Some(module) = &analysis.module else { return Json::Null };
        Json::Array(outline(analysis, module, &path))
    }

    /// The text of the document a request is about, with the request's position in it.
    fn document_at(&self, params: &Json) -> Option<(PathBuf, String, usize)> {
        let uri = params.at(&["textDocument", "uri"]).and_then(Json::as_str)?;
        let file = canonical(&text::path_of(uri)?);
        let text = self.documents.get(uri).cloned().or_else(|| std::fs::read_to_string(&file).ok())?;
        let offset = text::offset(&text, params.get("position")?)?;
        Some((file, text, offset))
    }

    /// What can be written at a position.
    fn completion(&mut self, params: &Json) -> Json {
        let Some((file, text, offset)) = self.document_at(params) else { return Json::Null };
        let program = program_of(&file);
        // As an edit is checked: only the edited file and what it imports, typed, when the edit
        // is to bodies. A big program checked whole took seconds for each request.
        let quick = completion::mend(&text, offset).and_then(|mend| {
            let mut overlay = self.overlay();
            overlay.insert(file.clone(), mend.text.clone());
            let mut analysis = edited_bodies(self.clean.as_ref()?, &program, &overlay)?;
            analysis.typed();
            let checked = analysis.checked.as_ref()?;
            let known = completion::Known {
                module: analysis.module.as_ref()?,
                sources: &analysis.sources,
                checked: Some(checked),
            };
            Some(completion::offer(&mend, &text::shown(&file), &known))
        });
        let items =
            quick.unwrap_or_else(|| completion::complete(&program, &file, &text, offset, self.overlay()));
        let (items, more) = completion::without_hooks(items, &text, offset);
        Json::object([("isIncomplete", Json::Bool(more)), ("items", Json::Array(items))])
    }

    /// The signature of the call a position is in.
    fn signature_help(&mut self, params: &Json) -> Json {
        let Some((file, text, offset)) = self.document_at(params) else { return Json::Null };
        signature::help(&program_of(&file), &file, &text, offset, self.overlay())
    }

    /// What an editor writes into a range of a document: types, parameter names and copies.
    fn inlay_hints(&mut self, params: &Json) -> Json {
        let Some(file) = params.at(&["textDocument", "uri"]).and_then(Json::as_str).and_then(text::path_of)
        else {
            return Json::Null;
        };
        let file = canonical(&file);
        let path = text::shown(&file);
        let Some(analysis) = self.analysis_of(&file) else { return Json::Null };
        let Some((text, _)) = analysis.sources.by_path(&path) else { return Json::Null };
        let place = |key: &str| params.at(&["range", key]).and_then(|position| text::offset(text, position));
        let (from, to) = (place("start").unwrap_or(0), place("end").unwrap_or(text.len()));
        Json::Array(inlay::hints(analysis, &path, from, to))
    }

    fn hover(&mut self, params: &Json) -> Json {
        let Some((found, analysis)) = self.at_position(params, false) else { return Json::Null };
        let Some(module) = &analysis.module else { return Json::Null };
        let name = found.name().to_owned();
        let shown: Vec<String> = declarations(&found, module, &analysis.sources, analysis.checked.as_ref())
            .iter()
            .filter_map(|declaration| {
                let sources =
                    if declaration.library { &crate::library::library().sources } else { &analysis.sources };
                hover_text(declaration, &name, sources)
            })
            .collect();
        let shown = if shown.is_empty() {
            // A method written in Rust has no declaration: what the checker knows of it.
            match builtin_signature(&found, analysis.checked.as_ref()) {
                Some(signature) => vec![signature],
                None => return Json::Null,
            }
        } else {
            shown
        };
        let contents = Json::object([
            ("kind", Json::string("markdown")),
            ("value", Json::string(shown.join("\n\n---\n\n"))),
        ]);
        Json::object([("contents", contents)])
    }

    fn code_actions(&mut self, params: &Json) -> Json {
        let Some(uri) = params.at(&["textDocument", "uri"]).and_then(Json::as_str) else {
            return Json::Array(Vec::new());
        };
        let Some(file) = text::path_of(uri).map(|path| canonical(&path)) else {
            return Json::Array(Vec::new());
        };
        let path = text::shown(&file);
        let documents = self.documents.clone();
        let Some(analysis) = self.analysis_of(&file) else { return Json::Array(Vec::new()) };
        let Some((text, _)) = analysis.sources.by_path(&path) else { return Json::Array(Vec::new()) };
        let place = |key: &str| params.at(&["range", key]).and_then(|position| text::offset(text, position));
        let (Some(from), Some(to)) = (place("start"), place("end")) else { return Json::Array(Vec::new()) };
        let uri_of = |path: &str| uri_among(&documents, path);
        Json::Array(analysis.fixes(&path, from, to, &uri_of))
    }

    /// The whole document formatted, as one edit, or no edits when it's formatted already or
    /// doesn't parse.
    fn formatting(&self, params: &Json) -> Json {
        let text = params
            .at(&["textDocument", "uri"])
            .and_then(Json::as_str)
            .and_then(|uri| self.documents.get(uri));
        let Some(text) = text else { return Json::Array(Vec::new()) };
        match fennec_fmt::format(text) {
            Ok(formatted) if formatted != *text => {
                let edit = Json::object([
                    ("range", text::range(text, 0, text.len())),
                    ("newText", Json::string(formatted)),
                ]);
                Json::Array(vec![edit])
            }
            _ => Json::Array(Vec::new()),
        }
    }
}

/// `uri_of`, for when the session itself is borrowed.
/// The library file a span of the library's is in, and the span's place in its text.
fn library_place(span: Span) -> Option<(&'static str, &'static str, usize, usize)> {
    let (path, text, base) = crate::library::library().sources.file_at(span)?;
    let local = |at: u32| usize::try_from(at.saturating_sub(base)).unwrap_or(0);
    Some((path, text, local(span.start), local(span.end)))
}

fn uri_among(documents: &BTreeMap<String, String>, path: &str) -> String {
    let open = documents
        .keys()
        .find(|uri| text::path_of(uri).is_some_and(|open| text::shown(&canonical(&open)) == path));
    open.cloned().unwrap_or_else(|| text::uri_of(Path::new(path)))
}

/// A path with its links resolved, as loading a kit names its files. A file that doesn't
/// exist yet keeps its path, with its folder's links resolved.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        let folder = path.parent().and_then(|folder| std::fs::canonicalize(folder).ok());
        match (folder, path.file_name()) {
            (Some(folder), Some(name)) => folder.join(name),
            _ => path.to_path_buf(),
        }
    })
}

fn sent_to(notification: &Json, uri: &str) -> bool {
    notification.at(&["params", "uri"]).and_then(Json::as_str) == Some(uri)
}

fn publish(uri: &str, diagnostics: Vec<Json>) -> Json {
    Json::object([
        ("jsonrpc", Json::string("2.0")),
        ("method", Json::string("textDocument/publishDiagnostics")),
        ("params", Json::object([("uri", Json::string(uri)), ("diagnostics", Json::Array(diagnostics))])),
    ])
}

fn error(id: Json, code: i32, message: &str) -> Json {
    let error = Json::object([("code", Json::Number(f64::from(code))), ("message", Json::string(message))]);
    Json::object([("jsonrpc", Json::string("2.0")), ("id", id), ("error", error)])
}

/// What the server can do, as `initialize` answers.
fn capabilities() -> Json {
    let sync = Json::object([
        ("openClose", Json::Bool(true)),
        ("change", Json::number(1)),
        ("save", Json::object([("includeText", Json::Bool(false))])),
    ]);
    let capabilities = Json::object([
        ("positionEncoding", Json::string("utf-16")),
        ("textDocumentSync", sync),
        ("definitionProvider", Json::Bool(true)),
        ("hoverProvider", Json::Bool(true)),
        (
            "codeActionProvider",
            Json::object([("codeActionKinds", Json::Array(vec![Json::string("quickfix")]))]),
        ),
        ("documentFormattingProvider", Json::Bool(true)),
        ("referencesProvider", Json::Bool(true)),
        ("renameProvider", Json::Bool(true)),
        ("documentSymbolProvider", Json::Bool(true)),
        ("completionProvider", Json::object([("triggerCharacters", Json::Array(vec![Json::string(".")]))])),
        ("inlayHintProvider", Json::Bool(true)),
        (
            "signatureHelpProvider",
            Json::object([("triggerCharacters", Json::Array(vec![Json::string("("), Json::string(",")]))]),
        ),
    ]);
    let info = Json::object([
        ("name", Json::string("fennec")),
        ("version", Json::string(env!("CARGO_PKG_VERSION"))),
    ]);
    Json::object([("capabilities", capabilities), ("serverInfo", info)])
}

/// The signature of a method written in Rust, of a built-in type or the standard library's, that
/// `target` calls, for a hover, since it has no declaration to show: `def split(self, str) ->
/// list[str]`, and which type it's built into.
fn builtin_signature(target: &Target, checked: Option<&fennec_check::Checked>) -> Option<String> {
    let Target::Member { object, name, function } = target else { return None };
    let ty = checked?.types.body(function)?.expr(*object)?;
    let ty = match ty {
        fennec_check::Type::Optional(inner) => inner,
        other => other,
    };
    let shown = fennec_check::builtin_method_shown(ty, name)?;
    Some(format!("```fennec\n{shown}\n```\n\nbuilt into `{ty}`"))
}
