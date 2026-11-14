//! The oracle stays a test oracle: nothing that ships depends on it, and it depends on nothing
//! past the checker.

use std::path::Path;

/// The crates that may take the oracle as a normal dependency: the fuzzer and the benchmarks,
/// which hold the compiler to it. Any crate may take it as a dev-dependency.
const MAY_DEPEND: &[&str] = &["fennec-fuzz", "fennec-bench"];

/// Each crate's name and the names of its normal dependencies, from every `crates/*/Cargo.toml`.
fn manifests() -> Vec<(String, Vec<String>)> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&crates).into_iter().flatten().flatten() {
        let manifest = entry.path().join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else { continue };
        found.push(normal_dependencies(&text));
    }
    assert!(found.len() > 10, "the crates weren't found beside this one");
    found
}

/// A manifest's package name and its normal dependencies: those under `[dependencies]`,
/// `[build-dependencies]` or a target's `dependencies`, written as keys or as tables of their own.
fn normal_dependencies(manifest: &str) -> (String, Vec<String>) {
    let mut name = String::new();
    let mut section = String::new();
    let mut dependencies = Vec::new();
    for line in manifest.lines().map(str::trim) {
        if let Some(header) = line.strip_prefix('[').and_then(|line| line.split(']').next()) {
            header.trim_matches('[').clone_into(&mut section);
            let table = section.rsplit_once('.').filter(|(table, _)| is_normal(table));
            if let Some((_, dependency)) = table {
                dependencies.push(dependency.trim_matches('"').to_owned());
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim().trim_matches('"');
        if section == "package" && key == "name" {
            value.trim().trim_matches('"').clone_into(&mut name);
        } else if is_normal(&section) && !key.starts_with('#') {
            // `fennec-ir.workspace = true` names `fennec-ir`.
            dependencies.push(key.split('.').next().unwrap_or(key).to_owned());
        }
    }
    (name, dependencies)
}

fn is_normal(section: &str) -> bool {
    let table = section.rsplit('.').next().unwrap_or(section);
    matches!(table, "dependencies" | "build-dependencies")
}

#[test]
fn only_the_fuzzer_and_the_benchmarks_depend_on_the_oracle() {
    let mut wrong = Vec::new();
    for (name, dependencies) in manifests() {
        if dependencies.iter().any(|dependency| dependency == "fennec-oracle")
            && !MAY_DEPEND.contains(&name.as_str())
        {
            wrong.push(name);
        }
    }
    assert!(
        wrong.is_empty(),
        "fennec-oracle is a test oracle, never part of what ships, but these crates depend on it: {}. \
         Make it a dev-dependency, or hold the compiler to it from fennec-fuzz or fennec-bench.",
        wrong.join(", ")
    );
}

#[test]
fn the_oracle_depends_on_nothing_past_the_checker() {
    let (_, dependencies) = manifests().into_iter().find(|(name, _)| name == "fennec-oracle").unwrap();
    for dependency in &dependencies {
        assert!(
            matches!(dependency.as_str(), "fennec-syntax" | "fennec-check"),
            "fennec-oracle depends on {dependency}: it shares only the parser and the checker with the \
             compiler, so a bug past them can't hide from it"
        );
    }
}

#[test]
fn manifests_are_read_as_cargo_reads_them() {
    let text = "[package]\nname = \"fennec-x\"\n\n[dependencies]\nfennec-oracle.workspace = true\nfennec-check = { path = \"../check\" }\n\n[dev-dependencies]\nfennec-temp.workspace = true\n\n[target.'cfg(unix)'.dependencies]\nrustix = \"1\"\n\n[dependencies.fennec-ir]\npath = \"../ir\"\n";
    let (name, dependencies) = normal_dependencies(text);
    assert_eq!(name, "fennec-x");
    assert_eq!(dependencies, ["fennec-oracle", "fennec-check", "rustix", "fennec-ir"]);
}
