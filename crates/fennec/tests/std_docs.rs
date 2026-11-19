//! The standard library's reference, held to what it says. Every example in std's doc comments
//! runs, and gives what it states: each code fence is a test written as a program writes its
//! code, importing what it uses, and given the fake capabilities it names, as any test is. A
//! result is stated by an `assert` in the code, or by an `output` fence right after it, holding
//! what the code prints. A `fennec check` fence is only checked, for code that needs what a test
//! can't have, like a database server. And every public function, type and method std declares
//! in Fennec has an example in its doc comment, but those in `fixtures/std_without_examples.txt`,
//! a list that may only shrink.

use std::collections::BTreeSet;
use std::path::Path;

use crate::executables::{FENNEC_DEADLINE, Within as _, at_once, fennec_command};

/// Where the list of public items with no example yet is.
const WITHOUT_EXAMPLES: &str = "tests/fixtures/std_without_examples.txt";

/// What the list says of itself.
const LIST_HEADER: &str = "# Public items of the standard library with no example in their doc comment yet, one a line,\n# as `file: name`. The list only shrinks: `std_docs.rs` fails on an item here that has one.\n";

/// The capabilities an example can name, and their types: what a test can be given.
const CAPABILITIES: [(&str, &str); 9] = [
    ("stdout", "Stdout"),
    ("stderr", "Stderr"),
    ("fs", "Fs"),
    ("net", "Net"),
    ("clock", "Clock"),
    ("env", "Env"),
    ("process", "Process"),
    ("entropy", "Entropy"),
    ("signals", "Signals"),
];

/// A line of a doc comment: its line in its file, counting from 1, and its text after `##`.
struct DocLine {
    line: usize,
    text: String,
}

/// An example in a doc comment: its file, like `time`, the line its fence opens on, its code,
/// what it prints, when an `output` fence follows it, and whether it's only checked.
struct Example {
    file: &'static str,
    line: usize,
    code: Vec<String>,
    output: Option<Vec<String>>,
    checked: bool,
}

/// The doc comment ending on the line before `line` (counting from 1), passing over annotations
/// like `@intrinsic` between them.
fn doc_above(lines: &[&str], line: usize) -> Vec<DocLine> {
    let mut at = line.saturating_sub(1);
    while at > 0 && lines.get(at - 1).is_some_and(|text| text.trim_start().starts_with('@')) {
        at -= 1;
    }
    let mut doc = Vec::new();
    while at > 0 {
        let Some(text) = lines.get(at - 1).and_then(|text| doc_text(text)) else { break };
        doc.push(DocLine { line: at, text });
        at -= 1;
    }
    doc.reverse();
    doc
}

/// The text of a `##` line after its marker and one space, or `None` for any other line.
fn doc_text(line: &str) -> Option<String> {
    let after = line.trim_start().strip_prefix("##")?;
    if after.starts_with('#') {
        return None;
    }
    Some(after.strip_prefix(' ').unwrap_or(after).trim_end().to_owned())
}

/// Every doc comment in a file: each run of `##` lines.
fn doc_comments(source: &str) -> Vec<Vec<DocLine>> {
    let mut comments: Vec<Vec<DocLine>> = Vec::new();
    let mut open = false;
    for (at, line) in source.lines().enumerate() {
        match doc_text(line) {
            Some(text) => {
                let doc = DocLine { line: at + 1, text };
                match comments.last_mut() {
                    Some(comment) if open => comment.push(doc),
                    _ => comments.push(vec![doc]),
                }
                open = true;
            }
            None => open = false,
        }
    }
    comments
}

/// What a line opening a fence says the fence holds: `None` for a line that doesn't open one.
fn fence_language(line: &DocLine) -> Option<&str> {
    line.text.trim().strip_prefix("```").map(str::trim)
}

/// The examples in one doc comment. A fence with no language, `fennec` or `fennec check` is one,
/// an `output` fence right after one that runs is what it prints, and a fence in another
/// language is only text.
fn examples_in(file: &'static str, comment: &[DocLine]) -> Result<Vec<Example>, String> {
    let mut examples: Vec<Example> = Vec::new();
    let mut lines = comment.iter();
    // Whether the fence before ended on the line before, so an `output` fence can follow it.
    let mut after_code = false;
    while let Some(open) = lines.next() {
        let Some(language) = fence_language(open) else {
            after_code = false;
            continue;
        };
        let language = language.to_owned();
        let mut body = Vec::new();
        let mut closed = false;
        for line in lines.by_ref() {
            if line.text.trim() == "```" {
                closed = true;
                break;
            }
            body.push(line.text.clone());
        }
        if !closed {
            return Err(format!("{file}.fen:{}: this fence is never closed", open.line));
        }
        match language.as_str() {
            "" | "fennec" | "fennec check" => {
                let checked = language == "fennec check";
                if !checked && !places(&body).contains(&Place::Statement) {
                    return Err(format!(
                        "{file}.fen:{}: this example runs nothing: call what it declares, and state what that gives",
                        open.line
                    ));
                }
                examples.push(Example { file, line: open.line, code: body, output: None, checked });
                after_code = !checked;
                continue;
            }
            "output" => {
                let Some(example) = examples.last_mut().filter(|_| after_code) else {
                    return Err(format!("{file}.fen:{}: an output fence has to follow code", open.line));
                };
                // What it printed is checked once it's run to its end, which a `return` would skip.
                if example.code.iter().any(|line| line.trim_start().starts_with("return")) {
                    return Err(format!(
                        "{file}.fen:{}: an example with an output fence can't `return`, which would skip checking it",
                        example.line
                    ));
                }
                example.output = Some(body);
            }
            _ => {}
        }
        after_code = false;
    }
    Ok(examples)
}

/// Every example in std's doc comments, by file, but `std.intrinsics`', which only std imports.
fn every_example() -> Result<Vec<(&'static str, Vec<Example>)>, String> {
    let mut found = Vec::new();
    for (file, source) in fennec_syntax::prelude_files() {
        if file == "intrinsics" {
            continue;
        }
        let mut examples = Vec::new();
        for comment in doc_comments(source) {
            examples.extend(examples_in(file, &comment)?);
        }
        found.push((file, examples));
    }
    Ok(found)
}

/// Where an example's line goes in its test file: an import at the top, a declaration beside the
/// tests, or a statement in the test's body.
#[derive(Clone, Copy, PartialEq)]
enum Place {
    Import,
    Declaration,
    Statement,
}

/// Where each of an example's lines goes. A line indented, or blank, goes with the one before.
fn places(code: &[String]) -> Vec<Place> {
    let mut place = Place::Statement;
    code.iter()
        .map(|line| {
            if line.starts_with(char::is_whitespace) || line.is_empty() {
                return place;
            }
            let first = line.split(|c: char| !c.is_alphanumeric() && c != '_').next().unwrap_or("");
            place = match first {
                "from" | "import" => Place::Import,
                "def" | "struct" | "enum" | "error" | "shared" | "interface" | "extend" | "export" => {
                    Place::Declaration
                }
                _ if line.starts_with('@') => Place::Declaration,
                _ => Place::Statement,
            };
            place
        })
        .collect()
}

/// The capabilities the statements name, in `CAPABILITIES`' order: a name standing alone, not a
/// field or a method after a dot.
fn named_capabilities(statements: &[&String]) -> Vec<(&'static str, &'static str)> {
    CAPABILITIES
        .into_iter()
        .filter(|(name, _)| {
            statements.iter().any(|line| {
                line.match_indices(name).any(|(at, _)| {
                    let before = line.get(..at).and_then(|text| text.chars().next_back());
                    let after = line.get(at + name.len()..).and_then(|text| text.chars().next());
                    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                    !word(before) && before != Some('.') && !word(after)
                })
            })
        })
        .collect()
}

/// Text written as a Fennec string literal.
fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let code = u32::from(c);
                out.push_str("\\u");
                out.extend(
                    [12, 8, 4, 0].map(|shift| char::from_digit((code >> shift) & 0xf, 16).unwrap_or('0')),
                );
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One file's examples as a test file, with the line of the std file each of its lines came
/// from, 0 for one written here.
fn test_file(examples: &[&Example]) -> (String, Vec<usize>) {
    let mut imports: Vec<(String, usize)> = Vec::new();
    let mut declarations: Vec<(String, usize)> = Vec::new();
    let mut tests: Vec<(String, usize)> = Vec::new();
    for example in examples {
        let placed = places(&example.code);
        let lines = example.code.iter().zip(placed).zip(example.line + 1..);
        let statements: Vec<&String> = lines
            .clone()
            .filter(|((_, place), _)| *place == Place::Statement)
            .map(|((line, _), _)| line)
            .collect();
        let mut capabilities = named_capabilities(&statements);
        if example.output.is_some() && !capabilities.iter().any(|(name, _)| *name == "stdout") {
            capabilities.insert(0, ("stdout", "Stdout"));
        }
        if statements.is_empty() {
            // Only a checked example can be declarations alone, like a whole program's `main`.
            for ((line, place), at) in lines {
                if place == Place::Import {
                    imports.push((line.clone(), at));
                } else if place == Place::Declaration {
                    declarations.push((line.clone(), at));
                }
            }
            continue;
        }
        let given: Vec<String> = capabilities.iter().map(|(name, ty)| format!("{name}: {ty}")).collect();
        tests.push((
            format!("test \"{}.fen:{}\"({}):", example.file, example.line, given.join(", ")),
            example.line,
        ));
        for ((line, place), at) in lines {
            match place {
                Place::Import => imports.push((line.clone(), at)),
                Place::Declaration => declarations.push((line.clone(), at)),
                Place::Statement => tests.push((format!("    {line}"), at)),
            }
        }
        if let Some(output) = &example.output {
            let printed: String = output.iter().flat_map(|line| [line.as_str(), "\n"]).collect();
            tests.push((format!("    assert stdout.captured() == {}", quoted(&printed)), example.line));
        }
        tests.push((String::new(), 0));
    }
    let mut text = String::new();
    let mut from = Vec::new();
    let imports = merged(imports);
    for (line, at) in imports.into_iter().chain(declarations).chain([(String::new(), 0)]).chain(tests) {
        text.push_str(&line);
        text.push('\n');
        from.push(at);
    }
    (text, from)
}

/// The examples' imports as one file's: each module imported once, with every name any example
/// takes from it, in the order they first come, since a name imported on two lines is a mistake.
/// Each line is shown at the first import it holds.
fn merged(imports: Vec<(String, usize)>) -> Vec<(String, usize)> {
    let mut modules: Vec<(String, Vec<String>, usize)> = Vec::new();
    let mut others: Vec<(String, usize)> = Vec::new();
    for (line, at) in imports {
        let Some((module, names)) = line.strip_prefix("from ").and_then(|rest| rest.split_once(" import "))
        else {
            if !others.iter().any(|(other, _)| *other == line) {
                others.push((line, at));
            }
            continue;
        };
        let names = names.split(',').map(|name| name.trim().to_owned());
        match modules.iter_mut().find(|(seen, ..)| seen == module) {
            Some((_, taken, _)) => {
                for name in names {
                    if !taken.contains(&name) {
                        taken.push(name);
                    }
                }
            }
            None => modules.push((module.to_owned(), names.collect(), at)),
        }
    }
    let mut lines: Vec<(String, usize)> = modules
        .into_iter()
        .map(|(module, names, at)| (format!("from {module} import {}", names.join(", ")), at))
        .collect();
    lines.extend(others);
    lines
}

/// What `fennec test` said, with each place in the test file at `path` shown as the line of the
/// std file `file` it came from.
fn shown_in_std(said: &str, path: &str, file: &str, from: &[usize]) -> String {
    let mut shown = String::new();
    let mut rest = said;
    while let Some((before, after)) = rest.split_once(path) {
        shown.push_str(before);
        rest = after;
        let digits: String =
            rest.strip_prefix(':').unwrap_or("").chars().take_while(char::is_ascii_digit).collect();
        let line = digits.parse::<usize>().ok().and_then(|line| from.get(line.checked_sub(1)?)).copied();
        match line {
            Some(line) if line > 0 => {
                shown.push_str("crates/syntax/src/stdlib/");
                shown.push_str(file);
                shown.push_str(".fen:");
                shown.push_str(&line.to_string());
                rest = rest.get(1 + digits.len()..).unwrap_or("");
            }
            _ => shown.push_str(path),
        }
    }
    shown.push_str(rest);
    shown
}

/// Runs one file's examples as tests in `dir`, or only checks them, those `checked` says, each in
/// a file of its own, since each may be a whole program with its own `main`. What `fennec` said
/// when one failed, or `None`.
fn run_examples(
    dir: &Path,
    file: &str,
    examples: &[&Example],
    checked: bool,
) -> std::io::Result<Option<String>> {
    if examples.is_empty() {
        return Ok(None);
    }
    let (text, from) = test_file(examples);
    let name = match examples.first() {
        Some(example) if checked => format!("{file}_checked_{}.fen", example.line),
        _ => format!("{file}_examples.fen"),
    };
    let path = dir.join(name);
    std::fs::write(&path, text)?;
    let mut command = fennec_command();
    if checked {
        command.arg("check");
    } else {
        command.arg("test").arg("--all");
    }
    let ran = command.arg(&path).env("FENNEC_HOME", dir.join("home")).output_before(FENNEC_DEADLINE)?;
    if ran.status.success() {
        return Ok(None);
    }
    let said = format!("{}{}", String::from_utf8_lossy(&ran.stdout), String::from_utf8_lossy(&ran.stderr));
    Ok(Some(shown_in_std(&said, &path.to_string_lossy(), file, &from)))
}

/// Every example in std's doc comments runs, as a program would write it, and gives what it says.
#[test]
fn every_example_in_the_standard_librarys_docs_runs_and_holds() {
    if !crate::llvm::ready() {
        return;
    }
    let files: Vec<(&str, Vec<Example>)> =
        every_example().unwrap().into_iter().filter(|(_, examples)| !examples.is_empty()).collect();
    let count: usize = files.iter().map(|(_, examples)| examples.len()).sum();
    assert!(count >= 50, "only {count} examples were found in std's doc comments");
    let dir =
        fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-std-docs-{}", std::process::id())));
    let failed = at_once(&files, |worker, (file, examples)| {
        let dir = dir.join(format!("worker-{worker}"));
        std::fs::create_dir_all(&dir)?;
        let (checked, run): (Vec<&Example>, Vec<&Example>) =
            examples.iter().partition(|example| example.checked);
        let mut said = vec![run_examples(&dir, file, &run, false)?];
        for example in checked {
            said.push(run_examples(&dir, file, &[example], true)?);
        }
        Ok(said)
    })
    .unwrap();
    let failed: Vec<String> = failed.into_iter().flatten().flatten().collect();
    assert!(failed.is_empty(), "std's doc examples that don't hold:\n{}", failed.join("\n"));
}

/// Each public item std declares in Fennec, as `file: Name` or `file: Type.method`, with whether
/// its doc comment has an example: an exported function, type or constant, each method not
/// starting with `_` of an exported type or interface, and each method a file adds with `extend`.
fn public_items() -> Result<Vec<(String, bool)>, fennec_syntax::Diagnostic> {
    let mut items = Vec::new();
    let files: Vec<(&str, &str)> = fennec_syntax::prelude_files().collect();
    let modules = fennec_syntax::prelude_modules()?;
    for (module, (file, source)) in modules.iter().zip(files) {
        if file == "intrinsics" {
            continue;
        }
        let lines: Vec<&str> = source.lines().collect();
        let line_of = |span: fennec_syntax::Span| -> usize {
            let base = fennec_syntax::prelude_file_at(span).map_or(0, |(_, _, base)| base);
            let at = usize::try_from(span.start.saturating_sub(base)).unwrap_or(0);
            source.get(..at).map_or(0, |before| before.matches('\n').count()) + 1
        };
        let exported = |line: usize| lines.get(line - 1).is_some_and(|text| text.starts_with("export "));
        let has_example = |line: usize| {
            doc_above(&lines, line)
                .iter()
                .any(|doc| matches!(fence_language(doc), Some("" | "fennec" | "fennec check")))
        };
        let mut add = |name: String, line: usize| items.push((format!("{file}: {name}"), has_example(line)));
        for item in &module.items {
            let (name, span, methods) = match item {
                fennec_syntax::Item::Function(def) => (&def.name, def.span, None),
                fennec_syntax::Item::Struct(ty)
                | fennec_syntax::Item::Enum(ty)
                | fennec_syntax::Item::Error(ty) => (&ty.name, ty.span, Some(&ty.methods)),
                fennec_syntax::Item::Interface(interface) => {
                    (&interface.name, interface.span, Some(&interface.methods))
                }
                fennec_syntax::Item::Const { name, span, .. }
                | fennec_syntax::Item::TypeAlias { name, span, .. } => (name, *span, None),
                fennec_syntax::Item::Extend(extension) => {
                    for method in extension.methods.iter().filter(|method| !method.name.starts_with('_')) {
                        add(format!("{}.{}", extension.name, method.name), line_of(method.span));
                    }
                    continue;
                }
                _ => continue,
            };
            let line = line_of(span);
            if !exported(line) {
                continue;
            }
            add(name.clone(), line);
            for method in methods.into_iter().flatten().filter(|method| !method.name.starts_with('_')) {
                add(format!("{name}.{}", method.name), line_of(method.span));
            }
        }
    }
    Ok(items)
}

/// Every public item std declares in Fennec has an example in its doc comment, but those listed
/// in `fixtures/std_without_examples.txt`. The list only shrinks: a new item comes with its
/// example, and an item given one leaves the list.
#[test]
fn every_public_item_in_the_standard_library_has_an_example() {
    let items = public_items().unwrap();
    assert!(items.len() > 500, "only {} public items were found in std", items.len());
    let without: BTreeSet<String> =
        items.iter().filter(|(_, has)| !*has).map(|(name, _)| name.clone()).collect();
    let all: BTreeSet<String> = items.into_iter().map(|(name, _)| name).collect();
    let list = Path::new(env!("CARGO_MANIFEST_DIR")).join(WITHOUT_EXAMPLES);
    // With `FENNEC_PIN_OUTPUTS` set, the list is written as it is now, which its diff then has to
    // show only shrinking.
    if std::env::var_os("FENNEC_PIN_OUTPUTS").is_some() {
        let mut text = LIST_HEADER.to_owned();
        for name in &without {
            text.push_str(name);
            text.push('\n');
        }
        std::fs::write(&list, text).unwrap();
    }
    let listed_text = std::fs::read_to_string(&list).unwrap();
    let listed: BTreeSet<String> = listed_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect();
    let new: Vec<&String> = without.difference(&listed).collect();
    let given: Vec<&String> = listed.difference(&without).filter(|name| all.contains(*name)).collect();
    let gone: Vec<&String> = listed.iter().filter(|name| !all.contains(*name)).collect();
    assert!(new.is_empty(), "give each of these an example in its doc comment: {new:?}");
    assert!(given.is_empty(), "these have an example now, so take them off {WITHOUT_EXAMPLES}: {given:?}");
    assert!(gone.is_empty(), "these aren't in std any more, so take them off {WITHOUT_EXAMPLES}: {gone:?}");
}

/// The examples in one doc comment are found as written: each fence with no language or
/// `fennec`, an `output` fence after one as what it prints, and other languages passed over.
#[test]
fn a_doc_comments_examples_are_its_fennec_fences() {
    let source = "## Adds.\n## ```fennec\n## assert add(1, 2) == 3\n## ```\n## ```output\n## 3\n## ```\n## ```text\n## not code\n## ```\n## ```\n## stdout.print(1)\n## ```\ndef add(a: int, b: int) -> int:\n    return a + b\n";
    let comments = doc_comments(source);
    assert_eq!(comments.len(), 1);
    let examples = examples_in("adds", &comments[0]).unwrap();
    assert_eq!(examples.len(), 2);
    assert_eq!(
        (examples[0].line, examples[0].code.as_slice()),
        (2, ["assert add(1, 2) == 3".to_owned()].as_slice())
    );
    assert_eq!(examples[0].output.as_deref(), Some(["3".to_owned()].as_slice()));
    assert_eq!((examples[1].line, examples[1].output.is_none()), (11, true));
    let lines: Vec<&str> = source.lines().collect();
    assert_eq!(doc_above(&lines, 14).len(), 13);
    assert!(examples_in("open", &doc_comments("## ```fennec\n## x = 1\n")[0]).is_err());
    assert!(examples_in("alone", &doc_comments("## ```output\n## 1\n## ```\n")[0]).is_err());
    // An example that only declares runs nothing, unless it's only checked, like a program's `main`.
    let declared = "## ```fennec\n## def f() -> int:\n##     return 1\n## ```\n";
    assert!(examples_in("declared", &doc_comments(declared)[0]).is_err());
    let checked = declared.replace("```fennec\n", "```fennec check\n");
    let examples = examples_in("checked", &doc_comments(&checked)[0]).unwrap();
    assert!(examples.len() == 1 && examples[0].checked);
    let after_checked = format!("{checked}## ```output\n## 1\n## ```\n");
    assert!(examples_in("printed", &doc_comments(&after_checked)[0]).is_err());
    // A `return` would leave before what it printed is checked.
    let leaves = "## ```fennec\n## x = 1\n## if x == 1:\n##     return\n## ```\n## ```output\n## 1\n## ```\n";
    assert!(examples_in("leaves", &doc_comments(leaves)[0]).is_err());
}

/// An example's lines go where a program would write them, and it's given only the capabilities
/// it names, with `stdout` for one that states what it prints.
#[test]
fn an_example_becomes_a_test_given_what_it_names() {
    let code: Vec<String> = [
        "from std.time import Duration",
        "def twice(n: int) -> int:",
        "    return n * 2",
        "fs.write_text(\"a\", str(twice(2)))?",
        "assert fs.read_text(\"a\")? == \"4\"",
        "x = net_fs.go(stdout_name)",
    ]
    .map(str::to_owned)
    .to_vec();
    let example =
        Example { file: "time", line: 10, code, output: Some(vec!["a {b}".to_owned()]), checked: false };
    let (text, from) = test_file(&[&example]);
    assert_eq!(
        text,
        "from std.time import Duration\ndef twice(n: int) -> int:\n    return n * 2\n\ntest \"time.fen:10\"(stdout: Stdout, fs: Fs):\n    fs.write_text(\"a\", str(twice(2)))?\n    assert fs.read_text(\"a\")? == \"4\"\n    x = net_fs.go(stdout_name)\n    assert stdout.captured() == \"a {b}\\n\"\n\n"
    );
    assert_eq!(from, [11, 12, 13, 0, 10, 14, 15, 16, 10, 0]);
    let said = "error\n  --> /tmp/x/time_examples.fen:6:5\n";
    assert_eq!(
        shown_in_std(said, "/tmp/x/time_examples.fen", "time", &from),
        "error\n  --> crates/syntax/src/stdlib/time.fen:14:5\n"
    );
}

/// Examples' imports become one line a module, with each name once, so two examples taking the
/// same name from a module don't import it twice.
#[test]
fn examples_imports_are_merged_by_module() {
    let imports = [
        ("from std.io import MemoryStream", 3),
        ("from std import json", 4),
        ("from std.io import MemoryStream, Reader", 9),
        ("import std.toml", 12),
        ("from std import base64", 15),
        ("import std.toml", 20),
    ]
    .map(|(line, at)| (line.to_owned(), at))
    .to_vec();
    let wanted = [
        ("from std.io import MemoryStream, Reader", 3),
        ("from std import json, base64", 4),
        ("import std.toml", 12),
    ]
    .map(|(line, at)| (line.to_owned(), at));
    assert_eq!(merged(imports), wanted);
}
