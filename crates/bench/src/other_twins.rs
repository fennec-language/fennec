//! A kernel's Go and Java twins, beside its Rust one: `name.go` and `name.java` next to
//! `name.fen`, each built the way someone using the language would build it, with nothing
//! tuned, so the suite can say where Fennec stands against them too. Rust stays the twin the
//! LLVM lane is held to; these are for showing. A Java run includes the JVM starting and its
//! JIT warming up, as it does for anyone running the program.

use std::path::Path;
use std::process::Command;

use crate::measure::Program;

/// A language with twins of its own.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Language {
    Go,
    Java,
}

impl Language {
    pub(crate) const ALL: [Language; 2] = [Language::Go, Language::Java];

    /// Its name, as the table shows it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Language::Go => "Go",
            Language::Java => "Java",
        }
    }

    /// Its name, as JSON keys and file extensions write it.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Language::Go => "go",
            Language::Java => "java",
        }
    }

    /// Whether this machine can build it.
    pub(crate) fn installed(self) -> bool {
        let (tool, flag) = match self {
            Language::Go => ("go", "version"),
            Language::Java => ("javac", "-version"),
        };
        Command::new(tool).arg(flag).output().is_ok_and(|output| output.status.success())
    }

    /// A kernel's twin in this language, if it has one: `name.go` or `name.java` beside
    /// `name.fen`.
    pub(crate) fn twin_of(self, kernel_source: &str) -> Option<std::path::PathBuf> {
        let twin = Path::new(kernel_source).with_extension(self.key());
        twin.is_file().then_some(twin)
    }

    /// Builds a twin into `out_dir`, as a program to run.
    pub(crate) fn build(self, source: &Path, out_dir: &Path) -> Result<Program, String> {
        let stem = source.file_stem().map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
        let (mut command, program) = match self {
            Language::Go => {
                let binary = out_dir.join(format!("{stem}.go.bin"));
                let mut command = Command::new("go");
                command.arg("build").arg("-o").arg(&binary).arg(source);
                (command, Program { command: binary, args: Vec::new(), stdin: None })
            }
            Language::Java => {
                // The class is named for the file, so it runs by that name from its own folder.
                let classes = out_dir.join(format!("{stem}.java.d"));
                let mut command = Command::new("javac");
                command.arg("-d").arg(&classes).arg(source);
                let args = vec!["-cp".to_owned(), classes.to_string_lossy().into_owned(), stem];
                (command, Program { command: "java".into(), args, stdin: None })
            }
        };
        let output =
            command.output().map_err(|err| format!("couldn't run {}'s compiler: {err}", self.name()))?;
        if output.status.success() {
            Ok(program)
        } else {
            let said = String::from_utf8_lossy(&output.stderr);
            Err(format!("{} didn't compile: {}", source.display(), said.lines().next().unwrap_or("")))
        }
    }
}
