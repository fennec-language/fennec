//! Annotations on fields: data written above a field, like `@skip`, that formats and other code
//! read at compile time. An annotation never changes the field, so each is checked here for
//! being one the language has, written the way it reads. A `def` can have one too: `@hot`, for a
//! function that may not allocate or copy, and std's `@intrinsic` functions, which have no body
//! because the engines provide them.

use fennec_syntax::{Annotation, Diagnostic, ExprKind, Field, FnDef};

use crate::checker::Checker;

/// The annotations a field can have, with what each is for.
const KNOWN: [(&str, &str); 2] = [
    ("skip", "`@skip` leaves the field out when a value is saved, and loading fills in its default"),
    ("serialize", "`@serialize(name=\"uid\")` gives the field another name when a value is saved"),
];

impl Checker {
    /// Checks the annotations on every function and method: the program's own can have none,
    /// and std's intrinsics are written without a body or arguments.
    pub(crate) fn check_function_annotations(&mut self) {
        let mut defs: Vec<(crate::program::Held<FnDef>, bool)> =
            self.program.functions.values().map(|entry| (entry.def.clone(), entry.prelude)).collect();
        defs.sort_by_key(|(def, _)| (def.span.start, def.span.end));
        for (def, prelude) in defs {
            let mut seen: Vec<&str> = Vec::new();
            for annotation in &def.annotations {
                if seen.contains(&annotation.name.as_str()) {
                    let message = format!("`@{}` is written twice on `{}`", annotation.name, def.name);
                    self.diagnostics
                        .push(Diagnostic::new(message, annotation.span).with_help("write it once"));
                    continue;
                }
                seen.push(&annotation.name);
                if let Some(problem) = function_problem(&def, annotation, prelude) {
                    self.diagnostics.push(problem);
                }
            }
        }
    }

    /// Checks the annotations on every field of the program's own types.
    /// A type that writes one of `def serialize` and `def deserialize` writes the other, since with
    /// neither they're generated together and they have to agree.
    pub(crate) fn check_saving_pairs(&mut self) {
        let names: Vec<(String, fennec_syntax::Span)> = self
            .program
            .types
            .values()
            .filter(|info| !info.prelude)
            .map(|info| (info.def.name.clone(), info.def.span))
            .collect();
        for (name, span) in names {
            let has = |checker: &Self, method: &str| checker.program.method(&name, method).is_some();
            let (saves, loads) = (has(self, "serialize"), has(self, "deserialize"));
            if saves != loads {
                let (written, missing) =
                    if saves { ("serialize", "deserialize") } else { ("deserialize", "serialize") };
                self.diagnostics.push(
                    Diagnostic::new(format!("`{name}` writes `def {written}` but not `def {missing}`"), span)
                        .with_help(
                            "a type writes both or neither: with neither, both are generated from its fields",
                        ),
                );
            }
        }
    }

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

/// What's wrong with one annotation on a `def`, if anything.
fn function_problem(def: &FnDef, annotation: &Annotation, prelude: bool) -> Option<Diagnostic> {
    let span = annotation.span;
    match annotation.name.as_str() {
        "intrinsic" if !prelude => {
            Some(Diagnostic::new("only the standard library has `@intrinsic` functions", span).with_help(
                "write the function's body: intrinsics are the machine operations std's types are built from",
            ))
        }
        "intrinsic" if !annotation.args.is_empty() => Some(
            Diagnostic::new("`@intrinsic` takes nothing", span)
                .with_help("write it as `@intrinsic`: the function's name says which one it is"),
        ),
        "intrinsic" if !def.body.is_empty() => Some(
            Diagnostic::new(format!("`{}` is `@intrinsic`, so it has no body", def.name), span)
                .with_help("end the signature without `:`, since the engines provide what it does"),
        ),
        "hot" if !annotation.args.is_empty() => Some(
            Diagnostic::new("`@hot` takes nothing", span)
                .with_help("write it as `@hot`: the function and what it calls may not allocate or copy"),
        ),
        "intrinsic" | "hot" => None,
        other => Some(
            Diagnostic::new(format!("`@{other}` isn't something a `def` can have"), span)
                .with_help("a `def` can be `@hot`, and std's machine operations are marked `@intrinsic`"),
        ),
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
