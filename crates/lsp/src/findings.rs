//! The linter's findings and the hints about what may copy or allocate, which the language
//! server shows beside the checker's errors, with the same words `fennec lint` uses. Each is
//! placed in its file once it's found: spans differ from one load of a program to the next, so a
//! quick check of an edit can keep the findings of files it didn't change only in this form.

use fennec_check::Checked;
use fennec_lint::{Lint, Severity};
use fennec_syntax::{Module, Sources, Span};

use crate::json::Json;
use crate::text;

/// A finding, placed in its file.
#[derive(Clone, Debug)]
pub(crate) struct Located {
    pub(crate) path: String,
    /// Where it is in its file's text, in bytes.
    pub(crate) from: usize,
    pub(crate) to: usize,
    pub(crate) range: Json,
    pub(crate) rule: &'static str,
    pub(crate) severity: Severity,
    /// What it says, with its help after it.
    pub(crate) message: String,
    /// What its fix does, in a few words.
    title: String,
    /// Its fix, as each edit's file, range and new text. Empty when it has none.
    fixes: Vec<(String, Json, String)>,
}

/// Where a span is: its file, that file's text, and the span's bytes in it. The standard
/// library's spans are nowhere a person can see.
fn place(sources: &Sources, span: Span) -> Option<(&str, &str, usize, usize)> {
    if span.in_prelude() {
        return None;
    }
    let (path, text, base) = sources.file_at(span)?;
    let local = |at: u32| usize::try_from(at.saturating_sub(base)).unwrap_or(0);
    Some((path, text, local(span.start), local(span.end)))
}

/// Lints placed in their files. One whose place or fix can't be found is left out.
pub(crate) fn located(sources: &Sources, lints: &[Lint]) -> Vec<Located> {
    lints
        .iter()
        .filter_map(|lint| {
            let diagnostic = &lint.diagnostic;
            let (path, text, from, to) = place(sources, diagnostic.span)?;
            let fixes = diagnostic
                .fixes
                .iter()
                .map(|edit| {
                    let (path, text, start, end) = place(sources, edit.span)?;
                    Some((path.to_owned(), text::range(text, start, end), edit.text.clone()))
                })
                .collect::<Option<Vec<_>>>()?;
            let message = match &diagnostic.help {
                Some(help) => format!("{}\nhelp: {help}", lint.message()),
                None => lint.message().to_owned(),
            };
            Some(Located {
                path: path.to_owned(),
                from,
                to,
                range: text::range(text, from, to),
                rule: lint.rule,
                severity: lint.severity,
                message,
                title: diagnostic.help.clone().unwrap_or_else(|| lint.message().to_owned()),
                fixes,
            })
        })
        .collect()
}

/// Everything there is to show beyond mistakes, for a program with none: what the linter
/// finds, then what may copy or allocate, as `fennec lint` shows them.
pub(crate) fn of_clean(module: &Module, sources: &Sources, checked: &Checked) -> Vec<Located> {
    let mut lints = fennec_lint::lint(module, sources);
    lints.extend(fennec_lint::costs(&fennec_ir::lower(module, &checked.signatures_with_types())));
    located(sources, &lints)
}

/// A finding as the protocol's diagnostic. A lint is a warning, since the program still runs.
/// A hint about code that runs on every pass of a loop is information, which editors show
/// plainly, and one about code that runs once is a hint, which they show faintly.
pub(crate) fn protocol(found: &Located) -> Json {
    let severity = match found.severity {
        Severity::Error => 2,
        Severity::Hint if found.message.contains("\nhelp: ") => 3,
        Severity::Hint => 4,
    };
    Json::object([
        ("range", found.range.clone()),
        ("severity", Json::number(severity)),
        ("code", Json::string(found.rule)),
        ("source", Json::string("fennec")),
        ("message", Json::string(found.message.as_str())),
    ])
}

/// A finding's fix as a code action, when it has one.
pub(crate) fn action(found: &Located, uri_of: &dyn Fn(&str) -> String) -> Option<Json> {
    if found.fixes.is_empty() {
        return None;
    }
    let mut changes: Vec<(String, Json)> = Vec::new();
    for (path, range, new_text) in &found.fixes {
        let edit = Json::object([("range", range.clone()), ("newText", Json::string(new_text.as_str()))]);
        let uri = uri_of(path);
        match changes.iter_mut().find(|(changed, _)| *changed == uri) {
            Some((_, Json::Array(edits))) => edits.push(edit),
            _ => changes.push((uri, Json::Array(vec![edit]))),
        }
    }
    Some(Json::object([
        ("title", Json::string(format!("Fix: {}", found.title))),
        ("kind", Json::string("quickfix")),
        ("diagnostics", Json::Array(vec![protocol(found)])),
        ("isPreferred", Json::Bool(true)),
        ("edit", Json::object([("changes", Json::Object(changes))])),
    ]))
}
