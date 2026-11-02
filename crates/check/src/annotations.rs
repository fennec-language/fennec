//! Annotations on fields: data written above a field, like `@skip`, that formats and other code
//! read at compile time. An annotation never changes the field, so each is checked here for
//! being one the language has, written the way it reads.

use fennec_syntax::{Annotation, Diagnostic, ExprKind, Field};

use crate::checker::Checker;

/// The annotations a field can have, with what each is for.
const KNOWN: [(&str, &str); 2] = [
    ("skip", "`@skip` leaves the field out when a value is saved, and loading fills in its default"),
    ("serialize", "`@serialize(name=\"uid\")` gives the field another name when a value is saved"),
];

impl Checker {
    /// Checks the annotations on every field of the program's own types.
    pub(crate) fn check_annotations(&mut self) {
        let defs: Vec<_> =
            self.program.types.values().filter(|info| !info.prelude).map(|info| info.def.clone()).collect();
        for def in defs {
            for field in &def.fields {
                if let (Some(written), Some(default)) = (&field.ty, &field.default) {
                    let resolved = self.field_type(&def.name, None, &field.name);
                    let name_end = field.span.start + u32::try_from(field.name.len()).unwrap_or(0);
                    let at = (field.name.as_str(), name_end);
                    let program = &self.program;
                    if let Some(problem) =
                        crate::program::default_problem(program, at, (written, &resolved), default)
                    {
                        self.diagnostics.push(problem);
                    }
                }
                let mut seen: Vec<&str> = Vec::new();
                for annotation in &field.annotations {
                    if seen.contains(&annotation.name.as_str()) {
                        let message = format!("`@{}` is written twice on `{}`", annotation.name, field.name);
                        self.diagnostics
                            .push(Diagnostic::new(message, annotation.span).with_help("write it once"));
                        continue;
                    }
                    seen.push(&annotation.name);
                    if let Some(problem) = problem(field, annotation) {
                        self.diagnostics.push(problem);
                    }
                }
            }
        }
    }
}

/// What's wrong with one annotation on a field, if anything.
fn problem(field: &Field, annotation: &Annotation) -> Option<Diagnostic> {
    let span = annotation.span;
    match annotation.name.as_str() {
        "skip" if !annotation.args.is_empty() => {
            Some(Diagnostic::new("`@skip` takes nothing", span).with_help("write it as `@skip`"))
        }
        "skip" if field.default.is_none() => Some(
            Diagnostic::new(format!("`@skip` needs `{}` to have a default", field.name), span)
                .with_help("loading a value fills in a skipped field from its default, so give it one"),
        ),
        "serialize" => {
            let named = match annotation.args.as_slice() {
                [arg] => arg.name.as_deref() == Some("name") && matches!(arg.value.kind, ExprKind::Str(_)),
                _ => false,
            };
            (!named).then(|| {
                Diagnostic::new("`@serialize` takes the field's saved name", span)
                    .with_help(format!("write it like `@serialize(name=\"{}\")`", field.name))
            })
        }
        "skip" => None,
        other => {
            let known: Vec<String> = KNOWN.iter().map(|(name, what)| format!("`@{name}`: {what}")).collect();
            Some(
                Diagnostic::new(format!("there's no annotation called `@{other}`"), span)
                    .with_help(format!("a field can have {}", known.join("; "))),
            )
        }
    }
}
