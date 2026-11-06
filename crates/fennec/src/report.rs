//! What `fennec check` and `fennec lint` found, as text for people or, with `--json`, as JSON
//! for agents and tools. Both forms show the same findings: the checker's errors, the linter's,
//! and hints about what may copy or allocate.
//!
//! The JSON is one object: `diagnostics`, each with its `rule` (`check` for the checker's own),
//! `severity` (`error` or `hint`), `file`, `line`, `column`, `end_line`, `end_column` (lines and
//! columns count from 1, columns in characters), `message`, `help` (or null), `also` (another
//! place it involves, or null) and `fixes`, the edits `fennec fix` would make; then how many
//! `errors`, `hints` and `fixable` there are. What stops a program being loaded, like a file that
//! doesn't parse, is found the same way with rule `load`, except that a problem with no place in a
//! file, like a missing `kit.toml`, has null for its `file`, lines and columns.

use fennec_lint::{Lint, Severity};
use fennec_lsp::Json;
use fennec_syntax::{Diagnostic, Edit, Sources, Span};

/// One finding to show.
pub(crate) struct Finding<'d> {
    pub(crate) rule: &'d str,
    pub(crate) severity: Severity,
    /// What it says, without its rule's name in front.
    pub(crate) message: &'d str,
    pub(crate) diagnostic: &'d Diagnostic,
}

impl<'d> Finding<'d> {
    /// Something that stopped the program being loaded.
    fn unloaded(diagnostic: &'d Diagnostic) -> Self {
        Finding { rule: "load", severity: Severity::Error, message: &diagnostic.message, diagnostic }
    }

    /// One of the checker's errors.
    pub(crate) fn checked(diagnostic: &'d Diagnostic) -> Self {
        Finding { rule: "check", severity: Severity::Error, message: &diagnostic.message, diagnostic }
    }

    pub(crate) fn linted(lint: &'d Lint) -> Self {
        Finding {
            rule: lint.rule,
            severity: lint.severity,
            message: lint.message(),
            diagnostic: &lint.diagnostic,
        }
    }
}

/// Where a span starts and ends: its file, then line and column at each end.
fn place(sources: &Sources, span: Span) -> (String, usize, usize, usize, usize) {
    let (file, line, column, _) = sources.locate(span).unwrap_or(("?", 0, 0, ""));
    let (_, end_line, end_column, _) =
        sources.locate(Span { start: span.end, end: span.end }).unwrap_or(("?", line, column, ""));
    (file.to_owned(), line, column, end_line, end_column)
}

fn located(sources: &Sources, span: Span) -> Vec<(&'static str, Json)> {
    let (file, line, column, end_line, end_column) = place(sources, span);
    vec![
        ("file", Json::string(file)),
        ("line", Json::number(line)),
        ("column", Json::number(column)),
        ("end_line", Json::number(end_line)),
        ("end_column", Json::number(end_column)),
    ]
}

fn object(fields: Vec<(&str, Json)>) -> Json {
    Json::Object(fields.into_iter().map(|(key, value)| (key.to_owned(), value)).collect())
}

fn edit(sources: &Sources, edit: &Edit) -> Json {
    let mut fields = located(sources, edit.span);
    fields.push(("text", Json::string(edit.text.clone())));
    object(fields)
}

fn severity(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Hint => "hint",
    }
}

fn finding(sources: &Sources, shown: &Finding<'_>) -> Json {
    let diagnostic = shown.diagnostic;
    let mut fields =
        vec![("rule", Json::string(shown.rule)), ("severity", Json::string(severity(shown.severity)))];
    fields.extend(located(sources, diagnostic.span));
    fields.push(("message", Json::string(shown.message)));
    fields.push(("help", diagnostic.help.as_ref().map_or(Json::Null, |help| Json::string(help.clone()))));
    fields.push(("also", diagnostic.also().map_or(Json::Null, |also| object(located(sources, also)))));
    fields.push(("also_is", diagnostic.also().map_or(Json::Null, |_| Json::string(diagnostic.also_is()))));
    fields.push(("fixes", Json::Array(diagnostic.fixes.iter().map(|fixed| edit(sources, fixed)).collect())));
    // A fix to make by hand, which `fennec fix` never makes.
    let hint = diagnostic.hint.as_ref().map_or(Json::Null, |hint| {
        Json::object([
            ("says", Json::string(hint.says.clone())),
            ("edits", Json::Array(hint.edits.iter().map(|fixed| edit(sources, fixed)).collect())),
        ])
    });
    fields.push(("hint", hint));
    object(fields)
}

/// Every finding as one JSON object, on a line of its own.
pub(crate) fn json(sources: &Sources, found: &[Finding<'_>]) -> String {
    let count = |wanted: Severity| found.iter().filter(|shown| shown.severity == wanted).count();
    let fixable = found.iter().filter(|shown| !shown.diagnostic.fixes.is_empty()).count();
    let report = Json::object([
        ("diagnostics", Json::Array(found.iter().map(|shown| finding(sources, shown)).collect())),
        ("errors", Json::number(count(Severity::Error))),
        ("hints", Json::number(count(Severity::Hint))),
        ("fixable", Json::number(fixable)),
    ]);
    format!("{report}\n")
}

/// A problem with no place in a file, as `Problems` has it rendered, as a finding with rule `load`.
fn unplaced(rendered: &str) -> Json {
    let rendered = rendered.trim_end().trim_start_matches("error: ");
    let (message, help) = rendered.split_once("\n  = help: ").unzip();
    Json::object([
        ("rule", Json::string("load")),
        ("severity", Json::string("error")),
        ("file", Json::Null),
        ("line", Json::Null),
        ("column", Json::Null),
        ("end_line", Json::Null),
        ("end_column", Json::Null),
        ("message", Json::string(message.unwrap_or(rendered))),
        ("help", help.map_or(Json::Null, Json::string)),
        ("also", Json::Null),
        ("also_is", Json::Null),
        ("fixes", Json::Array(Vec::new())),
    ])
}

/// Everything that stopped a program being loaded, as one JSON object.
pub(crate) fn unloaded(problems: &fennec_kit::Problems) -> String {
    let found: Vec<Finding<'_>> = problems.diagnostics.iter().map(Finding::unloaded).collect();
    let mut shown: Vec<Json> = problems.unplaced.iter().map(|rendered| unplaced(rendered)).collect();
    shown.extend(found.iter().map(|finding_of| finding(&problems.sources, finding_of)));
    let fixable = found.iter().filter(|shown| !shown.diagnostic.fixes.is_empty()).count();
    let errors = shown.len();
    let report = Json::object([
        ("diagnostics", Json::Array(shown)),
        ("errors", Json::number(errors)),
        ("hints", Json::number(0)),
        ("fixable", Json::number(fixable)),
    ]);
    format!("{report}\n")
}

/// A hint as a line of text: where it is, what it is, and how often it runs.
pub(crate) fn hint_line(sources: &Sources, shown: &Finding<'_>) -> String {
    let (file, line, column, _, _) = place(sources, shown.diagnostic.span);
    match &shown.diagnostic.help {
        Some(help) => format!("hint: {file}:{line}:{column}: {}: {}; {help}", shown.rule, shown.message),
        None => format!("hint: {file}:{line}:{column}: {}: {}", shown.rule, shown.message),
    }
}
