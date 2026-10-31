//! `fennec dev reference`: writes the language reference from `examples/`, so what it says is
//! what runs. Every example is a test, so nothing in the page can stop working unnoticed.
//!
//! An example says which decisions in `docs/decisions.md` it shows, with a comment like
//! `# decision: Collections are values.` at its top. A decision of the language that no example
//! shows is listed as planned, however far along it is. `--check` fails if the page is out of
//! date or an example names a decision that isn't there.

use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;

const PAGE: &str = "site/src/pages/reference.md";

/// What says a comment names a decision.
const DECISION: &str = "# decision: ";

/// The sections of the decisions that are about the language, rather than its tools or
/// the project around it.
const LANGUAGE: [&str; 14] = [
    "Values and memory",
    "Collections",
    "Layout",
    "Allocation",
    "Determinism",
    "Functions and types",
    "Lifetimes",
    "Standard library",
    "Visibility",
    "Files and kits",
    "Concurrency",
    "Syntax",
    "SIMD and GPU",
    "Native code",
];

const HEADER: &str = "---
layout: ../layouts/Doc.astro
title: Language reference
description: Fennec as it runs today, written from examples that are all tested.
---

# Language reference

<p class=\"lede\">Every program here runs as part of Fennec's tests, and prints what's shown. So this is what the language does today, not what it's meant to do.</p>

> Written by `fennec dev reference` from `examples/`. Change the examples, not this page.
";

/// A decision, by its section and its bold opening words.
struct Decision {
    section: String,
    text: String,
}

/// An example program.
pub(crate) struct Example {
    name: String,
    source: String,
    output: String,
}

impl Example {
    fn decisions(&self) -> impl Iterator<Item = &str> {
        self.source.lines().filter_map(|line| line.strip_prefix(DECISION))
    }

    /// Its source, without the comments that are only for this page.
    fn shown(&self) -> String {
        let kept: Vec<&str> = self.source.lines().filter(|line| !line.starts_with(DECISION)).collect();
        kept.join("\n").trim().to_owned()
    }

    fn write_to(&self, page: &mut String, level: &str) {
        let _ = writeln!(page, "\n{level} `{}`\n", self.name);
        let _ = writeln!(page, "```fennec\n{}\n```", self.shown());
        let output = self.output.trim_end();
        if !output.is_empty() {
            let _ = writeln!(page, "\nIt prints:\n\n```text\n{output}\n```");
        }
    }
}

/// The decisions in the text of `docs/decisions.md`, in order.
fn decisions(text: &str) -> Vec<Decision> {
    let mut section = String::new();
    let mut found = Vec::new();
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            heading.clone_into(&mut section);
        } else if let Some(bold) = line.strip_prefix("**").and_then(|rest| rest.split_once("**")) {
            found.push(Decision { section: section.clone(), text: bold.0.to_owned() });
        }
    }
    found
}

/// The whole page, or what's wrong with the examples.
pub(crate) fn page(text: &str, examples: &[Example]) -> Result<String, Vec<String>> {
    let decisions = decisions(text);
    let stale: Vec<String> = examples
        .iter()
        .flat_map(|example| example.decisions().map(move |named| (&example.name, named)))
        .filter(|(_, named)| !decisions.iter().any(|decision| decision.text == *named))
        .map(|(name, named)| format!("{name} names a decision that isn't in docs/decisions.md: {named}"))
        .collect();
    if !stale.is_empty() {
        return Err(stale);
    }
    let shows = |decision: &Decision| -> Vec<&Example> {
        examples.iter().filter(|example| example.decisions().any(|named| named == decision.text)).collect()
    };
    let mut page = HEADER.to_owned();
    let mut sections: Vec<&str> = Vec::new();
    for decision in &decisions {
        if !sections.contains(&decision.section.as_str()) {
            sections.push(&decision.section);
        }
    }
    for section in &sections {
        let shown: Vec<&Decision> = decisions
            .iter()
            .filter(|decision| decision.section == *section && !shows(decision).is_empty())
            .collect();
        if shown.is_empty() {
            continue;
        }
        let _ = writeln!(page, "\n## {section}");
        for decision in shown {
            let _ = writeln!(page, "\n**{}**", decision.text);
            for example in shows(decision) {
                example.write_to(&mut page, "###");
            }
        }
    }
    let others: Vec<&Example> =
        examples.iter().filter(|example| example.decisions().next().is_none()).collect();
    if !others.is_empty() {
        page.push_str("\n## More examples\n");
        for example in others {
            example.write_to(&mut page, "###");
        }
    }
    page.push_str(
        "\n## Planned\n\nNo example shows these yet, so don't rely on them. Some are partly built, and some aren't \
         started. [The decisions](https://github.com/fennec-language/fennec/blob/main/docs/decisions.md) say why \
         each was made.\n",
    );
    for section in LANGUAGE {
        let planned: Vec<&Decision> = decisions
            .iter()
            .filter(|decision| decision.section == section && shows(decision).is_empty())
            .collect();
        if planned.is_empty() {
            continue;
        }
        let _ = writeln!(page, "\n### {section}\n");
        for decision in planned {
            let _ = writeln!(page, "- {}", decision.text);
        }
    }
    Ok(page)
}

/// Every example in a folder, in name order. Its output is what it's tested to print.
fn examples(folder: &Path) -> std::io::Result<Vec<Example>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "fen") {
            let name = path.file_stem().map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
            let source = std::fs::read_to_string(&path)?;
            let output = std::fs::read_to_string(path.with_extension("out")).unwrap_or_default();
            found.push(Example { name, source, output });
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// Writes the page, or with `check`, says whether it's up to date. Runs from the repository.
pub(crate) fn reference(check: bool) -> ExitCode {
    let written = (|| -> std::io::Result<_> {
        let text = std::fs::read_to_string("docs/decisions.md")?;
        Ok(page(&text, &examples(Path::new("examples"))?))
    })();
    let made = match written {
        Ok(Ok(made)) => made,
        Ok(Err(problems)) => {
            eprintln!("error: {}", problems.join("\nerror: "));
            return ExitCode::FAILURE;
        }
        Err(err) => {
            eprintln!("error: run this from the repository: {err}");
            return ExitCode::FAILURE;
        }
    };
    if check {
        if std::fs::read_to_string(PAGE).is_ok_and(|current| current == made) {
            return ExitCode::SUCCESS;
        }
        eprintln!("error: {PAGE} is out of date; run `fennec dev reference`");
        return ExitCode::FAILURE;
    }
    match std::fs::write(PAGE, made) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: couldn't write {PAGE}: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Example, page};

    const DECISIONS: &str = "## Collections\n\n**Lists are values.** Why.\n\n**Lists are long.** Why.\n\n\
                             ## License\n\n**It's free,** and more.\n";

    fn example(source: &str) -> Example {
        Example { name: "lists".to_owned(), source: source.to_owned(), output: "3\n".to_owned() }
    }

    #[test]
    fn an_example_shows_the_decisions_it_names_and_the_rest_are_planned() {
        let source = "# decision: Lists are values.\ndef main(stdout: Stdout):\n    stdout.print(3)\n";
        let made = page(DECISIONS, &[example(source)]).unwrap();
        let (shown, planned) = made.split_once("## Planned").unwrap();
        assert!(shown.contains("## Collections\n\n**Lists are values.**\n\n### `lists`"), "{made}");
        assert!(shown.contains("It prints:\n\n```text\n3\n```"), "{made}");
        assert!(!shown.contains("# decision:"), "{made}");
        assert!(planned.contains("### Collections\n\n- Lists are long.\n"), "{made}");
        assert!(!planned.contains("Lists are values"), "{made}");
        assert!(!made.contains("It's free"), "only the language's decisions are planned: {made}");
    }

    #[test]
    fn an_example_that_names_no_decision_is_still_shown() {
        let made = page(DECISIONS, &[example("x = 1\n")]).unwrap();
        assert!(made.contains("## More examples\n\n### `lists`"), "{made}");
    }

    #[test]
    fn naming_a_decision_that_isnt_there_is_an_error() {
        let problems = page(DECISIONS, &[example("# decision: Lists are short.\nx = 1\n")]).unwrap_err();
        assert_eq!(problems, ["lists names a decision that isn't in docs/decisions.md: Lists are short."]);
    }
}
