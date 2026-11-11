//! The `fennec` command's tests, as one program.

mod cli;
mod examples;
mod executables;
mod game_math;
mod hints;
mod lint_fixes;
mod live;
mod llvm;
#[cfg(test)]
mod loader;
mod lsp;
mod parallel;
mod projects;
mod reached_runs;
mod replay;
mod selfhost;
mod syntax_errors;
mod unchanged_tests;
