//! Every kit in `examples/projects/` runs and prints exactly its `.out` file, and passes its
//! tests. Every kit in `examples/projects/errors/`
//! fails with exactly its `.err` file, which names the file the problem is in.

use std::fs;
use std::path::{Path, PathBuf};

fn projects_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/projects")
}

/// The kits in a folder, by name.
fn kits(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if let Some(name) = path.file_name().filter(|_| path.join("kit.toml").is_file()) {
            names.push(name.to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Runs a kit as `fennec run` does, from the checkout: its output, or what it said on failing.
fn run(shown_as: &str) -> Result<String, String> {
    let ran = crate::executables::fennec_command()
        .args(["run", "--no-test", shown_as])
        .current_dir(repo())
        .output()
        .map_err(|err| err.to_string())?;
    if ran.status.success() {
        Ok(String::from_utf8_lossy(&ran.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&ran.stderr).into_owned())
    }
}

/// The checkout.
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn projects_print_their_expected_output() {
    if !crate::llvm::ready() {
        return;
    }
    let dir = projects_dir();
    let names = kits(&dir).unwrap();
    assert!(!names.is_empty(), "no projects found");
    for name in names {
        let shown_as = format!("examples/projects/{name}");
        let expected = fs::read_to_string(dir.join(format!("{name}.out"))).unwrap();
        let actual = run(&shown_as).unwrap_or_else(|err| panic!("{name} failed:\n{err}"));
        assert_eq!(actual, expected, "{name} printed something different");
        let tested = crate::executables::fennec_command()
            .args(["test", "--all", &shown_as])
            .current_dir(repo())
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&tested.stdout);
        assert!(tested.status.success(), "{name}: {said}{}", String::from_utf8_lossy(&tested.stderr));
        assert!(!said.contains("no tests"), "{name} has no tests");
    }
}

#[test]
fn error_projects_fail_with_their_expected_message() {
    if !crate::llvm::ready() {
        return;
    }
    let dir = projects_dir().join("errors");
    let names = kits(&dir).unwrap();
    assert!(!names.is_empty(), "no error projects found");
    for name in names {
        let shown_as = format!("examples/projects/errors/{name}");
        let expected = fs::read_to_string(dir.join(format!("{name}.err"))).unwrap();
        let actual = run(&shown_as).expect_err(&format!("{name} should fail"));
        assert_eq!(actual, expected, "{name} failed with a different message");
    }
}

#[test]
fn kit_toml_takes_comments_and_an_empty_dependencies_table() {
    let text = "# my game\nname = \"arena\"  # its name\nversion = \"1.2.3\"\nfennec = \"0.0.1\"\n\n[dependencies]\n";
    let manifest = fennec_kit::Manifest::parse(text).unwrap();
    assert_eq!(
        (manifest.name.as_str(), manifest.version.as_str(), manifest.fennec.as_str()),
        ("arena", "1.2.3", "0.0.1")
    );
}

#[test]
fn kit_toml_reports_each_mistake() {
    let messages = |text: &str| -> Vec<String> {
        fennec_kit::Manifest::parse(text)
            .unwrap_err()
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect()
    };
    let valid = "name = \"arena\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n";
    assert_eq!(fennec_kit::Manifest::parse(&valid.replace("0.0.1", "dev")).unwrap().fennec, "dev");
    assert_eq!(messages(&valid.replace("0.1.0", "dev")), ["`dev` isn't a version"]);
    assert_eq!(messages(&valid.replace("arena", "Arena")), ["`Arena` can't name a kit"]);
    assert_eq!(messages(&valid.replace("arena", "star-fox")), ["`star-fox` can't name a kit"]);
    assert_eq!(messages(&valid.replace("arena", "9lives")), ["`9lives` can't name a kit"]);
    for version in ["1.0", "1.0.0.0", "+1.0.0", "1..0", "a.b.c"] {
        assert_eq!(messages(&valid.replace("0.1.0", version)), [format!("`{version}` isn't a version")]);
    }
    assert_eq!(
        messages(&valid.replace("0.1.0", "0.1#0")),
        ["`0.1#0` isn't a version"],
        "a # in a string isn't a comment"
    );
    assert_eq!(
        messages(&valid.replace("0.0.1", "99.0.0")),
        [format!("this kit needs Fennec 99.0.0, and this is Fennec {}", env!("CARGO_PKG_VERSION"))]
    );
    assert_eq!(
        messages(&valid.replace("\"arena\"", "arena")),
        ["`name` should be a string in double quotes"]
    );
    assert_eq!(messages(&format!("{valid}name = \"other\"\n")), ["`name` is set twice"]);
    assert_eq!(messages(&format!("{valid}[package]\n")), ["kit.toml has no table `[package]`"]);
    assert_eq!(messages(&format!("{valid}just words\n")), ["expected `key = \"value\"` or a `[table]`"]);
    assert_eq!(messages("fennec = \"0.0.1\"\n"), ["kit.toml needs `name`", "kit.toml needs `version`"]);
}

/// `[dependencies]` names kits from git, pinned to a commit's whole hash and maybe a folder in the
/// repository, or from a path.
#[test]
fn kit_toml_reads_dependencies_from_git_and_paths() {
    let commit = "3f9c2a1e7b4d3f9c2a1e7b4d3f9c2a1e7b4d3f9c";
    let text = format!(
        "name = \"arena\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n\n[dependencies]\nphysics = {{ git = \"https://git.example.com/studio/physics\", commit = \"{commit}\" }}\npostgres = {{ git = \"https://github.com/fennec-language/fennec\", commit = \"{commit}\", folder = \"kits/postgres\" }}  # first-party\ntools = {{ path = \"../tools\" }}\n"
    );
    let manifest = fennec_kit::Manifest::parse(&text).unwrap();
    let git = |url: &str, folder: Option<&str>| fennec_kit::Source::Git {
        url: url.to_owned(),
        commit: commit.to_owned(),
        folder: folder.map(str::to_owned),
    };
    let named =
        |name: &str, source| fennec_kit::Dependency { name: name.to_owned(), source, gets: Vec::new() };
    assert_eq!(
        manifest.dependencies,
        [
            named("physics", git("https://git.example.com/studio/physics", None)),
            named("postgres", git("https://github.com/fennec-language/fennec", Some("kits/postgres"))),
            named("tools", fennec_kit::Source::Path("../tools".to_owned())),
        ]
    );
}

#[test]
fn kit_toml_says_what_is_wrong_with_a_dependency() {
    let message = |line: &str| -> String {
        let text =
            format!("name = \"arena\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n[dependencies]\n{line}\n");
        let errors = fennec_kit::Manifest::parse(&text).unwrap_err();
        assert_eq!(errors.len(), 1, "{line}");
        errors.into_iter().next().map(|error| error.message).unwrap_or_default()
    };
    let commit = "3f9c2a1e7b4d3f9c2a1e7b4d3f9c2a1e7b4d3f9c";
    let url = "https://git.example.com/physics";
    for pinned in ["main", "v1.2.0", "3f9c2a1", "3F9C2A1E7B4D3F9C2A1E7B4D3F9C2A1E7B4D3F9C"] {
        assert_eq!(
            message(&format!("physics = {{ git = \"{url}\", commit = \"{pinned}\" }}")),
            format!("`{pinned}` isn't a commit's whole hash")
        );
    }
    assert_eq!(
        message(&format!("physics = {{ git = \"{url}\" }}")),
        "`physics` needs the `commit` it's pinned to"
    );
    for address in ["--upload-pack=touch /tmp/x", "ext::sh -c touch% /tmp/x"] {
        assert_eq!(
            message(&format!("physics = {{ git = \"{address}\", commit = \"{commit}\" }}")),
            format!("`{address}` isn't a git repository's address")
        );
    }
    assert_eq!(
        message(&format!("physics = {{ git = \"{url}\", commit = \"{commit}\", folder = \"../up\" }}")),
        "`../up` isn't a folder inside the repository"
    );
    assert_eq!(message("tools = { path = \"../tools\", git = \"x\" }"), "`tools` has both `git` and `path`");
    assert_eq!(message("tools = { place = \"../tools\" }"), "a dependency has no key called `place`");
    assert_eq!(message("fennec = \"0.4.2\""), "the Fennec a kit builds with isn't a dependency");
    assert_eq!(message("tools = { path = \"../tools\" "), "`tools` should be a table in braces");
    assert_eq!(
        message("lasers = \"1.0.0\""),
        "`lasers` asks for a version from the Den, which isn't built yet"
    );
    assert_eq!(message("Tools = { path = \"../tools\" }"), "`Tools` can't name a kit");
    assert_eq!(
        message("tools = { path = \"../tools\" }\ntools = { path = \"../other\" }"),
        "`tools` is listed twice"
    );
}

/// A dependency's `gets` names whole capabilities in quotes, each once. `gets = []` says what no
/// `gets` already says, an unknown name or a table that narrows one is refused, and none of these
/// offers a fix: what a kit gets is a person's to write.
#[test]
fn kit_toml_gets_names_capabilities_a_person_gives() {
    let parsed = |line: &str| {
        let text =
            format!("name = \"arena\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n[dependencies]\n{line}\n");
        fennec_kit::Manifest::parse(&text)
    };
    let message = |line: &str| -> String {
        let errors = parsed(line).unwrap_err();
        assert_eq!(errors.len(), 1, "{line}");
        assert!(errors.iter().all(|error| error.fixes.is_empty()), "{line}");
        errors.into_iter().next().map(|error| error.message).unwrap_or_default()
    };
    let gets = |line: &str| parsed(line).unwrap().dependencies.into_iter().next().unwrap().gets;
    assert_eq!(gets("http = { path = \"../http\", gets = [\"Net\"] }"), ["Net"]);
    assert_eq!(gets("http = { gets = [\"Net\", \"Fs\",], path = \"../http\" }"), ["Net", "Fs"]);
    assert_eq!(gets("db = { path = \"../db\", gets = [ \"Database\" , \"File\" ] }"), ["Database", "File"]);
    assert_eq!(gets("shapes = { path = \"../shapes\" }"), Vec::<String>::new());
    assert_eq!(
        message("http = { path = \"../http\", gets = [] }"),
        "`gets = []` gives `http` nothing, which leaving `gets` out already says"
    );
    assert_eq!(
        message("http = { path = \"../http\", gets = [\"Network\"] }"),
        "`Network` isn't a capability, so `http` can't get it"
    );
    assert_eq!(
        message("http = { path = \"../http\", gets = [\"Secret\"] }"),
        "`Secret` isn't a capability, so `http` can't get it"
    );
    assert_eq!(
        message("http = { path = \"../http\", gets = [\"Net\", \"Net\"] }"),
        "`Net` is in `http`'s `gets` twice"
    );
    assert_eq!(
        message("saves = { path = \"../saves\", gets = [{ Fs = { only = \"saves/\" } }] }"),
        "narrowing what `saves` gets, as `{ Fs = { only = \"saves/\" } }`, isn't built yet"
    );
    assert_eq!(
        message("http = { path = \"../http\", gets = [Net] }"),
        "`Net` in `http`'s `gets` should be a capability's type in quotes"
    );
    assert_eq!(
        message("http = { path = \"../http\", gets = \"Net\" }"),
        "`http`'s `gets` should be a list in brackets"
    );
}

#[test]
fn kit_toml_errors_point_at_their_line() {
    let text = "name = \"arena\"\n  colour = \"red\"  # indented\n";
    let errors = fennec_kit::Manifest::parse(text).unwrap_err();
    let start = text.find("colour").unwrap();
    let end = text.find("  #").unwrap();
    assert_eq!(errors.first().map(|error| error.span), Some(fennec_syntax::Span::new(start, end)));
}
