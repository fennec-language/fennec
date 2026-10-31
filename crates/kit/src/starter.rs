//! `fennec new` and `fennec init`: a starter kit to build on.

use std::path::Path;

use crate::manifest::valid_name;

/// Creates a kit in a new folder `dir`, named after the folder.
pub fn create(dir: &Path) -> Result<String, String> {
    let name = dir.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    if !valid_name(&name) {
        return Err(format!(
            "`{name}` can't name a kit: use lowercase letters, digits and `_`, starting with a letter"
        ));
    }
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    write(dir, &name)?;
    std::fs::write(dir.join("AGENTS.md"), crate::GUIDANCE)
        .map_err(|err| format!("couldn't write {}: {err}", dir.join("AGENTS.md").display()))?;
    Ok(name)
}

/// Writes a kit called `name` into `dir`, which needn't exist yet.
pub(crate) fn write(dir: &Path, name: &str) -> Result<(), String> {
    let files = [
        ("kit.toml", manifest(name)),
        ("src/main.fen", MAIN.replace("NAME", name)),
        ("src/greeting.fen", GREETING.to_owned()),
    ];
    for (path, text) in files {
        let path = dir.join(path);
        let written = match path.parent() {
            Some(parent) => std::fs::create_dir_all(parent).and_then(|()| std::fs::write(&path, text)),
            None => std::fs::write(&path, text),
        };
        written.map_err(|err| format!("couldn't write {}: {err}", path.display()))?;
    }
    Ok(())
}

fn manifest(name: &str) -> String {
    format!(
        "name = \"{name}\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n\n[dependencies]\n",
        env!("CARGO_PKG_VERSION")
    )
}

const MAIN: &str = "from greeting import greet

def main(stdout: Stdout):
    stdout.print(greet(\"NAME\"))
";

const GREETING: &str = "def greet(name: str) -> str:
    f\"Hello from {name}!\"

test \"greeting names who it greets\":
    assert greet(\"fox\") == \"Hello from fox!\"
";
